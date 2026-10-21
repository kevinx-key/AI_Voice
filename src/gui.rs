//! 设置界面（egui/eframe）
//!
//! 主线程归 eframe，托盘图标在同一个线程创建（winit 的消息循环兼作托盘的消息泵）。
//! 热键钩子、录音/识别工作线程各自独立，通过 channel 通信。

use crate::config::{Config, Paths};
use crate::llm_server::LlmState;
use crate::state::State;
use crate::tray::{self, Tray};
use crate::{Cmd, Evt};
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct DownloadState {
    pub running: bool,
    pub msg: String,
    pub done: bool,
}

/// Rime 部署进度（后台线程写，界面读）
#[derive(Default)]
pub struct RimeState {
    pub running: bool,
    pub msg: String,
    pub done: bool,
    pub ok: bool,
}

/// 悬浮窗文本批次：一段/多段原文 + 该批次整理后的文本（整理关闭时 None）
pub struct FloatBatch {
    pub raws: Vec<String>,
    pub polished: Option<String>,
}

/// 悬浮窗 viewport ID（同进程多视口）
const FLOAT_VIEWPORT_ID: &str = "listen_floating";
/// 悬浮窗标题（同时用于按标题定位窗口句柄做分层半透明）
const FLOAT_WINDOW_TITLE: &str = "倾听输入法 悬浮窗";

/// 「用户改了设置」：标记脏 + 安排 600ms 后自动保存。
///
/// 为什么是宏而不是方法（2026-10-21）：取消「保存并应用」后要把 62 处 `self.dirty = true`
/// 换成「打标记 + 定时保存」，但其中 9 处正借着 `self.cfg` 的 `&mut`（例如
/// `let l = &mut self.cfg.polish.local;`），方法调用会二次借用 `self` 而编译失败；
/// 宏展开成两个**字段赋值**，借用是分开的，没问题。
macro_rules! touch {
    ($s:expr) => {{
        $s.dirty = true;
        $s.autosave_at = Some(Instant::now() + Duration::from_millis(600));
    }};
}
/// 悬浮窗恒定不透明度（0~255）：毛玻璃约 65%
const FLOAT_ALPHA: u8 = 165;

/// 给悬浮窗套「半透明 + 毛玻璃」。返回是否成功。
///
/// 本机 glow(WGL 无 alpha 像素格式) 与 wgpu(Win32 surface 无透明 CompositeAlphaMode)
/// 都拿不到逐像素 alpha 交换链，winit 的 `with_transparent` 因此失效。改用 Windows 常规组合：
///  ① `WS_EX_LAYERED` + `SetLayeredWindowAttributes(LWA_ALPHA)`：整窗恒定半透明（透出背景）
///  ② `SetWindowCompositionAttribute(ACCENT_ENABLE_BLURBEHIND)`：把窗口背后的内容模糊 → 毛玻璃
/// 句柄用「按当前进程 PID 枚举顶层窗口 + 标题」获取（本机 FindWindowW 按标题不稳）。
///
/// 返回 `Err` 时**要说清楚哪一步失败**（2026-10-09）：以前只返回 bool 且不记日志，
/// 导致“悬浮窗变了不透明”这种问题零线索（实测待查时 `WS_EX_LAYERED` 根本没设上）。
#[cfg(windows)]
fn apply_float_transparency(title: &str) -> Result<(), String> {
    use windows::Win32::Foundation::{COLORREF, GetLastError};
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, LWA_ALPHA, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, WS_EX_LAYERED,
        GetWindowLongPtrW,
    };
    let Some(hwnd) = find_process_window(title) else {
        return Err(format!("按 PID+标题未找到窗口「{title}」"));
    };
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let new_ex = (ex as u32 | WS_EX_LAYERED.0) as isize;
        if new_ex != ex {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_ex);
            // 校验：SetWindowLongPtrW 失败会返回 0 且不动样式（旧实现忽略了返回值）
            let after = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            if after & WS_EX_LAYERED.0 as isize == 0 {
                return Err(format!(
                    "设 WS_EX_LAYERED 没生效（ex 0x{ex:X} → 0x{after:X}，LastError={:?}）",
                    GetLastError()
                ));
            }
        }
        if let Err(e) = SetLayeredWindowAttributes(hwnd, COLORREF(0), FLOAT_ALPHA, LWA_ALPHA) {
            return Err(format!("SetLayeredWindowAttributes 失败: {e}"));
        }
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED;
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, flags);
    }
    set_accent_blur(hwnd);
    Ok(())
}

#[cfg(not(windows))]
fn apply_float_transparency(_title: &str) -> Result<(), String> {
    Err("非 Windows 平台不支持".into())
}

#[cfg(windows)]
thread_local! {
    static FIND_TITLE: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    static FIND_RESULT: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

/// 枚举本进程顶层窗口，按标题精确匹配，返回句柄
#[cfg(windows)]
fn find_process_window(title: &str) -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::{HWND, LPARAM, TRUE};
    use windows::core::BOOL;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId,
    };

    unsafe extern "system" fn cb(hwnd: HWND, _l: LPARAM) -> BOOL {
        let mut wpid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut wpid)) };
        if wpid != std::process::id() {
            return TRUE;
        }
        let mut buf = [0u16; 256];
        let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
        if n > 0 {
            let s = String::from_utf16_lossy(&buf[..n as usize]);
            let want = FIND_TITLE.with(|t| t.borrow().clone());
            if s == want {
                FIND_RESULT.with(|r| r.set(hwnd.0 as isize));
                return BOOL(0); // 停止枚举
            }
        }
        TRUE
    }

    FIND_TITLE.with(|t| *t.borrow_mut() = title.to_string());
    FIND_RESULT.with(|r| r.set(0));
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(0));
    }
    let raw = FIND_RESULT.with(|r| r.get());
    if raw == 0 {
        None
    } else {
        Some(HWND(raw as *mut core::ffi::c_void))
    }
}

/// `SetWindowCompositionAttribute(ACCENT_ENABLE_BLURBEHIND)` —— Win10 常规毛玻璃（未导出 API，故按名取址）
#[cfg(windows)]
fn set_accent_blur(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::core::{PCSTR, w};

    #[repr(C)]
    struct AccentPolicy {
        accent_state: i32,
        accent_flags: i32,
        gradient_color: u32,
        animation_id: i32,
    }
    #[repr(C)]
    struct WcaData {
        attribute: i32,
        data: *mut core::ffi::c_void,
        size_of_data: usize,
    }
    type Fn = unsafe extern "system" fn(windows::Win32::Foundation::HWND, *mut WcaData) -> i32;

    unsafe {
        let Ok(h) = GetModuleHandleW(w!("user32.dll")) else {
            return;
        };
        let Some(proc) = GetProcAddress(h, PCSTR(b"SetWindowCompositionAttribute\0".as_ptr())) else {
            return;
        };
        let f: Fn = std::mem::transmute(proc);
        // ACCENT_ENABLE_BLURBEHIND = 3；gradient 全透明（只模糊、不加色，暗底由 Frame 提供）
        let mut policy = AccentPolicy {
            accent_state: 3,
            accent_flags: 2,
            gradient_color: 0x0000_0000,
            animation_id: 0,
        };
        let mut data = WcaData {
            attribute: 19, // WCA_ACCENT_POLICY
            data: (&mut policy as *mut AccentPolicy).cast(),
            size_of_data: std::mem::size_of::<AccentPolicy>(),
        };
        let _ = f(hwnd, &mut data);
    }
}

/// 追加一段原文：当前批已出整理行（或还没有批）则另起一批
fn float_push_segment(batches: &mut Vec<FloatBatch>, text: String, keep: usize) {
    if batches.last().map(|b| b.polished.is_some()).unwrap_or(true) {
        batches.push(FloatBatch {
            raws: Vec::new(),
            polished: None,
        });
    }
    if let Some(b) = batches.last_mut() {
        b.raws.push(text);
    }
    float_trim(batches, keep);
}

/// 整理完成：整理行挂到当前批次（忠实反映"一批覆盖 2~3 段"的注入行为）
fn float_attach_polished(batches: &mut Vec<FloatBatch>, text: String) {
    if batches.is_empty() {
        batches.push(FloatBatch {
            raws: Vec::new(),
            polished: None,
        });
    }
    if let Some(b) = batches.last_mut() {
        b.polished = Some(text);
    }
}

/// 只保留最近 keep 段原文（从最旧处裁）
fn float_trim(batches: &mut Vec<FloatBatch>, keep: usize) {
    let keep = keep.max(1);
    while batches.iter().map(|b| b.raws.len()).sum::<usize>() > keep {
        if batches.is_empty() {
            break;
        }
        if batches[0].raws.len() > 1 {
            batches[0].raws.remove(0);
        } else {
            batches.remove(0);
        }
    }
}

/// 悬浮窗每行的右键菜单：**复制本行**（复制该行完整文本，不受显示截断影响）。
///
/// 为什么不提供「复制选中」：egui 0.35 的 `Label` 选中状态不对外暴露文本
/// （`LabelSelectionState` 只公开 `has_selection`/`clear_selection`），且点击菜单项
/// 会让插件判定“点到别处”而清空选中（`label_text_selection.rs` 的 `delected_everything`）。
/// 选一段复制继续走 egui 原生 Ctrl+C，行为不变。
fn float_row_menu(resp: &egui::Response, text: &str) {
    resp.context_menu(|ui| {
        if ui.button("复制本行").clicked() {
            ui.ctx().copy_text(text.to_owned());
            // 菜单是 PopupCloseBehavior::CloseOnClick（默认），点按钮即自动关闭
        }
    });
}

/// 把悬浮窗位置钳制到**显示器工作区**（逻辑坐标）
///
/// 为什么需要（2026-10-09 用户实测）：记忆位置可能整条压在任务栏下面 ——
/// 实测 `pos_y=989`、窗高 48，而工作区只到逻辑 y=1040（物理 2080 ÷ 缩放 2），
/// 结果按 F9 时窗口“存在但看不见”（Win32 枚举里 `vis=True`，人眼里啥也没有）。
///
/// 取**主显示器**工作区（`SPI_GETWORKAREA`，不必新增 Gdi 特性）；
/// 位置已在主显示器右侧（多屏）时不动，避免把副屏上的窗口拽回主屏。
fn clamp_to_work_area(ctx: &egui::Context, x: f32, y: f32, w: f32, h: f32) -> (f32, f32) {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::RECT;
        use windows::Win32::UI::WindowsAndMessaging::{
            SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
        };
        let mut r = RECT::default();
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some(&mut r as *mut _ as *mut core::ffi::c_void),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        };
        if ok.is_err() {
            return (x, y);
        }
        let ppp = ctx.pixels_per_point().max(0.5);
        let (l, t) = (r.left as f32 / ppp, r.top as f32 / ppp);
        let (rt, bt) = (r.right as f32 / ppp, r.bottom as f32 / ppp);
        if x > rt {
            return (x, y); // 副屏，不干预
        }
        (
            x.clamp(l, (rt - w).max(l)),
            y.clamp(t, (bt - h).max(t)),
        )
    }
    #[cfg(not(windows))]
    {
        let _ = (ctx, w, h);
        (x, y)
    }
}

