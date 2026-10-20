//! 长语音分段器
//!
//! 两种切段时机（谁先满足用谁）：
//! 1) 缓冲达到 `target`（最长段）：切点吸附到目标点前一小段里**能量最低**处（≈自然停顿）
//! 2) 尾部静音达到 `silence_hold`：立即从静音起点切断 → "说完一句就上屏"
//!
//! SenseVoice 非流式，靠缩短段长获得实时感；段太短会略降精度，故设 `min_len` 下限。
//!
//! 噪声底自适应（2026-10-06）：原先用固定 `min_speech_rms` 判"有没有语音"，
//! 实测本机麦克风闾声底就在 0.0015 附近 → 永远所有窗口都算"有声" → 静音断句从不触发
//! （120 次会话里 35/35 首段都卡在 3.06~3.15s）。现在改为会话内估计噪声底
//! （只看得到的最安静窗口，回升极慢），阈值 = max(配置下限, 噪声底×2.5)。

/// 静音断句的最小成段长度（秒）：低于它一律不切，继续攒
///
/// 动机（2026-10-06 装机实测）：自适应阀值生效后，0.2~0.4s 的碎片也会被切出来送 ASR，
/// 而 SenseVoice/Paraformer 在 <0.5s 音频上会**整段幻觉**：实测 0.3s → “i u n u n u n y”、
/// 0.2s → “啊的我的我的你的”、0.4s → 25 字胡话，而且会被直接注入。
/// 碎片留在缓冲里跟后面语音合并即可（不会丢）。
const SILENCE_CUT_MIN_SECONDS: f32 = 0.8;
/// 静音断句前至少要观测到这么多“语音窗”（步进 100ms → 约 300ms 语音证据）
///
/// 动机：一个按键声/呼吸就能让 1~2 个窗越过门限，如果凭这个就切段，
/// 切出来的就是一段噪声碎片（又是幻觉燃料）。
const MIN_LOUD_WINDOWS: usize = 3;

pub struct Segmenter {
    buf: Vec<f32>,
    target: usize,
    lookback: usize,
    win: usize,
    /// 触发静音断句前，至少要有这么长的音频（含语音）
    min_len: usize,
    /// 静音断句的最小成段长度（见 SILENCE_CUT_MIN_SECONDS）
    silence_cut_min: usize,
    /// 尾部静音阈值长度（采样点）
    silence_hold: usize,
    /// 配置下限：低于此值一律当静音（防全零录制时阈值退化成 0）
    floor_rms: f32,
    /// 会话内噪声底估计（窗口 RMS）：只跟随更安静的观测，回升极慢
    floor: f32,
    /// 本会话观测到的最静窗口 RMS（动态范围判据用）
    min_win: f32,
    /// 本会话观测到的最大窗口 RMS
    peak_win: f32,
    /// 最近一次切段原因（target | silence | flush），仅用于日志
    last_cut: &'static str,
}

impl Segmenter {
    /// - `target_seconds`：最大段长（秒）
    /// - `silence_seconds`：尾部静音达到此长度即提前断句（0 = 关闭）
    /// - `min_seconds`：静音断句所需的最短音频（秒）
    /// - `min_speech_rms`：窗口 RMS 的判断下限（自适应阈值不会低于它）
    pub fn new(
        target_seconds: u32,
        sample_rate: u32,
        silence_seconds: f32,
        min_seconds: f32,
        min_speech_rms: f32,
    ) -> Self {
        let rate = sample_rate.max(1);
        let target = (target_seconds.max(1) as usize) * rate as usize;
        // 允许回溯到目标点前 3 秒找停顿
        let lookback = ((3 * rate) as usize).min(target / 2).max(1);
        // 能量窗口 200ms
        let win = (rate as usize / 5).max(1);
        let min_len = ((min_seconds.max(0.1) * rate as f32) as usize).max(win);
        let silence_cut_min = ((SILENCE_CUT_MIN_SECONDS * rate as f32) as usize)
            .max(min_len)
            .max(win);
        let silence_hold = ((silence_seconds.max(0.0) * rate as f32) as usize).max(win);
        let floor_rms = min_speech_rms.max(0.0);
        Self {
            buf: Vec::with_capacity(target + rate as usize),
            target,
            lookback,
            win,
            min_len,
            silence_cut_min,
            silence_hold,
            floor_rms,
            floor: f32::INFINITY,
            min_win: f32::INFINITY,
            peak_win: 0.0,
            last_cut: "",
        }
    }

