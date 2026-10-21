//! 倾听输入法（Rust 版）：按住热键说话 → 出字
//!
//! 主线程归 eframe（设置界面 + 托盘）；热键钩子与录音/识别工作线程各自独立。
//! 长语音走"流式采集 → 分段识别 → 定时注入"，不再等待全部说完。

#![cfg_attr(windows, windows_subsystem = "windows")]

mod asr;
mod audio;
mod bench;
mod chain;
mod config;
mod ctl;
mod gui;
mod hotkey;
mod hotwords;
mod gate;
mod inject;
mod llm_server;
mod learn;
mod models;
mod polish;
mod rime;
mod segmenter;
mod selftest;
mod state;
mod terms;
mod text;
mod tray;

use anyhow::{Context, Result};
use chain::{Chain, ChainState};
use clap::Parser;
use config::{Config, Paths};
use hotkey::{HotkeyEvent, HotkeySpec};
use llm_server::{LlamaServer, LlmState};
use polish::{NoopPolisher, Polisher};
use state::State;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 热键钩子槽（挂起摘、恢复/引擎开重装；托管模式与 ctl 线程共用）
#[cfg(windows)]
type HookSlot = Arc<std::sync::Mutex<Option<hotkey::windows::HookHandle>>>;
#[cfg(not(windows))]
type HookSlot = Arc<std::sync::Mutex<Option<()>>>;

/// 按下瞬间往前多取这么多毫秒音频（pre-roll），补回「按下 → 开录」的延迟
///
/// 实测按下→会话开始的空洞 = worker 命令轮询（20ms）+ 开流/轮询开销，约 100~150ms；
/// WASAPI 不会补发按下之前的声音，只能靠预热流 + 环形缓冲里这一段拿回来。
/// 取 350ms 而不是更大：pre-roll 也会把“按下之前那半句的尾巴”带进来，
/// 太大会让注入文本多出前一句的尾巴（实测 600ms 时容易凑出碎片段→幻觉）。
const PREROLL_MS: u64 = 350;
/// 松手后的收尾宽限（普通按住）：收尾音，用户常说边说边松手
const RELEASE_GRACE_HOLD_MS: u64 = 500;
/// 松手后的收尾宽限（短按/点按）：用户习惯“点一下再说话”，
/// hold 模式下点完就松手 → 之后说的话全丢（装机实测：频繁短按 → 用户体感“无法唤起录音中状态”）。
/// 短按后继续听这么久，把“点完才开口”的话也收进来（期间静音会被门控丢弃，不会多出字）。
const RELEASE_GRACE_TAP_MS: u64 = 1500;
/// 按住短于这个时长（按下时刻→松手）就算“点按”
const TAP_MAX_MS: u64 = 350;

#[derive(Parser, Debug)]
#[command(
    name = "listen",
    version,
    about = "按住热键说话 → 出字（本地/云端 ASR 可选，不吞键）"
)]
struct Cli {
    /// 指定配置文件（默认 exe 同级 config.toml）
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// 门控/分段回归跑分：对目录内 wav 跑**与真实会话同一套判据**，输出 TSV
    #[arg(long, value_name = "DIR")]
    gate: Option<PathBuf>,
    /// 录一段麦克风音频存成 16k 单声道 wav 后退出（噪声回归集采样用；按住热键录、松手停）
    ///
    /// 为什么加这个：本机没有 Audacity/ffmpeg/sox，Win10 自带「录音机」只能存 m4a；
    /// 而噪声回归集必须在**真实麦克风 + 真实房间**里采，所以直接复用正式会话的采集链路。
    #[arg(long, value_name = "WAV")]
    record: Option<PathBuf>,
    /// 转写单个 wav 后退出（回归用）
    #[arg(long, value_name = "WAV")]
    once: Option<PathBuf>,
    /// 跑音频回归集后退出（目录内 *.wav，可选 refs.tsv）
    #[arg(long, value_name = "DIR")]
    selftest: Option<PathBuf>,
    /// 中英混说横评：同一批 wav 依次跑多个本地模型（目录内 *.wav + ref.tsv）
    #[arg(long, value_name = "DIR")]
    bench: Option<PathBuf>,
    /// 不创建托盘图标（调试用）
    #[arg(long)]
    no_tray: bool,
    /// 受管模式：不创建托盘，启动 ListenCtl 控制管道（由倾听输入法主程序拉起）
    #[arg(long)]
    managed: bool,
    /// 下载本地模型后退出
    #[arg(long)]
    download_model: bool,
    /// 设置开机自启后退出（true/false）
    #[arg(long, value_name = "BOOL")]
    autostart: Option<bool>,
    /// 设置语音引擎总开关后退出（on/off；托盘菜单在 listen 未运行时调用）
    #[arg(long, value_name = "ON|OFF")]
    voice: Option<String>,
    /// 列出录音设备后退出
    #[arg(long)]
    devices: bool,
    /// 模拟长语音分段识别（不做注入），验证分段与增量输出
    #[arg(long, value_name = "WAV")]
    simulate: Option<PathBuf>,
    /// 注入一段文本后退出（手动 E2E：先聚焦目标窗口再运行）
    #[arg(long, value_name = "TEXT")]
    inject: Option<String>,
    /// 单条文本走完整整理链路（不用麦克风），打印启动/整理耗时
    #[arg(long, value_name = "TEXT")]
    polish: Option<String>,
    /// 首启向导：打开设置界面并停在「输入法」页
    #[arg(long)]
    first_run: bool,
    /// 部署 Rime 输入法（检测/铺方案包/写配置/编译词库）后退出
    #[arg(long)]
    rime_deploy: bool,
    /// 倾听输入法未安装时允许自动静默安装（配合 --rime-deploy）
    #[arg(long)]
    rime_install: bool,
    /// 保守卸载我们铺的 Rime 文件（保留用户词库/配置）后退出
    #[arg(long)]
    rime_uninstall: bool,
    /// 检测 Rime 环境后退出（打印结果，不改动任何东西）
    #[arg(long)]
    rime_status: bool,
    /// 打开设置窗口（若已有实例在运行，则唤起它的设置窗）
    #[arg(long)]
    settings: bool,
    /// 打印全链路状态后退出（listen 在跑时走 ListenCtl `CHAIN`，没跑则静态探测）
    #[arg(long)]
    status: bool,
    /// 配合 --status：输出 JSON（给脚本用）
    #[arg(long)]
    json: bool,
    /// 只跑一遍术语纠错（不录音、不联网）：排查「美国客户→美Docker户」这类误伤
    ///
    /// 输出 `输入 / 输出` 两行，和真实会话走**同一个** `TermCorrector`（不是另写一份），
    /// 所以它能当回归用：`listen.exe --correct "美国客户"` 必须原样返回。
    #[arg(long, value_name = "TEXT")]
    correct: Option<String>,
    /// 【内部】崩溃自启用：拿不到单实例锁时最多等这么多毫秒（等旧进程退出后接手）
    ///
    /// 平时是 0（已有实例就立刻退出，保持原语义）。只有 `install_crash_guard` 拉起的
    /// 新进程带这个参数，避免「旧进程还没退干净 → 新进程撞单实例锁 → 又死」。
    #[arg(long, value_name = "MS", default_value_t = 0, hide = true)]
    retry_lock: u64,
}

/// 主线程 → 工作线程
#[derive(Debug)]
enum Cmd {
    /// 主键按下（携带热键事件序号，用于 Press/Release 配对）
    Press(u64),
    /// 主键松开（携带序号）
    Release(u64),
    Raw,
    ReloadConfig,
    /// 热词表文件刚改过：只重建整理/识别后端（拿新热词；配置本身没变）
    ReloadHotwords,
    /// 手动拉起本地大模型
    LlmStart,
    /// 手动停止本地大模型（只停自己拉起的）
    LlmStop,
    /// 托管：取消当前会话并释放识别模型（挂起）
    Suspend,
    /// 托管：异步重载识别模型（恢复）
    Resume,
    /// 一键修复（链路状态卡按钮）：重装钩子 + 重开麦 + 重新加载模型/整理后端
    Repair,
    Quit,
}

/// 工作线程 → 主线程
#[derive(Debug)]
enum Evt {
    State(State),
    Note(String),
    Config(Box<Config>),
    LlmState(LlmState),
    /// ASR 原文段（悬浮窗调试版展示用）
    Segment { text: String },
    /// 整理批次文本（悬浮窗调试版展示用；整理关闭时不发）
    Polished { text: String },
    /// 「本次没出字」的原因（录音太短 / 整段过轻 / 后端未就绪）：
    /// 悬浮窗与运行日志都看得见，修“按了没反应却不知道为啥”。
    Hint(String),
    /// 门控实时读数（D3，悬浮窗第一行）：当前是否判为语音 + 本块电平（RMS）。
    /// 发送侧已节流（≤4 次/秒，语音状态变化时立即发），不会刷爆 GUI。
    Gate { speech: bool, rms: f32 },
    /// 全链路状态快照（2026-10-09，链路状态卡片/托盘 tooltip）：
    /// worker 每秒构建一次，**内容变化才发**，另每 5s 发一次心跳兼底
    Chain(Box<Chain>),
}

/// GUI 的 wgpu 配置：集显优先 + 不等垂直同步（2026-10-08，见《项目调试过程.md》§19）
///
/// 背景：`WARN GUI 帧间隔 >300ms`（最高 3361ms）**专项复现不出来** ——
/// 用 `tools\gui_stall_bench.py` 压 llama-server(ngl=99) 持续推理、反复显示/隐藏窗口，
/// 全部 0 次定格；日志里还有 2 次定格完全无任何应用侧事件对应。剩下的解释只有
/// 「整机 GPU/DWM 被别的程序占住」。争用既然来自外部，唯一稳的办法是**本程序的 UI
/// 不走独显**（顺带也不再跟跑大模型的那块卡抢）。
///
/// - `auto`（默认）：优先集显，没有再回退独显（**不主动用 WARP**，理由见下）
/// - `gpu`：强制独显（旧行为，出问题时用于对照）
/// - `software`：强制软件光栅化（D3D12 WARP）—— 只给明确知情的人用
///
/// ⚠️ **为什么 auto 不选 WARP**：实测量过（2026-10-08，本机 RTX 5070 Ti + `Microsoft Basic
/// Render Driver`），同一份配置下 30s 内进程 CPU 时间：
/// | 模式 | 窗口隐藏 | 窗口显示 |
/// |---|---|---|
/// | 独显 | 0.69s（≈2.3% 单核） | 1.11s（≈3.7% 单核） |
/// | WARP | **95.1s（≈317% 单核）** | **483.2s（≈1610% 单核）** |
/// 即 WARP 单帧要花掉约 1 CPU 秒（窗口可见时把 16 个核全吃满）——一个后台输入法不能这么烧，
/// 会跟 ASR/大模型抢 CPU。所以 WARP 只在 `gui_render = "software"` 时启用。
///
/// 另外不等垂直同步（`AutoNoVsync`）：Fifo 在隐藏/被遮挡窗口上会让 Present 等合成器。
fn gui_wgpu_options(cfg: &Config) -> eframe::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuSetup, WgpuSetupCreateNew};
    use eframe::wgpu;

    let mode = cfg.general.gui_render.trim().to_ascii_lowercase();
    let mut conf = eframe::WgpuConfiguration::default();
    conf.surface.present_mode = wgpu::PresentMode::AutoNoVsync;
    conf.surface.desired_maximum_frame_latency = Some(1);
    if mode == "gpu" {
        tracing::info!("GUI 渲染后端: gpu（强制独显，present=no-vsync）");
        return conf;
    }
    let want_soft = mode == "software";
    // auto：集显 → 独显（不碰 WARP）；software：先软件，拿不到才退集显/独显
    let order: [wgpu::DeviceType; 3] = if want_soft {
        [
            wgpu::DeviceType::Cpu,
            wgpu::DeviceType::IntegratedGpu,
            wgpu::DeviceType::DiscreteGpu,
        ]
    } else {
        [
            wgpu::DeviceType::IntegratedGpu,
            wgpu::DeviceType::DiscreteGpu,
            wgpu::DeviceType::DiscreteGpu,
        ]
    };
    conf.wgpu_setup = WgpuSetup::CreateNew(WgpuSetupCreateNew {
        native_adapter_selector: Some(Arc::new(
            move |adapters: &[wgpu::Adapter], _surface| {
                let mut chosen = None;
                for t in order {
                    if let Some(a) = adapters.iter().find(|a| a.get_info().device_type == t) {
                        chosen = Some(a.clone());
                        break;
                    }
                }
                // 绝不能返回 Err：egui-wgpu 把选择失败当**致命错误**（会把整个 GUI 启动搞挂），
                // 所以最后一级无条件退回枚举到的第一个适配器。
                chosen
                    .or_else(|| adapters.first().cloned())
                    .ok_or_else(|| "wgpu 没有枚举到任何适配器".to_string())
            },
        )),
        ..WgpuSetupCreateNew::without_display_handle()
    });
    if want_soft {
        tracing::warn!(
            "GUI 渲染后端: 软件光栅（WARP）—— 界面不再受整机 GPU 拖累，但 CPU 代价实测可达 3~16 个核，慎用"
        );
    } else {
        tracing::info!("GUI 渲染后端: {mode}（集显优先，本机无集显则用独显；gui_render 可改）");
    }
    conf
}

