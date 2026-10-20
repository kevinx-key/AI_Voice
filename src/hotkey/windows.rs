//! Windows 低级键盘钩子（WH_KEYBOARD_LL）
//!
//! 关键点：回调里**一律不拦截**（`CallNextHookEx` 原样返回），只旁路记录
//! 目标键的按下/松开，因此不会吞掉 F9 在原应用里的功能（Excel 重算等）。
//! 钩子必须在**独立线程**上安装并跑消息循环。

use super::{classify, HotkeyEvent, MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_WIN};
use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;
use std::time::Duration;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HHOOK, PM_NOREMOVE, PeekMessageW,
    PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN,
    WM_SYSKEYUP,
};

static HOOK_TX: OnceLock<Sender<(HotkeyEvent, u64)>> = OnceLock::new();
static HOTKEY_VK: AtomicU32 = AtomicU32::new(0);
/// 要求的修饰键集合（精确匹配；主键自身的修饰位在 classify 里排除）
static HOTKEY_MODS: AtomicU32 = AtomicU32::new(0);
/// 物理按键当前是否处于按下状态（抑制自动重复）
static KEY_DOWN: AtomicBool = AtomicBool::new(false);
/// 本次按下是否属于"录音"（非 Shift 变体）
static RECORDING: AtomicBool = AtomicBool::new(false);
/// 热键事件序号：每次"录音按下"递增，用于 Press/Release 配对
static PRESS_SEQ: AtomicU64 = AtomicU64::new(0);
/// 当前已武装（按下）的序号；Release 携带它，供消费端配对
static ARMED_SEQ: AtomicU64 = AtomicU64::new(0);

const WM_HEARTBEAT: u32 = WM_APP + 1;
/// 按键状态自检：KEYUP 丢失自愈 + “被修饰键挡下”计数上报
///
/// 动机（2026-10-06 装机实测）：用户报“按了几次没反应”，而日志里**一条按下都没有** ——
/// 低级钩子只在真正发出 Press 时才留痕，所以两种情况全隐：
///   ①KEYUP 丢失 → `KEY_DOWN` 卡在 true → 之后每次按下都被当自动重复静默丢掉；
///   ②修饰键卡住（Ctrl/Alt/Win 的 KEYUP 丢）→ `classify` 不匹配，同样静默丢掉。
/// 自检线程（非回调，可以记日志）每 2s 看一次，两种情况都能被发现并自愈。
const WM_KEYCHECK: u32 = WM_APP + 2;
/// 被修饰键挡下的次数（钩子回调里只做原子写，日志由自检线程打）
static IGNORED_MISMATCH: AtomicU64 = AtomicU64::new(0);
/// 最近一次被挡下时的修饰键状态
static LAST_IGNORED_MODS: AtomicU32 = AtomicU32::new(0);
/// 钩子回调被调用的总次数（任意按键）。**唯一能发现「被系统静默摘钩」的探针**：
/// 摘钩后回调根本不执行，其它任何计数/日志都不会动。仅原子自增，符合回调内不加锁不日志的纪律。
static HOOK_CALLS: AtomicU64 = AtomicU64::new(0);

// ---- 累计量（2026-10-09，链路状态卡片用）----
//
// 为什么不能直接用上面三个：心跳线程每 60s 会 `swap(0)` 把它们清零（那是**窗口量**，
// 专门用来看“近 60s 有没有回调”）。卡片要的是**从进程启动至今的累计值**，
// 所以另存三个永不清零的计数器（与窗口计数器在同一处 +1）。
static HOOK_CALLS_TOTAL: AtomicU64 = AtomicU64::new(0);
static HOOK_VK_CALLS_TOTAL: AtomicU64 = AtomicU64::new(0);
static KEYUP_SELFHEAL_TOTAL: AtomicU64 = AtomicU64::new(0);

/// 钩子统计（累计值；链路状态卡片用）
pub fn stats() -> super::HookStats {
    super::HookStats {
        calls: HOOK_CALLS_TOTAL.load(Ordering::Relaxed),
        vk_calls: HOOK_VK_CALLS_TOTAL.load(Ordering::Relaxed),
        selfheal: KEYUP_SELFHEAL_TOTAL.load(Ordering::Relaxed),
    }
}
/// 其中命中目标主键的次数（与 HOOK_CALLS 对比可区分“钩子死了”和“键被吞了”）
static HOOK_VK_CALLS: AtomicU64 = AtomicU64::new(0);
/// 回调内“KEYUP 被系统丢掉”的自愈次数。
/// 不自愈的话：KEY_DOWN 卡在 true → 后续每一次按下都被当成自动重复静默丢弃（零日志），
/// 一直要到自检才恢复 —— 这就是用户体感的“按了热键没反应”（2026-10-07 实测 90s 内丢 2 次）。
static KEYUP_SELFHEAL: AtomicU64 = AtomicU64::new(0);

