//! 用户词学习：把 Rime 侧 lua 钩子记下的「编码 + 上屏文本」聚合成**置顶词表**
//!
//! ```text
//! Rime（lua/listen_learn.lua，部署时铺进用户目录）
//!    ↓ 每次上屏 append 一行（时间戳 / 编码 / 文本）
//! <exe 同级>\hotwords\learn_log.tsv
//!    ↓ 本模块「增量导入」（按文件字节 offset，不重复吃老数据）
//! <exe 同级>\hotwords\learned.tsv   ← GUI「我打过的词」读写的就是它
//!    ↓ hotwords::load() 合成
//! <Rime 用户目录>\listen_hotwords.txt（custom_phrase 置顶表，quality 99 = 铁第一位）
//! ```
//!
//! **为什么不让 librime 自己调频**：它写 userdb 的权重远小于词典自带权重。
//! 实测：`shishi` 选第 2 个候选「实施」后，再打两次第一位仍然是「实时」。
//! 只有把我们自己那张 `initial_quality: 99` 的表用起来，才是「用过的词必排第一」。
//!
//! **删除语义**：导入按 offset 单向消费日志，所以用户从 `learned.tsv` 里删掉的词
//! 不会下次又被加回来（“重新学习全部”除外，那是明确的重新全量导入）。

use crate::config::{Config, Paths};
use crate::hotwords::{self, Term};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Rime 侧日志文件名（lua 钩子写、我们读）——放在**热词目录**里（exe 同级），
/// 部署时把绝对路径 patch 给 lua，所以不依赖 Rime 用户目录到底在哪
pub const LOG_FILE: &str = "learn_log.tsv";
/// lua 脚本在用户目录里的相对路径
pub const LUA_REL: &str = "lua/listen_learn.lua";
/// lua 脚本正文（部署时铺到用户目录）
pub const LUA_BODY: &str = include_str!("../resources/rime/lua/listen_learn.lua");
/// 读日志进度的偏移量记录（放在热词目录里，跟 learned.tsv 一起）
const OFFSET_FILE: &str = ".learn_offset";

/// learned.tsv 的读写锁（worker 周期导入 与 GUI 编辑 可能同时动手）
static FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// learned.tsv 里「说明」列的格式：`N 次`
fn note_of(count: u32) -> String {
    format!("{count} 次")
}
/// 从「说明」列解析使用次数（解析不出来按 1）
pub fn count_of(note: &str) -> u32 {
    note.split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
}
/// 次数 → 展示用权重（learned 只进打字侧，权重不影响识别；仅排序列用）
fn weight_of(count: u32) -> i32 {
    (40 + count as i32 * 3).min(99)
}

/// 学习日志的绝对路径（部署时写进 Rime patch，lua 侧往这里 append）
pub fn log_path(cfg: &Config, paths: &Paths) -> PathBuf {
    hotwords::dir(cfg, paths).join(LOG_FILE)
}

fn offset_path(cfg: &Config, paths: &Paths) -> PathBuf {
    hotwords::dir(cfg, paths).join(OFFSET_FILE)
}

fn read_offset(p: &Path) -> u64 {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn write_offset(p: &Path, v: u64) {
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, v.to_string());
}

/// 一次增量导入的结果
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Imported {
    /// 日志里新出现的 (编码, 词条) 对数
    pub new_pairs: usize,
    /// 老词条又被打了几次（累计新增次数）
    pub added_count: u32,
    /// 导入后词表总数
    pub total: usize,
}

impl Imported {
    pub fn is_empty(&self) -> bool {
        self.new_pairs == 0 && self.added_count == 0
    }
}

