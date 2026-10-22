//! 首次开启语音引擎时的「模型自动下载 + 自动配置」（2026-10-22 用户需求）
//!
//! 装完安装包**不带模型**（识别 1.5GB + 整理 2.4GB）。用户第一次打开语音引擎时：
//!   1. 下载 FunASR-Nano（sherpa-onnx release 的 `*.tar.bz2`，约 982MB → 解开约 1.5GB）
//!   2. 下载整理模型（Qwen3-4B GGUF Q4_K_M，约 2.4GB）
//!   3. 写配置（`model_type = funasr-nano`、`funasr_nano_model`、`polish.local.model`）
//!   4. 重载识别后端
//!
//! 全程在 **worker 线程**跑（不看窗口是否打开），进度通过 `Progress` 回调推给界面；
//! 用户点「取消」→ 置取消标志 → 下载循环立刻退出 → **删掉已下载/已解开的文件**（不留半成品）。
//!
//! 为什么用系统 `tar`：Windows 10+ 自带 bsdtar，`tar -xjf` 能直接解 `.tar.bz2`，
//! 不必为了解压链一堆 `bzip2`/`xz` 依赖。

use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// 内置下载地址（可用 `[models] asr_url / gguf_url` 覆盖）
pub const DEFAULT_ASR_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-funasr-nano-fp16-2025-12-30.tar.bz2";
pub const DEFAULT_GGUF_URL: &str =
    "https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf";
/// 解压后的目录名（tar 里就带这一层）
pub const ASR_DIR: &str = "sherpa-onnx-funasr-nano-fp16-2025-12-30";
/// 整理模型统一落成这个文件名（llama.cpp 不认 URL，只认路径）
pub const GGUF_FILE: &str = "Qwen3-4B-Q4_K_M.gguf";

/// 阶段
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// 准备（算大小/建目录）
    Preparing,
    /// 下载识别模型
    Asr,
    /// 解压识别模型
    Extract,
    /// 下载整理模型
    Polish,
    /// 写配置 + 重载
    Configure,
    Done,
    Cancelled,
    Failed,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Preparing => "准备",
            Phase::Asr => "下载识别模型",
            Phase::Extract => "解压识别模型",
            Phase::Polish => "下载整理模型",
            Phase::Configure => "写入配置并加载",
            Phase::Done => "完成",
            Phase::Cancelled => "已取消",
            Phase::Failed => "失败",
        }
    }

    /// 进度条用的总进度权重（0~1 的起点）
    fn base(self) -> f32 {
        match self {
            Phase::Preparing => 0.0,
            Phase::Asr => 0.02,
            Phase::Extract => 0.45,
            Phase::Polish => 0.55,
            Phase::Configure => 0.97,
            _ => 1.0,
        }
    }
}

/// 推给界面的一次进度
#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub phase: Phase,
    pub done: u64,
    pub total: u64,
    /// 当前阶段文案（含速度/剩余）
    pub msg: String,
    /// 总进度 0~1
    pub overall: f32,
}

impl Default for Phase {
    fn default() -> Self {
        Phase::Preparing
    }
}

/// 要装的东西
#[derive(Debug, Clone)]
pub struct Plan {
    pub asr_url: String,
    pub gguf_url: String,
    pub install_polish: bool,
    /// `models\` 根目录
    pub root: PathBuf,
    /// 配置里已指向的识别模型目录（存在且文件齐 → 不用重下）
    pub existing_asr: Option<PathBuf>,
    /// 配置里已指向的整理模型文件
    pub existing_gguf: Option<PathBuf>,
}


/// 从配置算出安装计划（worker 与 GUI 共用，避免两处口径不一致）
pub fn plan_for(cfg: &crate::config::Config, root: &Path) -> Plan {
    let root = if cfg.models.dir.trim().is_empty() {
        root.join("models")
    } else {
        PathBuf::from(cfg.models.dir.trim())
    };
    let existing_asr = {
        let p = cfg.asr.funasr_nano_model.trim();
        if p.is_empty() {
            None // 空 = 标准安装位，下面 asr_dir() 会算出来
        } else {
            let pb = PathBuf::from(p);
            if asr_dir_ready(&pb) { Some(pb) } else { None }
        }
    };
    let existing_gguf = {
        let p = cfg.polish.local.model.trim();
        if p.is_empty() {
            None
        } else {
            let pb = PathBuf::from(p);
            if pb.is_file() { Some(pb) } else { None }
        }
    };
    Plan {
        asr_url: if cfg.models.asr_url.trim().is_empty() {
            DEFAULT_ASR_URL.to_string()
        } else {
            cfg.models.asr_url.clone()
        },
        existing_asr,
        existing_gguf,
        gguf_url: if cfg.models.gguf_url.trim().is_empty() {
            DEFAULT_GGUF_URL.to_string()
        } else {
            cfg.models.gguf_url.clone()
        },
        install_polish: cfg.models.install_polish,
        root,
    }
}

