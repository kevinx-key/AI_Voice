//! 门控判据（C1）：决定「这段是语音吗」与「什么时候切段」。
//!
//! 两种实现，由 `config.general.gate_engine` 选（默认 `vad`）：
//!
//! - **`vad`（Silero VAD 主判据 + 端点驱动切分）**
//!   段由 VAD 自己吐：尾部静音到 `min_silence_duration` 就收尾（= 说了就上屏），
//!   长语音由 `max_speech_duration` 封顶（= 配置里的 `segment_seconds`，此模式下是**上限**）。
//!   我们只负责取段 + 过一遍既有防幻护栏（长度闸、重复堆叠守卫）。
//!   为什么换成 VAD：能量门分不清「键盘瞬变/环境噪声」和「人声」—— 实测键盘噪声段
//!   **4/4 全被送进 ASR**（只是恰好吐空串），而纯底噪 0 段进 ASR。见《项目调试过程.md》§18.3。
//!
//! - **`energy`（旧的自适应能量门）**
//!   就是原来的 `segmenter::Segmenter`（`noise_floor×2.5` + 200ms 窗 + 静音断句），
//!   **逻辑与单测原样保留**：既是回退路径，也是 `gate_bench` 的对照基线（`gate-before-vad.json`）。
//!
//! 为什么抽 trait（D5）：VAD 模型加载失败时要能**自动退回能量门**并明确记日志，
//! 而不是「按了没反应」。切换开关在 `config.toml`：`gate_engine = "vad" | "energy"`。
//!
//! 线程约束：VAD 实例内部有状态，方法虽然都是 `&self`，但 C 库注释写的是
//! *thread-safe for single-object usage*，**不是给并发用的**。本程序同一时刻只有一个
//! 判据在跑（worker 串行；`--gate`/`--simulate` 顺序跑文件），所以全局共享一个实例
//! 并在每次新建判据时 `reset()` 是安全的。

use crate::audio;
use crate::config::{Config, Paths};
use crate::models;
use crate::segmenter::Segmenter;
use anyhow::{Context, Result};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// 门控判据：`push` 音频进去，`take_segment` 取已判定为语音的段。
pub trait SpeechJudge: Send {
    /// 引擎名（日志/跑分用）
    fn name(&self) -> &'static str;
    /// 送入音频（工作线程内，任意块长）
    fn push(&mut self, samples: &[f32]);
    /// 取一个已切出的段（None = 还没切出来）
    fn take_segment(&mut self) -> Option<Vec<f32>>;
    /// 收尾：吐出残余语音（None = 剩下的没有语音证据，丢弃）
    fn flush(&mut self) -> Option<Vec<f32>>;
    /// 本会话是否出现过语音
    fn saw_speech(&self) -> bool;
    /// 段级能量闸门：`Some(t)` = 低于 t 当静音丢弃（能量实现）；
    /// `None` = 交判据自己负责（VAD 实现：段能吐出来就是语音，不该再用能量门砍一刀）
    fn gate_hint(&self) -> Option<f32>;
    /// 最近一次切段原因（日志用）
    fn last_cut_reason(&self) -> &'static str;
    /// 还有多少未切出的内容（`--gate` 用来区分「被丢弃」与「没东西可收尾」）
    fn pending_len(&self) -> usize;
    /// 清空状态，准备下一个会话/下一个 wav（`--gate` 跨文件复用同一个判据）
    fn reset(&mut self);
    /// **实时**读数（D3，悬浮窗用）：当前这一块音频是否被判为语音。
    ///
    /// 与 `saw_speech()`（会话级、只增不减）不同，这个是**逐块**状态，用来画
    /// 「语音 ● / 静音 ○」。默认 `None` = 该判据不支持实时读数（界面就不显示）。
    fn live_speech(&self) -> Option<bool> {
        None
    }
}

// ---------------------------------------------------------------- 能量门（旧路径）

