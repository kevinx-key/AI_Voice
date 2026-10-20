//! 语言 → 方案映射表 + `default.custom.yaml` patch 生成
//!
//! 方案 id 已按 resources 里实际抓到的方案包核对（2026-09-22）：
//! rime-ice → rime_ice / melt_eng；easy_en → easy_en；
//! rime-jaroomaji → jaroomaji（韩文方案包因上游无许可证，2026-09-26 移除）

use std::fmt;

/// 主力语言
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
    Ja,
    /// 二期预留（方案包未内置）
    Fr,
}

impl Lang {
    pub fn as_str(&self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::En => "en",
            Lang::Ja => "ja",
            Lang::Fr => "fr",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            // 空串 = 尚未配置（首启向导会问），不算未知语言
            "" | "zh" | "chinese" => Lang::Zh,
            "en" | "english" => Lang::En,
            "ja" | "jp" | "japanese" => Lang::Ja,
            "fr" | "french" => Lang::Fr,
            other => {
                tracing::warn!("未知语言「{other}」，按中文处理");
                Lang::Zh
            }
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Lang::Zh => "中文",
            Lang::En => "英文",
            Lang::Ja => "日文",
            Lang::Fr => "法文（二期）",
        }
    }

    pub fn all() -> [Lang; 4] {
        ALL
    }
}

/// 全部语言（新增语言变体时必须同步这里）
pub const ALL: [Lang; 4] = [Lang::Zh, Lang::En, Lang::Ja, Lang::Fr];

