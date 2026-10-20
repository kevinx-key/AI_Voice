//! 文本整理：关闭 / 云端 OpenAI 兼容 chat/completions / 本地 llama-server
//!
//! 约定：任何失败或超时都由**调用方**回退到 ASR 原文，本模块只负责报错。

use crate::config::Polish as PolishCfg;
use crate::llm_server::LlamaServer;
use anyhow::{bail, Context, Result};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub trait Polisher: Send + Sync {
    fn name(&self) -> &str;
    fn polish(&self, text: &str) -> Result<String>;
}

pub struct NoopPolisher;

impl Polisher for NoopPolisher {
    fn name(&self) -> &str {
        "关闭"
    }
    fn polish(&self, text: &str) -> Result<String> {
        Ok(text.to_string())
    }
}

const PROMPT_LIGHT: &str = "\
你是语音输入的文本清洗器。只做最小必要修改：删除「嗯、呃、那个、就是」等口头语，\
修正明显的重复词与同音错别字（含英文同音词，例如 grape→grep、doctor→Docker）。\
**不要添加任何标点符号**（不要句号、逗号、问号等），\
词与词之间用空格分隔；保留数字与英文技术词的原始写法。禁止改写句子结构、\
禁止增删信息、禁止解释、禁止输出任何额外文字。直接输出清洗后的文本。";

const PROMPT_POLISH: &str = "\
你是语音输入的文本整理器。在保留原意与全部信息的前提下，把口语整理成通顺表达：\
删除口头语、理顺语序、修正错别字。**不要添加任何标点符号**，词与词之间用空格分隔；\
保留数字与英文技术词的原始写法。禁止增加原文没有的信息、禁止解释、\
禁止输出任何额外文字。直接输出整理后的文本。";

/// 组装 system 提示词：基础提示 + vibecoding 术语表（提示 LLM 保留/还原英文原词）
///
/// 术语表去重 + 封顶：实测 112 条默认术语会把 system prompt 撑到 **478 token**
/// （llama_server.log 里 prompt eval 477~489），白吃 ctx、每次都白算一遍，
/// 而 ctx 一旦被吃掉，长口述的整理输出就更容易撞 max_tokens/ctx 被截断。
const MAX_PROMPT_TERMS: usize = 60;

fn build_system(mode: &str, terms: &[String]) -> String {
    let base = if mode == "polish" {
        PROMPT_POLISH
    } else {
        PROMPT_LIGHT
    };
    let mut uniq: Vec<&str> = Vec::new();
    for t in terms {
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        if uniq.iter().any(|u| u.eq_ignore_ascii_case(t)) {
            continue;
        }
        uniq.push(t);
        if uniq.len() >= MAX_PROMPT_TERMS {
            break;
        }
    }
    if uniq.is_empty() {
        return base.to_string();
    }
    format!(
        "{base}\n以下是本项目常见的英文技术术语。语音里若涉及它们，请输出这些英文原词，\
不要把英文音译成中文：{}。",
        uniq.join("、")
    )
}

/// 粗估 token 数：CJK 约 1 字 = 1 token，其余按 4 字符 = 1 token
/// （只用来自适应 max_tokens，不要求精确）
fn est_tokens(s: &str) -> u32 {
    let mut cjk = 0u32;
    let mut other = 0u32;
    for c in s.chars() {
        if ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3040}'..='\u{30ff}').contains(&c) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    cjk + other / 4 + 1
}

pub struct CloudPolisher {
    client: reqwest::blocking::Client,
    base_url: String,
    api_key: String,
    model: String,
    system: String,
}

impl CloudPolisher {
    pub fn new(cfg: &PolishCfg, terms: &[String]) -> Result<Self> {
        let base_url = cfg.base_url.trim().to_string();
        if base_url.is_empty() {
            bail!("整理功能未配置 base_url（config.toml → [polish]）");
        }
        let model = cfg.model.trim().to_string();
        if model.is_empty() {
            bail!("整理功能未配置 model（config.toml → [polish]）");
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_s.max(1)))
            .build()
            .context("创建 HTTP 客户端失败")?;
        let system = build_system(&cfg.mode, terms);
        Ok(Self {
            client,
            base_url,
            api_key: cfg.api_key.trim().to_string(),
            model,
            system: system.to_string(),
        })
    }
}