/// 自适应能量门：直接复用 `segmenter::Segmenter`（逻辑/测试零改动）
pub struct EnergyJudge {
    seg: Segmenter,
    /// 建 Segmenter 的参数（reset 时要原样重建）
    target_s: u32,
    silence_s: f32,
    min_record_s: f32,
    min_speech_rms: f32,
    /// 最近一块音频的 RMS（live_speech 用；能量门没有逐块状态，自己记）
    last_rms: f32,
}

impl EnergyJudge {
    pub fn new(cfg: &Config) -> Self {
        let g = &cfg.general;
        Self {
            last_rms: 0.0,
            seg: Segmenter::new(
                g.segment_seconds,
                audio::TARGET_RATE,
                g.silence_seconds,
                g.min_record_seconds,
                g.min_speech_rms,
            ),
            target_s: g.segment_seconds,
            silence_s: g.silence_seconds,
            min_record_s: g.min_record_seconds,
            min_speech_rms: g.min_speech_rms,
        }
    }
}

impl SpeechJudge for EnergyJudge {
    fn name(&self) -> &'static str {
        "energy"
    }
    fn push(&mut self, samples: &[f32]) {
        self.last_rms = audio::rms(samples);
        self.seg.push(samples);
    }
    fn take_segment(&mut self) -> Option<Vec<f32>> {
        self.seg.take_segment()
    }
    fn flush(&mut self) -> Option<Vec<f32>> {
        self.seg.flush()
    }
    fn saw_speech(&self) -> bool {
        self.seg.saw_speech()
    }
    fn gate_hint(&self) -> Option<f32> {
        Some((self.seg.noise_floor() * 1.5).max(self.min_speech_rms))
    }
    fn last_cut_reason(&self) -> &'static str {
        self.seg.last_cut_reason()
    }
    fn pending_len(&self) -> usize {
        self.seg.pending_len()
    }
    fn reset(&mut self) {
        self.last_rms = 0.0;
        self.seg = Segmenter::new(
            self.target_s,
            audio::TARGET_RATE,
            self.silence_s,
            self.min_record_s,
            self.min_speech_rms,
        );
    }
    /// 能量门的「实时语音」：本块 RMS 是否过当前闸门（与切段同一套阈值，读数才不骗人）
    fn live_speech(&self) -> Option<bool> {
        let th = self.gate_hint().unwrap_or(self.min_speech_rms);
        Some(self.last_rms >= th)
    }
}

// ---------------------------------------------------------------- Silero VAD（C1 主路径）

/// Silero VAD 判据：段完全由 VAD 的端点检测决定
pub struct VadJudge {
    det: Arc<sherpa_onnx::VoiceActivityDetector>,
    /// `front()` 借出的段在 `pop()` 后就没了，先拷到自己的队列
    ready: VecDeque<Vec<f32>>,
    saw: bool,
}

impl VadJudge {
    /// 建一个判据（模型缺失时按 `models::ensure_vad` 的镜像列表下载；已就绪则只做校验）
    pub fn new(cfg: &Config, root: &Path, log: &dyn Fn(String)) -> Result<Self> {
        let model = models::ensure_vad(root, log)?;
        let det = shared(&model, cfg)?;
        det.reset(); // 复用全局实例：每次新建判据都从干净状态开始
        Ok(Self {
            det,
            ready: VecDeque::new(),
            saw: false,
        })
    }

    /// 把 VAD 队列里已完成的段搬进 `ready`
    fn collect(&mut self) {
        while let Some(seg) = self.det.front() {
            let samples = seg.samples().to_vec();
            let n = samples.len();
            self.det.pop();
            if n > 0 {
                self.saw = true;
                self.ready.push_back(samples);
            }
        }
    }
}

