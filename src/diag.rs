//! 启动诊断：阶段日志 / 卡死现场 / 诊断包
//!
//! 为什么需要它（2026-10-26）：故障机器不在本机、拿不到日志时，程序必须能**自己留下证据**。
//! 约定：
//! - 只用 std，**不依赖 tracing 初始化成功**（日志层本身可能因为目录只读而失败）；
//! - 所有 IO 失败一律忽略——诊断代码绝不能变成新的启动失败点；
//! - 位置固定在 `%LOCALAPPDATA%\Listen\`（总是可写），与安装目录是否只读无关。
//!
//! 产物：
//! - `logs\startup.log`：阶段标记，一行一次；哪一步之后没有下一行，就死在哪一步；
//! - `logs\hang.txt`：首帧超时的现场，下次启动据此切换渲染后端；
//! - `support-<时间>\`：`--support` 导出的诊断包。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();
static LAST_PHASE: Mutex<Option<String>> = Mutex::new(None);
static FIRST_FRAME: AtomicBool = AtomicBool::new(false);

/// 第一帧已经画出来了（看门狗据此取消退出）
pub fn mark_first_frame() {
    if !FIRST_FRAME.swap(true, Ordering::SeqCst) {
        phase("first_frame");
    }
}

pub fn first_frame_seen() -> bool {
    FIRST_FRAME.load(Ordering::SeqCst)
}

/// 可写的用户数据目录：`%LOCALAPPDATA%\Listen`，退化为 `%TEMP%\listen-data`
pub fn data_dir() -> PathBuf {
    DATA_DIR
        .get_or_init(|| {
            if let Some(p) = std::env::var_os("LOCALAPPDATA").filter(|s| !s.is_empty()) {
                return PathBuf::from(p).join("Listen");
            }
            if let Some(p) = std::env::var_os("TEMP").filter(|s| !s.is_empty()) {
                return PathBuf::from(p).join("listen-data");
            }
            PathBuf::from("listen-data")
        })
        .clone()
}

pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

/// 目录是否可写（建目录 + 写探针 + 删探针；不留下文件）
///
/// 这是"安装到 Program Files 后启动即 panic"的判据（见 `项目调试过程.md` §56）。
pub fn dir_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".listen_write_test_{}", std::process::id()));
    match std::fs::write(&probe, b"1") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 记一个启动阶段（同时写 startup.log 和 tracing）
pub fn phase(name: &str) {
    phase_note(name, "");
}

/// 带细节的阶段（细节会写在同一行）
pub fn phase_note(name: &str, detail: &str) {
    if let Ok(mut g) = LAST_PHASE.lock() {
        *g = Some(if detail.is_empty() {
            name.to_string()
        } else {
            format!("{name} {detail}")
        });
    }
    tracing::info!("[boot] {name}{}", if detail.is_empty() { String::new() } else { format!(" {detail}") });
    append(&format!(
        "phase={name}{}{}",
        if detail.is_empty() { "" } else { " " },
        detail
    ));
}

fn append(msg: &str) {
    let dir = logs_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("startup.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(
            f,
            "[{}] pid={} ver={} {}",
            utc_stamp(),
            std::process::id(),
            env!("CARGO_PKG_VERSION"),
            msg
        );
    }
}

pub fn last_phase() -> String {
    LAST_PHASE
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| "unknown".into())
}

/// 首帧看门狗触发时写现场；返回路径（写失败也返回 None，不 panic）
pub fn write_hang(reason: &str, renderer: &str) -> Option<PathBuf> {
    let dir = logs_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("hang.txt");
    let body = format!(
        "time={}\npid={}\nreason={}\nrenderer={}\nlast_phase={}\nexe={}\n",
        utc_stamp(),
        std::process::id(),
        reason,
        renderer,
        last_phase(),
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    );
    std::fs::write(&path, body).ok();
    append(&format!("HANG reason={reason} renderer={renderer}"));
    Some(path)
}

/// 取出上一次的卡死现场（读完改名留档，避免无限循环）
pub fn take_hang() -> Option<String> {
    let path = logs_dir().join("hang.txt");
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::rename(&path, logs_dir().join("hang.prev.txt"));
    Some(text)
}