/// 按字符数截断（0 = 不截断）；截断时末尾加省略号
fn truncate_chars(s: &str, max: usize) -> String {
    if max == 0 {
        return s.to_string();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

pub struct Gui {
    pub paths: Paths,
    pub cfg: Config,
    pub cmd_tx: Sender<Cmd>,
    pub evt_rx: Receiver<Evt>,
    pub tray: Option<Tray>,
    pub state: State,
    pub notes: Vec<String>,
    pub devices: Vec<String>,
    pub download: Arc<Mutex<DownloadState>>,
    pub dirty: bool,
    pub status: String,
    pub quit: bool,
    /// 自动保存：改动后等这么久再落盘（连续改多个控件只写一次）
    autosave_at: Option<Instant>,
    /// 上次成功自动保存的时刻 + 给用户看的一句话（底部条显示）
    last_save_at: Option<Instant>,
    save_msg: String,
    pub show_keys: bool,
    pub last_state_tick: Instant,
    pub last_model_check: Instant,
    pub last_model_ready: bool,
    pub logged_layout: bool,
    pub llm_state: LlmState,
    /// AIVOICE_GUI_PROBE=1 时周期性打印控件 rect（GUI 自动化/布局排查用）
    probe: bool,
    probe_rects: Vec<(&'static str, egui::Rect)>,
    last_probe: Instant,
    /// GUI 帧间隔看门狗：渲染卡顿会让界面“定格”，历史上还曾连累热键转发
    last_frame_at: Instant,
    /// 首启向导模式
    pub wizard: bool,
    /// 外部唤起设置窗：命名事件句柄（裸值，避免 HANDLE 的 Send 限制）
    pub show_event: Option<isize>,
    /// 收到「显示设置窗」请求（事件监听线程置位，界面线程消费）
    pub show_requested: Arc<AtomicBool>,
    show_watcher_started: bool,
    pub rime_state: Arc<Mutex<RimeState>>,
    pub rime_info: crate::rime::WeaselInfo,
    pub last_rime_check: Instant,
    /// 热词管理器：正在编辑的包名（空 = 还没载入过）
    pub hw_pack: String,
    /// 热词管理器：编辑中的词条（只在开页/切换包时读盘，不每帧读）
    pub hw_terms: Vec<crate::hotwords::Term>,
    pub hw_filter: String,
    /// 快捷输入表单：快捷键 / 词语 / 备注（2026-10-21 用户要求「三栏就够，不要权重/类别」）
    pub hw_new_code: String,
    pub hw_new_text: String,
    pub hw_new_note: String,
    /// 列表里选中的行（编辑用）
    hw_pick: Option<usize>,
    pub hw_dirty: bool,
    pub hw_status: String,
    /// 热词管理器：「恢复内置词表」二次确认
    hw_confirm_restore: bool,
    /// 全链路快照（worker 每秒推 `Evt::Chain`；顶部链路状态卡与托盘 tooltip 共用）
    pub chain: crate::chain::Chain,
    /// 悬浮窗：批次文本
    pub float_batches: Vec<FloatBatch>,
    /// 悬浮窗：当前会话开始时刻（None = 无会话）
    float_session: Option<Instant>,
    /// 悬浮窗：会话结束后的隐藏截止时刻
    float_hide_at: Option<Instant>,
    /// 悬浮窗：最近一次会话时长（隐藏延迟期间展示）
    float_last_dur: Duration,
    /// 悬浮窗：最近一条提示（如“本次未出字：录音太短”）
    float_hint: Option<String>,
    /// 悬浮窗：门控实时读数（是否判为语音, 本块电平 RMS）——D3，会话开始时清空
    float_gate: Option<(bool, f32)>,
    /// 悬浮窗 viewport 是否已创建
    float_created: bool,
    /// 半透明套用尝试次数（失败时不刷日志，至多记 3 次）
    float_layer_tries: u8,
    /// 悬浮窗当前是否可见（上一帧实测）——用来决定刷新频率：没人看就不烧 GPU/CPU
    float_visible: bool,
    /// 悬浮窗：最近已知位置（拖动记忆）
    float_pos: Option<(f32, f32)>,
    /// 悬浮窗：内容实测高度（下一帧自适应）
    float_measured_h: Option<f32>,
    /// 悬浮窗：是否已套用分层窗口半透明（LWA_ALPHA）
    float_layered: bool,
    /// 设置主窗是否「希望可见」。eframe 首帧后会无条件 set_visible(true)，
    /// 会盖掉 with_visible(false)；故非显式打开时每帧压制回隐藏。
    visible_wanted: bool,
}

impl Gui {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        paths: Paths,
        cfg: Config,
        cmd_tx: Sender<Cmd>,
        evt_rx: Receiver<Evt>,
        wizard: bool,
        settings: bool,
        show_event: Option<isize>,
    ) -> Self {
        install_cjk_font(&cc.egui_ctx);
        let devices = crate::audio::list_input_devices();
        let model_ready = crate::asr::local_model_ready(&paths, &cfg);
        let ms = tray::MenuState {
            backend: cfg.asr.backend.clone(),
            polish: cfg.polish.tier().to_string(),
            autostart: cfg.general.autostart,
            model_ready,
        };
        let tray = tray::build(&ms).ok();
        let rime_info = crate::rime::detect(&cfg.rime.user_dir);
        Self {
            paths,
            cfg,
            cmd_tx,
            evt_rx,
            tray,
            state: State::Idle,
            notes: Vec::new(),
            devices,
            download: Arc::new(Mutex::new(DownloadState::default())),
            dirty: false,
            status: "就绪".into(),
            quit: false,
            autosave_at: None,
            last_save_at: None,
            save_msg: String::new(),
            show_keys: false,
            last_state_tick: Instant::now(),
            last_model_check: Instant::now(),
            last_model_ready: model_ready,
            logged_layout: false,
            llm_state: LlmState::Unknown,
            probe: probe_enabled(),
            probe_rects: Vec::new(),
            last_probe: Instant::now(),
            last_frame_at: Instant::now(),
            wizard,
            show_event,
            show_requested: Arc::new(AtomicBool::new(settings)),
            show_watcher_started: false,
            rime_state: Arc::new(Mutex::new(RimeState::default())),
            rime_info,
            last_rime_check: Instant::now(),
            hw_pack: String::new(),
            hw_terms: Vec::new(),
            hw_filter: String::new(),
        hw_new_code: String::new(),
        hw_new_text: String::new(),
        hw_new_note: String::new(),
        hw_pick: None,
            hw_dirty: false,
            hw_status: String::new(),
            hw_confirm_restore: false,
            chain: crate::chain::Chain::default(),
            float_batches: Vec::new(),
            float_session: None,
            float_hide_at: None,
            float_last_dur: Duration::ZERO,
            float_hint: None,
            float_gate: None,
            float_created: false,
            float_layer_tries: 0,
            float_visible: false,
            float_pos: None,
            float_measured_h: None,
            float_layered: false,
            visible_wanted: settings || wizard,
        }
    }

    /// 启动「外部唤起」监听线程（只启动一次）：等待命名事件 → 置位 + 请求重绘
    fn start_show_watcher(&mut self, ctx: &egui::Context) {
        if self.show_watcher_started {
            return;
        }
        self.show_watcher_started = true;
        let Some(raw) = self.show_event else {
            return;
        };
        let flag = self.show_requested.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("show-event".into())
            .spawn(move || {
                #[cfg(windows)]
                {
                    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
                    use windows::Win32::System::Threading::{INFINITE, WaitForSingleObject};
                    let h = HANDLE(raw as *mut core::ffi::c_void);
                    loop {
                        let r = unsafe { WaitForSingleObject(h, INFINITE) };
                        if r != WAIT_OBJECT_0 {
                            break;
                        }
                        flag.store(true, Ordering::SeqCst);
                        ctx.request_repaint();
                    }
                }
                #[cfg(not(windows))]
                {
                    let _ = raw;
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("启动 ListenShow 监听线程失败: {e}");
        }
    }

    fn probe_note(&mut self, name: &'static str, rect: egui::Rect) {
        if self.probe {
            self.probe_rects.push((name, rect));
        }
    }

    fn probe_flush(&mut self) {
        if !self.probe || self.probe_rects.is_empty() {
            return;
        }
        if self.last_probe.elapsed() < Duration::from_secs(3) {
            return;
        }
        self.last_probe = Instant::now();
        for (name, r) in &self.probe_rects {
            tracing::info!(
                "PROBE {name} center=({:.0},{:.0}) rect=({:.0},{:.0},{:.0},{:.0})",
                r.center().x,
                r.center().y,
                r.min.x,
                r.min.y,
                r.max.x,
                r.max.y
            );
        }
        self.probe_rects.clear();
    }

    fn pump(&mut self, ctx: &egui::Context) {
        // 热键不再在这里转发（2026-10-07）：已挪到 main.rs 的 hotkey-relay 独立线程。
        // 原实现把 hk_rx→cmd_tx 放在 App::ui 里，GUI 帧一停摆按键就被一起吞掉。

        // 工作线程 → 界面
        while let Ok(ev) = self.evt_rx.try_recv() {
            match ev {
                Evt::State(s) => {
                    // 悬浮窗生命周期：会话开始出现，结束后延迟隐藏
                    match s {
                        State::Recording => {
                            if self.float_session.is_none() {
                                self.float_session = Some(Instant::now());
                                self.float_batches.clear();
                                self.float_hint = None;
                                self.float_gate = None;
                            }
                            self.float_hide_at = None;
                        }
                        State::Idle => {
                            if let Some(t0) = self.float_session.take() {
                                self.float_last_dur = t0.elapsed();
                                let delay =
                                    self.cfg.floating.hide_delay_seconds.clamp(1, 60) as u64;
                                self.float_hide_at =
                                    Some(Instant::now() + Duration::from_secs(delay));
                            }
                        }
                        _ => {}
                    }
                    self.state = s;
                    if let Some(t) = &self.tray {
                        t.set_state(s);
                        // 状态图标与 tooltip 是同一个托盘项：set_state 会把 tooltip 覆盖成
                        // 「倾听输入法 · 待机」，所以这里立刻把链路摘要写回去（2026-10-09）
                        t.set_tooltip(&self.chain.summary());
                    }
                    self.last_state_tick = Instant::now();
                }
                Evt::Note(n) => {
                    // 「已注入 N 字」这类"这一批已经上屏了"的反馈：
                    // ① 悬浮窗把这些文字清掉（不留上一批）② 用一句话告诉用户结果
                    // ③ 重新起算停留时间（用户能看见"已注入"，然后按设置自动消失）
                    if n.starts_with("已注入") {
                        self.float_batches.clear();
                        self.float_hint = Some(n.clone());
                        let delay =
                            self.cfg.floating.hide_delay_seconds.clamp(1, 60) as u64;
                        self.float_hide_at =
                            Some(Instant::now() + Duration::from_secs(delay));
                    }
                    self.status = n.clone();
                    self.notes.push(n);
                    if self.notes.len() > 60 {
                        self.notes.remove(0);
                    }
                }
                Evt::Hint(h) => {
                    // “按了没反应”必须有可见反馈：悬浮窗 + 日志页都写
                    self.status = h.clone();
                    self.notes.push(h.clone());
                    if self.notes.len() > 60 {
                        self.notes.remove(0);
                    }
                    self.float_hint = Some(h);
                    let delay = self.cfg.floating.hide_delay_seconds.clamp(1, 60) as u64;
                    self.float_hide_at = Some(Instant::now() + Duration::from_secs(delay));
                }
                Evt::Config(c) => {
                    // 打字途中不要用磁盘值盖掉输入框：自动保存后 worker 会回推 Config，
                    // 而用户可能正把热键改成一半（"righ" 这种非法值不会落盘，落盘的是旧的 f9），
                    // 一盖就等于把他正在输的字符吃掉（2026-10-21 取消保存按钮后新增的风险）。
                    let typing = ctx.memory(|m| m.focused().is_some());
                    if !self.dirty && !typing {
                        self.cfg = *c;
                    }
                }
                Evt::LlmState(s) => {
                    self.status = format!("本地大模型：{}", s.label());
                    self.llm_state = s;
                }
                Evt::Segment { text } => {
                    let keep = self.cfg.floating.history.clamp(1, 10) as usize;
                    float_push_segment(&mut self.float_batches, text, keep);
                }
                Evt::Polished { text } => {
                    float_attach_polished(&mut self.float_batches, text);
                }
                // 门控实时读数（worker 已节流），只是覆盖一个值，不做任何分配
                Evt::Gate { speech, rms } => {
                    self.float_gate = Some((speech, rms));
                }
                Evt::Chain(c) => {
                    // 全链路快照：顶部卡片 + 托盘 tooltip 一起刷新
                    self.chain = *c;
                    if let Some(t) = &self.tray {
                        t.set_tooltip(&self.chain.summary());
                    }
                }
            }
        }

        // 托盘事件
        while let Ok(ev) = tray_icon::TrayIconEvent::receiver().try_recv() {
            tracing::debug!("托盘事件: {ev:?}");
        }
        while let Ok(ev) = tray_icon::menu::MenuEvent::receiver().try_recv() {
            let id = ev.id.0.clone();
            self.on_menu(&id, ctx);
        }

        // 模型就绪状态刷新（下载完成后菜单项要变灰）
        if self.last_model_check.elapsed() > Duration::from_secs(2) {
            self.last_model_check = Instant::now();
            let ready = crate::asr::local_model_ready(&self.paths, &self.cfg);
            if ready != self.last_model_ready {
                self.last_model_ready = ready;
                if let Some(t) = &self.tray {
                    t.set_model_ready(ready);
                }
            }
        }

        // 输入法环境刷新（注册表/目录扫描，别每帧做）
        if self.last_rime_check.elapsed() > Duration::from_secs(3) {
            self.last_rime_check = Instant::now();
            self.rime_info = crate::rime::detect(&self.cfg.rime.user_dir);
        }
    }

    fn start_rime_deploy(&mut self, install: bool) {
        let st = self.rime_state.clone();
        {
            let mut g = st.lock().unwrap();
            if g.running {
                return;
            }
            g.running = true;
            g.done = false;
            g.ok = false;
            g.msg = "准备部署…".into();
        }
        let paths = self.paths.clone();
        let cfg = self.cfg.clone();
        std::thread::spawn(move || {
            let st2 = st.clone();
            let mut log = move |m: String| {
                tracing::info!("[rime] {m}");
                if let Ok(mut g) = st2.lock() {
                    g.msg = m;
                }
            };
            let r = crate::rime_deploy_logged(&paths, &cfg, install, &mut log);
            let ok = r.is_ok();
            if let Ok(mut g) = st.lock() {
                g.running = false;
                g.done = true;
                g.ok = ok;
                g.msg = match r {
                    Ok(_) => "部署完成".into(),
                    Err(e) => format!("部署失败: {e:#}"),
                };
            }
        });
        self.status = "开始部署输入法…".into();
    }

    fn on_menu(&mut self, id: &str, ctx: &egui::Context) {
        match id {
            tray::ID_SHOW => {
                self.visible_wanted = true;
                show_window(ctx);
            }
            tray::ID_BACKEND_LOCAL => {
                self.cfg.asr.backend = "local".into();
                self.apply(ctx);
            }
            tray::ID_BACKEND_CLOUD => {
                self.cfg.asr.backend = "cloud".into();
                self.apply(ctx);
            }
            tray::ID_POLISH_OFF
            | tray::ID_POLISH_LOCAL_LIGHT
            | tray::ID_POLISH_LOCAL_POLISH
            | tray::ID_POLISH_CLOUD_LIGHT
            | tray::ID_POLISH_CLOUD_POLISH => {
                self.cfg.polish.set_tier(id);
                self.apply(ctx);
            }
            tray::ID_DOWNLOAD_MODEL => self.start_download(),
            tray::ID_AUTOSTART => {
                let on = !self.cfg.general.autostart;
                match crate::set_autostart(on) {
                    Ok(()) => {
                        self.cfg.general.autostart = on;
                        if let Some(t) = &self.tray {
                            t.set_autostart(on);
                        }
                        self.apply(ctx);
                    }
                    Err(e) => self.status = format!("设置开机自启失败: {e:#}"),
                }
            }
            tray::ID_OPEN_CONFIG => crate::open_path(&self.paths.config_file),
            tray::ID_OPEN_LOGS => crate::open_path(&self.paths.logs_dir()),
            tray::ID_QUIT => {
                self.quit = true;
                let _ = self.cmd_tx.send(Cmd::Quit);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            _ => {}
        }
    }



    /// 每帧调用：到点就自动保存 + 热生效（去掉按钮后的唯一保存入口）
    fn autosave_tick(&mut self, ctx: &egui::Context) {
        let Some(at) = self.autosave_at else { return };
        let now = Instant::now();
        if now < at {
            // 到点前先约好重绘，别让窗口闲着等不到这一帧
            ctx.request_repaint_after(at - now);
            return;
        }
        self.autosave_at = None;
        self.save_now();
    }

    /// 真正写盘：热键无效时**不写坏值**（保留磁盘上那个有效热键），其它改动照常保存
    fn save_now(&mut self) -> bool {
        // 热键无效时不写坏值：保留磁盘上那个有效热键（其余改动照常保存）
        let disk = Config::load_or_create(&self.paths).ok();
        let (out, hotkey_fixed) =
            crate::config::normalize_before_save(&self.cfg, disk.as_ref());
        match out.save(&self.paths) {
            Ok(()) => {
                self.dirty = false;
                self.last_save_at = Some(Instant::now());
                self.refresh_tray_hotkey();
                let _ = self.cmd_tx.send(Cmd::ReloadConfig);
                self.save_msg = if hotkey_fixed {
                    format!(
                        "已自动保存 {}（热键「{}」无效，保留了上一个有效值）",
                        now_hms(),
                        out.general.hotkey
                    )
                } else {
                    format!("已自动保存 {}", now_hms())
                };
                self.status = self.save_msg.clone();
                true
            }
            Err(e) => {
                self.save_msg = format!("保存失败: {e:#}");
                self.status = self.save_msg.clone();
                tracing::error!("自动保存失败: {e:#}");
                false
            }
        }
    }

    /// 写盘 + 通知工作线程热更新（托盘菜单这类「一次性动作」直接用它）
    fn apply(&mut self, _ctx: &egui::Context) {
        self.save_now();
    }

    /// 把「行为开关」类设置**立即**写盘并热生效（不需要点保存）
    ///
    /// 目前包含两项：`hotkey_mode`（录音方式）与 `flush_seconds`（出字间隔）。
    /// 都是「改完就想马上试」的东西，而用户实测过两次「在设置页选了、回去试却没变」——
    /// 因为这两项以前只置 `dirty`，不点「💾 保存并应用」等于没改（2026-10-20）。
    ///
    /// 实现上以**磁盘配置为基线、只 patch 这两个字段**：即使用户还在别的分区里改了一半
    /// （比如填到一半的模型路径），也不会被顺手落盘。
    fn apply_instant_fields(&mut self, ctx: &egui::Context) {
        let mut disk = Config::load_or_create(&self.paths).unwrap_or_else(|_| self.cfg.clone());
        disk.general.hotkey_mode = self.cfg.general.hotkey_mode.clone();
        disk.general.flush_seconds = self.cfg.general.flush_seconds;
        if let Err(e) = disk.save(&self.paths) {
            self.status = format!("保存失败: {e:#}");
            return;
        }
        let _ = self.cmd_tx.send(Cmd::ReloadConfig);
        let label = if self.cfg.general.is_toggle_mode() {
            "切换（按一下开始 / 再按一下停止）"
        } else {
            "按住说话"
        };
        self.status = format!(
            "录音方式「{label}」· 出字间隔「{}」—— 都已立即生效",
            self.cfg.general.flush_label()
        );
        self.notes.push(self.status.clone());
        if self.notes.len() > 60 {
            self.notes.remove(0);
        }
        tracing::info!(
            "设置页：录音方式「{label}」+ 出字间隔「{}」，已落盘并热生效",
            self.cfg.general.flush_label()
        );
        ctx.request_repaint();
    }

    /// 把当前 cfg 推给托盘勾选与热键（不动磁盘、不通知工作线程）
    fn refresh_tray_hotkey(&self) {
        if let Ok(spec) = crate::hotkey::parse(&self.cfg.general.hotkey) {
            crate::hotkey::set_hotkey(spec.vk, spec.mods);
        }
        if let Some(t) = &self.tray {
            t.set_backend(&self.cfg.asr.backend);
            t.set_polish(self.cfg.polish.tier());
            t.set_autostart(self.cfg.general.autostart);
        }
    }

    fn start_download(&mut self) {
        let dir = self.paths.resolve(&self.cfg.asr.local_model);
        let st = self.download.clone();
        let cmd_tx = self.cmd_tx.clone();
        {
            let mut g = st.lock().unwrap();
            if g.running {
                return;
            }
            g.running = true;
            g.done = false;
            g.msg = "准备下载…".into();
        }
        std::thread::spawn(move || {
            let log = {
                let st = st.clone();
                move |m: String| {
                    if let Ok(mut g) = st.lock() {
                        g.msg = m;
                    }
                }
            };
            let r = crate::models::ensure_model(&dir, &log);
            let ok = r.is_ok();
            if let Ok(mut g) = st.lock() {
                g.running = false;
                g.done = ok;
                g.msg = match r {
                    Ok(()) => "本地模型已就绪".into(),
                    Err(e) => format!("下载失败: {e:#}"),
                };
            }
            if ok {
                // 让工作线程重建识别后端
                let _ = cmd_tx.send(Cmd::ReloadConfig);
            }
        });
        self.status = "开始下载本地模型…".into();
    }

    fn draw(&mut self, ui: &mut egui::Ui) {
        // 顶部状态条
        egui::Panel::top("hdr").show(ui, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let (color, label) = state_visual(self.state);
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 6.0, color);
                ui.label(egui::RichText::new(label).strong());
                ui.separator();
                ui.label(&self.status);
            });
            ui.add_space(2.0);
        });

        // 顶部「链路状态」卡（始终可见，2026-10-09：8 环灯 + 一键修复）
        egui::Panel::top("chain_card").show(ui, |ui| {
            ui.add_space(2.0);
            self.chain_card(ui);
            ui.add_space(2.0);
        });

        // 底部操作条
        let bottom = egui::Panel::bottom("btns").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                // 2026-10-21：**取消「保存并应用」按钮** —— 这个界面上所有设置改完立即自动保存
                // + 热生效（`touch()` 打标记，600ms 后 `autosave_tick()` 落盘）。
                // 留一个状态字：让人知道「已经存了 / 正在存 / 存失败」。
                if self.dirty {
                    ui.colored_label(
                        egui::Color32::from_rgb(230, 160, 40),
                        "● 正在自动保存…",
                    );
                } else if !self.save_msg.is_empty() {
                    ui.weak(&self.save_msg);
                } else {
                    ui.weak("改动即时生效（自动保存）");
                }
                if ui.button("↺ 重新载入").clicked() {
                    match Config::load_or_create(&self.paths) {
                        Ok(c) => {
                            self.cfg = c;
                            self.dirty = false;
                            self.refresh_tray_hotkey();
                            self.status = "已从 config.toml 重新载入".into();
                        }
                        Err(e) => self.status = format!("载入失败: {e:#}"),
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let quit = ui.button("退出程序");
                    if quit.clicked() {
                        self.quit = true;
                        let _ = self.cmd_tx.send(Cmd::Quit);
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    self.probe_note("btn_quit", quit.rect);
                });
            });
            ui.add_space(4.0);
        });

        // 中间：可滚动的设置区
        let central = egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.section_general(ui);
                    self.section_asr(ui);
                    self.section_polish(ui);
                    self.section_inject(ui);
                    self.section_floating(ui);
                    self.section_rime(ui);
                    self.section_hotwords(ui);
                    self.section_model(ui);
                    self.section_log(ui);
                });
        });
        if !self.logged_layout {
            self.logged_layout = true;
            tracing::info!(
                "布局自检: 窗口高 {:.0}px / 底部操作条 rect={:?} / 内容区 rect={:?}",
                ui.max_rect().height(),
                bottom.response.rect,
                central.response.rect
            );
        }
        self.probe_flush();
    }

    fn section_general(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("通用")
            .default_open(true)
            .show(ui, |ui| {
                egui::Grid::new("g_general")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("主热键");
                        ui.horizontal(|ui| {
                            if ui
                                .text_edit_singleline(&mut self.cfg.general.hotkey)
                                .changed()
                            {
                                touch!(self);
                            }
                            ui.weak("如 f9 / right_alt / win+h / ctrl+alt+f8（字母键须带修饰键）");
                        });
                        ui.end_row();

                        ui.label("录音方式");
                        let mut mode = self.cfg.general.hotkey_mode.clone();
                        let mode_resp = egui::ComboBox::from_id_salt("mode")
                            .selected_text(mode_label(&mode))
                            .show_ui(ui, |ui| {
                                let r1 = ui.selectable_value(&mut mode, "hold".into(), "按住说话");
                                let r2 = ui.selectable_value(
                                    &mut mode,
                                    "toggle".into(),
                                    "切换：按一下开始 / 再按一下停止",
                                );
                                // GUI 自动化用：下拉里两个选项的真实坐标（弹出时才有效）
                                self.probe_note("combo_mode_item_hold", r1.rect);
                                self.probe_note("combo_mode_item_toggle", r2.rect);
                            });
                        // GUI 自动化用（AIVOICE_GUI_PROBE=1）：录音方式下拉的真实坐标
                        self.probe_note("combo_mode", mode_resp.response.rect);
                        if mode != self.cfg.general.hotkey_mode {
                            // ★ 录音方式改完**立即生效**（2026-10-20）
                            //
                            // 为什么不再走「保存并应用」：用户报「这个选项实际没有作用」——
                            // 实测他选的切换从没落到 config.toml 里（一直是 hold），
                            // 因为这一项改了不点保存就等于没改，而他接下来按 F9 一试，
                            // 还是「松手就截止」，自然判定「选项没用」。
                            // 这是纯行为开关（不涉及路径/密钥），改了立刻落盘 + 热生效最符合预期。
                            self.cfg.general.set_toggle_mode(mode.trim().eq_ignore_ascii_case("toggle"));
                            self.apply_instant_fields(ui.ctx());
                        }
                        ui.end_row();

                        ui.label("单次会话上限（分钟）");
                        let dv = ui.add(
                            egui::DragValue::new(&mut self.cfg.general.max_session_minutes)
                                .range(1..=180),
                        );
                        self.probe_note("dv_max_session", dv.rect);
                        if dv.changed() {
                            touch!(self);
                        }
                        ui.end_row();

                        // ---- 门控引擎（C1/D3）：vad 与 energy 的旋钮不是同一组，按引擎分开展示
                        // （字段与默认值全部保留，只是「不生效的不占地方」，并且各自带一句 hover）
                        let mut is_vad = self.cfg.general.gate_engine.trim().eq_ignore_ascii_case("vad");

                        ui.label("门控引擎");
                        let mut engine = self.cfg.general.gate_engine.clone();
                        egui::ComboBox::from_id_salt("gate_engine")
                            .selected_text(if is_vad { "VAD（推荐：只认人声）" } else { "能量门（兼容旧行为）" })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut engine, "vad".into(), "VAD（推荐：只认人声）");
                                ui.selectable_value(&mut engine, "energy".into(), "能量门（兼容旧行为）");
                            })
                            .response
                            .on_hover_text(
                                "VAD（Silero 神经网络）：分得清人声和键盘/环境噪声，轻声说话也进 —— 默认。\
                                 能量门（旧）：只看音量，键盘瞬变会被当成语音送进识别（幻觉字的燃料）。\
                                 模型缺失/加载失败时自动回退能量门并在日志里记 WARN。",
                            );
                        if engine != self.cfg.general.gate_engine {
                            self.cfg.general.gate_engine = engine;
                            touch!(self);
                            is_vad = self.cfg.general.gate_engine.eq_ignore_ascii_case("vad");
                            self.status = if is_vad {
                                "已切换到 VAD 门控：改用「VAD 灵敏度阈值 / 最短语音 / 尾部静音 / 最长段」；\
                                 能量门参数已隐藏（切回 energy 才显示），字段和默认值都保留"
                                    .into()
                            } else {
                                "已切换到能量门控（兼容旧行为）：改用「静音阈值 / 最短有效段 / 分段长度 / 静音断句」；\
                                 VAD 参数已隐藏"
                                    .into()
                            };
                        }
                        ui.end_row();

                        ui.label(if is_vad { "最长段上限（秒）" } else { "分段长度（秒）" });
                        ui.horizontal(|ui| {
                            if ui
                                .add(egui::DragValue::new(&mut self.cfg.general.segment_seconds).range(1..=60))
                                .on_hover_text(if is_vad {
                                    "VAD 模式：一段最多录这么久（max_speech_duration）—— 是上限不是目标长度。\
                                     正常说话由「尾部静音」收尾，只有一直不停顿才会被它截断。\
                                     调大：长句不会被腰斩；调小：更实时，但连着说不换气时会把一句切成两段。"
                                } else {
                                    "能量门：每录满这么久切一刀。调小更实时但句子容易切碎；\
                                     调大一段更完整，但要等更久才出字（SenseVoice 建议 ≤30s）。"
                                })
                                .changed()
                            {
                                touch!(self);
                            }
                            ui.weak(if is_vad { "上限，不是目标长度" } else { "越小越实时；SenseVoice 建议 ≤30s" });
                        });
                        ui.end_row();

                        ui.label(if is_vad { "尾部静音（秒）" } else { "静音断句（秒）" });
                        ui.horizontal(|ui| {
                            if ui
                                .add(egui::DragValue::new(&mut self.cfg.general.silence_seconds).speed(0.1).range(0.0..=3.0))
                                .on_hover_text(if is_vad {
                                    "VAD 模式：说完后静音达到这么久就收尾上屏（min_silence_duration）。\
                                     调小 = 上屏更跟手，但话中间稍一停顿就被切成两段；\
                                     调大 = 停顿不会被切断，代价是每次要多等一会儿才出字。0 = 关（等到最长段上限）。"
                                } else {
                                    "能量门：静音达到这么久就提前断句上屏。\
                                     调小更跟手但容易切碎；调大要等更久；0 = 关闭（只按分段长度切）。"
                                })
                                .changed()
                            {
                                touch!(self);
                            }
                            ui.weak(if is_vad { "说完多久上屏（0=关）" } else { "尾部静音达此长度即上屏（0=关闭）" });
                        });
                        ui.end_row();

                        if is_vad {
                            ui.label("VAD 灵敏度阈值");
                            ui.horizontal(|ui| {
                                if ui
                                    .add(egui::DragValue::new(&mut self.cfg.general.vad_threshold).speed(0.01).range(0.05..=0.95))
                                    .on_hover_text(
                                        "判为「是语音」的概率门槛（Silero 默认 0.5）。\
                                         调大 = 更严：环境噪声更难蒙混过关，但轻声/气声可能被漏掉；\
                                         调小 = 更灵敏：小声也能进，代价是噪声一起进来、幻觉字变多。",
                                    )
                                    .changed()
                                {
                                    touch!(self);
                                }
                                ui.weak("0.5 默认；噪声多就往大调");
                            });
                            ui.end_row();

                            ui.label("最短语音（秒）");
                            ui.horizontal(|ui| {
                                if ui
                                    .add(egui::DragValue::new(&mut self.cfg.general.vad_min_speech_seconds).speed(0.05).range(0.0..=2.0))
                                    .on_hover_text(
                                        "短于这么久的声音不当语音（min_speech_duration，Silero 默认 0.25s）。\
                                         调大 = 咳嗽/敲键这类“咔哒”更不容易成段（更干净）；\
                                         调小 = “嗯/好/对”这类极短回答不会被吃掉。",
                                    )
                                    .changed()
                                {
                                    touch!(self);
                                }
                                ui.weak("0.25 默认；短回答被吃就往小调");
                            });
                            ui.end_row();

                            ui.label(
                                egui::RichText::new(
                                    "能量门参数（静音阈值 / 最短有效段）已隐藏，切回 energy 显示；字段与默认值都保留",
                                )
                                .weak(),
                            );
                            ui.end_row();
                        }

                        ui.label("麦克风预热");
                        ui.horizontal(|ui| {
                            let cur = self.cfg.general.prewarm_gear();
                            egui::ComboBox::from_id_salt("prewarm_gear")
                                .selected_text(self.cfg.general.prewarm_label())
                                .width(220.0)
                                .show_ui(ui, |ui| {
                                    // 手改过 config.toml（非预设值）时，先把现值原样显示出来
                                    if cur.is_none() {
                                        let _ = ui.selectable_label(
                                            true,
                                            self.cfg.general.prewarm_label(),
                                        );
                                    }
                                    if ui
                                        .selectable_label(cur == Some(0), "只在按下时开麦（最省资源）")
                                        .on_hover_text(
                                            "不预热：每次按下才开麦。代价：每次按下都会丢约 0.1~0.3s 起头，\
                                             而且系统麦克风指示灯只在说话时才亮。",
                                        )
                                        .clicked()
                                    {
                                        self.cfg.general.set_prewarm_gear(0);
                                        touch!(self);
                                    }
                                    for (i, (minutes, text)) in
                                        crate::config::PREWARM_GEARS.iter().enumerate()
                                    {
                                        let idx = i + 1;
                                        let hint = if *minutes == 0 {
                                            "预热常开、从不自动关麦：按下即录、不丢起头（推荐）。\
                                             代价：系统麦克风指示灯常亮。"
                                        } else {
                                            "预热常开，但空闲这么久后自动关麦（省一点资源）。\
                                             代价：关麦后的下一次按下会丢一次起头。"
                                        };
                                        if ui
                                            .selectable_label(cur == Some(idx), *text)
                                            .on_hover_text(hint)
                                            .clicked()
                                        {
                                            self.cfg.general.set_prewarm_gear(idx);
                                            touch!(self);
                                        }
                                    }
                                });
                        });
                        ui.weak(
                            "默认「永不关麦」：按下即录、不丢起头；觉得占资源就选「只在按下时开麦」或空闲 N 分钟关",
                        );
                        ui.end_row();

                        ui.label("模型释放");
                        ui.horizontal(|ui| {
                            let cur = self.cfg.general.model_release_gear();
                            egui::ComboBox::from_id_salt("model_release")
                                .selected_text(self.cfg.general.model_release_label())
                                .width(220.0)
                                .show_ui(ui, |ui| {
                                    if cur.is_none() {
                                        let _ = ui.selectable_label(
                                            true,
                                            self.cfg.general.model_release_label(),
                                        );
                                    }
                                    for (idx, (minutes, text)) in
                                        crate::config::MODEL_RELEASE_GEARS.iter().enumerate()
                                    {
                                        let hint = if *minutes == 0 {
                                            "空闲也不卸载：下一次按下最快（推荐）。\
                                             代价：识别模型常驻内存、整理后端常驻显存。"
                                        } else {
                                            "空闲这么久就把**识别模型**卸载、并把整理用的 llama-server 停掉\
                                             （省内存/显存）。代价：空闲后第一次按下要重新加载约 +2s；\
                                             第一次整理要冷启动 10~25s（本次先用原文）。"
                                        };
                                        if ui
                                            .selectable_label(cur == Some(idx), *text)
                                            .on_hover_text(hint)
                                            .clicked()
                                        {
                                            self.cfg.general.set_model_release_gear(idx);
                                            touch!(self);
                                        }
                                    }
                                });
                        });
                        ui.weak(
                            "空闲后卸载识别模型 + 停掉整理后端（避免占内存/显存）；默认「永不释放」，与麦克风预热相互独立",
                        );
                        ui.end_row();

                        ui.label("出字间隔");
                        ui.horizontal(|ui| {
                            // 2026-10-20：四档可选（10/20/40/60 秒）。
                            // 用户报「按住说了半天没字，要等一分钟或松手才出」→ 老默认 60s 太长。
                            let mut gear = self.cfg.general.flush_gear();
                            let sel = self.cfg.general.flush_label();
                            let resp = egui::ComboBox::from_id_salt("flush_gear")
                                .selected_text(sel)
                                .show_ui(ui, |ui| {
                                    for (idx, (_secs, hint)) in
                                        crate::config::FLUSH_GEARS.iter().enumerate()
                                    {
                                        if ui
                                            .selectable_value(&mut gear, Some(idx), *hint)
                                            .changed()
                                        {
                                            // 立即生效（不用点保存）：用户实测过「选了 10 秒但没保存 → 还是 60 秒」
                                            self.cfg.general.set_flush_gear(idx);
                                            self.apply_instant_fields(ui.ctx());
                                        }
                                    }
                                })
                                .response
                                .on_hover_text(
                                    "录音期间每满这么久，就把这期间识别出来的文字**整理后上屏**一次\
                                     （会话结束时会立刻把剩余的吐完，不用等）。\
                                     越小越跟手（10 秒最顺手）；越大越省（大模型/接口调用次数更少，\
                                     但一段长口述里你要等更久才看到字）。",
                                );
                            self.probe_note("combo_flush_gear", resp.rect);
                            if self.cfg.general.flush_gear().is_none() {
                                ui.weak(format!(
                                    "（config.toml 里是自定义值 {} 秒）",
                                    self.cfg.general.flush_seconds
                                ));
                            }
                        });
                        ui.end_row();

                        if !is_vad {
                            ui.label(
                                egui::RichText::new("兼容：能量门参数（仅 gate_engine = energy 时生效）").weak(),
                            );
                            ui.end_row();

                            ui.label("静音阈值（RMS）");
                            if ui
                                .add(egui::DragValue::new(&mut self.cfg.general.min_speech_rms).speed(0.0001).range(0.0..=0.5))
                                .on_hover_text(
                                    "能量门：低于这个音量就当静音，不识别也不注入。\
                                     调小 = 更灵敏（小声也能进），代价是噪声一起进、幻觉字变多；\
                                     调大 = 更干净，但轻声说话会被整段吃掉。\
                                     实际门限取 max(本值, 噪声底×2.5)。",
                                )
                                .changed()
                            {
                                touch!(self);
                            }
                            ui.end_row();

                            ui.label("最短有效段（秒）");
                            if ui
                                .add(egui::DragValue::new(&mut self.cfg.general.min_record_seconds).speed(0.05).range(0.0..=5.0))
                                .on_hover_text(
                                    "能量门：短于这么久的段直接丢弃（不当语音）。\
                                     调大 = 咳嗽/敲键等零碎声音更不容易成段；\
                                     调小 = “嗯/好”这类极短回答不会被吃掉。",
                                )
                                .changed()
                            {
                                touch!(self);
                            }
                            ui.end_row();
                        }

                        ui.label("噪声过滤");
                        if ui
                            .checkbox(
                                &mut self.cfg.general.drop_short_english,
                                "丢弃环境音误判（纯英文短词，如 Okay./Yeah.）",
                            )
                            .changed()
                        {
                            touch!(self);
                        }
                        ui.end_row();

                        ui.label("录音设备");
                        let devs = self.devices.clone();
                        let mut dev = self.cfg.general.device.clone();
                        let default_name = crate::audio::default_input_device_name()
                            .unwrap_or_else(|| "（无默认设备）".into());
                        egui::ComboBox::from_id_salt("dev")
                            .width(360.0)
                            .selected_text(if dev.is_empty() {
                                format!("系统默认（{default_name}）")
                            } else {
                                dev.clone()
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut dev, String::new(), "系统默认");
                                for d in &devs {
                                    ui.selectable_value(&mut dev, d.clone(), d);
                                }
                            });
                        if dev != self.cfg.general.device {
                            self.cfg.general.device = dev;
                            touch!(self);
                        }
                        ui.end_row();

                        ui.label("开机自启");
                        let mut on = self.cfg.general.autostart;
                        if ui.checkbox(&mut on, "随系统启动").changed() {
                            match crate::set_autostart(on) {
                                Ok(()) => {
                                    self.cfg.general.autostart = on;
                                    touch!(self);
                                }
                                Err(e) => self.status = format!("设置开机自启失败: {e:#}"),
                            }
                        }
                        ui.end_row();

                        ui.label("日志级别");
                        if ui
                            .text_edit_singleline(&mut self.cfg.general.log_level)
                            .changed()
                        {
                            touch!(self);
                        }
                        ui.end_row();
                    });
            });
    }

    fn section_asr(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("识别")
            .default_open(true)
            .show(ui, |ui| {
                egui::Grid::new("g_asr")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("识别后端");
                        let mut b = self.cfg.asr.backend.clone();
                        egui::ComboBox::from_id_salt("backend")
                            .selected_text(if b == "cloud" {
                                "云端 ASR"
                            } else {
                                "本地模型"
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut b, "local".into(), "本地模型");
                                ui.selectable_value(&mut b, "cloud".into(), "云端 ASR");
                            });
                        if b != self.cfg.asr.backend {
                            self.cfg.asr.backend = b;
                            touch!(self);
                        }
                        ui.end_row();

                        // 本地模型类型：可自由切换（SenseVoice / Paraformer / FunASR-Nano）
                        let model_label = |t: &str| match t {
                            "paraformer" => "Paraformer（中英）",
                            "funasr-nano" => "FunASR-Nano（多语种 + 热词）",
                            _ => "SenseVoice（中英日韩粤）",
                        };
                        let mut mt = {
                            let t = self.cfg.asr.model_type.trim().replace('_', "-").to_ascii_lowercase();
                            if t == "funasr-nano" { t } else { self.cfg.asr.model_type.trim().to_ascii_lowercase() }
                        };
                        ui.label("本地模型");
                        egui::ComboBox::from_id_salt("model_type")
                            .selected_text(model_label(&mt))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut mt, "sensevoice".into(), model_label("sensevoice"));
                                ui.selectable_value(&mut mt, "paraformer".into(), model_label("paraformer"));
                                ui.selectable_value(&mut mt, "funasr-nano".into(), model_label("funasr-nano"));
                            });
                        if mt != self.cfg.asr.model_type {
                            self.cfg.asr.model_type = mt;
                            touch!(self);
                        }
                        ui.end_row();

                        ui.label("语言");
                        ui.horizontal(|ui| {
                            if ui
                                .text_edit_singleline(&mut self.cfg.asr.language)
                                .changed()
                            {
                                touch!(self);
                            }
                            ui.weak("留空=自动；可填 zh / en");
                        });
                        ui.end_row();

                        let is_nano = self
                            .cfg
                            .asr
                            .model_type
                            .trim()
                            .replace('_', "-")
                            .eq_ignore_ascii_case("funasr-nano");
                        ui.label(if is_nano { "模型目录（FunASR-Nano）" } else { "模型目录" });
                        let dir = if is_nano {
                            &mut self.cfg.asr.funasr_nano_model
                        } else {
                            &mut self.cfg.asr.local_model
                        };
                        if ui.text_edit_singleline(dir).changed() {
                            touch!(self);
                        }
                        ui.end_row();

                        ui.label("CPU 线程数");
                        if ui
                            .add(egui::DragValue::new(&mut self.cfg.asr.num_threads).range(1..=64))
                            .changed()
                        {
                            touch!(self);
                        }
                        ui.end_row();
                    });

                ui.add_space(6.0);
                ui.label(egui::RichText::new("云端 ASR（OpenAI 兼容 /v1/audio/transcriptions）").strong());
                egui::Grid::new("g_asr_cloud")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("服务地址");
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut self.cfg.asr.cloud.base_url)
                                        .desired_width(320.0)
                                        .hint_text("https://api.example.com/v1"),
                                )
                                .changed()
                            {
                                touch!(self);
                            }
                        });
                        ui.end_row();

                        ui.label("API Key");
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut self.cfg.asr.cloud.api_key)
                                        .desired_width(320.0)
                                        .password(!self.show_keys),
                                )
                                .changed()
                            {
                                touch!(self);
                            }
                            ui.checkbox(&mut self.show_keys, "显示");
                        });
                        ui.end_row();

                        ui.label("模型名");
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut self.cfg.asr.cloud.model)
                                    .desired_width(320.0),
                            )
                            .changed()
                        {
                            touch!(self);
                        }
                        ui.end_row();

                        ui.label("超时（秒）");
                        if ui
                            .add(egui::DragValue::new(&mut self.cfg.asr.cloud.timeout_s).range(1..=300))
                            .changed()
                        {
                            touch!(self);
                        }
                        ui.end_row();
                    });
                ui.weak("云端不可用时自动回退本地（需本地模型就绪）");
            });
    }

    fn section_polish(&mut self, ui: &mut egui::Ui) {
        let resp = egui::CollapsingHeader::new("文本整理").show(ui, |ui| {
            egui::Grid::new("g_polish")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("档位");
                    let cur = self.cfg.polish.tier();
                    let mut sel = cur;
                    egui::ComboBox::from_id_salt("ptier")
                        .width(220.0)
                        .selected_text(crate::config::Polish::tier_label(cur))
                        .show_ui(ui, |ui| {
                            for t in tray::POLISH_TIERS {
                                ui.selectable_value(&mut sel, t, crate::config::Polish::tier_label(t));
                            }
                        });
                    if sel != cur {
                        self.cfg.polish.set_tier(sel);
                        touch!(self);
                    }
                    ui.end_row();
                });

            if self.cfg.polish.provider == "local" {
                self.subsection_polish_local(ui);
            } else {
                egui::Grid::new("g_polish_cloud")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("服务地址");
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut self.cfg.polish.base_url)
                                    .desired_width(320.0)
                                    .hint_text("https://api.example.com/v1"),
                            )
                            .changed()
                        {
                            touch!(self);
                        }
                        ui.end_row();

                        ui.label("API Key");
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut self.cfg.polish.api_key)
                                    .desired_width(320.0)
                                    .password(!self.show_keys),
                            )
                            .changed()
                        {
                            touch!(self);
                        }
                        ui.end_row();

                        ui.label("模型名");
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut self.cfg.polish.model)
                                    .desired_width(320.0),
                            )
                            .changed()
                        {
                            touch!(self);
                        }
                        ui.end_row();

                        ui.label("超时（秒）");
                        if ui
                            .add(egui::DragValue::new(&mut self.cfg.polish.timeout_s).range(1..=120))
                            .changed()
                        {
                            touch!(self);
                        }
                        ui.end_row();
                    });
            }
            ui.weak("失败或超时会自动回退到 ASR 原文");
        });
        self.probe_note("hdr_polish", resp.header_response.rect);
    }

    fn subsection_polish_local(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(egui::RichText::new("本地大模型（llama-server，懒加载）").strong());
        egui::Grid::new("g_polish_local")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                let l = &mut self.cfg.polish.local;
                ui.label("llama-server");
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut l.exe)
                                .desired_width(300.0)
                                .hint_text("llama-server.exe"),
                        )
                        .changed()
                    {
                        touch!(self);
                    }
                    if ui.button("📂").on_hover_text("打开所在目录").clicked() {
                        open_parent(&l.exe);
                    }
                });
                ui.end_row();

                ui.label("GGUF 模型");
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut l.model)
                                .desired_width(300.0)
                                .hint_text("*.gguf"),
                        )
                        .changed()
                    {
                        touch!(self);
                    }
                    if ui.button("📂").on_hover_text("打开所在目录").clicked() {
                        open_parent(&l.model);
                    }
                });
                ui.end_row();

                ui.label("端口");
                if ui
                    .add(egui::DragValue::new(&mut l.port).range(1024..=65535))
                    .changed()
                {
                    touch!(self);
                }
                ui.end_row();

                ui.label("GPU 层数（ngl）");
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::DragValue::new(&mut l.ngl).range(0..=999))
                        .changed()
                    {
                        touch!(self);
                    }
                    ui.weak("99 = 全部；改小可卸载到 CPU");
                });
                ui.end_row();

                ui.label("上下文长度（ctx）");
                if ui
                    .add(egui::DragValue::new(&mut l.ctx).range(512..=32768))
                    .changed()
                {
                    touch!(self);
                }
                ui.end_row();

                ui.label("启动超时（秒）");
                if ui
                    .add(egui::DragValue::new(&mut l.start_timeout_s).range(10..=600))
                    .changed()
                {
                    touch!(self);
                }
                ui.end_row();

                ui.label("单次超时（秒）");
                if ui
                    .add(egui::DragValue::new(&mut self.cfg.polish.timeout_s).range(1..=120))
                    .changed()
                {
                    touch!(self);
                }
                ui.end_row();

                ui.label("关思考");
                if ui
                    .checkbox(&mut l.disable_thinking, "enable_thinking=false")
                    .on_hover_text("Qwen3.5 等思考型模型必须开，否则返回空且慢 100 倍")
                    .changed()
                {
                    touch!(self);
                }
                ui.end_row();

                ui.label("退出时关掉");
                if ui
                    .checkbox(&mut l.stop_on_exit, "只回收本程序拉起的")
                    .changed()
                {
                    touch!(self);
                }
                ui.end_row();

                ui.label("首次触发等待");
                if ui
                    .checkbox(&mut l.wait_on_first_use, "冷启动时同步等待（默认关=本次用原文）")
                    .changed()
                {
                    touch!(self);
                }
                ui.end_row();

                ui.label("状态");
                ui.horizontal(|ui| {
                    let (color, text) = match &self.llm_state {
                        LlmState::Unknown => (egui::Color32::GRAY, "未启动".to_string()),
                        LlmState::Starting => {
                            (egui::Color32::from_rgb(230, 160, 40), "启动中…".to_string())
                        }
                        LlmState::Ready => (
                            egui::Color32::from_rgb(60, 180, 100),
                            "就绪（本程序拉起）".to_string(),
                        ),
                        LlmState::Reused => (
                            egui::Color32::from_rgb(60, 140, 220),
                            "就绪（复用已有实例）".to_string(),
                        ),
                        LlmState::Failed(e) => {
                            (egui::Color32::from_rgb(220, 90, 90), format!("失败：{e}"))
                        }
                    };
                    ui.colored_label(color, text);
                    let start = ui.button("立即启动");
                    if start.clicked() {
                        let _ = self.cmd_tx.send(Cmd::LlmStart);
                    }
                    self.probe_note("btn_llm_start", start.rect);
                    let stop = ui.button("停止");
                    if stop.clicked() {
                        let _ = self.cmd_tx.send(Cmd::LlmStop);
                    }
                    self.probe_note("btn_llm_stop", stop.rect);
                });
                ui.end_row();
            });
        let log = self.paths.logs_dir().join("llama_server.log");
        ui.weak(format!("日志：{}", log.display()));
    }

    fn section_rime(&mut self, ui: &mut egui::Ui) {
        let hdr = egui::CollapsingHeader::new("输入法（Rime 二合一）").default_open(self.wizard);
        let resp = hdr.show(ui, |ui| {
            let info = self.rime_info.clone();
            let res_dir = self.paths.resources_dir(&self.cfg.rime);
            let res_ok = res_dir.join("rime").join("schemes").is_dir();

            if self.wizard {
                ui.colored_label(
                    egui::Color32::from_rgb(60, 140, 220),
                    "首次使用：选一种主力语言 → 点「立即部署」，装完即可用键盘打该语言（语音侧不受影响）",
                );
                ui.add_space(4.0);
            }

            egui::Grid::new("g_rime")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("倾听输入法");
                    ui.horizontal(|ui| {
                        if info.installed {
                            ui.colored_label(
                                egui::Color32::from_rgb(60, 180, 100),
                                format!("已安装 {}", info.version),
                            );
                        } else {
                            ui.colored_label(egui::Color32::from_rgb(230, 160, 40), "未安装");
                        }
                        if ui.button("重新检测").clicked() {
                            self.rime_info = crate::rime::detect(&self.cfg.rime.user_dir);
                        }
                    });
                    ui.end_row();

                    ui.label("用户目录");
                    ui.horizontal(|ui| {
                        ui.label(info.user_dir.display().to_string());
                        if ui.button("打开").clicked() {
                            crate::open_path(&info.user_dir);
                        }
                    });
                    ui.end_row();

                    ui.label("资源");
                    ui.horizontal(|ui| {
                        if res_ok {
                            ui.colored_label(egui::Color32::from_rgb(60, 180, 100), "已就绪");
                        } else {
                            ui.colored_label(
                                egui::Color32::from_rgb(220, 90, 90),
                                "缺失（跑 installer\\fetch_resources.ps1）",
                            );
                        }
                        ui.weak(res_dir.display().to_string());
                    });
                    ui.end_row();

                    ui.label("主力语言");
                    let mut lang = self.cfg.rime.primary_lang.clone();
                    egui::ComboBox::from_id_salt("rime_lang")
                        .width(180.0)
                        .selected_text(crate::rime::schemes::Lang::from_str(&lang).label())
                        .show_ui(ui, |ui| {
                            for l in crate::rime::schemes::Lang::all() {
                                let enabled = l != crate::rime::schemes::Lang::Fr;
                                ui.add_enabled_ui(enabled, |ui| {
                                    ui.selectable_value(&mut lang, l.as_str().to_string(), l.label());
                                });
                            }
                        });
                    if lang != self.cfg.rime.primary_lang {
                        self.cfg.rime.primary_lang = lang;
                        touch!(self);
                    }
                    ui.end_row();

                    ui.label("英文方案");
                    if ui
                        .checkbox(&mut self.cfg.rime.english_mixed, "一并放进方案列表（F4 可切）")
                        .changed()
                    {
                        touch!(self);
                    }
                    ui.end_row();

                    ui.label("中文简繁");
                    if ui
                        .checkbox(&mut self.cfg.rime.zh_trad, "默认繁体（Ctrl+Shift+4 可切）")
                        .changed()
                    {
                        touch!(self);
                    }
                    ui.end_row();

                    ui.label("语法模型");
                    ui.horizontal(|ui| {
                        if ui
                            .checkbox(&mut self.cfg.rime.grammar, "万象 LMDG（中文，打长句更顺）")
                            .changed()
                        {
                            touch!(self);
                        }
                    });
                    ui.end_row();
                });

            let (running, msg, done, ok) = {
                let g = self.rime_state.lock().unwrap();
                (g.running, g.msg.clone(), g.done, g.ok)
            };
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let label = if info.installed {
                    "⬇ 立即部署（铺方案 + 编译词库）"
                } else {
                    "⬇ 安装倾听输入法并部署"
                };
                if ui
                    .add_enabled(!running && res_ok, egui::Button::new(label))
                    .clicked()
                {
                    self.start_rime_deploy(!info.installed);
                }
                if ui.button("打开资源目录").clicked() {
                    crate::open_path(&res_dir);
                }
                if self.dirty {
                    ui.weak("（设置改动即时生效；只有方案/钩子这类结构变更需要重新部署）");
                }
            });
            if running {
                ui.add(egui::ProgressBar::new(0.0).animate(true).text(&msg));
            } else if done {
                if ok {
                    ui.colored_label(egui::Color32::from_rgb(60, 180, 100), &msg);
                } else {
                    ui.colored_label(egui::Color32::from_rgb(220, 90, 90), &msg);
                }
            }
            if !self.cfg.rime.last_deploy.is_empty() {
                ui.collapsing("最近一次部署日志", |ui| {
                    ui.small(&self.cfg.rime.last_deploy);
                });
            }
            // 低频入口（任务4a：原右键托盘菜单项移入，托盘只留高频项）
            if info.installed {
                ui.collapsing("更多（低频设置）", |ui| {
                    let deployer = info
                        .deployer
                        .clone()
                        .or_else(|| info.install_dir.clone().map(|d| d.join("WeaselDeployer.exe")));
                    ui.horizontal(|ui| {
                        if ui
                            .button("方案选单设定")
                            .on_hover_text("原托盘「输入法设定」：方案选单 / 界面风格 / 词典管理")
                            .clicked()
                        {
                            if let Some(p) = &deployer {
                                crate::launch_exe(p, &[]);
                            }
                        }
                        if ui.button("用户词典管理").clicked() {
                            if let Some(p) = &deployer {
                                crate::launch_exe(p, &["/dict"]);
                            }
                        }
                        if ui.button("用户资料同步").clicked() {
                            if let Some(p) = &deployer {
                                crate::launch_exe(p, &["/sync"]);
                            }
                        }
                        if let Some(dir) = info.install_dir.clone() {
                            if ui
                                .button("安装选项")
                                .on_hover_text("倾听输入法安装选项（安装 / 卸载 / 用户目录）")
                                .clicked()
                            {
                                crate::launch_exe(&dir.join("WeaselSetup.exe"), &[]);
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        if let Some(dir) = info.install_dir.clone() {
                            if ui.button("程序文件夹").clicked() {
                                crate::open_path(&dir);
                            }
                        }
                        if ui.button("日志文件夹").clicked() {
                            let log = std::env::temp_dir().join("rime.weasel");
                            let _ = std::fs::create_dir_all(&log);
                            crate::open_path(&log);
                        }
                        if ui
                            .button("帮助文档")
                            .on_hover_text("rime.im 官方文档")
                            .clicked()
                        {
                            crate::open_path(std::path::Path::new("https://rime.im/docs/"));
                        }
                        if ui
                            .button("参加讨论")
                            .on_hover_text("rime.im 社区")
                            .clicked()
                        {
                            crate::open_path(std::path::Path::new("https://rime.im/discuss/"));
                        }
                    });
                });
            }
            ui.weak("部署在后台进行，首次编译大词库（日文 94MB）可能要几分钟；期间可正常用语音输入");
        });
        self.probe_note("hdr_rime", resp.header_response.rect);
    }

    /// 热词管理器：一份词表同时驱动打字（Rime 置顶候选）与录音（ASR 热词偏置）
    fn section_hotwords(&mut self, ui: &mut egui::Ui) {
        let hdr = egui::CollapsingHeader::new("热词（打字 + 录音）");
        let resp = hdr.show(ui, |ui| {
            ui.weak(
                "一份词表同时驱动两条链：打字 → Rime 置顶候选（保存后切窗口即生效）；录音 → 识别热词偏置 + 术语保留",
            );
            if self.hw_pack.is_empty() {
                self.hw_pack = self
                    .cfg
                    .hotwords
                    .packs
                    .first()
                    .cloned()
                    .unwrap_or_else(|| crate::hotwords::KEYBOARD.to_string());
                self.hw_reload();
            }

            // ---- 包选择 + 开关 ----
            let names = self.hw_pack_names();
            ui.horizontal(|ui| {
                ui.label("词表");
                let mut pick = self.hw_pack.clone();
                egui::ComboBox::from_id_salt("hw_pack")
                    .width(230.0)
                    .selected_text(Self::pack_label(&pick))
                    .show_ui(ui, |ui| {
                        for n in &names {
                            ui.selectable_value(&mut pick, n.clone(), Self::pack_label(n));
                        }
                    });
                if pick != self.hw_pack {
                    if self.hw_dirty {
                        self.hw_status = "已放弃未保存的改动".into();
                    }
                    self.hw_pack = pick;
                    self.hw_reload();
                }
                let is_user = crate::hotwords::is_typing_only(&self.hw_pack);
                let mut on = if self.hw_pack == crate::hotwords::PHRASES {
                    self.cfg.hotwords.phrases
                } else if self.hw_pack == crate::hotwords::LEARNED {
                    self.cfg.hotwords.learn
                } else {
                    self.cfg.hotwords.packs.iter().any(|p| p == &self.hw_pack)
                };
                let label = if is_user { "启用（只用于打字）" } else { "启用此包" };
                if ui.checkbox(&mut on, label).changed() {
                    ui.weak(format!("当前词表：{}", self.hw_pack_label()));
            if self.hw_pack == crate::hotwords::PHRASES {
                        self.cfg.hotwords.phrases = on;
                    } else if self.hw_pack == crate::hotwords::LEARNED {
                        self.cfg.hotwords.learn = on;
                    } else {
                        self.cfg.hotwords.packs.retain(|p| p != &self.hw_pack);
                        if on {
                            self.cfg.hotwords.packs.push(self.hw_pack.clone());
                        }
                    }
                    touch!(self);
                }
                let mut en = self.cfg.hotwords.enabled;
                if ui.checkbox(&mut en, "总开关").changed() {
                    self.cfg.hotwords.enabled = en;
                    touch!(self);
                }
                // 全局「快捷输入」展开（英文 / 直通状态兜底，方案 C）
                let mut qe = self.cfg.hotwords.quick_expand;
                if ui
                    .checkbox(&mut qe, "英文/直通状态也展开快捷输入")
                    .on_hover_text(
                        "中文状态由输入法候选表直接出第一项；英文（直通）状态没有候选窗，\
                         这里用键盘钩子兜底：敲完编码再按空格 → 退格 + 注入文本。\
                         守卫：中文输入法活跃时自动沉默（靠 lua 心跳），不会重复出字。\
                         ⚠️ 需要先部署一次（lua 心跳）；见到误触就关掉这项。",
                    )
                    .changed()
                {
                    self.cfg.hotwords.quick_expand = qe;
                    touch!(self);
                    self.hw_status = format!(
                        "全局快捷输入已{}（重启 listen 后生效）",
                        if qe { "打开" } else { "关闭" }
                    );
                }
                // 打字联想（custom_phrase/enable_completion）：打前缀也带出热词
                let mut pc = self.cfg.hotwords.prefix_completion;
                if ui
                    .checkbox(&mut pc, "字词联想")
                    .on_hover_text(
                        "打开后**打前缀也能带出热词**：比如打 g 就能看到 GB / Gasket / GitHub…\
                         （Rime 的 custom_phrase 的 enable_completion）。\
                         代价：候选变多，单字母时首屏可能被热词占满。\
                         ⚠️ 这是方案结构改动，改完要点「输入法 → 立即部署」（一次 UAC）才生效。",
                    )
                    .changed()
                {
                    self.cfg.hotwords.prefix_completion = pc;
                    touch!(self);
                    self.hw_status = format!(
                        "字词联想已{}；请到「输入法」页点「立即部署」后生效",
                        if pc { "打开" } else { "关闭" }
                    );
                }
                ui.separator();
                ui.label("送识别模型热词上限");
                if ui
                    .add(egui::DragValue::new(&mut self.cfg.hotwords.asr_top_n).range(1..=200))
                    .changed()
                {
                    touch!(self);
                }
                ui.weak("（FunASR-Nano 的 LLM 上下文有限，默认 30）");
            });

            let n = self.hw_terms.len();
            let ncode = self.hw_terms.iter().filter(|t| !t.code.trim().is_empty()).count();
            ui.weak(format!(
                "本词表：{n} 条 · {ncode} 条带打字编码 · 仅录音 {} 条",
                n - ncode
            ));

            // ---- 全量统计（2026-10-21：用户以为「总共只有 118 条」——那只是当前选中的包）----
            // 这里把「启用的内置包 + 我的热词 + 我打过的词」全算出来，并说清进识别模型的条数上限。
            let mut parts: Vec<String> = Vec::new();
            let mut total = 0usize;
            for name in self.hw_pack_names() {
                if matches!(
                    name.as_str(),
                    x if x == crate::hotwords::PHRASES || x == crate::hotwords::LEARNED
                ) {
                    continue;
                }
                if !self.cfg.hotwords.packs.iter().any(|p| p == &name) {
                    continue;
                }
                let cnt = crate::hotwords::read_pack(&self.cfg, &self.paths, &name).len();
                if cnt > 0 {
                    total += cnt;
                    parts.push(format!("{name} {cnt}"));
                }
            }
            let learned_n = if self.cfg.hotwords.learn {
                crate::hotwords::read_pack(&self.cfg, &self.paths, crate::hotwords::LEARNED).len()
            } else {
                0
            };
            if learned_n > 0 {
                total += learned_n;
                parts.push(format!("我打过的词 {learned_n}"));
            }
            let merged = crate::hotwords::asr_terms(&self.cfg, &self.paths).len();
            ui.weak(format!(
                "全量：{} = {total} 条（去重后 {merged} 条）· 送识别模型前 {} 条 · 打字侧不限条数",
                parts.join(" + "),
                self.cfg.hotwords.asr_top_n
            ));
            if ui
                .button("✏ 编辑「我的热词」（条数不设上限，两条链都生效）")
                .clicked()
            {
                self.hw_pack = crate::hotwords::MINE.to_string();
                self.hw_reload();
                self.hw_status = "已切到「我的热词」：加多少条都行，保存后切窗口即生效".into();
            }

            // ---- 用户词学习（只对「我打过的词」有意义，但开关放这里常驻）----
            let learning = self.hw_pack == crate::hotwords::LEARNED;
            if learning {
                ui.weak(
                    "打字时你选过的词会自动记进这里，并置顶到第一位（同类机制：雾凇自带调频实测无效）",
                );
                ui.horizontal(|ui| {
                    if ui.button("⟳ 立即导入新词").clicked() {
                        match crate::learn::import_new(&self.cfg, &self.paths, false) {
                            Ok(r) => {
                                // 顺手写进打字侧（换个窗口即生效，不需要部署）
                                let applied = crate::hotwords::apply_to_rime(&self.cfg, &self.paths)
                                    .ok()
                                    .flatten()
                                    .map(|_| "，已写入打字侧")
                                    .unwrap_or("");
                                self.hw_status = if r.is_empty() {
                                    format!("没有新词（词表 {} 条）{applied}", r.total)
                                } else {
                                    format!(
                                        "导入完成：新词 {} 条 / 次数 +{} → 共 {} 条{applied}",
                                        r.new_pairs, r.added_count, r.total
                                    )
                                };
                            }
                            Err(e) => self.hw_status = format!("导入失败: {e:#}"),
                        }
                        self.hw_reload();
                    }
                    if ui
                        .button("🗑 清空已学词")
                        .on_hover_text("清掉置顶的词（日志进度不动，打过的词不会又被导入回来）")
                        .clicked()
                    {
                        match crate::learn::clear(&self.cfg, &self.paths) {
                            Ok(()) => self.hw_status = "已清空已学词".into(),
                            Err(e) => self.hw_status = format!("清空失败: {e:#}"),
                        }
                        self.hw_reload();
                    }
                    if ui
                        .button("↺ 重新学习全部")
                        .on_hover_text("忽略进度，把整份历史日志重新吃一遗（删过的词也会回来）")
                        .clicked()
                    {
                        match crate::learn::import_new(&self.cfg, &self.paths, true) {
                            Ok(r) => {
                                let applied = crate::hotwords::apply_to_rime(&self.cfg, &self.paths)
                                    .ok()
                                    .flatten()
                                    .map(|_| "，已写入打字侧")
                                    .unwrap_or("");
                                self.hw_status = format!(
                                    "全量重学：新词 {} 条 → 共 {} 条{applied}",
                                    r.new_pairs, r.total
                                )
                            }
                            Err(e) => self.hw_status = format!("重学失败: {e:#}"),
                        }
                        self.hw_reload();
                    }
                    ui.separator();
                    ui.label("最多置顶");
                    if ui
                        .add(egui::DragValue::new(&mut self.cfg.hotwords.learn_max).range(20..=2000))
                        .changed()
                    {
                        touch!(self);
                    }
                    ui.label("条 · 攒够");
                    if ui
                        .add(egui::DragValue::new(&mut self.cfg.hotwords.learn_auto_deploy).range(0..=999))
                        .on_hover_text("0 = 不自动写入，只能点「保存到文件」")
                        .changed()
                    {
                        touch!(self);
                    }
                    ui.weak("个新词自动写进打字表（无需 UAC）");
                });
                ui.weak(format!(
                    "日志已记 {} 条，其中 {} 条已导入",
                    crate::learn::log_lines(&self.cfg, &self.paths),
                    crate::learn::learned_terms(&self.cfg, &self.paths).len()
                ));
            }

            // ---- 工具条 ----
            ui.horizontal(|ui| {
                ui.label("过滤");
                ui.add(egui::TextEdit::singleline(&mut self.hw_filter).desired_width(120.0));
                if ui.button("＋ 加词条").clicked() {
                    self.hw_terms.push(crate::hotwords::Term {
                        weight: 50,
                        category: "自定义".into(),
                        ..Default::default()
                    });
                    self.hw_dirty = true;
                }
                if ui
                    .add_enabled(self.hw_dirty, egui::Button::new("💾 保存到文件"))
                    .clicked()
                {
                    self.hw_save();
                }
                if ui.button("↺ 重新载入").clicked() {
                    self.hw_reload();
                }
                if crate::hotwords::builtin_body(&self.hw_pack).is_some() {
                    if self.hw_confirm_restore {
                        if ui.button("确认恢复内置" ).clicked() {
                            self.hw_terms = crate::hotwords::builtin_body(&self.hw_pack)
                                .map(crate::hotwords::parse_tsv)
                                .unwrap_or_default();
                            self.hw_confirm_restore = false;
                            self.hw_dirty = true;
                            self.hw_status = "已载入内置副本，点「保存到文件」生效".into();
                        }
                    } else if ui.button("恢复内置词表").clicked() {
                        self.hw_confirm_restore = true;
                    }
                }
                if ui.button("打开词表目录").clicked() {
                    let d = crate::hotwords::dir(&self.cfg, &self.paths);
                    crate::open_path(&d);
                }
            });
            ui.weak(format!("当前词表：{}", self.hw_pack_label()));
            if self.hw_pack == crate::hotwords::PHRASES {
                ui.weak(
                    "快捷输入：编码只能小写字母（如 gds / yunghg），打完按空格 → 第一候选就是「词条」那段文本（长度不限，可随时改）。只用于打字，不进录音热词。",
                );
            } else if self.hw_pack == crate::hotwords::LEARNED {
                ui.weak(
                    "自动学习：只收「词语」（汉字 ≤6 个、无标点空格），按使用次数排序 —— 反复选的那个词会自动跑到第一候选（10 秒内生效，切窗口即可）。",
                );
            }
            if !self.hw_status.is_empty() {
                ui.weak(self.hw_status.clone());
            }

            // ---- 快捷输入：三栏表单（快捷键 / 词语 / 备注），2026-10-21 用户要求 ----
            if self.hw_pack == crate::hotwords::PHRASES {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("快捷键");
                    let code = ui.add(
                        egui::TextEdit::singleline(&mut self.hw_new_code)
                            .desired_width(110.0)
                            .hint_text("gds"),
                    );
                    self.probe_note("qe_code", code.rect);
                    ui.label("备注");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.hw_new_note)
                            .desired_width(220.0)
                            .hint_text("快递地址"),
                    );
                });
                ui.label("词语（要上屏的完整文本，长度不限）");
                let body = ui.add(
                    egui::TextEdit::multiline(&mut self.hw_new_text)
                        .desired_width(f32::INFINITY)
                        .desired_rows(4)
                        .hint_text("广东省深圳市南山区科技园南路 1 号 3 栋 502"),
                );
                self.probe_note("qe_text", body.rect);

                let code_ok = {
                    let c = self.hw_new_code.trim().to_lowercase();
                    c.len() >= 2 && c.chars().all(|ch| ch.is_ascii_lowercase())
                };
                let text_ok = !self.hw_new_text.trim().is_empty();
                ui.horizontal(|ui| {
                    let can_add = code_ok && text_ok;
                    let btn = ui.add_enabled(can_add, egui::Button::new("＋ 添加 / 更新"));
                    self.probe_note("qe_add", btn.rect);
                    if btn.clicked() {
                        let code = self.hw_new_code.trim().to_lowercase();
                        let text = self.hw_new_text.trim().to_string();
                        let note = self.hw_new_note.trim().to_string();
                        // 同快捷键视为「修改」：直接覆盖，不留重复行
                        if let Some(t) = self.hw_terms.iter_mut().find(|t| t.code == code) {
                            t.text = text;
                            t.note = note;
                        } else {
                            self.hw_terms.push(crate::hotwords::Term {
                                text,
                                code,
                                category: "快捷输入".into(),
                                weight: 90,
                                rank: 0,
                                note,
                            });
                        }
                        self.hw_dirty = true;
                        self.hw_new_code.clear();
                        self.hw_new_text.clear();
                        self.hw_new_note.clear();
                        self.hw_pick = None;
                    }
                    if ui.button("清空表单").clicked() {
                        self.hw_new_code.clear();
                        self.hw_new_text.clear();
                        self.hw_new_note.clear();
                        self.hw_pick = None;
                    }
                    if !code_ok && !self.hw_new_code.trim().is_empty() {
                        ui.colored_label(egui::Color32::from_rgb(220, 120, 90), "快捷键只能用小写字母（≥2 位）");
                    }
                    ui.separator();
                    if ui
                        .add_enabled(self.hw_dirty, egui::Button::new("💾 保存到文件"))
                        .clicked()
                    {
                        self.hw_save();
                    }
                });
                ui.weak("添加后点「保存到文件」→ 立刻写进打字表；中文状态打快捷键 + 空格就是第一候选，英文状态由全局展开兜底。");

                // 已有条目列表（快捷键 / 词语 / 备注 / 操作）
                let mut del: Option<usize> = None;
                egui::ScrollArea::vertical()
                    .id_salt("qe_scroll")
                    .max_height(220.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Grid::new("qe_grid")
                            .num_columns(4)
                            .striped(true)
                            .spacing([10.0, 4.0])
                            .show(ui, |ui| {
                                ui.strong("快捷键");
                                ui.strong("词语");
                                ui.strong("备注");
                                ui.strong("");
                                ui.end_row();
                                for (i, t) in self.hw_terms.iter().enumerate() {
                                    ui.monospace(&t.code);
                                    let short: String = t.text.chars().take(40).collect();
                                    ui.label(short);
                                    ui.weak(&t.note);
                                    ui.horizontal(|ui| {
                                        if ui.small_button("编辑").clicked() {
                                            self.hw_new_code = t.code.clone();
                                            self.hw_new_text = t.text.clone();
                                            self.hw_new_note = t.note.clone();
                                            self.hw_pick = Some(i);
                                        }
                                        if ui.small_button("删除").clicked() {
                                            del = Some(i);
                                        }
                                    });
                                    ui.end_row();
                                }
                            });
                    });
                if let Some(i) = del {
                    self.hw_terms.remove(i);
                    self.hw_dirty = true;
                    self.hw_pick = None;
                }
            } else {

            // ---- 词条表格 ----
            let filter = self.hw_filter.trim().to_lowercase();
            let mut remove: Option<usize> = None;
            let mut changed = false;
            egui::ScrollArea::vertical()
                .id_salt("hw_scroll")
                .max_height(300.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new("hw_grid")
                        .num_columns(6)
                        .striped(true)
                        .spacing([8.0, 4.0])
                        .show(ui, |ui| {
                            ui.strong("词条");
                            ui.strong("编码");
                            ui.strong("类别");
                            ui.strong("权重");
                            ui.strong("说明");
                            ui.strong("");
                            ui.end_row();
                            for (i, t) in self.hw_terms.iter_mut().enumerate() {
                                if !filter.is_empty()
                                    && !t.text.to_lowercase().contains(&filter)
                                    && !t.code.to_lowercase().contains(&filter)
                                    && !t.category.to_lowercase().contains(&filter)
                                {
                                    continue;
                                }
                                changed |= ui
                                    .add(
                                        egui::TextEdit::singleline(&mut t.text)
                                            .desired_width(130.0),
                                    )
                                    .changed();
                                changed |= ui
                                    .add(
                                        egui::TextEdit::singleline(&mut t.code)
                                            .desired_width(90.0)
                                            .hint_text("小写拼音/缩写；空=仅录音"),
                                    )
                                    .changed();
                                changed |= ui
                                    .add(
                                        egui::TextEdit::singleline(&mut t.category)
                                            .desired_width(80.0),
                                    )
                                    .changed();
                                changed |= ui
                                    .add(egui::DragValue::new(&mut t.weight).range(0..=100))
                                    .changed();
                                changed |= ui
                                    .add(
                                        egui::TextEdit::singleline(&mut t.note)
                                            .desired_width(180.0),
                                    )
                                    .changed();
                                if ui.small_button("✖").on_hover_text("删除这一条").clicked() {
                                    remove = Some(i);
                                }
                                ui.end_row();
                            }
                        });
                });
            if let Some(i) = remove {
                self.hw_terms.remove(i);
                changed = true;
            }
            if changed {
                self.hw_dirty = true;
            }

            }

            // ---- 生效 ----
            ui.horizontal(|ui| {
                ui.weak("改完先「保存到文件」，再让两端生效：");
                ui.weak("录音侧立即生效（保存即写 config）；打字侧换个窗口即生效（内容改动无需重新部署）");
            });
            let (running, _, _, _) = {
                let g = self.rime_state.lock().unwrap();
                (g.running, g.done, g.ok, 0)
            };
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        self.rime_info.installed && !running,
                        egui::Button::new("立即重新部署（弹 UAC）"),
                    )
                    .on_hover_text("把词表铺给打字侧：写 listen_hotwords.txt + 重新编译词库")
                    .clicked()
                {
                    self.start_rime_deploy(false);
                }
                if !self.rime_info.installed {
                    ui.weak("（没装倾听输入法：打字侧要先去上面部署一次）");
                }
            });
        });
        self.probe_note("hdr_hotwords", resp.header_response.rect);
    }

    /// 可选的词表：我的词表（快捷输入 / 我打过的词）+ 内置包 + 用户目录里自己放的 *.tsv
    /// 词表在下拉里的显示名（第一项固定是「快捷输入」，2026-10-21 用户要求）
    fn pack_label(name: &str) -> String {
        match name {
            crate::hotwords::PHRASES => "快捷输入（自定义编码 → 任意文本）".to_string(),
            crate::hotwords::LEARNED => "我打过的词（自动学习，按使用次数排前）".to_string(),
            crate::hotwords::MINE => "我的热词（自己加，条数不限）".to_string(),
            crate::hotwords::KEYBOARD => "内置：键圈".to_string(),
            crate::hotwords::VIBECODING => "内置：AI 编程".to_string(),
            other => other.to_string(),
        }
    }

    /// 当前包的中文标签（标题行/提示用）
    fn hw_pack_label(&self) -> String {
        Self::pack_label(&self.hw_pack)
    }

    fn hw_pack_names(&self) -> Vec<String> {
        let mut names: Vec<String> = vec![
            crate::hotwords::PHRASES.to_string(),
            crate::hotwords::LEARNED.to_string(),
        ];
        for n in crate::hotwords::builtin_names() {
            if !names.iter().any(|x| x == n) {
                names.push(n.to_string());
            }
        }
        let d = crate::hotwords::dir(&self.cfg, &self.paths);
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("tsv")) {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        if !names.iter().any(|n| n == stem) {
                            names.push(stem.to_string());
                        }
                    }
                }
            }
        }
        for p in &self.cfg.hotwords.packs {
            if !names.contains(p) {
                names.push(p.clone());
            }
        }
        names
    }

    /// 从磁盘（缺失/为空则内置副本）载入当前包到编辑器
    fn hw_reload(&mut self) {
        self.hw_terms = crate::hotwords::read_pack(&self.cfg, &self.paths, &self.hw_pack);
        self.hw_filter.clear();
        self.hw_dirty = false;
        self.hw_confirm_restore = false;
        let path = crate::hotwords::pack_path(&self.cfg, &self.paths, &self.hw_pack);
        self.hw_status = format!("已载入 {} 条（{}）", self.hw_terms.len(), path.display());
    }

    /// 写回 tsv + 让 config 变更立即生效（录音侧热词随配置热更新重建）
    fn hw_save(&mut self) {
        let d = crate::hotwords::dir(&self.cfg, &self.paths);
        if let Err(e) = std::fs::create_dir_all(&d) {
            self.hw_status = format!("创建目录失败: {e}");
            return;
        }
        let path = crate::hotwords::pack_path(&self.cfg, &self.paths, &self.hw_pack);
        // 「我打过的词」走带锁的写入（worker 每 30s 也会往同一个文件导入）
        let res = if self.hw_pack == crate::hotwords::LEARNED {
            crate::learn::save_terms(&self.cfg, &self.paths, &self.hw_terms)
        } else {
            let body = crate::hotwords::render_tsv(&self.hw_pack, &self.hw_terms);
            std::fs::write(&path, body).map_err(|e| anyhow::anyhow!("{e}"))
        };
        match res {
            Ok(()) => {
                self.hw_dirty = false;
                let ncode = self.hw_terms.iter().filter(|t| !t.code.trim().is_empty()).count();
                // ★ 打字侧立刻重写 Rime 候选表（2026-10-21 修）：
                // 旧实现只发 ReloadHotwords（录音/整理侧），打字侧要等「学习到 20 个新词」
                // 才会被 tick_learn 顺手刷一次 —— 用户新加的「快捷输入」（快递地址 gds）
                // 当场打字根本出不来，看起来像功能坏了。
                let rime_note = match crate::hotwords::apply_to_rime(&self.cfg, &self.paths) {
                    Ok(Some(p)) => {
                        tracing::info!("[热词] 已重写打字表: {p}");
                        "，已写入打字表（切窗口即生效）"
                    }
                    Ok(None) => "（未检测到倾听输入法，跳过打字表）",
                    Err(e) => {
                        tracing::warn!("[热词] 重写打字表失败: {e:#}");
                        "（写打字表失败，见日志）"
                    }
                };
                self.hw_status = format!(
                    "已保存 {} 条（{ncode} 条带打字编码）→ {}{rime_note}",
                    self.hw_terms.len(),
                    path.display()
                );
                touch!(self); // 让自动保存把 config（启用包/上限）一并落盘
                // 录音侧立即生效（tsv 改的是文件、config 没变，所以要显式通知）
                let _ = self.cmd_tx.send(Cmd::ReloadHotwords);
                tracing::info!("[热词] {}", self.hw_status);
            }
            Err(e) => self.hw_status = format!("保存失败: {e:#}"),
        }
    }

    fn section_inject(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("注入").show(ui, |ui| {
            egui::Grid::new("g_inject")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("注入后恢复剪贴板");
                    if ui
                        .checkbox(&mut self.cfg.inject.restore_clipboard, "")
                        .changed()
                    {
                        touch!(self);
                    }
                    ui.end_row();

                    ui.label("写入后延迟（ms）");
                    if ui
                        .add(egui::DragValue::new(&mut self.cfg.inject.paste_delay_ms).range(0..=2000))
                        .changed()
                    {
                        touch!(self);
                    }
                    ui.end_row();

                    ui.label("恢复延迟（ms）");
                    if ui
                        .add(egui::DragValue::new(&mut self.cfg.inject.restore_delay_ms).range(0..=5000))
                        .changed()
                    {
                        touch!(self);
                    }
                    ui.end_row();

                    ui.label("Shift+热键 撤销换原文");
                    if ui
                        .checkbox(&mut self.cfg.inject.raw_replace_undo, "")
                        .changed()
                    {
                        touch!(self);
                    }
                    ui.end_row();

                    ui.label("分段间分隔符");
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut self.cfg.inject.chunk_separator)
                                    .desired_width(60.0),
                            )
                            .changed()
                        {
                            touch!(self);
                        }
                        ui.weak("聊天框建议用空格（换行可能直接发送消息）");
                    });
                    ui.end_row();
                });
        });
    }

    /// 顶部「语音引擎」状态卡：状态点 + 总开关 + 摘要
    /// 「链路状态」卡（2026-10-09）：8 环灯 + 总判 + 一键修复
    ///
    /// 判级与文案全部来自 `chain::Chain::rows()`（与 `listen.exe --status` **同一处**），
    /// 界面只负责上色——不然两个入口会各判一套，修了一处另一处还错。
    fn chain_card(&mut self, ui: &mut egui::Ui) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            let healthy = self.chain.healthy();
            // 左：总判 + 总开关；右：一键修复（用 Sides 而不是 horizontal：
            // 否则按钮会被长文案挤出窗口右边 —— 2026-10-09 截图实测到的布局 bug）
            // 注意：两个闭包同时传进 Sides::show，不能各自借 &mut self（借用冲突），
            // 所以先在闭包里只改局部变量，闭包结束后再回写。
            let enabled = self.cfg.general.voice_enabled;
            let mut on = enabled;
            let mut want_repair = false;
            egui::Sides::new().height(24.0).show(
                ui,
                |ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                    let color = if healthy { OK_GREEN } else { BAD_RED };
                    ui.painter().circle_filled(rect.center(), 6.0, color);
                    ui.label(
                        egui::RichText::new(if healthy {
                            "链路状态：正常"
                        } else {
                            "链路状态：有异常"
                        })
                        .strong()
                        .color(color),
                    );
                    ui.separator();
                    ui.checkbox(&mut on, "启用语音输入")
                        .on_hover_text("OFF = 停引擎（卸模型/摘热键），倾听输入法不再自动拉起");
                },
                |ui| {
                    if ui
                        .add_enabled(enabled, egui::Button::new("🔧 一键修复"))
                        .on_hover_text(
                            "重装钩子 + 重开麦克风 + 重新加载识别模型 + 拉起整理后端；\
                             只修“不必重启进程就能修”的环，录音/识别中不可用",
                        )
                        .clicked()
                    {
                        want_repair = true;
                    }
                },
            );
            if on != enabled {
                self.cfg.general.voice_enabled = on;
                let ctx = ui.ctx().clone();
                self.apply(&ctx);
            }
            if want_repair {
                let _ = self.cmd_tx.send(crate::Cmd::Repair);
                self.status = "已请求一键修复…".into();
            }
            // 列宽上限：长文案（麦克风静音提示 / 注入失败原因）必须**换行**而不是把整张卡撑宽
            // （2026-10-09 截图实测：不设限宽时右侧内容直接被窗口裁掉，一键修复按钮都看不见）
            let col_w = (ui.available_width() - 96.0).max(240.0);
            ui.set_max_width(col_w + 96.0);
            egui::Grid::new("chain_rows")
                .num_columns(2)
                .spacing([10.0, 3.0])
                .max_col_width(col_w)
                .show(ui, |ui| {
                    for (lvl, name, text) in self.chain.rows() {
                        let color = level_color(lvl);
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(10.0, 10.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().circle_filled(rect.center(), 5.0, color);
                            // 悬停名字也能看完整文案（换行后仍然好读）
                            ui.label(egui::RichText::new(name).strong())
                                .on_hover_text(text.clone());
                        });
                        ui.scope(|ui| {
                            ui.set_max_width(col_w);
                            ui.label(egui::RichText::new(text).color(color));
                        });
                        ui.end_row();
                    }
                });
            if let Some(advice) = self.chain.advice() {
                ui.colored_label(BAD_RED, advice);
            }
            ui.weak("命令行查同一份：listen.exe --status（--json 给脚本）；日志：安装目录\\logs");
        });
    }

    fn section_floating(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("悬浮窗")
            .default_open(true)
            .show(ui, |ui| {
                egui::Grid::new("g_floating")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("显示悬浮窗");
                        if ui
                            .checkbox(&mut self.cfg.floating.enabled, "说话时显示状态与最近段落")
                            .changed()
                        {
                            touch!(self);
                        }
                        ui.end_row();

                        ui.label("字号");
                        if ui
                            .add(egui::DragValue::new(&mut self.cfg.floating.font_size).range(12..=32))
                            .changed()
                        {
                            touch!(self);
                        }
                        ui.end_row();

                        ui.label("保留段数");
                        if ui
                            .add(egui::DragValue::new(&mut self.cfg.floating.history).range(1..=10))
                            .changed()
                        {
                            touch!(self);
                        }
                        ui.end_row();

                        ui.label("每行最大字数");
                        ui.horizontal(|ui| {
                            if ui
                                .add(egui::DragValue::new(&mut self.cfg.floating.max_chars).range(0..=200))
                                .changed()
                            {
                                touch!(self);
                            }
                            ui.weak("0 = 不截断");
                        });
                        ui.end_row();

                        ui.label("停留时间");
                        ui.horizontal(|ui| {
                            // 2026-10-20：用户报「太久不消失影响体验」（老配置 30 秒、而且只是个填数字的框）
                            let mut gear = self.cfg.floating.hide_gear();
                            let resp = egui::ComboBox::from_id_salt("float_hide_gear")
                                .selected_text(self.cfg.floating.hide_label())
                                .show_ui(ui, |ui| {
                                    for (idx, (_s, hint)) in
                                        crate::config::FLOAT_HIDE_GEARS.iter().enumerate()
                                    {
                                        let it = ui.selectable_value(&mut gear, Some(idx), *hint);
                                        // GUI 自动化用：弹层每一项的真实坐标（AIVOICE_GUI_PROBE=1）
                                        const NAMES: [&str; 5] =
                                            ["fh0", "fh1", "fh2", "fh3", "fh4"];
                                        if let Some(n) = NAMES.get(idx) {
                                            self.probe_note(n, it.rect);
                                        }
                                        if it.changed() {
                                            self.cfg.floating.set_hide_gear(idx);
                                            touch!(self);
                                        }
                                    }
                                })
                                .response
                                .on_hover_text(
                                    "会话结束后悬浮窗再停留多久然后自动消失（秒）。\
                                     越短越不挡视线；想回看刚才说了什么就选长一点。",
                                );
                            self.probe_note("combo_float_hide", resp.rect);
                            ui.weak("会话结束后自动消失");
                        });
                        ui.end_row();

                        ui.label("显示文字");
                        if ui
                            .checkbox(
                                &mut self.cfg.floating.show_text,
                                "在悬浮窗里显示识别/整理的文字（默认关）",
                            )
                            .on_hover_text(
                                "关（默认）：悬浮窗只显示状态点 / 门控电平 / 计时 / 「已注入 N 字」。\
                                 开：会显示识别原文与整理后的文字（调试用，输出后仍可能停留到自动消失）。",
                            )
                            .changed()
                        {
                            touch!(self);
                        }
                        ui.end_row();
                    });
                ui.weak("拖动悬浮窗可移动，位置自动记忆；这里的改动即时生效并自动保存");
            });
    }

    fn section_model(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("本地模型")
            .default_open(true)
            .show(ui, |ui| {
                // 用 pump() 每 2s 刷新的缓存，避免每帧都去 stat 磁盘
                let ready = self.last_model_ready;
                let dir = self.paths.resolve(&self.cfg.asr.local_model);
                ui.label(format!(
                    "状态：{}",
                    if ready { "已就绪" } else { "缺失" }
                ));
                ui.weak(format!("目录：{}", dir.display()));

                let (running, msg, done) = {
                    let g = self.download.lock().unwrap();
                    (g.running, g.msg.clone(), g.done)
                };
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!running && !ready, egui::Button::new("⬇ 下载本地模型（约 230MB）"))
                        .clicked()
                    {
                        self.start_download();
                    }
                    if ui.button("打开模型目录").clicked() {
                        crate::open_path(&dir);
                    }
                });
                if running {
                    ui.add(egui::ProgressBar::new(0.0).animate(true).text(&msg));
                } else if !msg.is_empty() {
                    if done {
                        ui.colored_label(egui::Color32::from_rgb(60, 180, 100), &msg);
                    } else {
                        ui.colored_label(egui::Color32::from_rgb(220, 90, 90), &msg);
                    }
                }
                ui.weak("也可以手动把 model.int8.onnx 与 tokens.txt 放进该目录（U 盘拷贝）");
            });
    }

    fn section_log(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("运行日志").show(ui, |ui| {
            if ui.button("打开日志目录").clicked() {
                crate::open_path(&self.paths.logs_dir());
            }
            egui::ScrollArea::vertical()
                .max_height(140.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for n in &self.notes {
                        ui.small(n);
                    }
                });
        });
    }

    // ------------------------------------------------------------ 悬浮窗（调试版）

    /// 每帧维护悬浮窗 viewport：会话出现、结束延迟隐藏；隐藏时不额外请求重绘
    fn draw_float_viewport(&mut self, ctx: &egui::Context) {
        if !self.cfg.floating.enabled {
            if self.float_created {
                // 不再调用 show_viewport_immediate → egui 回收该 viewport（窗口关闭）
                self.float_created = false;
                self.float_measured_h = None;
                self.float_layered = false;
            }
            self.float_visible = false;
            return;
        }

        // 隐藏延迟到点
        if let Some(t) = self.float_hide_at {
            if Instant::now() >= t {
                self.float_hide_at = None;
                self.save_float_pos();
            }
        }
        let visible = self.float_session.is_some() || self.float_hide_at.is_some();
        self.float_visible = visible;
        let (w, h) = self.float_window_size();

        let mut builder = egui::ViewportBuilder::default()
            .with_title(FLOAT_WINDOW_TITLE)
            .with_decorations(false)
            // 透明改由 Win32（LWA_ALPHA + accent 模糊）实现，不再走 winit 的 DWM 透明
            .with_transparent(false)
            .with_always_on_top()
            .with_taskbar(false)
            .with_active(false) // 不抢焦点（语音注入必须留给目标窗口）
            .with_resizable(false)
            .with_inner_size([w, h])
            .with_visible(visible);
        if !self.float_created {
            let (x, y) = self.float_initial_pos(ctx, w, h);
            // ✅ 必须钳制到「显示器工作区」：否则记忆位置可能整条压在任务栏下面
            // → 用户按 F9 什么都看不到（2026-10-09 实测：pos_y=989、窗高 48，工作区只到 1040 逻辑）
            let (x, y) = clamp_to_work_area(ctx, x, y, w, h);
            if (x, y) != self.float_initial_pos(ctx, w, h) {
                tracing::info!("悬浮窗位置已从记忆值钳回可见区: ({x:.0}, {y:.0})");
            }
            builder = builder.with_position([x, y]);
        }

        let mut measured: Option<f32> = None;
        let mut outer: Option<(f32, f32)> = None;
        let mut drag_done = false;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of(FLOAT_VIEWPORT_ID),
            builder,
            |ui, _class| {
                outer = ui
                    .ctx()
                    .input(|i| i.viewport().outer_rect)
                    .map(|r| (r.min.x, r.min.y));
                let (m, d) = self.float_ui(ui);
                measured = m;
                drag_done = d;
            },
        );
        self.float_created = true;
        // 窗口刚创建后套一次半透明+毛玻璃（失败则保持不透明，不影响功能）；
        // 失败要说清楚原因，但别每帧刷日志（至多 3 次）
        if !self.float_layered {
            match apply_float_transparency(FLOAT_WINDOW_TITLE) {
                Ok(()) => {
                    self.float_layered = true;
                    tracing::info!("悬浮窗半透明已生效（LWA_ALPHA={FLOAT_ALPHA} + 毛玻璃）");
                }
                Err(e) => {
                    if self.float_layer_tries < 3 {
                        self.float_layer_tries += 1;
                        tracing::warn!("悬浮窗半透明未生效（第 {} 次）: {e}", self.float_layer_tries);
                    }
                }
            }
        }
        if let Some(m) = measured {
            self.float_measured_h = Some(m);
        }
        if let Some(p) = outer {
            let mut p = p;
            // 拖动结束后：若落点被工作区切掉（例如拖到任务栏上），拉回可见区并落盘
            if drag_done {
                let (cx, cy) = clamp_to_work_area(ctx, p.0, p.1, w, h);
                if (cx, cy) != p {
                    tracing::info!(
                        "悬浮窗落点不可见，已拉回: ({:.0},{:.0}) → ({cx:.0},{cy:.0})",
                        p.0,
                        p.1
                    );
                    ctx.send_viewport_cmd_to(
                        egui::ViewportId::from_hash_of(FLOAT_VIEWPORT_ID),
                        egui::ViewportCommand::OuterPosition(egui::pos2(cx, cy)),
                    );
                    p = (cx, cy);
                }
            }
            if self.float_pos != Some(p) {
                let was_known = self.float_pos.is_some();
                self.float_pos = Some(p);
                // 拖动结束或窗口刚隐藏时落盘（拖拽中不写文件）
                if drag_done || (was_known && !visible) {
                    self.save_float_pos();
                }
            }
        }
    }

    /// 悬浮窗内容：状态 + 实时语句同一行（省一行高度）+ 历史行；返回（内容高度, 拖动是否结束）
    fn float_ui(&mut self, ui: &mut egui::Ui) -> (Option<f32>, bool) {
        let font = self.cfg.floating.font_size.clamp(12, 32) as f32;
        let max_chars = self.cfg.floating.max_chars as usize;
        let raw_color = egui::Color32::from_rgb(232, 236, 242);
        let polish_color = egui::Color32::from_rgb(96, 220, 226);
        let dim_color = egui::Color32::from_rgb(130, 138, 150);
        let hint_color = egui::Color32::from_rgb(240, 186, 96);

        // 整窗拖动（无边框窗口：按住在任意位置拖动）
        let resp = ui.interact(
            ui.max_rect(),
            egui::Id::new("listen_float_drag"),
            egui::Sense::click_and_drag(),
        );
        if resp.dragged() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        let drag_done = resp.drag_stopped();

        // 展开为显示行：(文本, 颜色)；原文行在前，整理行随后
        //
        // `show_text = false`（默认）时**一行字都不显示**：文字已经注入到目标窗口了，
        // 悬浮窗再留着上一批就是干扰（用户原话：「输出的文字不应该再出现在悬浮窗中」）。
        // 只保留状态 / 门控电平 / 计时 / 一句「已注入 N 字」。
        let mut lines: Vec<(String, egui::Color32)> = Vec::new();
        if self.cfg.floating.show_text {
            for b in &self.float_batches {
                for raw in &b.raws {
                    lines.push((truncate_chars(raw, max_chars), raw_color));
                }
                if let Some(p) = &b.polished {
                    lines.push((truncate_chars(p, max_chars), polish_color));
                }
            }
        }
        let _ = (raw_color, polish_color);

        let frame = egui::Frame::NONE
            // 高透黑：透明清屏下 alpha 即最终不透明度（约 55%）
            .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 140))
            .corner_radius(10.0)
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                // 第一行：状态点 + 状态 + 实时语句（同一行）+ 右侧计时
                egui::Sides::new().height(font * 1.6).shrink_left().show(
                    ui,
                    |ui| {
                        let (color, label) = state_visual(self.state);
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(font * 0.9, font * 0.9),
                            egui::Sense::hover(),
                        );
                        ui.painter().circle_filled(rect.center(), font * 0.3, color);
                        ui.label(egui::RichText::new(label).size(font).strong().color(raw_color));
                        // 实时语句紧接状态同一行（过长截断，不换行、不挤走计时）
                        match lines.first() {
                            Some((text, color)) => {
                                let resp = ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(text).size(font).color(*color),
                                    )
                                    .truncate(),
                                );
                                float_row_menu(&resp, text);
                            }
                            None => {
                                // 没有文字行时显示一句话：已注入 N 字 / 未出字原因 / 等待语音
                                let (msg, color) = match &self.float_hint {
                                    Some(h) => (h.clone(), hint_color),
                                    None => ("（等待语音…）".to_string(), dim_color),
                                };
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(msg).size(font * 0.9).color(color),
                                    )
                                    .truncate(),
                                );
                            }
                        }
                    },
                    |ui| {
                        // D3 实时读数：当前是否判为语音 + 电平（worker 已节流到 ≤4 次/秒）
                        if let Some((speech, rms)) = self.float_gate {
                            let color = if speech {
                                egui::Color32::from_rgb(120, 230, 150)
                            } else {
                                dim_color
                            };
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} {}  电平 {:.3}",
                                    if speech { "语音" } else { "静音" },
                                    if speech { "●" } else { "○" },
                                    rms
                                ))
                                .size(font * 0.85)
                                .color(color),
                            )
                            .on_hover_text(
                                "门控实时读数：● = 此刻判为语音（会被切成一段送识别），○ = 静音。\
                                 电平是最近一块音频的 RMS；一直 ○ 且电平很小 = 声音太轻没进去。",
                            );
                        }
                        ui.label(
                            egui::RichText::new(self.float_timer_text())
                                .size(font)
                                .color(dim_color),
                        );
                    },
                );
                // 其余历史行（第一行已随状态同一行显示）
                for (text, color) in lines.iter().skip(1) {
                    let resp = ui.label(egui::RichText::new(text).size(font).color(*color));
                    float_row_menu(&resp, text);
                }
            });
        (Some(frame.response.rect.height() + 2.0), drag_done)
    }

    fn float_timer_text(&self) -> String {
        let d = match self.float_session {
            Some(t0) => t0.elapsed(),
            None => self.float_last_dur,
        };
        let s = d.as_secs();
        format!("{:02}:{:02}", s / 60, s % 60)
    }

    /// 窗口尺寸：宽随「每行最大字数×字号」，高用上一帧实测值（自适应内容）
    fn float_window_size(&self) -> (f32, f32) {
        let font = self.cfg.floating.font_size.clamp(12, 32) as f32;
        let max_chars = self.cfg.floating.max_chars;
        let w = if max_chars == 0 {
            640.0
        } else {
            (font * max_chars as f32 * 0.72).clamp(320.0, 1000.0)
        };
        let h = self.float_measured_h.unwrap_or_else(|| {
            let lines = self
                .float_batches
                .iter()
                .map(|b| b.raws.len() + b.polished.is_some() as usize)
                .sum::<usize>()
                .max(1);
            (font * 1.6 * lines as f32 + font * 3.0 + 40.0).clamp(64.0, 600.0)
        });
        (w, h)
    }

    /// 初始位置：配置记忆优先；否则当前显示器底部居中
    fn float_initial_pos(&self, ctx: &egui::Context, w: f32, h: f32) -> (f32, f32) {
        let (cx, cy) = (self.cfg.floating.pos_x, self.cfg.floating.pos_y);
        if cx >= 0 && cy >= 0 {
            return (cx as f32, cy as f32);
        }
        let mon = ctx
            .input(|i| i.viewport().monitor_size)
            .unwrap_or(egui::vec2(1920.0, 1080.0));
        (((mon.x - w) / 2.0).max(0.0), (mon.y - h - 80.0).max(0.0))
    }

    /// 位置记忆落盘（未保存的设置变更时不写，避免误持久化）
    fn save_float_pos(&mut self) {
        let Some((x, y)) = self.float_pos else { return };
        let (x, y) = (x.round() as i32, y.round() as i32);
        if self.cfg.floating.pos_x == x && self.cfg.floating.pos_y == y {
            return;
        }
        self.cfg.floating.pos_x = x;
        self.cfg.floating.pos_y = y;
        if !self.dirty {
            if let Err(e) = self.cfg.save(&self.paths) {
                tracing::warn!("保存悬浮窗位置失败: {e:#}");
            }
        }
    }
}