/// 增量导入：读日志的新增部分 → 合并进 `learned.tsv` → 记录进度
///
/// `relearn = true` 时忽略进度，把整份日志重新吃一遍（“重新学习全部”）。
/// 一条「词」而不是「句子」：热词表只收词语（2026-10-21 用户要求）
///
/// 规则（与 lua 侧的第一道过滤互补，这里是唯一权威）：
/// - 只允许汉字 / 字母 / 数字 / `_` `-`：出现空格、标点、括号、表情 → 丢掉（句子基本都被这条挡掉）
/// - 汉字 ≤ 6 个（`杰盛微`=3 ✓，`这句话太长了超过六个字` ✗）
/// - 纯英文/数字 ≤ 24 个字符（`PostgreSQL` 这类术语别误杀）
pub fn is_word_like(word: &str) -> bool {
    let w = word.trim();
    if w.is_empty() {
        return false;
    }
    let mut cjk = 0usize;
    for c in w.chars() {
        if is_cjk(c) {
            cjk += 1;
        } else if !(c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return false; // 标点/空格/符号 → 当句子处理
        }
    }
    if cjk > 6 {
        return false;
    }
    if cjk == 0 && w.chars().count() > 24 {
        return false;
    }
    true
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF | 0x3040..=0x30FF | 0xAC00..=0xD7AF)
}
pub fn import_new(cfg: &Config, paths: &Paths, relearn: bool) -> Result<Imported> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let log = log_path(cfg, paths);
    let data = std::fs::read(&log).unwrap_or_default();
    let off_path = offset_path(cfg, paths);
    let mut start = if relearn { 0 } else { read_offset(&off_path) };
    if start as usize > data.len() {
        start = 0; // 日志被删/被截断 → 从头来
    }

    // 只吃「完整行」，半行留到下次（lua 正在写）
    let mut consumed = start as usize;
    let text = String::from_utf8_lossy(&data[consumed..]).into_owned();
    let mut agg: HashMap<(String, String), u32> = HashMap::new();
    for line in text.split_inclusive('\n') {
        if !line.ends_with('\n') {
            break;
        }
        consumed += line.len();
        let mut it = line.trim_end().split('\t');
        let (_ts, code, word) = (it.next(), it.next(), it.next());
        let (Some(code), Some(word)) = (code, word) else {
            continue;
        };
        let (code, word) = (code.trim().to_lowercase(), word.trim().to_string());
        // 与 lua 侧同口径再挡一次（编码纯字母 2~12、词条非空且不太长）
        if code.len() < 2
            || code.len() > 12
            || !code.chars().all(|c| c.is_ascii_lowercase())
            || !is_word_like(&word)
        {
            continue;
        }
        *agg.entry((code, word)).or_default() += 1;
    }

    let mut terms: Vec<(Term, u32)> = hotwords::read_pack(cfg, paths, hotwords::LEARNED)
        .into_iter()
        .map(|t| {
            let n = count_of(&t.note);
            (t, n)
        })
        .collect();

    let mut new_pairs = 0usize;
    let mut added = 0u32;
    for ((code, word), n) in agg {
        added += n;
        match terms
            .iter_mut()
            .find(|(t, _)| t.code == code && t.text == word)
        {
            Some((_, cnt)) => *cnt += n,
            None => {
                new_pairs += 1;
                terms.push((
                    Term {
                        text: word,
                        code,
                        category: "learned".into(),
                        weight: 0,
                        rank: 0, // 真正排序权重在 hotwords::load() 里按「使用次数」给
                        note: String::new(),
                    },
                    n,
                ));
            }
        }
    }

    if !(new_pairs == 0 && added == 0) || relearn {
        // 次数降序 → 超上限按次数淘汰（被淘汰的不会回来：日志已消费）
        terms.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.text.cmp(&b.0.text)));
        let max = cfg.hotwords.learn_max.max(1);
        terms.truncate(max);
        for (t, n) in terms.iter_mut() {
            t.weight = weight_of(*n);
            t.note = note_of(*n);
        }
        let list: Vec<Term> = terms.into_iter().map(|(t, _)| t).collect();
        let path = hotwords::pack_path(cfg, paths, hotwords::LEARNED);
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).ok();
        }
        std::fs::write(&path, hotwords::render_tsv(hotwords::LEARNED, &list))
            .with_context(|| format!("写学习词表失败: {}", path.display()))?;
        write_offset(&off_path, consumed as u64);
        tracing::info!(
            "[学习] 新词 {new_pairs} 条 / 累计次数 +{added} → {} 条",
            list.len()
        );
        return Ok(Imported {
            new_pairs,
            added_count: added,
            total: list.len(),
        });
    }

    Ok(Imported {
        new_pairs: 0,
        added_count: 0,
        total: terms.len(),
    })
}

/// 清空已学词表（日志进度不动 → 打过的词不会又被导入回来）
pub fn clear(cfg: &Config, paths: &Paths) -> Result<()> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = hotwords::pack_path(cfg, paths, hotwords::LEARNED);
    let body = hotwords::render_tsv(hotwords::LEARNED, &[]);
    std::fs::write(&path, body).with_context(|| format!("清空失败: {}", path.display()))?;
    let log = log_path(cfg, paths);
    let len = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
    write_offset(&offset_path(cfg, paths), len);
    tracing::info!("[学习] 已清空（进度停在 {len} 字节）");
    Ok(())
}

/// 当前学习词条数（读文件，不依赖配置开关）
pub fn learned_terms(cfg: &Config, paths: &Paths) -> Vec<Term> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    hotwords::read_pack(cfg, paths, hotwords::LEARNED)
}

/// GUI 保存「我打过的词」（走同一把锁，避免与 worker 的周期导入撞车）
pub fn save_terms(cfg: &Config, paths: &Paths, terms: &[Term]) -> Result<()> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = hotwords::pack_path(cfg, paths, hotwords::LEARNED);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).ok();
    }
    std::fs::write(&path, hotwords::render_tsv(hotwords::LEARNED, terms))
        .with_context(|| format!("写学习词表失败: {}", path.display()))
}

