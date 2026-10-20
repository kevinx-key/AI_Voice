//! 运行状态机：idle → recording → transcribing → polishing → injecting → idle

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Recording,
    Transcribing,
    // 整理/注入已移到独立"输出线程"，录音线程不再构造这两个状态；
    // 保留枚举值供界面文案/颜色表使用（未来若把状态回传界面可复用）。
    #[allow(dead_code)]
    Polishing,
    #[allow(dead_code)]
    Injecting,
    Error,
}

impl State {
    pub fn label(&self) -> &'static str {
        match self {
            State::Idle => "待机",
            State::Recording => "录音中",
            State::Transcribing => "识别中",
            State::Polishing => "整理中",
            State::Injecting => "注入中",
            State::Error => "错误",
        }
    }

    /// 状态圆点颜色（RGBA）
    pub fn rgba(&self) -> [u8; 4] {
        match self {
            State::Idle => [140, 148, 160, 255],
            State::Recording => [232, 64, 64, 255],
            State::Transcribing => [240, 176, 48, 255],
            State::Polishing => [72, 144, 240, 255],
            State::Injecting => [64, 192, 112, 255],
            State::Error => [208, 48, 160, 255],
        }
    }
}
