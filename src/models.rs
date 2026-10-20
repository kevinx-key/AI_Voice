//! 本地模型管理：缺失时从 hf-mirror 下载（modelscope 兜底），支持手动放置
//!
//! 手动放置：把 `model.int8.onnx` 与 `tokens.txt` 拷进 `models/sense-voice/` 即可。

use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const REPO: &str = "csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17";

/// 镜像优先级：hf-mirror（国内）→ huggingface 官方
const MIRRORS: &[&str] = &[
    "https://hf-mirror.com",
    "https://huggingface.co",
];

pub const FILES: &[(&str, u64)] = &[
    ("model.int8.onnx", 239_233_841),
    ("tokens.txt", 315_894),
];

pub fn model_ready(dir: &Path) -> bool {
    dir.join("model.int8.onnx").is_file() && dir.join("tokens.txt").is_file()
}

/// 确保模型就绪；缺失则下载。`log` 用于向托盘/控制台回报进度。
pub fn ensure_model(dir: &Path, log: &dyn Fn(String)) -> Result<()> {
    if model_ready(dir) {
        log("本地模型已就绪".into());
        return Ok(());
    }
    std::fs::create_dir_all(dir)
        .with_context(|| format!("创建模型目录失败: {}", dir.display()))?;

    let mut last_err: Option<anyhow::Error> = None;
    for base in MIRRORS {
        let mut ok = true;
        for (name, expected) in FILES {
            let dest = dir.join(name);
            if dest.is_file() {
                continue;
            }
            let url = format!("{base}/{REPO}/resolve/main/{name}");
            log(format!("开始下载 {name} …"));
            match download(&url, &dest, *expected, log) {
                Ok(()) => log(format!("{name} 下载完成")),
                Err(e) => {
                    log(format!("{name} 下载失败: {e:#}"));
                    last_err = Some(e);
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            log("本地模型下载完成".into());
            return Ok(());
        }
        log(format!("镜像 {base} 失败，尝试下一个源 …"));
    }

    Err(last_err.unwrap_or_else(|| {
        anyhow::anyhow!(
            "模型下载失败。可手动下载后放入 {}（需要 model.int8.onnx 与 tokens.txt）",
            dir.display()
        )
    }))
}

fn download(url: &str, dest: &Path, expected: u64, log: &dyn Fn(String)) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3600))
        .build()
        .context("创建下载客户端失败")?;
    let mut resp = client
        .get(url)
        .send()
        .with_context(|| format!("请求失败: {url}"))?;
    if !resp.status().is_success() {
        bail!("HTTP {}", resp.status());
    }
    let total = resp
        .content_length()
        .filter(|&n| n > 0)
        .unwrap_or(expected)
        .max(1);

    let tmp: PathBuf = dest.with_extension("part");
    let mut file = std::fs::File::create(&tmp)
        .with_context(|| format!("创建临时文件失败: {}", tmp.display()))?;
    let mut buf = vec![0u8; 1 << 20];
    let mut done: u64 = 0;
    let mut step: u64 = 0;
    loop {
        let n = resp.read(&mut buf).context("读取下载流失败")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).context("写入文件失败")?;
        done += n as u64;
        let pct = done * 100 / total;
        if pct / 10 > step {
            step = pct / 10;
            log(format!("下载中 {pct}%（{} MB / {} MB）", done >> 20, total >> 20));
        }
    }
    file.flush().context("刷新下载文件失败")?;
    file.sync_all().context("同步下载文件失败")?;
    drop(file);

    if done != expected {
        let _ = std::fs::remove_file(&tmp);
        bail!("文件不完整（{done} / 预期 {expected} 字节）");
    }
    std::fs::rename(&tmp, dest).context("重命名下载文件失败")?;
    Ok(())
}

// ---------------------------------------------------------------- 门控 VAD 模型（C1）

/// Silero VAD：门控判据用的语音活动检测模型（~630KB）
///
/// 来源是 sherpa-onnx 官方 release（GitHub），本机直连受限 → 走 release 代理镜像；
/// 这个文件比 sense-voice 小得多，所以除了字节数还校验 **sha256**（防镜像返回错误页/半截文件）。
pub const VAD_FILE: &str = "silero_vad.onnx";
pub const VAD_BYTES: u64 = 643_854;
pub const VAD_SHA256: &str = "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6";
const VAD_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx";
/// 空串 = 直连 GitHub（前面几个都是 release 代理，前缀直接拼在原 URL 前）
const VAD_MIRRORS: &[&str] = &[
    "https://ghfast.top/",
    "https://ghproxy.net/",
    "https://gh-proxy.com/",
    "",
];

/// VAD 模型是否已就绪（大小 + sha256 都对）
pub fn vad_ready(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.len() == VAD_BYTES).unwrap_or(false)
        && sha256_file(path).map(|h| h == VAD_SHA256).unwrap_or(false)
}

fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(path)
        .with_context(|| format!("打开文件失败: {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).context("读取文件失败")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// 确保 VAD 模型就绪（`<root>/models/silero_vad.onnx`）；缺失/损坏则下载。
///
/// 安装包会直接带这份模型（`resources\models\` → `{app}\models\`），所以正常装机**不走下载**；
/// 下载是给「手动放了 exe、没跑安装包」的场景兜底。
pub fn ensure_vad(root: &Path, log: &dyn Fn(String)) -> Result<PathBuf> {
    let dir = root.join("models");
    let dest = dir.join(VAD_FILE);
    if vad_ready(&dest) {
        return Ok(dest);
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("创建目录失败: {}", dir.display()))?;

    let mut last_err: Option<anyhow::Error> = None;
    for prefix in VAD_MIRRORS {
        let url = format!("{prefix}{VAD_URL}");
        log(format!("开始下载 {VAD_FILE}（{} KB）…", VAD_BYTES >> 10));
        match download(&url, &dest, VAD_BYTES, log) {
            Ok(()) => {
                let got = sha256_file(&dest).unwrap_or_default();
                if got == VAD_SHA256 {
                    log("VAD 模型下载完成（sha256 校验通过）".into());
                    return Ok(dest);
                }
                let _ = std::fs::remove_file(&dest);
                let e = anyhow::anyhow!("sha256 不匹配：{got}（预期 {VAD_SHA256}）——已删掉重试下一个源");
                log(format!("校验失败: {e}"));
                last_err = Some(e);
            }
            Err(e) => {
                log(format!("下载失败: {e:#}"));
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        anyhow::anyhow!(
            "VAD 模型不可用。可手动下载 {} 放到 {}",
            VAD_URL,
            dest.display()
        )
    }))
}
