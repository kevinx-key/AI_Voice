//! 录音：cpal 采集 → 多声道降单声道 → 流式重采样到 16k → RMS
//!
//! 长语音用**流式**采集：音频线程持续把单声道样本推进共享缓冲，
//! 工作线程按需 `drain()` 取走并喂给分段器，内存只保留未处理部分。
//!
//! 设备兼容策略：按设备**默认配置**打开（不强推 16k，避免 WASAPI 的
//! `Invalid sample rate`），取回后自行重采样到 16k 供 ASR 使用。

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, StreamConfig};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const TARGET_RATE: u32 = 16000;

/// 环形缓冲保留时长（秒）：会话外只留最近这段做 pre-roll；
/// 会话内 worker 被 ASR 推理阻塞（FunASR-Nano 3s 段≈1s、云端最长 15s）时也靠它兜住音频。
/// 压穿时 `drain_from_ms` 会回报丢失毫秒数，调用方 warn，绝不静默丢。
pub const RING_SECONDS: f32 = 3.0;

// ---------------------------------------------------------------- 全局音频时钟
//
// 为什么要时钟：预热常开后，音频在被取走之前一直堆在环形缓冲里。按下时刻与
// “worker 终于空出手来”的时刻可能差上百毫秒（甚至更久，如果它卡在 ASR 推理里），
// 只有用**毫秒绝对时刻**切片，才能准确取到「按下时刻 − pre-roll」那一段。
// 时钟跨采集流累加（设备重开不归零），stale 的按下时刻也能靠 low 水位夹回来。

/// 进程内墙钟（毫秒，单调）
///
/// 为什么不能用 `AUDIO_MS` 判断“流还在不在收音频”：那个时钟**只在有音频块到达时推进**
/// （`Inner::push` 里 `fetch_add`），没音频时它和 `last_audio` 一起冻住，差值恒为 0
/// → 拿它当“停流”判据永远是 false（历史 bug，见 `is_broken`）。墙钟不依赖音频，才能反向判“没声音了”。
static WALL0: OnceLock<Instant> = OnceLock::new();