fn main() -> Result<()> {
    attach_parent_console();

    let cli = Cli::parse();
    let paths = match &cli.config {
        Some(p) => Paths::from_config_file(p)?,
        None => Paths::default_from_exe()?,
    };
    let cfg = Config::load_or_create(&paths)?;

    init_logging(&paths, &cfg.general.log_level, cli.once.is_some() || cli.selftest.is_some() || cli.bench.is_some());
    tracing::info!(
        "倾听输入法 v{} 启动（root={}）",
        env!("CARGO_PKG_VERSION"),
        paths.root.display()
    );

    // ---- 无界面的一次性命令 ----
    if cli.rime_deploy {
        return run_rime_deploy(&paths, &cfg, cli.rime_install);
    }
    // 热词包：首次运行把内置包铺到 exe 同级 hotwords\（GUI 与 ASR 都读它；已存在不动）
    for l in hotwords::ensure_user_packs(&cfg, &paths) {
        tracing::info!("{}", l.trim());
    }
    // 启动时把「打字热词表」与当前配置同步一次：手改 `hotwords\*.tsv` 后重启也能生效
    // （旧实现只在「学习到 N 个新词」时才刷，手改的表要等很久）
    match hotwords::apply_to_rime(&cfg, &paths) {
        Ok(Some(p)) => tracing::info!("打字热词表已同步: {p}"),
        Ok(None) => {}
        Err(e) => tracing::warn!("打字热词表同步失败: {e:#}"),
    }
    if let Some(wav) = &cli.once {
        return run_once(&paths, &cfg, wav);
    }
    if let Some(dir) = &cli.selftest {
        let code = selftest::run(&paths, &cfg, dir)?;
        std::process::exit(code);
    }
    if let Some(dir) = &cli.bench {
        let code = bench::run(&paths, &cfg, dir)?;
        std::process::exit(code);
    }
    if cli.devices {
        return list_devices();
    }
    if let Some(wav) = &cli.simulate {
        return run_simulate(&paths, &cfg, wav);
    }
    if let Some(text) = &cli.inject {
        return run_inject_test(&paths, &cfg, text);
    }
    if let Some(text) = &cli.polish {
        return run_polish_cli(&paths, &cfg, text);
    }
    if let Some(wav) = &cli.record {
        return run_record(&paths, &cfg, wav);
    }
    if let Some(dir) = &cli.gate {
        return run_gate(&paths, &cfg, dir);
    }
    if cli.rime_status {
        return run_rime_status(&paths, &cfg);
    }
    if cli.status {
        return run_status(&paths, &cfg, cli.json);
    }
    if let Some(text) = &cli.correct {
        // 和会话里用的是同一个 corrector（`process_segment` 里那一句）
        let c = terms::TermCorrector::new(&cfg.glossary);
        println!("输入: {text}");
        println!("输出: {}", c.correct(text));
        return Ok(());
    }
    if cli.rime_uninstall {
        return run_rime_uninstall(&paths, &cfg);
    }
    if cli.download_model {
        let dir = paths.resolve(&cfg.asr.local_model);
        let log = |m: String| println!("{m}");
        models::ensure_model(&dir, &log)?;
        return Ok(());
    }
    if let Some(on) = cli.autostart {
        set_autostart(on)?;
        let mut c = cfg.clone();
        c.general.autostart = on;
        c.save(&paths)?;
        println!("开机自启已{}", if on { "开启" } else { "关闭" });
        return Ok(());
    }
    if let Some(v) = &cli.voice {
        let on = match v.trim().to_ascii_lowercase().as_str() {
            "on" | "true" | "1" => true,
            "off" | "false" | "0" => false,
            _ => anyhow::bail!("--voice 取值应为 on/off（收到 {v}）"),
        };
        let mut c = cfg.clone();
        c.general.voice_enabled = on;
        c.save(&paths)?;
        sync_voice_flag(&paths, on);
        println!("语音输入已{}", if on { "启用" } else { "关闭" });
        return Ok(());
    }

    // ---- 常驻模式 ----
    // 唤起事件：先建好命名事件，之后其他进程（例如倾听输入法托盘菜单跑
    // `listen.exe --settings`）可以通过它请求把设置窗口显示到前台。
    let show_event = create_show_event();

    // 崩溃自愈（2026-10-20）：见 `install_crash_guard` 的注释。
    // 只装在常驻模式：`--status` / `--selftest` 这类一次性命令 panic 不该顺带拉起常驻进程。
    install_crash_guard(&paths, cli.config.clone(), cli.managed);

    // 上一次是不是崩过？→ 告诉用户（悬浮窗 + 日志页），然后把这现场留档改名
    let crash_note = take_crash_note(&paths);

    let _guard = match acquire_single_instance(Duration::from_millis(cli.retry_lock)) {
        Ok(Some(h)) => Some(h),
        Ok(None) => {
            // 已有实例在跑：只有「显式设置请求」才把设置窗叫到前台；
            // 普通无参启动（自启/双击快捷方式/误触）一律静默退出，不再乱弹设置页。
            if cli.settings {
                signal_show_settings();
                if !cli.managed {
                    println!("倾听输入法 已在运行，正在打开设置窗口");
                }
            }
            tracing::info!(
                "已有实例在运行，本次直接退出（managed={}, settings={}）",
                cli.managed,
                cli.settings
            );
            return Ok(());
        }
        Err(e) => {
            tracing::warn!("单实例互斥体创建失败，继续运行: {e:#}");
            None
        }
    };

    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let (evt_tx, evt_rx) = mpsc::channel::<Evt>();
    // 「上次异常退出」的提示交给界面显示（悬浮窗 + 运行日志页）
    if let Some(note) = crash_note {
        // 用 Note（只进状态栏/日志页）而不是 Hint：Hint 会把**悬浮窗**拉出来，
        // 而这时候用户根本没按键（用户报「没有任何按键时语音悬浮窗自动弹出」，2026-10-20）
        tracing::warn!("{note}");
        let _ = evt_tx.send(Evt::Note(note));
    }

    // 热键线程 → 主线程（事件 + 序号，用于配对丢弃过期 Release）
    let (hk_tx, hk_rx) = mpsc::channel::<(HotkeyEvent, u64)>();
    let spec = hotkey::parse(&cfg.general.hotkey).unwrap_or_else(|e| {
        tracing::warn!("热键配置无效，已回退 f9：{e:#}");
        hotkey::parse("f9").expect("默认热键 f9 必定可解析")
    });
    // 钩子槽：托管模式交给 ListenCtl（挂起摘、恢复重装）；独立模式由 main/worker 持有
    let hook_slot: HookSlot = Arc::new(std::sync::Mutex::new(None));
    if cfg.general.voice_enabled {
        #[cfg(windows)]
        {
            let h = hotkey::spawn(&spec, hk_tx.clone()).context("安装全局热键失败")?;
            *hook_slot.lock().unwrap() = Some(h);
        }
        #[cfg(not(windows))]
        {
            let _ = hotkey::spawn(&spec, hk_tx.clone()).context("安装全局热键失败")?;
        }
        tracing::info!(
            "热键已就绪：{}「{}」说话；取原文 = 主键修饰 + Shift（不吞键）",
            if cfg.general.is_toggle_mode() {
                "按一下"
            } else {
                "按住"
            },
            spec.name
        );
    } else {
        tracing::info!("语音引擎总开关为 OFF：不安装热键、不加载识别模型");
    }

    // 全局「快捷输入」展开（2026-10-21 方案 C 的第二半）：
    // 中文状态由 Rime 候选表出（第一项）；英文 / 直通状态由这里的键盘钩子兜底。
    let mut _expand_hook: Option<hotkey::expand_win::ExpandHandle> = None;
    if cfg.hotwords.enabled && cfg.hotwords.quick_expand {
        let tsv = hotwords::pack_path(&cfg, &paths, hotwords::PHRASES);
        let beat = std::env::var_os("APPDATA")
            .map(|p| std::path::PathBuf::from(p).join("Rime").join("listen_ime_beat.txt"));
        let text = std::fs::read_to_string(&tsv).unwrap_or_default();
        let map = hotkey::expand::PhraseMap::from_tsv(&text, cfg.hotwords.quick_expand_min_len);
        let shared = hotkey::expand::Shared::new(true, beat.clone(), map);
        let (etx, erx) = std::sync::mpsc::channel::<hotkey::expand::ExpandReq>();
        let inj_cfg = cfg.inject.clone();
        std::thread::Builder::new()
            .name("expand-run".into())
            .spawn(move || {
                let inj = inject::Injector::new(&inj_cfg);
                while let Ok((n, text)) = erx.recv() {
                    // 等应用处理完那个结束符再动手，否则退格会打到别处
                    std::thread::sleep(Duration::from_millis(60));
                    if let Err(e) = inject::send_backspaces(n) {
                        tracing::warn!("快捷输入展开失败（退格）: {e:#}");
                        continue;
                    }
                    match inj.inject(&text) {
                        Ok(()) => tracing::info!(
                            "快捷输入展开：退格 {n} 次 → 注入 {} 字",
                            text.chars().count()
                        ),
                        Err(e) => tracing::warn!("快捷输入展开失败（注入）: {e:#}"),
                    }
                }
            })
            .ok();
        // 守卫前置条件：心跳文件由 `listen_learn.lua` 写。lua 要靠**部署**才生效，
        // 没部署时心跳永远不存在 → 无法区分中英状态 → 宁可先不装（否则中文状态会重复出字）。
        let beat_ready = beat.as_ref().map(|p| p.exists()).unwrap_or(false);
        if !beat_ready {
            tracing::warn!(
                "全局快捷输入：等一次「立即部署」（lua 心跳 listen_ime_beat.txt 还没生成）—— 部署后重启 listen 即生效"
            );
        } else {
            match hotkey::expand_win::spawn(shared.clone(), etx, tsv) {
                Ok(h) => {
                    tracing::info!(
                        "全局快捷输入已就绪（{} 条编码，最短 {} 位；中文状态交给输入法本身）",
                        shared.map.read().map(|m| m.len()).unwrap_or(0),
                        cfg.hotwords.quick_expand_min_len
                    );
                    _expand_hook = Some(h);
                }
                Err(e) => tracing::warn!("全局快捷输入未启用: {e:#}"),
            }
        }
    }

    // 热键转发**独立线程**（2026-10-07）：热键事件不再经 GUI 帧循环转发。
    // 原实现把 hk_rx→cmd_tx 放在 eframe 的 App::ui（gui::pump）里，只要 GUI 卡一帧
    // （wgpu 渲染与 llama-server 抢 GPU / 窗口重绘停摆），按键就被一起吞掉 ——
    // 用户看到的就是「按了没反应 + 悬浮窗/托盘状态不变」。
    {
        let relay_tx = cmd_tx.clone();
        std::thread::Builder::new()
            .name("hotkey-relay".into())
            .spawn(move || {
                while let Ok((hk, seq)) = hk_rx.recv() {
                    let since_press = audio::clock_ms().saturating_sub(audio::press_ms());
                    match hk {
                        // 只对 Press 看“钩子→转发”滞后；对 Release 这个差值等于“按住时长”
                        HotkeyEvent::Press => tracing::info!(
                            "热键事件转发 Press seq={seq}（钩子→转发滞后 {since_press}ms）"
                        ),
                        HotkeyEvent::Release => tracing::info!(
                            "热键事件转发 Release seq={seq}（按住 {since_press}ms）"
                        ),
                        HotkeyEvent::Raw => tracing::info!("热键事件转发 Raw（取原文）"),
                    }
                    let cmd = match hk {
                        HotkeyEvent::Press => Cmd::Press(seq),
                        HotkeyEvent::Release => Cmd::Release(seq),
                        HotkeyEvent::Raw => Cmd::Raw,
                    };
                    if relay_tx.send(cmd).is_err() {
                        break;
                    }
                }
            })
            .context("启动热键转发线程失败")?;
    }

    // 引擎状态：设置窗链路卡片 / 托盘 / fork（ListenCtl STATUS）都读它
    let engine_state = ctl::ManagedState::new();

    // 工作线程
    // 全链路快照：worker 每秒写；ctl 的 CHAIN、GUI 的卡片、托盘 tooltip 读同一份
    let chain_state = ChainState::new();

    let worker_paths = paths.clone();
    let worker_cfg = cfg.clone();
    let worker_engine = engine_state.clone();
    let worker_spec = spec.clone();
    let worker_hk_tx = hk_tx.clone();
    let worker_hook = hook_slot.clone();
    let worker_chain = chain_state.clone();
    let worker_managed = cli.managed;
    std::thread::Builder::new()
        .name("worker".into())
        .spawn(move || {
            let mut w = Worker::new(
                worker_paths,
                worker_cfg,
                evt_tx,
                worker_engine,
                worker_spec,
                worker_hk_tx,
                worker_hook,
                worker_chain,
                worker_managed,
            );
            w.run(cmd_rx);
        })
        .context("创建工作线程失败")?;

    // ListenCtl 管道服务：**不再只在托管模式启动**（2026-10-09）
    //
    // 为什么改：`listen.exe --status`（新）与 fork 的「它在不在跑」判断都靠这条管道；
    // 独立模式（用户自己双击 exe / 快捷方式）以前根本没有通道 → `--status` 会误报
    // “listen 未运行”，fork 也会白白尝试拉起再撞单实例锁（30s 退避）。
    // 单实例互斥体保证同一时刻只有一个服务端，不存在两个管道冲突。
    #[cfg(windows)]
    ctl::start(ctl::CtlConfig {
        paths: paths.clone(),
        cmd_tx: cmd_tx.clone(),
        hk_tx: hk_tx.clone(),
        hook: hook_slot.clone(),
        state: engine_state.clone(),
        hotkey: spec.clone(),
        exit_minutes: cfg.general.managed_suspend_exit_minutes,
        chain: chain_state.clone(),
    });

    // 主线程：eframe（设置界面 + 托盘）
    // 托管模式下也给用户一个「录音中」图标：默认保留自己的状态色点托盘图标（`managed_tray`）。
    // 旧行为是托管一律不建图标 —— 少一个图标，但用户“按了不知道在不在录”。
    let no_tray = cli.no_tray || (cli.managed && !cfg.general.managed_tray);
    if cli.managed && !no_tray {
        tracing::info!("托管模式：仍显示自己的状态托盘图标（managed_tray=true）");
    }
    // 只有显式请求（--settings / --first-run）才在启动时显示设置窗；
    // 无参/--managed 启动一律静默后台（修「设置页莫名自动弹出」）。
    let show_on_start = cli.settings || cli.first_run;
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([640.0, 760.0])
        .with_min_inner_size([540.0, 480.0])
        .with_title("倾听输入法 设置");
    if !show_on_start {
        viewport = viewport.with_visible(false);
    }
    let options = eframe::NativeOptions {
        viewport,
        wgpu_options: gui_wgpu_options(&cfg),
        ..Default::default()
    };
    eframe::run_native(
        "倾听输入法 设置",
        options,
        Box::new(move |cc| {
            let mut g = gui::Gui::new(
                cc,
                paths,
                cfg,
                cmd_tx,
                evt_rx,
                cli.first_run,
                cli.settings,
                show_event,
            );
            if no_tray {
                g.tray = None;
            }
            Ok(Box::new(g))
        }),
    )
    .map_err(|e| anyhow::anyhow!("GUI 启动失败: {e}"))?;
    // 走到这里 = GUI 事件循环正常结束（托盘「退出程序」/窗口关闭流程），
    // 留一行收尾日志：以后日志里「突然断掉」和「正常退出」能一眼分开（2026-10-20）
    tracing::info!("GUI 事件循环已结束 → 进程正常退出");
    Ok(())
}

// ---------------------------------------------------------------- 工作线程

/// 输出任务：交给独立"输出线程"做整理 + 注入，避免阻塞录音线程
struct OutputJob {
    raw: String,
    polish: bool,
    /// true = 取原文（Ctrl+Z 撤销上次粘贴再粘贴原文）
    replace_raw: bool,
    polisher: Arc<dyn Polisher + Send + Sync>,
    inject: config::Inject,
    /// 注入前去掉句读标点（保留 ITN 数字规范）
    strip_punctuation: bool,
    /// 注入文本结尾补一个空格
    trailing_space: bool,
}

/// 输出线程主循环：串行处理（保证注入顺序），自行创建 Injector（剪贴板不跨线程共享）
fn output_loop(rx: Receiver<OutputJob>, evt: Sender<Evt>, chain: ChainState) {
    for job in rx {
        let text = if job.polish {
            match job.polisher.polish(&job.raw) {
                Ok(p) if !p.trim().is_empty() => p,
                Ok(_) => job.raw.clone(),
                Err(e) => {
                    tracing::warn!("整理失败，回退原文: {e:#}");
                    let _ = evt.send(Evt::Note(format!("整理失败，已用原文: {e}")));
                    job.raw.clone()
                }
            }
        } else {
            job.raw.clone()
        };
        // 定型：去句读标点（整理后 LLM 可能又加标点，这里兜底）+ 结尾空格
        let text = text::finalize(&text, job.strip_punctuation, job.trailing_space);
        if job.polish {
            let _ = evt.send(Evt::Polished { text: text.clone() });
        }
        let injector = inject::Injector::new(&job.inject);
        // 目标窗口标题必须**注入前**抓（注入后焦点/窗口可能变），给链路卡片显示“上次注入到哪”
        let target = inject::foreground_window_title();
        let res = if job.replace_raw {
            injector.replace_with_raw(&text)
        } else {
            injector.inject(&text)
        };
        let chars = text.chars().count();
        match res {
            Ok(()) => {
                if !job.replace_raw {
                    tracing::info!("已注入 {chars} 字");
                    let _ = evt.send(Evt::Note(format!("已注入 {chars} 字")));
                }
                chain.update(|c| {
                    c.inject = Some(chain::Inject {
                        at: chain::now_hms(),
                        ok: true,
                        chars: chars as u32,
                        target,
                        err: None,
                    })
                });
                // 立刻推一次快照：注入结果不用等 worker 那个 1s 节拍
                let _ = evt.send(Evt::Chain(Box::new(chain.get())));
            }
            Err(e) => {
                tracing::error!("注入失败: {e:#}");
                let _ = evt.send(Evt::Note(format!("注入失败: {e}")));
                chain.update(|c| {
                    c.inject = Some(chain::Inject {
                        at: chain::now_hms(),
                        ok: false,
                        chars: 0,
                        target,
                        err: Some(format!("{e:#}")),
                    })
                });
                let _ = evt.send(Evt::Chain(Box::new(chain.get())));
            }
        }
    }
}

/// 单个分段的处理结果：会话统计 + “本次没出字”的原因反馈
/// 段级闸门判定：一段切出来后，决定要不要送 ASR。
///
/// 抽成**共用函数**的目的（2026-10-07）：让「基线跑分」和「真实会话」走同一套判据。
/// 原 `--simulate` 用的是 `rms < min_speech_rms`，与会话的
/// `max(噪声底×1.5, min_speech_rms)` + `saw_speech()` **不是一套** ——
/// 拿它当基线会测到不存在的线上行为（这也是“改完不知道好不好”的根因之一）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegVerdict {
    Accept,
    TooShort,
    NoSpeech,
    Quiet,
}

/// 返回（判定, 段长秒, 段 RMS）
/// `gate = None` = 能量门不参与（VAD 模式：段能吐出来就是语音，再叠一层能量门就会
/// 回到“轻声说话被吃掉”的老问题）。判定顺序不变：长度 → 有没有语音 → 能量。
fn segment_verdict(
    cfg: &Config,
    samples: &[f32],
    gate: Option<f32>,
    speech_seen: bool,
) -> (SegVerdict, f32, f32) {
    let secs = samples.len() as f32 / audio::TARGET_RATE as f32;
    if secs < cfg.general.min_record_seconds {
        return (SegVerdict::TooShort, secs, 0.0);
    }
    if !speech_seen {
        return (SegVerdict::NoSpeech, secs, 0.0);
    }
    let level = audio::rms(samples);
    if let Some(g) = gate {
        if level < g {
            return (SegVerdict::Quiet, secs, level);
        }
    }
    (SegVerdict::Accept, secs, level)
}

enum SegOutcome {
    /// 出字了（字符数）
    Text(usize),
    /// 段太短（秒）
    TooShort(f32),
    /// 整段过轻，疑似静音/环境噪声（秒）
    Quiet(f32),
    /// 本会话从没出现过语音窗（纯噪声：用户按了没说话）
    NoSpeech(f32),
    /// 识别结果为空
    Empty,
    /// 识别结果疑似幻觉（同字符重复堆叠 —— 碎片/噪声段的典型崩溃形态）
    Halluc(f32),
    /// 识别失败
    Failed,
}

impl SegOutcome {
    fn describe(&self) -> String {
        match self {
            SegOutcome::Text(n) => format!("{n} 字"),
            SegOutcome::TooShort(s) => format!("录音太短（{s:.2}s）"),
            SegOutcome::Quiet(s) => format!("整段过轻（{s:.1}s，疑似静音）"),
            SegOutcome::NoSpeech(s) => format!("未检测到语音（{s:.1}s 纯噪声）"),
            SegOutcome::Empty => "识别结果为空".into(),
            SegOutcome::Halluc(s) => format!("识别结果疑似幻觉（{s:.1}s 重复堆叠，已丢弃）"),
            SegOutcome::Failed => "识别失败".into(),
        }
    }
}

/// config.toml 指纹：(mtime 毫秒, 文件长度)。读不到返回 None
///
/// 用 (mtime, len) 而不是只比 mtime：某些编辑器「保存」会保留 mtime（时间精度/秒级抖动），
/// 而长度变化一定跟着内容变；两者一起看，误判/漏判都极小。
fn cfg_stamp(path: &std::path::Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Some((ms, meta.len()))
}

struct Worker {
    paths: Paths,
    cfg: Config,
    evt: Sender<Evt>,
    asr: Option<Box<dyn asr::AsrBackend>>,
    /// 挂起/恢复：识别模型异步加载中（结果由 run 循环轮询安装）
    asr_loader: Option<Receiver<Result<Box<dyn asr::AsrBackend>>>>,
    /// 引擎状态（设置窗状态卡用；托管模式下与 ctl 共享同一对象）
    engine: Arc<ctl::ManagedState>,
    /// 语音引擎总开关（OFF：不录音、模型释放、钩子已摘）
    voice_enabled: bool,
    /// 当前热键规格（引擎开启时重装钩子用）
    hotkey: HotkeySpec,
    /// 热键事件发送端（重装钩子用）
    hk_tx: Sender<(HotkeyEvent, u64)>,
    /// 热键钩子槽（与 main/ctl 共用）
    hook: HookSlot,
    /// 已挂起（托管）：不可开会话、模型已释放
    suspended: bool,
    polisher: Arc<dyn Polisher + Send + Sync>,
    /// 输出线程：整理 + 注入（与录音解耦，避免整理/注入阻塞下一次录音）
    out_tx: Sender<OutputJob>,
    /// 本地大模型（懒加载；进程只在真正需要时才拉起）
    llm: Arc<LlamaServer>,
    capture: Option<audio::StreamCapture>,
    /// 预热常开（配置）：true = 采集流跨会话复用，按下即可取到 pre-roll
    prewarm: bool,
    /// 预热空闲自动关麦（Duration::ZERO = 不关）
    idle_close: Duration,
    /// 最后一次会话/按下时刻（空闲关麦判定）
    last_active: Instant,
    /// 已喂给分段器的音频水位（全局音频钟 ms）：
    /// 防止本次 pre-roll 把上一次会话的尾巴再喂一遍（那就变成“重复原文”了）
    consumed_ms: u64,
    /// 识别后端不可用时的自动重试时刻（30s 退避）
    asr_retry_at: Option<Instant>,
    last_raw: Option<String>,
    state: State,
    /// vibecoding 术语纠错（配置变更时重建）
    corrector: terms::TermCorrector,
    /// 文本后处理（去标点 / 结尾空格）
    text_norm: config::TextNorm,
    /// 全链路快照（2026-10-09）：每秒更新；ctl 的 `CHAIN`、GUI 卡片、托盘 tooltip 读同一份
    chain: ChainState,
    /// 是否托管模式（--managed，卡片文案用）
    managed: bool,
    /// 钩子累计回调采样窗口（算「近 60s 回调」用）：(采样时刻, 累计次数)
    hook_win: std::collections::VecDeque<(Instant, u64)>,
    /// 最近一次关麦原因（卡片刻意区分「空闲关麦」与「开麦失败」）
    mic_closed_why: Option<String>,
    /// 最近一次会话摘要（卡片）
    last_session: Option<chain::Session>,
    /// 最近一次识别耗时（毫秒，卡片）
    last_asr_ms: Option<u64>,
    /// 识别模型是**因空闲被主动释放**的（不是加载失败）：
    /// ① 抑制 `tick_keepalive` 的 30s 自动重试（否则刚释放就被拉回来）
    /// ② 挂起/恢复、总开关、一键修复都要清它
    /// ③ 卡片文案用 `已释放（空闲 N 分钟）` 而不是 `加载失败`
    asr_released: bool,
    /// 已释放多久的“原因”文案（卡片展示；0 分钟 = 没释放）
    asr_released_min: u32,
    /// 空闲释放动作的防重复标记（Some = 已经释放过，等“人又回来用”再清）
    models_released_at: Option<Instant>,
    /// 本会话**实际生效**的门控引擎（与 `cfg.general.gate_engine` 不一致 = VAD 回退，卡片黄灯）
    gate_used: String,
    /// 上次推给界面的快照文本（变化才推）+ 上次推送时刻（5s 心跳兼底）
    chain_pushed: String,
    chain_at: Instant,
    /// 5s 心跳兼底：即使内容没变也推一次（避免界面因丢事件而永久停在旧状态）
    chain_beat: Instant,
    /// 「有块但电平恒为 0」的起始时刻（数字静音判定；2026-10-09）
    mic_silent_since: Option<Instant>,
    /// 用户词学习：上次扫 Rime 学习日志的时刻（30s 一次）
    learn_last: Instant,
    /// 用户词学习：自上次写入打字表以来攒下的新词条数
    learn_pending: Arc<std::sync::atomic::AtomicUsize>,
    /// config.toml 监视（2026-10-20）：(mtime_ms, 文件长度) 指纹 + 上次检查时刻
    ///
    /// 为什么需要：录音方式（`hotkey_mode`）以前**只有**「设置页点保存并应用」和
    /// ListenCtl 的 `SET` 才会通知 worker 重读配置。直接改 `config.toml`（README 里就是这么
    /// 教用户的）或点设置页的「↺ 重新载入」都不会通知 worker —— 用户看到的就是
    /// 「我明明改成切换模式了，按下去还是松手就截止」（实测 2026-10-20：config.toml 里
    /// 一直是 hold，日志里从来没有一条 toggle 会话）。现在 worker 自己盯着文件，
    /// 任何一条写入路径都会在 0.5s 内热生效。
    cfg_stamp: Option<(u64, u64)>,
    cfg_watch_at: Instant,
    /// 「物理键还按着却收到 Release」被无视的次数（= 抖动/伪造松开事件；卡片/日志可见）
    release_ignored: u64,
    /// 整理后端存活探针的缓存值 + 上次探测时刻（3s 一次，探针本身 200ms 硬上限）
    llm_alive_cached: bool,
    llm_alive_at: Instant,
}

    /// 当前进程常驻内存（MB）：用于"释放模型前后"对比日志