/// 一个目录里的 FunASR-Nano 是否就绪（与 `asr::local_model_ready` 同口径）
pub fn asr_dir_ready(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut has_enc = false;
    let mut has_llm = false;
    let mut has_emb = false;
    let mut has_tok = false;
    for e in rd.flatten() {
        let p = e.path();
        let name = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
        if name.ends_with(".onnx") {
            if name.starts_with("encoder_adaptor") {
                has_enc = true;
            }
            if name.starts_with("llm") {
                has_llm = true;
            }
            if name.starts_with("embedding") {
                has_emb = true;
            }
        }
        if p.is_dir() && p.join("tokenizer.json").is_file() {
            has_tok = true;
        }
    }
    has_enc && has_llm && has_emb && has_tok
}

impl Plan {
    /// 识别模型实际使用的目录：优先配置里已有的，否则标准安装位
    pub fn asr_dir(&self) -> PathBuf {
        self.existing_asr
            .clone()
            .unwrap_or_else(|| self.root.join("asr").join(ASR_DIR))
    }
    /// 整理模型实际使用的文件路径
    pub fn gguf_path(&self) -> PathBuf {
        self.existing_gguf
            .clone()
            .unwrap_or_else(|| self.root.join("llm").join(GGUF_FILE))
    }
    /// 标准安装位（下载/解压目标；不能用 existing_asr，否则会解到用户自己的目录里）
    fn install_asr_dir(&self) -> PathBuf {
        self.root.join("asr").join(ASR_DIR)
    }
}

impl Plan {
    /// 识别模型是否已就绪（3 个 onnx + tokenizer 目录，和运行时判定同口径）
    pub fn asr_ready(&self) -> bool {
        asr_dir_ready(&self.asr_dir())
    }
    pub fn polish_ready(&self) -> bool {
        self.gguf_path().is_file()
    }
    /// 是否需要装（auto_install 时用来判断要不要触发）
    pub fn needed(&self) -> bool {
        !self.asr_ready() || (self.install_polish && !self.polish_ready())
    }
}

/// 下载一个文件到 `dest`（先写 `.part`，完成后改名）
///
/// `expected` = 0 表示不知道大小（用 Content-Length）。`cancel` 每读一块检查一次。
fn fetch(
    url: &str,
    dest: &Path,
    expected: u64,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<u64> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(6 * 3600))
        .build()
        .context("创建下载客户端失败")?;
    let mut resp = client
        .get(url)
        .send()
        .with_context(|| format!("请求失败: {url}"))?;
    if !resp.status().is_success() {
        bail!("HTTP {}: {url}", resp.status());
    }
    let total = resp
        .content_length()
        .filter(|&n| n > 0)
        .unwrap_or(expected)
        .max(1);
    if let Some(d) = dest.parent() {
        std::fs::create_dir_all(d).with_context(|| format!("创建目录失败: {}", d.display()))?;
    }
    let tmp = dest.with_extension("part");
    let mut file =
        std::fs::File::create(&tmp).with_context(|| format!("创建临时文件失败: {}", tmp.display()))?;
    let mut buf = vec![0u8; 1 << 20];
    let mut done: u64 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(file);
            let _ = std::fs::remove_file(&tmp);
            bail!("已取消");
        }
        let n = resp.read(&mut buf).context("读取下载流失败")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).context("写入文件失败")?;
        done += n as u64;
        on_progress(done, total);
    }
    file.flush().context("刷新下载文件失败")?;
    file.sync_all().ok();
    drop(file);
    if done == 0 {
        let _ = std::fs::remove_file(&tmp);
        bail!("下载到 0 字节: {url}");
    }
    if expected > 0 && done != expected {
        let _ = std::fs::remove_file(&tmp);
        bail!("文件长度不符（{done} / 预期 {expected}）: {url}");
    }
    std::fs::rename(&tmp, dest).context("重命名下载文件失败")?;
    Ok(done)
}