/// 进程启动以来的墙钟毫秒（单调）
pub fn wall_ms() -> u64 {
    WALL0.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// 全局音频时钟（毫秒，单调）
static AUDIO_MS: AtomicU64 = AtomicU64::new(0);
/// 热键按下瞬间的音频时钟（由 `hotkey::windows` 回调写入，见 `note_press`）
static PRESS_MS: AtomicU64 = AtomicU64::new(0);

/// 当前音频时钟（毫秒）
pub fn clock_ms() -> u64 {
    AUDIO_MS.load(Ordering::Relaxed)
}

/// 热键按下瞬间记录音频时钟（在 WH_KEYBOARD_LL 回调里调用，只做原子读写，不加锁）
pub fn note_press() {
    PRESS_MS.store(AUDIO_MS.load(Ordering::Relaxed), Ordering::SeqCst);
}

/// 最近一次按下的音频时钟（毫秒；0 = 未记录）
pub fn press_ms() -> u64 {
    PRESS_MS.load(Ordering::SeqCst)
}

/// 判定“流已死”的无音频时长（毫秒）
///
/// 为什么不用 cpal 的错误回调当判据：实测本机**每次重开流都会立刻报一次**
/// `A buffer underrun or overrun occurred`（不影响音频，覆盖率 96~100%），
/// 用它当“流损坏”会让 worker 每 40ms 重开一次设备（重开风暴）。
/// 现在只认“真的没数据了”（设备被拔/切换/独占）：连续这么久没新样本才算死。
pub const STALL_MS: u64 = 1500;

struct Inner {
    /// 未被取走的单声道样本（设备采样率），会话外被裁剪到 RING_SECONDS
    ring: Mutex<VecDeque<f32>>,
    /// 环形上限（样本数，设备采样率）
    cap: AtomicU32,
    stop: AtomicBool,
    /// 最近一次收到音频时的全局音频钟（毫秒）
    last_audio: AtomicU64,
    /// 最近一次收到音频的**墙钟**毫秒（判定“流还在不在收”，与 AUDIO_MS 无关）
    last_audio_wall: AtomicU64,
    /// 本流是否至少收到过一块音频
    ///
    /// 用处：区分“设备被拔/切换（本来在收，突然没了）”与“设备根本不吐数据”。
    /// 后者报“损坏”只会变成每 1.5s 重开一次的风暴，所以**从没收过块就不算损坏**。
    seen_audio: AtomicBool,
    /// 最近一块音频的 RMS（f32 位模式；界面实时电平用，不消耗环形缓冲）
    last_rms_bits: AtomicU32,
    rate: AtomicU32,
    channels: AtomicU32,
}

impl Inner {
    fn push<T>(&self, data: &[T], channels: usize)
    where
        T: Sample + SizedSample,
        f32: FromSample<T>,
    {
        if self.stop.load(Ordering::Relaxed) || data.is_empty() || channels == 0 {
            return;
        }
        let frames = data.len() / channels;
        let rate = self.rate.load(Ordering::Relaxed).max(1) as u64;
        let cap = self.cap.load(Ordering::Relaxed) as usize;
        let mut ring = self.ring.lock().unwrap();
        ring.reserve(frames);
        // 顺路算 RMS（界面实时电平）：不额外遍历，也不消耗环形缓冲
        let mut sq = 0.0f64;
        for frame in data.chunks_exact(channels) {
            let mut acc = 0.0f32;
            for s in frame {
                acc += s.to_sample::<f32>();
            }
            let m = acc / channels as f32;
            sq += (m as f64) * (m as f64);
            if cap > 0 && ring.len() >= cap {
                ring.pop_front();
            }
            ring.push_back(m);
        }
        self.last_rms_bits.store(
            ((sq / frames.max(1) as f64).sqrt() as f32).to_bits(),
            Ordering::Relaxed,
        );
        // 时钟在持锁期间推进：保证「ring 里最后一个样本的时刻 == 时钟」
        let step = frames as u64 * 1000 / rate;
        let now = AUDIO_MS.fetch_add(step, Ordering::Relaxed) + step;
        self.last_audio.store(now, Ordering::Relaxed);
        self.last_audio_wall.store(wall_ms(), Ordering::Relaxed);
        self.seen_audio.store(true, Ordering::Relaxed);
    }
}

/// 流式录音会话：持续采集，按需取走
pub struct StreamCapture {
    inner: Arc<Inner>,
    join: Option<std::thread::JoinHandle<()>>,
    rate: u32,
}

impl StreamCapture {
    /// 打开设备并开始采集（等待设备就绪后才返回，设备错误会立即抛出）
    pub fn start(device_name: &str) -> Result<Self> {
        let inner = Arc::new(Inner {
            ring: Mutex::new(VecDeque::new()),
            cap: AtomicU32::new((RING_SECONDS * TARGET_RATE as f32) as u32),
            stop: AtomicBool::new(false),
            last_audio: AtomicU64::new(0),
            last_audio_wall: AtomicU64::new(wall_ms()),
            seen_audio: AtomicBool::new(false),
            last_rms_bits: AtomicU32::new(0f32.to_bits()),
            rate: AtomicU32::new(TARGET_RATE),
            channels: AtomicU32::new(1),
        });
        let inner_thread = inner.clone();
        let dev_name = device_name.to_string();

        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        let join = std::thread::Builder::new()
            .name("audio-capture".into())
            .spawn(move || {
                let stream = match open_stream(&dev_name, &inner_thread) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                if let Err(e) = stream.play() {
                    let _ = ready_tx.send(Err(anyhow!("录音流启动失败: {e}")));
                    return;
                }
                let _ = ready_tx.send(Ok(()));
                while !inner_thread.stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(20));
                }
                drop(stream);
            })
            .context("创建录音线程失败")?;

        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(anyhow!("打开录音设备超时")),
        }

        let rate = inner.rate.load(Ordering::Relaxed);
        Ok(Self {
            inner,
            join: Some(join),
            rate,
        })
    }

    /// 设备实际采样率
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// 采集流是否已“死”（连续 STALL_MS 没有新音频）：设备被拔/切换/独占时重开
    ///
    /// ⚠️ 2026-10-09 修（原实现是死代码）：原判据是 `AUDIO_MS - last_audio > STALL_MS`，
    /// 但 `AUDIO_MS` **只在有音频块时推进**、且 `push` 末尾立刻把 `last_audio` 设成它，
    /// 于是差值恒为 0 → 这个函数**永远返回 false**，“设备断开→重开”从来没生效过。
    /// 现在改用墙钟 `wall_ms()`；并加 `seen_audio` 门槛：本流**从没收到过块**时不报损坏
    /// （那种情况重开也没用，只会变成每 1.5s 一次的重开风暴），交给界面 + 一键修复。
    pub fn is_broken(&self) -> bool {
        self.inner.seen_audio.load(Ordering::Relaxed)
            && wall_ms().saturating_sub(self.inner.last_audio_wall.load(Ordering::Relaxed))
                > STALL_MS
    }

    /// 实时电平：`(RMS, 是否还在收到音频)`
    ///
    /// 给「链路状态」卡片用：麦开着但一直不来块、或者电平一直是 0，能一眼看出来。
    /// **不消耗环形缓冲**（读的是回调线程顺路写的原子值），不会影响会话切片。
    pub fn live_level(&self) -> (f32, bool) {
        let rms = f32::from_bits(self.inner.last_rms_bits.load(Ordering::Relaxed));
        let quiet_ms = wall_ms().saturating_sub(self.inner.last_audio_wall.load(Ordering::Relaxed));
        (rms, self.inner.seen_audio.load(Ordering::Relaxed) && quiet_ms < 500)
    }

    /// 取走全部样本，返回 (样本, 取走时的音频时钟 ms)
    pub fn drain_all(&self) -> (Vec<f32>, u64) {
        let mut ring = self.inner.ring.lock().unwrap();
        let out: Vec<f32> = ring.drain(..).collect();
        (out, AUDIO_MS.load(Ordering::Relaxed))
    }

    /// 从音频时钟 `start_ms` 起取走样本（更早的直接丢），
    /// 返回 (样本, 取走时的音频时钟 ms, 因环形缓冲已被覆盖而丢掉的毫秒数)
    pub fn drain_from_ms(&self, start_ms: u64) -> (Vec<f32>, u64, u64) {
        let mut ring = self.inner.ring.lock().unwrap();
        let now_ms = AUDIO_MS.load(Ordering::Relaxed);
        let rate = self.rate.max(1) as u64;
        let low_ms = now_ms.saturating_sub(ring.len() as u64 * 1000 / rate);
        let dropped_ms = low_ms.saturating_sub(start_ms);
        let want_ms = start_ms.max(low_ms);
        let skip = (((want_ms - low_ms) as u128 * rate as u128 / 1000) as usize).min(ring.len());
        ring.drain(..skip);
        let out: Vec<f32> = ring.drain(..).collect();
        (out, now_ms, dropped_ms)
    }

    pub fn stop(mut self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn open_stream(device_name: &str, inner: &Arc<Inner>) -> Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = if device_name.trim().is_empty() {
        host.default_input_device()
            .ok_or_else(|| anyhow!("未找到默认录音设备"))?
    } else {
        let want = device_name.to_ascii_lowercase();
        host.input_devices()
            .context("枚举录音设备失败")?
            .find(|d| d.to_string().to_ascii_lowercase().contains(&want))
            .ok_or_else(|| anyhow!("未找到名称包含「{device_name}」的录音设备"))?
    };

    let supported = device
        .default_input_config()
        .context("获取设备默认输入配置失败")?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.config();
    let channels = config.channels as usize;
    let rate = config.sample_rate;

    inner.rate.store(rate, Ordering::Relaxed);
    inner.channels.store(channels as u32, Ordering::Relaxed);
    inner
        .cap
        .store((RING_SECONDS * rate as f32) as u32, Ordering::Relaxed);
    // 新流从「现在」开始计时，否则会被误判成停流（刚开的流当然还没有音频）
    inner
        .last_audio
        .store(AUDIO_MS.load(Ordering::Relaxed), Ordering::Relaxed);
    inner.last_audio_wall.store(wall_ms(), Ordering::Relaxed);
    inner.seen_audio.store(false, Ordering::Relaxed);

    tracing::info!(
        "录音设备「{}」{}Hz {}ch {}",
        device,
        rate,
        channels,
        sample_format
    );

    // 流错误回调只记日志：本机每次开流都会报一次 `buffer underrun/overrun`，
    // 当“损坏”信号用会引发重开风暴；真正的判据是 is_broken() 的停流检测
    let err_fn = move |e| tracing::warn!("录音流错误（仅记录，不影响音频）: {e}");
    let stream = match sample_format {
        SampleFormat::F32 => build::<f32>(&device, &config, inner.clone(), err_fn)?,
        SampleFormat::I16 => build::<i16>(&device, &config, inner.clone(), err_fn)?,
        SampleFormat::U16 => build::<u16>(&device, &config, inner.clone(), err_fn)?,
        SampleFormat::I32 => build::<i32>(&device, &config, inner.clone(), err_fn)?,
        SampleFormat::F64 => build::<f64>(&device, &config, inner.clone(), err_fn)?,
        other => return Err(anyhow!("不支持的采样格式: {other}")),
    };
    Ok(stream)
}