fn rss_mb() -> u64 {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows::Win32::System::Threading::GetCurrentProcess;
        let mut c = PROCESS_MEMORY_COUNTERS::default();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb).is_ok() {
            return (c.WorkingSetSize / 1024 / 1024) as u64;
        }
        0
    }
    #[cfg(not(windows))]
    {
        0
    }
}


/// 修剪工作集（`EmptyWorkingSet`）：把不活跃页换出去，任务管理器里的常驻立刻降下来
///
/// 只影响"报告值/页换出"，不丢数据；下次访问会再换回来（可能有一次轻微缺页）。
/// 只在「已经释放模型、明确空闲」时调用，避免影响正在用的热路径。
fn trim_working_set() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::ProcessStatus::EmptyWorkingSet;
        use windows::Win32::System::Threading::GetCurrentProcess;
        let _ = EmptyWorkingSet(GetCurrentProcess());
    }
}

impl Worker {
    #[allow(clippy::too_many_arguments)]
    fn new(
        paths: Paths,
        cfg: Config,
        evt: Sender<Evt>,
        engine: Arc<ctl::ManagedState>,
        hotkey: HotkeySpec,
        hk_tx: Sender<(HotkeyEvent, u64)>,
        hook: HookSlot,
        chain: ChainState,
        managed: bool,
    ) -> Self {
        let gate_used = cfg.general.gate_engine.clone();
        let voice_enabled = cfg.general.voice_enabled;
        sync_voice_flag(&paths, voice_enabled);
        let asr = if voice_enabled {
            match asr::build(&cfg, &paths) {
                Ok(b) => {
                    tracing::info!("识别后端就绪: {}", b.name());
                    Some(b)
                }
                Err(e) => {
                    tracing::error!("ASR 后端初始化失败: {e:#}");
                    let _ = evt.send(Evt::Note(format!("识别后端不可用: {e}")));
                    None
                }
            }
        } else {
            tracing::info!("语音引擎总开关为 OFF：跳过识别模型加载");
            None
        };
        // 初始化期间可能已收到 SUSPEND（ctl 线程已置位），别把状态改回 ready
        if !voice_enabled {
            engine.set(ctl::ST_DISABLED);
        } else if engine.get() == ctl::ST_SUSPENDED {
            // 保持挂起
        } else if asr.is_some() {
            engine.set(ctl::ST_READY);
        } else {
            engine.set(ctl::ST_NOT_RUNNING);
        }
        let llm = make_llm(&cfg, &paths, &evt);
        let polisher = build_polisher(&cfg, &paths, &llm);
        let corrector = terms::TermCorrector::new(&cfg.glossary);
        let text_norm = cfg.text.clone();
        tracing::info!("文本整理: {}", polisher.name());
        // 输出线程：整理 + 注入。与录音线程解耦，录音结束立即返回、可马上开始下一次录音。
        let (out_tx, out_rx) = mpsc::channel::<OutputJob>();
        let out_evt = evt.clone();
        let out_chain = chain.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("output".into())
            .spawn(move || output_loop(out_rx, out_evt, out_chain))
        {
            tracing::error!("创建输出线程失败（注入将不可用）: {e}");
        }
        let prewarm = cfg.general.prewarm_capture;
        let idle_close = Duration::from_secs(cfg.general.prewarm_idle_minutes.max(0) as u64 * 60);
        // 初始指纹：启动时先记下，别把「启动时读到的这份」当成外部改动再热更新一次
        let cfg_stamp0 = cfg_stamp(&paths.config_file);
        let mut w = Self {
            paths,
            cfg,
            evt,
            asr,
            asr_loader: None,
            engine,
            voice_enabled,
            hotkey,
            hk_tx,
            hook,
            suspended: false,
            polisher,
            out_tx,
            llm,
            capture: None,
            prewarm,
            idle_close,
            last_active: Instant::now(),
            consumed_ms: 0,
            asr_retry_at: None,
            last_raw: None,
            state: State::Idle,
            corrector,
            text_norm,
            gate_used,
            chain,
            managed,
            hook_win: std::collections::VecDeque::new(),
            mic_closed_why: None,
            last_session: None,
            last_asr_ms: None,
            asr_released: false,
            asr_released_min: 0,
            models_released_at: None,
            chain_pushed: String::new(),
            chain_at: Instant::now(),
            chain_beat: Instant::now(),
            mic_silent_since: None,
            learn_last: Instant::now(),
            learn_pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            cfg_stamp: cfg_stamp0,
            cfg_watch_at: Instant::now(),
            release_ignored: 0,
            llm_alive_cached: false,
            // 让首次 tick_chain 立刻探一次
            llm_alive_at: Instant::now() - Duration::from_secs(60),
        };
        // 引擎就绪即预热（挂起也保持：挂起只表示“输入法不在前台”，热键照旧可用）
        if w.voice_enabled && w.asr.is_some() && w.prewarm {
            if let Err(e) = w.ensure_capture("引擎就绪预热") {
                tracing::warn!("预热录音开启失败（下次按下再试）: {e:#}");
            }
        }
        w
    }

    fn run(&mut self, rx: Receiver<Cmd>) {
        let crash_after = crash_test_after();
        let started = Instant::now();
        loop {
            // 【仅测试】LISTEN_PANIC_TEST=<秒>：N 秒后在 worker 线程里故意 panic，
            // 用来验证崩溃自愈链路（写现场 / 自动重启 / 启动提示）。不设变量则完全无副作用。
            if let Some(after) = crash_after {
                if started.elapsed() >= after {
                    panic!("[test] 模拟崩溃（LISTEN_PANIC_TEST={}s）", after.as_secs());
                }
            }
            // 轮询 20ms：按下→开录的延迟直接受它影响（原 120ms，用户体感就是“起头半个字没了”）
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(Cmd::Quit) => {
                    self.shutdown();
                    break;
                }
                // 会话是阻塞式的：进入后由 run_session 内部处理结束条件
                Ok(Cmd::Press(seq)) => self.on_press(&rx, seq),
                Ok(Cmd::Release(_)) => {
                    // 会话内的松开由 run_session 内部消费；会话外的游离松开忽略
                }
                Ok(other) => {
                    self.handle(other);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            self.poll_asr_loader();
            self.tick_keepalive();
            self.tick_config_watch();
            self.tick_chain();
            self.tick_learn();
        }
        tracing::info!("工作线程退出");
    }

    /// 盯着 config.toml：文件一变就热更新（任何写入路径都算：手改 / 设置页 / ctl / --voice）
    fn tick_config_watch(&mut self) {
        // 500ms 一次 stat：开销可忽略，但完全没必要每 20ms 看一次
        if self.cfg_watch_at.elapsed() < Duration::from_millis(500) {
            return;
        }
        self.cfg_watch_at = Instant::now();
        let Some(stamp) = cfg_stamp(&self.paths.config_file) else {
            return;
        };
        if self.cfg_stamp == Some(stamp) {
            return;
        }
        self.cfg_stamp = Some(stamp);
        tracing::info!("检测到 config.toml 已被改写（外部编辑/别的进程写入）→ 自动热更新");
        self.reload_config("config.toml 已被改写");
    }

    /// 一次热键按下
    fn on_press(&mut self, rx: &Receiver<Cmd>, seq: u64) {
        // “人回来用了”：刷新活动时间 + 清掉“已释放”标记（必须在 note_activity 之前判断）
        let reloading = self.asr_released && self.asr.is_none();
        self.note_activity();
        if !self.voice_enabled {
            // 升到 info：总开关 OFF 时会摘钩子，此分支是「按了零日志」的静默路径之一
            tracing::info!("热键按下 seq={seq}，但语音引擎已关闭 → 忽略（托盘菜单/设置页可打开）");
            let _ = self.evt.send(Evt::Hint("语音引擎已关闭（请在设置页打开）".into()));
            return;
        }
        if reloading {
            tracing::info!("空闲释放过识别模型，本次按下重新加载");
            let _ = self.evt
                .send(Evt::Hint("模型已释放，正在重新加载（首次约 +2s）".into()));
        }
        // 每次按下都记一条（info）：便于分辨“按键没上来”还是“上来了没录到”
        let lag = audio::clock_ms().saturating_sub(audio::press_ms());
        tracing::info!(
            "热键按下 seq={seq}（后端 {}，录音方式 {}，钩子→worker 滞后 {lag}ms）",
            if self.asr.is_some() { "就绪" } else { "加载中/不可用" },
            self.cfg.general.mode_label()
        );
        if self.asr.is_none() {
            // 后端未就绪：先等它加载完（实测模型加载 0.8~2s）。
            // 预热环形缓冲还在（3s），按下的这几秒音频不会丢——比“提示一下什么都不录”好得多。
            // 空闲释放过模型时给长一点的等待（我们知道它真在加载，实测 ~1.8s）。
            if self.asr_loader.is_none() {
                self.asr_retry_at = None;
                self.spawn_asr_load();
            }
            let budget = if reloading { 4000 } else { 2500 };
            let deadline = Instant::now() + Duration::from_millis(budget);
            while self.asr.is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
                self.poll_asr_loader();
            }
            if self.asr.is_none() {
                tracing::warn!("识别后端仍未就绪，本次不录音");
                let _ = self.evt.send(Evt::Hint("识别后端未就绪，本次未录音".into()));
                return;
            }
        }
        // 挂起（输入法不在前台）：钩子与模型都还在，按下即用，不再“按了没反应”
        if self.suspended {
            self.suspended = false;
            self.engine.set(ctl::ST_READY);
            tracing::info!("挂起中收到热键：直接恢复并录音（模型常驻，无等待）");
        }
        self.run_session(rx, seq);
    }

    /// 空闲维护：识别后端自动重试 + 预热空闲关麦（每轮循环调一次，几纳秒开销）
    fn tick_keepalive(&mut self) {
        // 后端不可用（模型缺失/损坏/切换失败）→ 30s 退避自动重试，不再“坏了就永远坏”
        // ⚠️ 因空闲**主动释放**的不算故障：`asr_released` 抑制这条自动重试（2026-10-09）
        if self.voice_enabled && self.asr.is_none() && self.asr_loader.is_none() && !self.asr_released
        {
            let due = self.asr_retry_at.map_or(true, |t| Instant::now() >= t);
            if due {
                if self.asr_retry_at.is_none() {
                    tracing::info!("识别后端未就绪，30s 退避自动重试加载");
                }
                self.asr_retry_at = Some(Instant::now() + Duration::from_secs(30));
                self.spawn_asr_load();
            }
        }
        // 预热空闲关麦：常开是为了“按下即录”，长时间不用就把麦克风还回去
        if self.prewarm
            && !self.idle_close.is_zero()
            && self.capture.is_some()
            && self.last_active.elapsed() >= self.idle_close
        {
            let mins = self.idle_close.as_secs() / 60;
            self.drop_capture(&format!("空闲 {mins} 分钟"));
        }
        // 预热流自愈（2026-10-09，配合 audio.rs 里 `is_broken()` 的死代码修复）：
        // 设备被拔/切换/独占时预热流会静默变哑，以前要等下一次按下才发现；
        // 现在空闲时顺手看一眼并重开。**只在 Idle 做**（会话内由 run_session 自己管）。
        if self.prewarm
            && self.voice_enabled
            && self.state == State::Idle
            && self.capture.as_ref().is_some_and(|c| c.is_broken())
        {
            tracing::warn!("预热采集流已无音频（设备被拔/切换？），自动重开");
            if let Err(e) = self.ensure_capture("流损坏自愈") {
                tracing::warn!("预热流自动重开失败: {e:#}");
            }
        }
        // 空闲释放模型（2026-10-09 用户需求）：与麦克风档位**各自独立**；0 = 永不。
        // 为何不看 `last_active` 之外的东西：它已经在“按下/会话/开麦”处刷新，正好是“人不在用”的判据。
        let rel_min = self.cfg.general.model_release_minutes;
        if rel_min > 0
            && self.voice_enabled
            && self.state == State::Idle
            && self.asr_loader.is_none()
            && self.models_released_at.is_none()
            && self.last_active.elapsed() >= Duration::from_secs(rel_min as u64 * 60)
        {
            self.release_idle_models(rel_min);
        }
    }

    /// 空闲释放模型：卸载 ASR 识别模型 + 停掉本程序拉起的 llama-server
    ///
    /// 幂等（靠 `models_released_at` 防重复）；任何“人又回来用”的路径都会把它清掉
    /// （见 `note_activity`），下一次按下会自动重载（实测 ~1.8s）。
    fn release_idle_models(&mut self, mins: u32) {
        if self.models_released_at.is_some() {
            return;
        }
        self.models_released_at = Some(Instant::now());
        if self.asr.is_some() {
            self.asr = None;
            self.asr_released = true;
            self.asr_released_min = mins;
            self.asr_retry_at = None; // 别让 30s 退避重试把它拉回来（asr_released 也会抑制）
            tracing::info!("空闲 {mins} 分钟：已释放识别模型（省内存；下次按下自动重载）");
        }
        // 整理后端：默认**常驻**（停了之后第一次整理会退化成原文，体验下降）。
        // 想省那 ~2.8GB 就把 `llm_idle_stop_minutes` 设成 30 之类。
        let llm_min = self.cfg.general.llm_idle_stop_minutes;
        if llm_min > 0 {
            self.llm.stop(); // 只回收“本程序拉起的”实例，幂等
            tracing::info!(
                "空闲 {mins} 分钟：已停止整理后端 llama-server（省 ~2.8GB；下次整理冷启动，首次用原文）"
            );
        } else {
            tracing::info!("空闲 {mins} 分钟：识别模型已释放；整理后端常驻（llm_idle_stop_minutes=0）");
        }
        // 释放完再修剪一次工作集：模型卸载后分配器往往还留着页，
        // 不修剪的话任务管理器里数字下不去（用户以为"没省"）。
        let before = rss_mb();
        trim_working_set();
        tracing::info!(
            "已修剪工作集：常驻 {before} MB → {} MB（模型已卸；下次按下自动重载）",
            rss_mb()
        );
    }