impl eframe::App for Gui {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // 透明清屏：设置窗由面板铺满不露底；悬浮窗的底色完全交给 Frame 的半透明黑控制。
        // 若用默认清屏色（不透明黑灰），Frame 再叠加会把整体推到近乎不透明，出不来「高透黑」。
        egui::Color32::TRANSPARENT.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.pump(&ctx);
        // 自动保存（取消「保存并应用」后的唯一保存入口）：改动 600ms 后落盘 + 热生效
        self.autosave_tick(&ctx);

        // GUI 帧间隔看门狗：>300ms 说明渲染被占住（wgpu 与 llama-server 抢 GPU 是已知嫌疑）。
        // 热键转发已挪到独立线程，所以这里只影响界面观感与诊断。
        let frame_dt = self.last_frame_at.elapsed();
        self.last_frame_at = Instant::now();
        if frame_dt > Duration::from_millis(300) {
            tracing::warn!(
                "GUI 帧间隔 {}ms（界面可能定格；热键转发已不受影响）",
                frame_dt.as_millis()
            );
        }

        // 外部唤起（倾听输入法托盘/设置窗的「倾听输入法 设置」、第二次启动、--settings）
        self.start_show_watcher(&ctx);
        if self.show_requested.swap(false, Ordering::SeqCst) {
            tracing::info!("收到唤起请求 → 显示设置窗口");
            self.visible_wanted = true;
            show_window(&ctx);
        }

