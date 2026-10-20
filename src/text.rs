//! 文本后处理：去句读标点、保数字规范、结尾空格
//!
//! 动机：SenseVoice 的 ITN 会补标点（。，！？）；用户要「不加标点、改空格」，
//! 但 ITN 同时把口语数字规范成 `3.3`/`18080` 这类写法，必须保留。
//! 因此只去「句读」标点，保留 `.`（小数点/域名）与代码符号（_-/@#等）。

/// 中日韩句读/括号/引号（一律去掉）
fn is_cjk_punct(c: char) -> bool {
    matches!(
        c,
        '，' | '。' | '、' | '！' | '？' | '；' | '：' | '“' | '”' | '‘' | '’'
            | '（' | '）' | '《' | '》' | '〈' | '〉' | '「' | '」' | '『' | '』'
            | '【' | '】' | '〔' | '〕' | '〖' | '〗' | '…' | '—' | '～' | '·'
            | '．' | '｜' | '＼' | '／' | '　'
    )
}

/// 去句读标点：`，。！？；：, . ! ? ; :`（其中 `.`/`,` 夹在 ASCII 字母数字间时保留，
/// 以保住 `3.3V`、`model.int8.onnx`、`127.0.0.1`），并把连续空白压成一个空格。
pub fn strip_punctuation(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        if is_cjk_punct(c) {
            out.push(' '); // 标点处断成空格（用户要「改空格」）
            continue;
        }
        if matches!(c, ',' | '.' | '!' | '?' | ';' | ':') {
            if c == '.' || c == ',' || c == ':' {
                let prev = i.checked_sub(1).and_then(|j| chars.get(j)).copied();
                let next = chars.get(i + 1).copied();
                // 小数点/域名点/千分位/端口号：两侧都是 ASCII 字母数字 → 保留
                if let (Some(p), Some(n)) = (prev, next) {
                    if p.is_ascii_alphanumeric() && n.is_ascii_alphanumeric() {
                        out.push(c);
                        continue;
                    }
                }
            }
            out.push(' '); // 其余句读 → 空格
            continue;
        }
        out.push(c);
    }
    collapse_spaces(&out)
}

/// 连续空白 → 单个空格，并去掉首尾空格
pub fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_whitespace() {
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// 最终注入文本定型：可选去标点 + 可选结尾空格
pub fn finalize(s: &str, strip: bool, trailing_space: bool) -> String {
    let mut t = if strip {
        strip_punctuation(s)
    } else {
        collapse_spaces(s)
    };
    if trailing_space && !t.is_empty() {
        t.push(' ');
    }
    t
}

/// ASR 在**碎片/噪声段**上的典型崩溃形态：同一字符连续堆叠。
///
/// 实测（2026-10-08 装机日志 `app.log.2026-10-08`）3/58 段长这样，而且**全部集中在连点会话**
/// （音频被按键切碎）：
/// ```text
/// 我 k l 这这这这这这这这这这这的这的这的
/// 二十一十的十十十十十号十十十十的据的据的一据证据
/// 这个一个这一一一一一一一一一一一一一的一的东西
/// ```
/// 判据：某字符连续 ≥4 次。正常中文里 4 连重复极罕见；
/// 真正会连写的拟声/笑声已白名单，避免误杀。
///
/// 返回 `(字符, 连续次数)` 供日志说明丢弃原因。
///
/// 注：这是**止血**，不是根治 —— 根因是“碎片音频不该送 ASR”（见 C1：VAD 门控）。
pub fn repeat_hallucination(text: &str) -> Option<(char, usize)> {
    const MIN_RUN: usize = 4;
    /// 拟声/笑声：正常使用就会连续重复，不当作幻觉
    const LEGIT: &[char] = &['哈', '呵', '嘿', '嘻', '噗', '咦'];
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        let mut j = i + 1;
        while j < chars.len() && chars[j] == c {
            j += 1;
        }
        if j - i >= MIN_RUN && !LEGIT.contains(&c) {
            return Some((c, j - i));
        }
        i = j;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_are_caught_but_normal_text_is_not() {
        // 实测的三例（2026-10-08 装机日志，均集中在连点会话）
        assert!(repeat_hallucination("我 k l 这这这这这这这这这这这的这的这的").is_some());
        assert!(repeat_hallucination("二十一十的十十十十十号十十十十的据的据的一据证据").is_some());
        assert!(repeat_hallucination("这个一个这一一一一一一一一一一一一一的一的东西").is_some());
        // 正常语句不能误杀
        assert!(repeat_hallucination("现在我已经把电脑重启了").is_none());
        assert!(repeat_hallucination("这个一个东西").is_none());
        assert!(repeat_hallucination("电压是 3.3V，对吗").is_none());
        // 拟声/笑声白名单：正常使用会连续重复
        assert!(repeat_hallucination("哈哈哈哈哈哈哈").is_none());
    }

    #[test]
    fn strips_sentence_punct_keeps_decimal() {
        assert_eq!(strip_punctuation("你好，世界。"), "你好 世界");
        assert_eq!(strip_punctuation("电压是3.3V，对吗？"), "电压是3.3V 对吗");
        assert_eq!(strip_punctuation("model.int8.onnx"), "model.int8.onnx");
        assert_eq!(strip_punctuation("127.0.0.1:18080"), "127.0.0.1:18080");
        assert_eq!(strip_punctuation("用 VS Code, 和 cargo build。"), "用 VS Code 和 cargo build");
    }

    #[test]
    fn finalize_trailing_space() {
        assert_eq!(finalize("你好。", true, true), "你好 ");
        assert_eq!(finalize("", true, true), "");
    }
}