/// 从 hang.txt 里取 `renderer=` 那一行
pub fn hang_renderer(hang: &str) -> Option<String> {
    hang.lines()
        .find_map(|l| l.strip_prefix("renderer=").map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
}

/// 配置脱敏：只处理**键名**明确的敏感项，避免误伤 `hotkey` / `switch_key` / `max_tokens`
pub fn redact(text: &str) -> String {
    const SENSITIVE: [&str; 5] = ["api_key", "apikey", "token", "secret", "password"];
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        if let Some(eq) = line.find('=') {
            // 取 `=` 左边最后一段（`[polish.cloud] api_key` 这种已是叶子键；`cloud.api_key` 取 api_key）
            let key = line[..eq].trim().trim_matches('"').trim_matches('\'');
            let leaf = key.rsplit('.').next().unwrap_or(key).to_ascii_lowercase();
            if SENSITIVE.contains(&leaf.as_str()) {
                out.push_str(&line[..=eq]);
                out.push_str(" \"***\"");
                out.push('\n');
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// 导出诊断包（复制而不是移动；不包含任何密钥）
pub fn collect_support(root: &Path, config_file: &Path, data_root: &Path) -> std::io::Result<PathBuf> {
    let stamp = utc_stamp().replace([':', '-'], "").replace('T', "-").replace('Z', "");
    let dest = data_dir().join(format!("support-{stamp}"));
    std::fs::create_dir_all(&dest)?;

    let mut readme = String::new();
    readme.push_str("倾听输入法 诊断包\n");
    readme.push_str(&format!("生成时间: {}\n", utc_stamp()));
    readme.push_str(&format!("exe: {}\n", std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default()));
    readme.push_str(&format!("install_root: {} (writable={})\n", root.display(), dir_writable(root)));
    readme.push_str(&format!("data_root: {}\n", data_root.display()));
    readme.push_str("说明: config.redacted.toml 已把 api_key/token/secret/password 替换为 ***\n");
    let _ = std::fs::write(dest.join("README.txt"), readme);

    // 启动阶段日志 + 卡死现场 + 应用日志
    let logs = logs_dir();
    for name in ["startup.log", "hang.txt", "hang.prev.txt", "last_panic.txt", "last_panic.prev.txt"] {
        let _ = std::fs::copy(logs.join(name), dest.join(name));
    }
    if let Ok(rd) = std::fs::read_dir(&logs) {
        let mut app_logs: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("app.log."))
                    .unwrap_or(false)
            })
            .collect();
        app_logs.sort();
        for p in app_logs.iter().rev().take(3) {
            if let Some(name) = p.file_name() {
                let _ = std::fs::copy(p, dest.join(name));
            }
        }
    }
    // 配置（脱敏）
    if let Ok(text) = std::fs::read_to_string(config_file) {
        let _ = std::fs::write(dest.join("config.redacted.toml"), redact(&text));
    }
    Ok(dest)
}

// ---------------------------------------------------------------- 时间戳（UTC，免依赖）

fn utc_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Howard Hinnant 的 days → (year, month, day)
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (y + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_hits_only_sensitive_keys() {
        let src = "[asr.cloud]\napi_key = \"sk-123\"\n[general]\nhotkey = \"f9\"\nswitch_key = \"x\"\nmax_tokens = 1200\n[polish.cloud]\ntoken = \"abc\"\n";
        let out = redact(src);
        assert!(out.contains("api_key = \"***\""), "{out}");
        assert!(out.contains("token = \"***\""), "{out}");
        assert!(out.contains("hotkey = \"f9\""), "hotkey 不能被误伤: {out}");
        assert!(out.contains("switch_key = \"x\""), "switch_key 不能被误伤: {out}");
        assert!(out.contains("max_tokens = 1200"), "max_tokens 不能被误伤: {out}");
        assert!(!out.contains("sk-123"), "{out}");
        assert!(!out.contains("\"abc\""), "{out}");
    }

    #[test]
    fn civil_days_roundtrip_samples() {
        // 1970-01-01 / 2000-03-01 / 2026-10-26
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        let (y, m, d) = civil_from_days(20_753);
        assert_eq!(y, 2026);
        assert!((1..=12).contains(&m));
        assert!((1..=31).contains(&d));
    }

    #[test]
    fn probe_detects_writable_temp() {
        let dir = std::env::temp_dir().join(format!("listen_wtest_{}", std::process::id()));
        assert!(dir_writable(&dir), "临时目录必须可写");
        // 探针文件不能留下
        let left = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten().any(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with(".listen_write_test_")
                })
            })
            .unwrap_or(false);
        assert!(!left, "写探针必须删掉");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn data_dir_is_absolute_or_named() {
        let d = data_dir();
        assert!(d.ends_with("Listen") || d.ends_with("listen-data"), "{d:?}");
    }
}