        if ctx.input(|i| i.viewport().close_requested()) && !self.quit {
            tracing::info!("窗口关闭请求 → 隐藏到托盘（程序继续运行）");
            self.visible_wanted = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            // 关窗前把还没到点的自动保存**立刻做掉**（2026-10-21：不再有「未保存」概念）
            if self.dirty {
                self.autosave_at = None;
                self.save_now();
            }
        }

        // eframe 首帧渲染后会无条件 `window.set_visible(true)`（epi_integration.rs:326），
        // 会盖掉启动时的 `with_visible(false)` → 导致「切到输入法就弹设置页」。
        // 这里持续压制：只要不是用户显式打开，就每帧把窗口隐藏回去。
        if !self.visible_wanted {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        egui::CentralPanel::default().show(ui, |ui| self.draw(ui));

        // 悬浮窗（同进程多视口）：会话期间出现，结束后延迟隐藏
        self.draw_float_viewport(&ctx);

        // 让状态/托盘事件保持刷新。
        // 两个窗口都不可见时降频到 400ms（2026-10-08）：没人看的时候 20fps 只是白烧
        // GPU/CPU，而且隐藏窗口的 Present 是「整机 GPU 卡顿」最容易牵连到本进程的时刻。
        // 400ms 而不是完全停：托盘菜单/悬浮窗/事件通道还要靠这帧去轮询。
        let interval = if self.visible_wanted || self.float_visible { 50 } else { 400 };
        ctx.request_repaint_after(Duration::from_millis(interval));
    }
}

