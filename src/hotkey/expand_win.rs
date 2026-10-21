//! 全局字符扩展的 Windows 低级钩子（`expand.rs` 的运行期一半）
//!
//! 只做三件事：收键 → 维护缓冲 → 命中后把「退格数 + 文本」丢给 worker。
//! 钩子回调里不加锁、不打日志、不阻塞（Windows 超时会摘钩）。

use anyhow::{Context, Result};
use std::cell::RefCell;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, PostThreadMessageW,
    SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL,
    WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN,
};

use super::expand::{ime_active, plan, step_for, ExpandReq, Shared, Step};

static SHARED: std::sync::OnceLock<Arc<Shared>> = std::sync::OnceLock::new();
static TX: std::sync::OnceLock<Sender<ExpandReq>> = std::sync::OnceLock::new();

const HEARTBEAT_FRESH: Duration = Duration::from_millis(1200);

thread_local! {
    static BUF: RefCell<super::expand::Buffer> = RefCell::new(super::expand::Buffer::default());
}

pub struct ExpandHandle {
    hook: isize,
    tid: u32,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ExpandHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = PostThreadMessageW(self.tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        if self.hook != 0 {
            unsafe {
                let _ = UnhookWindowsHookEx(HHOOK(self.hook as *mut _));
            }
        }
    }
}

unsafe fn our_process_foreground() -> bool {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return false;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid == unsafe { GetCurrentProcessId() }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam.0 as u32 == WM_KEYDOWN || (code >= 0 && wparam.0 as u32 == WM_SYSKEYDOWN)
    {
        // 只有 KEYDOWN 才处理（自动重复会重复进缓冲，但编码匹配后立刻清空，够用）
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let vk = kb.vkCode as u16;
        let step = step_for(vk);
        if step != Step::Reset || vk >= 0x21 {
            // 忽略纯修饰键（Shift/Ctrl/Alt/Win）触发的 Reset，避免清掉正常输入
            let modifier = matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5);
            if !modifier {
                if let (Some(sh), Some(tx)) = (SHARED.get(), TX.get()) {
                    if sh.enabled.load(Ordering::Relaxed) {
                        // 守卫 1：中文输入状态（lua 心跳新鲜）→ 输入法自己会出候选，别抢
                        if ime_active(&sh.beat.try_read().ok().and_then(|g| g.clone()), HEARTBEAT_FRESH)
                        {
                            sh.skipped_ime.fetch_add(1, Ordering::Relaxed);
                            BUF.with(|b| {
                                if let Ok(mut b) = b.try_borrow_mut() {
                                    b.clear();
                                }
                            });
                        } else if !unsafe { our_process_foreground() } {
                            // 守卫 2：自己的设置窗里不展开（不然编辑框会打架）
                            let mut fire = false;
                            BUF.with(|b| {
                                if let Ok(mut buf) = b.try_borrow_mut() {
                                    match step {
                                        Step::Push(c) => buf.push(c),
                                        Step::Pop => buf.backspace(),
                                        Step::Terminate => {
                                            if let Ok(map) = sh.map.try_read() {
                                                if let Some(req) = plan(&map, buf.as_str()) {
                                                    let _ = tx.send(req);
                                                    fire = true;
                                                }
                                            }
                                            buf.clear();
                                        }
                                        Step::Reset => buf.clear(),
                                    }
                                }
                            });
                            if fire {
                                sh.expanded.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// 装钩子并跑消息循环；顺便每 5 秒重读一次 `phrases.tsv`（改词表不用重启）
pub fn spawn(shared: Arc<Shared>, tx: Sender<ExpandReq>, tsv: std::path::PathBuf) -> Result<ExpandHandle> {
    // 词表刷新线程（钩子里不能读文件，这里做快照）
    {
        let sh = shared.clone();
        std::thread::Builder::new()
            .name("expand-map".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_secs(5));
                let min_len = sh.map.read().map(|m| m.min_len).unwrap_or(3);
                if let Ok(text) = std::fs::read_to_string(&tsv) {
                    let map = super::expand::PhraseMap::from_tsv(&text, min_len);
                    if let Ok(mut g) = sh.map.write() {
                        *g = map;
                    }
                }
            })
            .ok();
    }
    let _ = SHARED.set(shared);
    let _ = TX.set(tx);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<u32, String>>();
    let join = std::thread::Builder::new()
        .name("expand-hook".into())
        .spawn(move || unsafe {
            let tid = windows::Win32::System::Threading::GetCurrentThreadId();
            // 先建消息队列，保证 WM_QUIT 可达
            let mut m = MSG::default();
            let _ = windows::Win32::UI::WindowsAndMessaging::PeekMessageW(
                &mut m,
                None,
                0,
                0,
                windows::Win32::UI::WindowsAndMessaging::PM_NOREMOVE,
            );
            let h = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0)
            {
                Ok(h) => h,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("{e:?}")));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(tid));
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                // 不需要翻译，交给系统
            }
            let _ = UnhookWindowsHookEx(h);
            let _ = HC_ACTION;
        })
        .context("创建 expand-hook 线程失败")?;
    let tid = match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(tid)) => tid,
        Ok(Err(e)) => anyhow::bail!("装全局扩展钩子失败: {e}"),
        Err(_) => anyhow::bail!("装全局扩展钩子超时"),
    };
    // hook 句柄在 drop 时再摘（这里用一个非 0 占位，实际卸载走 WM_QUIT 分支）
    Ok(ExpandHandle {
        hook: 0,
        tid,
        join: Some(join),
    })
}
