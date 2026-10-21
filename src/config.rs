//! 配置结构：exe 同级 config.toml（绿色），缺失则写默认。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub asr: Asr,
    pub polish: Polish,
    pub inject: Inject,
    pub rime: Rime,
    pub floating: Floating,
    pub text: TextNorm,
    pub glossary: Glossary,
    pub hotwords: Hotwords,
    pub bench: Bench,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: General::default(),
            asr: Asr::default(),
            polish: Polish::default(),
            inject: Inject::default(),
            rime: Rime::default(),
            floating: Floating::default(),
            text: TextNorm::default(),
            glossary: Glossary::default(),
            hotwords: Hotwords::default(),
            bench: Bench::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// 主热键：按住说话
    pub hotkey: String,
    /// 取原文热键：撤销并粘贴 ASR 原文
    pub raw_hotkey: String,
    /// hold = 按住说话；toggle = 按一下开始、再按一下停止（长语音用）
    pub hotkey_mode: String,
    /// 单次会话上限（分钟）
    pub max_session_minutes: u32,
    /// ASR 分段目标长度（秒）。SenseVoice 实测 ~25s 内可靠，超长会劣化；
    /// 默认取小值以获得"说完就上屏"的实时体验（配合 silence_seconds 静音断句）
    pub segment_seconds: u32,
    /// 尾部静音达到此秒数即提前断句（配合 segment_seconds；0 = 关闭）
    pub silence_seconds: f32,
    /// 丢弃"纯英文且很短"的识别结果（环境音常被误判为英文短词）
    pub drop_short_english: bool,
    /// 出字间隔（秒）：每满这么久就把已识别的文字**整理后上屏**一次
    ///
    /// 2026-10-20 起设置页给「每 10 / 20 / 40 / 60 秒」四档（[`FLUSH_GEARS`]）。
    /// 用户报的是「按住说了半天没字，要等 1 分钟或者松手才出」——老默认 60s 太长。
    /// 间隔越小越跟手（代价：整理后端调用更频繁、每批字数更少）。
    pub flush_seconds: u32,
    pub min_speech_rms: f32,
    /// 小于此时长的段直接丢弃
    pub min_record_seconds: f32,
    pub autostart: bool,
    pub log_level: String,
    /// 可选：指定录音设备名（模糊匹配），空=系统默认
    pub device: String,
    /// 托管模式（--managed）：挂起后 N 分钟无恢复自动退出；0 = 不自动退（默认 0）
    ///
    /// 默认 0 的原因：挂起**不再摘热键**（见 `ctl::do_suspend`），进程一退 F9 就彻底没响应，
    /// 而重新拉起只能等输入法激活（fork 侧），会出现分钟级空窗——实测最长 39 分钟。
    pub managed_suspend_exit_minutes: u32,
    /// 语音引擎总开关（持久化）：false = 停引擎 + 摘热键 + 托管不再拉起
    pub voice_enabled: bool,
    /// 预热录音：引擎就绪就把采集流常开，按下瞬间用环形缓冲的 pre-roll 补回起头丢字
    pub prewarm_capture: bool,
    /// 预热空闲自动关麦（分钟；0 = **永不关**，2026-10-09 起为默认）。关掉后下一次按下要吃一次起头延迟
    ///
    /// 为什么默认改成 0（永不关）：语音输入是「随时按随时说」的东西，空闲关麦的代价是
    /// **下一次按下必丢起头**（实测 `起头补 0ms`）——用户体感就是“偶尔第一句少字”，
    /// 而省下来的只是采集线程那点开销（实测见 README「麦克风常开的代价」）。
    /// 需要省资源的人在设置页选档位即可（6 选 1：只在按下时开麦 / 永不 / 3 / 5 / 10 / 30 分钟）。
    pub prewarm_idle_minutes: u32,
    /// 空闲多久后**释放模型**（分钟；0 = 永不释放，默认）—— 2026-10-09 新增（用户需求）
    ///
    /// “释放”= 卸载 ASR 识别模型（省 RAM）+ 停掉本程序拉起的 llama-server（省显存）。
    /// 代价：下次按下要重新加载识别模型（实测 ~1.8s，见 `asr_released`），
    /// 首次整理要冷启动大模型（10~25s，本次用原文）。
    /// 档位与麦克风预热同一套：永不 / 3 / 5 / 10 / 30 分钟（见 `MODEL_RELEASE_GEARS`）。
    pub model_release_minutes: u32,
    /// 托管模式（`--managed`，由输入法 fork 拉起）下**是否仍然显示自己的托盘图标**（默认 true）
    ///
    /// 为什么要它（2026-10-08）：托盘图标是「正在录音」最直接的反馈，而托管模式下原本
    /// `no_tray = cli.no_tray || cli.managed` 直接不建图标 —— 用户体感就是「按了不知道在不在录」。
    /// 这个图标是 listen.exe 自己画的**状态色点**（`src/tray.rs`，录音=红），与 fork（WeaselServer）
    /// 的输入法托盘图标互不相干；若 fork 那边也开了托盘图标（Rime 配置 `display_tray_icon: true`），
    /// 会有两个图标——不想要就把这里设 false（= 旧行为）。
    pub managed_tray: bool,
    /// 门控引擎（2026-10-08 C1）：`vad`（默认，Silero VAD 主判据 + 端点驱动切分）/ `energy`（旧能量门）
    ///
    /// VAD 模式下：`silence_seconds` = 尾部静音（VAD 的 min_silence_duration）、
    /// `segment_seconds` = **最长段上限**（max_speech_duration）；另两个旋钮见下面两项。
    /// VAD 不可用（模型缺失/加载失败）时会**自动回退能量门**并记 WARN，不会“按了没反应”。
    pub gate_engine: String,
    /// VAD 灵敏度阈值（0~1）：越大越难判为语音（Silero 默认 0.5）
    pub vad_threshold: f32,
    /// VAD 最短语音（秒）：短于它的碎片不当语音（Silero 默认 0.25）
    pub vad_min_speech_seconds: f32,
    /// GUI 渲染后端（2026-10-08）：`auto`（默认）= 优先集显，没有则独显；
    /// `gpu` = 强制独显；`software` = 强制 D3D12 软件光栅（WARP）。
    ///
    /// 为什么：设置窗/悬浮窗原先跑在独显上，整机 GPU/DWM 被别的程序占住时界面会定格
    /// 1~3.4s（专项复现不出来任何应用内成因，见 `项目调试过程.md` §19）。
    /// ⚠️ 本机无集显时 `auto` 仍是独显；`software` 能彻底避开独显争用，
    /// 但实测 CPU 代价 3~16 个核，不推荐常开。详见 `main.rs::gui_wgpu_options`。
    pub gui_render: String,
}

