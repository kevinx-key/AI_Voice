# tests\gate —— 门控/分段回归集

> 用途：改门控（能量阈值 → VAD）时，把「改前 vs 改后」变成**可比数字**，
> 避免用"感觉好点了"当验收标准（这是本项目踩过的坑）。
> 跑分脚本：`python tools\gate_bench.py run --tag <标签>`
> 对比判定：`python tools\gate_bench.py compare 改前.json 改后.json`

## 目录约定

```
tests\gate\
  noise_only\         ① 纯底噪：按 F9 但不说话（3~5s）
  noise_soft_speech\  ② 底噪里轻声说话（说满一句）
  keyboard\           ③ 键盘敲击 / 鼠标点击（3~5s，不说法）
  normal_speech\      ④ 正常音量说话（说满一句）
```

每类 3~5 条，命名 `01.wav`、`02.wav` …（见下"怎么录"）。

> **⚠️ 时长必须 > 3 秒（推荐按住 4~5 秒）**。两个原因，都踩过：
> 1. `RING_SECONDS = 3.0`（`src/audio.rs:22`）是未取走样本的上限 ——
>    2026-10-08 前 `--record` 只在松手后取一次，按住 5s 也只落盘「最后 3 秒」，
>    **开头整段丢**，样本长度全卡在 3.00s（已修，并有 `tools\check_record_fix.py` 回归守着）。
> 2. 3.00s 恰好等于段目标 `segment_seconds`，切段永远只发生在同一个边界上，
>    回归集就退化成「反复测同一个边界」；不够 3s 的纯噪声甚至会**一次切段都不触发**（见"送 ASR"一列）。

## 怎么录（不用装任何录音软件）

本机没有 Audacity / ffmpeg / sox，Win10 自带「录音机」只能存 m4a，
所以 `listen.exe` 自带了一个录音入口（与正式会话**走同一条采集链路**，
所以录出来的样本能代表真实使用条件）。用引导脚本录（自动分配编号、录完立刻体检）：

```powershell
# ⚠️ 先退出正在运行的倾听输入法（托盘 → 退出程序），
#    否则它自己也会装一个 F9 钩子，你按一下就同时触发它的会话（往当前窗口注字）
cd K:\AI_Voice_rs
python tools\record_gate.py              # 四类各录到 3 条（已有的不重录）
python tools\record_gate.py --plan       # 只打印将要录哪些编号，不录
python tools\record_gate.py --list       # 只体检现有样本（时长/RMS/峰值）
python tools\record_gate.py --redo --per-class 4   # 原地重录纯编号文件 + 每类补到 4 条
```

单条等价命令（脚本内部就是调它）：

```powershell
K:\AI_Voice_rs\target\release\listen.exe --record K:\AI_Voice_rs\tests\gate\noise_only\01.wav
# 看到「按住「f9」开始录」后，按住 F9 录 4~5 秒、松手结束
```

录完它会打印 `时长 / 样本数 / RMS / 峰值`，可以据此判断这条样本合不合格：

| 类别 | 时长建议 | RMS 参考 |
|---|---|---|
| `noise_only` | 3~5s | 应和你房间的底噪同量级（本机实测安静时 ≈ 0.0007） |
| `keyboard` | 3~5s | 敲击瞬间会明显高于底噪 |
| `noise_soft_speech` | 说满一句 | 明显高于底噪，但低于正常说话 |
| `normal_speech` | 说满一句 | 最高 |

## 硬指标（`tools\gate_bench.py compare` 会按这个判 PASS/FAIL）

| 类别 | 指标 | 要求 | 为什么 |
|---|---|---|---|
| `noise_only` | 出字数 | **必须 = 0** | 治"底噪成字"；出字即幻觉 |
| `keyboard` | 出字数 | **必须 = 0** | 键盘/鼠标瞬变是幻觉的主要燃料 |
| `noise_soft_speech` | 出字数 | ≥ 改前的 90% 且 > 0 | 治"轻声说话被能量门吃掉"（末轮实测 3/21 会话出字 0 字） |
| `normal_speech` | 出字数 | ≥ 改前的 95% | 召回不能掉 |
| `normal_speech` | 段数 | ≤ 改前的 1.50 倍 | 段数暴涨 = 碎片化 = 幻觉风险 |

> 缺哪个类别就 SKIP 哪一行，不会误判；但**四类齐全**才能真的判定。

## 两个必须看的辅助列（`gate_bench.py run` 的表里）

| 列 | 含义 | 为什么关键 |
|---|---|---|
| **送 ASR**（`asr_segs`） | 通过判据、**真的送进识别模型**的段数 | 这才是幻觉暴露面。2026-10-08 实测：键盘噪声 **4/4 段全被送进 ASR**，只是 Paraformer 恰好吐空串 —— 出字数看不出问题，这个数看得见。**C1（VAD）要压的就是它** |
| 尾段丢弃（`tail_dropped`） | 收尾段「语音证据不足」被丢弃的次数 | 以前这类段既不计段也不计被闸，于是 `<3s` 的样本**根本没被测**、`chars=0` 是空转结论 |

## `before-vad` 基线（2026-10-08，16 条样本，装机配置 `segment_seconds=3`）

| 类别 | 文件 | 音频s | 切段 | 被闸 | 送ASR | 出字 | 尾段丢弃 |
|---|---|---|---|---|---|---|---|
| noise_only | 4 | 14.2 | 2 | 2 | 0 | 0 | 4 |
| keyboard | 4 | 25.6 | 6 | 2 | **4** | 0 | 4 |
| noise_soft_speech | 4 | 13.7 | 6 | 0 | 6 | 44 | 3 |
| normal_speech | 4 | 16.5 | 7 | 0 | 7 | 56 | 2 |

存盘：`tests\gate\gate-before-vad.json`。改完门控后用 `gate_bench.py compare` 判定
（硬指标那两行必须 PASS，辅助列用于解释"为什么变好了"）。

## `after-vad` 结果（2026-10-08，C1 落地后，同一批样本同一配置）

存盘：`tests\gate\gate-after-vad.json`（头部带 `门控=vad`，能看出实际跑的是哪个判据）。

| 类别 | 切段 | 被闸 | **送ASR** | 出字 | 对照（before 切段/送ASR/出字） |
|---|---|---|---|---|---|
| noise_only | 0 | 0 | **0** | 0 | 2 / 0 / 0 |
| keyboard | 0 | 0 | **0** ✅ | 0 | 6 / **4** / 0 |
| noise_soft_speech | 4 | 0 | 4 | **42** | 6 / 6 / 44 |
| normal_speech | 5 | 0 | 5 | **57** | 7 / 7 / 56 |

总判定 **PASS**。关键收益就是那个辅助列：**键盘噪声段「送 ASR」4 → 0**（之前 4/4 全穿过能量门）。
另：软/正常两类从「2 段」合并成「1 段」——VAD 在自然停顿处切，比能量门的定长切更整；
软类总字数 44 → 42（0.95x，在 0.90 闸内）。

## 备注

- `normal_speech` 目前只测"出字数"，不测字对不对（正确率另有 `--selftest` 跑固定参考文本）。
- 加/换麦克风、换房间之后，请重新录一遍（底噪变了，旧基线不再可比）。
- `--gate` 的 TSV 列名会被 `tools\gate_bench.py` **按头行读取**（不再写死下标）：加列只改 `src/main.rs` 的 `GateAcc` + 打印格式。
- 改采样链路后先跑 `python tools\check_record_fix.py`（合成按住 F9，验「按住多久 = 落盘多久」，不需要人）。
