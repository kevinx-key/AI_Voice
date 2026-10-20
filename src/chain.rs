//! 全链路状态快照（2026-10-09）：链路状态卡片 / 托盘 tooltip / `listen.exe --status` / ListenCtl `CHAIN`
//!
//! 目的：一句话回答「语音录入现在到底能不能用、哪一环不对」。
//! 一条链路按**用户能感知的顺序**串起来：
//!
//! ```text
//! 热键钩子 → 引擎总开关/托管态 → 识别模型 → 麦克风 → 门控引擎 → 整理后端 → 注入 → 最近一次会话
//! ```
//!
//! 数据流：worker 每秒把快照写进 [`ChainState`]（`Arc<Mutex<Chain>>`，与 ctl 线程共用同一份）：
//! - GUI 收 `Evt::Chain` 只管画（判级逻辑在这里，不在界面里重复）；
//! - 托盘 tooltip 用 [`Chain::summary`]；
//! - `listen.exe --status` 走管道 `CHAIN` 取 [`Chain::wire`]（**单行**，管道协议是一问一答一行）；
//! - 脚本用 `--status --json` 取 [`Chain::json`]。
//!
//! 判级只有一处（[`Chain::rows`]）：绿 = 正常，黄 = 能用但降级，红 = 坏了。

use serde::Serialize;
use std::sync::{Arc, Mutex};

/// 本地时间 `HH:MM:SS`（本机 UTC+8；与 gui.rs 原 `now_hms` 同口径，勿改成 UTC）
pub fn now_hms() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let s = secs % 86400 + 8 * 3600;
    format!("{:02}:{:02}:{:02}", (s / 3600) % 24, (s / 60) % 60, s % 60)
}

/// 单环健康度
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Level {
    /// 绿：正常
    Ok,
    /// 黄：能用，但降级/需要留意（例如麦已空闲关、整理后端没起、输入法不在前台）
    Warn,
    /// 红：坏了（模型加载失败、麦开着收不到音频、注入失败）
    Bad,
}