impl Default for General {
    fn default() -> Self {
        Self {
            hotkey: "f9".into(),
            raw_hotkey: "shift+f9".into(),
            hotkey_mode: "hold".into(),
            max_session_minutes: 30,
            segment_seconds: 3,
            silence_seconds: 0.5,
            drop_short_english: true,
            flush_seconds: 20,
            min_speech_rms: 0.0015,
            min_record_seconds: 0.2,
            autostart: false,
            log_level: "info".into(),
            device: String::new(),
            managed_suspend_exit_minutes: 0,
            voice_enabled: true,
            prewarm_capture: true,
            prewarm_idle_minutes: 0,
            model_release_minutes: 0,
            managed_tray: true,
            gate_engine: "vad".into(),
            vad_threshold: 0.5,
            vad_min_speech_seconds: 0.25,
            gui_render: "auto".into(),
        }
    }
}

/// 出字间隔档位（设置页 4 选 1；2026-10-20）
///
/// 用户原话：「要等 1 分钟或者是我结束录音他才会自动输出，这感觉不是很好不流畅，
/// 我希望提供一个选项让用户自己选每 10/20/40/60 秒输出一次」。
/// 语义是「每满这么久，就把这期间识别出来的文字整理后上屏一次」（会话结束仍会立刻吐完）。
pub const FLUSH_GEARS: [(u32, &str); 4] = [
    (10, "每 10 秒（最跟手，整理调用最频繁）"),
    (20, "每 20 秒（推荐，默认）"),
    (40, "每 40 秒"),
    (60, "每 60 秒（最省，长口述每 1 分钟才出一次字）"),
];

/// 悬浮窗停留时间档位（设置页 5 选 1；2026-10-20）
///
/// 用户原话：「我需要在设置中有一个选项能够实现语音输入悬浮框多久自动消失，
/// 现在太久不消失导致影响用户体验」。语义 = 会话结束后悬浮窗再停留多久（秒）。
pub const FLOAT_HIDE_GEARS: [(u32, &str); 5] = [
    (1, "1 秒（默认，一闪就走）"),
    (3, "3 秒"),
    (5, "5 秒"),
    (10, "10 秒"),
    (30, "30 秒（想回看文字时用）"),
];
/// 麦克风预热档位（界面 6 选 1 用；2026-10-09）
///
/// 索引约定（见 [`General::prewarm_gear`]）：
/// - `0` = **只在按下时开麦**（`prewarm_capture = false`，最省资源，代价：每次按下丢起头）
/// - `1..=5` = 预热常开 + `PREWARM_GEARS[i-1]` 分钟空闲后关麦（第 1 个 = 0 分钟 = **永不关**，默认档）
pub const PREWARM_GEARS: [(u32, &str); 5] = [
    (0, "永不关麦（推荐）"),
    (3, "空闲 3 分钟关麦"),
    (5, "空闲 5 分钟关麦"),
    (10, "空闲 10 分钟关麦"),
    (30, "空闲 30 分钟关麦"),
];

/// 模型释放档位（界面 5 选 1；2026-10-09）
///
/// 索引 = 下标（`MODEL_RELEASE_GEARS[i]`）；`None` = config.toml 里是自改的非常规值。
/// 与麦克风档位不同：这里没有“只在按下时开麦”那一档，基就是 5 项。
pub const MODEL_RELEASE_GEARS: [(u32, &str); 5] = [
    (0, "永不释放（推荐）"),
    (3, "空闲 3 分钟释放"),
    (5, "空闲 5 分钟释放"),
    (10, "空闲 10 分钟释放"),
    (30, "空闲 30 分钟释放"),
];

impl General {
    /// 当前预热档位索引；`None` = config.toml 里是自改的非常规值（界面额外显示「自定义」）
    pub fn prewarm_gear(&self) -> Option<usize> {
        if !self.prewarm_capture {
            return Some(0);
        }
        PREWARM_GEARS
            .iter()
            .position(|(m, _)| *m == self.prewarm_idle_minutes)
            .map(|i| i + 1)
    }

    /// 按档位索引写回两个字段（幂等；越界按“永不关麦”处理）
    pub fn set_prewarm_gear(&mut self, idx: usize) {
        match idx {
            0 => self.prewarm_capture = false,
            i => {
                self.prewarm_capture = true;
                self.prewarm_idle_minutes =
                    PREWARM_GEARS.get(i - 1).map(|(m, _)| *m).unwrap_or(0);
            }
        }
    }