fn build<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    inner: Arc<Inner>,
    err_fn: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream>
where
    T: Sample + SizedSample,
    f32: FromSample<T>,
{
    let channels = config.channels as usize;
    let stream = device
        .build_input_stream::<T, _, _>(
            config.clone(),
            move |data: &[T], _| inner.push(data, channels),
            err_fn,
            None,
        )
        .context("创建录音流失败")?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_inner() -> Inner {
        Inner {
            ring: Mutex::new(VecDeque::new()),
            cap: AtomicU32::new(16000),
            stop: AtomicBool::new(false),
            last_audio: AtomicU64::new(0),
            last_audio_wall: AtomicU64::new(0),
            seen_audio: AtomicBool::new(false),
            last_rms_bits: AtomicU32::new(0),
            rate: AtomicU32::new(16000),
            channels: AtomicU32::new(1),
        }
    }

    /// 链路卡片拿这个 RMS 当“麦到底在不在收声音”，算错会误导排查
    #[test]
    fn push_computes_block_rms() {
        let inner = test_inner();
        // 幅度 0.5 的正弦 → RMS = 0.5/√2 ≈ 0.3536
        let mono: Vec<f32> = (0..1600).map(|i| 0.5 * (i as f32 * 0.1).sin()).collect();
        inner.push(&mono, 1);
        let rms = f32::from_bits(inner.last_rms_bits.load(Ordering::Relaxed));
        assert!((rms - 0.3536).abs() < 0.02, "单声道 RMS 算错了: {rms}");
        assert!(inner.seen_audio.load(Ordering::Relaxed));
        assert_eq!(inner.ring.lock().unwrap().len(), 1600, "环形缓冲必须照旧收到样本");

        // 立体声同值：降单声道后幅度不变
        let stereo: Vec<f32> = (0..3200)
            .map(|i| 0.5 * ((i / 2) as f32 * 0.1).sin())
            .collect();
        inner.push(&stereo, 2);
        let rms2 = f32::from_bits(inner.last_rms_bits.load(Ordering::Relaxed));
        assert!((rms2 - 0.3536).abs() < 0.02, "立体声 RMS 算错了: {rms2}");
        assert_eq!(inner.ring.lock().unwrap().len(), 1600 + 1600);
    }

    /// 底噪（很小但不为零）不能被取整成 0 —— 那会让卡片无法区分“安安静静”与“麦坏了”
    #[test]
    fn tiny_rms_is_kept() {
        let inner = test_inner();
        let quiet: Vec<f32> = (0..1600)
            .map(|i| 0.0007 * (i as f32 * 0.3).sin())
            .collect();
        inner.push(&quiet, 1);
        let rms = f32::from_bits(inner.last_rms_bits.load(Ordering::Relaxed));
        assert!(rms > 0.0004 && rms < 0.001, "底噪 RMS 丢了: {rms}");
    }
}