/// 解压 `.tar.bz2` 到指定目录（用系统 tar）
fn extract_tar_bz2(archive: &Path, dest: &Path, cancel: &AtomicBool) -> Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("创建目录失败: {}", dest.display()))?;
    if cancel.load(Ordering::Relaxed) {
        bail!("已取消");
    }
    // 关键：**用相对文件名 + 工作目录**，不要把 `K:\...` 直接喂给 tar ——
    // GNU tar（Git 自带、常在 PATH 前面）会把 `K:\path` 当成「远程主机 K: 上的路径」，
    // 报 "Cannot connect to K: resolve failed"。bsdtar 没这问题，但两者都得兼容。
    let archive_name = archive
        .file_name()
        .context("归档路径没有文件名")?
        .to_os_string();
    let workdir = archive.parent().unwrap_or(dest);
    let mut child = std::process::Command::new("tar")
        .current_dir(workdir)
        .arg("-xjf")
        .arg(&archive_name)
        .arg("-C")
        .arg(dest)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("启动 tar 失败（Windows 10+ 自带 tar，检查是否被杀软拦截）")?;
    // 解压期间轮询：取消就杀进程
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("已取消");
        }
        match child.try_wait().context("等待 tar 失败")? {
            Some(st) => break st,
            None => std::thread::sleep(Duration::from_millis(200)),
        }
    };
    if !status.success() {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        bail!("解压失败（tar {status}）: {}", err.trim());
    }
    Ok(())
}