    /// 档位文案（含「自定义」兜底；链路报告与界面共用）
    pub fn prewarm_label(&self) -> String {
        match self.prewarm_gear() {
            Some(0) => "只在按下时开麦".to_string(),
            Some(i) => PREWARM_GEARS[i - 1].1.to_string(),
            None => format!("自定义：空闲 {} 分钟关麦", self.prewarm_idle_minutes),
        }
    }

    /// 模型释放档位索引；`None` = 自改的非常规值
    pub fn model_release_gear(&self) -> Option<usize> {
        MODEL_RELEASE_GEARS
            .iter()
            .position(|(m, _)| *m == self.model_release_minutes)
    }

    /// 按档位索引写回 `model_release_minutes`（越界按“永不释放”）
    pub fn set_model_release_gear(&mut self, idx: usize) {
        self.model_release_minutes = MODEL_RELEASE_GEARS.get(idx).map(|(m, _)| *m).unwrap_or(0);
    }

    /// 模型释放档位文案（含「自定义」兜底）
    pub fn model_release_label(&self) -> String {
        match self.model_release_gear() {
            Some(i) => MODEL_RELEASE_GEARS[i].1.to_string(),
            None => format!("自定义：空闲 {} 分钟释放", self.model_release_minutes),
        }
    }

    /// 出字间隔档位索引；`None` = config.toml 里是自改的非常规值（界面显示「自定义」）
    pub fn flush_gear(&self) -> Option<usize> {
        FLUSH_GEARS.iter().position(|(s, _)| *s == self.flush_seconds)
    }

    /// 按档位索引写回 `flush_seconds`（越界忽略）
    pub fn set_flush_gear(&mut self, idx: usize) {
        if let Some((s, _)) = FLUSH_GEARS.get(idx) {
            self.flush_seconds = *s;
        }
    }

    /// 出字间隔文案（含「自定义」兜底；界面/日志共用）
    pub fn flush_label(&self) -> String {
        match self.flush_gear() {
            Some(i) => format!("每 {} 秒", FLUSH_GEARS[i].0),
            None => format!("自定义：每 {} 秒", self.flush_seconds.max(1)),
        }
    }

    /// 录音方式是否「切换模式」（按一下开始 / 再按一下停止）
    ///
    /// 为什么要有这个函数（2026-10-20 用户报「这个选项实际没有作用」）：
    /// 旧实现每处都是 `hotkey_mode == "toggle"` 直接比字符串，于是
    ///   - 手写 `hotkey_mode = "Toggle"` / `" TOGGLE "` / `"切换"` → **静默**按「按住说话」跑；
    ///   - GUI/ctl 之外任何一条写配置的路径稍有出入也静默失效。
    /// 用户体感就是「选了切换，按下去还是松手就截止」。现在统一走这里规范化，
    /// 非法值只记一次 WARN（`General::mode_label` 会把它显示成「按住说话（无法识别的取值）」）。
    pub fn is_toggle_mode(&self) -> bool {
        match self.hotkey_mode.trim() {
            v if v.eq_ignore_ascii_case("toggle") || v.eq_ignore_ascii_case("switch") => true,
            "切换" | "切换模式" => true,
            v if v.eq_ignore_ascii_case("hold") || v.eq_ignore_ascii_case("push") => false,
            "按住" | "按住说话" | "" => false,
            other => {
                warn_unknown_mode(other);
                false
            }
        }
    }

    /// 录音方式的中文文案（日志/设置页/托盘共用同一套说法）
    pub fn mode_label(&self) -> &'static str {
        if self.is_toggle_mode() {
            "切换（按一下开始 / 再按一下停止）"
        } else {
            "按住说话"
        }
    }

    /// 规范化写回 `hotkey_mode`（GUI/ctl 一律写 "hold" | "toggle"）
    pub fn set_toggle_mode(&mut self, toggle: bool) {
        self.hotkey_mode = if toggle { "toggle".into() } else { "hold".into() };
    }
}

impl Floating {
    /// 停留时间档位索引；`None` = config.toml 里是自改的非常规值
    pub fn hide_gear(&self) -> Option<usize> {
        FLOAT_HIDE_GEARS
            .iter()
            .position(|(s, _)| *s == self.hide_delay_seconds)
    }

    /// 按档位索引写回 `hide_delay_seconds`（越界忽略）
    pub fn set_hide_gear(&mut self, idx: usize) {
        if let Some((s, _)) = FLOAT_HIDE_GEARS.get(idx) {
            self.hide_delay_seconds = *s;
        }
    }

    /// 停留时间文案（含「自定义」兜底）
    pub fn hide_label(&self) -> String {
        match self.hide_gear() {
            Some(i) => format!("{} 秒", FLOAT_HIDE_GEARS[i].0),
            None => format!("自定义：{} 秒", self.hide_delay_seconds.clamp(1, 60)),
        }
    }
}
/// 自动保存前的兜底：热键无效时**不把坏值写进配置**，保留磁盘上那个有效热键。
///
/// 返回 `(要写入的配置, 是否替换过热键)`。抽成纯函数是为了能单测 —— 设置页那套点击
/// 在本机规则下不允许自动化（禁止模拟鼠标），只能靠单测把这条逻辑钉死。
pub fn normalize_before_save(cfg: &Config, disk: Option<&Config>) -> (Config, bool) {
    let mut out = cfg.clone();
    if crate::hotkey::parse(&out.general.hotkey).is_err() {
        if let Some(d) = disk {
            if crate::hotkey::parse(&d.general.hotkey).is_ok() {
                out.general.hotkey = d.general.hotkey.clone();
                return (out, true);
            }
        }
    }
    (out, false)
}
/// 无法识别的 `hotkey_mode` 只警告一次（同一取值不刷屏）
fn warn_unknown_mode(raw: &str) {
    use std::sync::{Mutex, OnceLock};
    static LAST: OnceLock<Mutex<String>> = OnceLock::new();
    let m = LAST.get_or_init(|| Mutex::new(String::new()));
    if let Ok(mut last) = m.lock() {
        if *last != raw {
            *last = raw.to_string();
            tracing::warn!(
                "配置 hotkey_mode = {raw:?} 无法识别（只认 hold / toggle，也接受中文「按住」「切换」）→ 本次按「按住说话」处理"
            );
        }
    }
}