    /// “人又回来用”了：清掉“已释放”标记（下次按下/会话会重新加载模型）
    fn note_activity(&mut self) {
        self.last_active = Instant::now();
        self.models_released_at = None;
        if self.asr_released {
            self.asr_released = false;
            self.asr_released_min = 0;
        }
    }


/// 每 1s 构建全链路快照并推给界面（内容变化才推 + 5s 心跳兼底）
    ///
    /// 开销：全是原子读 + 一次字符串比较；**不 drain 环形缓冲、不碰 ASR**，
    /// 所以会话内也每秒跑（否则长口述期间卡片/托盘会一直停在旧数据）。
    /// 用户词学习：每 30s 扫一次 Rime lua 写下的学习日志，攒够阈值就把词表重写进打字侧
    ///
    /// 注意：**不需要提权部署** —— 实测 `listen_hotwords.txt` 是每个输入会话开始时重读的纯文本表，
    /// 改完内容换个窗口就生效（所以这里不弹 UAC，不阻塞 worker）。
    fn tick_learn(&mut self) {
        if self.learn_last.elapsed() < Duration::from_secs(10) {
            return;
        }
        self.learn_last = Instant::now();
        if !(self.cfg.hotwords.enabled && self.cfg.hotwords.learn) {
            return;
        }
        match crate::learn::import_new(&self.cfg, &self.paths, false) {
            Ok(r) if !r.is_empty() => {
                tracing::info!(
                    "[学习] 新词 {} 条（次数 +{}），词表共 {} 条",
                    r.new_pairs,
                    r.added_count,
                    r.total
                );
                if r.new_pairs > 0 {
                    self.learn_pending
                        .fetch_add(r.new_pairs, std::sync::atomic::Ordering::SeqCst);
                }
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("[学习] 导入学习日志失败: {e:#}"),
        }

        // 2026-10-21：**立即**生效，不再等「攒够 N 个新词」（默认阈值是 20）。
        // 用户选一次「杰盛微」，10 秒内就该在候选表里排到第一；旧行为要等 20 个新词，
        // 体感就是"输入法没学会"。阈值配置保留但只用于「要不要弹提示」。
        let threshold = self.cfg.hotwords.learn_auto_deploy;
        let pending = self.learn_pending.load(std::sync::atomic::Ordering::SeqCst);
        if pending == 0 {
            return;
        }
        match crate::hotwords::apply_to_rime(&self.cfg, &self.paths) {
            Ok(Some(_)) => {
                tracing::info!("[学习] 已把 {pending} 个新词写进打字表（切窗口即生效，无需部署）");
                self.learn_pending
                    .store(0, std::sync::atomic::Ordering::SeqCst);
                // 攒够阈值才弹提示，但**每次都写表**（上面 apply_to_rime 无条件执行）
                if threshold > 0 && pending >= threshold {
                    let _ = self.evt.send(Evt::Hint(format!(
                        "记住新词 {pending} 条，已写进打字表（切一下窗口生效）"
                    )));
                }
            }
            Ok(None) => {
                // 倾听输入法没装/用户目录不在 → 不重试风暴，直接归零等下次变化
                self.learn_pending
                    .store(0, std::sync::atomic::Ordering::SeqCst);
            }
            Err(e) => tracing::warn!("[学习] 写打字表失败（保留待写入状态）: {e:#}"),
        }
    }

    fn tick_chain(&mut self) {
        if self.chain_at.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.chain_at = Instant::now();

        // 钩子：累计值（hotkey 模块）+ 近 60s 滚动窗口（自己算，不依赖那个 60s 心跳日志）
        let hs = hotkey::stats();
        self.hook_win.push_back((Instant::now(), hs.calls));
        while self
            .hook_win
            .front()
            .is_some_and(|(t, _)| t.elapsed() > Duration::from_secs(60))
        {
            self.hook_win.pop_front();
        }
        let oldest = self.hook_win.front().map(|(_, c)| *c).unwrap_or(hs.calls);
        let installed = self.hook.lock().map(|g| g.is_some()).unwrap_or(false);

        // 整理后端存活探针：3s 一次、200ms 硬上限（绝不把录音线程卡在 HTTP 上）。
        // 详见 `llm_server::LlamaServer::alive_fast` 的注释。
        if self.llm_alive_at.elapsed() >= Duration::from_secs(3) {
            self.llm_alive_at = Instant::now();
            self.llm_alive_cached = self.llm.alive_fast();
        }

        let (rms, flowing) = self
            .capture
            .as_ref()
            .map(|c| c.live_level())
            .unwrap_or((0.0, false));
        // 数字静音：有块、但电平**恰好 0.0** 连续 ≥3s
        // （真实环境底噪不会是精确 0；这能区分“房间安静”与“被系统/硬件静音”）
        let silent = if flowing && rms == 0.0 {
            self.mic_silent_since
                .get_or_insert_with(Instant::now)
                .elapsed()
                >= Duration::from_secs(3)
        } else {
            self.mic_silent_since = None;
            false
        };

        let asr_phase = if !self.voice_enabled {
            "off"
        } else if self.asr.is_some() {
            "ready"
        } else if self.asr_loader.is_some() {
            "loading"
        } else if self.asr_released {
            // 空闲主动释放 ≠ 加载失败（卡片文案与建议完全不同）
            "released"
        } else {
            "failed"
        };
        let backend = self
            .asr
            .as_ref()
            .map(|b| b.name().to_string())
            .unwrap_or_else(|| match self.cfg.asr.backend.as_str() {
                "cloud" => "云端 ASR（未就绪）".to_string(),
                _ => format!("本地 {}（未加载）", self.cfg.asr.model_type),
            });

        let snap = Chain {
            voice_enabled: self.voice_enabled,
            state: match self.engine.get() {
                ctl::ST_READY => "ready",
                ctl::ST_SUSPENDED => "suspended",
                ctl::ST_STARTING => "starting",
                ctl::ST_DISABLED => "disabled",
                _ => "not_running",
            }
            .into(),
            managed: self.managed,
            gate: self.gate_used.clone(),
            gate_cfg: self.cfg.general.gate_engine.clone(),
            hook: chain::Hook {
                installed,
                calls: hs.calls,
                vk_calls: hs.vk_calls,
                selfheal: hs.selfheal,
                calls_60s: hs.calls.saturating_sub(oldest),
            },
            asr: chain::Asr {
                phase: asr_phase.into(),
                backend,
                ms_last: self.last_asr_ms,
                released_min: self.asr_released_min,
            },
            mic: chain::Mic {
                opened: self.capture.is_some(),
                prewarm: self.prewarm,
                idle_close_min: self.cfg.general.prewarm_idle_minutes,
                closed_why: self.mic_closed_why.clone(),
                rms,
                flowing,
                silent,
            },
            polish: chain::Polish {
                tier: self.cfg.polish.tier().to_string(),
                // `llm.state()` 只在**本进程拉过**时才有值；端口上已有实例时应报“已在运行”。
                // 不然卡片会出现“活着但显示未启动”的矛盾（实测踩到过）。
                //
                // 探针用 `alive_fast()`（200ms 硬上限）+ 3s 缓存（2026-10-20）：
                // 本机连一个没人监听的本地端口要 ~2s 才失败，而这张快照每秒都要算一次，
                // 直接把录音线程卡成“松手 2s 才收尾 / 短按窗口错过”。状态晚 3 秒没人看得出来，
                // 录音被卡住用户立刻就感觉得到。
                llm: {
                    let alive = self.llm_alive_cached;
                    match self.llm.state() {
                        LlmState::Unknown if alive => "已在运行（复用/已有实例）".to_string(),
                        s => s.label(),
                    }
                },
                llm_alive: self.llm_alive_cached,
            },
            last_session: self.last_session.clone(),
            // inject 由输出线程写：下面合并时保留现值（不覆盖）
            inject: None,
        };

        let wire = snap.wire();
        let changed = wire != self.chain_pushed;
        let beat = self.chain_beat.elapsed() >= Duration::from_secs(5);
        self.chain.update(|cur| {
            let inject = cur.inject.clone();
            *cur = snap;
            cur.inject = inject;
        });
        if changed || beat {
            self.chain_pushed = wire;
            self.chain_beat = Instant::now();
            let _ = self.evt.send(Evt::Chain(Box::new(self.chain.get())));
        }
    }

    /// 一键修复（链路状态卡片按钮）：不必重启进程就能修的环，能重来的都重来
    ///
    /// 顺序：重装钩子 → 重开麦 → 强制重载识别模型 → 后台拉起整理后端。
    /// 会话进行中不动（会把正在说的话切碎），只提示。
    fn repair(&mut self) {
        if self.state != State::Idle {
            let _ = self
                .evt
                .send(Evt::Hint("正在录音/识别中，稍后再点「一键修复」".into()));
            return;
        }
        tracing::info!("一键修复：重装钩子 + 重开麦 + 重载模型/整理后端");
        let mut done: Vec<String> = Vec::new();

        if self.voice_enabled {
            // 1. 热键钩子（幂等：未挂时重装）
            self.install_hook();
            let ok = self.hook.lock().map(|g| g.is_some()).unwrap_or(false);
            done.push(if ok { "钩子✓".into() } else { "钩子✗".into() });
            // 2. 麦克风（先关后开：设备被占用/切换过的流重开往往就好）
            self.drop_capture("一键修复");
            match self.ensure_capture("一键修复") {
                Ok(()) => done.push("麦克风✓".into()),
                Err(e) => {
                    tracing::warn!("一键修复重开麦克风失败: {e:#}");
                    done.push("麦克风✗".into());
                }
            }
            // 3. 识别模型（丢掉旧实例，异步重载；不阻塞界面）
            self.asr = None;
            self.asr_retry_at = None;
            self.asr_released = false;
            self.asr_released_min = 0;
            self.models_released_at = None;
            if self.asr_loader.is_none() {
                self.spawn_asr_load();
            }
            done.push("模型重载中".into());
        }
        // 4. 整理后端（本地且没活着 → 后台拉起，不在这里同步等冷启动）
        if self.cfg.polish.enabled
            && self.cfg.polish.provider == "local"
            && !self.llm.alive_fast()
        {
            self.llm.spawn_background();
            done.push("整理后端拉起中".into());
        }
        let _ = self
            .evt
            .send(Evt::Hint(format!("一键修复：{}", done.join(" · "))));
        // 修复结果立刻反映到卡片（不等下一个 1s 节拍）
        self.chain_at = Instant::now() - Duration::from_secs(2);
        self.tick_chain();
    }

    // ------------------------------------------------------------ 采集流（预热常开）

    /// 确保采集流可用（预热常开：一次打开、跳会话复用；报错的流会重开）
    fn ensure_capture(&mut self, why: &str) -> Result<()> {
        if let Some(c) = &self.capture {
            if !c.is_broken() {
                return Ok(());
            }
        }
        if self.capture.is_some() {
            self.drop_capture(why);
        }
        let cap = audio::StreamCapture::start(&self.cfg.general.device)?;
        tracing::info!("采集流已打开（{why}，{}Hz）", cap.rate());
        self.capture = Some(cap);
        self.mic_closed_why = None;
        // 新流的环里没有历史音频：水位抬到当前，避免下次切片把旧数据当 pre-roll
        self.consumed_ms = audio::clock_ms();
        // 同理刷新“最近活动”：否则在“空闲 10 分钟后关麦”之后重新开预热（如设置页保存并应用），
        // 下一次 tick_keepalive 会拿旧时间戳判定“已空闲” → 刚开就立刻关；
        // 实测后果：那次会话 `起头补 0ms`（无 pre-roll）→ 起头丢字。
        self.last_active = Instant::now();
        Ok(())
    }

    /// 关掉采集流（引擎关闭 / 空闲回收 / 流损坏）
    fn drop_capture(&mut self, why: &str) {
        if let Some(c) = self.capture.take() {
            c.stop();
            tracing::info!("采集流已关闭（{why}）");
            // 记下“为什么关”：链路卡片要能区分「空闲关麦（正常降级）」与「开麦失败（真问题）」
            self.mic_closed_why = Some(why.to_string());
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Quit | Cmd::Press(_) | Cmd::Release(_) => {}
            Cmd::Suspend => self.suspend_now(),
            Cmd::Resume => self.resume_async(),
            Cmd::LlmStart => {
                let me = self.llm.clone();
                std::thread::Builder::new()
                    .name("llm-manual-start".into())
                    .spawn(move || {
                        if let Err(e) = me.ensure_started() {
                            tracing::warn!("手动启动本地大模型失败: {e:#}");
                        }
                    })
                    .ok();
            }
            Cmd::LlmStop => self.llm.stop(),
            Cmd::Repair => self.repair(),
            Cmd::ReloadHotwords => {
                // 热词表 tsv 改过（配置没变）：整理 prompt 的术语表 + ASR 热词都要换新的
                tracing::info!("热词表已更新，重建识别/整理后端");
                self.polisher = build_polisher(&self.cfg, &self.paths, &self.llm);
                if self.voice_enabled && !self.suspended && self.asr_loader.is_none() {
                    self.rebuild_asr();
                }
            }
            Cmd::Raw => {
                if let Some(raw) = self.last_raw.clone() {
                    // 交给输出线程（与录音/其他注入串行，避免剪贴板竞争）
                    let job = OutputJob {
                        raw,
                        polish: false,
                        replace_raw: true,
                        polisher: self.polisher.clone(),
                        inject: self.cfg.inject.clone(),
                        strip_punctuation: false,
                        trailing_space: false,
                    };
                    if self.out_tx.send(job).is_err() {
                        tracing::error!("输出线程已退出，取原文失败");
                    }
                } else {
                    tracing::info!("尚无原文可替换");
                }
            }
            Cmd::ReloadConfig => self.reload_config("配置热更新"),
        }
    }

    /// 重新载入 config.toml 并热生效（设置页保存 / ctl `SET` / 手改文件都走这一条）
    fn reload_config(&mut self, why: &str) {
        let old_mode = self.cfg.general.mode_label();
        match Config::load_or_create(&self.paths) {
            Ok(c) => {
                let backend_changed = c.asr.backend != self.cfg.asr.backend;
                let hotwords_changed =
                    c.hotwords != self.cfg.hotwords || c.glossary != self.cfg.glossary;
                let polish_changed = c.polish.enabled != self.cfg.polish.enabled
                    || c.polish.provider != self.cfg.polish.provider
                    || c.polish.mode != self.cfg.polish.mode
                    || c.polish.base_url != self.cfg.polish.base_url
                    || c.polish.model != self.cfg.polish.model;
                let llm_changed = c.polish.local != self.cfg.polish.local;
                let voice_changed = c.general.voice_enabled != self.voice_enabled;
                self.cfg = c;
                // 术语表 / 文本后处理随配置热更新
                self.corrector = terms::TermCorrector::new(&self.cfg.glossary);
                self.text_norm = self.cfg.text.clone();
                // 预热录音（常开/空闲关麦）随配置热生效
                self.prewarm = self.cfg.general.prewarm_capture;
                self.idle_close = Duration::from_secs(
                    self.cfg.general.prewarm_idle_minutes.max(0) as u64 * 60,
                );
                // 模型释放档位也是配置项：改了它就把“刚睡下”的标记清掉，让新值立即生效
                self.models_released_at = None;
                if !self.prewarm {
                    self.drop_capture("关闭预热");
                } else if self.voice_enabled && self.asr.is_some() {
                    if let Err(e) = self.ensure_capture("开启预热") {
                        tracing::warn!("开启预热录音失败: {e:#}");
                    }
                }
                // 引擎总开关联动（设置窗/外部改 config 都走这里）
                if voice_changed {
                    if self.cfg.general.voice_enabled {
                        self.voice_on();
                    } else {
                        self.voice_off();
                    }
                }
                // 挂起/加载中/总开关 OFF 不重建识别后端（等 RESUME / 开关开启统一加载）
                let asr_missing = self.asr.is_none() && self.asr_loader.is_none();
                if self.voice_enabled
                    && !self.suspended
                    && self.asr_loader.is_none()
                    && (backend_changed || asr_missing || hotwords_changed)
                {
                    self.rebuild_asr();
                }
                if llm_changed {
                    // 旧实例在这里被替换 → Drop 按 stop_on_exit 回收（只回收自己拉起的）
                    self.llm = make_llm(&self.cfg, &self.paths, &self.evt);
                    let _ = self.evt.send(Evt::LlmState(LlmState::Unknown));
                }
                if polish_changed || llm_changed || hotwords_changed {
                    self.polisher = build_polisher(&self.cfg, &self.paths, &self.llm);
                    tracing::info!("文本整理: {}", self.polisher.name());
                }
                // 录音方式单独打一条（用户最关心、也最容易“看着改了其实没生效”）
                let new_mode = self.cfg.general.mode_label();
                if new_mode != old_mode {
                    tracing::info!(
                        "录音方式已切换为「{new_mode}」（{why}）—— 下一次按下生效"
                    );
                    let _ = self.evt.send(Evt::Note(format!("录音方式：{new_mode}")));
                }
                self.cfg_stamp = cfg_stamp(&self.paths.config_file);
                tracing::info!("配置已热更新（{why}；录音方式 = {new_mode}）");
                // 同步给设置界面（外部 SET 命令改配置时，界面不落伍）
                let _ = self.evt.send(Evt::Config(Box::new(self.cfg.clone())));
            }
            Err(e) => tracing::error!("重新载入配置失败: {e:#}"),
        }
    }

    /// 托管挂起：停会话（若有）；模型**常驻不卸载**（启动加速）；幂等
    /// 托管挂起：停会话（若有）；模型与钩子都**保持**（启动加速 + 热键随时可用）
    ///
    /// 2026-10-06 改：原实现挂起会掉热键，而 fork 只要“输入法不在前台”就发 SUSPEND，
    /// 用户看到的就是“按 F9 没任何反应”（当天 19 次 SUSPEND / 12 次 RESUME）。
    /// 现在挂起只表示“不在输入法上下文”，热键按下即恢复并录音（模型常驻，无等待）。
    fn suspend_now(&mut self) {
        if !self.voice_enabled {
            return; // 总开关 OFF：引擎已停，不进入挂起态
        }
        // 预热流保持开着（否则挂起后第一次按下必丢起头）
        self.suspended = true;
        self.engine.set(ctl::ST_SUSPENDED);
        tracing::info!("挂起：钩子与模型常驻，热键仍可用；采集流保持预热");
    }

    /// 托管恢复：异步重载识别模型（已就绪/加载中则只清挂起标志）；幂等
    fn resume_async(&mut self) {
        if !self.voice_enabled {
            return; // 总开关 OFF：RESUME 不启动引擎
        }
        self.suspended = false;
        self.models_released_at = None;
        self.asr_released = false;
        self.asr_released_min = 0;
        if self.asr.is_some() {
            self.engine.set(ctl::ST_READY);
            return;
        }
        self.spawn_asr_load();
    }

    /// 异步加载识别后端（幂等：已就绪/已在加载则忽略）
    /// 调用方：托管恢复、启动/切换配置、后端不可用时的 30s 退避重试、按下时的等待
    fn spawn_asr_load(&mut self) {
        if self.asr.is_some() || self.asr_loader.is_some() {
            return;
        }
        self.engine.set(ctl::ST_STARTING);
        let (tx, rx) = mpsc::channel::<Result<Box<dyn asr::AsrBackend>>>();
        let cfg = self.cfg.clone();
        let paths = self.paths.clone();
        match std::thread::Builder::new()
            .name("asr-reload".into())
            .spawn(move || {
                let r = asr::build(&cfg, &paths);
                let _ = tx.send(r);
            }) {
            Ok(_) => {
                self.asr_loader = Some(rx);
                tracing::info!("识别后端异步加载中…");
            }
            Err(e) => tracing::error!("启动模型加载线程失败: {e}"),
        }
    }

    /// 轮询异步加载结果（挂起期间也**安装**：模型常驻是设计选择，别再丢掉结果）
    fn poll_asr_loader(&mut self) {
        let result = match &self.asr_loader {
            Some(rx) => match rx.try_recv() {
                Ok(r) => Some(r),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err(anyhow::anyhow!("识别模型加载线程异常退出")))
                }
            },
            None => None,
        };
        let Some(r) = result else { return };
        self.asr_loader = None;
        match r {
            Ok(b) => {
                tracing::info!("识别模型已就绪: {}", b.name());
                self.asr = Some(b);
                self.asr_retry_at = None;
                // 加载成功 = 不再是“已释放”状态（但**保留** models_released_at：
                // 那表示“刚睡下去”，不应该因为回溯加载而立刻再睡一次）
                self.asr_released = false;
                self.asr_released_min = 0;
                self.engine.set(if self.suspended {
                    ctl::ST_SUSPENDED
                } else {
                    ctl::ST_READY
                });
                // 预热：引擎就绪即把采集流常开（按下就能取到 pre-roll）
                if self.prewarm && self.capture.is_none() {
                    if let Err(e) = self.ensure_capture("模型就绪预热") {
                        tracing::warn!("预热录音开启失败（下次按下再试）: {e:#}");
                    }
                }
                let _ = self.evt.send(Evt::Note("识别模型已就绪".into()));
            }
            Err(e) => {
                tracing::error!("识别模型加载失败: {e:#}");
                self.engine.set(ctl::ST_NOT_RUNNING);
                let _ = self.evt.send(Evt::Note(format!("识别模型加载失败: {e}")));
            }
        }
    }

    /// 开始一次录音会话（阻塞直到会话结束）
    ///
    /// `active_seq`：本次会话对应的热键按下序号；只有携带**相同序号**的 Release
    /// 才会结束录音，过期/无主的 Release 一律忽略（修「按住却提前停」）。
    ///
    /// 采集流由“预热”管理（不再每会话开关）；会话只负责从环形缓冲切片 + 识别 + 注入：
    /// - 起点 = `max(按下时刻 − PREROLL_MS, 已消费水位)`：补回「按下 → 开录」丢的起头，
    ///   又不会把上一次会话的尾巴再喂一遍（那会变成“重复原文”）；
    /// - 松手后再收 `RELEASE_GRACE_*` 宽限（普通按住 0.5s 收尾音；短按 1.5s，兼顾“点完才开口”）。
    fn run_session(&mut self, rx: &Receiver<Cmd>, active_seq: u64) {
        let warm = self.capture.is_some();
        if self.capture.is_none() {
            if let Err(e) = self.ensure_capture("会话开始") {
                self.fail(&format!("打开录音设备失败: {e:#}"));
                return;
            }
        }
        let mut cur_rate = match &self.capture {
            Some(c) => c.rate(),
            None => return,
        };
        let mut resampler = audio::StreamResampler::new(cur_rate, audio::TARGET_RATE);
        // 门控判据：vad（默认）或 energy；VAD 不可用时 build() 内部自动回退并记 WARN
        let mut segmenter = gate::build(&self.cfg, &self.paths);
        // 记录**实际生效**的门控引擎（VAD 模型缺失时会自动回退能量门，卡片要能看出来）
        self.gate_used = segmenter.name().to_string();
        let mut pending = String::new();
        let mut last_flush = Instant::now();
        let max_secs = self.cfg.general.max_session_minutes.max(1) as u64 * 60;
        let deadline = Instant::now() + Duration::from_secs(max_secs);
        let toggle = self.cfg.general.is_toggle_mode();
        self.last_active = Instant::now();

        // 起头切片：按下时刻 − pre-roll，但不早于已消费水位
        let press_ms = audio::press_ms();
        let preroll = if warm { PREROLL_MS } else { 0 };
        let ideal_ms = press_ms.saturating_sub(preroll);
        let want_ms = self.consumed_ms.max(ideal_ms);
        let head_loss_ms = want_ms.saturating_sub(ideal_ms);
        let preroll_used_ms = press_ms.saturating_sub(want_ms);

        let mut fed = 0usize;
        if let Some(cap) = &self.capture {
            let (raw, end_ms, dropped_ms) = cap.drain_from_ms(want_ms);
            self.consumed_ms = end_ms;
            if dropped_ms > 0 {
                tracing::warn!("按下时音频已被环形缓冲覆盖 {dropped_ms}ms（worker 正忙，可能缺几句话头）");
            }
            if !raw.is_empty() {
                let mut mono = Vec::with_capacity(raw.len() / 3 + 8);
                resampler.push(&raw, &mut mono);
                if !mono.is_empty() {
                    fed += mono.len();
                    segmenter.push(&mono);
                }
            }
        }

        self.set_state(State::Recording);
        tracing::info!(
            "会话开始（{}，门控 {}，分段 {}s，静音断句 {}s，注入间隔 {}s，上限 {} 分钟；起头补 {}ms/滞后 {}ms）",
            if toggle { "切换模式" } else { "按住模式" },
            self.cfg.general.gate_engine,
            self.cfg.general.segment_seconds,
            self.cfg.general.silence_seconds,
            self.cfg.general.flush_seconds,
            self.cfg.general.max_session_minutes,
            preroll_used_ms.min(PREROLL_MS),
            head_loss_ms
        );

        let mut ending = false;
        let mut cancel = false;
        // 挂起请求：优雅收尾（识别+注入）后再挂起，不丢弃已录内容
        let mut suspend_after = false;
        // 结束原因（写进会话结束日志 —— 「怎么自己停了」这个疑问必须能从日志里直接回答）
        let mut end_reason: &str = "未知";
        // 当前会话对应的按下序号（可按新按下重新武装）
        let mut active_seq = active_seq;
        // 松手后的尾音宽限到期时刻
        let mut end_at: Option<Instant> = None;
        // 宽限期间最早可提前收尾的时刻（识别到内容后最多再等 300ms）
        let mut end_min_at: Option<Instant> = None;
        // 物理键已抬起的起始时刻（松手看门狗）
        let mut key_up_since: Option<Instant> = None;
        // 统计（会话结束日志 + “没出字”反馈）
        let mut rec_segs = 0usize;
        let mut rec_chars = 0usize;
        let mut drop_why: Option<String> = None;
        // D3 悬浮窗实时读数：最近一块音频的电平 + 上次已发的语音状态 + 节流时刻
        let mut block_rms = 0.0f32;
        let mut gate_speech = false;
        let mut gate_sent_at = Instant::now() - Duration::from_secs(1);

        loop {
            // 0. 流健康：设备切换/断开 → 重开（否则“预热常开”会退化成“永远静音”）
            if self.capture.as_ref().map_or(true, |c| c.is_broken()) {
                if let Some(cap) = &self.capture {
                    let (raw, end_ms) = cap.drain_all();
                    self.consumed_ms = end_ms;
                    if !raw.is_empty() {
                        let mut mono = Vec::new();
                        resampler.push(&raw, &mut mono);
                        fed += mono.len();
                        segmenter.push(&mono);
                    }
                }
                match self.ensure_capture("流损坏重开") {
                    Ok(()) => {
                        let new_rate = self.capture.as_ref().map(|c| c.rate()).unwrap_or(cur_rate);
                        if new_rate != cur_rate {
                            tracing::warn!("采集流采样率变化 {cur_rate} → {new_rate}，重建重采样器");
                            resampler = audio::StreamResampler::new(new_rate, audio::TARGET_RATE);
                            cur_rate = new_rate;
                        }
                        tracing::warn!("采集流已重开（本段可能缺少量音频）");
                    }
                    Err(e) => {
                        tracing::error!("重开采集流失败: {e:#}");
                        let _ = self.evt.send(Evt::Hint(format!("录音设备异常：{e}")));
                        self.fail(&format!("录音设备异常: {e:#}"));
                        break;
                    }
                }
            }

            // 1. 取音频 → 重采样 → 分段（一次取光；worker 被 ASR 阻塞时音频在环形缓冲里等着）
            if let Some(cap) = &self.capture {
                let (raw, end_ms) = cap.drain_all();
                self.consumed_ms = end_ms;
                if !raw.is_empty() {
                    let mut mono = Vec::with_capacity(raw.len() / 3 + 8);
                    resampler.push(&raw, &mut mono);
                    if !mono.is_empty() {
                        fed += mono.len();
                        block_rms = audio::rms(&mono);
                        segmenter.push(&mono);
                    }
                }
            }

            // 1.5 门控实时读数（D3）→ 悬浮窗「语音 ● 电平 0.012」
            // 节流：语音状态一变立刻发（说话/收声的瞬间要看得到），否则最多 4 次/秒。
            if let Some(speech) = segmenter.live_speech() {
                let now = Instant::now();
                if speech != gate_speech
                    || now.duration_since(gate_sent_at) >= Duration::from_millis(250)
                {
                    gate_speech = speech;
                    gate_sent_at = now;
                    let _ = self.evt.send(Evt::Gate {
                        speech,
                        rms: block_rms,
                    });
                }
            }

            // 2. 切出的段立即识别（边录边转）
            while let Some(seg) = segmenter.take_segment() {
                let cut = segmenter.last_cut_reason();
                let gate = segmenter.gate_hint();
                let out = self.process_segment(&seg, &mut pending, gate, cut, segmenter.saw_speech());
                match out {
                    SegOutcome::Text(n) => {
                        rec_segs += 1;
                        rec_chars += n;
                    }
                    other => drop_why = Some(other.describe()),
                }
            }

            // 3. 到点就注入已识别的部分
            if last_flush.elapsed().as_secs() >= self.cfg.general.flush_seconds.max(1) as u64 {
                self.flush_pending(&mut pending, true);
                last_flush = Instant::now();
            }

            // 3.5 松手看门狗：KEYUP 万一丢了（系统摘钩/重装/事件被吞），
            // 靠物理键状态兜底收尾，不让会话一直录到 30 分钟上限
            if !toggle && !ending {
                if hotkey::is_key_down(self.hotkey.vk) {
                    key_up_since = None;
                } else {
                    let since = *key_up_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= Duration::from_millis(700) {
                        tracing::warn!("未收到 Release（松手看门狗兜底）：按物理键状态收尾");
                        end_reason = "松手（看门狗兜底：KEYUP 丢了）";
                        ending = true;
                    }
                }
            }

            // 4. 处理命令
            while let Ok(cmd) = rx.try_recv() {
                match cmd {
                    Cmd::Release(seq) if !toggle && seq == active_seq => {
                        // ★ 松开事件必须被**物理键状态**证实（2026-10-20）
                        //
                        // 为什么：用户报「按住说话、没松手，程序自己截止了，而且很频繁」。
                        // 会话在按住模式里唯一能“自己”结束的入口就是这个 Release（另一个是
                        // 上面的看门狗）。而 Release 既可能来自真实松开，也可能来自
                        //   ① 键盘抖动/粘连：KEYUP+KEYDOWN 成对出现（只隔几毫秒）；
                        //   ② 别的程序/驱动注入的假 KEYUP；
                        //   ③ 钩子把 auto-repeat 判成“新按下”之后的错位配对。
                        // 这三种情况下物理键其实还按着 —— 所以这里用 GetAsyncKeyState 复核：
                        // 键仍按着 = 这次 Release 不可信，**继续录**（真松手时下面的看门狗
                        // 会在 700ms 后兜底收尾，最差只是晚 0.7s，不会把话砍断）。
                        if hotkey::is_key_down(self.hotkey.vk) {
                            self.release_ignored += 1;
                            tracing::warn!(
                                "忽略 Release(seq={seq})：物理键仍处于按下状态（假的/抖动的松开事件，累计 {} 次）→ 继续录音",
                                self.release_ignored
                            );
                            key_up_since = None;
                        } else {
                            end_reason = "松手";
                            ending = true;
                            break;
                        }
                    }
                    Cmd::Release(seq) => {
                        if toggle {
                            // 切换模式下松开**本来就不该结束会话**，这是设计；用 debug 免得刷日志
                            tracing::debug!("切换模式：松开 seq={seq}（不结束会话）");
                        } else if seq != active_seq {
                            // 序号不匹配 = 过期/无主的松开，忽略（提到 warn：这个 bug 曾经只能靠 debug 日志查）
                            tracing::warn!("忽略过期 Release（seq={seq} active_seq={active_seq}）");
                        }
                    }
                    Cmd::Press(seq) => {
                        if toggle {
                            // 日志不可缺：这一支以前是静默的，导致“每支按下都能在日志里找到落点”
                            // 的对账无法进行（2026-10-08 装机对账里被误判成“按下被吞”）。
                            tracing::info!("切换模式：再次按下 seq={seq} → 停止录音");
                            end_reason = "切换模式再次按下";
                            ending = true;
                            break;
                        }
                        // 按住模式：会话中又收到"按下"（掉键/重按）→ 以最新序号重新武装，
                        // 否则后续 Release 会因序号不匹配被忽略、会话卡到上限；
                        // 若正在尾音宽限中，说明用户接着又说了一句 → 取消收尾继续录
                        active_seq = seq;
                        ending = false;
                        end_at = None;
                        tracing::info!("会话中再次按下 seq={seq}：取消收尾，合并继续录（按住模式）");
                        // 可见反馈：状态仍是 Recording，set_state 同态会直接 return，
                        // 用户于是以为“按了没反应”（装机日志里这类 rearm 高达 51 次/30 分钟）。
                        let _ = self.evt.send(Evt::Hint("续录中（会话继续，不用重按）".into()));
                    }
                    Cmd::Suspend => {
                        // 挂起（托管）绝不能砍掉正在录的这段话（2026-10-20）
                        //
                        // 旧行为：收到 SUSPEND 就 `ending = true` —— 于是 fork 因为
                        // 「输入法不在前台/焦点变了」发一次挂起，正在录的这一段就被截断了。
                        // 用户体感正是「没松手，程序自己截止了」。现在：只记下“待挂起”，
                        // 会话该录多久还录多久（松手/再按一下/上限），收尾后再挂。
                        suspend_after = true;
                        tracing::info!(
                            "收到挂起请求：先把这一段录完（松手后自动收尾并挂起，不打断录音）"
                        );
                    }
                    Cmd::Quit => {
                        // 退出前把这一轮还没上屏的文字吐出去（否则用户体感就是「识别了却没出字」）
                        self.flush_pending(&mut pending, false);
                        self.drop_capture("退出");
                        self.shutdown();
                        std::process::exit(0);
                    }
                    other => {
                        self.handle(other);
                        // 总开关在会话中途被关掉：取消本次会话
                        if !self.voice_enabled {
                            end_reason = "总开关被关闭";
                            cancel = true;
                            break;
                        }
                    }
                }
            }
            if cancel || Instant::now() >= deadline {
                if !cancel {
                    tracing::info!("会话达到上限，自动结束");
                    end_reason = "达到会话上限";
                }
                break;
            }
            if ending {
                match end_at {
                    None => {
                        // 0.5s（普通按住的收尾音）/ 1.5s（短按：用户常“点一下再说话”）
                        let grace = if toggle {
                            RELEASE_GRACE_HOLD_MS
                        } else {
                            let held = audio::clock_ms().saturating_sub(press_ms);
                            if held < TAP_MAX_MS {
                                tracing::info!(
                                    "短按（按住 {held}ms）→ 继续听 {RELEASE_GRACE_TAP_MS}ms（点完才开口也能收进来）"
                                );
                                RELEASE_GRACE_TAP_MS
                            } else {
                                RELEASE_GRACE_HOLD_MS
                            }
                        };
                        end_at = Some(Instant::now() + Duration::from_millis(grace));
                        end_min_at = Some(Instant::now() + Duration::from_millis(300));
                    }
                    Some(t) => {
                        let now = Instant::now();
                        // 宽限期间已切出并识别到内容（=人说完停了一下）→ 提前收尾，
                        // 不必死等 1.5s（否则“点一下再说话”只能收前半句）
                        if now >= t || (rec_segs > 0 && end_min_at.map_or(false, |m| now >= m)) {
                            break;
                        }
                    }
                }
            }
            // 会话内也要推链路快照：run_session 是阻塞的，不在这个循环里 tick 的话
            // 一场 20 分钟的长口述期间卡片/托盘会一直是旧数据（“实时监控”就名不副实）
            self.tick_chain();
            // 会话进行中也盯 config.toml：用户常常改了录音方式就马上按 F9 试，
            // 不该等到下一次进程重启才生效
            self.tick_config_watch();
            std::thread::sleep(Duration::from_millis(40));
        }

        if cancel {
            // cancel 只有一个来源：会话中途总开关被关掉（`!voice_enabled`）。
            // 老日志写成「挂起请求」是误导——挂起请求现在**不会**取消会话（见 §28）。
            // 先把已识别但还没上屏的文字吐出去，别让用户看到「识别出来了却没了」。
            self.flush_pending(&mut pending, false);
            tracing::info!("会话已取消（语音引擎总开关被关闭，原因：{end_reason}）");
            if !self.prewarm {
                self.drop_capture("会话取消");
            }
            self.suspend_now();
        } else {
            // 收尾：吐出重采样器与分段器的余量，识别完再注入一次
            if let Some(cap) = &self.capture {
                let (raw, end_ms) = cap.drain_all();
                self.consumed_ms = end_ms;
                let mut mono = Vec::new();
                if !raw.is_empty() {
                    resampler.push(&raw, &mut mono);
                }
                resampler.flush(&mut mono);
                if !mono.is_empty() {
                    fed += mono.len();
                    segmenter.push(&mono);
                }
            }
            if let Some(seg) = segmenter.flush() {
                let gate = segmenter.gate_hint();
                match self.process_segment(&seg, &mut pending, gate, "flush", segmenter.saw_speech())
                {
                    SegOutcome::Text(n) => {
                        rec_segs += 1;
                        rec_chars += n;
                    }
                    other => drop_why = Some(other.describe()),
                }
            }
            self.flush_pending(&mut pending, false);
            self.set_state(State::Idle);
            let audio_secs = fed as f32 / audio::TARGET_RATE as f32;
            let held_ms = audio::clock_ms().saturating_sub(press_ms);
            tracing::info!(
                "会话结束：音频 {audio_secs:.1}s → 出字 {rec_segs} 段 {rec_chars} 字（原因：{end_reason}；按住 {held_ms}ms）"
            );
            // 「怎么自己停了」必须一眼可见：不是**用户主动**收尾就补一条 WARN（带键状态 +
            // 被无视的假松开次数），下次现场直接能从日志定位是谁结束的。
            // 用户主动收尾 = 松手（含看门狗兜底）或 toggle 再按一下；这两类不刷 WARN。
            let user_driven =
                end_reason.starts_with("松手") || end_reason.starts_with("切换模式");
            if !user_driven {
                tracing::warn!(
                    "本会话不是用户主动收尾（原因：{end_reason}），录音方式={}，物理键现在={}，已无视假松开 {} 次",
                    self.cfg.general.mode_label(),
                    if hotkey::is_key_down(self.hotkey.vk) {
                        "按下"
                    } else {
                        "抬起"
                    },
                    self.release_ignored
                );
            }
            // 会话摘要进链路快照（卡片/--status 看“上一次到底出没出字”）
            let no_text_why = if rec_segs == 0 {
                Some(
                    drop_why
                        .clone()
                        .unwrap_or_else(|| "未检测到语音".into()),
                )
            } else {
                None
            };
            self.last_session = Some(chain::Session {
                at: chain::now_hms(),
                audio_s: audio_secs,
                segs: rec_segs as u32,
                chars: rec_chars as u32,
                why: no_text_why.clone(),
            });
            if let Some(why) = no_text_why {
                tracing::warn!("本次未出字（{why}）");
                let _ = self.evt.send(Evt::Hint(format!("本次未出字：{why}")));
            }
            if !self.prewarm {
                self.drop_capture("会话结束（未开预热）");
            }
            if suspend_after {
                self.suspend_now();
            }
        }
        self.last_active = Instant::now();

        // 注意：这里**不再**清空通道里残留的 Press/Release。
        // 旧实现会把"会话收尾期间用户已按下的下一个 Press"一起丢掉，导致必须按两次
        // 才能开始下一次录音（本次修复）。残留的 Release 由 run() 的 `Cmd::Release(_)` 忽略，
        // 新 Press 由 run() 正常开启新会话；Quit/Raw/ReloadConfig 等也照常由 run() 处理。
    }

    /// 处理一个音频段。`gate` = 自适应噪声门限（低于它视为环境噪声），`cut` = 切分原因（日志用），
    /// `speech_seen` = 本会话是否出现过语音窗（纯噪声会话直接丢弃，防 ASR 幻觉出整句）
    fn process_segment(
        &mut self,
        samples: &[f32],
        pending: &mut String,
        gate: Option<f32>,
        cut: &str,
        speech_seen: bool,
    ) -> SegOutcome {
        let (verdict, secs, level) = segment_verdict(&self.cfg, samples, gate, speech_seen);
        match verdict {
            SegVerdict::TooShort => {
                tracing::info!("段太短（{secs:.2}s，切分 {cut}），跳过");
                return SegOutcome::TooShort(secs);
            }
            SegVerdict::NoSpeech => {
                tracing::info!("本会话未出现语音窗（{secs:.1}s，切分 {cut}），疑似纯噪声，跳过");
                return SegOutcome::NoSpeech(secs);
            }
            SegVerdict::Quiet => {
                tracing::info!(
                    "段过轻（{secs:.1}s，RMS {level:.4} < {:.4}，切分 {cut}），疑似静音，跳过",
                    gate.unwrap_or_default()
                );
                return SegOutcome::Quiet(secs);
            }
            SegVerdict::Accept => {}
        }
        self.set_state(State::Transcribing);
        let t0 = Instant::now();
        let out = match self.transcribe(samples) {
            Ok(t) if !t.trim().is_empty() => {
                // 幻觉守卫（止血）：碎片/噪声段上 ASR 会崩成“同字符堆叠”（实测 3/58 段，
                // 全集中在连点会话——音频被按键切碎）。根因是“碎片音频不该送 ASR”，= C1。
                if let Some((ch, run)) = text::repeat_hallucination(t.trim()) {
                    tracing::warn!(
                        "丢弃重复堆叠幻觉（'{ch}' 连续 {run} 次，{secs:.1}s 段）: {}",
                        preview(t.trim())
                    );
                    return SegOutcome::Halluc(secs);
                }
                // vibecoding 术语纠错（欧拉马→Ollama）+ 去句读标点（保留 ITN 数字）
                let t = self.corrector.correct(t.trim());
                let t = if self.text_norm.strip_punctuation {
                    text::strip_punctuation(&t)
                } else {
                    t
                };
                if t.is_empty() {
                    // 整段都是标点，无可注入内容
                    SegOutcome::Empty
                } else if self.cfg.general.drop_short_english && secs < 1.2 && is_ascii_english(&t)
                {
                    // 噪声门控：环境音常被 SenseVoice 误判为"纯英文短词"（Okay./Yeah.）
                    tracing::info!("丢弃疑似环境音（{secs:.1}s, 纯英文）: {}", preview(&t));
                    SegOutcome::Quiet(secs)
                } else {
                    let chars = t.chars().count();
                    self.last_asr_ms = Some(t0.elapsed().as_millis() as u64);
                    tracing::info!(
                        "识别({:.0}ms, {:.1}s, 切分 {cut}): {}",
                        t0.elapsed().as_millis(),
                        secs,
                        preview(&t)
                    );
                    let _ = self.evt.send(Evt::Segment { text: t.clone() });
                    if !pending.is_empty() {
                        pending.push_str(&self.cfg.inject.chunk_separator);
                    }
                    pending.push_str(&t);
                    SegOutcome::Text(chars)
                }
            }
            Ok(_) => SegOutcome::Empty,
            Err(e) => {
                tracing::error!("识别失败: {e:#}");
                let _ = self.evt.send(Evt::Note(format!("识别失败: {e}")));
                SegOutcome::Failed
            }
        };
        if self.capture.is_some() {
            self.set_state(State::Recording);
        }
        out
    }

    /// 把已识别的原文交给"输出线程"做整理 + 注入。**立即返回**，不阻塞录音线程，
    /// 因此会话一结束就能马上开始下一次录音（整理/注入在后台串行执行）。
    fn flush_pending(&mut self, pending: &mut String, _keep_recording: bool) {
        if pending.trim().is_empty() {
            pending.clear();
            return;
        }
        let raw = std::mem::take(pending);
        // 取原文（Shift+F9）针对「最近一次注入的完整原文」，而不是最后一段
        self.last_raw = Some(raw.clone());
        let polish_on = self.polisher.name() != "关闭";
        let job = OutputJob {
            raw,
            polish: polish_on,
            replace_raw: false,
            polisher: self.polisher.clone(),
            inject: self.cfg.inject.clone(),
            strip_punctuation: self.text_norm.strip_punctuation,
            trailing_space: self.text_norm.trailing_space,
        };
        if self.out_tx.send(job).is_err() {
            tracing::error!("输出线程已退出，本次未能注入");
        }
    }

    /// 识别（云端失败自动回退本地）
    fn transcribe(&mut self, samples: &[f32]) -> Result<String> {
        let backend = self
            .asr
            .as_ref()
            .context("识别后端未初始化（检查 config.toml 与本地模型）")?;
        let is_cloud = self.cfg.asr.backend == "cloud";
        match backend.transcribe(samples, audio::TARGET_RATE) {
            Ok(t) => Ok(t),
            Err(e) if is_cloud => {
                tracing::warn!("云端 ASR 失败，尝试回退本地: {e:#}");
                let mut local_cfg = self.cfg.clone();
                local_cfg.asr.backend = "local".into();
                if !asr::local_model_ready(&self.paths, &local_cfg) {
                    return Err(e);
                }
                let local = asr::build(&local_cfg, &self.paths)?;
                let t = local.transcribe(samples, audio::TARGET_RATE)?;
                self.asr = Some(local);
                self.cfg.asr.backend = "local".into();
                self.persist();
                let _ = self.evt.send(Evt::Note("云端不可用，已回退本地".into()));
                Ok(t)
            }
            Err(e) => Err(e),
        }
    }

    fn rebuild_asr(&mut self) {
        self.asr = None;
        match asr::build(&self.cfg, &self.paths) {
            Ok(b) => {
                tracing::info!("识别后端切换为: {}", b.name());
                self.asr = Some(b);
                self.engine.set(ctl::ST_READY);
                let _ = self
                    .evt
                    .send(Evt::Note(format!("识别后端: {}", self.cfg.asr.backend)));
            }
            Err(e) => {
                tracing::error!("识别后端切换失败: {e:#}");
                self.engine.set(ctl::ST_NOT_RUNNING);
                let _ = self.evt.send(Evt::Note(format!("后端不可用: {e}")));
            }
        }
    }

    /// 引擎总开关 OFF：停会话、卸模型、摘热键、置 disabled、同步标志文件
    fn voice_off(&mut self) {
        if !self.voice_enabled {
            return;
        }
        self.voice_enabled = false;
        self.drop_capture("引擎关闭");
        self.asr = None;
        self.asr_loader = None;
        self.uninstall_hook();
        self.engine.set(ctl::ST_DISABLED);
        sync_voice_flag(&self.paths, false);
        if self.state != State::Idle {
            self.set_state(State::Idle);
        }
        tracing::info!("语音引擎已关闭（总开关 OFF）：模型已释放、热键已摘、托管不再拉起");
    }

    /// 引擎总开关 ON：清标志文件、**重装热键**、异步加载模型
    ///
    /// ⚠️ 2026-10-09 修：旧实现在 `suspended` 时直接 return（“等 RESUME 装钩子并加载模型”），
    /// 但 2026-10-06 起挂起**不再摘钩子**（挂起只表示“输入法不在前台”），
    /// 于是“OFF → ON”如果恰好处于挂起态，钩子就再也装不回来 → **按 F9 毫无反应**（实测踩到）。
    /// 现在：不管挂不挂起都装钩子；加载模型后只把**状态**设成 suspended/ready。
    fn voice_on(&mut self) {
        if self.voice_enabled {
            return;
        }
        self.voice_enabled = true;
        sync_voice_flag(&self.paths, true);
        self.models_released_at = None;
        self.asr_released = false;
        self.asr_released_min = 0;
        self.install_hook();
        let ready_state = if self.suspended {
            ctl::ST_SUSPENDED
        } else {
            ctl::ST_READY
        };
        if self.asr.is_some() {
            self.engine.set(ready_state);
            if self.prewarm {
                if let Err(e) = self.ensure_capture("引擎开启预热") {
                    tracing::warn!("开启预热录音失败: {e:#}");
                }
            }
            tracing::info!(
                "语音引擎已开启：热键已装、模型常驻{}",
                if self.suspended { "（挂起中）" } else { "" }
            );
        } else {
            self.spawn_asr_load(); // 加载完会按 suspended 自己选 ready/suspended
            tracing::info!("语音引擎已开启：热键已装、模型异步加载中");
        }
    }

    /// 重装热键钩子（幂等；引擎开启/恢复时用）
    fn install_hook(&mut self) {
        #[cfg(windows)]
        {
            let Ok(mut g) = self.hook.lock() else { return };
            if g.is_some() {
                return;
            }
            let spec = hotkey::parse(&self.cfg.general.hotkey).unwrap_or_else(|e| {
                tracing::warn!("热键配置无效，已回退 f9：{e:#}");
                hotkey::parse("f9").expect("默认热键 f9 必定可解析")
            });
            self.hotkey = spec.clone();
            match hotkey::spawn(&spec, self.hk_tx.clone()) {
                Ok(h) => {
                    *g = Some(h);
                    tracing::info!("热键钩子已重装: {}", spec.name);
                }
                Err(e) => tracing::error!("热键钩子重装失败: {e:#}"),
            }
        }
    }

    /// 摘热键钩子（引擎关闭时；等钩子线程退出，避免与新钩子并存）
    fn uninstall_hook(&mut self) {
        #[cfg(windows)]
        {
            if let Ok(mut g) = self.hook.lock() {
                if let Some(h) = g.take() {
                    h.stop_and_wait();
                }
            }
        }
    }

    fn persist(&mut self) {
        if let Err(e) = self.cfg.save(&self.paths) {
            tracing::warn!("保存配置失败: {e:#}");
        } else {
            // 自己写的别让 config 监视器当成“外部改动”再触发一次热更新
            self.cfg_stamp = cfg_stamp(&self.paths.config_file);
        }
        let _ = self.evt.send(Evt::Config(Box::new(self.cfg.clone())));
    }

    fn set_state(&mut self, s: State) {
        if self.state == s {
            return;
        }
        self.state = s;
        // 托管托盘要能显示“录音中”（2026-10-08）：`Transcribing` 也算还在会话里，
        // 只在 Idle/Error 时熄灭 —— 否则一段一段识别时托盘会一闪一闪。
        // 注意这里**不写日志**：会话本来就一秒钟切好几段，日志会被刷屏。
        self.engine
            .set_recording(matches!(s, State::Recording | State::Transcribing));
        let _ = self.evt.send(Evt::State(s));
    }

    /// 退出回收：只停自己拉起的 llama-server
    fn shutdown(&self) {
        if self.cfg.polish.local.stop_on_exit {
            self.llm.stop();
        }
    }

    fn fail(&mut self, msg: &str) {
        tracing::error!("{msg}");
        let _ = self.evt.send(Evt::Note(msg.to_string()));
        self.set_state(State::Error);
        std::thread::sleep(Duration::from_millis(1200));
        self.set_state(State::Idle);
    }
}