pub fn show_window(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
}

fn state_visual(s: State) -> (egui::Color32, &'static str) {
    let c = s.rgba();
    (
        egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]),
        s.label(),
    )
}

fn mode_label(m: &str) -> &'static str {
    // 与 `General::is_toggle_mode()` 同口径：大小写/中文别名都当「切换」
    let t = m.trim();
    if t.eq_ignore_ascii_case("toggle")
        || t.eq_ignore_ascii_case("switch")
        || t == "切换"
        || t == "切换模式"
    {
        "切换：按一下开始 / 再按一下停止"
    } else {
        "按住说话"
    }
}

/// AIVOICE_GUI_PROBE=1 → 周期性打印控件 rect（GUI 自动化/布局排查用）
fn probe_enabled() -> bool {
    static ONCE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ONCE.get_or_init(|| {
        std::env::var("AIVOICE_GUI_PROBE")
            .map(|v| v == "1")
            .unwrap_or(false)
    })
}

/// 打开某个文件所在的目录（配置里的路径填错时方便定位）
fn open_parent(path: &str) {
    let p = std::path::Path::new(path);
    let dir = p.parent().filter(|d| !d.as_os_str().is_empty());
    match dir {
        Some(d) => crate::open_path(d),
        None => crate::open_path(p),
    }
}

