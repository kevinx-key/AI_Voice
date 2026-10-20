//! vibecoding 术语纠错：精确别名替换 + 拼音模糊匹配
//!
//! SenseVoice 在中英混说里会把英文专有名词音译成中文（Ollama→「欧拉马」）。
//! 上游 sherpa-onnx 的 SenseVoice 不支持热词，故在 ASR 输出后做纠错：
//!   1) 精确别名替换（`[glossary] aliases = ["欧拉马=Ollama"]`）
//!   2) 拼音模糊匹配：把中文窗口转拼音，与别名的拼音比对（容忍 1 个编辑距离），
//!      以覆盖没写进别名表的近似音译
//!
//! ⚠️ 模糊匹配必须**保守**（2026-10-20 用户实测「美国客户」→「美Docker户」）：
//! 旧实现对所有别名都开模糊匹配，于是 2 个字的别名「多克」(duoke) 把
//! 「国客」(guoke，只差一个字母) 吃掉了 —— 一句话里普普通通的「客户」被替换成 Docker。
//! 现在只对**≥3 个汉字且拼音≥6 字母**的别名开模糊匹配（欧拉马/克劳德/乌班图…这种
//! 长音译才是它的目标），2 字别名只做精确替换。

use crate::config::Glossary;
use pinyin::ToPinyin;

/// 模糊匹配的最小别名长度（汉字数）：2 字别名太容易撞上常用词，一律不参与模糊匹配
const FUZZY_MIN_CJK: usize = 3;
/// 模糊匹配的最小拼音长度：3 个汉字正常 ≥6 个字母
const FUZZY_MIN_PINYIN: usize = 6;

pub struct TermCorrector {
    /// (别名, 术语)，按别名长度降序（先换长的，避免子串抢先）
    aliases: Vec<(String, String)>,
    /// (别名拼音, 别名汉字数, 术语)
    fuzzy: Vec<(String, usize, String)>,
    /// (错词小写, 正确词) 整词纠正
    corrections: Vec<(String, String)>,
}

impl TermCorrector {
    pub fn new(g: &Glossary) -> Self {
        let mut aliases = Vec::new();
        let mut fuzzy = Vec::new();
        let mut corrections = Vec::new();
        if g.enabled {
            for a in &g.aliases {
                let Some((k, v)) = a.split_once('=') else {
                    continue;
                };
                let (k, v) = (k.trim().to_string(), v.trim().to_string());
                if k.is_empty() || v.is_empty() {
                    continue;
                }
                let len = k.chars().count();
                let py = to_pinyin(&k);
                // 只有「长音译」才够格做模糊匹配（见文件头注释）
                if len >= FUZZY_MIN_CJK && py.chars().count() >= FUZZY_MIN_PINYIN {
                    fuzzy.push((py, len, v.clone()));
                }
                aliases.push((k, v));
            }
            aliases.sort_by(|a, b| b.0.chars().count().cmp(&a.0.chars().count()));
            for c in &g.corrections {
                let Some((k, v)) = c.split_once('=') else {
                    continue;
                };
                let (k, v) = (k.trim(), v.trim());
                if !k.is_empty() && !v.is_empty() && k != v {
                    corrections.push((k.to_ascii_lowercase(), v.to_string()));
                }
            }
        }
        Self {
            aliases,
            fuzzy,
            corrections,
        }
    }

    /// 纠错：整词英文纠正 → 别名替换 → 拼音模糊匹配
    pub fn correct(&self, text: &str) -> String {
        if self.aliases.is_empty() && self.corrections.is_empty() {
            return text.to_string();
        }
        let mut s = apply_word_corrections(text, &self.corrections);
        for (k, v) in &self.aliases {
            if s.contains(k.as_str()) {
                s = s.replace(k.as_str(), v);
            }
        }
        if !self.fuzzy.is_empty() {
            s = self.fuzzy_replace(&s);
        }
        s
    }

