//! 全局热键抽象：修饰键精确匹配（按下 / 松开 / 取原文）
//!
//! 语法：`[修饰+]*键`，修饰 = win（别名 meta/super）/ ctrl / alt / shift。
//! - 录音：修饰状态**恰好**等于配置写的集合（写了的按住、没写的没按）
//! - 取原文：恰好等于「主键修饰集 + Shift」
//! - 字母键必须带修饰键；主键不能是 Shift（取原文冲突）

use anyhow::{bail, Result};
use std::sync::mpsc::Sender;

#[cfg(windows)]
pub mod windows;

/// 逻辑修饰键位掩码（左右任一算按住）
pub const MOD_SHIFT: u8 = 1 << 0;
pub const MOD_CTRL: u8 = 1 << 1;
pub const MOD_ALT: u8 = 1 << 2;
pub const MOD_WIN: u8 = 1 << 3;

/// 钩子统计（累计值）：链路状态卡片判断“钩子是否被静默摘除 / 发生过丢键”
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HookStats {
    /// 累计回调次数（钩子被系统调用的总次数）
    pub calls: u64,
    /// 累计命中目标主键次数
    pub vk_calls: u64,
    /// 累计「回调内自愈 KEYUP 丢失」次数（>0 = 发生过丢键）
    pub selfheal: u64,
}

#[cfg(windows)]
pub use windows::stats;

#[cfg(not(windows))]
pub fn stats() -> HookStats {
    HookStats::default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// 主键按下（修饰恰好 = 主键修饰集）→ 开始录音
    Press,
    /// 主键松开 → 结束录音
    Release,
    /// 取原文（修饰恰好 = 主键修饰集 + Shift）按下
    Raw,
}

/// 热键规格：主键虚拟码 + 要求的修饰键（不含 Shift）+ 展示名
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeySpec {
    pub vk: u16,
    pub mods: u8,
    pub name: String,
}

/// 主键自身是修饰键时返回其逻辑位（精确匹配时两侧都要排除它）
pub fn key_modifier_bit(vk: u16) -> u8 {
    match vk {
        0xA0 | 0xA1 => MOD_SHIFT,
        0xA2 | 0xA3 => MOD_CTRL,
        0xA4 | 0xA5 => MOD_ALT,
        0x5B | 0x5C => MOD_WIN,
        _ => 0,
    }
}

/// 修饰键精确匹配判定（纯函数，便于单测）
///
/// - `spec_mods`：配置要求的修饰集（parse 已保证不含 Shift）
/// - `state`：主键按下瞬间的逻辑修饰状态
///
/// 主键自身若为修饰键（如 right_alt），其对应位在两侧都排除。
pub fn classify(vk: u16, spec_mods: u8, state: u8) -> Option<HotkeyEvent> {
    let own = key_modifier_bit(vk);
    let expect = spec_mods & !own;
    let state = state & !own;
    if state == expect {
        Some(HotkeyEvent::Press)
    } else if state == expect | MOD_SHIFT {
        Some(HotkeyEvent::Raw)
    } else {
        None
    }
}

/// 解析主键（不含修饰前缀）："f9" / "h" / "right_alt" 等
fn parse_vk(name: &str) -> Result<u16> {
    let n = name.trim().to_ascii_lowercase();

    if let Some(rest) = n.strip_prefix('f') {
        if let Ok(idx) = rest.parse::<u16>() {
            if (1..=24).contains(&idx) {
                return Ok(0x70 + idx - 1);
            }
        }
    }
    if n.len() == 1 && n.as_bytes()[0].is_ascii_lowercase() {
        return Ok(n.as_bytes()[0].to_ascii_uppercase() as u16);
    }

    let vk = match n.as_str() {
        "right_alt" | "ralt" | "rightalt" => 0xA5,
        "left_alt" | "lalt" => 0xA4,
        "right_ctrl" | "rctrl" => 0xA3,
        "left_ctrl" | "lctrl" => 0xA2,
        "right_shift" | "rshift" => 0xA1,
        "left_shift" | "lshift" => 0xA0,
        "caps_lock" | "capslock" | "caps" => 0x14,
        "scroll_lock" | "scrolllock" => 0x91,
        "pause" => 0x13,
        "insert" | "ins" => 0x2D,
        "delete" | "del" => 0x2E,
        "home" => 0x24,
        "end" => 0x23,
        "page_up" => 0x21,
        "page_down" => 0x22,
        "tab" => 0x09,
        "backtick" | "grave" => 0xC0,
        "space" => 0x20,
        _ => bail!(
            "不支持的热键名: {name}（支持 f1-f24 / 字母 a-z（须带修饰键）/ \
             right_alt / left_alt / caps_lock / scroll_lock / pause / insert 等）"
        ),
    };
    Ok(vk)
}