/// 悬浮窗（调试版）：状态/计时 + 最近 N 段「原文行 + 整理行」
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Floating {
    pub enabled: bool,
    /// 字号 12~32
    pub font_size: u32,
    /// 保留段数 1~10
    pub history: u32,
    /// 每行最大字数，0 = 不截断
    pub max_chars: u32,
    /// 会话结束后悬浮窗停留多久（秒）：设置页 5 选 1（[`FLOAT_HIDE_GEARS`]）
    ///
    /// 2026-10-20 用户报「现在太久不消失影响体验」——老配置里是 30 秒，且设置页只是一个
    /// 自由填数字的框。现在给 1/3/5/10/30 秒五档，默认 3 秒。
    pub hide_delay_seconds: u32,
    /// 悬浮窗是否显示**识别/整理的文字**
    ///
    /// 默认 `false`（2026-10-20 用户要求）：「输出的文字不应该再出现在悬浮窗中」——
    /// 文字已经注入到目标窗口了，悬浮窗再留着上一批就成了干扰。关掉后悬浮窗只显示
    /// 状态点 / 门控电平 / 计时 / 「已注入 N 字」这类一句话反馈。
    pub show_text: bool,
    /// 拖动记忆（-1 = 未设置，用默认底部居中）
    pub pos_x: i32,
    pub pos_y: i32,
}

impl Default for Floating {
    fn default() -> Self {
        Self {
            enabled: true,
            font_size: 16,
            history: 3,
            max_chars: 60,
            hide_delay_seconds: 1,
            show_text: false,
            pos_x: -1,
            pos_y: -1,
        }
    }
}

/// 文本后处理：去标点 / 空格连接（保留 ITN 的数字规范）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TextNorm {
    /// 去掉所有标点（中英文），保留数字与英文
    pub strip_punctuation: bool,
    /// 注入文本结尾补一个空格
    pub trailing_space: bool,
}

impl Default for TextNorm {
    fn default() -> Self {
        Self {
            strip_punctuation: true,
            trailing_space: true,
        }
    }
}

/// 用户手写补充的术语表（**日常术语在热词包里**，见 `[hotwords]`；这里是兜底/覆盖层）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Glossary {
    pub enabled: bool,
    /// 关注的英文术语（提示 LLM 保留/还原；优先级高于热词包）
    pub terms: Vec<String>,
    /// 别名替换："欧拉马=Ollama"（ASR 常把英文音译成中文；同时作为拼音模糊匹配的目标）
    pub aliases: Vec<String>,
    /// 英文错词纠正："grape=grep"（整词、忽略大小写；修同音混淆）
    pub corrections: Vec<String>,
}

impl Default for Glossary {
    fn default() -> Self {
        Self {
            enabled: true,
            terms: Vec::new(),
            aliases: default_glossary_aliases(),
            corrections: default_glossary_corrections(),
        }
    }
}

/// 热词包：一份词表同时驱动打字（Rime 置顶候选）与录音（ASR 热词偏置 + 术语保留）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotwords {
    pub enabled: bool,
    /// 启用哪些包（`<exe 同级>\hotwords\<名>.tsv`；内置 keyboard / vibecoding）
    pub packs: Vec<String>,
    /// 空 = exe 同级 `hotwords\`
    pub dir: String,
    /// 送 FunASR-Nano 热词 prompt 的最大条数（其 LLM KV 上限 512，节目已按 30 条保底）
    pub asr_top_n: usize,
    /// 快捷输入表（`hotwords\phrases.tsv`，编码 → 任意文本）；**只用于打字**
    pub phrases: bool,
    /// 全局「快捷输入」展开（英文 / 直通状态下也能用 `gds` → 快递地址）
    ///
    /// 中文状态走 Rime 候选表（第一项），这里负责兜底：
    /// 键盘钩子缓冲最近字符 → 命中用户编码 → 退格 + 注入文本。
    /// **守卫**：靠 `listen_learn.lua` 写的 `%APPDATA%\Rime\listen_ime_beat.txt` 心跳判断
    /// 当前是不是中文输入状态；心跳新鲜就保持沉默，避免与输入法重复出字。
    pub quick_expand: bool,
    /// 触发展开的最短编码长度（默认 3，防误伤）
    pub quick_expand_min_len: usize,
    /// 打字「字词联想」：给 Rime 的 `custom_phrase` 开 `enable_completion`
    ///
    /// ⚠️ **默认关**（2026-10-21 晚改）：打开后打任何前缀都会把所有以它开头的热词列出来，
    /// 显示成 `相关~guan` 这种 —— 配合 `initial_quality: 1000000` 会直接占满候选窗，
    /// 用户实测"输入法没法用"。想开的话建议只加很少的短编码。
    /// ⚠️ 这是**方案结构改动**：改完必须「立即部署」一次（一次 UAC）才生效。
    pub prefix_completion: bool,
    /// 用户词学习（`hotwords\learned.tsv`，打过的词自动置顶）；**只用于打字**
    pub learn: bool,
    /// 「我打过的词」最多置顶多少条（超出按使用次数淘汰）
    pub learn_max: usize,
    /// 攒够这么多新词就自动重新部署（0 = 只手动；部署会弹 UAC）
    pub learn_auto_deploy: usize,
}