/// 有状态流式重采样器（设备采样率 → 16k）
///
/// 整数倍降采样走盒式平均（自带低通，抗混叠优于线性插值）；
/// 其余比例走线性插值。跨调用保持状态，可无限续喂。
pub struct StreamResampler {
    from: u32,
    to: u32,
    factor: usize,
    pending: Vec<f32>,
    frac: f64,
}

impl StreamResampler {
    pub fn new(from: u32, to: u32) -> Self {
        let factor = if from > to && from % to == 0 {
            (from / to) as usize
        } else {
            0
        };
        Self {
            from,
            to,
            factor,
            pending: Vec::new(),
            frac: 0.0,
        }
    }

    /// 喂入一段输入，追加输出到 `out`
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.from == self.to {
            out.extend_from_slice(input);
            return;
        }
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(input);

        if self.factor > 0 {
            let n = buf.len() / self.factor;
            for i in 0..n {
                let s = &buf[i * self.factor..(i + 1) * self.factor];
                out.push(s.iter().sum::<f32>() / self.factor as f32);
            }
            self.pending = buf[n * self.factor..].to_vec();
        } else {
            let ratio = self.from as f64 / self.to as f64;
            // saturating_sub：空 buf 时 len-1 会下溢 panic
            while self.frac + ratio < buf.len().saturating_sub(1) as f64 {
                let i0 = self.frac.floor() as usize;
                let f = (self.frac - i0 as f64) as f32;
                let a = buf[i0];
                let b = buf[i0 + 1];
                out.push(a + (b - a) * f);
                self.frac += ratio;
            }
            let consumed = self.frac.floor() as usize;
            if consumed > 0 {
                buf.drain(..consumed);
                self.frac -= consumed as f64;
            }
            self.pending = buf;
        }
    }

    /// 收尾：把残留样本按同样规则吐出
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        let rest = std::mem::take(&mut self.pending);
        if !rest.is_empty() {
            self.push(&rest, out);
        }
    }
}

