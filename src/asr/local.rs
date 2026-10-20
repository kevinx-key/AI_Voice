//! 本地 ASR：sherpa-onnx
//! - sensevoice：SenseVoice-small int8（默认，中英日韩粤）
//! - paraformer：Paraformer（中英双语，支持热词/上下文偏置 → vibecoding 术语）

use super::AsrBackend;
use anyhow::{anyhow, bail, Result};
use sherpa_onnx::{
    OfflineParaformerModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineSenseVoiceModelConfig,
};
use std::path::{Path, PathBuf};

pub struct LocalAsr {
    recognizer: OfflineRecognizer,
    label: String,
}

/// 在模型目录里找 onnx：model.int8.onnx → model.onnx → 任意 *.onnx
fn find_onnx(dir: &Path) -> Option<PathBuf> {
    for name in ["model.int8.onnx", "model.onnx"] {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    let mut found: Option<PathBuf> = None;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("onnx")) {
                found = Some(p);
                break;
            }
        }
    }
    found
}

impl LocalAsr {
    pub fn new(
        model_dir: &Path,
        model_type: &str,
        language: &str,
        num_threads: i32,
        hotwords_file: Option<&Path>,
        hotwords_score: f32,
    ) -> Result<Self> {
        let model = find_onnx(model_dir)
            .ok_or_else(|| anyhow!("本地模型缺失（{}/*.onnx 不存在）", model_dir.display()))?;
        let tokens = model_dir.join("tokens.txt");
        if !tokens.is_file() {
            bail!("tokens.txt 缺失: {}", tokens.display());
        }

        let mut config = OfflineRecognizerConfig::default();
        config.model_config.tokens = Some(tokens.to_string_lossy().into_owned());
        config.model_config.num_threads = num_threads.max(1);
        config.model_config.provider = Some("cpu".to_string());
        config.feat_config.sample_rate = 16000;
        config.feat_config.feature_dim = 80;

        let label;
        if model_type.eq_ignore_ascii_case("paraformer") {
            config.model_config.paraformer = OfflineParaformerModelConfig {
                model: Some(model.to_string_lossy().into_owned()),
            };
            // 注意：本版 sherpa-onnx 的**离线** Paraformer 只支持 greedy_search；
            // 热词（需 modified_beam_search）仅流式 Paraformer 支持 → 这里不传 hotwords，
            // 英文术语靠 glossary 后处理层（别名 + 拼音模糊 + LLM）纠正。
            let _ = hotwords_file;
            let _ = hotwords_score;
            config.decoding_method = Some("greedy_search".to_string());
            label = "本地 Paraformer（中英）";
        } else {
            let lang = language.trim();
            config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
                model: Some(model.to_string_lossy().into_owned()),
                language: Some(if lang.is_empty() {
                    "auto".to_string()
                } else {
                    lang.to_string()
                }),
                use_itn: true,
            };
            label = "本地 SenseVoice";
        }

        let recognizer = OfflineRecognizer::create(&config).ok_or_else(|| {
            anyhow!(
                "创建识别器失败（模型损坏或路径非法）: {}",
                model.display()
            )
        })?;
        Ok(Self {
            recognizer,
            label: label.to_string(),
        })
    }
}

impl AsrBackend for LocalAsr {
    fn name(&self) -> &str {
        &self.label
    }

    fn transcribe(&self, samples: &[f32], sample_rate: u32) -> Result<String> {
        // 极短音频（一个字/一个音节）补静音到 ~1s：避免空结果/幻觉
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