    /// 当前自适应"有声"阈值：噪声底 ×2.5，但不低于配置下限
    pub fn speech_threshold(&self) -> f32 {
        (self.noise_floor() * 2.5).max(self.floor_rms)
    }

    /// 当前噪声底估计（未观测到时退回配置下限）
    pub fn noise_floor(&self) -> f32 {
        if self.floor.is_finite() {
            self.floor.max(self.floor_rms)
        } else {
            self.floor_rms
        }
    }

    /// 本会话是否出现过真正的“语音窗”
    ///
    /// 用途：纯噪声会话（用户按了键但没说话）会被 SenseVoice 幻觉出一整句
    /// （实测 3.7s 环境音 → “假设这个已经失败了”并直接注入）。
    ///
    /// 判据用**动态范围 + 绝对电平**而非单一阈值（单一阈值总有一头翻车）：
    /// - 人声起伏大：峰值窗 ≥ 最静窗（或配置下限）×2.2
    /// - 或者电平明显高于配置静音门限（均匀说话、没有明显静音窗时兜底）
    ///
    /// 误判“没说话”会丢整句，所以两个条件任一满足就算有语音（宁多勿少）。
    pub fn saw_speech(&self) -> bool {
        let base = self.min_win.max(self.floor_rms);
        let dynamic = self.peak_win >= base * 2.2;
        let loud = self.peak_win >= self.floor_rms * 8.0;
        (dynamic || loud) && self.peak_win >= self.floor_rms * 2.0
    }