/// 读取当前逻辑修饰键状态（左右任一算按住）
fn read_mods() -> u8 {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN,
    };
    let down = |vk: u16| unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 };
    let mut m = 0u8;
    if down(VK_LSHIFT.0) || down(VK_RSHIFT.0) {
        m |= MOD_SHIFT;
    }
    if down(VK_LCONTROL.0) || down(VK_RCONTROL.0) {
        m |= MOD_CTRL;
    }
    if down(VK_LMENU.0) || down(VK_RMENU.0) {
        m |= MOD_ALT;
    }
    if down(VK_LWIN.0) || down(VK_RWIN.0) {
        m |= MOD_WIN;
    }
    m
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // 心跳探针：必须在任何过滤/分支之前计数，否则“钩子没被调用”就仍不可见
    HOOK_CALLS.fetch_add(1, Ordering::Relaxed);
    HOOK_CALLS_TOTAL.fetch_add(1, Ordering::Relaxed);
    if code >= 0 {
        // SAFETY: 系统保证 lparam 指向有效的 KBDLLHOOKSTRUCT
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let vk = kb.vkCode as u16;
        if vk == HOTKEY_VK.load(Ordering::Relaxed) as u16 {
            HOOK_VK_CALLS.fetch_add(1, Ordering::Relaxed);
            HOOK_VK_CALLS_TOTAL.fetch_add(1, Ordering::Relaxed);
            let msg = wparam.0 as u32;
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
            if down || up {
                if let Some(tx) = HOOK_TX.get() {
                    if down {
                        // ★ 治“按了没反应”的关键一行（2026-10-07）：KEYUP 被系统丢掉时，
                        // KEY_DOWN 会卡在 true（连自动重复），后续每一次按下都被静默丢弃。
                        // 这里当场判定“物理键已抬起”并复位，把本次按下当新按下处理：
                        // 代价从“往后数秒内所有按下全吞”降到“最多影响丢 KEYUP 那一次”。
                        if KEY_DOWN.load(Ordering::SeqCst) && !is_key_down(vk) {
                            KEY_DOWN.store(false, Ordering::SeqCst);
                            RECORDING.store(false, Ordering::SeqCst);
                            KEYUP_SELFHEAL.fetch_add(1, Ordering::Relaxed);
                            KEYUP_SELFHEAL_TOTAL.fetch_add(1, Ordering::Relaxed);
                        }
                        if !KEY_DOWN.swap(true, Ordering::SeqCst) {
                            let spec_mods = HOTKEY_MODS.load(Ordering::Relaxed) as u8;
                            let state = read_mods();
                            // 注意：LL 钩子回调里禁止日志/加锁（回调超时会被系统摘钩），
                            // 事件日志统一在消费端（gui::pump）打。
                            match classify(vk, spec_mods, state) {
                                Some(HotkeyEvent::Raw) => {
                                    let _ = tx.send((HotkeyEvent::Raw, 0));
                                }
                                Some(HotkeyEvent::Press) => {
                                    RECORDING.store(true, Ordering::SeqCst);
                                    // 记下按下瞬间的音频钟（预热环形缓冲据此精确切片，补回起头丢字）
                                    crate::audio::note_press();
                                    let seq = PRESS_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
                                    ARMED_SEQ.store(seq, Ordering::SeqCst);
                                    let _ = tx.send((HotkeyEvent::Press, seq));
                                }
                                _ => {
                                    // 修饰键不匹配（或未匹配）：只累加计数，让自检线程去记日志
                                    //（回调里禁止日志/加锁，超时会被系统摘钩）
                                    LAST_IGNORED_MODS.store(state as u32, Ordering::Relaxed);
                                    IGNORED_MISMATCH.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                    } else if KEY_DOWN.swap(false, Ordering::SeqCst)
                        && RECORDING.swap(false, Ordering::SeqCst)
                    {
                        // 携带当前武装序号：消费端据此丢弃过期/无主的 Release
                        let seq = ARMED_SEQ.load(Ordering::SeqCst);
                        let _ = tx.send((HotkeyEvent::Release, seq));
                    }
                }
            }
        }
    }
    // 一律放行，绝不吞键
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// 物理按键是否处于按下状态（松手看门狗用；SendInput 注入的按键也能反映）
pub fn is_key_down(vk: u16) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

pub struct HookHandle {
    thread_id: u32,
    stop: std::sync::Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl HookHandle {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }

    /// 停止并等待钩子线程完全退出（挂起后重装前调用，避免新旧钩子并存）
    pub fn stop_and_wait(mut self) {
        self.stop();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for HookHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 运行时切换热键（钩子回调动态读取，无需重装钩子）
pub fn set_vk(vk: u16, mods: u8) {
    HOTKEY_VK.store(vk as u32, Ordering::Relaxed);
    HOTKEY_MODS.store(mods as u32, Ordering::Relaxed);
    KEY_DOWN.store(false, Ordering::SeqCst);
    RECORDING.store(false, Ordering::SeqCst);
    tracing::info!("热键已切换为 vk=0x{vk:02X} mods=0x{mods:02X}");
}

/// 安装钩子（不写原子：vk/mods 由 spawn/set_vk 维护，
/// 看门狗重装时保留运行时切换的值，避免被改回启动值）
fn install() -> Result<isize> {
    let h = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) }
        .context("SetWindowsHookExW 失败")?;
    Ok(h.0 as isize)
}

/// 在独立线程安装钩子并跑消息循环
pub fn spawn(vk: u16, mods: u8, tx: Sender<(HotkeyEvent, u64)>) -> Result<HookHandle> {
    HOTKEY_VK.store(vk as u32, Ordering::Relaxed);
    HOTKEY_MODS.store(mods as u32, Ordering::Relaxed);
    // 重装（挂起→恢复）时清掉上次遗留的按键状态
    KEY_DOWN.store(false, Ordering::SeqCst);
    RECORDING.store(false, Ordering::SeqCst);
    ARMED_SEQ.store(0, Ordering::SeqCst);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<u32, String>>();
    // 线程启动后立刻上报 OS tid（安装超时/失败时用于 PostThreadMessageW 收尾）
    let (tid_tx, tid_rx) = std::sync::mpsc::channel::<u32>();
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();

    let join = std::thread::Builder::new()
        .name("hotkey-hook".into())
        .spawn(move || {
            let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
            // ★ 提高钩子线程优先级：CPU 被 LLM 等吃满时，低级钩子回调若不能及时返回，
            // Windows 会**直接跳过这次回调**（LowLevelHooksTimeout）——实测就是 KEYUP 被丢，
            // 进而触发上面那条“静默吞键”。与“回调里不日志/不加锁”同一个道理。
            unsafe {
                use windows::Win32::System::Threading::{
                    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
                };
                let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
            }
            // 先创建线程消息队列：保证之后任何时刻 PostThreadMessageW(WM_QUIT) 可达，
            // stop_and_wait 的 join 才不会挂住（挂起路径依赖它）
            unsafe {
                let mut m = MSG::default();
                let _ = PeekMessageW(&mut m, None, 0, 0, PM_NOREMOVE);
            }
            let _ = tid_tx.send(tid);
            let mut cur = match install() {
                Ok(h) => h,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("{e:#}")));
                    return;
                }
            };
            // 通道只占一次（挂起/恢复会重复安装，重装复用同一接收端）
            if HOOK_TX.get().is_none() {
                let _ = HOOK_TX.set(tx);
            }
            if stop_thread.load(Ordering::SeqCst) {
                // spawn 已超时放弃：立即退出，不留下活动钩子
                unsafe {
                    let _ = UnhookWindowsHookEx(HHOOK(cur as *mut _));
                }
                return;
            }
            let _ = ready_tx.send(Ok(tid));

            // 看门狗：每 5s 自检按键状态，每 60s 重装钩子
            // （Windows 在超时/无响应时会静默摘除低级钩子，重装是最直接的修复）
            let hb_tid = tid;
            std::thread::Builder::new()
                .name("hotkey-watchdog".into())
                .spawn(move || {
                    let mut ticks = 0u32;
                    loop {
                        // 2s 节拍（原 5s）：KEYUP 丢失后的恢复窗口越短越好
                        std::thread::sleep(Duration::from_secs(2));
                        if stop_thread.load(Ordering::SeqCst) {
                            break;
                        }
                        unsafe {
                            let _ = PostThreadMessageW(hb_tid, WM_KEYCHECK, WPARAM(0), LPARAM(0));
                        }
                        ticks += 1;
                        if ticks % 30 == 0 {
                            unsafe {
                                let _ = PostThreadMessageW(hb_tid, WM_HEARTBEAT, WPARAM(0), LPARAM(0));
                            }
                        }
                    }
                })
                .ok();

            let mut hb_ticks = 0u32;
            let mut hb_calls = 0u64;
            let mut hb_vk = 0u64;
            let mut hb_heal = 0u64;
            let mut msg = MSG::default();
            loop {
                let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                if ret.0 <= 0 {
                    break; // WM_QUIT 或错误
                }
                if msg.message == WM_KEYCHECK {
                    // ⓪ 钩子活性心跳：回调**到底有没有被系统调用**。
                    // 这是本项目唯一曾经完全不可观测的吞键路径（系统静默摘钩），
                    // 只能靠“回调计数停更”抓现行。
                    hb_calls += HOOK_CALLS.swap(0, Ordering::SeqCst);
                    hb_vk += HOOK_VK_CALLS.swap(0, Ordering::SeqCst);
                    hb_heal += KEYUP_SELFHEAL.swap(0, Ordering::SeqCst);
                    hb_ticks += 1;
                    if hb_ticks % 30 == 0 {
                        tracing::info!(
                            "热键钩子心跳：近 60s 回调 {hb_calls} 次（命中主键 {hb_vk} 次，回调内自愈 KEYUP 丢失 {hb_heal} 次）"
                        );
                        hb_calls = 0;
                        hb_vk = 0;
                        hb_heal = 0;
                    }
                    // ① KEYUP 丢失自愈：记着“按着”但物理键已抬起
                    if KEY_DOWN.load(Ordering::SeqCst)
                        && !is_key_down(HOTKEY_VK.load(Ordering::Relaxed) as u16)
                    {
                        KEY_DOWN.store(false, Ordering::SeqCst);
                        RECORDING.store(false, Ordering::SeqCst);
                        // 顺手发一个 Release：正在录的会话能正常收尾（不会录到 30 分钟上限）
                        if let Some(tx) = HOOK_TX.get() {
                            let _ = tx.send((HotkeyEvent::Release, ARMED_SEQ.load(Ordering::SeqCst)));
                        }
                        tracing::warn!("热键 KEYUP 丢失，已自愈（按键状态复位；之前几次按下可能被吞）");
                    }
                    // ② 被修饰键挡下的上报（Ctrl/Alt/Win/Shift 卡住时按 F9 会静默无反应）
                    let n = IGNORED_MISMATCH.swap(0, Ordering::SeqCst);
                    if n > 0 {
                        let m = LAST_IGNORED_MODS.load(Ordering::Relaxed);
                        tracing::warn!(
                            "热键被修饰键挡下 {n} 次（最近 mods=0x{m:02X}；0x01=Alt 0x02=Ctrl 0x04=Shift 0x08=Win，可能是按键卡住）"
                        );
                    }
                    continue;
                }
                if msg.message == WM_HEARTBEAT {
                    // ★ 先装新的、再摘旧的（2026-10-07）：Windows 允许低级钩子链式共存
                    //（我们一律 CallNextHookEx），所以“重叠”安全；反过来先摘后装会留一个
                    // **没有钩子的窗口**，落在窗口里的 KEYUP 会被丢掉 → KEY_DOWN 卡住
                    // → 之后所有按下被静默吞。
                    match install() {
                        Ok(h) => {
                            let old = cur;
                            cur = h;
                            if old != 0 {
                                unsafe {
                                    let _ = UnhookWindowsHookEx(HHOOK(old as *mut _));
                                }
                            }
                            tracing::debug!("热键钩子已重装（先装后摘，无间隙）");
                        }
                        Err(e) => tracing::error!("热键钩子重装失败: {e:#}"),
                    }
                    continue;
                }
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }

            unsafe {
                let _ = UnhookWindowsHookEx(HHOOK(cur as *mut _));
            }
            tracing::info!("热键钩子线程退出");
        })
        .context("创建热键线程失败")?;

    let thread_id = match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(tid)) => tid,
        Ok(Err(e)) => {
            stop.store(true, Ordering::SeqCst);
            anyhow::bail!("{e}");
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            if let Ok(tid) = tid_rx.try_recv() {
                unsafe {
                    let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
                }
            }
            anyhow::bail!("热键钩子安装超时");
        }
    };
    tracing::info!("热键钩子已安装（vk=0x{vk:02X} mods=0x{mods:02X}，thread={thread_id}）");
    Ok(HookHandle {
        thread_id,
        stop,
        join: Some(join),
    })
}
