//! 本地 ASR：FunASR-Nano（2025-12，LLM/Qwen3-0.6B 解码，多语种，支持 prompt 热词）
//!
//! 模型目录需含：encoder_adaptor*.onnx + llm*.onnx + embedding*.onnx + Qwen3-0.6B/（tokenizer）
//! 热词：sherpa-onnx 把 comma 分隔的词表拼进 LLM 的 user prompt，读取得越早偏置越稳。

use super::AsrBackend;
use anyhow::{anyhow, Result};
use sherpa_onnx::{
    OfflineFunASRNanoModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
};
use std::path::{Path, PathBuf};

pub struct FunAsrNano {
    recognizer: OfflineRecognizer,
    label: String,
}

/// 在目录里按优先级找 `<stem>.int8.onnx` → `.fp16.onnx` → `.onnx` → 任意 `<stem>*.onnx`
fn find_named(dir: &Path, stem: &str) -> Option<PathBuf> {
    for cand in [
        format!("{stem}.int8.onnx"),
        format!("{stem}.fp16.onnx"),
        format!("{stem}.onnx"),
    ] {
        let p = dir.join(&cand);
        if p.is_file() {
            return Some(p);
        }
    }
    let mut found: Option<PathBuf> = None;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("onnx")) {
                continue;
            }
            let name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            if name.starts_with(stem) {
                found = Some(p);
                break;
            }
        }
    }
    found
}

/// tokenizer 目录：优先 `Qwen3-0.6B/`，否则任一含 `tokenizer.json` 的子目录
fn find_tokenizer_dir(dir: &Path) -> Option<PathBuf> {
    let q = dir.join("Qwen3-0.6B");
    if q.join("tokenizer.json").is_file() {
        return Some(q);
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && p.join("tokenizer.json").is_file() {
                return Some(p);
            }
        }
    }
    None
}

impl FunAsrNano {
    pub fn new(
        model_dir: &Path,
        language: &str,
        num_threads: i32,
        hotwords: Option<&str>,
    ) -> Result<Self> {
        let encoder = find_named(model_dir, "encoder_adaptor")
            .ok_or_else(|| anyhow!("encoder_adaptor*.onnx 缺失: {}", model_dir.display()))?;
        let llm = find_named(model_dir, "llm")
            .ok_or_else(|| anyhow!("llm*.onnx 缺失: {}", model_dir.display()))?;
        let embedding = find_named(model_dir, "embedding")
            .ok_or_else(|| anyhow!("embedding*.onnx 缺失: {}", model_dir.display()))?;
        let tokenizer = find_tokenizer_dir(model_dir).ok_or_else(|| {
            anyhow!("tokenizer 目录缺失（需含 tokenizer.json）: {}", model_dir.display())
        })?;

        let mut config = OfflineRecognizerConfig::default();
        config.model_config.num_threads = num_threads.max(1);
        config.model_config.provider = Some("cpu".to_string());
        config.feat_config.sample_rate = 16000;
        config.feat_config.feature_dim = 80;
        config.model_config.funasr_nano = OfflineFunASRNanoModelConfig {
            encoder_adaptor: Some(encoder.to_string_lossy().into_owned()),
            llm: Some(llm.to_string_lossy().into_owned()),
            embedding: Some(embedding.to_string_lossy().into_owned()),
            tokenizer: Some(tokenizer.to_string_lossy().into_owned()),
            system_prompt: None,
            user_prompt: None,
            max_new_tokens: 256,
            temperature: 1e-6,
            top_p: 0.8,
            seed: 42,
            language: if language.trim().is_empty() {
                None
            } else {
                Some(language.trim().to_string())
            },
            itn: 1,
            hotwords: hotwords.map(|s| s.to_string()),
        };

        let recognizer = OfflineRecognizer::create(&config).ok_or_else(|| {
            anyhow!(
                "创建 FunASR-Nano 识别器失败（模型损坏或路径非法）: {}",
                model_dir.display()
            )
        })?;
        Ok(Self {
            recognizer,
            label: "本地 FunASR-Nano".to_string(),
        })
    }
}

impl AsrBackend for FunAsrNano {
    fn name(&self) -> &str {
        &self.label
    }

    fn transcribe(&self, samples: &[f32], sample_rate: u32) -> Result<String> {
        // 极短音频补静音到 ~1s：避免空结果/幻觉（与 SenseVoice 分支一致）
        let min_len = sample_rate.max(1) as usize;
        let padded: Vec<f32>;
        let samples = if samples.len() < min_len {
            let mut v = samples.to_vec();
            v.resize(min_len, 0.0);
            padded = v;
            padded.as_slice()
        } else {
            samples
        };
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(sample_rate as i32, samples);
        self.recognizer.decode(&stream);
        let text = stream.get_result().map(|r| r.text).unwrap_or_default();
        Ok(text.trim().to_string())
    }
}