    /// 最近一次切段原因（target | silence | flush）
    pub fn last_cut_reason(&self) -> &'static str {
        self.last_cut
    }

    /// 缓冲里还有多少未切出的样本。
    ///
    /// `--gate` 用它区分两种「没切出段」：
    /// - `flush()` 返回 None 且这里 > 0：**有内容但被证据闸丢弃**（真被拦了）
    /// - `flush()` 返回 None 且这里 == 0：压根没东西可收尾（空转，不能算门控的功劳）
    pub fn pending_len(&self) -> usize {
        self.buf.len()
    }

    pub fn push(&mut self, samples: &[f32]) {
        self.buf.extend_from_slice(samples);
    }

    /// 缓冲达到目标长度，或尾部静音够长时切出一段
    pub fn take_segment(&mut self) -> Option<Vec<f32>> {
        self.refresh_floor();
        self.trim_leading_silence();
        if self.buf.len() >= self.target {
            let cut = find_quiet_cut(&self.buf, self.target - self.lookback, self.target, self.win);
            self.last_cut = "target";
            return Some(self.buf.drain(..cut).collect());
        }
        if self.silence_hold > 0 && self.buf.len() >= self.min_len + self.silence_hold {
            let threshold = self.speech_threshold();
            if let Some(cut) =
                trailing_silence_cut(&self.buf, self.win, self.silence_hold, threshold)
            {
                // 两道闸：段不能太短 + 切点前必须有足够的“语音证据”，否则继续攒
                if cut >= self.silence_cut_min
                    && self.loud_windows_before(cut, threshold) >= MIN_LOUD_WINDOWS
                {
                    self.last_cut = "silence";
                    return Some(self.buf.drain(..cut).collect());
                }
            }
        }
        None
    }

    /// `[0, end)` 里越过阀值的窗口数（判“切点前真的有语音”用）
    fn loud_windows_before(&self, end: usize, threshold: f32) -> usize {
        let win = self.win;
        let step = (win / 2).max(1);
        let hi = end.min(self.buf.len());
        let mut i = 0usize;
        let mut n = 0usize;
        while i + win <= hi {
            if crate::audio::rms(&self.buf[i..i + win]) >= threshold {
                n += 1;
            }
            i += step;
        }
        n
    }

    /// 收尾：吐出剩余全部
    ///
    /// 带语音证据闸门：只剩噪声/碎片的尾巴直接丢弃（否则 0.4s 碎片会被送 ASR，
    /// 实测会幻觉出“异议”这种两字而直接注入）。
    pub fn flush(&mut self) -> Option<Vec<f32>> {
        self.refresh_floor();
        if self.buf.is_empty() {
            return None;
        }
        let threshold = self.speech_threshold();
        let n = self.loud_windows_before(self.buf.len(), threshold);
        if n < MIN_LOUD_WINDOWS {
            tracing::info!("收尾段语音证据不足（{n} 个语音窗），当噪声丢弃");
            self.buf.clear();
            return None;
        }
        self.last_cut = "flush";
        Some(std::mem::take(&mut self.buf))
    }

    /// 噪声底估计（每次切段前跑一遍；开销 = 每 200ms 窗一次 RMS）
    fn refresh_floor(&mut self) {
        let win = self.win;
        let step = (win / 2).max(1);
        let n = self.buf.len();
        let mut i = 0usize;
        while i + win <= n {
            let r = crate::audio::rms(&self.buf[i..i + win]);
            if r < self.min_win {
                self.min_win = r;
            }
            if r > self.peak_win {
                self.peak_win = r;
            }
            // 更安静 → 立刻跟随；只有"看起来是静音"的窗口才允许噪声底缓慢回升
            if r < self.floor {
                self.floor = r;
            } else if self.floor.is_finite() && r < self.speech_threshold() {
                self.floor = self.floor * 0.99 + r * 0.01;
            }
            i += step;
        }
    }

    /// 裁掉开头静音：避免"前导静音占满第一个 target 窗口"
    /// （实测 07:39:14：3.2s 会话里前 1.7s 静音被切成一整段丢掉，真正的语音又得多等一个窗口）
    ///
    /// 只在"确实有后续有声窗口"时才裁；且要求连续 ≥ 3 个静音窗（≈600ms），
    /// 避免把轻声起音当噪声吃掉。
    fn trim_leading_silence(&mut self) {
        let win = self.win;
        let step = (win / 2).max(1);
        let threshold = self.speech_threshold();
        let n = self.buf.len();
        let mut i = 0usize;
        let mut quiet_run = 0usize;
        let mut cut: Option<usize> = None;
        while i + win <= n {
            if crate::audio::rms(&self.buf[i..i + win]) < threshold {
                quiet_run += 1;
            } else {
                if quiet_run >= 3 {
                    cut = Some(i);
                }
                break;
            }
            i += step;
        }
        if let Some(c) = cut {
            if c > 0 {
                self.buf.drain(..c);
            }
        }
    }
}

/// 尾部静音检测：返回"最后一个有语音窗口的结束位置"（即静音起点），
/// 要求该点之后确有 >= `hold` 的静音，否则 None。
fn trailing_silence_cut(buf: &[f32], win: usize, hold: usize, threshold: f32) -> Option<usize> {
    let n = buf.len();
    if n < win * 2 {
        return None;
    }
    let step = (win / 2).max(1);
    let mut last_loud_end = 0usize;
    let mut i = 0;
    while i + win <= n {
        if crate::audio::rms(&buf[i..i + win]) >= threshold {
            last_loud_end = i + win;
        }
        i += step;
    }
    if last_loud_end == 0 {
        return None; // 全静音：不切，交给 flush/门控丢弃
    }
    if n - last_loud_end >= hold {
        Some(last_loud_end)
    } else {
        None
    }
}