/// 日志里累计记录了多少条（用于 GUI 展示「学到的原始记录」）
pub fn log_lines(cfg: &Config, paths: &Paths) -> usize {
    std::fs::read_to_string(log_path(cfg, paths))
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_paths(tag: &str) -> (Paths, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("listen_learn_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (
            Paths {
                root: dir.clone(),
                config_file: dir.join("config.toml"),
            },
            dir,
        )
    }

    /// 直接把日志写到 `<hotwords 目录>/learn_log.tsv`
    fn write_log(cfg: &Config, paths: &Paths, lines: &[&str]) {
        let d = hotwords::dir(cfg, paths);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(LOG_FILE), lines.join("\n") + "\n").unwrap();
    }

    #[test]
    fn note_and_weight_roundtrip() {
        assert_eq!(count_of(&note_of(7)), 7);
        assert_eq!(count_of(""), 1);
        assert_eq!(count_of("坏数据"), 1);
        assert!(weight_of(100) <= 99);
    }

    #[test]
    fn import_aggregates_and_skips_bad_lines() {
        let (paths, dir) = tmp_paths("agg");
        let cfg = Config::default();
        write_log(
            &cfg,
            &paths,
            &[
                "1\tshishi\t实施",
                "2\tshishi\t实施",
                "3\tshishi\t实时",
                "4\tSHI SHI\t大小写与空格都非法",
                "5\ta\t单字母编码非法",
                "6\tnihaoshijie\t这句话太长了超过十二个字肯定被丢掉",
            ],
        );
        let r = import_new(&cfg, &paths, false).unwrap();
        assert_eq!(r.new_pairs, 2);
        assert_eq!(r.added_count, 3);
        let terms = learned_terms(&cfg, &paths);
        let top = terms.iter().find(|t| t.text == "实施").unwrap();
        assert_eq!(top.code, "shishi");
        assert_eq!(top.note, "2 次"); // 2 次 → 排第一
        assert_eq!(terms.first().unwrap().text, "实施");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn second_import_is_incremental() {
        let (paths, dir) = tmp_paths("incr");
        let cfg = Config::default();
        write_log(&cfg, &paths, &["1\tshishi\t实施"]);
        assert_eq!(import_new(&cfg, &paths, false).unwrap().new_pairs, 1);
        // 没有新行 → 什么都不算
        assert!(import_new(&cfg, &paths, false).unwrap().is_empty());
        // 追加两行 → 只吃新的（次数累加）
        let p = log_path(&cfg, &paths);
        let mut s = std::fs::read_to_string(&p).unwrap();
        s.push_str("2\tshishi\t实施\n");
        s.push_str("3\tshishi\t实施\n");
        std::fs::write(&p, s).unwrap();
        let r = import_new(&cfg, &paths, false).unwrap();
        assert_eq!(r.new_pairs, 0);
        assert_eq!(r.added_count, 2);
        assert_eq!(learned_terms(&cfg, &paths)[0].note, "3 次");
        // 删除后不会自己回来（进度已消费）
        let kept = learned_terms(&cfg, &paths);
        assert_eq!(kept.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clear_stops_reimport() {
        let (paths, dir) = tmp_paths("clear");
        let cfg = Config::default();
        write_log(&cfg, &paths, &["1\tshishi\t实施"]);
        import_new(&cfg, &paths, false).unwrap();
        clear(&cfg, &paths).unwrap();
        assert!(learned_terms(&cfg, &paths).is_empty());
        // 清空后老日志不再被导入
        assert!(import_new(&cfg, &paths, false).unwrap().is_empty());
        assert!(learned_terms(&cfg, &paths).is_empty());
        // “重新学习全部”才把它捞回来
        assert_eq!(import_new(&cfg, &paths, true).unwrap().new_pairs, 1);
        assert_eq!(learned_terms(&cfg, &paths).len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn max_trims_lowest_counts() {
        let (paths, dir) = tmp_paths("max");
        let mut cfg = Config::default();
        cfg.hotwords.learn_max = 2;
        write_log(&cfg, &paths, &["1\taa\t甲甲", "2\tbb\t乙乙", "3\tcc\t丙丙"]);
        import_new(&cfg, &paths, false).unwrap();
        assert_eq!(learned_terms(&cfg, &paths).len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }
    // ---- 2026-10-21：只收「词语」，不收句子 ----

    #[test]
    fn word_like_filter() {
        assert!(is_word_like("杰盛微"), "3 个汉字是词");
        assert!(is_word_like("PostgreSQL"), "英文术语不能误杀");
        assert!(is_word_like("nRF52832"));
        assert!(!is_word_like(""), "空串");
        assert!(!is_word_like("这句话太长了超过六个字"), "超过 6 个汉字当句子丢掉");
        assert!(!is_word_like("你好，世界"), "带标点 = 句子");
        assert!(!is_word_like("hello world"), "带空格 = 句子");
        assert!(!is_word_like("好的。"), "句末标点");
    }
}