    fn fuzzy_replace(&self, text: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::with_capacity(text.len());
        let mut i = 0usize;
        'outer: while i < chars.len() {
            if is_cjk(chars[i]) {
                for (py, len, term) in &self.fuzzy {
                    for w in [*len, len.saturating_sub(1), len + 1] {
                        if w == 0 || i + w > chars.len() {
                            continue;
                        }
                        let win = &chars[i..i + w];
                        if !win.iter().all(|&c| is_cjk(c)) {
                            continue;
                        }
                        let wpy = to_pinyin_chars(win);
                        if wpy.is_empty() {
                            continue;
                        }
                        let hit = wpy == *py
                            || (py.chars().count() >= FUZZY_MIN_PINYIN
                                && (wpy.chars().count() as i32
                                    - py.chars().count() as i32)
                                    .unsigned_abs()
                                    <= 1
                                && levenshtein(&wpy, py) <= 1);
                        if hit {
                            out.push_str(term);
                            i += w;
                            continue 'outer;
                        }
                    }
                }
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }
}

/// 整词纠正：按 ASCII 字母数字/下划线切词，忽略大小写匹配 corrections
fn apply_word_corrections(text: &str, corrections: &[(String, String)]) -> String {
    if corrections.is_empty() {
        return text.to_string();
    }
    fn flush(token: &mut String, out: &mut String, corrections: &[(String, String)]) {
        if token.is_empty() {
            return;
        }
        let lower = token.to_ascii_lowercase();
        match corrections.iter().find(|(k, _)| *k == lower) {
            Some((_, v)) => out.push_str(v),
            None => out.push_str(token),
        }
        token.clear();
    }
    let mut out = String::with_capacity(text.len());
    let mut token = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            token.push(c);
        } else {
            flush(&mut token, &mut out, corrections);
            out.push(c);
        }
    }
    flush(&mut token, &mut out, corrections);
    out
}

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

fn to_pinyin(s: &str) -> String {
    s.chars()
        .filter_map(|c| c.to_pinyin().map(|p| p.plain()))
        .collect()
}

fn to_pinyin_chars(cs: &[char]) -> String {
    cs.iter()
        .filter_map(|c| c.to_pinyin().map(|p| p.plain()))
        .collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Glossary;

    fn g(aliases: &[&str]) -> Glossary {
        Glossary {
            enabled: true,
            terms: vec![],
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            corrections: vec![],
        }
    }

    #[test]
    fn word_correction() {
        let gl = Glossary {
            enabled: true,
            terms: vec![],
            aliases: vec![],
            corrections: vec!["grape=grep".into()],
        };
        let c = TermCorrector::new(&gl);
        assert_eq!(c.correct("use grape to search"), "use grep to search");
        assert_eq!(c.correct("Grape is sweet"), "grep is sweet");
        // 子串不误伤
        assert_eq!(c.correct("grapes are sweet"), "grapes are sweet");
    }

    #[test]
    fn exact_alias() {
        let c = TermCorrector::new(&g(&["欧拉马=Ollama"]));
        assert_eq!(c.correct("我用欧拉马跑模型"), "我用Ollama跑模型");
    }

    #[test]
    fn pinyin_fuzzy() {
        // 别名「欧拉马」(oulama)，输入「哦拉马」(olama) 差 1 → 命中
        let c = TermCorrector::new(&g(&["欧拉马=Ollama"]));
        assert_eq!(c.correct("哦拉马"), "Ollama");
    }

    /// 用户实测回归（2026-10-20）：说「美国客户」被纠成「美Docker户」。
    /// 根因：「多克」(duoke) 这个 2 字别名开了拼音模糊匹配，「国客」(guoke) 只差 1 个字母就被吃掉了。
    /// 修法：模糊匹配只给 ≥3 字、拼音 ≥6 字母的长音译别名；2 字别名只做精确替换。
    #[test]
    fn common_words_are_not_corrupted_by_short_aliases() {
        let gl = Glossary {
            enabled: true,
            terms: vec![],
            aliases: vec!["多克=Docker".into(), "多克尔=Docker".into()],
            corrections: vec![],
        };
        let c = TermCorrector::new(&gl);
        // 日常词一律不许动
        for s in [
            "美国客户",
            "客户",
            "国客",
            "我们的美国客户收到了货款",
            "这个是客户的要求",
        ] {
            assert_eq!(c.correct(s), s, "{s:?} 不该被术语纠错改动");
        }
        // 真写了别名（精确匹配）仍然要能纠
        assert_eq!(c.correct("我用多克跑服务"), "我用Docker跑服务");
        assert_eq!(c.correct("多克尔"), "Docker");
    }

    /// 长音译的模糊匹配不能被上面的收紧误伤
    #[test]
    fn long_transliterations_still_fuzzy_match() {
        let c = TermCorrector::new(&g(&["克劳德=Claude", "欧拉马=Ollama", "尤班图=Ubuntu"]));
        assert_eq!(c.correct("我用克劳德写代码"), "我用Claude写代码");
        assert_eq!(c.correct("我用克劳得写代码"), "我用Claude写代码"); // 差 1
        assert_eq!(c.correct("欧拉马"), "Ollama");
        // 「乌班图」(wubantu) 与「尤班图」(youbantu) 差 2 个字母 → 模糊匹配不该硬掰，
        // 想要它就写进 aliases（`乌班图=Ubuntu`）做精确替换
        assert_eq!(c.correct("乌班图"), "乌班图");
    }
}