/// 总开关标志文件路径：listen.exe 同级（fork 侧 spawn 前检查）
fn voice_flag_path(paths: &Paths) -> PathBuf {
    paths.root.join("voice_off.flag")
}

/// 同步 `voice_off.flag`：OFF 写、ON 删（fork 据此跳过托管拉起）
fn sync_voice_flag(paths: &Paths, enabled: bool) {
    let p = voice_flag_path(paths);
    if enabled {
        if p.exists() {
            if let Err(e) = std::fs::remove_file(&p) {
                tracing::warn!("删除 voice_off.flag 失败: {e}");
            }
        }
    } else if let Err(e) = std::fs::write(&p, b"voice disabled\n") {
        tracing::warn!("写 voice_off.flag 失败（fork 可能仍会拉起）: {e}");
    }
}

fn make_llm(cfg: &Config, paths: &Paths, evt: &Sender<Evt>) -> Arc<LlamaServer> {
    let evt2 = evt.clone();
    let server = LlamaServer::new(&cfg.polish.local, paths).with_sink(Arc::new(move |s: LlmState| {
        let _ = evt2.send(Evt::LlmState(s));
    }));
    // 上一次运行被强杀时 llama-server 会变成孤儿（实测常驻 2.8GB）。开机先收掉它，
    // 免得既复用了端口又挂着一个吃内存的僵尸。
    if cfg.polish.enabled && cfg.polish.provider == "local" {
        server.reap_orphan();
    }
    Arc::new(server)
}