/// 在 `[lo, hi)` 内找长度为 `win` 的最低能量窗口，返回其中点作为切点
fn find_quiet_cut(buf: &[f32], lo: usize, hi: usize, win: usize) -> usize {
    let hi = hi.min(buf.len());
    let lo = lo.max(1);
    if hi <= lo + win {
        return hi;
    }
    let step = (win / 2).max(1);
    let mut best_e = f64::MAX;
    let mut best_at = lo + win / 2;
    let mut i = lo;
    while i + win <= hi {
        let e: f64 = buf[i..i + win]
            .iter()
            .map(|&s| (s as f64) * (s as f64))
            .sum();
        if e < best_e {
            best_e = e;
            best_at = i + win / 2;
        }
        i += step;
    }
    best_at.clamp(1, buf.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::StreamResampler;

    #[test]
    fn cuts_at_quiet_point() {
        let rate = 16000u32;
        let mut seg = Segmenter::new(2, rate, 0.5, 0.4, 0.006);
        // 目标切点 2.0s，回溯窗口 [1.0s, 2.0s]；在 1.8~2.0s 放一段静音
        let loud = vec![0.5f32; (rate as f32 * 1.8) as usize];
        let quiet = vec![0.0001f32; (rate / 5) as usize];
        let tail = vec![0.5f32; rate as usize / 2];
        seg.push(&loud);
        seg.push(&quiet);
        seg.push(&tail);
        let s = seg.take_segment().expect("should cut");
        let cut_s = s.len() as f32 / rate as f32;
        assert!((1.85..=1.95).contains(&cut_s), "cut at {cut_s}s");
    }

    #[test]
    fn no_cut_before_target() {
        let rate = 16000u32;
        let mut seg = Segmenter::new(2, rate, 0.5, 0.4, 0.006);
        seg.push(&vec![0.5f32; rate as usize]); // 只喂 1s
        assert!(seg.take_segment().is_none());
    }

    #[test]
    fn cuts_on_trailing_silence() {
        // 目标 8s，但说完 1.5s + 0.6s 静音 → 应提前切出（"说完就上屏"）
        let rate = 16000u32;
        let mut seg = Segmenter::new(8, rate, 0.5, 0.4, 0.006);
        seg.push(&vec![0.5f32; (rate as f32 * 1.5) as usize]);
        seg.push(&vec![0.0f32; (rate as f32 * 0.6) as usize]);
        let s = seg.take_segment().expect("应在静音处提前切段");
        let cut_s = s.len() as f32 / rate as f32;
        assert!((1.4..=1.6).contains(&cut_s), "cut at {cut_s}s");
    }

    #[test]
    fn no_cut_on_pure_silence() {
        // 纯静音不应触发静音断句（交给门控/收尾丢弃）
        let rate = 16000u32;
        let mut seg = Segmenter::new(8, rate, 0.5, 0.4, 0.006);
        seg.push(&vec![0.0f32; (rate as f32 * 2.0) as usize]);
        assert!(seg.take_segment().is_none());
    }

    #[test]
    fn no_fragment_cut_on_blip() {
        // 装机实测回归：一个“咯嗒”/呼吸（1~2 个 200ms 窗）越门限后接长静音，
        // 旧逻辑会切出 0.2~0.4s 碎片段 → SenseVoice/Paraformer 整段幻觉并注入。
        let rate = 16000u32;
        let mut seg = Segmenter::new(25, rate, 0.5, 0.2, 0.0015);
        seg.push(&vec![0.004f32; (rate as f32 * 0.6) as usize]); // 0.6s 噪声底
        seg.push(&vec![0.05f32; (rate as f32 * 0.2) as usize]); // 0.2s 一声响
        seg.push(&vec![0.004f32; (rate as f32 * 1.2) as usize]); // 1.2s 静下来
        assert!(
            seg.take_segment().is_none(),
            "0.2s 碎片不应被切出来送 ASR（那是幻觉燃料）"
        );
        // 真语音（≥0.8s 且够多语音窗）+ 尾巴静音 → 仍要正常切
        let mut ok = Segmenter::new(25, rate, 0.5, 0.2, 0.0015);
        ok.push(&vec![0.004f32; (rate as f32 * 0.5) as usize]);
        ok.push(&vec![0.05f32; (rate as f32 * 1.2) as usize]);
        ok.push(&vec![0.004f32; (rate as f32 * 0.8) as usize]);
        let s = ok.take_segment().expect("真实语音应正常静音断句");
        let secs = s.len() as f32 / rate as f32;
        // 段长 ≈ 1.2s 语音（开头那 0.5s 噪声已被前导静音裁剪吃掉）
        assert!((1.0..=1.5).contains(&secs), "cut at {secs}s");
        assert_eq!(ok.last_cut_reason(), "silence");
    }

    #[test]
    fn saw_speech_flags_pure_noise() {
        // 纯噪声会话：用户按了键但没说话 → 不能交给 ASR（会幻觉出整句）
        let rate = 16000u32;
        let mut noise = Segmenter::new(25, rate, 0.5, 0.4, 0.0015);
        noise.push(&vec![0.003f32; (rate as f32 * 3.0) as usize]);
        noise.refresh_floor();
        assert!(!noise.saw_speech(), "纯噪声不应被当成有语音");

        // 噪声 + 人声 → 应标记有语音
        let mut mixed = Segmenter::new(25, rate, 0.5, 0.4, 0.0015);
        mixed.push(&vec![0.003f32; (rate as f32 * 1.0) as usize]);
        mixed.push(&vec![0.05f32; (rate as f32 * 1.0) as usize]);
        mixed.refresh_floor();
        assert!(mixed.saw_speech(), "人声应被识别成语音窗");

        // 均匀的大声（没明显静音窗）也不能误判成“没说话”
        let mut flat = Segmenter::new(25, rate, 0.5, 0.4, 0.0015);
        flat.push(&vec![0.03f32; (rate as f32 * 2.0) as usize]);
        flat.refresh_floor();
        assert!(flat.saw_speech(), "均匀人声不能丢");
    }

    #[test]
    fn adaptive_threshold_cuts_with_noisy_mic() {
        // 实测本机闾声底就在 min_speech_rms(0.0015) 之上 → 固定阈值下静音断句从不触发。
        // 自适应后：闾声底 0.004、说话 0.05、尾部静音 0.6s → 应在 1.5s 处切开。
        let rate = 16000u32;
        let mut seg = Segmenter::new(8, rate, 0.5, 0.4, 0.0015);
        let speech = vec![0.05f32; (rate as f32 * 1.5) as usize];
        let noise = vec![0.004f32; (rate as f32 * 0.6) as usize];
        seg.push(&speech);
        seg.push(&noise);
        let s = seg.take_segment().expect("闾声底之上应能识别出静音起点");
        let cut_s = s.len() as f32 / rate as f32;
        assert!((1.4..=1.6).contains(&cut_s), "cut at {cut_s}s");
        assert_eq!(seg.last_cut_reason(), "silence");
        assert!(seg.speech_threshold() > 0.0015, "阈值应该抬到闾声底之上");
    }

    #[test]
    fn leading_silence_is_trimmed() {
        // 前导静音不应占满第一个 target 窗口（实机：3.2s 会话里前 1.7s 静音被切段丢弃）
        let rate = 16000u32;
        let mut seg = Segmenter::new(8, rate, 0.5, 0.4, 0.0015);
        seg.push(&vec![0.004f32; (rate as f32 * 1.0) as usize]); // 1.0s 闾声
        seg.push(&vec![0.05f32; (rate as f32 * 1.0) as usize]); // 1.0s 说话
        assert!(seg.take_segment().is_none()); // target 8s 未到、尾部静音不够
        let rest = seg.flush().expect("应有剩余音频");
        let secs = rest.len() as f32 / rate as f32;
        assert!(secs < 1.5, "前导静音应被裁掉，剩 {secs}s");
    }

    #[test]
    fn resampler_roundtrip_length() {
        let mut rs = StreamResampler::new(48000, 16000);
        let mut out = Vec::new();
        let input = vec![0.1f32; 48000];
        rs.push(&input, &mut out);
        assert_eq!(out.len(), 16000);
    }

    #[test]
    fn resampler_is_chunk_agnostic() {
        let input: Vec<f32> = (0..48000).map(|i| (i as f32 * 0.001).sin()).collect();
        let mut a = Vec::new();
        let mut rs = StreamResampler::new(48000, 16000);
        rs.push(&input, &mut a);

        let mut b = Vec::new();
        let mut rs2 = StreamResampler::new(48000, 16000);
        for chunk in input.chunks(777) {
            rs2.push(chunk, &mut b);
        }
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert!((x - y).abs() < 1e-6, "{x} vs {y}");
        }
    }
}
