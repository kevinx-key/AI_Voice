//! 热词包：**一份数据同时驱动「打字」与「录音」**
//!
//! - 数据：`<exe 同级>\hotwords\<包名>.tsv`（首次运行从内置包铺出，之后归用户所有，GUI 里增删改）
//!   格式：`词条⇥编码⇥类别⇥权重⇥说明`，`#` 开头为注释；编码留空 = 只用于录音侧
//! - 打字：`--rime-deploy` 时把带编码的词条写成 Rime 自定义短语表 `listen_hotwords.txt`
//!   （雾凇 `custom_phrase` 翻译器置顶候选；`rime_ice.custom.yaml` 里 patch 了 `user_dict`）
//! - 录音：ASR 热词偏置（FunASR-Nano prompt / Paraformer 热词文件）+ 整理 prompt 术语保留
//!
//! 权重 0~100 = 优先级：越大越先被送进识别模型的热词 prompt（FunASR-Nano 的 LLM KV 有上限）。

use crate::config::{Config, Paths};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// 内置包：键圈（机械键盘社区）
pub const KEYBOARD: &str = "keyboard";
/// 内置包：vibecoding / 嵌入式
/// 内置包：vibecoding / 嵌入式
pub const VIBECODING: &str = "vibecoding";

/// 用户词表：快捷输入（编码 → 任意文本，如 `gds` → 快递地址）；**只用于打字**
pub const PHRASES: &str = "phrases";
/// 用户词表：我打过的词（`learn::import_new` 自动维护）；**只用于打字**
pub const LEARNED: &str = "learned";
/// 用户自己的热词表「我的热词」：**两条链都生效**（打字置顶 + 录音热词），条数不设上限
///
/// 2026-10-21 用户问「只有 118 条热词，是不是架构不支持更多」——不是：内置包（键圈 194、
/// AI 编程 118）之外，这里就是给他自己无限加词的地方（一份 tsv，加多少都行；
/// 打字侧进候选表无上限，录音侧只受「送识别模型的条数上限 asr_top_n」约束）。
pub const MINE: &str = "mine";

/// Rime 候选表的 rank 基数（第三列权重，越大越靠前；同编码内比较）
pub const RANK_PHRASES: i32 = 1_000_000; // 快捷输入：永远第一
pub const RANK_MINE: i32 = 500_000; // 我的热词
pub const RANK_LEARNED: i32 = 100_000; // 我打过的词（+ 使用次数）

/// 只进打字侧、不进录音热词的词表（地址、私人词条不该进识别 prompt）
pub fn is_typing_only(name: &str) -> bool {
    name == PHRASES || name == LEARNED
}

/// 用户词表的空模板（首次运行铺出；已存在不覆盖）
const PHRASES_TEMPLATE: &str = "\
# 倾听输入法 · 快捷输入（自己定义编码 → 任意文本）\n\
# 格式：词条 <TAB> 编码 <TAB> 类别 <TAB> 权重 <TAB> 说明\n\
#   编码只能用小写字母（全拼字母表只有 a-z）；打完编码后第一个候选就是这里的文本\n\
#   只用于打字，不会进录音热词；本文件也用 GUI「热词 → 快捷输入」页编辑\n\
# 示例（去掉开头的 # 即生效）：\n\
# 广东省深圳市南山区科技园南路 1 号\tgds\t快捷输入\t90\t发货地址\n\
# 13800138000\tsj\t快捷输入\t90\t手机号\n";

const MINE_TEMPLATE: &str = "\
# 倾听输入法 · 我的热词（自己加，两条链都生效：打字置顶候选 + 录音热词偏置）\n\
# 格式：词条 <TAB> 编码 <TAB> 类别 <TAB> 权重 <TAB> 说明\n\
#   编码只能用小写字母；编码留空 = 只进录音侧（不生成打字候选）\n\
#   权重 0~100，越大越先被送进识别模型的热词 prompt\n\
#   条数不设上限：打字侧全量进候选表；录音侧受「送识别模型热词上限」约束\n\
# 示例（去掉开头的 # 即生效）：\n\
# 美国客户\tmgkh\t业务\t95\t别说成 Docker\n\
# nRF52832\t\t芯片\t90\t只进录音侧\n";