/// 按 (enabled, provider, mode) 分发整理后端
fn build_polisher(
    cfg: &Config,
    paths: &Paths,
    llm: &Arc<LlamaServer>,
) -> Arc<dyn Polisher + Send + Sync> {
    if !cfg.polish.enabled {
        return Arc::new(NoopPolisher);
    }
    // 术语保留：热词包 + 用户 glossary（整理 prompt 里最多取前 60 条，按权重序）
    let terms: Vec<String> = crate::hotwords::asr_terms(cfg, paths);
    let terms: &[String] = &terms;
    if cfg.polish.provider == "local" {
        match polish::LocalPolisher::new(llm.clone(), &cfg.polish, terms) {
            Ok(p) => Arc::new(p),
            Err(e) => {
                tracing::warn!("本地整理后端初始化失败，已关闭: {e:#}");
                Arc::new(NoopPolisher)
            }
        }
    } else {
        match polish::CloudPolisher::new(&cfg.polish, terms) {
            Ok(p) => Arc::new(p),
            Err(e) => {
                tracing::warn!("整理后端初始化失败，已关闭: {e:#}");
                Arc::new(NoopPolisher)
            }
        }
    }
}

fn preview(s: &str) -> String {
    let p: String = s.chars().take(40).collect();
    if s.chars().count() > 40 {
        format!("{p}…")
    } else {
        p
    }
}

/// 是否"纯英文"文本（仅 ASCII 字母/数字/标点，且至少一个字母；无 CJK）。
/// 用于丢弃环境音被误判出的英文短词。
fn is_ascii_english(t: &str) -> bool {
    let mut has_letter = false;
    for c in t.chars() {
        if c.is_ascii_alphabetic() {
            has_letter = true;
        } else if c.is_ascii_digit() || c.is_ascii_whitespace() || c.is_ascii_punctuation() {
            // 允许
        } else {
            return false;
        }
    }
    has_letter
}

// ---------------------------------------------------------------- 辅助

/// `--gate` 的单文件累计量。
///
/// 原先是 `(出字段数, 被闸段, 字数, ASR毫秒)` 四元组，但 2026-10-08 发现最关键的那个数
/// 根本不在里面：**键盘/噪声段实际上全都穿过了能量门、被送进了 ASR**（`asr_ms > 0`），
/// 只是 Paraformer 恰好返回空串（没有字符、也不算“被闸”），报告于是显示
/// `segs=0 skip=1`，看起来“门控拦住了”—— 实际上拦不住，只是运气好。
/// C1（VAD）要压的就是 `asr_segments`（暴露面），所以必须把它单独立出来报。
#[derive(Default)]
struct GateAcc {
    /// 切出的段总数
    segments: usize,
    /// 被 `segment_verdict` 闸下、**没送 ASR** 的段数
    skipped: usize,
    /// 送进 ASR 的段数（= 幻觉暴露面）
    asr_segments: usize,
    /// 出字段数（ASR 返回非空且没被幻觉守卫丢掉）
    text_segments: usize,
    /// 出字数
    chars: usize,
    /// ASR 累计耗时（ms）
    asr_ms: f32,
    /// 被重复堆叠幻觉守卫丢掉的段数
    halluc: usize,
    /// 收尾段“语音证据不足”被丢弃的次数（不送 ASR）
    tail_dropped: usize,
}

impl GateAcc {
    /// 按类别累加（`--gate` 的 SUM 行用）
    fn merge(&mut self, o: &GateAcc) {
        self.segments += o.segments;
        self.skipped += o.skipped;
        self.asr_segments += o.asr_segments;
        self.text_segments += o.text_segments;
        self.chars += o.chars;
        self.asr_ms += o.asr_ms;
        self.halluc += o.halluc;
        self.tail_dropped += o.tail_dropped;
    }
}

