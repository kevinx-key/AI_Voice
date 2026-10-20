# 中英混说（code-switching）横评

目的：用同一批音频横评多个本地 ASR 引擎，指标 = **相似度 / 英文术语命中率 / RTF（速度）**。

## 1. 录音（20 条）

把 `ref.tsv` 里的 20 句，逐句**照着念**，录成同名 wav 放进本目录（`01.wav` … `20.wav`）：

- 采样率 16k（其他采样率也能读，程序会重采样）、单声道最佳
- 用 `listen.exe` 自带的录音也行，任何录音工具都行
- 念的时候按你平时说话的习惯（中英混着说），别刻意放慢

## 2. 横评

```powershell
.\target\release\listen.exe --bench K:\AI_Voice_rs\tests\codeswitch
```

模型清单取自 `config.toml` 的 `[[bench.models]]`（出厂默认：SenseVoice / FunASR-Nano fp16 / FunASR-Nano fp16 +热词）。改 `config.toml` 即可增删/切换：

```toml
[[bench.models]]
name = "FunASR-Nano fp16 +热词"
type = "funasr-nano"
dir = "K:\\AI_Voice\\models\\asr\\sherpa-onnx-funasr-nano-fp16-2025-12-30"
hotwords = true      # 用 [glossary] 词表做 LLM prompt 热词偏置
```

`ref.tsv` 里第 3 列是**该句必须命中的英文术语**（分号分隔），命中率越高说明 code-switch 越好。

> 想让"+热词"那行生效，`config.toml` 的 `[glossary]` 需 `enabled = true` 且 `terms` 含目标词。
> **热词要精简聚焦**：FunASR-Nano 的 LLM KV 上限小（本机导出 512），词表过长会挤掉音频 token
> 导致空输出/截短（程序已限 30 条）；实测**把整个 glossary（100+ 词）当热词反而会伤到普通句**
> （纯中文句被截短）。建议 `[glossary].terms` 只放你真正在乎的术语（几十个以内）。

## 3. 已观察（用 FunASR-Nano 自带 test_wavs 的日英混说做的代理测试）

| 模型 | 日英混说相似度 | 英文术语命中 | RTF |
|------|:---:|:---:|:---:|
| SenseVoice | 76% | 0/2 | 0.025 |
| FunASR-Nano fp16 | 74% | 0/2 | 0.37 |
| FunASR-Nano fp16 +热词 | **97%** | **2/2** | 0.36 |

结论：**不加词表时，中/英模型都会把英文词转写成中文/日文假名；FunASR-Nano 的 prompt 热词能把英文原词"拽回来"。**
int8 版实测明显劣化（漏字/乱码），已改用 fp16。