/// 一次性重采样（wav 文件用）
pub fn resample_to_16k(input: &[f32], from: u32) -> Vec<f32> {
    if input.is_empty() || from == 0 {
        return Vec::new();
    }
    let mut rs = StreamResampler::new(from, TARGET_RATE);
    let mut out = Vec::with_capacity(input.len() * TARGET_RATE as usize / from as usize + 8);
    rs.push(input, &mut out);
    rs.flush(&mut out);
    out
}

/// 均方根（静音判定用）
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}

/// 列出输入设备（GUI 下拉框 / --devices 用）
pub fn list_input_devices() -> Vec<String> {
    let host = cpal::default_host();
    let default = host.default_input_device().map(|d| d.to_string());
    let mut out = Vec::new();
    if let Ok(devs) = host.input_devices() {
        for d in devs {
            let name = d.to_string();
            if Some(&name) == default.as_ref() {
                out.push(name);
            } else {
                out.push(name);
            }
        }
    }
    out
}

pub fn default_input_device_name() -> Option<String> {
    cpal::default_host().default_input_device().map(|d| d.to_string())
}

/// 读取 wav 文件为 16k 单声道（CLI --once / --selftest 用）
pub fn read_wav_16k(path: &std::path::Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("打开 wav 失败: {}", path.display()))?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    let mut interleaved: Vec<f32> = Vec::new();
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for s in reader.samples::<f32>() {
                interleaved.push(s.context("读取采样失败")?);
            }
        }
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            for s in reader.samples::<i32>() {
                interleaved.push(s.context("读取采样失败")? as f32 / max);
            }
        }
    }
    let mut mono = Vec::with_capacity(interleaved.len() / channels.max(1));
    if channels <= 1 {
        mono = interleaved;
    } else {
        for frame in interleaved.chunks_exact(channels) {
            mono.push(frame.iter().sum::<f32>() / channels as f32);
        }
    }
    Ok(resample_to_16k(&mono, spec.sample_rate))
}

/// 编码 16-bit PCM wav（云端上传用）
pub fn encode_wav(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::new(&mut buf, spec).context("创建 wav 写入器失败")?;
        for &s in samples {
            let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            w.write_sample(v).context("写 wav 采样失败")?;
        }
        w.finalize().context("完成 wav 写入失败")?;
    }
    Ok(buf.into_inner())
}
