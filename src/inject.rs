//! 文本注入：剪贴板 + 模拟 Ctrl+V，注入后恢复原剪贴板
//!
//! 注意：arboard 写入的剪贴板内容由**本进程托管**，粘贴完成前不能退出，
//! 因此 `Clipboard` 实例要活到恢复剪贴板之后。

use crate::config::Inject as InjectCfg;
use anyhow::{Context, Result};
use arboard::Clipboard;
use std::time::Duration;

/// 虚拟键码（SendInput 用）
const VK_CONTROL: u16 = 0x11;
const VK_V: u16 = 0x56;
const VK_Z: u16 = 0x5A;

pub struct Injector {
    restore: bool,
    paste_delay: Duration,
    restore_delay: Duration,
    raw_undo: bool,
}

impl Injector {
    pub fn new(cfg: &InjectCfg) -> Self {
        Self {
            restore: cfg.restore_clipboard,
            paste_delay: Duration::from_millis(cfg.paste_delay_ms),
            restore_delay: Duration::from_millis(cfg.restore_delay_ms),
            raw_undo: cfg.raw_replace_undo,
        }
    }

    fn write_clipboard(cb: &mut Clipboard, text: &str) -> Result<()> {
        // 剪贴板可能被其他进程短暂占用，重试几次
        let mut last: Option<arboard::Error> = None;
        for attempt in 0..6 {
            match cb.set_text(text.to_string()) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last = Some(e);
                    if attempt < 5 {
                        std::thread::sleep(Duration::from_millis(80));
                    }
                }
            }
        }
        Err(anyhow::anyhow!(
            "写入剪贴板失败: {}",
            last.map(|e| e.to_string()).unwrap_or_default()
        ))
    }

    /// 注入文本：存旧剪贴板 → 写新文本 → Ctrl+V → 恢复旧剪贴板
    pub fn inject(&self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let mut cb = Clipboard::new().context("打开剪贴板失败")?;
        let old = if self.restore {
            cb.get_text().ok().filter(|s| !s.is_empty())
        } else {
            None
        };

        Self::write_clipboard(&mut cb, text)?;
        std::thread::sleep(self.paste_delay);
        // 先记住结果：即使 Ctrl+V 失败，也必须走完剪贴板恢复再返回错误
        let paste = send_combo(VK_CONTROL, VK_V);

        if let Some(old) = old {
            std::thread::sleep(self.restore_delay);
            if let Err(e) = Self::write_clipboard(&mut cb, &old) {
                tracing::warn!("恢复剪贴板失败（不影响已注入文本）: {e:#}");
            }
        }
        paste.context("发送 Ctrl+V 失败")?;
        // 注：“已注入 N 字”只由 output_loop 记一条，避免日志里每次出现两行
        Ok(())
    }

    /// 取原文：Ctrl+Z 撤销上一次粘贴，再粘贴 ASR 原文
    pub fn replace_with_raw(&self, raw: &str) -> Result<()> {
        if raw.is_empty() {
            return Ok(());
        }
        if self.raw_undo {
            send_combo(VK_CONTROL, VK_Z).context("发送 Ctrl+Z 失败")?;
            std::thread::sleep(Duration::from_millis(150));
        }
        self.inject(raw)
    }
}

/// 前台窗口标题（注入前记录目标，链路状态卡片显示「上次注入到哪个窗口」）
///
/// 空串 = 拿不到（无前台窗口 / 无标题）。
#[cfg(windows)]
pub fn foreground_window_title() -> String {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW};
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return String::new();
    }
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

#[cfg(not(windows))]
pub fn foreground_window_title() -> String {
    String::new()
}

/// 发送「按住 modifier → 点按 key」组合键。
/// 用系统 SendInput 代替 enigo：enigo 依赖 windows 0.61，会与 eframe/wgpu 需要的
/// windows 0.62 在依赖图里并存，导致 windows-core 多版本、wgpu-hal 编译失败。
#[cfg(windows)]
fn send_combo(modifier_vk: u16, key_vk: u16) -> Result<()> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    };

    fn ev(vk: u16, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    let inputs = [
        ev(modifier_vk, false),
        ev(key_vk, false),
        ev(key_vk, true),
        ev(modifier_vk, true),
    ];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        anyhow::bail!("SendInput 只发送了 {sent}/{} 个事件", inputs.len());
    }
    Ok(())
}

#[cfg(not(windows))]
fn send_combo(_modifier_vk: u16, _key_vk: u16) -> Result<()> {
    anyhow::bail!("文本注入仅支持 Windows")
}
