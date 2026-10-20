//! 中英混说横评：同一批 wav 依次跑多个本地模型，出「相似度 / 术语命中率 / RTF」表。
//!
//! 用法：`listen --bench <wav目录>`
//! 目录内：`*.wav`（按文件名排序）+ 可选 `ref.tsv`（`文件名<TAB>参考文本<TAB>术语;术语`）。
//! 模型清单来自 `config.toml` 的 `[[bench.models]]`；为空时用出厂清单。

use crate::asr::{funasr_nano::FunAsrNano, local::LocalAsr, AsrBackend};
use crate::audio::read_wav_16k;
use crate::config::{BenchModel, Config, Paths};
use anyhow::{Context, Result};
use std::path::Path;
use std::time::Instant;

struct Case {
    name: String,
    truth: String,
    terms: Vec<String>,
}

/// 读 ref.tsv（`文件名 \t 参考文本 \t 术语1;术语2`；术语列可省）
fn load_cases(dir: &Path) -> Result<Vec<Case>> {
    let f = dir.join("ref.tsv");
    let mut out = Vec::new();
    let text = std::fs::read_to_string(&f)
        .with_context(|| format!("读取参考文件失败: {}（每行：文件名<TAB>参考文本<TAB>术语;术语）", f.display()))?;
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.splitn(3, '\t');
        let (Some(name), Some(truth)) = (it.next(), it.next()) else {
            continue;
        };
        let terms = it
            .next()
            .unwrap_or("")
            .split(';')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        out.push(Case {
            name: name.trim().to_string(),
            truth: truth.to_string(),
            terms,
        });
    }
    Ok(out)
}

/// 术语命中：归一化后子串命中（英文忽略大小写）
fn term_hits(text: &str, terms: &[String]) -> usize {
    let t = crate::selftest::norm(text);
    terms.iter().filter(|term| t.contains(&crate::selftest::norm(term))).count()
}

fn build_backend(m: &BenchModel, paths: &Paths, cfg: &Config) -> Result<Box<dyn AsrBackend>> {
    let dir = paths.resolve(&m.dir);
    let kind = m.kind.trim().replace('_', "-").to_ascii_lowercase();
    if kind == "funasr-nano" {
        let hot = if m.hotwords {
            crate::asr::build_hotwords_string(cfg, &crate::asr::hotword_terms(cfg, paths))
        } else {
            None
        };
        Ok(Box::new(FunAsrNano::new(
            &dir,
            &m.language,
            cfg.asr.num_threads,
            hot.as_deref(),
        )?))
    } else {
        Ok(Box::new(LocalAsr::new(
            &dir,
            &m.kind,
            &m.language,
            cfg.asr.num_threads,
            None,
            0.0,
        )?))
    }
}

struct Score {
    sim: f64,
    hits: usize,
    total_terms: usize,
    secs: f32,
    audio_s: f32,
    text: String,
}

pub fn run(paths: &Paths, cfg: &Config, wav_dir: &Path) -> Result<i32> {
    let cases = load_cases(wav_dir)?;
    if cases.is_empty() {
        anyhow::bail!("ref.tsv 为空或不存在: {}", wav_dir.join("ref.tsv").display());
    }

    let models = if cfg.bench.models.is_empty() {
        crate::config::default_bench_models()
    } else {
        cfg.bench.models.clone()
    };

    println!("==== 中英混说横评：{} 用例 × {} 模型 ====\n", cases.len(), models.len());

    // 每模型对每用例的打分：[model][case]
    let mut scores: Vec<Vec<Option<Score>>> = Vec::new();
    let mut model_names = Vec::new();

    for m in &models {
        let dir = paths.resolve(&m.dir);
        model_names.push(m.name.clone());
        if !dir.is_dir() {
            println!("[SKIP] {}  模型目录不存在: {}", m.name, dir.display());
            scores.push(cases.iter().map(|_| None).collect());
            continue;
        }
        println!("加载 {} …", m.name);
        let t0 = Instant::now();
        let backend = match build_backend(m, paths, cfg) {
            Ok(b) => b,
            Err(e) => {
                println!("[SKIP] {}  加载失败: {e:#}", m.name);
                scores.push(cases.iter().map(|_| None).collect());
                continue;
            }
        };
        println!("  加载完成 {:.1}s（{}）\n", t0.elapsed().as_secs_f32(), backend.name());

        let mut row = Vec::new();
        for c in &cases {
            let wav = wav_dir.join(&c.name);
            let samples = match read_wav_16k(&wav) {
                Ok(s) => s,
                Err(e) => {
                    println!("[SKIP] {} / {}  {e:#}", m.name, c.name);
                    row.push(None);
                    continue;
                }
            };
            let t = Instant::now();
            let text = match backend.transcribe(&samples, 16000) {
                Ok(t) => t,
                Err(e) => {
                    println!("[FAIL] {} / {}  识别失败: {e:#}", m.name, c.name);
                    row.push(None);
                    continue;
                }
            };
            let secs = t.elapsed().as_secs_f32();
            let audio_s = samples.len() as f32 / 16000.0;
            row.push(Some(Score {
                sim: crate::selftest::ratio(
                    &crate::selftest::norm(&c.truth),
                    &crate::selftest::norm(&text),
                ),
                hits: term_hits(&text, &c.terms),
                total_terms: c.terms.len(),
                secs,
                audio_s,
                text,
            }));
        }
        scores.push(row);
    }

    // 逐用例明细
    println!("---- 逐用例明细 ----");
    for (ci, c) in cases.iter().enumerate() {
        println!("\n[{}] 参考: {}", c.name, c.truth);
        if !c.terms.is_empty() {
            println!("     术语({}): {}", c.terms.len(), c.terms.join(" / "));
        }
        for (mi, m) in models.iter().enumerate() {
            match scores.get(mi).and_then(|r| r.get(ci)).and_then(|s| s.as_ref()) {
                Some(s) => {
                    let term = if s.total_terms == 0 {
                        "—".to_string()
                    } else {
                        format!("{}/{}", s.hits, s.total_terms)
                    };
                    println!(
                        "   {:<20} 相似度 {:>3.0}%  术语 {:>4}  RTF {:.3}  → {}",
                        m.name,
                        s.sim * 100.0,
                        term,
                        s.secs / s.audio_s.max(0.01),
                        s.text
                    );
                }
                None => println!("   {:<20} （跳过）", m.name),
            }
        }
    }

    // 汇总
    println!("\n---- 汇总 ----");
    println!("{:<22}{:>10}{:>12}{:>10}{:>10}", "模型", "平均相似度", "术语命中率", "平均RTF", "总耗时");
    for (mi, m) in models.iter().enumerate() {
        let row = &scores[mi];
        let n = row.iter().filter(|s| s.is_some()).count();
        if n == 0 {
            println!("{:<22}{:>10}", m.name, "（无有效结果）");
            continue;
        }
        let (mut sim, mut hits, mut terms, mut rtf, mut total) = (0.0f64, 0usize, 0usize, 0f64, 0f64);
        for s in row.iter().flatten() {
            sim += s.sim;
            hits += s.hits;
            terms += s.total_terms;
            rtf += (s.secs / s.audio_s.max(0.01)) as f64;
            total += s.secs as f64;
        }
        let term_pct = if terms == 0 {
            "—".to_string()
        } else {
            format!("{:.0}%", hits as f64 * 100.0 / terms as f64)
        };
        println!(
            "{:<22}{:>9.0}%{:>12}{:>10.3}{:>9.1}s",
            m.name,
            sim * 100.0 / n as f64,
            term_pct,
            rtf / n as f64,
            total
        );
    }

    Ok(0)
}
