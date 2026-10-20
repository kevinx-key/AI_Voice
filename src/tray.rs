//! 托盘：状态图标（自己画圆点，不引入 image crate）+ 菜单

use crate::state::State;
use anyhow::{Context, Result};
use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

pub const ID_SHOW: &str = "general.show";
pub const ID_BACKEND_LOCAL: &str = "backend.local";
pub const ID_BACKEND_CLOUD: &str = "backend.cloud";
// 整理档位菜单 ID 直接用档位值（config::Polish::tier 的取值），
// on_menu 收到后原样透传 set_tier，避免再映射一层
pub const ID_POLISH_OFF: &str = "off";
pub const ID_POLISH_LOCAL_LIGHT: &str = "local.light";
pub const ID_POLISH_LOCAL_POLISH: &str = "local.polish";
pub const ID_POLISH_CLOUD_LIGHT: &str = "cloud.light";
pub const ID_POLISH_CLOUD_POLISH: &str = "cloud.polish";
pub const ID_DOWNLOAD_MODEL: &str = "model.download";
pub const ID_AUTOSTART: &str = "general.autostart";
pub const ID_OPEN_CONFIG: &str = "general.config";
pub const ID_OPEN_LOGS: &str = "general.logs";
pub const ID_QUIT: &str = "general.quit";

pub struct Tray {
    /// 必须持有实例，drop 会移除托盘图标
    pub icon: TrayIcon,
    pub backend_local: CheckMenuItem,
    pub backend_cloud: CheckMenuItem,
    pub polish: [CheckMenuItem; 5],
    pub autostart: CheckMenuItem,
    pub download_model: MenuItem,
}

/// 与 config::Polish::tier() 一一对应
pub const POLISH_TIERS: [&str; 5] = [
    "off",
    "local.light",
    "local.polish",
    "cloud.light",
    "cloud.polish",
];

/// 画一个带软边的实心圆点作为托盘图标
pub fn state_icon(state: State) -> Icon {
    const S: u32 = 32;
    let c = state.rgba();
    let mut buf = vec![0u8; (S * S * 4) as usize];
    let center = (S as f32 - 1.0) / 2.0;
    let radius = S as f32 * 0.34;
    for y in 0..S {
        for x in 0..S {
            let d = ((x as f32 - center).powi(2) + (y as f32 - center).powi(2)).sqrt();
            let alpha = ((radius + 1.0 - d) / 1.6).clamp(0.0, 1.0);
            let i = ((y * S + x) * 4) as usize;
            buf[i] = c[0];
            buf[i + 1] = c[1];
            buf[i + 2] = c[2];
            buf[i + 3] = (c[3] as f32 * alpha) as u8;
        }
    }
    Icon::from_rgba(buf, S, S).expect("生成图标失败")
}

pub struct MenuState {
    pub backend: String,
    pub polish: String,
    pub autostart: bool,
    pub model_ready: bool,
}

pub fn build(ms: &MenuState) -> Result<Tray> {
    let menu = Menu::new();

    // 注意：muda 的 new() 会自动生成数字 ID（"0"、"1"…），
    // 菜单事件按 ID 分发（gui.rs on_menu），必须用 with_id 挂上我们的常量
    let show = MenuItem::with_id(ID_SHOW, "打开设置…", true, None);

    let backend_local = CheckMenuItem::with_id(
        ID_BACKEND_LOCAL,
        "本地 SenseVoice",
        true,
        ms.backend != "cloud",
        None,
    );
    let backend_cloud = CheckMenuItem::with_id(
        ID_BACKEND_CLOUD,
        "云端 ASR（OpenAI 兼容）",
        true,
        ms.backend == "cloud",
        None,
    );
    let backend_menu = Submenu::new("识别后端", true);
    backend_menu.append_items(&[&backend_local, &backend_cloud])?;

    let polish_items = POLISH_TIERS.map(|t| {
        CheckMenuItem::with_id(
            t,
            crate::config::Polish::tier_label(t),
            true,
            ms.polish == t,
            None,
        )
    });
    let polish_menu = Submenu::new("文本整理", true);
    for it in &polish_items {
        polish_menu.append(it)?;
    }

    let download_model = MenuItem::with_id(
        ID_DOWNLOAD_MODEL,
        if ms.model_ready {
            "本地模型：已就绪"
        } else {
            "下载本地模型（230MB）"
        },
        !ms.model_ready,
        None,
    );
    let autostart = CheckMenuItem::with_id(ID_AUTOSTART, "开机自启", true, ms.autostart, None);
    let open_config = MenuItem::with_id(ID_OPEN_CONFIG, "打开 config.toml", true, None);
    let open_logs = MenuItem::with_id(ID_OPEN_LOGS, "打开日志目录", true, None);
    let quit = MenuItem::with_id(ID_QUIT, "退出", true, None);

    menu.append_items(&[
        &show,
        &PredefinedMenuItem::separator(),
        &backend_menu,
        &polish_menu,
        &PredefinedMenuItem::separator(),
        &download_model,
        &autostart,
        &PredefinedMenuItem::separator(),
        &open_config,
        &open_logs,
        &PredefinedMenuItem::separator(),
        &quit,
    ])?;

    let icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("倾听输入法 · 待机")
        .with_icon(state_icon(State::Idle))
        .build()
        .context("创建托盘图标失败")?;

    Ok(Tray {
        icon,
        backend_local,
        backend_cloud,
        polish: polish_items,
        autostart,
        download_model,
    })
}

impl Tray {
    pub fn set_state(&self, state: State) {
        let _ = self.icon.set_icon(Some(state_icon(state)));
        let _ = self
            .icon
            .set_tooltip(Some(format!("倾听输入法 · {}", state.label())));
    }

    pub fn set_backend(&self, backend: &str) {
        self.backend_local.set_checked(backend != "cloud");
        self.backend_cloud.set_checked(backend == "cloud");
    }

    pub fn set_polish(&self, tier: &str) {
        for (i, t) in POLISH_TIERS.iter().enumerate() {
            self.polish[i].set_checked(*t == tier);
        }
    }

    /// 链路状态一行摘要（`chain::Chain::summary()`，2026-10-09）：
    /// 鼠标悬停托盘就能看「哪一环不对」，不用开设置窗。
    /// ⚠️ 与 `set_state` 的 tooltip 互相覆盖 —— 调用方要**先** `set_state` 再 `set_tooltip`。
    pub fn set_tooltip(&self, text: &str) {
        let _ = self.icon.set_tooltip(Some(text));
    }

    pub fn set_autostart(&self, on: bool) {
        self.autostart.set_checked(on);
    }

    pub fn set_model_ready(&self, ready: bool) {
        self.download_model.set_enabled(!ready);
        self.download_model.set_text(if ready {
            "本地模型：已就绪"
        } else {
            "下载本地模型（230MB）"
        });
    }
}