const LEARNED_TEMPLATE: &str = "\
# 倾听输入法 · 我打过的词（自动学习，勿手写）\n\
# 格式：词条 <TAB> 编码 <TAB> 类别 <TAB> 权重 <TAB> 说明（说明里的数字 = 打过几次）\n\
# 自动：每次上屏会把「你敲的编码 + 上屏文本」记到 learn_log.tsv，主程序增量导入到这里\n\
# 手动：可以直接删掉不想置顶的词（删了不会自己回来）\n";

/// 内置包内容（编译进二进制：免安装包改动、dev 与打包行为一致）
const BUILTIN: [(&str, &str); 2] = [
    (KEYBOARD, include_str!("../resources/hotwords/keyboard.tsv")),
    (
        VIBECODING,
        include_str!("../resources/hotwords/vibecoding.tsv"),
    ),
];

/// 一条热词
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Term {
    /// 词条本身（上屏/送 ASR 的样子）
    pub text: String,
    /// 打字编码（小写）；空 = 只进录音侧
    pub code: String,
    pub category: String,
    /// 我们自己的显示/筛选权重（0~100，GUI 里那一列）
    pub weight: i32,
    /// 写进 Rime 候选表的**排序权重**（`词条⇥编码⇥权重` 的第三列）
    ///
    /// 由 `load()` 按来源分配：快捷输入 > 我的热词 > 我打过的词（按次数） > 内置包（按 weight）。
    /// 同编码下数值越大越靠前 —— 这就是「多次选过的词自动排到第一」的实现方式（2026-10-21）。
    pub rank: i32,
    /// 说明（中文释义，仅展示）
    pub note: String,
}

impl Term {
    fn from_fields(f: &[&str]) -> Option<Self> {
        let text = f.first()?.trim();
        if text.is_empty() || text.starts_with('#') {
            return None;
        }
        let get = |i: usize| f.get(i).map(|s| s.trim()).unwrap_or("");
        let weight = get(3).parse().unwrap_or(50);
        Some(Self {
            text: text.to_string(),
            code: get(1).to_lowercase(),
            category: get(2).to_string(),
            weight,
            rank: weight,
            note: get(4).to_string(),
        })
    }
}

/// 解析 tsv（容错：3~5 列、空行、注释行、缺列）
pub fn parse_tsv(text: &str) -> Vec<Term> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            // 至少要有「词条 + 编码」两列（编码可为空）——防止把说明性文字当词条收进来
            if f.len() < 2 {
                return None;
            }
            Term::from_fields(&f)
        })
        .collect()
}

/// 把词条写回 tsv（写死表头，注释块统一由我们维护）
pub fn render_tsv(name: &str, terms: &[Term]) -> String {
    let mut s = match name {
        KEYBOARD => "# 倾听输入法 · 热词包：键圈（机械键盘社区）\n".to_string(),
        PHRASES => PHRASES_TEMPLATE.to_string(),
        LEARNED => LEARNED_TEMPLATE.to_string(),
        _ => format!("# 倾听输入法 · 热词包：{name}\n"),
    };
    if !matches!(name, PHRASES | LEARNED) {
        s.push_str(
            "# 格式：词条 <TAB> 编码 <TAB> 类别 <TAB> 权重 <TAB> 说明\n\
             #   编码只能用小写字母（全拼字母表只有 a-z，数字/符号打不出来 → 含数字的词条算仅录音）\n\
             #   编码留空 = 只用于录音侧（ASR 热词偏置 / 术语保留），不生成打字候选\n\
             #   权重 0~100 = 优先级，越大越先被送进识别模型的热词 prompt\n\
             # 本文件可手工编辑；GUI「热词」页读写的就是它\n",
        );
    }
    for t in terms {
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            t.text, t.code, t.category, t.weight, t.note
        ));
    }
    s
}

/// 热词目录（空配置 = exe 同级 `hotwords\`）
pub fn dir(cfg: &Config, paths: &Paths) -> PathBuf {
    if cfg.hotwords.dir.trim().is_empty() {
        paths.root.join("hotwords")
    } else {
        paths.resolve(&cfg.hotwords.dir)
    }
}

/// 某个包的 tsv 路径
pub fn pack_path(cfg: &Config, paths: &Paths, name: &str) -> PathBuf {
    dir(cfg, paths).join(format!("{name}.tsv"))
}