/// `--gate <DIR>`：门控/分段回归跑分（不注入、不用麦克风）。
///
/// 走**与真实会话同一套判据**（`Segmenter` + `segment_verdict`），只是输入换成 wav。
/// 目录约定 `DIR/<类别>/<名字>.wav`（也接受 `DIR/*.wav`，归到 `_root` 类）。
/// 输出：`#` 头行 + 每文件一行 TSV + 按类别的 `SUM` 行——供脚本汇总/对比基线。
fn run_gate(paths: &Paths, cfg: &Config, dir: &Path) -> Result<()> {
    fn drain(
        cfg: &Config,
        seg: &mut dyn gate::SpeechJudge,
        backend: &dyn asr::AsrBackend,
        acc: &mut GateAcc,
        text: &mut String,
    ) -> Result<()> {
        while let Some(s) = seg.take_segment() {
            acc.segments += 1;
            let gate = seg.gate_hint();
            let (v, _, _) = segment_verdict(cfg, &s, gate, seg.saw_speech());
            if v != SegVerdict::Accept {
                acc.skipped += 1;
                continue;
            }
            acc.asr_segments += 1;
            let t0 = Instant::now();
            let t = backend.transcribe(&s, audio::TARGET_RATE)?;
            acc.asr_ms += t0.elapsed().as_secs_f32() * 1000.0;
            let t = t.trim();
            if t.is_empty() {
                continue;
            }
            // 与真实会话同一道幻觉守卫（保证基线跑的是线上行为）
            if crate::text::repeat_hallucination(t).is_some() {
                acc.halluc += 1;
                continue;
            }
            acc.text_segments += 1;
            acc.chars += t.chars().count();
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(t);
        }
        Ok(())
    }

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for e in std::fs::read_dir(dir).with_context(|| format!("读目录失败: {}", dir.display()))? {
        let p = e?.path();
        if p.is_dir() {
            let cls = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let mut v: Vec<PathBuf> = std::fs::read_dir(&p)
                .map(|rd| rd.flatten().map(|x| x.path()).collect())
                .unwrap_or_default();
            v.retain(|x| x.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")));
            v.sort();
            for f in v {
                files.push((cls.clone(), f));
            }
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")) {
            files.push(("_root".into(), p));
        }
    }
    if files.is_empty() {
        anyhow::bail!("{} 下没找到 wav（约定 DIR/<类别>/*.wav）", dir.display());
    }
    files.sort();

    let backend = asr::build(cfg, paths)?;
    // 判据只建一次（VAD 模型只加载一次），各文件前 reset；名字写进头行，
    // 否则“VAD 失败静默回退能量门”会让两次跑分看起来同名、无法归因
    let mut seg = gate::build(cfg, paths);
    println!(
        "# 门控基线 | 引擎={} | 门控={} | 分段目标={}s | 静音断句={}s | min_speech_rms={} | min_record={}s | 目录={}",
        backend.name(),
        seg.name(),
        cfg.general.segment_seconds,
        cfg.general.silence_seconds,
        cfg.general.min_speech_rms,
        cfg.general.min_record_seconds,
        dir.display()
    );
    println!("cls\tfile\tsecs\tcuts\tskipped\tasr_segs\tsegs\tchars\tasr_ms\thalluc\ttail_dropped\ttext");

    // cls -> (文件数, 音频秒, 出字段, 被闸段, 字, ASR 毫秒)
    let mut agg: std::collections::BTreeMap<String, (usize, f32, GateAcc)> =
        std::collections::BTreeMap::new();

    for (cls, path) in &files {
        let samples = audio::read_wav_16k(path)?;
        let secs = samples.len() as f32 / audio::TARGET_RATE as f32;
        // 与线上同一条判据选择路径：gate_engine=vad 就用 VAD（跑分才代表线上行为）
        seg.reset();
        let mut acc = GateAcc::default();
        let mut text = String::new();

        // 100ms 步进喂入，与实时采集同节奇
        let step = 1600usize;
        let mut i = 0usize;
        while i < samples.len() {
            let end = (i + step).min(samples.len());
            seg.push(&samples[i..end]);
            i = end;
            drain(cfg, seg.as_mut(), backend.as_ref(), &mut acc, &mut text)?;
        }
        // 收尾：flush 出的尾段也走同一判据（与真实会话一致）
        let pending_before = seg.pending_len();
        if let Some(tail) = seg.flush() {
            acc.segments += 1;
            let gate = seg.gate_hint();
            let (v, _, _) = segment_verdict(cfg, &tail, gate, seg.saw_speech());
            if v == SegVerdict::Accept {
                acc.asr_segments += 1;
                let t0 = Instant::now();
                let t = backend.transcribe(&tail, audio::TARGET_RATE)?;
                acc.asr_ms += t0.elapsed().as_secs_f32() * 1000.0;
                let t = t.trim();
                if !t.is_empty() {
                    // 尾段历史上**没走**幻觉守卫（线上走了）→ 基线跑在分叉路径上，一并补上
                    if crate::text::repeat_hallucination(t).is_some() {
                        acc.halluc += 1;
                    } else {
                        acc.text_segments += 1;
                        acc.chars += t.chars().count();
                        if !text.is_empty() {
                            text.push(' ');
                        }
                        text.push_str(t);
                    }
                }
            } else {
                acc.skipped += 1;
            }
        } else if pending_before > 0 {
            // 有内容但“语音证据不足”——被丢弃，没进 ASR（与 segs=0 的“没东西可收尾”区分开）
            acc.tail_dropped += 1;
        }

        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        // text 里的制表符/换行会破坏 TSV，替掉
        let flat = text.replace(['\t', '\n', '\r'], " ");
        println!(
            "{cls}\t{name}\t{secs:.2}\t{}\t{}\t{}\t{}\t{}\t{:.0}\t{}\t{}\t{flat}",
            acc.segments,
            acc.skipped,
            acc.asr_segments,
            acc.text_segments,
            acc.chars,
            acc.asr_ms,
            acc.halluc,
            acc.tail_dropped
        );
        let e = agg
            .entry(cls.clone())
            .or_insert_with(|| (0, 0.0, GateAcc::default()));
        e.0 += 1;
        e.1 += secs;
        e.2.merge(&acc);
    }

    for (cls, (n, secs, a)) in &agg {
        println!(
            "SUM\t{cls}\tfiles={n}\tsecs={secs:.1}\tcuts={}\tskipped={}\tasr_segs={}\tsegs={}\tchars={}\tasr_ms={:.0}\thalluc={}\ttail_dropped={}\tchars_per_seg={:.1}",
            a.segments,
            a.skipped,
            a.asr_segments,
            a.text_segments,
            a.chars,
            a.asr_ms,
            a.halluc,
            a.tail_dropped,
            if a.text_segments > 0 {
                a.chars as f32 / a.text_segments as f32
            } else {
                0.0
            }
        );
    }
    Ok(())
}

/// `--record <WAV>`：按住热键录一段，落盘 16k 单声道 wav 后退出。
///
/// 与正式会话**走同一条采集链路**（cpal → 多声道降单声道 → 流式重采样 16k），
/// 所以录出来的样本能代表真实使用条件（这正是噪声回归集的价值）。
fn run_record(_paths: &Paths, cfg: &Config, out: &Path) -> Result<()> {
    let cap = audio::StreamCapture::start(&cfg.general.device).context("打开录音设备失败")?;
    let rate = cap.rate();
    println!("设备采样率 {rate}Hz → 输出 16k 单声道");

    let spec = hotkey::parse(&cfg.general.hotkey).unwrap_or_else(|_| {
        hotkey::parse("f9").expect("默认热键 f9 必定可解析")
    });
    let (tx, rx) = mpsc::channel::<(HotkeyEvent, u64)>();
    #[cfg(windows)]
    let hook = hotkey::spawn(&spec, tx).context("安装热键失败")?;
    #[cfg(not(windows))]
    let hook = {
        hotkey::spawn(&spec, tx).context("安装热键失败")?;
    };

    println!("按住「{}」开始录，松手结束（60s 上限）。", spec.name);
    println!("提示：若托盘里已有倾听输入法在跑，同一次按下也会触发它 —— 录样本前建议先退出它。");

    // 等按下（120s 内没动静就退出，避免脚本挂住）
    let wait_deadline = Instant::now() + Duration::from_secs(120);
    let mut started = false;
    while Instant::now() < wait_deadline {
        if let Ok((HotkeyEvent::Press, _)) = rx.recv_timeout(Duration::from_millis(200)) {
            started = true;
            break;
        }
    }
    if !started {
        anyhow::bail!("等待热键超时（120s 内没有按下「{}」）", spec.name);
    }
    // 从**按下瞬间**（含预热 pre-roll）开始切片，和正式会话一致，起头不丢
    let start_ms = audio::press_ms();
    println!("● 录音中…（松手结束）");

    // ★ 必须**边录边取**：环形缓冲只有 RING_SECONDS(3s)，只到最后取一次的话，
    // 超过 3s 的部分会被后来的音频覆盖 —— 录出来的永远是「最后 3 秒」，开头直接丢，
    // 而且样本长度全卡在 3.00s（2026-10-08 实测：按住 5s 的 10 条样本全部恰好 3.00s，
    // 于是 <3s 的纯噪声样本连一次切段都触发不了，回归集等于空转）。
    // 正式会话是 worker 每 40ms 取一次，这里照做。
    let (mut raw, _end_ms, dropped) = cap.drain_from_ms(start_ms);
    let hard = Instant::now() + Duration::from_secs(60);
    loop {
        match rx.recv_timeout(Duration::from_millis(30)) {
            Ok((HotkeyEvent::Release, _)) => break,
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() >= hard {
                    println!("（达到 60s 上限，自动结束）");
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        let (chunk, _) = cap.drain_all();
        raw.extend_from_slice(&chunk);
    }
    // 松手后再收 300ms 尾音（与正式会话的 RELEASE_GRACE 一致）
    std::thread::sleep(Duration::from_millis(300));
    let (tail, _) = cap.drain_all();
    raw.extend_from_slice(&tail);
    if dropped > 0 {
        println!("！！环形缓冲已覆盖 {dropped}ms（开头可能缺失，建议重录）");
    }
    cap.stop();
    drop(hook);

    let mut mono = Vec::new();
    let mut rs = audio::StreamResampler::new(rate, audio::TARGET_RATE);
    rs.push(&raw, &mut mono);
    rs.flush(&mut mono);
    if mono.is_empty() {
        anyhow::bail!("没录到任何音频");
    }

    let secs = mono.len() as f32 / audio::TARGET_RATE as f32;
    let peak = mono.iter().fold(0f32, |a, &b| a.max(b.abs()));
    let level = audio::rms(&mono);

    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let bytes = audio::encode_wav(&mono, audio::TARGET_RATE)?;
    std::fs::write(out, &bytes).with_context(|| format!("写 wav 失败: {}", out.display()))?;

    println!(
        "已写出 {}（{secs:.2}s / {} 样本 / RMS {level:.5} / 峰值 {peak:.4}）",
        out.display(),
        mono.len()
    );
    println!("采样提示：纯底噪 3~5s；键盘敲击 3~5s；轻声说话说满一句；正常说话说满一句。");
    Ok(())
}

fn run_once(paths: &Paths, cfg: &Config, wav: &Path) -> Result<()> {
    let samples = audio::read_wav_16k(wav)?;
    let dur = samples.len() as f32 / audio::TARGET_RATE as f32;
    let backend = asr::build(cfg, paths)?;
    let t0 = Instant::now();
    let text = backend.transcribe(&samples, audio::TARGET_RATE)?;
    let dt = t0.elapsed();
    println!("后端: {}", backend.name());
    println!(
        "音频: {dur:.2}s  识别: {:.2}s  RTF: {:.3}",
        dt.as_secs_f32(),
        dt.as_secs_f32() / dur.max(0.01)
    );
    println!("RMS:  {:.4}", audio::rms(&samples));
    println!("文本: {text}");
    Ok(())
}

fn run_simulate(paths: &Paths, cfg: &Config, wav: &Path) -> Result<()> {
    let samples = audio::read_wav_16k(wav)?;
    let total = samples.len() as f32 / audio::TARGET_RATE as f32;
    println!(
        "音频总长 {total:.1}s | 分段目标 {}s | 注入间隔 {}s | 静音阈值 {}",
        cfg.general.segment_seconds, cfg.general.flush_seconds, cfg.general.min_speech_rms
    );
    println!("加载识别后端…");
    let backend = asr::build(cfg, paths)?;
    println!("后端: {}\n", backend.name());

    let (evt_tx, _evt_rx) = mpsc::channel::<Evt>();
    let llm = make_llm(cfg, paths, &evt_tx);
    if cfg.polish.enabled && cfg.polish.provider == "local" {
        println!("启动本地大模型…");
        llm.ensure_started()?;
    }
    let polisher = build_polisher(cfg, paths, &llm);
    println!("整理: {}\n", polisher.name());

    let mut seg = gate::build(cfg, paths);
    let mut pending = String::new();
    let mut flushes: Vec<String> = Vec::new();
    let mut last_flush_at = 0.0f32;
    let mut seg_idx = 0usize;
    let mut skipped = 0usize;

    let t0 = Instant::now();
    let step = 1600usize; // 100ms，模拟实时喂入
    let mut i = 0usize;
    while i < samples.len() {
        let end = (i + step).min(samples.len());
        seg.push(&samples[i..end]);
        let now_s = end as f32 / audio::TARGET_RATE as f32;

        while let Some(s) = seg.take_segment() {
            let dur = s.len() as f32 / audio::TARGET_RATE as f32;
            // 能量门只在能量判据下生效（VAD 模式 gate_hint = None）
            let too_quiet = matches!(seg.gate_hint(), Some(g) if audio::rms(&s) < g);
            if dur < cfg.general.min_record_seconds || too_quiet {
                skipped += 1;
                continue;
            }
            seg_idx += 1;
            let t = Instant::now();
            let text = backend.transcribe(&s, audio::TARGET_RATE)?;
            println!(
                "  [段{seg_idx}] {dur:.1}s 音频 → {:.0}ms 识别 → {} 字：{}",
                t.elapsed().as_millis(),
                text.chars().count(),
                text.trim()
            );
            if !pending.is_empty() {
                pending.push_str(&cfg.inject.chunk_separator);
            }
            pending.push_str(text.trim());
        }

        if now_s - last_flush_at >= cfg.general.flush_seconds as f32 && !pending.trim().is_empty() {
            let text = polish_or_raw(polisher.as_ref(), std::mem::take(&mut pending).trim());
            println!(
                "\n>>> 第 {} 次注入（音频 {now_s:.0}s 处）：{} 字\n    {}\n",
                flushes.len() + 1,
                text.chars().count(),
                text
            );
            flushes.push(text);
            last_flush_at = now_s;
        }
        i = end;
    }

    if let Some(s) = seg.flush() {
        let dur = s.len() as f32 / audio::TARGET_RATE as f32;
        if dur >= cfg.general.min_record_seconds && audio::rms(&s) >= cfg.general.min_speech_rms {
            let text = backend.transcribe(&s, audio::TARGET_RATE)?;
            println!("\n  [尾段] {dur:.1}s 音频 → {} 字：{}", text.chars().count(), text.trim());
            if !pending.is_empty() {
                pending.push_str(&cfg.inject.chunk_separator);
            }
            pending.push_str(text.trim());
        }
    }
    if !pending.trim().is_empty() {
        let text = polish_or_raw(polisher.as_ref(), std::mem::take(&mut pending).trim());
        println!(
            "\n>>> 最后一次注入：{} 字\n    {}\n",
            text.chars().count(),
            text
        );
        flushes.push(text);
    }

    let dt = t0.elapsed().as_secs_f32();
    println!(
        "===== 共切 {} 段（跳过 {} 段静音）→ {} 次注入；总耗时 {:.1}s，RTF {:.3} =====",
        seg_idx,
        skipped,
        flushes.len(),
        dt,
        dt / total.max(0.01)
    );
    Ok(())
}

/// 整理（关闭时直通；失败/超时回退原文），并打印耗时
fn polish_or_raw(p: &dyn Polisher, text: &str) -> String {
    if p.name() == "关闭" || text.is_empty() {
        return text.to_string();
    }
    let t = Instant::now();
    match p.polish(text) {
        Ok(s) if !s.trim().is_empty() => {
            println!("    [整理 {:.0}ms] {}", t.elapsed().as_millis(), s.trim());
            s
        }
        Ok(_) => {
            println!("    [整理] 返回空，已用原文");
            text.to_string()
        }
        Err(e) => {
            println!("    [整理失败 {:.0}ms] {e}", t.elapsed().as_millis());
            text.to_string()
        }
    }
}

/// 单条文本走完整整理链路（不用麦克风）：listen.exe --polish "嗯 那个 ..."
fn run_polish_cli(paths: &Paths, cfg: &Config, text: &str) -> Result<()> {
    let server = Arc::new(LlamaServer::new(&cfg.polish.local, paths));
    println!("llama-server: {}", cfg.polish.local.exe);
    println!("模型: {}", cfg.polish.local.model);

    let was_alive = server.alive();
    let t0 = Instant::now();
    server.ensure_started()?;
    let start_ms = t0.elapsed().as_millis();
    println!(
        "启动: {}（{}ms）  状态: {}",
        if was_alive { "复用已有实例" } else { "本程序拉起" },
        start_ms,
        server.state().label()
    );

    let polisher = polish::LocalPolisher::new(server.clone(), &cfg.polish, &cfg.glossary.terms)?;
    let t1 = Instant::now();
    let out = polisher.polish(text)?;
    println!("整理耗时: {}ms", t1.elapsed().as_millis());
    println!("原文: {text}");
    println!("结果: {out}");
    if was_alive {
        println!("复用已有实例，未回收");
    } else {
        server.stop();
        println!("已回收本程序拉起的实例");
    }
    Ok(())
}

/// 检测 Rime 环境（只读，不改动任何东西）
/// `--status`：打印全链路状态（自动化排查用）
///
/// - listen **正在跑** → 走 ListenCtl `CHAIN`，拿到的是**进程内实时快照**（与设置页卡同源、同判级）
/// - listen **没跑** → 只能静态探测（总开关 / 预热档位 / 门控 / 模型文件），并明确说明此刻按 F9 无反应
fn run_status(paths: &Paths, cfg: &Config, json: bool) -> Result<()> {
    #[cfg(windows)]
    if let Some(reply) = ctl::query("CHAIN", 800) {
        let body = reply.strip_prefix("OK ").unwrap_or(&reply);
        let snapshot = Chain::from_wire(body);
        print!(
            "{}",
            if json {
                snapshot.json()
            } else {
                snapshot.report()
            }
        );
        return Ok(());
    }

    // listen 不在运行：静态探测（拿不到钩子/麦克风/会话这些运行时信息）
    let model_ok = asr::local_model_ready(paths, cfg);
    let model_dir = paths.resolve(&cfg.asr.local_model);
    if json {
        println!(
            "{}",
            serde_json::json!({
                "running": false,
                "voice_enabled": cfg.general.voice_enabled,
                "prewarm": cfg.general.prewarm_label(),
                "gate_engine": cfg.general.gate_engine,
                "asr_backend": cfg.asr.backend,
                "asr_model": cfg.asr.model_type,
                "asr_model_dir": model_dir.display().to_string(),
                "asr_model_ready": model_ok,
                "polish": cfg.polish.tier(),
                "logs_dir": paths.logs_dir().display().to_string(),
            })
        );
        return Ok(());
    }
    println!("倾听输入法 链路状态（{}）", chain::now_hms());
    println!("  ! listen 未运行：此刻按 F9 不会有反应（切到倾听输入法时由 fork 自动拉起）");
    println!(
        "  · 语音引擎总开关：{}",
        if cfg.general.voice_enabled {
            "开"
        } else {
            "关（不录音、不装钩子）"
        }
    );
    println!("  · 麦克风预热：{}（要不要省资源就看它）", cfg.general.prewarm_label());
    println!("  · 门控引擎：{}", cfg.general.gate_engine);
    println!(
        "  · 识别模型：{} · {} · {}（{}）",
        cfg.asr.backend,
        cfg.asr.model_type,
        if model_ok { "文件齐 ✓" } else { "文件缺失 ✗" },
        model_dir.display()
    );
    println!("  · 整理：{}", cfg.polish.tier());
    println!("  · 日志目录：{}", paths.logs_dir().display());
    Ok(())
}

fn run_rime_status(paths: &Paths, cfg: &Config) -> Result<()> {
    let res = paths.resources_dir(&cfg.rime);
    let info = rime::detect(&cfg.rime.user_dir);
    println!("资源目录: {}", res.display());
    println!("倾听输入法: {}", if info.installed { "已安装" } else { "未安装" });
    println!("版本: {}", if info.version.is_empty() { "（未知）" } else { &info.version });
    println!("安装目录: {}", info.install_dir.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "（未找到）".into()));
    println!("WeaselDeployer: {}", info.deployer.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "（未找到）".into()));
    println!("用户目录: {}{}", info.user_dir.display(), if info.user_dir_custom { "（自定义）" } else { "（默认）" });
    println!("主力语言: {}（配置值: {:?}）", cfg.rime.lang().label(), cfg.rime.primary_lang);
    println!("英文混输: {}  简繁: {}  语法模型: {}", cfg.rime.english_mixed, if cfg.rime.zh_trad { "繁" } else { "简" }, cfg.rime.grammar);
    if res.join("rime").is_dir() {
        let mut names: Vec<String> = Vec::new();
        for e in std::fs::read_dir(res.join("rime")).into_iter().flatten().flatten() {
            names.push(e.file_name().to_string_lossy().to_string());
        }
        names.sort();
        println!("资源内容: {}", names.join(", "));
    } else {
        println!("资源内容: 缺失（先跑 installer\\fetch_resources.ps1）");
    }
    Ok(())
}

/// 部署 Rime 的完整流程（CLI 与 GUI 共用）；`log` 收每步进度
pub fn rime_deploy_logged(
    paths: &Paths,
    cfg: &Config,
    allow_install: bool,
    log: &mut dyn FnMut(String),
) -> Result<Config> {
    let res = paths.resources_dir(&cfg.rime);
    log(format!("资源目录: {}", res.display()));
    let mut info = rime::detect(&cfg.rime.user_dir);
    log(format!(
        "倾听输入法: {} {}",
        if info.installed { "已安装" } else { "未安装" },
        info.version
    ));
    let mut installed_by_us = cfg.rime.weasel_installed_by_us;
    if !info.installed {
        if !allow_install {
            anyhow::bail!("倾听输入法未安装（勾选「自动安装倾听输入法」或手动装一次）");
        }
        let inst = res.join("rime").join("weasel-0.17.4.0-installer.exe");
        log("静默安装倾听输入法…（如弹出 UAC 请允许）".into());
        info = rime::install_silent(&inst)?;
        installed_by_us = true;
        log(format!("倾听输入法已安装: {}", info.version));
    }
    log(format!("用户目录: {}", info.user_dir.display()));

    let plan = rime::make_plan(&cfg.rime)?;
    log(format!(
        "主力语言: {} → 方案列表 {:?}",
        plan.lang.label(),
        plan.schema_list
    ));

    log("铺方案包…".into());
    for l in rime::copy_schemes(&res, &info.user_dir)? {
        log(l);
    }
    if cfg.rime.grammar && plan.grammar_schema.is_some() {
        if let Some(l) = rime::copy_grammar(&res, &info.user_dir)? {
            log(l);
        }
    }
    log("写热词…".into());
    for l in hotwords::ensure_user_packs(cfg, paths) {
        log(l);
    }
    // 先把 Rime 刚记下的学习日志导进「我打过的词」，再生成置顶表
    // （不能让用户点了重部署、结果表里还是没有刚学到的词）
    if cfg.hotwords.enabled && cfg.hotwords.learn {
        match learn::import_new(cfg, paths, false) {
            Ok(r) if !r.is_empty() => log(format!(
                "  用户词学习：新词 {} 条（次数 +{}）→ 共 {} 条",
                r.new_pairs, r.added_count, r.total
            )),
            Ok(_) => {}
            Err(e) => log(format!("  !! 导入学习日志失败: {e:#}")),
        }
    }
    // 必须在 copy_schemes 之后：copy_schemes 会把清单里「本轮源里没有」的条目当陈旧文件删掉
    let terms_written = match hotwords::write_rime_table(cfg, paths, &info.user_dir) {
        Ok(Some(l)) => {
            log(l);
            true
        }
        Ok(None) => {
            log("  热词表为空（热词全关或全无打字编码），跳过".into());
            false
        }
        Err(e) => {
            log(format!("  !! 写热词表失败: {e:#}"));
            false
        }
    };
    log("写配置…".into());
    // 用户词学习：先把 lua 钩子铺进用户目录，再把日志绝对路径 patch 给 lua
    let mut learn_path: Option<String> = None;
    if cfg.hotwords.enabled && cfg.hotwords.learn {
        let p = learn::log_path(cfg, paths);
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        match rime::write_learn_lua(&info.user_dir) {
            Ok(l) => {
                log(l);
                learn_path = Some(p.to_string_lossy().replace('\\', "/"));
                log(format!("  学习日志 → {}", learn_path.clone().unwrap_or_default()));
            }
            Err(e) => log(format!("  !! 铺学习钩子失败: {e:#}")),
        }
    }
    for l in rime::write_configs(
        &info.user_dir,
        &plan,
        &cfg.rime,
        terms_written,
        cfg.hotwords.prefix_completion,
        learn_path.as_deref(),
    )? {
        log(l);
    }

    log("开始部署（首次编译大词库可能要几分钟，请稍候）…".into());
    let summary = rime::deploy(&info, &plan.schema_list)?;
    log(summary.clone());

    let mut c = cfg.clone();
    c.rime.weasel_version = info.version.clone();
    c.rime.weasel_installed_by_us = installed_by_us;
    c.rime.last_deploy = summary.lines().take(8).collect::<Vec<_>>().join("\n");
    c.save(paths)?;
    Ok(c)
}

/// 部署 Rime（CLI 入口）
fn run_rime_deploy(paths: &Paths, cfg: &Config, allow_install: bool) -> Result<()> {
    let mut log = |m: String| println!("{m}");
    rime_deploy_logged(paths, cfg, allow_install, &mut log)?;
    Ok(())
}

/// 保守卸载：只删我们铺的文件，保留用户词库/配置
fn run_rime_uninstall(paths: &Paths, cfg: &Config) -> Result<()> {
    let res = paths.resources_dir(&cfg.rime);
    let info = rime::detect(&cfg.rime.user_dir);
    println!("用户目录: {}", info.user_dir.display());
    for l in rime::uninstall_our_files(&info.user_dir, &res) {
        println!("{l}");
    }
    println!(
        "用户词库/配置已保留；倾听输入法本体{}",
        if cfg.rime.weasel_installed_by_us {
            "由本程序安装，如需卸载请到「控制面板 → 程序和功能」"
        } else {
            "非本程序安装，未改动"
        }
    );
    Ok(())
}

fn list_devices() -> Result<()> {
    println!(
        "默认输入设备: {}",
        audio::default_input_device_name().unwrap_or_else(|| "（无）".into())
    );
    println!("\n全部输入设备（config.toml → general.device 可填名称片段）:");
    for (i, d) in audio::list_input_devices().iter().enumerate() {
        println!("  [{i}] {d}");
    }
    Ok(())
}

fn run_inject_test(paths: &Paths, cfg: &Config, text: &str) -> Result<()> {    println!("3 秒后注入，请立刻聚焦目标窗口（记事本等）…");
    std::thread::sleep(Duration::from_secs(3));
    let inj = inject::Injector::new(&cfg.inject);
    let t0 = Instant::now();
    inj.inject(text)?;
    println!(
        "已注入 {} 字，耗时 {:.2}s（root={}）",
        text.chars().count(),
        t0.elapsed().as_secs_f32(),
        paths.root.display()
    );
    Ok(())
}

pub fn open_path(p: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(p)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(p).spawn();
    }
}