impl Polisher for CloudPolisher {
    fn name(&self) -> &str {
        "云端整理"
    }
    fn polish(&self, text: &str) -> Result<String> {
        if text.trim().is_empty() {
            return Ok(text.to_string());
        }
        let url = format!(
            "{}/chat/completions",
            self.base_url.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "model": self.model,
            "temperature": 0.2,
            "messages": [
                {"role": "system", "content": self.system},
                {"role": "user", "content": text},
            ],
        });
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .with_context(|| format!("请求整理服务失败: {url}"))?;
        let status = resp.status();
        let raw = resp.text().unwrap_or_default();
        if !status.is_success() {
            let brief: String = raw.chars().take(300).collect();
            bail!("整理服务返回 {status}: {brief}");
        }
        let v: serde_json::Value =
            serde_json::from_str(&raw).context("整理服务返回非 JSON")?;
        let out = v
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if out.is_empty() {
            bail!("整理服务返回空结果");
        }
        // 云端也可能因为 max_tokens 被截断：宁可回退原文，也不能静默吞掉后半段
        if v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .and_then(|f| f.as_str())
            == Some("length")
        {
            bail!("整理结果被截断（finish_reason=length）→ 回退原文");
        }
        Ok(out)
    }
}

/// chat_template_kwargs 支持情况：0=未知 1=支持 2=不支持
const KW_UNKNOWN: u8 = 0;
const KW_OK: u8 = 1;
const KW_UNSUPPORTED: u8 = 2;

pub struct LocalPolisher {
    server: Arc<LlamaServer>,
    client: reqwest::blocking::Client,
    system: String,
    temperature: f32,
    top_p: f32,
    max_tokens: u32,
    /// llama-server 的 ctx 上限（用于把 max_tokens 限在预算内，避免撞 ctx 墙返回空）
    ctx: u32,
    disable_thinking: bool,
    kwargs_support: AtomicU8,
}

impl LocalPolisher {
    pub fn new(server: Arc<LlamaServer>, cfg: &PolishCfg, terms: &[String]) -> Result<Self> {
        // 本地模型首请求可能含预热，超时给个下限
        let timeout = Duration::from_secs(cfg.timeout_s.max(20));
        let client = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
            .context("创建 HTTP 客户端失败")?;
        let system = build_system(&cfg.mode, terms);
        let l = &cfg.local;
        Ok(Self {
            server,
            client,
            system: system.to_string(),
            temperature: l.temperature,
            top_p: l.top_p,
            // 兜底写死上限：不设 max_tokens 会撞满 ctx 返回空
            max_tokens: l.max_tokens.clamp(16, 2048),
            ctx: l.ctx.max(512),
            disable_thinking: l.disable_thinking,
            kwargs_support: AtomicU8::new(KW_UNKNOWN),
        })
    }

    fn request(
        &self,
        system: &str,
        text: &str,
        use_kwargs: bool,
        max_tokens: u32,
    ) -> Result<(u16, String)> {
        let mut body = serde_json::json!({
            "model": "local",
            "temperature": self.temperature,
            "top_p": self.top_p,
            "max_tokens": max_tokens,
            "stream": false,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": text},
            ],
        });
        if use_kwargs {
            body["chat_template_kwargs"] = serde_json::json!({"enable_thinking": false});
        }
        let url = format!("{}/chat/completions", self.server.base_url());
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .with_context(|| format!("请求本地大模型失败: {url}"))?;
        let status = resp.status().as_u16();
        let raw = resp.text().unwrap_or_default();
        Ok((status, raw))
    }
}

impl Polisher for LocalPolisher {
    fn name(&self) -> &str {
        "本地大模型"
    }

