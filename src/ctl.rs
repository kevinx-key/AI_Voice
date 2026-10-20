//! ListenCtl：托管模式（`--managed`）下的控制管道服务端（Windows 命名管道）
//!
//! 协议（fork 侧按同一 spec 对接，见 `docs/plans/任务2-统一运行时.md` §2.3）：
//! - 管道 `\\.\pipe\ListenCtl`；UTF-8；`\n` 行协议；一问一答；顺序多连接
//! - 单行上限 4KB；命令逐字：
//!   `PING` → `OK`；`STATUS` → `OK <starting|ready|suspended|disabled> <hold|toggle> <local|cloud> <polish档> <recording|idle>`
//!   （前 4 个字段顺序冻结；最后那个 `recording|idle` 是 2026-10-08 追加的，给 fork 的托盘点灯用）；
//!   `CHAIN` → `OK <k=v 空格分隔的单行链路快照>`（2026-10-09 追加，给 `listen.exe --status` 用，
//!   字段含义见 `src/chain.rs::Chain::wire`）；
//!   `REPAIR` → `OK`（2026-10-09 追加：等同于设置页那个「一键修复」按钮，
//!   给回归/自动化用：`python tools\ctl_client.py REPAIR`）；
//!   `SUSPEND` / `RESUME` → `OK`（幂等）；`SHOW_SETTINGS` → `OK`；
//!   `SET backend <local|cloud>` / `SET polish <tier>` / `SET hotkey_mode <hold|toggle>`
//!   / `SET voice <on|off>` → `OK` | `ERR <reason>`；`QUIT` → `OK` 后退出；未知 → `ERR unknown command`
//!
//! TODO（兜底链第 2 条，暂不实现）：listen 侧轮询前台线程输入配置，连续 3 次
//! 非本输入法 → 自行挂起。原因：HKL 无法区分「倾听输入法」与微软拼音等其他
//! zh-CN 输入法（同一语言布局共用），可靠实现需要 TSF profile GUID（仅 fork 侧
//! 掌握）；主链仍由 fork 的 OnActivated + Deactivate 兜底，故此处留 TODO。

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

/// 托管状态：starting（启动/模型加载中）→ ready → suspended（挂起）
pub const ST_STARTING: u8 = 0;
pub const ST_READY: u8 = 1;
pub const ST_SUSPENDED: u8 = 2;
/// 语音引擎总开关 OFF：引擎已停（进程保留，便于继续操作设置窗）
pub const ST_DISABLED: u8 = 3;
/// 引擎不可用（ASR 初始化失败）：仅用于设置窗状态卡展示
pub const ST_NOT_RUNNING: u8 = 4;

/// 托管运行状态（ctl 线程与 worker 线程写，STATUS 命令读）
pub struct ManagedState {
    state: AtomicU8,
    /// 当前是否在会话里（录音/识别中）。
    ///
    /// 为什么要单独立一位（2026-10-08）：托管模式下**托盘归 fork**，而它只能靠轮询 `STATUS`
    /// 得知状态；原来协议里只有 starting/ready/suspended/disabled，没有“正在录音”，
    /// 于是托管下托盘永不变红，用户唯一反馈是悬浮窗（用户最初把它当成 bug#1）。
    /// 与 `state` 分开存：`state` 表示“引擎可用性”，录音不改变可用性（ready 里才能录）。
    recording: AtomicBool,
}

impl ManagedState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU8::new(ST_STARTING),
            recording: AtomicBool::new(false),
        })
    }

    pub fn get(&self) -> u8 {
        self.state.load(Ordering::SeqCst)
    }

    pub fn set(&self, s: u8) {
        self.state.store(s, Ordering::SeqCst);
    }

    /// 会话开始/结束（返回是否发生变化，便于只记一次日志）
    pub fn set_recording(&self, on: bool) -> bool {
        self.recording.swap(on, Ordering::SeqCst) != on
    }

    pub fn is_recording(&self) -> bool {
        self.recording.load(Ordering::SeqCst)
    }

    /// 协议里的状态字符串（fork 侧按字符串映射托盘文案；未知值一律归 starting）
    pub fn label(&self) -> &'static str {
        match self.get() {
            ST_READY => "ready",
            ST_SUSPENDED => "suspended",
            ST_DISABLED => "disabled",
            _ => "starting",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_flag() {
        let s = ManagedState::new();
        assert!(!s.is_recording());
        assert!(s.set_recording(true), "首次置位应报告变化");
        assert!(s.is_recording());
        assert!(!s.set_recording(true), "重复置位不算变化");
        assert!(s.set_recording(false));
        assert!(!s.is_recording());
    }

    #[test]
    fn state_labels() {
        let s = ManagedState::new();
        assert_eq!(s.label(), "starting");
        s.set(ST_READY);
        assert_eq!(s.label(), "ready");
        s.set(ST_SUSPENDED);
        assert_eq!(s.label(), "suspended");
        s.set(ST_DISABLED);
        assert_eq!(s.label(), "disabled");
        // 协议兼容：未运行对 fork 仍按 starting 上报
        s.set(ST_NOT_RUNNING);
        assert_eq!(s.label(), "starting");
    }
}