/// 内置包正文（用于「恢复内置词表」与首次铺出）
pub fn builtin_body(name: &str) -> Option<&'static str> {
    BUILTIN.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)
}

/// 全部内置包名
pub fn builtin_names() -> Vec<&'static str> {
    BUILTIN.iter().map(|(n, _)| *n).collect()
}

/// 首次运行把内置包 / 用户词表铺到用户目录（**已存在不覆盖**：用户改过就归用户）
pub fn ensure_user_packs(cfg: &Config, paths: &Paths) -> Vec<String> {
    let mut log = Vec::new();
    let d = dir(cfg, paths);
    if let Err(e) = std::fs::create_dir_all(&d) {
        log.push(format!("  !! 创建热词目录失败 {}: {e}", d.display()));
        return log;
    }
    for (name, body) in BUILTIN {
        let p = d.join(format!("{name}.tsv"));
        if p.is_file() {
            continue;
        }
        match std::fs::write(&p, body) {
            Ok(()) => log.push(format!("  铺出内置热词包 {}", p.display())),
            Err(e) => log.push(format!("  !! 写 {} 失败: {e}", p.display())),
        }
    }
    // 用户词表（第一次铺空模板；快捷输入的示例是注释掉的）
    for (name, body) in [
        (PHRASES, PHRASES_TEMPLATE),
        (LEARNED, LEARNED_TEMPLATE),
        (MINE, MINE_TEMPLATE),
    ] {
        let p = d.join(format!("{name}.tsv"));
        if p.is_file() {
            continue;
        }
        match std::fs::write(&p, body) {
            Ok(()) => log.push(format!("  铺出用户词表 {}", p.display())),
            Err(e) => log.push(format!("  !! 写 {} 失败: {e}", p.display())),
        }
    }
    log
}

/// 读一个包（文件缺失/解析为空 → 回退内置正文）
pub fn read_pack(cfg: &Config, paths: &Paths, name: &str) -> Vec<Term> {
    let p = pack_path(cfg, paths, name);
    if let Ok(text) = std::fs::read_to_string(&p) {
        let terms = parse_tsv(&text);
        if !terms.is_empty() {
            return terms;
        }
        // 只有「内置包被清空」才值得警告（说明用户把内置词表删空了，会回退内置副本）。
        // 用户自己的表（mine / phrases / learned）本来就是空模板，按空处理即可 ——
        // 老实现每次启动都刷三条 WARN，纯噪音（2026-10-21 修正）。
        match builtin_body(name) {
            Some(body) => {
                tracing::warn!("热词包 {} 解析为空，回退内置副本", p.display());
                return parse_tsv(body);
            }
            None => return Vec::new(),
        }
    }
    builtin_body(name).map(parse_tsv).unwrap_or_default()
}

/// 合成打字用的全部词条（**优先级：快捷输入 > 我打过的词 > 热词包**，同编码前者胜）
pub fn load(cfg: &Config, paths: &Paths) -> Vec<Term> {
    if !cfg.hotwords.enabled {
        return Vec::new();
    }
    let mut all: Vec<Term> = Vec::new();
    if cfg.hotwords.phrases {
        // 快捷输入排最前：用户自定义编码 → 任意文本（`gds` → 快递地址），必须第一候选
        all.extend(read_pack(cfg, paths, PHRASES).into_iter().map(|mut t| {
            t.rank = RANK_PHRASES + t.weight.clamp(0, 100) * 10;
            t
        }));
    }
    if cfg.hotwords.learn {
        let mut learned = read_pack(cfg, paths, LEARNED);
        learned.truncate(cfg.hotwords.learn_max.max(1)); // 文件本身按次数降序
        // 学到的词按「用过几次」定 rank：次数越多越靠前 → 多次选过的词自动第一
        all.extend(learned.into_iter().map(|mut t| {
            let n = crate::learn::count_of(&t.note).min(500) as i32;
            t.rank = RANK_LEARNED + n;
            t
        }));
    }
    for name in &cfg.hotwords.packs {
        let name = name.trim();
        if name.is_empty() || is_typing_only(name) {
            continue;
        }
        let base = if name == MINE { RANK_MINE } else { 0 };
        all.extend(read_pack(cfg, paths, name).into_iter().map(|mut t| {
            t.rank = base + t.weight.clamp(0, 100) * 10;
            t
        }));
    }
    // 同编码/同词条已去重；再按权重降序，让候选表与 GUI 的顺序好看（不影响同编码的胜负）
    sort_dedup(priority_dedup(all))
}