impl Default for Hotwords {
    fn default() -> Self {
        Self {
            enabled: true,
            packs: vec!["vibecoding".into(), "keyboard".into(), "mine".into()],
            dir: String::new(),
            asr_top_n: 30,
            phrases: true,
            prefix_completion: false,
            quick_expand: true,
            quick_expand_min_len: 3,
            learn: true,
            learn_max: 2000,
            learn_auto_deploy: 20,
        }
    }
}

/// 默认「中文音译 → 英文术语」别名（也用于拼音模糊匹配的锚点）
pub fn default_glossary_aliases() -> Vec<String> {
    [
        "欧拉马=Ollama", "奥拉马=Ollama", "哦拉马=Ollama", "欧拉玛=Ollama",
        "奎文=Qwen", "快问=Qwen",
        "克劳德=Claude", "克勞德=Claude",
        "尤班图=Ubuntu", "乌班图=Ubuntu",
        "吉特=Git", "吉特哈勃=GitHub",
        "多克=Docker", "多克尔=Docker",
        "拉斯=Rust", "卡狗=cargo",
        "派森=Python", "派啪=pip",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// 默认「英文错词 → 正确词」整词纠正（忽略大小写；修同音混淆）
/// 只放**明确**的；歧义的（如 52832/52833）不要放，否则会误改正确文本。
pub fn default_glossary_corrections() -> Vec<String> {
    [
        "grape=grep",
        "kernal=kernel",
        "recieve=receive",
        "seperator=separator",
        "paramter=parameter",
        "prameter=parameter",
        "environemnt=environment",
        "sucess=success",
        "thier=their",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// 中英混说横评：可自由切换的本地模型清单（`listen --bench <wav目录>` 依次跑）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Bench {
    pub models: Vec<BenchModel>,
}

impl Default for Bench {
    fn default() -> Self {
        Self {
            models: default_bench_models(),
        }
    }
}

/// 单个横评模型：name 仅展示；type = sensevoice | paraformer | funasr-nano；dir = 模型目录
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BenchModel {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub dir: String,
    /// funasr-nano：是否把 glossary 词表作为热词 prompt 注入
    pub hotwords: bool,
    /// 空 = 自动
    pub language: String,
}

impl Default for BenchModel {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: "sensevoice".into(),
            dir: String::new(),
            hotwords: false,
            language: String::new(),
        }
    }
}

/// 出厂横评清单：当前本机可用的本地模型（含"是否用热词"对照）
pub fn default_bench_models() -> Vec<BenchModel> {
    vec![
        BenchModel {
            name: "SenseVoice".into(),
            kind: "sensevoice".into(),
            dir: "models/sense-voice".into(),
            hotwords: false,
            language: String::new(),
        },
        BenchModel {
            name: "FunASR-Nano fp16".into(),
            kind: "funasr-nano".into(),
            dir: r"K:\AI_Voice\models\asr\sherpa-onnx-funasr-nano-fp16-2025-12-30".into(),
            hotwords: false,
            language: String::new(),
        },
        BenchModel {
            name: "FunASR-Nano fp16 +热词".into(),
            kind: "funasr-nano".into(),
            dir: r"K:\AI_Voice\models\asr\sherpa-onnx-funasr-nano-fp16-2025-12-30".into(),
            hotwords: true,
            language: String::new(),
        },
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Asr {
    /// local | cloud
    pub backend: String,
    /// 空=自动；可填 "zh" / "en"
    pub language: String,
    pub local_model: String,
    /// 本地模型类型：sensevoice | paraformer | funasr-nano
    pub model_type: String,
    /// FunASR-Nano 模型目录（model_type = funasr-nano 时使用）
    pub funasr_nano_model: String,
    /// Paraformer 热词加权（越大越偏向热词）
    pub hotwords_score: f32,
    pub num_threads: i32,
    pub cloud: CloudAsr,
}

impl Default for Asr {
    fn default() -> Self {
        Self {
            backend: "local".into(),
            language: String::new(),
            local_model: "models/sense-voice".into(),
            model_type: "sensevoice".into(),
            funasr_nano_model: r"K:\AI_Voice\models\asr\sherpa-onnx-funasr-nano-fp16-2025-12-30".into(),
            hotwords_score: 1.5,
            num_threads: 4,
            cloud: CloudAsr::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CloudAsr {
    /// 例：https://api.example.com/v1
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout_s: u64,
}

impl Default for CloudAsr {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            api_key: String::new(),
            model: "whisper-1".into(),
            timeout_s: 15,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Polish {
    pub enabled: bool,
    /// local | cloud（enabled=false 时忽略；旧配置缺此字段 → local）
    pub provider: String,
    /// light | polish
    pub mode: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout_s: u64,
    pub local: PolishLocal,
}

impl Default for Polish {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: "local".into(),
            mode: "light".into(),
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            timeout_s: 5,
            local: PolishLocal::default(),
        }
    }
}

impl Polish {
    /// 界面档位：off | local.light | local.polish | cloud.light | cloud.polish
    pub fn tier(&self) -> &'static str {
        if !self.enabled {
            return "off";
        }
        let cloud = self.provider == "cloud";
        match (cloud, self.mode.as_str()) {
            (false, "polish") => "local.polish",
            (false, _) => "local.light",
            (true, "polish") => "cloud.polish",
            (true, _) => "cloud.light",
        }
    }

    pub fn set_tier(&mut self, tier: &str) {
        match tier {
            "off" => self.enabled = false,
            "local.light" => {
                self.enabled = true;
                self.provider = "local".into();
                self.mode = "light".into();
            }
            "local.polish" => {
                self.enabled = true;
                self.provider = "local".into();
                self.mode = "polish".into();
            }
            "cloud.light" => {
                self.enabled = true;
                self.provider = "cloud".into();
                self.mode = "light".into();
            }
            "cloud.polish" => {
                self.enabled = true;
                self.provider = "cloud".into();
                self.mode = "polish".into();
            }
            _ => {}
        }
    }

    pub fn tier_label(tier: &str) -> &'static str {
        match tier {
            "local.light" => "本地 · 轻度清洗",
            "local.polish" => "本地 · 中度润色",
            "cloud.light" => "云端 · 轻度清洗",
            "cloud.polish" => "云端 · 中度润色",
            _ => "关闭",
        }
    }
}

/// 本地大模型（llama-server）设置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolishLocal {
    pub exe: String,
    pub model: String,
    pub host: String,
    pub port: u16,
    /// GPU 层数，99 = 全部
    pub ngl: i32,
    pub ctx: u32,
    pub temperature: f32,
    pub top_p: f32,
    /// 必须设上限，否则思考残留会撞满 ctx
    pub max_tokens: u32,
    /// 关思考（Qwen3.5 等思考型模型必须 true）
    pub disable_thinking: bool,
    /// 冷启动等待上限（秒）
    pub start_timeout_s: u64,
    /// 首次触发是否同步等待（false = 本次回退原文，后台拉起）
    pub wait_on_first_use: bool,
    /// 程序退出时回收自己拉起的进程
    pub stop_on_exit: bool,
}

impl Default for PolishLocal {
    fn default() -> Self {
        Self {
            exe: r"K:\projects\llama.cpp\build\bin\Release\llama-server.exe".into(),
            model: r"K:\AI_Voice\models\llm\Qwen3.5-4B-Q4_K_M.gguf".into(),
            host: "127.0.0.1".into(),
            port: 18080,
            ngl: 99,
            ctx: 4096,
            temperature: 0.2,
            top_p: 0.9,
            max_tokens: 512,
            disable_thinking: true,
            start_timeout_s: 150,
            wait_on_first_use: false,
            stop_on_exit: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Inject {
    pub restore_clipboard: bool,
    pub paste_delay_ms: u64,
    pub restore_delay_ms: u64,
    pub raw_replace_undo: bool,
    /// 长语音分段注入时，段与段之间的分隔符（聊天框里用空格最安全）
    pub chunk_separator: String,
}

impl Default for Inject {
    fn default() -> Self {
        Self {
            restore_clipboard: true,
            paste_delay_ms: 250,
            restore_delay_ms: 600,
            raw_replace_undo: true,
            chunk_separator: " ".into(),
        }
    }
}

/// Rime 输入法（倾听输入法）二合一配置
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rime {
    /// 资源目录（空 = 自动：exe 同级 resources\，找不到再往上级找）
    pub resources: String,
    /// Rime 用户目录（空 = 自动检测：HKCU\Software\Rime\Weasel\RimeUserDir → %APPDATA%\Rime）
    pub user_dir: String,
    /// 主力语言：zh | en | ja（空 = 未配置，首启向导会问）
    pub primary_lang: String,
    /// 英文方案是否一并放进方案列表（F4 可切）
    pub english_mixed: bool,
    /// 中文简繁：false=简 true=繁
    pub zh_trad: bool,
    /// 是否启用万象语法模型（仅中文）
    pub grammar: bool,
    /// 检测到的倾听输入法版本（部署时写入）
    pub weasel_version: String,
    /// 倾听输入法是不是我们装的（卸载时才询问是否一并卸载）
    pub weasel_installed_by_us: bool,
    /// 最近一次部署结果（日志尾部，GUI 展示用）
    pub last_deploy: String,
}

impl Rime {
    pub fn lang(&self) -> crate::rime::schemes::Lang {
        crate::rime::schemes::Lang::from_str(&self.primary_lang)
    }
}

/// Windows canonicalize 会加 `\\?\` 前缀，去掉（显示与外部命令更友好）
fn strip_unc(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest.to_string());
    }
    p
}

/// 应用路径：root = 配置文件所在目录（绿色部署根）
#[derive(Debug, Clone)]
pub struct Paths {    pub root: PathBuf,
    pub config_file: PathBuf,
}

impl Paths {
    /// 默认：exe 同级 config.toml
    pub fn default_from_exe() -> Result<Self> {
        let exe = std::env::current_exe().context("无法获取 exe 路径")?;
        let dir = exe
            .parent()
            .context("exe 无父目录")?
            .to_path_buf();
        Ok(Self {
            config_file: dir.join("config.toml"),
            root: dir,
        })
    }

    /// 指定配置文件：root = 该文件的父目录（尽量转绝对路径，避免相对路径下资源查找走偏）
    pub fn from_config_file(p: impl AsRef<Path>) -> Result<Self> {
        let config_file = p.as_ref().to_path_buf();
        let root = config_file
            .parent()
            .map(|x| x.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let root = root.canonicalize().unwrap_or(root);
        let root = strip_unc(root);
        Ok(Self {
            config_file: root.join(config_file.file_name().unwrap_or_default()),
            root,
        })
    }

    pub fn resolve(&self, rel: &str) -> PathBuf {
        let p = Path::new(rel);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        }
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// 资源目录：显式配置优先；否则 exe 同级 resources\，再往上级找 3 层（开发期 target\release 用）
    pub fn resources_dir(&self, cfg: &Rime) -> PathBuf {
        if !cfg.resources.trim().is_empty() {
            return self.resolve(&cfg.resources);
        }
        let mut dir = Some(self.root.clone());
        for _ in 0..4 {
            let Some(d) = dir else { break };
            let cand = d.join("resources").join("rime");
            if cand.is_dir() {
                return d.join("resources");
            }
            dir = d.parent().map(|p| p.to_path_buf());
        }
        self.root.join("resources")
    }
}

impl Config {
    pub fn load_or_create(paths: &Paths) -> Result<Self> {
        if paths.config_file.is_file() {
            let text = std::fs::read_to_string(&paths.config_file)
                .with_context(|| format!("读取配置失败: {}", paths.config_file.display()))?;
            let cfg: Config = toml::from_str(&text)
                .with_context(|| format!("解析配置失败: {}", paths.config_file.display()))?;
            Ok(cfg)
        } else {
            let cfg = Config::default();
            cfg.save(paths)?;
            Ok(cfg)
        }
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        if let Some(dir) = paths.config_file.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let text = toml::to_string_pretty(self).context("序列化配置失败")?;
        std::fs::write(&paths.config_file, text)
            .with_context(|| format!("写入配置失败: {}", paths.config_file.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prewarm_gear_roundtrip() {
        let mut g = General::default();
        assert_eq!(g.prewarm_gear(), Some(1), "默认档位 = 永不关麦");
        assert_eq!(g.prewarm_label(), "永不关麦（推荐）");
        for idx in 0..=PREWARM_GEARS.len() {
            g.set_prewarm_gear(idx);
            assert_eq!(g.prewarm_gear(), Some(idx), "档位 {idx} 必须能原样回读");
        }
        g.set_prewarm_gear(0);
        assert!(!g.prewarm_capture, "档位 0 = 只在按下时开麦");
        g.set_prewarm_gear(3);
        assert!(g.prewarm_capture);
        assert_eq!(g.prewarm_idle_minutes, 5, "索引 3 = PREWARM_GEARS[2] = 5 分钟");
    }

    #[test]
    fn prewarm_gear_keeps_hand_edited_value() {
        let mut g = General::default();
        g.prewarm_idle_minutes = 7; // 手改的非预设值
        assert_eq!(g.prewarm_gear(), None);
        assert_eq!(g.prewarm_label(), "自定义：空闲 7 分钟关麦");
    }

    #[test]
    fn model_release_gear_roundtrip() {
        let mut g = General::default();
        assert_eq!(g.model_release_minutes, 0, "默认 = 永不释放");
        assert_eq!(g.model_release_gear(), Some(0));
        assert_eq!(g.model_release_label(), "永不释放（推荐）");
        for idx in 0..MODEL_RELEASE_GEARS.len() {
            g.set_model_release_gear(idx);
            assert_eq!(g.model_release_gear(), Some(idx), "档位 {idx} 必须能原样回读");
        }
        g.set_model_release_gear(2);
        assert_eq!(g.model_release_minutes, 5, "索引 2 = 5 分钟");
        g.model_release_minutes = 7;
        assert_eq!(g.model_release_gear(), None);
        assert_eq!(g.model_release_label(), "自定义：空闲 7 分钟释放");
    }

    #[test]
    fn old_config_with_10_minutes_maps_to_gear() {
        let mut g = General::default();
        g.prewarm_idle_minutes = 10; // 改默认（10→0）之前落盘的老配置
        assert_eq!(g.prewarm_gear(), Some(4));
        assert_eq!(g.prewarm_label(), "空闲 10 分钟关麦");
    }

    // ---- 录音方式（hotkey_mode）：用户报「选了切换却没生效」的回归 ----
    //
    // 历史 bug：处处 `hotkey_mode == "toggle"` 比字面量，写成 "Toggle"/"切换"/带空格
    // 全都**静默**按「按住说话」跑。这里把这些写法钉死成行为契约。

    #[test]
    fn hotkey_mode_accepts_common_spellings() {
        let mut g = General::default();
        assert!(!g.is_toggle_mode(), "默认 = 按住说话");
        assert_eq!(g.mode_label(), "按住说话");

        for v in ["toggle", "Toggle", " TOGGLE ", "switch", "切换", "切换模式"] {
            g.hotkey_mode = v.to_string();
            assert!(g.is_toggle_mode(), "{v:?} 必须被认成切换模式");
            assert_eq!(g.mode_label(), "切换（按一下开始 / 再按一下停止）");
        }
        for v in ["hold", "Hold", " HOLD ", "push", "按住", "按住说话", ""] {
            g.hotkey_mode = v.to_string();
            assert!(!g.is_toggle_mode(), "{v:?} 必须被认成按住说话");
            assert_eq!(g.mode_label(), "按住说话");
        }
        // 无法识别 = 按 hold 处理（并且只记一次 WARN，不能 panic / 不能误判成 toggle）
        g.hotkey_mode = "whatever".into();
        assert!(!g.is_toggle_mode());
    }

    #[test]
    fn hotkey_mode_writes_canonical_values() {
        let mut g = General::default();
        g.set_toggle_mode(true);
        assert_eq!(g.hotkey_mode, "toggle");
        assert!(g.is_toggle_mode());
        g.set_toggle_mode(false);
        assert_eq!(g.hotkey_mode, "hold");
        assert!(!g.is_toggle_mode());
    }

    /// 设置页/ctl 写的就是 canonical 值，存盘再读出来必须还是同一个语义（不会“存了却读回去变了”）
    #[test]
    fn hotkey_mode_survives_toml_roundtrip() {        let mut g = General::default();
        g.set_toggle_mode(true);
        let text = toml::to_string_pretty(&g).expect("序列化");
        assert!(text.contains("hotkey_mode = \"toggle\""), "落盘文本：{text}");
        let back: General = toml::from_str(&text).expect("反序列化");
        assert!(back.is_toggle_mode(), "存盘再读回必须仍是切换模式");
    }

    // ---- 出字间隔（flush_seconds）四档：用户报「要等 1 分钟才出字」的回归 ----

    #[test]
    fn flush_gears_are_the_four_presets() {
        assert_eq!(
            FLUSH_GEARS.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![10, 20, 40, 60],
            "设置页四档必须是 10/20/40/60 秒（用户明确要求的档位）"
        );
        // 默认 20s：老默认 60s 就是「按住说了半天没字」的元凶
        let g = General::default();
        assert_eq!(g.flush_seconds, 20);
        assert_eq!(g.flush_gear(), Some(1));
        assert_eq!(g.flush_label(), "每 20 秒");
    }

    #[test]
    fn flush_gear_roundtrip_and_custom() {
        let mut g = General::default();
        for idx in 0..FLUSH_GEARS.len() {
            g.set_flush_gear(idx);
            assert_eq!(g.flush_seconds, FLUSH_GEARS[idx].0, "档位 {idx} 必须写回预设秒数");
            assert_eq!(g.flush_gear(), Some(idx), "档位 {idx} 必须能原样回读");
        }
        // 手改的非常规值：不当成任何档位，文案显示「自定义」，且不会被悄悄改掉
        g.flush_seconds = 15;
        assert_eq!(g.flush_gear(), None);
        assert_eq!(g.flush_label(), "自定义：每 15 秒");
        // 越界索引不 panic、不改值
        g.set_flush_gear(99);
        assert_eq!(g.flush_seconds, 15);
    }

    // ---- 悬浮窗：停留时间五档 + 「不显示文字」默认（用户 2026-10-20 的两条要求） ----

    #[test]
    fn float_hide_gears_and_show_text_default() {
        assert_eq!(
            FLOAT_HIDE_GEARS.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![1, 3, 5, 10, 30],
            "停留时间五档必须是 1/3/5/10/30 秒"
        );
        let mut f = Floating::default();
        assert_eq!(f.hide_delay_seconds, 1, "默认 1 秒（用户 2026-10-21 要求：一闪就走）");
        assert_eq!(f.hide_gear(), Some(0));
        assert_eq!(f.hide_label(), "1 秒");
        assert!(!f.show_text, "默认不在悬浮窗显示识别/整理文字");
        for idx in 0..FLOAT_HIDE_GEARS.len() {
            f.set_hide_gear(idx);
            assert_eq!(f.hide_delay_seconds, FLOAT_HIDE_GEARS[idx].0);
            assert_eq!(f.hide_gear(), Some(idx));
        }
        // 手改的非常规值：显示「自定义」，不被悄悄改掉；越界索引不 panic
        f.hide_delay_seconds = 7;
        assert_eq!(f.hide_gear(), None);
        assert_eq!(f.hide_label(), "自定义：7 秒");
        f.set_hide_gear(99);
        assert_eq!(f.hide_delay_seconds, 7);
    }

    /// 老 config.toml 里没有 `show_text` → 反序列化后必须落到默认（false）
    #[test]
    fn old_floating_config_without_show_text_defaults_to_false() {
        // 注意：必须整份 Config 解析（`[floating]` 是子表），直接 parse Floating 会全落默认值
        let text = "[floating]\nenabled = true\nhide_delay_seconds = 30\npos_x = -1\npos_y = -1\n";
        let c: Config = toml::from_str(text).expect("老配置必须能读");
        let f = c.floating;
        assert_eq!(f.hide_delay_seconds, 30, "老值要保留");
        assert!(!f.show_text, "缺字段 → 默认不显示文字");
        assert_eq!(f.hide_gear(), Some(4), "30 秒是第五档");
    }

    // ---- 自动保存（取消「保存并应用」后的保存路径）：热键无效时不能把坏值写坏 ----

    #[test]
    fn autosave_keeps_last_good_hotkey() {
        let mut disk = Config::default(); // 磁盘上 f9 有效
        disk.general.hotkey = "f9".into();
        // 用户在输入框里敲到一半（"f" 不带修饰键 = 非法）
        let mut cur = Config::default();
        cur.general.hotkey = "f".into();
        cur.general.flush_seconds = 10; // 同时还有别的改动

        let (out, fixed) = normalize_before_save(&cur, Some(&disk));
        assert!(fixed, "非法热键必须标记为已修正");
        assert_eq!(out.general.hotkey, "f9", "写盘必须用磁盘上的有效热键");
        assert_eq!(out.general.flush_seconds, 10, "其它改动照样保存");

        // 热键合法时原样写入
        let mut ok = Config::default();
        ok.general.hotkey = "right_alt".into();
        let (out2, fixed2) = normalize_before_save(&ok, Some(&disk));
        assert!(!fixed2);
        assert_eq!(out2.general.hotkey, "right_alt");
    }

    /// 没有磁盘配置可比对时，非法热键也不会 panic（保持原样，交给上层提示）
    #[test]
    fn autosave_without_disk_is_safe() {
        let mut cur = Config::default();
        cur.general.hotkey = "win+".into();
        let (out, fixed) = normalize_before_save(&cur, None);
        assert!(!fixed);
        assert_eq!(out.general.hotkey, "win+");
    }

    /// 存盘 → 读回，出字间隔必须原样（GUI 保存后 worker 热更新读的就是这个）
    #[test]
    fn flush_seconds_survives_toml_roundtrip() {
        let mut g = General::default();
        g.set_flush_gear(0); // 10s
        let text = toml::to_string_pretty(&g).expect("序列化");
        assert!(text.contains("flush_seconds = 10"), "落盘文本：{text}");
        let back: General = toml::from_str(&text).expect("反序列化");
        assert_eq!(back.flush_seconds, 10);
        assert_eq!(back.flush_gear(), Some(0));
    }
}