fn now_hms() -> String {
    // 与 `listen.exe --status` / 链路卡片同一实现（改一处两边都变）
    crate::chain::now_hms()
}

/// 链路健康度 → 颜色（与 `chain::Level` 一一对应）
const OK_GREEN: egui::Color32 = egui::Color32::from_rgb(60, 180, 100);
const BAD_RED: egui::Color32 = egui::Color32::from_rgb(220, 90, 90);

fn level_color(level: crate::chain::Level) -> egui::Color32 {
    match level {
        crate::chain::Level::Ok => OK_GREEN,
        crate::chain::Level::Warn => egui::Color32::from_rgb(230, 160, 40),
        crate::chain::Level::Bad => BAD_RED,
    }
}

/// egui 默认字体不含中文，从系统字体目录加载一个
fn install_cjk_font(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        "C:/Windows/Fonts/Deng.ttf",
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/simhei.ttf",
        "C:/Windows/Fonts/simsun.ttc",
    ];
    for path in CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let mut fonts = egui::FontDefinitions::default();
        fonts
            .font_data
            .insert("cjk".to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
        if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            list.insert(0, "cjk".to_owned());
        }
        if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            list.push("cjk".to_owned());
        }
        ctx.set_fonts(fonts);
        tracing::info!("已加载中文字体: {path}");
        return;
    }
    tracing::warn!("未找到可用的中文字体，界面中文可能显示为方块");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_batch_groups_by_polish() {
        let mut b = Vec::new();
        float_push_segment(&mut b, "一段".into(), 10);
        float_push_segment(&mut b, "二段".into(), 10);
        // 同一批次：两条原文
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].raws.len(), 2);
        assert!(b[0].polished.is_none());

        float_attach_polished(&mut b, "整理".into());
        assert_eq!(b[0].polished.as_deref(), Some("整理"));

        // 出过整理行 → 新段另起一批
        float_push_segment(&mut b, "三段".into(), 10);
        assert_eq!(b.len(), 2);
        assert_eq!(b[1].raws, vec!["三段".to_string()]);
    }

    #[test]
    fn float_trim_keeps_recent() {
        let mut b = Vec::new();
        for t in ["a", "b", "c", "d"] {
            float_push_segment(&mut b, t.into(), 2);
        }
        let total: usize = b.iter().map(|x| x.raws.len()).sum();
        assert_eq!(total, 2);
        let last = b.last().unwrap();
        assert_eq!(last.raws, vec!["c".to_string(), "d".to_string()]);
    }

    #[test]
    fn float_trim_drops_empty_batches() {
        let mut b = Vec::new();
        float_push_segment(&mut b, "a".into(), 1);
        float_attach_polished(&mut b, "A".into());
        float_push_segment(&mut b, "b".into(), 1);
        // 保留 1 段：首批被裁掉（含其整理行）
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].raws, vec!["b".to_string()]);
        assert!(b[0].polished.is_none());
    }

    #[test]
    fn truncate_chars_basic() {
        assert_eq!(truncate_chars("hello", 0), "hello");
        assert_eq!(truncate_chars("hello", 10), "hello");
        assert_eq!(truncate_chars("hello", 5), "hello");
        assert_eq!(truncate_chars("hello", 4), "hel…");
        // 中文按字符数（不是字节数）
        assert_eq!(truncate_chars("语音输入测试", 4), "语音输…");
    }
}