/// 去重（保序）：同编码只留第一条（优先级靠调用方给的顺序），同词条忽略大小写只留一条
fn priority_dedup(terms: Vec<Term>) -> Vec<Term> {
    let mut codes = std::collections::HashSet::new();
    let mut texts = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(terms.len());
    for t in terms {
        let code = t.code.trim().to_lowercase();
        let text = t.text.to_lowercase();
        if !code.is_empty() && !codes.insert(code) {
            continue;
        }
        if !texts.insert(text) {
            continue;
        }
        out.push(t);
    }
    out
}

/// 权重降序 + 词条去重（忽略大小写，保留权重最高的一条）
pub fn sort_dedup(mut terms: Vec<Term>) -> Vec<Term> {
    terms.sort_by(|a, b| {
        b.weight
            .cmp(&a.weight)
            .then_with(|| a.text.to_lowercase().cmp(&b.text.to_lowercase()))
    });
    let mut seen = std::collections::HashSet::new();
    terms.retain(|t| seen.insert(t.text.to_lowercase()));
    terms
}

/// 送 ASR / 整理用的术语表：用户 `[glossary] terms` 最优先，其余按权重降序
///
/// 注意：快捷输入（`phrases`）与我打过的词（`learned`）**不进录音侧**（地址等私人词条不送模型）。
pub fn asr_terms(cfg: &Config, paths: &Paths) -> Vec<String> {
    // (词条, 排序权重)：用户手写术语直接给最高分，保证排最前
    let mut pairs: Vec<(String, i32)> = Vec::new();
    if cfg.glossary.enabled {
        pairs.extend(
            cfg.glossary
                .terms
                .iter()
                .map(|s| (s.trim().to_string(), 999)),
        );
    }
    if cfg.hotwords.enabled {
        for name in &cfg.hotwords.packs {
            let name = name.trim();
            if name.is_empty() || is_typing_only(name) {
                continue;
            }
            pairs.extend(
                read_pack(cfg, paths, name)
                    .into_iter()
                    .map(|t| (t.text, t.weight)),
            );
        }
    }
    pairs.retain(|(t, _)| !t.is_empty());
    pairs.sort_by(|a, b| b.1.cmp(&a.1));
    let mut seen = std::collections::HashSet::new();
    pairs.retain(|(t, _)| seen.insert(t.to_lowercase()));
    pairs.into_iter().map(|(t, _)| t).collect()
}

/// 立即把词表写进 Rime 用户目录（**不需要重新部署**）
///
/// 实测（2026-10-09，weasel 0.17.4 / librime 1.13）：`listen_hotwords.txt` 是**每个输入会话开始时**
/// 重读的纯文本表（日志 `tsv.cc: reading tsv file`），所以改完内容只要换个窗口/输入框（新会话）
/// 就生效；只有**结构变更**（patch / lua 钩子 / 方案列表 / 主力语言）才必须重新部署。
///
/// 返回 Ok(None) = 倾听输入法未装或用户目录不在（比如只想调 config 的机器）。
pub fn apply_to_rime(cfg: &Config, paths: &Paths) -> Result<Option<String>> {
    let info = crate::rime::detect(&cfg.rime.user_dir);
    if !info.installed || !info.user_dir.is_dir() {
        return Ok(None);
    }
    write_rime_table(cfg, paths, &info.user_dir)
}

