# 第三方组件与许可

本项目自身代码以 **MIT** 发布（见 `LICENSE`）。运行时/构建期用到或随安装包分发以下第三方组件，
它们的许可各自独立：

| 组件 | 用途 | 许可 | 说明 |
|---|---|---|---|
| [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) | 本地语音识别引擎（静态链接） | Apache-2.0 | `vendor/sherpa-onnx-sys` 为本地 patch（去掉 TTS 相关依赖，仅保留 ASR），未修改其许可 |
| [sherpa-onnx-sys](https://github.com/thewh1teagle/sherpa-rs) 绑定 | Rust 绑定 | Apache-2.0 | 同上 |
| [eframe / egui](https://github.com/emilk/egui) | 设置界面 | MIT OR Apache-2.0 | 由 Cargo 依赖引入 |
| 其余 Rust crates | — | 见 `Cargo.lock` 各 crate 元数据 | 未做二次分发修改 |
| [Rime / Weasel](https://github.com/rime/weasel)（含本项目 fork：<https://github.com/kevinx-key/K3R-voice>） | 键盘输入法引擎（**不在本仓库，安装包内单独分发**） | GPL-3.0 | 本仓库只调用其安装器/部署接口，**不静态链接**；若再分发安装包，需同时提供对应 GPL 源码（即上面的 fork 仓库） |
| [rime-ice](https://github.com/iDvel/rime-ice) 等方案包、万象语法模型 | 输入法方案与语言模型（**不在本仓库，安装包内单独分发**） | 各自上游许可（GPL-3.0 等） | 同上，安装包再分发时遵循上游许可 |
| Qwen / SenseVoice / Paraformer / FunASR-Nano 模型权重 | 本地 LLM 与 ASR 模型（**不在本仓库**，首次运行下载） | 各模型上游许可 | 本仓库只提供下载/加载逻辑 |

## 本仓库不包含

为避免许可与体积问题，以下内容用 `.gitignore` 排除，**不在源码仓库内**：

- `resources/`（约 575MB：Rime 安装器、方案包、语法模型）
- `models/`（ASR 模型权重）
- `dist/`（构建产物与安装包）
- `target/`、`.cache/`
- `config.toml`（本机配置，可能含 API Key 与本地路径）

需要完整离线包时运行 `installer/fetch_resources.ps1`（自带镜像与校验）或使用 release 里的安装包。