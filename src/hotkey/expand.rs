//! 全局「快捷输入」展开（方案 C）：英文/直通状态下也能把 `gds` 展开成快递地址
//!
//! 背景：Rime 在英文直通（`ascii_mode`）下不产生候选窗，编码会被原样交给应用 ——
//! 候选表权重再高也救不了。所以中文状态走 Rime（候选表第一项），英文状态由这里的
//! 低级键盘钩子兜底：缓冲最近敲的字符 → 命中用户定义的编码 → 退格 N 次 + 注入文本。
//!
//! 防重复出字的守卫（关键）：`resources\rime\lua\listen_learn.lua` 在中文状态下
//! **每次按键**都会写 `%APPDATA%\Rime\listen_ime_beat.txt`。心跳新鲜 = 当前是中文状态
//! （输入法已经处理过这些键）→ 本模块保持沉默；心跳变旧（英文直通 / 没装输入法）→ 接手。
//!
//! 约束：钩子回调里**不能加锁、不能日志、不能阻塞**（Windows 会摘钩），
//! 所以这里只做纯计算 + 无锁读（`try_read`），真正的按键注入交给 worker 线程。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

/// 编码 → 展开文本 的快照
#[derive(Debug, Clone, Default)]
pub struct PhraseMap {
    map: HashMap<String, String>,
    /// 最短编码长度（短于它的不触发，防误伤）
    pub min_len: usize,
}

impl PhraseMap {
    /// 从 `phrases.tsv` 正文构建（`词条⇥编码⇥类别⇥权重⇥说明`）
    pub fn from_tsv(text: &str, min_len: usize) -> Self {
        let mut map = HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut it = line.split('\t');
            let (Some(text), Some(code)) = (it.next(), it.next()) else {
                continue;
            };
            let text = text.trim();
            let code = code.trim().to_lowercase();
            // 编码必须是纯小写字母（和 Rime 的 speller 一致）
            if text.is_empty() || code.len() < 2 || !code.chars().all(|c| c.is_ascii_lowercase()) {
                continue;
            }
            map.insert(code, text.to_string());
        }
        Self { map, min_len }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }


    /// 精确命中（大小写不敏感）才返回
    pub fn lookup(&self, buf: &str) -> Option<&str> {
        if buf.len() < self.min_len.max(3) {
            return None;
        }
        self.map.get(&buf.to_lowercase()).map(|s| s.as_str())
    }
}

/// 键盘缓冲：只认字母数字，遇到别的键就清空
#[derive(Debug, Default)]
pub struct Buffer {
    s: String,
}

impl Buffer {
    const MAX: usize = 32;

    pub fn push(&mut self, ch: char) {
        if self.s.len() >= Self::MAX {
            self.s.clear();
        }
        self.s.push(ch.to_ascii_lowercase());
    }

    pub fn backspace(&mut self) {
        self.s.pop();
    }

    pub fn clear(&mut self) {
        self.s.clear();
    }

    pub fn as_str(&self) -> &str {
        &self.s
    }
}

/// 心跳新鲜度：中文状态下 lua 每键都写这个文件
pub fn ime_active(beat: &Option<PathBuf>, fresh: Duration) -> bool {
    let Some(p) = beat else { return false };
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(mtime)
        .map(|d| d < fresh)
        .unwrap_or(false)
}

/// 展开请求：`(要退格的字符数, 要注入的文本)`
pub type ExpandReq = (usize, String);

/// 运行期状态（钩子只读原子量）
pub struct Shared {
    pub enabled: AtomicBool,
    pub beat: RwLock<Option<PathBuf>>,
    pub map: RwLock<PhraseMap>,
    /// 统计：发生过多少次展开（自检/日志用）
    pub expanded: AtomicU64,
    /// 被「IME 活跃」守卫挡掉多少次
    pub skipped_ime: AtomicU64,
}

impl Shared {
    pub fn new(enabled: bool, beat: Option<PathBuf>, map: PhraseMap) -> Arc<Self> {
        Arc::new(Self {
            enabled: AtomicBool::new(enabled),
            beat: RwLock::new(beat),
            map: RwLock::new(map),
            expanded: AtomicU64::new(0),
            skipped_ime: AtomicU64::new(0),
        })
    }
}