/// 解析 "win+h" / "f9" / "ctrl+alt+f8" / "right_alt" 等
pub fn parse(spec: &str) -> Result<HotkeySpec> {
    let raw = spec.trim();
    let mut base = raw.to_ascii_lowercase();
    let mut mods = 0u8;

    loop {
        let mut hit: Option<(u8, String)> = None;
        for (prefix, bit) in [
            ("win+", MOD_WIN),
            ("meta+", MOD_WIN),
            ("super+", MOD_WIN),
            ("ctrl+", MOD_CTRL),
            ("control+", MOD_CTRL),
            ("alt+", MOD_ALT),
            ("shift+", MOD_SHIFT),
        ] {
            if let Some(rest) = base.strip_prefix(prefix) {
                hit = Some((bit, rest.to_string()));
                break;
            }
        }
        match hit {
            Some((bit, rest)) => {
                if mods & bit != 0 {
                    bail!("修饰键重复: {raw}");
                }
                mods |= bit;
                base = rest;
            }
            None => break,
        }
    }

    if base.is_empty() {
        bail!("缺少主键: {raw}");
    }
    if mods & MOD_SHIFT != 0 {
        bail!("主热键不能含 Shift（Shift 保留给取原文）: {raw}");
    }
    let vk = parse_vk(&base)?;
    if key_modifier_bit(vk) & MOD_SHIFT != 0 {
        bail!("Shift 键不能作主键（与取原文冲突）: {raw}");
    }
    if (0x41..=0x5A).contains(&vk) && mods == 0 {
        bail!("字母键必须带修饰键（如 win+h），否则打字会误触发: {raw}");
    }

    Ok(HotkeySpec {
        vk,
        mods,
        name: raw.to_string(),
    })
}

#[cfg(windows)]
pub fn spawn(spec: &HotkeySpec, tx: Sender<(HotkeyEvent, u64)>) -> Result<windows::HookHandle> {
    windows::spawn(spec.vk, spec.mods, tx)
}

/// 运行时切换热键（无需重装钩子）
#[cfg(windows)]
pub fn set_hotkey(vk: u16, mods: u8) {
    windows::set_vk(vk, mods)
}

/// 物理键是否按下（会话看门狗：KEYUP 丢失时的兜底收尾）
#[cfg(windows)]
pub fn is_key_down(vk: u16) -> bool {
    windows::is_key_down(vk)
}

#[cfg(not(windows))]
pub fn is_key_down(_vk: u16) -> bool {
    // 非 Windows 暂无热键实现（二期）：返回 true 表示“不触发看门狗”
    true
}

#[cfg(not(windows))]
pub fn set_hotkey(_vk: u16, _mods: u8) {}

#[cfg(not(windows))]
pub fn spawn(_spec: &HotkeySpec, _tx: Sender<(HotkeyEvent, u64)>) -> Result<()> {
    bail!("当前平台的热键实现尚未提供（二期）")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        let s = parse("f9").unwrap();
        assert_eq!((s.vk, s.mods), (0x78, 0));
        let s = parse("right_alt").unwrap();
        assert_eq!((s.vk, s.mods), (0xA5, 0));
        let s = parse("win+h").unwrap();
        assert_eq!((s.vk, s.mods), (0x48, MOD_WIN));
        let s = parse("Meta+H").unwrap();
        assert_eq!((s.vk, s.mods), (0x48, MOD_WIN));
        let s = parse("super+h").unwrap();
        assert_eq!((s.vk, s.mods), (0x48, MOD_WIN));
        let s = parse("win+f9").unwrap();
        assert_eq!((s.vk, s.mods), (0x78, MOD_WIN));
        let s = parse("ctrl+alt+f8").unwrap();
        assert_eq!((s.vk, s.mods), (0x77, MOD_CTRL | MOD_ALT));
        let s = parse("ctrl+right_alt").unwrap();
        assert_eq!((s.vk, s.mods), (0xA5, MOD_CTRL));
    }

    #[test]
    fn parse_rejects() {
        for bad in [
            "h",
            "win+",
            "win+shift+h",
            "shift+f9",
            "left_shift",
            "ctrl+ctrl+f9",
            "win+q1",
            "f25",
            "小狼毫",
        ] {
            assert!(parse(bad).is_err(), "{bad} 应被拒绝");
        }
    }

    #[test]
    fn classify_exact() {
        // win+h：精确匹配
        assert_eq!(classify(0x48, MOD_WIN, MOD_WIN), Some(HotkeyEvent::Press));
        assert_eq!(
            classify(0x48, MOD_WIN, MOD_WIN | MOD_SHIFT),
            Some(HotkeyEvent::Raw)
        );
        assert_eq!(classify(0x48, MOD_WIN, MOD_CTRL | MOD_WIN), None);
        assert_eq!(classify(0x48, MOD_WIN, 0), None);

        // f9 回归：无修饰录音，Shift 取原文，Ctrl 不触发
        assert_eq!(classify(0x78, 0, 0), Some(HotkeyEvent::Press));
        assert_eq!(classify(0x78, 0, MOD_SHIFT), Some(HotkeyEvent::Raw));
        assert_eq!(classify(0x78, 0, MOD_CTRL), None);
        assert_eq!(classify(0x78, 0, MOD_SHIFT | MOD_CTRL), None);

        // 主键自身是修饰键（right_alt）：own 位两侧排除
        assert_eq!(classify(0xA5, 0, MOD_ALT), Some(HotkeyEvent::Press));
        assert_eq!(
            classify(0xA5, 0, MOD_ALT | MOD_SHIFT),
            Some(HotkeyEvent::Raw)
        );
        assert_eq!(classify(0xA5, 0, MOD_ALT | MOD_CTRL), None);

        // ctrl+right_alt
        assert_eq!(
            classify(0xA5, MOD_CTRL, MOD_CTRL | MOD_ALT),
            Some(HotkeyEvent::Press)
        );
    }
}
