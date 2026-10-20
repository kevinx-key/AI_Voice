//! 云端 ASR：OpenAI 兼容 `/v1/audio/transcriptions`（BYOK）

use super::AsrBackend;
use crate::audio::encode_wav;
use crate::config::CloudAsr as CloudCfg;
use anyhow::{bail, Context, Result};
use reqwest::blocking::multipart::{Form, Part};
use std::time::Duration;

pub struct CloudAsr {
    client: reqwest::blocking::Client,
    base_url: String,
    api_key: String,
    model: String,
    language: String,
}

impl CloudAsr {
    pub fn new(cfg: &CloudCfg, language: &str) -> Result<Self> {
        let base_url = cfg.base_url.trim().to_string();
        if base_url.is_empty() {
            bail!("云端 ASR 未配置 base_url（config.toml → [asr.cloud]）");
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_s.max(1)))
            .build()
            .context("创建 HTTP 客户端失败")?;
        Ok(Self {
            client,
            base_url,
            api_key: cfg.api_key.trim().to_string(),
            model: if cfg.model.trim().is_empty() {
                "whisper-1".to_string()
            } else {
                cfg.model.trim().to_string()
            },
            language: language.trim().to_string(),
        })
    }

    fn endpoint(&self) -> String {
        format!(
            "{}/audio/transcriptions",
            self.base_url.trim_end_matches('/')
        )
    }
}

impl AsrBackend for CloudAsr {
    fn name(&self) -> &str {
        "云端 ASR"
    }

    fn transcribe(&self, samples: &[f32], sample_rate: u32) -> Result<String> {
        let wav = encode_wav(samples, sample_rate)?;
        let part = Part::bytes(wav)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .context("构造 multipart 失败")?;
        let mut form = Form::new()
            .part("file", part)
            .text("model", self.model.clone())
            .text("response_format", "json");
        if !self.language.is_empty() {
            form = form.text("language", self.language.clone());
        }

        let url = self.endpoint();
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .with_context(|| format!("请求云端 ASR 失败: {url}"))?;
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        if !status.is_success() {
            let brief: String = body.chars().take(300).collect();
            bail!("云端 ASR 返回 {status}: {brief}");
        }

        // 兼容 {"text": "..."} 与纯文本两种返回
        let text = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("text").and_then(|t| t.as_str()).map(String::from))
            .unwrap_or_else(|| body.trim().to_string());
        Ok(text.trim().to_string())
    }
}