    fn polish(&self, text: &str) -> Result<String> {
        if text.trim().is_empty() {
            return Ok(text.to_string());
        }
        if !self.server.alive() {
            if self.server.wait_on_first_use() {
                self.server.ensure_started()?;
            } else {
                // 首次降级：后台拉起，本次注入原文
                self.server.spawn_background();
                bail!("本地大模型启动中，本次未整理");
            }
        }

        let mut system = self.system.clone();
        // 输出上限自适应：原实现固定 max_tokens（默认 512），长口述时会被静默截断
        // （曾经发生过：prompt 478 token，输出截在 512，后半段直接没了，还没任何日志）
        let in_tok = est_tokens(text);
        let sys_tok = est_tokens(&system);
        let want = ((in_tok as f64 * 1.5) as u32 + 48).max(self.max_tokens);
        let budget = self.ctx.saturating_sub(sys_tok + in_tok + 64).max(64);
        let max_tokens = want.min(budget).clamp(16, 2048);
        if max_tokens < want {
            tracing::warn!(
                "整理 max_tokens 受 ctx 限制：{want} → {max_tokens}（输入约 {in_tok} token）"
            );
        }
        let mut use_kwargs =
            self.disable_thinking && self.kwargs_support.load(Ordering::SeqCst) != KW_UNSUPPORTED;
        let (mut status, mut raw) = self.request(&system, text, use_kwargs, max_tokens)?;

        // 老版 llama.cpp 不认 chat_template_kwargs → 退回 /no_think 软开关
        if !(200..300).contains(&status)
            && use_kwargs
            && self.kwargs_support.load(Ordering::SeqCst) == KW_UNKNOWN
            && raw.contains("chat_template_kwargs")
        {
            tracing::warn!("llama-server 不支持 chat_template_kwargs，改用 /no_think");
            self.kwargs_support.store(KW_UNSUPPORTED, Ordering::SeqCst);
            use_kwargs = false;
            system = format!("{system}\n/no_think");
            (status, raw) = self.request(&system, text, false, max_tokens)?;
        }
        if !(200..300).contains(&status) {
            let brief: String = raw.chars().take(300).collect();
            bail!("本地大模型返回 {status}: {brief}");
        }
        if use_kwargs {
            self.kwargs_support.store(KW_OK, Ordering::SeqCst);
        }

        let v: serde_json::Value = serde_json::from_str(&raw).context("本地大模型返回非 JSON")?;
        let out = v
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("");
        let out = strip_think(out);
        if out.is_empty() {
            bail!("本地大模型返回空结果（检查 disable_thinking / max_tokens）");
        }
        // 撞上 max_tokens/ctx 被截断：宁可回退原文，也不能静默吞掉后半段
        let finish = v
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .and_then(|f| f.as_str())
            .unwrap_or("");
        if finish == "length" {
            bail!("整理输出被截断（finish_reason=length, max_tokens={max_tokens}）→ 回退原文");
        }
        Ok(out)
    }
}

/// 兜底：剥掉 `<think>…</think>` / `<thinking>…</thinking>`（跨行）
pub fn strip_think(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let Some((i, open_len)) = find_open(rest) else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..i]);
        let after = &rest[i + open_len..];
        match find_close(after) {
            Some((j, close_len)) => rest = &after[j + close_len..],
            None => break, // 未闭合：后面全是思考，丢弃
        }
    }
    out.trim().to_string()
}

fn find_open(s: &str) -> Option<(usize, usize)> {
    let mut idx = 0;
    while let Some(p) = s[idx..].find("<think") {
        let start = idx + p;
        let after = &s[start + 6..];
        if after.starts_with('>') {
            return Some((start, 7));
        }
        if after.starts_with("ing>") {
            return Some((start, 10));
        }
        idx = start + 6;
    }
    None
}

fn find_close(s: &str) -> Option<(usize, usize)> {
    let a = s.find("</think>").map(|p| (p, 8));
    let b = s.find("</thinking>").map(|p| (p, 11));
    match (a, b) {
        (Some(x), Some(y)) => Some(if x.0 <= y.0 { x } else { y }),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::strip_think;

    #[test]
    fn strip_think_works() {
        assert_eq!(strip_think(" 你好"), "你好");
        assert_eq!(strip_think("<think>\n想半天\n</think>结果"), "结果");
        assert_eq!(strip_think("<thinking>想</thinking> 结果 "), "结果");
        assert_eq!(strip_think("前<think>中</think>后"), "前后");
        assert_eq!(strip_think("<think>没闭合"), "");
    }
}