/// 生成 Rime 自定义短语表（custom_phrase 格式：`词条⇥编码`；无编码的词条不生成）
///
/// 表头 `#@/db_name` 必须与文件名一致 —— 雾凇 rime_ice.schema.yaml 的 `custom_phrase`
/// 翻译器用 `db_class: stabledb` + `user_dict: listen_hotwords`，librime 直接读这个纯文本表。
pub fn rime_table(terms: &[Term]) -> String {
    let mut s = String::from(
        "# Rime table\n\
         # coding: utf-8\n",
    );
    s.push_str(&format!("#@/db_name\t{}\n", crate::rime::TERMS_FILE));
    s.push_str(
        "#@/db_type\ttabledb\n\
         #\n\
         # 倾听输入法热词包 → 打字候选项（置顶）。自动生成，勿手改：\n\
         #   改词条到 GUI「热词」页（或 exe 同级 hotwords\\*.tsv），然后重新部署。\n\
         #\n\
         # 此行之后不能写注释\n",
    );
    // 按 rank 降序写：同编码内权重大的排前面（rime-ice 的 custom_phrase 表第三列就是权重）
    let mut ordered: Vec<&Term> = terms.iter().collect();
    ordered.sort_by(|a, b| b.rank.cmp(&a.rank).then_with(|| a.text.cmp(&b.text)));
    for t in ordered {
        let code = t.code.trim();
        // 全拼输入缓冲只认 a-z（雾凇 speller/alphabet 不含数字/符号）
        if code.is_empty()
            || t.text.trim().is_empty()
            || !code.chars().all(|c| c.is_ascii_lowercase())
        {
            continue;
        }
        // 第三列 = 权重（rime-ice 的 custom_phrase.txt 格式：词汇⇥编码⇥权重）
        // 旧实现只写两列 → 同编码的所有词权重都是 0 → 候选顺序等于随机，
        // 用户反复选「杰盛微」也压不过「杰升为」（2026-10-21 修）。
        s.push_str(&format!("{}\t{}\t{}\n", t.text, code, t.rank.max(1)));
    }
    s
}