impl fmt::Display for Lang {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一个语言的部署计划
pub struct Plan {
    pub lang: Lang,
    /// 写进 default.custom.yaml 的方案列表（第一个是默认方案）
    pub schema_list: Vec<&'static str>,
    /// 需要写 grammar 补丁的方案（只有中文）
    pub grammar_schema: Option<&'static str>,
    /// 英文方案 id（english_mixed 时追加到列表）
    pub english_schema: Option<&'static str>,
    /// 需要生成/维护的 <schema>.custom.yaml
    pub patch_schemas: Vec<&'static str>,
}

/// 法文二期：方案包未内置时返回 None（GUI 会提示）
pub fn plan(lang: Lang, english_mixed: bool, _zh_trad: bool) -> Option<Plan> {
    let mut p = match lang {
        Lang::Zh => Plan {
            lang,
            // 雾凇自带 melt_eng（英文混输）；radical_pinyin（部件反查）是
            // rime_ice.schema.yaml 的 schema/dependencies，部署时自动编译，
            // 刻意不列进 schema_list（列了会多一个可切换方案）
            schema_list: vec!["rime_ice"],
            grammar_schema: Some("rime_ice"),
            english_schema: Some("melt_eng"),
            patch_schemas: vec!["rime_ice"],
        },
        Lang::En => Plan {
            lang,
            schema_list: vec!["easy_en"],
            grammar_schema: None,
            english_schema: None,
            patch_schemas: vec!["easy_en"],
        },
        Lang::Ja => Plan {
            lang,
            schema_list: vec!["jaroomaji"],
            grammar_schema: None,
            english_schema: Some("easy_en"),
            patch_schemas: vec!["jaroomaji"],
        },
        // 二期：法文方案包未内置
        Lang::Fr => return None,
    };
    if english_mixed {
        if let Some(e) = p.english_schema {
            if !p.schema_list.contains(&e) {
                p.schema_list.push(e);
            }
        }
    }
    Some(p)
}

/// 生成 default.custom.yaml 里我们管理的那一段（缩进 2 空格，插在 `patch:` 下面）
pub fn default_block(p: &Plan, zh_trad: bool) -> String {
    let mut s = String::new();
    s.push_str("  schema_list:\n");
    for sc in &p.schema_list {
        s.push_str(&format!("    - schema: {sc}\n"));
    }
    // Shift 切中英文：Shift_L/Shift_R 均设为 commit_code（提交当前编码并切到西文；再按切回）
    s.push_str("  # Shift 切换中英文（倾听输入法默认写入）\n");
    s.push_str("  ascii_composer:\n");
    s.push_str("    switch_key:\n");
    s.push_str("      Shift_L: commit_code\n");
    s.push_str("      Shift_R: commit_code\n");
    // 简繁默认态：开关在方案内（rime-ice 的 traditionalization），这里只给中文写
    if p.lang == Lang::Zh {
        s.push_str(&format!(
            "  # 简繁由 rime-ice 的 traditionalization 开关控制，默认 {}（Ctrl+Shift+4 可切）\n",
            if zh_trad { "繁" } else { "简" }
        ));
    }
    s
}

/// rime-ice 语法模型（万象 LMDG）补丁；文件名 = 用户目录根下的 .gram 名（去扩展名）
pub const GRAMMAR_LANGUAGE: &str = "wanxiang-lts-zh-hans";

pub fn grammar_block() -> String {
    let mut s = String::new();
    s.push_str("  grammar:\n");
    s.push_str(&format!("    language: {GRAMMAR_LANGUAGE}\n"));
    s.push_str("    collocation_max_length: 6\n");
    s.push_str("    collocation_min_length: 3\n");
    s.push_str("    collocation_penalty: -14\n");
    s.push_str("    non_collocation_penalty: -6\n");
    s.push_str("    weak_collocation_penalty: -100\n");
    s.push_str("    rear_penalty: -20\n");
    s.push_str("  translator/contextual_suggestions: false\n");
    s.push_str("  translator/max_homophones: 8\n");
    s
}

/// 打字热词表：把 `custom_phrase` 翻译器的词表指到我们生成的 `listen_hotwords.txt`
/// （雾凇自带的是 `custom_phrase.txt`，会被方案包升级覆盖，所以另起一份自己的）
pub const TERMS_DICT: &str = "listen_hotwords";

/// 用户词学习钩子（lua processor 名）
pub const LEARN_PROCESSOR: &str = "lua_processor@*listen_learn";

/// 生成 <schema>.custom.yaml 里我们管理的那一段
/// - zh_trad：简繁默认态（索引由调用方按 schema 文件实际位置算好）
/// - grammar：是否启用语法模型
/// - terms：是否启用打字热词表（仅带 custom_phrase 翻译器的方案）
/// - learn_path：用户词学习日志的绝对路径（Some = 挂 lua 钩子）
pub fn schema_block(
    switch_idx: Option<usize>,
    zh_trad: bool,
    grammar: bool,
    terms: bool,
    // 打字联想：`custom_phrase/enable_completion`（打前缀也出热词候选）
    completion: bool,
    learn_path: Option<&str>,
) -> String {
    let mut s = String::new();
    if grammar {
        s.push_str(&grammar_block());
    }
    if terms {
        s.push_str(&format!("  custom_phrase/user_dict: {TERMS_DICT}\n"));
        // 显式写 true/false：关掉时也要能覆盖掉上一次部署留下的 true
        s.push_str(&format!(
            "  custom_phrase/enable_completion: {}\n",
            if completion { "true" } else { "false" }
        ));
    }
    if let Some(p) = learn_path {
        // 追加一个旁路 processor：不处理按键，只把「编码 + 上屏文本」记到本地文件
        //
        // ⚠️ librime 的 `/+` 语义是「把**列表**追加到目标列表」，值写标量会报
        // `error applying patch to engine/processors/+` 并**让整个方案编译失败**（实测 2026-10-09）。
        s.push_str("  engine/processors/+:\n");
        s.push_str(&format!("    - {LEARN_PROCESSOR}\n"));
        s.push_str(&format!("  listen_learn/path: \"{p}\"\n"));
    }
    if let Some(i) = switch_idx {
        s.push_str(&format!(
            "  switches/@{i}/reset: {}\n",
            if zh_trad { 1 } else { 0 }
        ));
    }
    s
}

/// 在方案 schema 文件里找 `switches:` 列表里某个开关的下标
/// （用于生成 switches/@N/reset 补丁，避免写死下标）
pub fn switch_index(schema_yaml: &str, name: &str) -> Option<usize> {
    let mut in_switches = false;
    let mut idx = 0usize;
    for raw in schema_yaml.lines() {
        let line = raw.trim_end();
        if line.trim_start().starts_with('#') {
            continue;
        }
        if !in_switches {
            if line.starts_with("switches:") {
                in_switches = true;
            }
            continue;
        }
        // switches 块结束：出现顶格且不是列表项的键
        if !line.starts_with(' ') && !line.trim().is_empty() {
            break;
        }
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("- name:") {
            // 兼容带引号的写法（- name: "x" / - name: 'x'）
            let n = rest
                .split('#')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches(|c| c == '"' || c == '\'');
            if n == name {
                return Some(idx);
            }
            idx += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "switches:\n  - name: ascii_mode\n    states: [ 中, Ａ ]\n  - name: traditionalization  # 简繁\n    states: [ 简, 繁 ]\n  - name: \"emoji\"\n    reset: 1\n";

    #[test]
    fn find_switch_index() {
        assert_eq!(switch_index(SAMPLE, "ascii_mode"), Some(0));
        assert_eq!(switch_index(SAMPLE, "traditionalization"), Some(1));
        assert_eq!(switch_index(SAMPLE, "emoji"), Some(2));
        assert_eq!(switch_index(SAMPLE, "nope"), None);
    }

    #[test]
    fn zh_plan_contains_melt_eng() {
        let p = plan(Lang::Zh, true, false).unwrap();
        assert_eq!(p.schema_list, vec!["rime_ice", "melt_eng"]);
        let b = default_block(&p, false);
        assert!(b.contains("- schema: rime_ice"));
        assert!(b.contains("- schema: melt_eng"));
    }

    #[test]
    fn fr_is_reserved() {
        assert!(plan(Lang::Fr, true, false).is_none());
    }

    #[test]
    fn hotword_patch_line() {
        // 热词表 patch：把 custom_phrase 翻译器的词表指到我们生成的文件
        let b = schema_block(None, false, false, true, false, None);
        assert_eq!(
            b,
            "  custom_phrase/user_dict: listen_hotwords\n  custom_phrase/enable_completion: false\n"
        );
        assert!(schema_block(None, false, false, false, false, None).is_empty());
        // 与 grammar / 简繁共处时三者都在
        let all = schema_block(Some(2), true, true, true, false, None);
        assert!(all.contains("grammar:"));
        assert!(all.contains("custom_phrase/user_dict: listen_hotwords"));
        assert!(all.contains("switches/@2/reset: 1"));
    }

    #[test]
    fn learn_hook_patch_line() {
        let b = schema_block(None, false, false, false, false, Some("C:/a/b/learn_log.tsv"));
        // `/+` 后面必须跟列表项（写标量会让 librime 拒绝整段 patch，进而方案编译失败）
        assert!(
            b.contains("  engine/processors/+:\n    - lua_processor@*listen_learn\n"),
            "实际:\n{b}"
        );
        assert!(b.contains("listen_learn/path: \"C:/a/b/learn_log.tsv\""));
        // 不挂钩子时不能出现 processors 行（否则会重复追加）
        assert!(!schema_block(None, false, false, true, false, None).contains("processors"));
    }
    #[test]
    fn completion_patch_is_explicit() {
        // 打开：写 true；关闭：也要显式写 false（否则上一次部署的 true 会残留）
        let on = schema_block(None, false, false, true, true, None);
        assert!(on.contains("custom_phrase/enable_completion: true"), "{on}");
        let off = schema_block(None, false, false, true, false, None);
        assert!(off.contains("custom_phrase/enable_completion: false"), "{off}");
        // 不带热词表时不碰这个键
        let none = schema_block(None, false, false, false, true, None);
        assert!(!none.contains("enable_completion"), "{none}");
    }
}