/// 安装（或补齐）模型；`progress` 每块数据都会回调（界面自行节流）
pub fn install(
    plan: &Plan,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<()> {
    let mut p = Progress {
        phase: Phase::Preparing,
        msg: "准备下载…".into(),
        ..Default::default()
    };
    progress(p.clone());

    // ---- 1) 识别模型 ----
    if !plan.asr_ready() {
        let dest_dir = plan.root.join("asr");
        let archive = dest_dir.join(format!("{ASR_DIR}.tar.bz2"));
        let expected = 0; // 服务端给 Content-Length
        let mut last = Instant::now();
        let url = plan.asr_url.clone();
        let mut on = |done: u64, total: u64| {
            if last.elapsed() >= Duration::from_millis(250) {
                last = Instant::now();
                let frac = done as f32 / total.max(1) as f32;
                progress(Progress {
                    phase: Phase::Asr,
                    done,
                    total,
                    overall: Phase::Asr.base() + frac * (Phase::Extract.base() - Phase::Asr.base()),
                    msg: human(done, total),
                });
            }
        };
        fetch(&url, &archive, expected, cancel, &mut on)?;
        progress(Progress {
            phase: Phase::Extract,
            msg: "解压中（约 1.5GB，需要一两分钟）…".into(),
            overall: Phase::Extract.base(),
            ..Default::default()
        });
        extract_tar_bz2(&archive, &dest_dir, cancel)?;
        // 解压后校验；失败则把目标目录删掉再报错，避免留下半截
        if !plan.asr_ready() {
            let _ = std::fs::remove_dir_all(plan.install_asr_dir());
            bail!("解压后没找到模型文件（预期 {}）", plan.install_asr_dir().display());
        }
        let _ = std::fs::remove_file(&archive); // 包解完就删，省 1GB
    }

    // ---- 2) 整理模型 ----
    if plan.install_polish && !plan.polish_ready() {
        let gguf = plan.gguf_path();
        let mut last = Instant::now();
        let url = plan.gguf_url.clone();
        let mut on = |done: u64, total: u64| {
            if last.elapsed() >= Duration::from_millis(250) {
                last = Instant::now();
                let frac = done as f32 / total.max(1) as f32;
                progress(Progress {
                    phase: Phase::Polish,
                    done,
                    total,
                    overall: Phase::Polish.base()
                        + frac * (Phase::Configure.base() - Phase::Polish.base()),
                    msg: human(done, total),
                });
            }
        };
        fetch(&url, &gguf, 0, cancel, &mut on)?;
    }

    progress(Progress {
        phase: Phase::Done,
        overall: 1.0,
        msg: "模型就绪".into(),
        ..Default::default()
    });
    Ok(())
}

fn human(done: u64, total: u64) -> String {
    let pct = (done as f64 / total.max(1) as f64 * 100.0).min(100.0);
    format!(
        "{:.1}%  {:.0} MB / {:.0} MB",
        pct,
        done as f64 / 1048576.0,
        total as f64 / 1048576.0
    )
}

/// 取消后清理：删 `.part`、删本次解出来的目录（连带 tar 包）
pub fn cleanup(plan: &Plan) -> Vec<String> {
    let mut removed = Vec::new();
    let dirs = [plan.root.join("asr"), plan.root.join("llm")];
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            if name.ends_with(".part") || name.ends_with(".tar.bz2") || name == ASR_DIR {
                let ok = if p.is_dir() {
                    std::fs::remove_dir_all(&p).is_ok()
                } else {
                    std::fs::remove_file(&p).is_ok()
                };
                if ok {
                    removed.push(p.display().to_string());
                }
            }
        }
    }
    // 整理模型（可能在下载中/半截）
    let gguf = plan.gguf_path();
    let part = gguf.with_extension("part");
    for p in [gguf, part] {
        if p.is_file() && std::fs::remove_file(&p).is_ok() {
            removed.push(p.display().to_string());
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_in(dir: &Path) -> Plan {
        Plan {
            asr_url: DEFAULT_ASR_URL.into(),
            gguf_url: DEFAULT_GGUF_URL.into(),
            install_polish: true,
            root: dir.to_path_buf(),
            existing_asr: None,
            existing_gguf: None,
        }
    }

    #[test]
    fn phase_progress_is_monotonic() {
        let order = [
            Phase::Preparing,
            Phase::Asr,
            Phase::Extract,
            Phase::Polish,
            Phase::Configure,
            Phase::Done,
        ];
        let mut last = -1.0;
        for p in order {
            let b = p.base();
            assert!(b >= last, "{p:?} 的权重必须递增");
            last = b;
        }
        assert!((Phase::Done.base() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn human_format_has_percent_and_mb() {
        let s = human(512 * 1048576, 1024 * 1048576);
        assert!(s.contains("50.0%"), "{s}");
        assert!(s.contains("512 MB"), "{s}");
        assert!(s.contains("1024 MB"), "{s}");
    }

    #[test]
    fn needed_detects_missing_models() {
        let dir = std::env::temp_dir().join(format!("listen_models_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = plan_in(&dir);
        assert!(p.needed(), "空目录应当需要安装");
        // 造出 asr 的 4 个文件 → 只需整理模型
        std::fs::create_dir_all(p.asr_dir().join("Qwen3-0.6B")).unwrap();
        std::fs::write(p.asr_dir().join("Qwen3-0.6B").join("tokenizer.json"), b"{}").unwrap();
        for f in [
            "llm.fp16.onnx",
            "encoder_adaptor.int8.onnx",
            "embedding.int8.onnx",
        ] {
            std::fs::write(p.asr_dir().join(f), b"x").unwrap();
        }
        assert!(p.asr_ready());
        assert!(p.needed(), "还缺整理模型");
        std::fs::create_dir_all(p.gguf_path().parent().unwrap()).unwrap();
        std::fs::write(p.gguf_path(), b"x").unwrap();
        assert!(!p.needed(), "都齐了就不用装");
        // 不装整理模型时，只有 asr 就够
        let mut q = plan_in(&dir);
        q.install_polish = false;
        std::fs::remove_file(q.gguf_path()).unwrap();
        assert!(!q.needed());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_removes_partial_artifacts_only() {
        let dir = std::env::temp_dir().join(format!("listen_cleanup_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("asr")).unwrap();
        std::fs::create_dir_all(dir.join("llm")).unwrap();
        let p = plan_in(&dir);
        // 半截文件 + 解出来的目录 + 正常的 gguf
        std::fs::write(dir.join("asr").join("x.tar.bz2"), b"x").unwrap();
        std::fs::write(dir.join("asr").join("y.part"), b"x").unwrap();
        std::fs::create_dir_all(p.asr_dir()).unwrap();
        std::fs::write(p.asr_dir().join("llm.fp16.onnx"), b"x").unwrap();
        std::fs::write(p.gguf_path(), b"x").unwrap();
        let removed = cleanup(&p);
        assert!(!removed.is_empty(), "应当删掉一些文件");
        assert!(!p.asr_dir().exists(), "解出来的目录要删掉");
        assert!(!p.gguf_path().exists(), "已下载的整理模型也要删掉（用户取消了）");
        assert!(!dir.join("asr").join("y.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