/// 写 Rime 短语表（顺带登记到卸载清单）
pub fn write_rime_table(cfg: &Config, paths: &Paths, user_dir: &Path) -> Result<Option<String>> {
    let terms = load(cfg, paths);
    let n = terms.iter().filter(|t| !t.code.trim().is_empty()).count();
    if n == 0 {
        // 全部关闭/无编码 → 不留空表，顺手把旧的摘掉
        let p = user_dir.join(crate::rime::TERMS_FILE);
        if p.is_file() {
            let _ = std::fs::remove_file(&p);
            crate::rime::unregister_file(user_dir, crate::rime::TERMS_FILE);
        }
        return Ok(None);
    }
    let body = rime_table(&terms);
    let p = user_dir.join(crate::rime::TERMS_FILE);
    std::fs::write(&p, body).with_context(|| format!("写热词表失败: {}", p.display()))?;
    crate::rime::register_file(user_dir, crate::rime::TERMS_FILE)?;
    Ok(Some(format!("  打字热词表 {n} 条 → {}", p.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    // ---- 2026-10-21：候选排序（Rime 表第三列权重） ----

    #[test]
    fn rime_table_writes_weight_column() {
        let terms = vec![
            Term { text: "杰盛微".into(), code: "jieshengwei".into(), rank: 100_000 + 7, weight: 90, ..Default::default() },
            Term { text: "杰升为".into(), code: "jieshengwei".into(), rank: 50, weight: 50, ..Default::default() },
        ];
        let table = rime_table(&terms);
        // 两列是不够的：同编码会随机排序（旧 bug）
        assert!(table.contains("杰盛微\tjieshengwei\t100007"), "{table}");
        assert!(table.contains("杰升为\tjieshengwei\t50"), "{table}");
        // rank 大的先写
        let a = table.find("杰盛微").unwrap();
        let b = table.find("杰升为").unwrap();
        assert!(a < b, "高频词必须排在前面:\n{table}");
    }

    #[test]
    fn source_priority_phrases_beats_learned_beats_packs() {
        // load() 的 rank 分配：快捷输入 > 我的热词 > 我打过的词 > 内置包
        assert!(RANK_PHRASES > RANK_MINE);
        assert!(RANK_MINE > RANK_LEARNED);
        assert!(RANK_LEARNED > 100 * 10);
    }

    #[test]
    fn tsv_flexible_columns() {
        let t = parse_tsv("# 注释\nGB\tgb\nIC\tic\t行业机制\t86\t意向调查\n\n坏行没制表符\n");
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].text, "GB");
        assert_eq!(t[0].code, "gb");
        assert_eq!(t[0].weight, 50); // 缺权重 → 默认 50
        assert_eq!(t[1].weight, 86);
        assert_eq!(t[1].note, "意向调查");
    }

    #[test]
    fn builtin_packs_parse() {
        for name in builtin_names() {
            let terms = parse_tsv(builtin_body(name).unwrap());
            assert!(terms.len() > 50, "{name} 词条太少: {}", terms.len());
            assert!(terms.iter().any(|t| !t.code.is_empty()));
            // 打字编码只能是小写字母（全拼字母表 a-z），否则 Rime 表里是死条目
            for t in &terms {
                assert!(
                    t.code.is_empty() || t.code.chars().all(|c| c.is_ascii_lowercase()),
                    "{name}: {} 的编码「{}」不合法",
                    t.text,
                    t.code
                );
            }
        }
    }

    #[test]
    fn rime_table_drops_non_letter_codes() {
        let ts = parse_tsv("R1\tr1\tx\t90\t\nnRF52832\tnrf52832\tx\t80\t\nFR4\tfr4\tx\t70\t\nGB\tgb\tx\t60\t\n");
        let body = rime_table(&ts);
        assert!(body.contains("GB\tgb\t"), "GB 应进表（带权重列）: {body}");
        for bad in ["R1", "nRF52832", "FR4"] {
            assert!(!body.contains(bad), "{bad} 不该进打字表");
        }
    }

    #[test]
    fn dedup_keeps_highest_weight() {
        let ts = parse_tsv("GB\tgb\tx\t10\t\nGB\tgb\ty\t90\t\nPC\t\tx\t5\t\n");
        let out = sort_dedup(ts);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].weight, 90);
    }

    #[test]
    fn rime_table_skips_empty_code() {
        let ts = parse_tsv("GB\tgb\tx\t90\t\nGroup Buy\t\tx\t80\t\n");
        let body = rime_table(&ts);
        assert!(body.contains("GB\tgb\t"), "GB 应进表（带权重列）: {body}");
        assert!(!body.contains("Group Buy"));
        assert!(body.contains("listen_hotwords.txt"));
    }

    #[test]
    fn write_table_registers_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("listen_hw_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths {
            root: dir.clone(),
            config_file: dir.join("config.toml"),
        };
        let user_dir = dir.join("rime");
        std::fs::create_dir_all(&user_dir).unwrap();
        let mut cfg = Config::default();
        cfg.hotwords.packs = vec![KEYBOARD.to_string()];

        let out = write_rime_table(&cfg, &paths, &user_dir).unwrap().unwrap();
        assert!(out.contains("打字热词表"));
        let body = std::fs::read_to_string(user_dir.join(crate::rime::TERMS_FILE)).unwrap();
        assert!(body.contains("#@/db_name\tlisten_hotwords.txt"));
        // 生成的是合法 Rime 表：非注释行都是「词条<TAB>编码」
        assert!(body.lines().skip_while(|l| !l.starts_with("#@/")).count() > 100);
        let mf = std::fs::read_to_string(user_dir.join(crate::rime::MANIFEST)).unwrap();
        assert!(mf.contains(crate::rime::TERMS_FILE), "卸载清单应登记热词表");

        // 关掉热词 → 表与清单条目一并摘掉
        cfg.hotwords.enabled = false;
        assert!(write_rime_table(&cfg, &paths, &user_dir).unwrap().is_none());
        assert!(!user_dir.join(crate::rime::TERMS_FILE).exists());
        let mf = std::fs::read_to_string(user_dir.join(crate::rime::MANIFEST)).unwrap();
        assert!(!mf.contains(crate::rime::TERMS_FILE));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn asr_terms_prefers_user_glossary() {
        let dir = std::env::temp_dir().join(format!("listen_hw_gl_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths {
            root: dir.clone(),
            config_file: dir.join("config.toml"),
        };
        let mut cfg = Config::default();
        cfg.glossary.terms = vec!["我的术语".into()];
        let terms = asr_terms(&cfg, &paths);
        assert_eq!(terms[0], "我的术语");
        assert!(terms.contains(&"GB".to_string()));
        // 权重降序：GB(87) 在 MX(83) 前面
        let gb = terms.iter().position(|t| t == "GB").unwrap();
        let mx = terms.iter().position(|t| t == "MX").unwrap();
        assert!(gb < mx);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
