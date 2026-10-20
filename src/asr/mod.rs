//! ASR 后端抽象 + 工厂

pub mod cloud;
pub mod funasr_nano;
pub mod local;

use crate::config::{Config, Paths};
use anyhow::Result;
use std::path::PathBuf;

pub trait AsrBackend: Send {
    fn name(&self) -> &str;
    fn transcribe(&self, samples: &[f32], sample_rate: u32) -> Result<String>;
}

/// 本地模型类型是否为 FunASR-Nano
fn is_funasr_nano(model_type: &str) -> bool {
    let t = model_type.trim().replace('_', "-");
    t.eq_ignore_ascii_case("funasr-nano")
}

/// 按配置实例化后端（本地模型常驻，避免每次加载）
pub fn build(cfg: &Config, paths: &Paths) -> Result<Box<dyn AsrBackend>> {
    match cfg.asr.backend.as_str() {
        "cloud" => Ok(Box::new(cloud::CloudAsr::new(
            &cfg.asr.cloud,
            &cfg.asr.language,
        )?)),
        _ if is_funasr_nano(&cfg.asr.model_type) => {
            let dir = paths.resolve(&cfg.asr.funasr_nano_model);
            let terms = hotword_terms(cfg, paths);
            tracing::info!(
                "热词 {} 条（前 {} 条进 FunASR prompt）",
                terms.len(),
                cfg.hotwords.asr_top_n.min(terms.len())
            );
            let hotwords = build_hotwords_string(cfg, &terms);
            Ok(Box::new(funasr_nano::FunAsrNano::new(
                &dir,
                &cfg.asr.language,
                cfg.asr.num_threads,
                hotwords.as_deref(),
            )?))
        }
        _ => {
            let dir = paths.resolve(&cfg.asr.local_model);
            let terms = hotword_terms(cfg, paths);
            tracing::info!(
                "热词 {} 条（{}：热词文件仅 Paraformer 用，本机离线版实际靠术语后处理纠错）",
                terms.len(),
                cfg.asr.model_type
            );
            let hotwords = write_hotwords(paths, cfg, &terms);
            Ok(Box::new(local::LocalAsr::new(
                &dir,
                &cfg.asr.model_type,
                &cfg.asr.language,
                cfg.asr.num_threads,
                hotwords.as_deref(),
                cfg.asr.hotwords_score,
            )?))
        }
    }
}

/// 热词候选（保序、去重）：
/// 1. 用户 `[glossary] terms`（手写补充，最高优先）
/// 2. 启用的热词包（权重降序，见 `hotwords::asr_terms`）
/// 3. `[glossary] aliases` 的目标词（如「欧拉马=Ollama」→ Ollama）
pub fn hotword_terms(cfg: &Config, paths: &Paths) -> Vec<String> {
    let mut terms = crate::hotwords::asr_terms(cfg, paths);
    for a in &cfg.glossary.aliases {
        if let Some((_, v)) = a.split_once('=') {
            let v = v.trim();
            if !v.is_empty() && !terms.iter().any(|t| t.eq_ignore_ascii_case(v)) {
                terms.push(v.to_string());
            }
        }
    }
    terms.retain(|t| !t.trim().is_empty());
    terms
}

/// FunASR-Nano 热词：拼成逗号分隔字符串（走 LLM user prompt）
pub fn build_hotwords_string(cfg: &Config, terms: &[String]) -> Option<String> {
    if terms.is_empty() {
        return None;
    }
    let terms: Vec<String> = terms.to_vec();
    // FunASR-Nano 的 LLM KV 容量有限（本机导出 max_total_len=512）：prompt(含热词) + 音频 token
    // 超限时音频会被截断导致空输出。音频约 16 token/秒，21s ≈ 363，留给热词 ≈ 110 token
    // → 英语技术词 2~3 token/个，默认取 30 条保底不翻车（`[hotwords] asr_top_n` 可调）。
    let max = cfg.hotwords.asr_top_n.max(1);
    let terms = if terms.len() > max {
        tracing::debug!(
            "FunASR-Nano 热词 {} 条超上限（KV 上限），取权重最高的前 {} 条",
            terms.len(),
            max
        );
        terms[..max].to_vec()
    } else {
        terms
    };
    Some(terms.join(","))
}

/// Paraformer 热词文件：把热词表写成「每行一词」供上下文偏置使用
fn write_hotwords(paths: &Paths, cfg: &Config, terms: &[String]) -> Option<PathBuf> {
    if terms.is_empty() || !cfg.asr.model_type.eq_ignore_ascii_case("paraformer") {
        return None;
    }
    let p = paths.root.join("paraformer_hotwords.txt");
    if let Err(e) = std::fs::write(&p, terms.join("\n")) {
        tracing::warn!("写热词文件失败: {e}");
        return None;
    }
    // 旧名（改名前遗留）清掉，别留垃圾
    let _ = std::fs::remove_file(paths.root.join("hotwords.txt"));
    tracing::info!("热词文件已写入 {}（{} 个）", p.display(), terms.len());
    Some(p)
}

/// 本地模型是否就绪：
/// - sensevoice/paraformer：tokens.txt + 任一 *.onnx
/// - funasr-nano：encoder_adaptor/llm/embedding 三个 onnx + tokenizer 目录
pub fn local_model_ready(paths: &Paths, cfg: &Config) -> bool {
    if is_funasr_nano(&cfg.asr.model_type) {
        let dir = paths.resolve(&cfg.asr.funasr_nano_model);
        let has = |stem: &str| {
            std::fs::read_dir(&dir)
                .map(|rd| {
                    rd.flatten().any(|e| {
                        let p = e.path();
                        p.extension().is_some_and(|x| x.eq_ignore_ascii_case("onnx"))
                            && p.file_name()
                                .map(|s| s.to_string_lossy().starts_with(stem))
                                .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        };
        let tok = dir.join("Qwen3-0.6B").join("tokenizer.json").is_file()
            || std::fs::read_dir(&dir)
                .map(|rd| rd.flatten().any(|e| e.path().join("tokenizer.json").is_file()))
                .unwrap_or(false);
        return has("encoder_adaptor") && has("llm") && has("embedding") && tok;
    }
    let dir = paths.resolve(&cfg.asr.local_model);
    if !dir.join("tokens.txt").is_file() {
        return false;
    }
    if dir.join("model.int8.onnx").is_file() || dir.join("model.onnx").is_file() {
        return true;
    }
    std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten().any(|e| {
                e.path()
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("onnx"))
            })
        })
        .unwrap_or(false)
}
