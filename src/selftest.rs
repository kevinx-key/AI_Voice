//! 音频回归自检：ASR 精度（相似度）+ 耗时
//!
//! 用法：`listen --selftest <wav目录>`
//! 若目录内存在 `refs.tsv`（`文件名<TAB>参考文本`），会计算相似度并与阈值比较；
//! 否则只输出识别结果与耗时。

use crate::audio::read_wav_16k;
use crate::config::{Config, Paths};
use anyhow::{Context, Result};
use std::path::Path;
use std::time::Instant;

/// Python 版自检脚本里的参考文本（同名 wav 自动套用）
const BUILTIN_REFS: &[(&str, &str, f64)] = &[
    (
        "far_3.wav",
        "周末要不要去露营，最近天气超舒服，露营？我怕虫子咬，而且晚上睡帐篷会不会很冷啊？放心，我借了专业装备还有暖宝宝，再带点火锅食材，边吃边看星星超惬意。",
        0.90,
    ),
    (
        "noise_en.wav",
        "So what's interesting here is I feel that you know brands knowing this when people sort of speak to the voice assistance at home and if you want to be the brand.",
        0.90,
    ),
    (
        "ja_en_codeswitch.wav",
        "このカフェのwi-fiがアン ステーブル 過ぎて、google meetでディスコネクトされて クライエントに悪い印象を与えてしまった。",
        0.60,
    ),
    (
        "rag_math.wav",
        "对微分形式的积分是微分几何中的基本概念",
        0.90,
    ),
];

/// 剪贴板读写往返（注入链路的前置条件）
fn clipboard_roundtrip() -> Result<()> {
    let mut cb = arboard::Clipboard::new().context("打开剪贴板失败")?;
    let old = cb.get_text().ok();
    let probe = "AI-VOICE-SELFTEST-123";
    cb.set_text(probe.to_string()).context("写入剪贴板失败")?;
    let got = cb.get_text().context("读取剪贴板失败")?;
    if let Some(old) = old.filter(|s| !s.is_empty()) {
        let _ = cb.set_text(old);
    }
    if got != probe {
        anyhow::bail!("读回内容不符: {got:?}");
    }
    Ok(())
}

pub(crate) fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !c.is_whitespace()
                && !"，。？、,.?！!；;：:（）()\"'“”‘’".contains(*c)
        })
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// difflib.SequenceMatcher.ratio 的等价实现（Ratcliff-Obershelp）
pub(crate) fn ratio(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let matched = matched_chars(&a, &b);
    (2.0 * matched as f64) / ((a.len() + b.len()) as f64).max(1.0)
}

fn matched_chars(a: &[char], b: &[char]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    // 最长公共子串（DP）
    let mut prev = vec![0usize; b.len() + 1];
    let mut cur = vec![0usize; b.len() + 1];
    let mut best = (0usize, 0usize, 0usize); // len, a_start, b_start
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            if a[i - 1] == b[j - 1] {
                cur[j] = prev[j - 1] + 1;
                if cur[j] > best.0 {
                    best = (cur[j], i - cur[j], j - cur[j]);
                }
            } else {
                cur[j] = 0;
            }
        }
        std::mem::swap(&mut prev, &mut cur);
        cur.iter_mut().for_each(|v| *v = 0);
    }
    let (len, ai, bi) = best;
    if len == 0 {
        return 0;
    }
    len + matched_chars(&a[..ai], &b[..bi])
        + matched_chars(&a[ai + len..], &b[bi + len..])
}

fn load_refs(dir: &Path) -> Vec<(String, String, f64)> {
    let mut refs = BUILTIN_REFS
        .iter()
        .map(|(n, t, th)| (n.to_string(), t.to_string(), *th))
        .collect::<Vec<_>>();
    let f = dir.join("refs.tsv");
    if let Ok(text) = std::fs::read_to_string(&f) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut it = line.splitn(3, '\t');
            let (Some(name), Some(truth)) = (it.next(), it.next()) else {
                continue;
            };
            let thr = it.next().and_then(|t| t.trim().parse().ok()).unwrap_or(0.90);
            refs.retain(|(n, _, _)| n != name);
            refs.push((name.to_string(), truth.to_string(), thr));
        }
    }
    refs
}

pub fn run(paths: &Paths, cfg: &Config, wav_dir: &Path) -> Result<i32> {
    let mut wavs: Vec<_> = std::fs::read_dir(wav_dir)
        .with_context(|| format!("读取目录失败: {}", wav_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e.eq_ignore_ascii_case("wav")).unwrap_or(false))
        .collect();
    wavs.sort();
    if wavs.is_empty() {
        anyhow::bail!("目录内没有 wav 文件: {}", wav_dir.display());
    }

    let refs = load_refs(wav_dir);

    println!("加载本地 ASR 模型 …");
    let t0 = Instant::now();
    let backend = crate::asr::build(cfg, paths)?;
    println!("模型加载完成，耗时 {:.1}s\n", t0.elapsed().as_secs_f32());

    let (mut pass, mut total) = (0usize, 0usize);
    let mut lines = Vec::new();

    // 剪贴板读写（注入链路的前置条件）
    match clipboard_roundtrip() {
        Ok(()) => println!("[PASS] 剪贴板读写"),
        Err(e) => {
            total += 1;
            println!("[FAIL] 剪贴板读写  {e:#}");
        }
    }

    for wav in &wavs {
        let name = wav
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let samples = match read_wav_16k(wav) {
            Ok(s) => s,
            Err(e) => {
                println!("[SKIP] {name}  {e:#}");
                continue;
            }
        };
        let t = Instant::now();
        let text = match backend.transcribe(&samples, 16000) {
            Ok(t) => t,
            Err(e) => {
                println!("[FAIL] {name}  识别失败: {e:#}");
                total += 1;
                continue;
            }
        };
        let dt = t.elapsed().as_secs_f32();
        let audio_s = samples.len() as f32 / 16000.0;

        if let Some((_, truth, thr)) = refs.iter().find(|(n, _, _)| *n == name) {
            total += 1;
            let r = ratio(&norm(truth), &norm(&text));
            let ok = r >= *thr;
            if ok {
                pass += 1;
            }
            println!(
                "[{}] {name}  相似度 {:.0}%（阈值 {:.0}%） 音频 {:.1}s / 识别 {:.2}s（RTF {:.3}）",
                if ok { "PASS" } else { "FAIL" },
                r * 100.0,
                thr * 100.0,
                audio_s,
                dt,
                dt / audio_s.max(0.01)
            );
        } else {
            println!("[INFO] {name}  音频 {audio_s:.1}s / 识别 {dt:.2}s");
        }
        lines.push(format!("  {name}: {text}"));
    }

    println!("\n---- 识别文本 ----");
    for l in &lines {
        println!("{l}");
    }

    if total > 0 {
        println!("\n===== 自检结果: {pass}/{total} 通过 =====");
        Ok(if pass == total { 0 } else { 1 })
    } else {
        println!("\n===== 无参考文本，仅输出识别结果 =====");
        Ok(0)
    }
}