/// 一次按键的处理结果（纯函数，便于单测）
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// 普通字符，进缓冲
    Push(char),
    /// 退格
    Pop,
    /// 结束符（空格/回车/tab）：可能触发展开
    Terminate,
    /// 其它键（方向键、符号…）：清空缓冲
    Reset,
}

/// 把虚拟键变成 `Step`（只覆盖展开需要的键，其余一律 Reset）
pub fn step_for(vk: u16) -> Step {
    match vk {
        // A-Z
        0x41..=0x5A => Step::Push((b'a' + (vk as u8 - 0x41)) as char),
        // 0-9
        0x30..=0x39 => Step::Push((b'0' + (vk as u8 - 0x30)) as char),
        // 空格 / 回车 / tab
        0x20 | 0x0D | 0x09 => Step::Terminate,
        // 退格
        0x08 => Step::Pop,
        _ => Step::Reset,
    }
}

/// 缓冲命中时算出「退格几个 + 注入什么」
///
/// 结束符本身也要退掉（否则会留下一个空格/换行）
pub fn plan(map: &PhraseMap, buf: &str) -> Option<ExpandReq> {
    let text = map.lookup(buf)?;
    Some((buf.chars().count() + 1, text.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TSV: &str = "\
# 注释
广东省深圳市南山区科技园南路 1 号 3 栋 502\tgds\t快捷输入\t90\t发货地址
yunghg 术语\t yunghg\t快捷输入\t90\t测试
坏行没制表符
";

    #[test]
    fn parses_phrases_tsv() {
        let m = PhraseMap::from_tsv(TSV, 3);
        assert_eq!(m.len(), 2, "词条里带空格没问题（文本不限），坏行要跳过: {m:?}");
        assert_eq!(
            m.lookup("gds"),
            Some("广东省深圳市南山区科技园南路 1 号 3 栋 502"),
            "文本长度不限"
        );
        assert!(m.lookup("g").is_none(), "太短不触发");
        assert!(m.lookup("gdsx").is_none());
    }

    #[test]
    fn shortest_code_respected() {
        let m = PhraseMap::from_tsv("地址\tab\t快捷输入\t90\tx\n", 3);
        assert!(m.lookup("ab").is_none(), "min_len=3 时两位编码不触发");
    }

    #[test]
    fn buffer_tracks_and_resets() {
        let mut b = Buffer::default();
        for c in "gds".chars() {
            b.push(c);
        }
        assert_eq!(b.as_str(), "gds");
        b.backspace();
        assert_eq!(b.as_str(), "gd");
        b.clear();
        assert_eq!(b.as_str(), "");
    }

    #[test]
    fn vk_mapping() {
        assert_eq!(step_for(0x47), Step::Push('g'));
        assert_eq!(step_for(0x20), Step::Terminate);
        assert_eq!(step_for(0x0D), Step::Terminate);
        assert_eq!(step_for(0x08), Step::Pop);
        assert_eq!(step_for(0x25), Step::Reset); // 左方向键
    }

    #[test]
    fn plan_counts_backspaces_including_terminator() {
        let m = PhraseMap::from_tsv("我的地址\tgds\t快捷输入\t90\tx\n", 3);
        assert_eq!(plan(&m, "gds"), Some((4, "我的地址".to_string())));
        assert_eq!(plan(&m, "gd"), None);
    }

    #[test]
    fn heartbeat_freshness() {
        let dir = std::env::temp_dir().join(format!("listen_beat_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("beat.txt");
        std::fs::write(&p, b"x").unwrap();
        assert!(ime_active(&Some(p.clone()), Duration::from_secs(5)), "刚写的心跳算新鲜");
        std::thread::sleep(Duration::from_millis(30));
        assert!(!ime_active(&Some(p), Duration::from_millis(10)), "过期后不算");
        assert!(!ime_active(&None, Duration::from_secs(5)), "没有心跳文件 → 不活跃");
    }
}