impl SpeechJudge for VadJudge {
    fn name(&self) -> &'static str {
        "vad"
    }
    fn push(&mut self, samples: &[f32]) {
        self.det.accept_waveform(samples);
        self.collect();
    }
    fn take_segment(&mut self) -> Option<Vec<f32>> {
        self.ready.pop_front()
    }
    fn flush(&mut self) -> Option<Vec<f32>> {
        self.det.flush();
        self.collect();
        self.ready.pop_front()
    }
    fn saw_speech(&self) -> bool {
        self.saw
    }
    fn gate_hint(&self) -> Option<f32> {
        None // VAD 说是语音就是语音，不再叠一层能量门（否则又回到"轻声被吃掉"）
    }
    fn last_cut_reason(&self) -> &'static str {
        "vad"
    }
    fn pending_len(&self) -> usize {
        // VAD 内部缓冲长度没暴露；用"当前是否正在检测语音"近似
        // （有内容但还没成段 = 收尾时会丢，`--gate` 的 tail_dropped 靠它）
        usize::from(self.det.detected())
    }
    fn reset(&mut self) {
        self.det.reset();
        self.ready.clear();
        self.saw = false;
    }
    /// VAD 自己的逐块判定（`detected()` 就是它此刻的内部状态）
    fn live_speech(&self) -> Option<bool> {
        Some(self.det.detected())
    }
}

/// 全局只留一个 VAD 实例（模型 630KB，加载虽快，但没必要每会话重来）。
/// 缓存键含全部参数：改了旋钮就重建，避免"改了设置没生效"。
fn shared(model: &Path, cfg: &Config) -> Result<Arc<sherpa_onnx::VoiceActivityDetector>> {
    static CACHE: Mutex<Option<(String, Arc<sherpa_onnx::VoiceActivityDetector>)>> = Mutex::new(None);
    let g = &cfg.general;
    let key = format!(
        "{}|{}|{}|{}|{}",
        model.display(),
        g.vad_threshold,
        g.silence_seconds,
        g.vad_min_speech_seconds,
        g.segment_seconds
    );
    let mut guard = CACHE.lock().unwrap();
    if let Some((k, det)) = guard.as_ref() {
        if *k == key {
            return Ok(det.clone());
        }
    }
    let conf = sherpa_onnx::VadModelConfig {
        silero_vad: sherpa_onnx::SileroVadModelConfig {
            model: Some(model.to_string_lossy().into_owned()),
            threshold: g.vad_threshold.clamp(0.01, 0.99),
            // 尾部静音：复用既有的 silence_seconds（默认 0.5s）= "说完就上屏"
            min_silence_duration: g.silence_seconds.max(0.05),
            min_speech_duration: g.vad_min_speech_seconds.max(0.0),
            window_size: 512, // 16k 下 Silero 的固定窗长
            // 最长段（上限）：复用 segment_seconds
            max_speech_duration: g.segment_seconds.max(1) as f32,
        },
        sample_rate: audio::TARGET_RATE as i32,
        num_threads: 1,
        provider: Some("cpu".into()), // 别跟 ASR/大模型抢 GPU
        ..Default::default()
    };
    let det = sherpa_onnx::VoiceActivityDetector::create(&conf, 30.0)
        .context("创建 VAD 失败（模型文件损坏或不受支持）")?;
    tracing::info!(
        "门控判据已就绪: Silero VAD（阈值 {:.2}，尾部静音 {:.2}s，最短语音 {:.2}s，最长段 {}s）",
        conf.silero_vad.threshold,
        conf.silero_vad.min_silence_duration,
        conf.silero_vad.min_speech_duration,
        conf.silero_vad.max_speech_duration
    );
    *guard = Some((key, Arc::new(det)));
    Ok(guard.as_ref().unwrap().1.clone())
}

/// 按配置建判据；VAD 不可用（模型缺失/加载失败）→ **自动回退能量门**并明确记日志
pub fn build(cfg: &Config, paths: &Paths) -> Box<dyn SpeechJudge> {
    let want = cfg.general.gate_engine.trim().to_ascii_lowercase();
    if want == "energy" {
        return Box::new(EnergyJudge::new(cfg));
    }
    let log = |m: String| tracing::info!("门控 VAD: {m}");
    match VadJudge::new(cfg, &paths.root, &log) {
        Ok(j) => Box::new(j),
        Err(e) => {
            tracing::warn!(
                "VAD 门控不可用，本次回退能量门控（{e:#}）——想固定用能量门可在 config.toml 设 gate_engine = \"energy\""
            );
            Box::new(EnergyJudge::new(cfg))
        }
    }
}