impl Level {
    pub fn mark(self) -> &'static str {
        match self {
            Level::Ok => "✓",
            Level::Warn => "!",
            Level::Bad => "✗",
        }
    }
    /// 托盘/一行摘要里的灯
    pub fn dot(self) -> &'static str {
        match self {
            Level::Ok => "●",
            Level::Warn => "◐",
            Level::Bad => "○",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Hook {
    pub installed: bool,
    /// 累计回调次数（钩子被调用的总次数：判「钩子是否被静默摘除」）
    pub calls: u64,
    /// 累计命中主键次数
    pub vk_calls: u64,
    /// 回调内自愈 KEYUP 丢失次数（>0 = 发生过丢键，黄灯）
    pub selfheal: u64,
    /// 近 60s 回调次数（滚动窗口；空闲不敲键盘时正常也是 0，别只看这个数）
    pub calls_60s: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Asr {
    /// `ready` | `loading` | `failed` | `off`
    pub phase: String,
    /// 后端名（本地 Paraformer / 本地 SenseVoice / 云端 …）
    pub backend: String,
    /// 最近一次识别耗时（毫秒）
    pub ms_last: Option<u64>,
    /// `phase == "released"` 时的“空闲了多少分钟才释放”（卡片文案用）
    pub released_min: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Mic {
    /// 采集流当前是否打开
    pub opened: bool,
    /// 配置里是否开了预热常开
    pub prewarm: bool,
    /// 配置里的空闲关麦分钟数（0 = 永不关）
    pub idle_close_min: u32,
    /// 已关麦时的“为什么”（空闲 N 分钟 / 打开失败 …）
    pub closed_why: Option<String>,
    /// 最近一块音频的 RMS
    pub rms: f32,
    /// 是否还在收到音频块（墙钟判定）
    pub flowing: bool,
    /// 是否「在收块、但电平恒为 0」≥3s（疑似系统/硬件把麦静音了）
    ///
    /// 为什么要单列（2026-10-09 踩到）：`Stop-Process -Force` 强杀持流进程后
    /// 紧接着开新流，会拿到一段**全是 0 的流**；而 `flowing` 依然是 true，
    /// 只看 RMS（截断成 0.0000）也分不清“房间安静”与“麦被静音”。电平“恰好 = 0.0”是数字静音的铁证。
    pub silent: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Polish {
    /// 档位（off / local.light / local.polish / cloud.light / cloud.polish）
    pub tier: String,
    /// llama-server 状态（未启动 / 启动中 / 就绪…）；云端档位时为空
    pub llm: String,
    pub llm_alive: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Session {
    /// 结束时刻 `HH:MM:SS`
    pub at: String,
    pub audio_s: f32,
    pub segs: u32,
    pub chars: u32,
    /// `segs == 0` 时的原因
    pub why: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Inject {
    pub at: String,
    pub ok: bool,
    pub chars: u32,
    /// 目标窗口标题（注入时抓的前台窗口）
    pub target: String,
    pub err: Option<String>,
}

/// 全链路快照
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Chain {
    pub voice_enabled: bool,
    /// `ready` | `suspended` | `starting` | `disabled` | `not_running`
    pub state: String,
    /// 是否托管模式（fork 拉起）
    pub managed: bool,
    /// 门控引擎：实际生效的那个（`vad` / `energy`）
    pub gate: String,
    /// 配置请求的门控引擎（与实际不一致 = VAD 回退，黄灯）
    pub gate_cfg: String,
    pub hook: Hook,
    pub asr: Asr,
    pub mic: Mic,
    pub polish: Polish,
    pub last_session: Option<Session>,
    pub inject: Option<Inject>,
}

impl Chain {
    /// 判级 + 文案：**唯一的判级处**（GUI 上色、`--status` 报告、tooltip 都从这里取）
    pub fn rows(&self) -> Vec<(Level, &'static str, String)> {
        let mut v = Vec::with_capacity(8);

        // 1. 热键钩子
        let hook = if !self.voice_enabled {
            (Level::Warn, "语音引擎总开关为 OFF：钩子已摘".to_string())
        } else if !self.hook.installed {
            (Level::Bad, "钩子未挂：按 F9 不会有任何反应（点「一键修复」重装）".to_string())
        } else if self.hook.selfheal > 0 {
            (
                Level::Warn,
                format!(
                    "已挂 · 累计回调 {}（命中 {}，**自愈 KEYUP 丢失 {}**）· 近 60s {}",
                    self.hook.calls, self.hook.vk_calls, self.hook.selfheal, self.hook.calls_60s
                ),
            )
        } else {
            (
                Level::Ok,
                format!(
                    "已挂 · 累计回调 {}（命中 {}，自愈 0）· 近 60s {}",
                    self.hook.calls, self.hook.vk_calls, self.hook.calls_60s
                ),
            )
        };
        v.push((hook.0, "热键钩子", hook.1));

        // 2. 引擎 / 托管态
        let engine = match self.state.as_str() {
            _ if !self.voice_enabled => (Level::Warn, "已关闭（设置页可重新开启）".to_string()),
            "ready" => (
                Level::Ok,
                format!(
                    "就绪{}",
                    if self.managed { "（托管：输入法前台）" } else { "（独立运行）" }
                ),
            ),
            "suspended" => (
                Level::Warn,
                "已挂起（输入法不在前台）· 钩子与模型仍在，F9 照旧可用".to_string(),
            ),
            "starting" => (Level::Warn, "启动中（正在加载识别模型）".to_string()),
            "disabled" => (Level::Warn, "已关闭（语音引擎总开关 OFF）".to_string()),
            _ => (Level::Bad, "未运行（识别后端不可用）".to_string()),
        };
        v.push((
            engine.0,
            "引擎",
            format!("{} · 总开关 {}", engine.1, if self.voice_enabled { "开" } else { "关" }),
        ));

        // 3. 识别模型
        let asr = match self.asr.phase.as_str() {
            "ready" => (
                Level::Ok,
                match self.asr.ms_last {
                    Some(ms) => format!("{} · 就绪 · 上次识别 {ms}ms", self.asr.backend),
                    None => format!("{} · 就绪", self.asr.backend),
                },
            ),
            "loading" => (Level::Warn, format!("{} · 加载中…", self.asr.backend)),
            "released" => (
                Level::Warn,
                format!(
                    "已释放（空闲 {} 分钟未用，省内存/显存）· 下次按下自动重载（约 +2s），首次整理会冷启",
                    self.asr.released_min
                ),
            ),
            "failed" => (
                Level::Bad,
                format!("{} · 加载失败（后台每 30s 自动重试）", self.asr.backend),
            ),
            _ => (Level::Warn, "未加载（语音引擎关闭中）".to_string()),
        };
        v.push((asr.0, "识别模型", asr.1));

        // 4. 麦克风（含实时电平 + 空闲关麦说明）
        let mic = if self.mic.opened {
            if self.mic.silent {
                (
                    Level::Warn,
                    "已打开、有音频块但**电平始终是 0.0**（数字静音）· \
                     检查 系统声音设置→输入→麦克风音量/静音，或键盘上的麦克风静音键；\
                     也可点「一键修复」重开麦"
                        .to_string(),
                )
            } else if self.mic.flowing {
                (
                    Level::Ok,
                    format!(
                        "{} · RMS {:.5} ●有声 · 空闲关麦：{}",
                        "预热常开",
                        self.mic.rms,
                        idle_label(self.mic.idle_close_min)
                    ),
                )
            } else {
                (
                    Level::Bad,
                    format!(
                        "已打开但**收不到音频**（RMS {:.5} ○无声）· 点「一键修复」重开麦",
                        self.mic.rms
                    ),
                )
            }
        } else if !self.mic.prewarm {
            (
                Level::Warn,
                "只在按下时开麦（最省资源）· 每次按下都会丢一点起头（约 0.1~0.3s）".to_string(),
            )
        } else {
            let why = self
                .mic
                .closed_why
                .clone()
                .unwrap_or_else(|| "已关闭".into());
            if self.mic.idle_close_min == 0 {
                (Level::Bad, format!("{why}（但配置是永不关麦）· 点「一键修复」重开麦"))
            } else {
                (
                    Level::Warn,
                    format!("{why} · 下次按下会丢一次起头（想彻底不丢就把空闲关麦设为「永不」）"),
                )
            }
        };
        v.push((mic.0, "麦克风", mic.1));

        // 5. 门控引擎
        let gate = if self.gate == "vad" {
            (Level::Ok, "Silero VAD（只认人声）".to_string())
        } else if self.gate_cfg == "vad" {
            (
                Level::Warn,
                "回退到能量门（VAD 模型不可用）· 键盘噪声可能被送进 ASR".to_string(),
            )
        } else {
            (Level::Warn, "能量门（兼容模式）".to_string())
        };
        v.push((gate.0, "门控引擎", gate.1));

        // 6. 整理后端
        let polish = if self.polish.tier == "off" {
            (Level::Ok, "已关闭（用户选择，出原文）".to_string())
        } else if self.polish.tier.starts_with("cloud") {
            (Level::Ok, format!("{} · 云端（可用性由整理失败时回退原文兜底）", self.polish.tier))
        } else if self.polish.llm_alive {
            (
                Level::Ok,
                format!("{} · llama-server {}", self.polish.tier, self.polish.llm),
            )
        } else {
            (
                Level::Warn,
                format!(
                    "{} · llama-server {}（首次整理会冷启动 10~25s，本次先用原文）",
                    self.polish.tier, self.polish.llm
                ),
            )
        };
        v.push((polish.0, "整理", polish.1));

        // 7. 注入链路
        let inject = match &self.inject {
            None => (Level::Warn, "还没有注入记录".to_string()),
            Some(i) if i.ok => (
                Level::Ok,
                format!(
                    "上次成功（{} 字，{}{}）",
                    i.chars,
                    i.at,
                    if i.target.is_empty() {
                        String::new()
                    } else {
                        format!("，目标「{}」", i.target)
                    }
                ),
            ),
            Some(i) => (
                Level::Bad,
                format!(
                    "上次失败（{}，目标「{}」）：{}　※ 目标窗口若是管理员权限，Windows UIPI 会拦下注入",
                    i.at,
                    i.target,
                    i.err.clone().unwrap_or_default()
                ),
            ),
        };
        v.push((inject.0, "注入", inject.1));

        // 8. 最近一次会话
        let sess = match &self.last_session {
            None => (Level::Warn, "本次运行还没有会话记录".to_string()),
            Some(s) if s.segs > 0 => (
                Level::Ok,
                format!(
                    "{} · 音频 {:.1}s · 出字 {} 段 {} 字",
                    s.at, s.audio_s, s.segs, s.chars
                ),
            ),
            Some(s) => (
                Level::Warn,
                format!(
                    "{} · 音频 {:.1}s · 未出字（{}）",
                    s.at,
                    s.audio_s,
                    s.why.clone().unwrap_or_else(|| "未知原因".into())
                ),
            ),
        };
        v.push((sess.0, "最近会话", sess.1));

        v
    }

    /// 整体是否健康（红 = 不健康）
    pub fn healthy(&self) -> bool {
        self.rows().iter().all(|(l, _, _)| *l != Level::Bad)
    }

    /// 有红项时给一句“怎么办”
    pub fn advice(&self) -> Option<String> {
        let bad: Vec<&str> = self
            .rows()
            .into_iter()
            .filter(|(l, _, _)| *l == Level::Bad)
            .map(|(_, n, _)| n)
            .collect();
        if bad.is_empty() {
            return None;
        }
        Some(format!(
            "{} 不正常 → 建议点「一键修复」（重装钩子 + 重开麦 + 重新加载模型）；仍不行就看日志",
            bad.join(" / ")
        ))
    }

    /// 从 ListenCtl `CHAIN` 应答体还原（`listen.exe --status` 用）
    ///
    /// 容错优先：未知键忽略、坏字段取默认值 —— 老/新版本互通时最多丢一项，不会整份报错。
    pub fn from_wire(s: &str) -> Chain {
        let mut c = Chain::default();
        for kv in s.split_whitespace() {
            let Some((k, v)) = kv.split_once('=') else {
                continue;
            };
            match k {
                "state" => c.state = v.into(),
                "managed" => c.managed = flag(v),
                "voice" => c.voice_enabled = flag(v),
                "gate" => {
                    // 新版本发 `gate=实际:配置`（旧版本只有实际）
                    let mut it = v.split(':');
                    c.gate = it.next().unwrap_or_default().into();
                    c.gate_cfg = it.next().unwrap_or(&c.gate.clone()).to_string();
                }
                "hook" => {
                    // 已挂:累计回调:命中:自愈:近60s
                    let f: Vec<&str> = v.split(':').collect();
                    c.hook = Hook {
                        installed: f.first().is_some_and(|x| flag(x)),
                        calls: f.get(1).and_then(|x| x.parse().ok()).unwrap_or(0),
                        vk_calls: f.get(2).and_then(|x| x.parse().ok()).unwrap_or(0),
                        selfheal: f.get(3).and_then(|x| x.parse().ok()).unwrap_or(0),
                        calls_60s: f.get(4).and_then(|x| x.parse().ok()).unwrap_or(0),
                    };
                }
                "asr" => {
                    // phase:后端:上次 ms:（已释放时的空闲分钟）
                    let f: Vec<&str> = v.splitn(4, ':').collect();
                    c.asr.phase = f.first().copied().unwrap_or_default().to_string();
                    c.asr.backend = unesc(f.get(1).copied().unwrap_or_default());
                    c.asr.ms_last = f.get(2).and_then(|x| x.parse().ok());
                    c.asr.released_min = f.get(3).and_then(|x| x.parse().ok()).unwrap_or(0);
                }
                "mic" => {
                    // 已打开:预热:空闲分钟:还在收:rms:关麦原因:数字静音（原因含 `:`，故倒数第二段用 splitn 吃掉剩余）
                    let f: Vec<&str> = v.splitn(7, ':').collect();
                    c.mic = Mic {
                        opened: f.first().is_some_and(|x| flag(x)),
                        prewarm: f.get(1).is_some_and(|x| flag(x)),
                        idle_close_min: f.get(2).and_then(|x| x.parse().ok()).unwrap_or(0),
                        flowing: f.get(3).is_some_and(|x| flag(x)),
                        rms: f.get(4).and_then(|x| x.parse().ok()).unwrap_or(0.0),
                        closed_why: f
                            .get(5)
                            .filter(|x| **x != "-" && !x.is_empty())
                            .map(|x| unesc(x)),
                        silent: f.get(6).is_some_and(|x| flag(x)),
                    };
                }
                "polish" => {
                    // 档位:llama 状态:存活
                    let mut it = v.split(':');
                    c.polish.tier = unesc(it.next().unwrap_or_default());
                    c.polish.llm = unesc(it.next().unwrap_or_default());
                    c.polish.llm_alive = it.next().is_some_and(flag);
                }
                "inject" => {
                    // ok(1/0/2=还没记录):字数:时刻:目标（窗口标题可能自带 `:`，用 splitn 吃掉剩余）
                    let f: Vec<&str> = v.splitn(4, ':').collect();
                    let ok = f.first().and_then(|x| x.parse::<u8>().ok()).unwrap_or(2);
                    c.inject = (ok != 2).then(|| Inject {
                        ok: ok == 1,
                        chars: f.get(1).and_then(|x| x.parse().ok()).unwrap_or(0),
                        at: f.get(2).map(|x| hms_restore(x)).unwrap_or_default(),
                        target: f.get(3).map(|x| unesc(x)).unwrap_or_default(),
                        err: None,
                    });
                }
                "session" => {
                    // 时刻:音频秒:段数:字数:未出字原因（原因可能含 `:`，用 splitn 吃掉剩余）
                    let f: Vec<&str> = v.splitn(5, ':').collect();
                    let at = f.first().map(|x| hms_restore(x)).unwrap_or_default();
                    c.last_session = (!at.is_empty()).then(|| Session {
                        at,
                        audio_s: f.get(1).and_then(|x| x.parse().ok()).unwrap_or(0.0),
                        segs: f.get(2).and_then(|x| x.parse().ok()).unwrap_or(0),
                        chars: f.get(3).and_then(|x| x.parse().ok()).unwrap_or(0),
                        why: f.get(4).filter(|x| !x.is_empty()).map(|x| unesc(x)),
                    });
                }
                _ => {}
            }
        }
        c
    }

    /// 托盘 tooltip / 一行摘要（人读）
    pub fn summary(&self) -> String {
        let rows = self.rows();
        let dots: String = rows.iter().map(|(l, _, _)| l.dot()).collect();
        let verdict = if self.healthy() { "正常" } else { "有异常" };
        let last = match &self.last_session {
            Some(s) if s.segs > 0 => format!("上次出字 {} 字", s.chars),
            Some(_) => "上次未出字".to_string(),
            None => "尚无会话".to_string(),
        };
        format!(
            "倾听输入法 · {verdict} {dots} ｜ {last} ｜ {} ｜ 麦{}",
            match self.state.as_str() {
                "ready" => "就绪",
                "suspended" => "挂起",
                "starting" => "启动中",
                _ if !self.voice_enabled => "已关闭",
                _ => "异常",
            },
            if self.mic.opened { "常开" } else { "已关" }
        )
    }

    /// ListenCtl `CHAIN` 应答体：**单行** `k=v`（值内不许有空格，见 `esc`）
    pub fn wire(&self) -> String {
        format!(
            "state={} managed={} voice={} hook={}:{}:{}:{}:{} asr={}:{}:{}:{} mic={}:{}:{}:{}:{}:{}:{} gate={}:{} polish={}:{}:{} inject={}:{}:{}:{} session={}:{}:{}:{}:{}",
            self.state,
            self.managed as u8,
            self.voice_enabled as u8,
            self.hook.installed as u8,
            self.hook.calls,
            self.hook.vk_calls,
            self.hook.selfheal,
            self.hook.calls_60s,
            self.asr.phase,
            esc(&self.asr.backend),
            self.asr.ms_last.map(|m| m.to_string()).unwrap_or_else(|| "-".into()),
            self.asr.released_min,
            self.mic.opened as u8,
            self.mic.prewarm as u8,
            self.mic.idle_close_min,
            self.mic.flowing as u8,
            self.mic.rms,
            esc(self.mic.closed_why.as_deref().unwrap_or("-")),
            self.mic.silent as u8,
            self.gate,
            self.gate_cfg,
            esc(&self.polish.tier),
            esc(&self.polish.llm),
            self.polish.llm_alive as u8,
            self.inject.as_ref().map(|i| i.ok as u8).unwrap_or(2),
            self.inject.as_ref().map(|i| i.chars).unwrap_or(0),
            esc(&hms_wire(
                &self.inject.as_ref().map(|i| i.at.clone()).unwrap_or_default(),
            )),
            esc(&self.inject.as_ref().map(|i| i.target.clone()).unwrap_or_default()),
            esc(&hms_wire(
                &self.last_session.as_ref().map(|s| s.at.clone()).unwrap_or_default(),
            )),
            self.last_session.as_ref().map(|s| s.audio_s).unwrap_or(0.0),
            self.last_session.as_ref().map(|s| s.segs).unwrap_or(0),
            self.last_session.as_ref().map(|s| s.chars).unwrap_or(0),
            esc(&self
                .last_session
                .as_ref()
                .and_then(|s| s.why.clone())
                .unwrap_or_default()),
        )
    }

    /// `listen.exe --status` 的多行报告
    pub fn report(&self) -> String {
        let mut s = format!("倾听输入法 链路状态（{}）\n", now_hms());
        s.push_str(&format!("  总判：{}\n", if self.healthy() { "正常" } else { "有异常" }));
        for (lvl, name, text) in self.rows() {
            s.push_str(&format!("  {} {:<8}{}\n", lvl.mark(), name, text));
        }
        if let Some(a) = self.advice() {
            s.push_str(&format!("  → {a}\n"));
        }
        s
    }

    /// `--status --json`
    pub fn json(&self) -> String {
        let mut root = serde_json::json!({});
        if let Ok(serde_json::Value::Object(o)) = serde_json::to_value(self) {
            root = serde_json::Value::Object(o);
        }
        // 附上判级结果，脚本不必自己再判一遍
        root["healthy"] = serde_json::json!(self.healthy());
        root["levels"] = serde_json::json!(self
            .rows()
            .into_iter()
            .map(|(l, n, _)| (n, format!("{:?}", l)))
            .collect::<Vec<_>>());
        serde_json::to_string_pretty(&root).unwrap_or_else(|_| "{}".into())
    }
}

fn esc(s: &str) -> String {
    // 管道协议是「空格分隔的 k=v」，值里不能有空白
    s.replace([' ', '\n', '\r', '\t'], "_")
}

/// wire 里的时刻写成无分隔数字（`231205`）
///
/// ⚠️ 不能用 `23:12:05`：`:` 是组内字段分隔符，时间戳自己带两个冒号会把后面
/// 所有字段的序号全部移错（2026-10-09 单测抳出来的真 bug：`session.chars` 永远解析成 0）。
fn hms_wire(t: &str) -> String {
    t.replace(':', "")
}

/// 把 `231205` 还原成 `23:12:05`（不是 6 位数字则原样返回）
fn hms_restore(s: &str) -> String {
    if s.len() == 6 && s.bytes().all(|b| b.is_ascii_digit()) {
        format!("{}:{}:{}", &s[0..2], &s[2..4], &s[4..6])
    } else {
        s.to_string()
    }
}

/// 逆着 [`esc`]：管道值里的下划线大概率是空格（仅用于显示字段）
fn unesc(s: &str) -> String {
    s.replace('_', " ")
}

fn flag(s: &str) -> bool {
    s == "1"
}

fn idle_label(min: u32) -> String {
    if min == 0 {
        "永不（推荐）".to_string()
    } else {
        format!("空闲 {min} 分钟")
    }
}

/// 共享快照（worker 写、ctl 读、GUI 通过 Evt 收）
#[derive(Clone, Default)]
pub struct ChainState(Arc<Mutex<Chain>>);

impl ChainState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self) -> Chain {
        self.0.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn update(&self, f: impl FnOnce(&mut Chain)) {
        if let Ok(mut g) = self.0.lock() {
            f(&mut g);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy_chain() -> Chain {
        Chain {
            managed: false,
            voice_enabled: true,
            state: "ready".into(),
            gate: "vad".into(),
            gate_cfg: "vad".into(),
            hook: Hook {
                installed: true,
                calls: 100,
                vk_calls: 10,
                selfheal: 0,
                calls_60s: 3,
            },
            asr: Asr {
                phase: "ready".into(),
                backend: "本地 Paraformer（中英）".into(),
                ms_last: Some(47),
                released_min: 0,
            },
            mic: Mic {
                opened: true,
                prewarm: true,
                idle_close_min: 0,
                closed_why: None,
                rms: 0.0007,
                flowing: true,
                silent: false,
            },
            polish: Polish {
                tier: "local.light".into(),
                llm: "未启动".into(),
                llm_alive: false,
            },
            last_session: Some(Session {
                at: "23:44:44".into(),
                audio_s: 2.7,
                segs: 1,
                chars: 9,
                why: None,
            }),
            inject: Some(Inject {
                at: "23:44:44".into(),
                ok: true,
                chars: 11,
                target: "记事本".into(),
                err: None,
            }),
        }
    }

    #[test]
    fn healthy_chain_has_no_red() {
        let c = healthy_chain();
        assert!(c.healthy(), "{:?}", c.rows());
        assert!(c.advice().is_none());
        assert_eq!(c.rows().len(), 8, "链路必须固定 8 环");
    }

    #[test]
    fn dead_mic_and_failed_asr_are_red() {
        let mut c = healthy_chain();
        c.mic.flowing = false;
        c.asr.phase = "failed".into();
        assert!(!c.healthy());
        let bad: Vec<&str> = c
            .rows()
            .into_iter()
            .filter(|(l, _, _)| *l == Level::Bad)
            .map(|(_, n, _)| n)
            .collect();
        assert_eq!(bad, vec!["识别模型", "麦克风"]);
        assert!(c.advice().unwrap().contains("一键修复"));
    }

    #[test]
    fn wire_is_single_line_and_parseable() {
        let w = healthy_chain().wire();
        assert!(!w.contains('\n'), "管道协议是一问一答一行");
        let toks: Vec<&str> = w.split_whitespace().collect();
        assert_eq!(toks.len(), 10, "字段数变了就要同步 from_wire：{w}");
        for tok in toks {
            let (k, v) = tok.split_once('=').expect("每个 token 必须是 k=v");
            assert!(!k.is_empty() && !v.is_empty(), "空键或空值: {tok}");
        }
        assert!(w.contains("state=ready"));
        assert!(w.contains("hook=1:100:10:0:3"));
        // 时刻不能带冒号（否则会把组内字段序号全部移错）
        assert!(w.contains("session=234444:"), "实际: {w}");
    }

    #[test]
    fn json_and_report_are_produced() {
        let c = healthy_chain();
        let j = c.json();
        assert!(j.contains("\"healthy\": true"));
        let r = c.report();
        assert!(r.contains("热键钩子") && r.contains("最近会话"));
    }

    #[test]
    fn wire_roundtrip_keeps_what_matters() {
        let a = healthy_chain();
        let b = Chain::from_wire(&a.wire());
        assert_eq!(b.state, a.state);
        assert_eq!(b.managed, a.managed);
        assert_eq!(b.voice_enabled, a.voice_enabled);
        assert_eq!(b.hook, a.hook, "钩子统计必须原样回去");
        assert_eq!(b.asr, a.asr);
        assert_eq!(b.mic, a.mic);
        assert_eq!(b.polish, a.polish);
        assert_eq!(b.inject.as_ref().map(|i| (i.ok, i.chars)), Some((true, 11)));
        assert_eq!(b.inject.as_ref().map(|i| i.at.clone()), Some("23:44:44".into()));
        assert_eq!(b.last_session.as_ref().map(|s| s.chars), Some(9));
        assert_eq!(
            b.last_session.as_ref().map(|s| s.at.clone()),
            Some("23:44:44".to_string()),
            "时刻必须能从无分隔形式还原回 HH:MM:SS"
        );
        assert_eq!(b.healthy(), a.healthy());
    }

    #[test]
    fn from_wire_tolerates_garbage() {
        let c = Chain::from_wire("unknown=1 state=ready hook=");
        assert_eq!(c.state, "ready");
        assert_eq!(c.hook, Hook::default());
        assert!(c.inject.is_none(), "ok=2 表示还没有注入记录");
    }

    #[test]
    fn released_model_is_yellow_with_hint() {
        // 空闲释放是用户在设置里选的降级（不是故障）：黄灯 + 说清下次要重载
        let mut c = healthy_chain();
        c.asr.phase = "released".into();
        c.asr.released_min = 10;
        assert!(c.healthy(), "空闲释放不是红项");
        let (lvl, _, text) = c.rows().into_iter().find(|(_, n, _)| *n == "识别模型").unwrap();
        assert_eq!(lvl, Level::Warn);
        assert!(text.contains("已释放") && text.contains("10"), "{text}");
        let b = Chain::from_wire(&c.wire());
        assert_eq!(b.asr.phase, "released");
        assert_eq!(b.asr.released_min, 10);
    }

    #[test]
    fn digital_silence_is_flagged() {
        // 2026-10-09 实际踩到：流在“有块”但电平恒为 0.0（数字静音）时，
        // 只看 flowing/rms 会当成正常；必须单列一个可行动的黄灯。
        let mut c = healthy_chain();
        c.mic.rms = 0.0;
        c.mic.silent = true;
        let (lvl, _, text) = c.rows().into_iter().find(|(_, n, _)| *n == "麦克风").unwrap();
        assert_eq!(lvl, Level::Warn);
        assert!(text.contains("数字静音"), "{text}");
        assert!(text.contains("静音键") || text.contains("音量"), "{text}");
    }

    #[test]
    fn idle_closed_mic_is_yellow_not_red() {
        let mut c = healthy_chain();
        c.mic.opened = false;
        c.mic.closed_why = Some("空闲 10 分钟".into());
        c.mic.idle_close_min = 10;
        assert!(c.healthy(), "空闲关麦是正常降级，不是故障");
        let (lvl, _, _) = c.rows().into_iter().find(|(_, n, _)| *n == "麦克风").unwrap();
        assert_eq!(lvl, Level::Warn);
    }

    #[test]
    fn never_close_config_but_closed_mic_is_red() {
        // 配置说“永不关麦”，麦却是关的 → 说明是开麦失败，红灯
        let mut c = healthy_chain();
        c.mic.opened = false;
        c.mic.closed_why = Some("打开失败: 设备被占用".into());
        c.mic.idle_close_min = 0;
        assert!(!c.healthy());
    }
}