#[cfg(windows)]
pub use server::{CtlConfig, start};

/// 客户端：连 ListenCtl 管道一问一答（`listen.exe --status` 用）
///
/// 只管一件事：**在 listen.exe 没跑时优雅返回 None**（那时退化成静态探测）。
/// 读应答必须用 `PeekNamedPipe` 轮询 + 超时（同步管道读没有超时，peer 死了会永久挂住）。
#[cfg(windows)]
pub fn query(cmd: &str, timeout_ms: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_GENERIC_READ, FILE_GENERIC_WRITE, OPEN_EXISTING, ReadFile, WriteFile,
    };
    use windows::Win32::System::Pipes::{PeekNamedPipe, WaitNamedPipeW};
    use windows::core::w;

    let name = w!(r"\\.\pipe\ListenCtl");
    let open = || unsafe {
        CreateFileW(
            name,
            FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
            windows::Win32::Storage::FileSystem::FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        )
    };
    let h = match open() {
        Ok(h) => h,
        // 连不上：服务端可能正忙（也可能是根本没在跑）。WaitNamedPipe 能区分：
        // 管道不存在 → 立刻返回 false；管道忙 → 等它空出来。
        Err(_) => {
            if !unsafe { WaitNamedPipeW(name, timeout_ms) }.as_bool() {
                return None;
            }
            match open() {
                Ok(h) => h,
                Err(_) => return None,
            }
        }
    };

    let line = format!("{cmd}\n");
    let mut written = 0u32;
    let wrote = unsafe { WriteFile(h, Some(line.as_bytes()), Some(&mut written), None) }.is_ok();
    let reply = if !wrote {
        None
    } else {
        let start = std::time::Instant::now();
        let mut out = String::new();
        loop {
            let mut avail = 0u32;
            if unsafe { PeekNamedPipe(h, None, 0, None, Some(&mut avail), None) }.is_err() {
                break None;
            }
            if avail > 0 {
                let mut buf = [0u8; 1024];
                let mut got = 0u32;
                let want = avail.min(buf.len() as u32);
                if unsafe { ReadFile(h, Some(&mut buf[..want as usize]), Some(&mut got), None) }
                    .is_err()
                    || got == 0
                {
                    break None;
                }
                out.push_str(&String::from_utf8_lossy(&buf[..got as usize]));
                if let Some(pos) = out.find('\n') {
                    out.truncate(pos);
                    break Some(out);
                }
                if out.len() > 8192 {
                    break None;
                }
                continue;
            }
            if start.elapsed().as_millis() as u32 >= timeout_ms {
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    unsafe {
        let _ = CloseHandle(h);
    }
    reply
}

#[cfg(windows)]
mod server {
    use super::{ManagedState, ST_DISABLED, ST_READY, ST_STARTING, ST_SUSPENDED};
    use crate::Cmd;
    use crate::chain::ChainState;
    use crate::config::{Config, Paths};
    use crate::hotkey::{self, HotkeyEvent, HotkeySpec, windows::HookHandle};
    use std::sync::mpsc::Sender;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows::Win32::Storage::FileSystem::{
        FlushFileBuffers, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows::core::w;

    /// 管道名（与 fork 侧约定，勿改）
    pub const PIPE_NAME: &str = r"\\.\pipe\ListenCtl";
    /// 单行上限（字节，不含换行）
    const MAX_LINE: usize = 4096;

    pub struct CtlConfig {
        pub paths: Paths,
        pub cmd_tx: Sender<Cmd>,
        pub hk_tx: Sender<(HotkeyEvent, u64)>,
        pub hook: Arc<Mutex<Option<HookHandle>>>,
        pub state: Arc<ManagedState>,
        pub hotkey: HotkeySpec,
        pub exit_minutes: u32,
        /// 全链路快照（worker 每秒写；`CHAIN` 命令读，见 `src/chain.rs`）
        pub chain: ChainState,
    }

    pub fn start(cfg: CtlConfig) {
        match std::thread::Builder::new()
            .name("listen-ctl".into())
            .spawn(move || serve(cfg))
        {
            Ok(_) => tracing::info!("ListenCtl 管道服务线程已启动（{}）", PIPE_NAME),
            Err(e) => tracing::error!("启动 ListenCtl 线程失败: {e}"),
        }
    }

    fn serve(cfg: CtlConfig) {
        let deadline: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        spawn_watchdog(deadline.clone(), cfg.cmd_tx.clone(), cfg.state.clone());

        let h = unsafe {
            CreateNamedPipeW(
                w!(r"\\.\pipe\ListenCtl"),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                MAX_LINE as u32,
                MAX_LINE as u32,
                0,
                None,
            )
        };
        if h == INVALID_HANDLE_VALUE {
            let e = unsafe { GetLastError() };
            tracing::error!("创建 ListenCtl 管道失败（可能被占用），控制服务不可用: {e:?}");
            return; // 记日志继续，不崩溃
        }
        tracing::info!("ListenCtl 管道已就绪: {}", PIPE_NAME);

        loop {
            if let Err(e) = unsafe { ConnectNamedPipe(h, None) } {
                let err = unsafe { GetLastError() };
                // 客户端在 ConnectNamedPipe 之前已连上：视为成功
                if err != ERROR_PIPE_CONNECTED {
                    tracing::warn!("ConnectNamedPipe 失败: {e}（last_error={err:?}）");
                    break;
                }
            }
            handle_conn(h, &cfg, &deadline);
            unsafe {
                let _ = FlushFileBuffers(h);
                let _ = DisconnectNamedPipe(h);
            }
        }
        unsafe {
            let _ = CloseHandle(h);
        }
        tracing::warn!("ListenCtl 服务循环退出");
    }

    /// 处理一条连接：按行读 → 逐行应答，直到客户端断开或超长行
    fn handle_conn(h: HANDLE, cfg: &CtlConfig, deadline: &Mutex<Option<Instant>>) {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let mut n = 0u32;
            if let Err(e) = unsafe { ReadFile(h, Some(&mut chunk[..]), Some(&mut n), None) } {
                tracing::debug!("ListenCtl 客户端断开: {e}");
                return;
            }
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&chunk[..n as usize]);
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                if line.len().saturating_sub(1) > MAX_LINE {
                    let _ = write_line(h, "ERR line too long");
                    return;
                }
                let text = String::from_utf8_lossy(&line[..line.len() - 1]);
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                let (reply, quit) = dispatch(text, cfg, deadline);
                if !write_line(h, &reply) {
                    return;
                }
                if quit {
                    // 让客户端先读到 OK，再优雅退出
                    let _ = unsafe { FlushFileBuffers(h) };
                    std::thread::sleep(Duration::from_millis(150));
                    tracing::info!("收到 QUIT，退出进程");
                    wait_worker_exit(&cfg.cmd_tx);
                    std::process::exit(0);
                }
            }
            if buf.len() > MAX_LINE {
                let _ = write_line(h, "ERR line too long");
                return;
            }
        }
    }

    fn write_line(h: HANDLE, line: &str) -> bool {
        let mut data = line.as_bytes().to_vec();
        data.push(b'\n');
        let mut written = 0u32;
        match unsafe { WriteFile(h, Some(data.as_slice()), Some(&mut written), None) } {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("ListenCtl 写入失败: {e}");
                false
            }
        }
    }

    /// 命令分发 → (应答, 是否退出)
    fn dispatch(line: &str, cfg: &CtlConfig, deadline: &Mutex<Option<Instant>>) -> (String, bool) {
        let mut it = line.split_whitespace();
        let cmd = it.next().unwrap_or("");
        match cmd {
            "PING" => ("OK".into(), false),
            "STATUS" => (status(cfg), false),
            "CHAIN" => (format!("OK {}", cfg.chain.get().wire()), false),
            "REPAIR" => {
                // 一键修复：重装钩子 + 重开麦 + 重载模型（与设置页按钮同一条路径）
                if cfg.cmd_tx.send(Cmd::Repair).is_err() {
                    ("ERR worker 已退出".into(), false)
                } else {
                    ("OK".into(), false)
                }
            }
            "SUSPEND" => {
                do_suspend(cfg, deadline);
                ("OK".into(), false)
            }
            "RESUME" => {
                do_resume(cfg, deadline);
                ("OK".into(), false)
            }
            "SHOW_SETTINGS" => {
                crate::signal_show_settings();
                ("OK".into(), false)
            }
            "QUIT" => ("OK".into(), true),
            "SET" => (do_set(it.next(), it.next(), cfg), false),
            _ => ("ERR unknown command".into(), false),
        }
    }

    /// `STATUS`：`OK <state> <hotkey_mode> <backend> <polish> <recording|idle>`
    ///
    /// ⚠️ **前 4 个字段的顺序与含义不许改**（fork 按序号读，且兼容更短的历史格式）；
    /// 第 5 个 `recording|idle` 是 2026-10-08 追加的：托管模式下托盘归 fork，
    /// 它轮询这一位把图标切成“录音中”。新增字段一律往后接，不要插在中间。
    ///
    /// 值取自磁盘上当前生效配置（SET / 界面保存都会先落盘再热生效）；
    /// 配置读取失败时降级为 `OK <state>`（更短），fork 按 token 数兼容。
    fn status(cfg: &CtlConfig) -> String {
        let base = format!("OK {}", cfg.state.label());
        let Ok(c) = Config::load_or_create(&cfg.paths) else {
            tracing::warn!("STATUS 读取配置失败，降级为 3 token");
            return base;
        };
        let mode = if c.general.is_toggle_mode() {
            "toggle"
        } else {
            "hold"
        };
        let backend = if c.asr.backend == "cloud" {
            "cloud"
        } else {
            "local"
        };
        let rec = if cfg.state.is_recording() {
            "recording"
        } else {
            "idle"
        };
        format!("{} {} {} {} {}", base, mode, backend, c.polish.tier(), rec)
    }

    fn do_suspend(cfg: &CtlConfig, deadline: &Mutex<Option<Instant>>) {
        if cfg.state.get() == ST_DISABLED {
            // 总开关 OFF：引擎已停，且不允许挂起超时自动退（避免设置窗被退出）
            tracing::info!("SUSPEND：语音引擎已关闭（总开关 OFF），忽略");
            return;
        }
        if cfg.state.get() == ST_SUSPENDED {
            tracing::info!("SUSPEND：已处于挂起状态（幂等，忽略）");
            return;
        }
        // 1) worker：取消当前会话（录音则丢弃）；模型与采集流保持常驻
        let _ = cfg.cmd_tx.send(Cmd::Suspend);
        // 2) **不再摘热键钩子**（2026-10-06 改）。
        //    fork 只要“输入法不在前台”就发 SUSPEND，而摘钩子会让 F9 完全没响应
        //    （当天 19 次 SUSPEND / 12 次 RESUME，用户体感就是“用着用着就不灵了”）。
        //    挂起现在只表示“不在输入法上下文”，按下热键即自动恢复并录音。
        cfg.state.set(ST_SUSPENDED);
        arm_timeout(cfg, deadline);
        tracing::info!("SUSPEND：已挂起（钩子与模型常驻，热键仍可用）");
    }

    fn do_resume(cfg: &CtlConfig, deadline: &Mutex<Option<Instant>>) {
        // 取消挂起计时
        if let Ok(mut g) = deadline.lock() {
            *g = None;
        }
        // 总开关 OFF：RESUME 不启动引擎（需在设置窗里重新打开）
        let voice_on = Config::load_or_create(&cfg.paths)
            .map(|c| c.general.voice_enabled)
            .unwrap_or(true);
        if !voice_on {
            cfg.state.set(ST_DISABLED);
            tracing::info!("RESUME：语音引擎总开关为 OFF，忽略");
            return;
        }
        ensure_hook(cfg);
        let _ = cfg.cmd_tx.send(Cmd::Resume);
        if cfg.state.get() != ST_READY {
            cfg.state.set(ST_STARTING);
        }
        tracing::info!("RESUME：钩子就绪，模型异步加载中");
    }

    /// 重装热键钩子（幂等；用磁盘上最新配置的热键）
    fn ensure_hook(cfg: &CtlConfig) {
        let Ok(mut g) = cfg.hook.lock() else { return };
        if g.is_some() {
            return;
        }
        let spec = Config::load_or_create(&cfg.paths)
            .ok()
            .and_then(|c| hotkey::parse(&c.general.hotkey).ok())
            .unwrap_or_else(|| cfg.hotkey.clone());
        match hotkey::spawn(&spec, cfg.hk_tx.clone()) {
            Ok(h) => {
                *g = Some(h);
                tracing::info!("热键钩子已重装: {}", spec.name);
            }
            Err(e) => tracing::error!("热键钩子重装失败: {e:#}"),
        }
    }

    /// `SET`：写配置 + 通知 worker 热生效；无法解析的键/值 → ERR
    fn do_set(key: Option<&str>, val: Option<&str>, cfg: &CtlConfig) -> String {
        let mut c = match Config::load_or_create(&cfg.paths) {
            Ok(c) => c,
            Err(e) => return format!("ERR 读取配置失败: {e}"),
        };
        match (key, val) {
            (Some("backend"), Some("local")) => c.asr.backend = "local".into(),
            (Some("backend"), Some("cloud")) => c.asr.backend = "cloud".into(),
            (Some("backend"), _) => return "ERR invalid backend (local|cloud)".into(),
            (Some("polish"), Some(t)) if crate::tray::POLISH_TIERS.contains(&t) => {
                c.polish.set_tier(t)
            }
            (Some("polish"), _) => return "ERR invalid polish tier".into(),
            (Some("hotkey_mode"), Some(v)) => {
                // 宽容一点：大小写/中文别名都认（历史上写成 "Toggle"/"切换" 会静默失效）
                match v.trim().to_ascii_lowercase().as_str() {
                    "hold" | "按住" => c.general.set_toggle_mode(false),
                    "toggle" | "switch" | "切换" => c.general.set_toggle_mode(true),
                    _ => return "ERR invalid hotkey mode (hold|toggle)".into(),
                }
            }
            (Some("voice"), Some("on")) => c.general.voice_enabled = true,
            (Some("voice"), Some("off")) => c.general.voice_enabled = false,
            (Some("voice"), _) => return "ERR invalid voice (on|off)".into(),
            _ => return "ERR unknown command".into(),
        }
        if let Err(e) = c.save(&cfg.paths) {
            return format!("ERR 保存配置失败: {e}");
        }
        // 热生效：worker 重读配置（backend 切换会重建识别后端；hotkey_mode 下次会话生效）
        let _ = cfg.cmd_tx.send(Cmd::ReloadConfig);
        tracing::info!("SET 已应用: {} = {:?}", key.unwrap_or(""), val);
        "OK".into()
    }

    /// 挂起计时（读磁盘最新配置；0 = 不自动退）
    fn arm_timeout(cfg: &CtlConfig, deadline: &Mutex<Option<Instant>>) {
        let minutes = Config::load_or_create(&cfg.paths)
            .map(|c| c.general.managed_suspend_exit_minutes)
            .unwrap_or(cfg.exit_minutes);
        let Ok(mut g) = deadline.lock() else { return };
        if minutes == 0 {
            *g = None;
            tracing::info!("挂起自动退出已关闭（managed_suspend_exit_minutes = 0）");
        } else {
            *g = Some(Instant::now() + Duration::from_secs(minutes as u64 * 60));
            tracing::info!("挂起计时开始：{minutes} 分钟未恢复将自动退出");
        }
    }

    fn spawn_watchdog(
        deadline: Arc<Mutex<Option<Instant>>>,
        cmd_tx: Sender<Cmd>,
        state: Arc<ManagedState>,
    ) {
        let _ = std::thread::Builder::new()
            .name("ctl-timeout".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_secs(5));
                // 总开关 OFF：不自动退出（设置窗需要保留），并清掉可能残留的计时
                if state.get() == ST_DISABLED {
                    if let Ok(mut g) = deadline.lock() {
                        *g = None;
                    }
                    continue;
                }
                let due = match deadline.lock() {
                    Ok(g) => (*g).map(|d| Instant::now() >= d).unwrap_or(false),
                    Err(_) => false,
                };
                if due {
                    tracing::info!("挂起超时未恢复，自动退出");
                    wait_worker_exit(&cmd_tx);
                    std::process::exit(0);
                }
            });
    }

    /// 等 worker 线程收尾（回收 llama-server 等）：向已断开的通道发送会失败
    fn wait_worker_exit(cmd_tx: &Sender<Cmd>) {
        for _ in 0..20 {
            if cmd_tx.send(Cmd::Quit).is_err() {
                return; // 接收端已断开 = worker 已退出
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