/// 启动外部程序（可带参数），不等待
pub fn launch_exe(p: &Path, args: &[&str]) {
    let _ = std::process::Command::new(p).args(args).spawn();
}

fn init_logging(paths: &Paths, level: &str, console: bool) {
    use tracing_subscriber::prelude::*;
    let dir = paths.logs_dir();
    std::fs::create_dir_all(&dir).ok();
    let appender = tracing_appender::rolling::daily(&dir, "app.log");
    let (nb, guard) = tracing_appender::non_blocking(appender);
    std::mem::forget(guard);

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(nb)
        .with_ansi(false);

    if console {
        let console_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stdout);
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(file_layer)
            .with(console_layer)
            .try_init();
    } else {
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(file_layer)
            .try_init();
    }
}

/// GUI 子系统程序若从终端启动，挂到父控制台以便看到 CLI 输出
fn attach_parent_console() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Console::{
            ATTACH_PARENT_PROCESS, AttachConsole, GetConsoleWindow,
        };
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
            let _ = GetConsoleWindow();
        }
    }
}

/// 单实例：命名互斥体（返回 None 表示已有实例在跑）
///
/// 两个必须的细节（2026-10-06 实测）：
/// 1) `lpSecurityDescriptor = null` = NULL DACL（全员可访问）。默认 DACL 的对象
///    跨完整性级别（提权/非提权）打不开，实测返回「拒绝访问 0x80070005」；
/// 2) **任何**失败都当“已有实例”。原实现只在 `ERROR_ALREADY_EXISTS` 时退出，
///    拿到 ACCESS_DENIED 就「继续运行」→ 多实例各自装 F9 钩子、各自录音注入，
///    现象就是“同一句话被识别两遍/注入两遍”（11:13:39 日志里两套会话成对出现）。
#[cfg(windows)]
fn acquire_single_instance(retry: Duration) -> Result<Option<windows::Win32::Foundation::HANDLE>> {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::Security::SECURITY_ATTRIBUTES;
    use windows::Win32::System::Threading::CreateMutexW;
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: false.into(),
    };
    // 崩溃自启时给旧进程一点时间退出（`--retry-lock`）：否则新进程会撞上还没释放的互斥体，
    // 表现就是「崩溃后自动重启、但重启的进程立刻又退了」——等于没修。
    let deadline = std::time::Instant::now() + retry;
    loop {
        match unsafe {
            CreateMutexW(
                Some(&sa as *const SECURITY_ATTRIBUTES),
                true,
                windows::core::w!("Global\\ListenRust"),
            )
        } {
            Ok(h) => {
                let err = unsafe { GetLastError() };
                if err == ERROR_ALREADY_EXISTS {
                    // ⚠️ 必须**先关掉这次拿到的句柄**再重试（2026-10-20 实测踩到）：
                    // CreateMutex 在已存在时也会返回一个有效句柄，我们自己的这个句柄
                    // 会让互斥体对象一直活着 → 之后每次尝试都还是 ALREADY_EXISTS，
                    // 于是「等旧进程退出再接手」永远等不到（新进程 5 秒后自己退了）。
                    let _ = unsafe { windows::Win32::Foundation::CloseHandle(h) };
                    if std::time::Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(120));
                        continue;
                    }
                    return Ok(None);
                }
                return Ok(Some(h));
            }
            Err(e) => {
                tracing::warn!("单实例互斥体不可用（视为已有实例在运行，本次退出）: {e}");
                return Ok(None);
            }
        }
    }
}

#[cfg(not(windows))]
fn acquire_single_instance(_retry: Duration) -> Result<Option<()>> {
    Ok(Some(()))
}

/// 崩溃现场文件名（exe 同级 `logs\`）
pub const PANIC_FILE: &str = "last_panic.txt";

/// 【仅测试】`LISTEN_PANIC_TEST=<秒>`：N 秒后在 worker 线程里 panic，用于验证崩溃自愈。
/// 生产环境不设这个变量，此函数返回 None（零开销、无行为变化）。
fn crash_test_after() -> Option<Duration> {
    std::env::var("LISTEN_PANIC_TEST")
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|s| *s > 0)
        .map(Duration::from_secs)
}

/// 崩溃自愈：panic 时写现场 + **自动拉起一个新进程**（2026-10-20）
///
/// 为什么需要（用户报「设置页面自己关了以后，整个语音引擎都不能用，只能在设置里重新打开语音」）：
/// 那次事故的日志里**一行收尾都没有**——没有 `收到 QUIT`、没有 `工作线程退出`、没有事件日志
/// WER 记录，最后一行是 21:56:34 的钩子心跳，然后直接断掉。这是「进程被无声带走」的典型样子
/// （Rust panic 不会写 app.log：日志是 tracing 异步写的，GUI 子系统也没有 stderr）。
/// 而 GUI 与录音引擎**同一个进程**：主线程一死，热键钩子/worker 全没了；fork 只在
/// 「输入法激活」时才拉进程，所以用户会遇到分钟级空窗，只能手动打开设置把引擎拉回来。
///
/// 这里做三件事：
/// 1. `panic` 钩子：**同步**写 `logs\last_panic.txt`（带线程名/位置/backtrace），崩溃现场不再丢；
/// 2. 主线程 / worker 线程崩溃 = 这个进程已经没用了 → 立刻拉起新实例（带 `--retry-lock`
///    等旧进程释放单实例互斥体），再退出；
/// 3. 只在第一轮崩溃时自启（子进程带 `LISTEN_RESPAWN=1`），避免「一启动就崩」的死循环；
///    第二轮仍崩就只留现场，不再纠缠。
fn install_crash_guard(
    paths: &Paths,
    config_arg: Option<std::path::PathBuf>,
    managed: bool,
) {
    use std::io::Write as _;
    let marker = paths.logs_dir().join(PANIC_FILE);
    let exe = std::env::current_exe().unwrap_or_default();
    let already_restarted = std::env::var("LISTEN_RESPAWN").is_ok();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "未知位置".into());
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "（无法读取 panic 信息）".to_string()
        };
        let thread = std::thread::current()
            .name()
            .unwrap_or("未命名线程")
            .to_string();
        let bt = std::backtrace::Backtrace::force_capture();
        let text = format!(
            "[{}] PANIC（线程 {thread}）\n位置: {loc}\n信息: {payload}\n\n{bt}\n",
            chain::now_hms()
        );
        // 同步写：主日志是异步 writer，进程被带走时可能一条都不落
        if let Some(dir) = marker.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&marker, text.as_bytes());
        // print 到 stderr（从终端启动时能直接看到）
        let _ = std::io::stderr().write_all(text.as_bytes());
        // 关键线程崩溃：进程已不可用 → 拉起新实例
        let fatal = thread == "main" || thread == "worker";
        if fatal {
            if already_restarted {
                let msg = "[crash] 已重启过一轮仍然崩溃 → 不再自启（现场见 logs\\last_panic.txt）\n";
                let _ = std::io::stderr().write_all(msg.as_bytes());
            } else if !exe.as_os_str().is_empty() {
                let mut cmd = std::process::Command::new(&exe);
                if let Some(p) = &config_arg {
                    cmd.arg("--config").arg(p);
                }
                if managed {
                    cmd.arg("--managed");
                }
                cmd.args(["--retry-lock", "5000"]);
                // 子进程打上标记：它再崩就不许再自启（防死循环）
                cmd.env("LISTEN_RESPAWN", "1");
                // 测试钩子只对第一个进程生效，别让自启的子进程也去故意崩
                cmd.env_remove("LISTEN_PANIC_TEST");
                match cmd.spawn() {
                    Ok(child) => {
                        let msg = format!(
                            "[crash] 已自动拉起新实例 pid={}（等旧进程释放单实例锁）\n",
                            child.id()
                        );
                        let _ = std::io::stderr().write_all(msg.as_bytes());
                    }
                    Err(e) => {
                        let msg = format!("[crash] 自动重启失败: {e}\n");
                        let _ = std::io::stderr().write_all(msg.as_bytes());
                    }
                }
            }
            // GUI 已经不可用，别等 unwind：越早退出，新实例接手越快
            std::process::exit(0);
        }
        // 非关键线程（钩子/tray/输出线程…）：只留现场，让进程继续跑
        default_hook(info);
    }));
}

/// 读「上次崩溃」现场：够新（15 分钟内）就返回一条给用户看的话，并把它改名留档
fn take_crash_note(paths: &Paths) -> Option<String> {
    let marker = paths.logs_dir().join(PANIC_FILE);
    let meta = std::fs::metadata(&marker).ok()?;
    let fresh = meta
        .modified()
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|d| d <= Duration::from_secs(15 * 60))
        .unwrap_or(false);
    if !fresh {
        return None;
    }
    let first = std::fs::read_to_string(&marker)
        .ok()
        .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "（详情见 logs\\last_panic.txt）".into());
    // 留档但不重复提示：改名成 .prev.txt（下次崩溃会重新写 last_panic.txt）
    let _ = std::fs::rename(&marker, paths.logs_dir().join("last_panic.prev.txt"));
    Some(format!(
        "上次运行异常退出（已自动重启）: {first} —— 详情见 logs\\last_panic.txt"
    ))
}

/// 命名事件名：跨进程「显示设置窗口」请求
#[cfg(windows)]
const SHOW_EVENT_NAME: windows::core::PCWSTR = windows::core::w!("Global\\ListenShow");

/// 创建「显示设置窗口」命名事件（常驻实例调用；返回裸句柄，便于跨线程传递）
///
/// 同样用 NULL DACL：否则提权实例创建的事件，非提权进程 `OpenEventW` 会被拒，
/// 表现就是“点了设置没反应”（§11.6 那个修复只盖了一半）。
#[cfg(windows)]
fn create_show_event() -> Option<isize> {
    use windows::Win32::Security::SECURITY_ATTRIBUTES;
    use windows::Win32::System::Threading::CreateEventW;
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: false.into(),
    };
    match unsafe {
        CreateEventW(Some(&sa as *const SECURITY_ATTRIBUTES), false, false, SHOW_EVENT_NAME)
    } {
        Ok(h) => Some(h.0 as isize),
        Err(e) => {
            tracing::warn!("创建 ListenShow 事件失败（外部唤起设置窗不可用）: {e}");
            None
        }
    }
}

#[cfg(not(windows))]
fn create_show_event() -> Option<isize> {
    None
}

/// 请求已有实例把设置窗口显示到前台（第二次启动 / `--settings` 调用）
#[cfg(windows)]
fn signal_show_settings() {
    use windows::Win32::System::Threading::{EVENT_MODIFY_STATE, OpenEventW, SetEvent};
    match unsafe { OpenEventW(EVENT_MODIFY_STATE, false, SHOW_EVENT_NAME) } {
        Ok(h) => {
            if let Err(e) = unsafe { SetEvent(h) } {
                tracing::warn!("SetEvent(ListenShow) 失败: {e}");
            }
        }
        Err(e) => tracing::warn!("打开 ListenShow 事件失败: {e}"),
    }
}

#[cfg(not(windows))]
fn signal_show_settings() {}

/// 开机自启：HKCU\...\Run 注册表项
#[cfg(windows)]
pub fn set_autostart(enable: bool) -> Result<()> {
    use windows::Win32::Foundation::{ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_CREATE_KEY_DISPOSITION, REG_OPEN_CREATE_OPTIONS,
        REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW,
    };
    use windows::core::w;

    let sub = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    let mut hkey = HKEY::default();
    let mut disp = REG_CREATE_KEY_DISPOSITION(0);
    let rc: WIN32_ERROR = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            sub,
            Some(0),
            None,
            REG_OPEN_CREATE_OPTIONS(0),
            KEY_SET_VALUE,
            None,
            &mut hkey,
            Some(&mut disp),
        )
    };
    if rc != ERROR_SUCCESS {
        anyhow::bail!("打开注册表 Run 项失败: {rc:?}");
    }

    let name = w!("Listen");
    let rc = if enable {
        let exe = std::env::current_exe().context("获取 exe 路径失败")?;
        // 自启用 --managed：静默常驻（不弹设置窗、不建托盘），由输入法激活驱动
        let quoted = format!("\"{}\" --managed", exe.display());
        let wide: Vec<u16> = quoted.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes =
            unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
        unsafe { RegSetValueExW(hkey, name, Some(0), REG_SZ, Some(bytes)) }
    } else {
        unsafe { RegDeleteValueW(hkey, name) }
    };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    if rc != ERROR_SUCCESS && rc.0 != 2 {
        anyhow::bail!("写入注册表失败: {rc:?}");
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn set_autostart(_enable: bool) -> Result<()> {
    anyhow::bail!("开机自启见二期（M6）")
}
