# 倾听输入法 · AI 语音输入引擎（Rust 版）

单一二进制：本地语音识别 + 本地大模型整理 + 可自由定制的输入法。默认离线运行。

## 这是什么

两个入口，一套引擎：

- **输入法形态**：全局热键 → 流式采集 → 停顿切段 → 识别 → 整理 → 注入焦点窗口。
  长口述按可配置间隔（10 / 20 / 40 / 60 s）增量上屏，会话结束补齐剩余。
- **引擎形态**：`listen.exe` 同时是 CLI 与本地服务 —— 单文件转写、长音频分段、门控回归、
  模型横评、术语纠错自检、机读状态（`--status --json`）；`\\.\pipe\ListenCtl` 提供
  `PING / STATUS / CHAIN / SUSPEND / RESUME / SET / REPAIR / QUIT`，供其他程序驱动录音与切后端。

## 设计取舍

- **离线优先**：ASR（SenseVoice / Paraformer / FunASR-Nano 可切换）与整理（llama-server，5 档）
  都能在本机跑完。默认配置下不需要网络、账号或 API Key，语音数据不出本机；免费、无订阅。
- **输入自由度高**：输入语言、方案、词表、候选权重、按键行为都由配置决定，随改随生效。
  词条数不设上限：打字侧全量入表，录音侧按 `asr_top_n` 精选进识别提示（默认 30，上限 200）。
  支持凭历史用词自动调序，以及前缀联想。
- **零运行时依赖**：单文件 exe（38.6 MB，静态 sherpa-onnx + 静态 CRT）。无 Python、
  无 VC++ 运行库、无 DLL。
- **可观测、可恢复**：链路拆 8 环实时监控 + 一键修复；会话结束日志写明结束原因；
  崩溃留现场（`logs\last_panic.txt`）并自动重启一次；设置改动自动落盘。

| 指标 | 实测 |
|---|---|
| 处理速度 | 1200 s 连续语音 → 23.9 s（RTF **0.020**） |
| 单段识别 | 中位 60~90 ms（≤25 s 音频） |
| 模型加载 | 0.8~2.4 s；整理后端首次冷启 10~25 s，之后复用 |
| 内存 | 峰值不随时长增长（只保留当前段） |
| 回归 | 单测 **72/72**；E2E：录音方式 28/28、热键/挂起 17/17、崩溃自愈 8/8、悬浮窗 6/6 |

| 模块 | 状态 |
|---|---|
| 语音输入（热键 / 长口述 / 增量上屏） | ✅ |
| 文本整理（关闭 / 本地 / 云端，5 档） | ✅ |
| 输入法（方案部署 + 词表 + 用词学习 + 前缀联想） | ✅ |
| 引擎接口（CLI + ListenCtl + JSON 状态） | ✅ |
| 打包（绿色 zip + Inno 安装包 417 MB） | ✅ |
| macOS / Linux | ⏳ 二期 |

> 参照实现：`K:\AI_Voice`（Python 版，作为对照基线）

**项目文档（三文件协同）**：本文件（了解项目）· `调试计划.md`（未来开发：每次开工先读）· `项目调试过程.md`（沉默物料：历史调试与踩坑全集，出问题或回溯时读）。历史计划/交接文档留档在 `docs\archive\`。

---

## 快速开始

前置：Windows 10/11 x64、Rust stable（`x86_64-pc-windows-msvc`）+ VS Build Tools，
以及 sherpa-onnx 的 **no-tts 静态库**（解压到 `.cache/`，见文末「构建环境」）。
不想自己编就直接用 release 里的安装包。

```powershell
git clone https://github.com/kevinx-key/AI_Voice.git
cd AI_Voice

# 一次性：把 sherpa-onnx no-tts 静态库解压到 .cache/
#   https://github.com/k2-fsa/sherpa-onnx/releases → *-static-MT-Release-no-tts-lib.tar.bz2
#   （.cargo/config.toml 里的 SHERPA_ONNX_LIB_DIR 已指向 .cache/<解压目录>/lib）

cargo build --release
.\target\release\listen.exe
```

启动后：

- 弹出 **设置界面**（无黑框控制台）
- 托盘出现一个圆点图标

| 托盘颜色 | 状态 |
|---------|------|
| 灰 | 待机 |
| 红 | 录音中 |
| 橙 | 识别中 |
| 蓝 | 整理中 |
| 绿 | 注入中 |
| 紫 | 错误（1.2s 后回待机） |

**按住 `F9` 说中文 → 松开出字**。`Shift+F9` 撤销上一次粘贴并换成 ASR 原文。

---

## 长语音口述（重点）

说 20 分钟也不会失败，而且**每隔一会儿就把文字送进目标窗口**，不用等全部说完。

原理：音频**流式**采集 → 按 ~25 秒切段（切点吸附到附近能量最低处 ≈ 自然停顿）→
每段立刻识别 → 累计满「出字间隔」就整理+注入一次。

> **出字间隔可调（2026-10-20 起）**：设置页「通用 → 出字间隔」四选一 —— **10 / 20 / 40 / 60 秒**（默认 20）。
> 越小越跟手（短句多时差别很明显），越大越省（大模型/接口调用次数更少）。
> 会话一结束**立刻**把剩余文字吐完，不用等间隔到点。

实测（1200 秒连续语音，出字间隔 60 秒那次的记录）：

```
音频总长 1200.0s | 分段目标 25s | 注入间隔 60s
  [段1] 23.0s 音频 → 472ms 识别 → 40 字：...
  ...
>>> 第 1 次注入（音频 60s 处）：124 字
>>> 第 2 次注入（音频 120s 处）：198 字
  ...
>>> 第 20 次注入（音频 1200s 处）：138 字
===== 共切 50 段 → 21 次注入；总耗时 23.9s，RTF 0.020 =====
```

- 20 分钟语音只需 **24 秒**处理完（边录边转，实际体感为"说完就有"）
- 峰值内存不随时长增长（只保留当前段）

**为什么分段是 25 秒**：SenseVoice 在长音频上会明显劣化——实测 60s/120s 整段输入
输出是乱码合并，而 25s 内准确率稳定（Python 版也用 25s 上限）。

### 长语音建议开 `toggle` 模式

按住 F9 说 20 分钟不现实。设置界面「录音方式」选 **切换**（或改 `config.toml` 的
`hotkey_mode = "toggle"`）：**按一下开始，再按一下停止**。

> **改完不用重启、也不用点保存**（2026-10-20 起）：
> - 设置页下拉**一改就生效**（只把 `hotkey_mode` 这一个字段落盘 + 热更新，不动你其它没保存的编辑）；
> - 手改 `config.toml` 也会在 **0.5 秒内**被 worker 自己发现并热生效（日志：`检测到 config.toml 已被改写`）；
> - 生效时机是**下一次按下**（正在进行的会话仍按老方式收尾），日志会写
>   `录音方式已切换为「切换（按一下开始 / 再按一下停止）」…—— 下一次按下生效`，
>   每次按下也都会打印当前 `录音方式`。
> - `hotkey_mode` 认 `toggle` / `Toggle` / `switch` / `切换` / `切换模式`（`hold` / `按住` 为按住说话），
>   写错只记一条 WARN 并按按住说话处理。

---

## 链路状态（实时监控 + 一键修复）

设置页顶部常驻一张卡片，按**用户能感知的顺序**把语音录入拆成 8 环，每环一个灯：

```
热键钩子 → 引擎（总开关/托管态）→ 识别模型 → 麦克风 → 门控引擎 → 整理后端 → 注入 → 最近一次会话
```

| 灯 | 绿 | 黄（能用但降级） | 红（坏了） |
|---|---|---|---|
| 热键钩子 | 已挂 + 累计回调/近 60s 回调 | 发生过 KEYUP 自愈 | 钩子未挂（按 F9 没反应） |
| 引擎 | ready（含是否托管） | 挂起（输入法不在前台）/启动中/总开关 OFF | 未运行 |
| 识别模型 | 就绪（含上次识别耗时） | 加载中 | 加载失败（后台每 30s 重试） |
| 麦克风 | 常开 + RMS 有读数 | 空闲已关麦 / 只在按下时开麦 / **数字静音** | 开着但收不到音频 |
| 门控引擎 | Silero VAD | 回退能量门（VAD 不可用）/兼容模式 | — |
| 整理 | 档位 + llama-server 状态 | 未启动（首次整理会冷启 10~25s） | — |
| 注入 | 上次成功（字数 + 目标窗口） | 还没有记录 | 上次失败（附 UIPI 提醒） |
| 最近会话 | 出字 N 段 M 字 | 未出字（带原因） | — |

- **判级只有一处**（`src/chain.rs::Chain::rows()`）：界面配色、托盘 tooltip、`--status` 输出
  同源同判级，不会各判一套（修了一处另一处还错）。
- 刷新：worker 每秒算一次快照，**内容变化才推**（加 5s 心跳兜底）；
  **会话进行中也照推**（长口述期间不会停在旧数据）。
- **探针绝不许卡录音线程**（2026-10-20）：整理后端那一环用 `alive_fast()`（连接 120ms / 总 200ms 上限，
  3s 一次 + 缓存）。原因是本机实测「连一个没人监听的本地端口」要 ~2s 才失败，而旧实现每秒两次走
  2s 超时的正式客户端 → 整理后端没起时 `钩子→worker 滞后` 达 **1920ms**、短按的 1.5s 窗口被吃掉。
  状态晚 3 秒没人看得出来，录音被卡住用户立刻就感觉得到。
- **一键修复**：重装钩子 + 重开麦克风 + 重新加载识别模型 + 拉起整理后端
  （只修“不必重启进程就能修”的环；录音/识别中不可用）。
- 鼠标悬停**托盘图标**也有一行摘要：`倾听输入法 · 正常 ●●●●●●◐◐ ｜ 上次出字 11 字 ｜ 就绪 ｜ 麦常开`。
- 命令行查同一份：`listen.exe --status`（人读）/ `listen.exe --status --json`（脚本，附 `healthy` 与每环 `levels`）。

### 停掉之后按 F9，能不能拉回来（2026-10-09 实测）

| 停掉的状态 | 谁停的 | 按 F9 | 代价 |
|---|---|---|---|
| **麦克风已空闲关麦** | 预热档位 3/5/10/30 分钟 | ✅ 自动重开麦 + 正常录音 | 这一次**起头丢 ~0.35s**（`起头补 0ms`） |
| **识别模型已释放** | 模型释放档位 | ✅ 自动重载 | +0.9~2.4s（在等待预算内完成 → **起头不丢**） |
| **整理后端 llama-server 已停** | 同上（一起停） | ✅ 后台自动拉起 | 本次注入**原文**（不阻塞），下一次整理正常 |
| **挂起（输入法不在前台）** | fork 发 SUSPEND | ✅ 直接恢复并录音 | 无（钩子与模型本来就没卸） |
| **总开关 OFF** | 设置页/托盘勾选 | ⛔ **不响应（设计如此）** | 钩子已摘，要在设置页重新打开 |
| **某一环坏了**（设备被拔/被静音…） | — | — | 点链路卡片的**一键修复**（重装钩子 + 重开麦 + 重载模型） |

---

## 设置界面

双击 exe 直接打开，可调**全部**设置。**改完即生效**（2026-10-21 取消「保存并应用」按钮）：改动 0.6 秒后自动写回 `config.toml` 并热更新引擎，关窗前也会先存一次：

| 分区 | 内容 |
|------|------|
| **链路状态（顶部常驻）** | 8 环实时灯（钩子/引擎/识别模型/麦克风/门控/整理/注入/最近会话）+ 总判 + **一键修复**；见下節 |
| **通用** | 主热键、录音方式（**改完即时生效，不用点保存**）、会话上限、分段长度、**出字间隔（4 选 1：10 / 20 / 40 / 60 秒）**、**麦克风预热档位（6 选 1）**、录音设备（下拉枚举）、开机自启、日志级别 |
| **识别** | 本地/云端切换、语言、模型目录、线程数；云端 ASR 的地址/Key/模型/超时 |
| **文本整理** | 档位 5 选 1（关闭 / 本地·轻度 / 本地·中度 / 云端·轻度 / 云端·中度）；本地大模型设置区（exe、模型、端口、ngl、ctx、超时、关思考、退出回收、首次等待）+ 状态行 + 立即启动/停止 |
| **注入** | 恢复剪贴板、写入延迟、恢复延迟、Shift+热键换原文、段间分隔符 |
| **输入法** | 倾听输入法/用户目录/资源状态、主力语言（中/英/日）、英文方案、中文简繁、语法模型、一键部署 + 打开目录（首启向导复用此页） |
| **热词（打字 + 录音）** | 热词包开关 + 词条表格（增删改/过滤）+ 保存 + 立即重新部署；**我的词表**：快捷输入 / 我打过的词（学习开关、上限、自动写入阈值、导入/清空/重学）；见下节 |
| **本地模型** | 状态、一键下载（带进度条）、打开目录 |
| **运行日志** | 实时日志 + 打开日志目录 |

- 关闭窗口 = **隐藏到托盘**（程序继续跑，热键照常；关窗前会把未落盘的改动先存掉）；要退出用窗口里的「退出程序」或托盘菜单
- 底部不再是「保存并应用」，而是一行状态：`改动即时生效（自动保存）` / `● 正在自动保存…` / `已自动保存 HH:MM:SS`。
  **热键写错时不会把坏值写进配置**（保留上一个有效热键），状态栏会说明
- 界面中文用系统字体（等线/微软雅黑），不额外占体积
- 托盘菜单保留快捷项：打开设置 / 识别后端 / 文本整理 / 下载模型 / 开机自启 / 打开 config.toml / 打开日志目录 / 退出

---

## 命令行

| 参数 | 说明 |
|------|------|
| `--config <PATH>` | 指定配置文件（默认 exe 同级 `config.toml`） |
| `--once <WAV>` | 转写单个 wav 后退出 |
| `--simulate <WAV>` | **模拟长语音分段识别**（不注入），验证分段与增量输出；整理档位非关闭时同时跑整理链路 |
| `--selftest <DIR>` | 跑目录内全部 `*.wav`，输出相似度/耗时表 |
| `--bench <DIR>` | **中英混说横评**：目录内 `*.wav` + `ref.tsv`，依次跑 `[[bench.models]]` 里的多个本地模型，出「相似度/术语命中率/RTF」表 |
| `--no-tray` | 不创建托盘图标（调试用） |
| `--devices` | 列出录音设备 |
| `--inject <TEXT>` | 3 秒后注入一段文本（手动 E2E：先聚焦目标窗口） |
| `--download-model` | 下载本地模型后退出 |
| `--autostart true\|false` | 设置开机自启后退出 |
| `--polish <TEXT>` | 单条文本走完整整理链路（不用麦克风），打印启动/整理耗时与是否复用 |
| `--first-run` | 打开设置界面并停在「输入法」页（首启向导） |
| `--settings` | 打开设置窗口；已有实例在运行时唤起它的设置窗（fork 托盘/设置窗入口用） |
| `--rime-status` | 检测倾听输入法/用户目录/资源后退出（只读，不改动） |
| `--rime-deploy` | 部署输入法（铺方案包 → 写配置 → 编译词库）；`--rime-install` 允许自动装倾听输入法 |
| `--rime-uninstall` | 保守卸载我们铺的 Rime 文件（保留用户词库/配置） |
| `--correct <TEXT>` | 只跑一遍术语纠错（不录音、不联网）：`listen.exe --correct "美国客户"` 必须原样返回；`多克` → `Docker` |
| `--status` | **打印全链路状态**（listen 在跑时走 ListenCtl `CHAIN`，拿到的是进程内实时快照；没跑则静态探测）；`--json` 输出给脚本 |

---

## 配置（`config.toml`，绿色：exe 同级）

```toml
[general]
hotkey = "f9"              # 语法 [修饰+]*键：win / ctrl / alt / shift 可组合（如 win+h / ctrl+alt+f8）
raw_hotkey = "shift+f9"    # 仅展示：取原文固定为「主键修饰 + Shift」
hotkey_mode = "hold"       # hold | toggle（也认 Toggle/switch/切换）。
                           # 改完无需重启：设置页下拉立即生效；手改本文件 0.5s 内自动热生效
max_session_minutes = 30
gate_engine = "vad"        # 门控引擎：vad = Silero VAD（默认，只认人声）/ energy = 旧能量门（只看音量，兼容）
vad_threshold = 0.5        # VAD 灵敏度阈值（0~1）：越大越严（噪声少、轻声可能被漏）
vad_min_speech_seconds = 0.25  # VAD 最短语音（秒）：短于此不当语音（滤咳嗽/敲键“咔哒”）
segment_seconds = 25       # vad = 一段的最长上限；energy = ASR 分段长度。勿超 30（SenseVoice 会劣化）
silence_seconds = 0.5      # vad = 尾部静音（说完多久上屏）；energy = 静音断句。调小更跟手但易切碎
flush_seconds = 20         # 出字间隔（秒）：设置页 4 选 1 = 10 / 20 / 40 / 60（默认 20）；会话结束会立刻吐完剩余
min_speech_rms = 0.0015   # [能量门参数，gate_engine=energy 时才生效] 绝对下限；实际门控取 max(本值, 噪声底×2.5)
min_record_seconds = 0.3   # [能量门参数] 短于此时长的段直接丢弃
prewarm_capture = true     # 预热录音：麦克风常开 + 按下瞬间 pre-roll 补回起头丢字（设置页可关）
prewarm_idle_minutes = 10  # 空闲这么久自动关麦（0 = 不关）
managed_suspend_exit_minutes = 0  # 托管挂起后自动退出（分钟）；0 = 不自动退，见《行为细节》
autostart = false
log_level = "info"
device = ""                # 空=系统默认；可填设备名片段

[asr]
backend = "local"          # local | cloud
language = ""
local_model = "models/sense-voice"
num_threads = 4
[asr.cloud]                # OpenAI 兼容 /v1/audio/transcriptions
base_url = ""
api_key = ""
model = "whisper-1"
timeout_s = 15

[polish]                   # 整理后端：local | cloud
enabled = false
provider = "local"         # local = 本地 llama-server；cloud = OpenAI 兼容
mode = "light"             # light | polish
base_url = ""              # 仅 cloud 用
api_key = ""
model = ""
timeout_s = 5              # 本地自动取 max(此值, 20s)

[polish.local]             # 本地大模型（懒加载 + 复用 + 可选回收）
exe = "K:\\projects\\llama.cpp\\build\\bin\\Release\\llama-server.exe"
model = "K:\\AI_Voice\\models\\llm\\Qwen3.5-4B-Q4_K_M.gguf"
host = "127.0.0.1"
port = 18080
ngl = 99                   # GPU 层数
ctx = 4096
temperature = 0.2
top_p = 0.9
max_tokens = 512
disable_thinking = true    # 关思考（思考型模型必开）
start_timeout_s = 150
wait_on_first_use = false  # 首次触发是否同步等待（false = 本次用原文）
stop_on_exit = true        # 退出时回收自己拉起的实例

[floating]                 # 悬浮窗（状态/电平/计时；默认不显示识别文字）
enabled = true
hide_delay_seconds = 3     # 会话结束后停留几秒（设置页 5 选 1：1/3/5/10/30）
show_text = false          # 是否在悬浮窗显示识别/整理文字（默认关：文字已经注入目标窗口了）
history = 3
max_chars = 60
font_size = 16

[inject]
restore_clipboard = true
paste_delay_ms = 250
restore_delay_ms = 600
raw_replace_undo = true
chunk_separator = " "      # 分段注入的分隔符（聊天框建议空格）

[hotwords]                 # 热词包：一份词表同时驱动打字（Rime 置顶候选）与录音（ASR 热词偏置）
enabled = true
packs = ["vibecoding", "keyboard"]  # <exe 同级>\hotwords\<名>.tsv；内置 vibecoding / keyboard
dir = ""                   # 空 = exe 同级 hotwords\
asr_top_n = 30             # 送 FunASR-Nano 热词 prompt 的条数上限（其 LLM 上下文有限）
phrases = true             # 快捷输入表（hotwords\phrases.tsv）；只用于打字
learn = true               # 用户词学习（hotwords\learned.tsv，我打过的词自动置顶）；只用于打字
learn_max = 300            # 我打过的词最多置顶多少条（超出按使用次数淘汰）
prefix_completion = true   # 字词联想：打前缀也出热词候选（改完需「立即部署」一次）
# 另外 `hotwords\mine.tsv` = 「我的热词」：自己加，条数**不设上限**，打字+录音两条链都生效
learn_auto_deploy = 20     # 攒够这么多新词就自动写进打字表（0 = 只能手动）

# [glossary]               # 可选：手写补充术语（优先级高于热词包，GUI 不展示）
# terms = ["nRF54L15"]
# aliases = ["欧拉马=Ollama"]   # ASR 常把英文音译成中文；精确替换 + （仅 ≥3 字长音译）拼音模糊匹配
# corrections = ["grape=grep"] # 英文同音错词（整词、忽略大小写）
```

---

## 本地模型

本机 ASR 支持三种本地模型（设置页「识别 → 本地模型」可自由切换）：

| 类型 | 说明 | 体积 | 速度(CPU) |
|------|------|------|-----------|
| `sensevoice` | SenseVoice-small，中英日韩粤，快但**中英混说会把英文转成中文** | 230MB | 极快（RTF≈0.03） |
| `paraformer` | Paraformer 中英双语 | ~230MB | 快 |
| `funasr-nano` | **FunASR-Nano（2025-12，Qwen3-0.6B LLM 解码）**，多语种 + 支持 **prompt 热词**（把术语"拽回"英文原词） | ~1.7GB(fp16) | 中（RTF≈0.35） |

> FunASR-Nano 是本机"中英混说 + 术语保真"的首选：实测把**热词包**词表作为热词注入 LLM prompt，
> 可把内嵌英文术语从"音译"纠正回英文原词（代理测试术语命中 0/2 → 2/2）。
> 注意用 **fp16**（int8 导出实测明显劣化）；其 LLM KV 上限 512，热词过多会被截断（按权重取前 `[hotwords] asr_top_n` 条，默认 30）。

### 中英混说横评

```powershell
# 目录内放 20 条 *.wav + ref.tsv（文件名<TAB>参考文本<TAB>术语;术语）
.\target\release\listen.exe --bench K:\AI_Voice_rs\tests\codeswitch
```

用例与说明见 `tests\codeswitch\`；模型清单改 `config.toml` 的 `[[bench.models]]`。

### SenseVoice（默认）

- 目录：`models/sense-voice/`，需要 `model.int8.onnx`（约 230MB）+ `tokens.txt`
- 获取：设置界面「⬇ 下载本地模型」（hf-mirror → huggingface 自动切换，带进度）
  / `--download-model` / 手动拷贝（U 盘）
- 来源：`csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17`
- 模型不随安装包分发

---

## 本地大模型整理（llama-server）

不联网也能把口语整理成书面语：程序自己发现并拉起本机已装好的 **llama-server + GGUF 模型**。

设置界面「文本整理」档位 5 选 1：

| 档位 | 行为 |
|------|------|
| 关闭 | 直通（注入 ASR 原文） |
| 本地 · 轻度清洗 | 本地大模型，只删口头语/改错别字/补标点 |
| 本地 · 中度润色 | 本地大模型，整理成通顺书面表达 |
| 云端 · 轻度清洗 / 云端 · 中度润色 | OpenAI 兼容 `/v1/chat/completions` |

### 生命周期（懒加载 + 复用 + 可选回收）

- **懒加载**：不在启动时拉起；**第一次真正要整理时**才在后台启动（冷启动约 10~25s）
- **首次降级**：冷启动那次**不等待**，本次直接注入 ASR 原文并提示「本地大模型启动中」，
  就绪后后续自动使用（想改成同步等待：`wait_on_first_use = true`）
- **复用**：端口上已有 llama-server（比如 Python 版拉起的）→ 直接复用，不重复启动
- **回收**：只回收**本程序自己拉起**的进程（`stop_on_exit = true` 时）；
  别人启动的一律不动
- **端口被占用且不是 llama-server** → 明确报错并提示改 `polish.local.port`，不误杀

### 关思考（必做）

Qwen3.5 等思考型模型若不关思考，`content` 会是**空字符串**、`completion_tokens` 撞满 ctx，慢 100 倍。
程序固定发 `chat_template_kwargs:{"enable_thinking":false}`；老版 llama.cpp 不认时自动退回 system 提示词尾追加 `/no_think`，并做 `<think>…</think>` 剥离兜底。对照实验数据见《项目调试过程.md》§3.1。

### 自检（不用麦克风）

```powershell
.\target\release\listen.exe --polish "嗯 那个 我我我觉得这个 nRF52832 的 SAADC 采样 就是 有点问题"
# llama-server: …exe
# 启动: 本程序拉起（23715ms）  状态: 就绪（本程序拉起）
# 整理耗时: 283ms
# 结果: 我觉得这个 nRF52832 的 SAADC 采样有点问题。
```

实测（Qwen3.5-4B-Q4_K_M，RTX 5070 Ti；旧记录写作 RTX 3080 已更正）：

| 项 | 结果 |
|----|------|
| 冷启动 | 23.7s（首次）/ 2.0s（模型已在页缓存） |
| 单次整理 | 283~938ms（100~200 字输入） |
| 1200s 长语音 + 本地整理 | 50 段 → 21 次注入全部整理成功，总耗时 37.5s，RTF 0.031 |
| 复用已有实例 | 不重复启动；退出时不杀它 |
| 退出回收 | 自己拉起的被回收；未勾选则继续存活 |

排错看 `logs/llama_server.log`（llama-server 自己的输出）与 `logs/app.log`。

---

## 输入法（Rime 二合一）

同一个安装包 = **语音输入**（本程序）+ **键盘输入**（倾听输入法 Rime + 三套离线方案包）。
首启向导选一种主力语言 → 自动铺方案包并部署，之后按 `F4` 可切方案、`Ctrl+Shift+4` 切简繁。

| 主力语言 | 方案 | 说明 |
|---------|------|------|
| 中文 | 雾凇拼音 `rime_ice`（+ `melt_eng` 英文混输） | 默认方案列表 `[rime_ice, melt_eng]` |
| 英文 | `easy_en` | 14MB 词库 |
| 日文 | `jaroomaji`（罗马字） | 85MB 词库，首次编译较久 |
| 法文 | —（二期预留） | 方案包未内置 |

### 交付模型（为什么这么做）

Rime 在 Windows 只能是倾听输入法（TSF 输入法），本体跑在 `WeaselServer.exe`，**无法并入本进程**。
所以我们只做三层：**交付层**（铺方案包/写配置/调部署）+ **界面层**（设置页兼首启向导）+ **配置层**（只 patch 我们管理的键）。

文件落位（`%APPDATA%\Rime`，librime 只在**用户目录根**找 `<方案>.schema.yaml`，不搜子目录，所以是平铺）：

```
Rime\
  rime_ice.schema.yaml  ...   ← 方案包平铺（96 个功能文件，来源可查清单）
  cn_dicts\ lua\ opencc\ ...  ← 方案包自带的子目录（相对引用）
  wanxiang-lts-zh-hans.gram   ← 万象语法模型（400MB，仅中文用）
  default.custom.yaml         ← 我们写的方案列表（带标记块，卸载可摘）
  rime_ice.custom.yaml        ← 语法模型 + 简繁默认态 + 热词表指向（带标记块）
  listen_hotwords.txt         ← 打字热词表（从 hotwords\*.tsv 生成，custom_phrase 置顶候选）
  .listen-files.txt           ← 我们铺的文件清单（卸载按它删，绝不碰用户文件）
  <用户自己的> *.userdb\ build\ *.custom.yaml  ← 一律不动
```

### 部署为什么必须提权

- 倾听输入法安装器是 `RequestExecutionLevel admin`，装完 `WeaselServer` 也是提权进程；**非提权进程给提权进程发 IPC 会被 UIPI 拦掉** → 部署器永久卡住（实测复现 3 次）；
- 反过来提权给非提权发消息是允许的 → **一律提权部署**。
- `WeaselDeployer.exe /deploy` 参数必须**精确整串**（weasel 用 `wcscmp` 比较），多带参数会掉进 GUI 分支并占住部署互斥体。

> 完整根因、复现过程与另外三个关键坑（平铺布局 / 退出码判定 / 卡死误判）见《项目调试过程.md》§4.1、§5.5。

### 实测

| 项 | 结果 |
|----|------|
| 中文部署（首次，含 400MB 语法模型） | 16.0s，产物 `rime_ice.table.bin` 60.6MB |
| 切日文（首次编译 85MB 词库） | 6.8~10.6s，产物 `jaroomaji.table.bin` 70.4MB |
| 简繁/方案列表变更 | 重新部署后 `build\*.schema.yaml` 更新，开关默认态生效 |
| 保守卸载 | 96 个文件 + 空目录 + 语法模型 + 管理块全清；`build\`、`user.yaml`、`*.userdb\` 保留 |
| 文件层自动校验 | `tools\verify_rime.ps1` 14/14 通过（另有 1 条已知告警：`radical_pinyin` 部件反查） |

```powershell
# 环境体检（只读）
.\target\release\listen.exe --rime-status
# 部署（会弹一次 UAC；首次编译词库可能要几分钟）
.\target\release\listen.exe --rime-deploy
# 文件层校验 / 卸载残留检查
powershell -ExecutionPolicy Bypass -File tools\verify_rime.ps1 -Lang zh
powershell -ExecutionPolicy Bypass -File tools\verify_rime.ps1 -AfterUninstall
# 取资源（国内镜像优先，约 622MB）
powershell -ExecutionPolicy Bypass -File installer\fetch_resources.ps1
# 二合一安装包（含语法模型，约 417MB / 417,513,301 B；-NoLM 可省掉 400MB 语法模型）
powershell -ExecutionPolicy Bypass -File installer\build.ps1 -Setup
```

---

## 热词包（打字 + 录音双关联）

**一份词表同时驱动两条链**：打字时置顶候选（Rime），说话时偏置识别 + 保留术语（ASR / 整理）。

```
<exe 同级>\hotwords\
  keyboard.tsv     热词包：键圈（机械键盘社区）194 条 —— 团购/缩写/配列/轴体/键帽/结构/黑话
  vibecoding.tsv   热词包：AI 编程 + 嵌入式 118 条 —— 原 default_glossary_terms() 迁过来
  phrases.tsv      快捷输入：自己定编码 → 任意文本（如 `gds` → 快递地址），**只用于打字**
  mine.tsv         我的热词：自己加，**条数不限**，打字+录音两条链都生效（默认启用）
  learned.tsv      我打过的词：自动学习（用过一次的词就置顶），**只用于打字**
  learn_log.tsv    Rime 里的 lua 钩子写的原始记录（程序自动增量导入成 learned.tsv）
```

| 链路 | 怎么生效 |
|------|---------|
| **录音** | 词条按权重降序合并 → FunASR-Nano 热词 prompt（前 `asr_top_n` 条，默认 30）/ 整理 prompt 术语保留（前 60 条）/ `[glossary] aliases` 拼音模糊纠错。**保存即生效**（重建识别/整理后端）。⚠️ 模型相关：SenseVoice 无热词能力（只能靠术语后处理），Paraformer 热词也要流式版才生效（本机离线版不支持，代码里已注明）——**包越大收益越明显的是 FunASR-Nano**（调试计划任务 6 的主引擎候选） |
| **打字** | 带编码的词条写成 Rime 自定义短语表 `%APPDATA%\Rime\listen_hotwords.txt`（`custom_phrase` 翻译器，权重最高→置顶），`rime_ice.custom.yaml` 里 patch `custom_phrase/user_dict` 指过去。**内容改动不用重新部署**：librime 每个输入会话开始时重读这张纯文本表（日志 `tsv.cc: reading tsv file`），保存后**换个窗口/输入框就生效**（实测 2026-10-09）；只有 patch / lua 钩子 / 方案列表这类**结构变更**才需重新部署（一次 UAC） |

### 用户词学习（打过的词自动第一位）

**为什么自己搞**：librime 原生的用户词典调频权重远小于词典自带权重 —— 实测 `shishi` 选第 2 个候选
「实施」后，再打两次第一位仍然是「实时」（写进 `rime_ice.userdb` 了，但排不上来）。只有用我们那张
`initial_quality: 99` 的表才能做到「用过的词必排第一」。

```
Rime 里的 lua 钩子（部署时铺 lua\listen_learn.lua，patch 一个旁路 lua_processor）
  └─ 每次上屏：记「你敲的编码 + 上屏文本」→ 追加到 hotwords\learn_log.tsv
       ↓ 程序每 30s 增量导入（按字节 offset 消费，不重复吃老数据）
     hotwords\learned.tsv（GUI「我打过的词」可看可删）
       ↓ 攒够 learn_auto_deploy 个新词（默认 20）就自动重写打字表
     %APPDATA%\Rime\listen_hotwords.txt → 换个窗口即生效（无需 UAC）
```

- 记录条件（lua 侧过滤）：编码纯小写字母 2~12 位；上屏文本含中文、2~4 个字、不含空白；
  且**编码长度与字数相称**（挡住“长句里选中间那个词”产生的错编码）
- 实测：`shishi`+空格 → **实施**（原第一位「实时」）；6 字长句不会被记
- 删除语义：导入是**单向消费日志**，所以从 `learned.tsv` 里删掉的词不会下次又被加回来；
  想全部重来用「↺ 重新学习全部」，想清空用「🗑 清空已学词」
- 隐私：日志与词表都在本地（`hotwords\`），不联网；`learned/phrases` **不进录音热词**（地址、私人词条不会送模型）

### 快捷输入（`gds` → 快递地址）

> **在哪配**：设置页 → **热词（打字 + 录音）** → 词表下拉选 **`phrases`（快捷输入）** →
> 「＋ 加词条」→ 填 `词条`＝完整地址、`编码`＝`gds`（全小写字母）、`权重`＝90 → 点 **「💾 保存到文件」**。
> 保存即重写打字表（2026-10-21 修：旧实现要等「学到 20 个新词」才刷新，表现为"加了词当场打不出来"），
> 切一下窗口后打 `gds` + 空格，第一个候选就是地址。也可以直接编辑 `hotwords\phrases.tsv`（TAB 分隔）后重启。

GUI「热词 → 我的词表 → 快捷输入」里加一行即可：`词条 = 要上屏的文本`、`编码 = 小写字母`。
实测 `gds` + 空格 → 完整地址上屏。编码只能用小写字母（雾凇全拼的 `speller/alphabet` 只有 a-z），
文本单行、不能含制表符。

### 词表格式（`hotwords\*.tsv`，可直接编辑；GUI「热词」页读写的就是它）

```
GB	gb	行业机制	87	团购／开团
客制化	kezhihua	中文黑话	62	客制化(custom)
G80		复古键盘	69	Cherry G80 系列
```

`词条 ⇥ 编码 ⇥ 类别 ⇥ 权重 ⇥ 说明`（`#` 开头是注释）

- **编码只能用小写字母**：雾凇全拼的 `speller/alphabet` 只有 a-z —— 数字/符号打不出候选，
  所以 `R1` / `G80` / `WS2812` / `nRF52832` 这类编码自动降级为「仅录音」（这些词在英文模式直接敲即可）
- **编码留空 = 只进录音侧**（ASR 热词偏置），不生成打字候选
- **权重 0~100**：越大越先被送进识别模型的热词 prompt（FunASR-Nano 的 LLM KV 有上限，不可能全塞）
- 与常用拼音冲突的编码（如 `sa` → 撒，权重 140910）自动改成「仅录音」，不抢正常打字的候选
- 内置包首次运行铺到 `hotwords\`，**已存在不覆盖**（改过就归你）；卸载时只按清单删我们铺的文件

### 生成器（改资料包后重跑）

```powershell
python tools\gen_hotwords.py --dry-run   # 只报告：词条数/冲突降级/进 prompt 的前 30 条
python tools\gen_hotwords.py             # 写 resources\hotwords\keyboard.tsv
```

数据源：`docs\键圈术语资料包`（`键圈术语库.json` 223 条结构化术语 + `键圈热词榜.md` 的
GeekHack 11,875 篇帖子词频 → 权重）；冲突判定拿雾凇自带词库（`cn_dicts\*.dict.yaml`，
19 万高频编码）当命令库。

---

## 行为细节

- **热键不吞键**：低级钩子 `WH_KEYBOARD_LL` 回调一律 `CallNextHookEx` 原样放行，
  只旁路记录按下/松开。F9 在 Excel（重算）/ Word（更新域）里照常工作。
- **热键语法**：`[修饰+]*键`（修饰 win / ctrl / alt / shift，可组合），如 `win+h`、`ctrl+alt+f8`；
  **精确匹配**（写了的按住、没写的没按）；字母键必须带修饰键；取原文 = 主键修饰 + Shift。
  注意：`win+h` 与系统「听写/语音输入」快捷键冲突，用之前先关掉系统听写（否则两个都会触发）。
  热键配置无效时启动会回退 f9（不会起不来）；设置页保存前会校验。
- **已知 Windows 限制**：焦点在**本程序自己的设置窗口**上时，低级键盘钩子不会被系统调用
  （输入目标是钩子进程自己的窗口时，系统跳过该钩子）——即设置窗在前台且聚焦时热键不响应；
  设置窗隐藏到托盘（常态）或焦点在其他应用时一切正常。非本程序 bug，无法用钩子规避。
- **录音**：按设备**默认配置**打开（不强推 16k，避免 WASAPI `Invalid sample rate`），
  多声道降单声道 → **流式**重采样到 16k（整数倍走盒式平均自带低通，其余线性插值）。
- **预热录音（默认开，`prewarm_capture`）**：引擎就绪就把采集流常开，进程内维护 3s 环形缓冲；
  按下瞬间取「按下时刻 − 350ms」（`PREROLL_MS`，pre-roll）喂给分段器 → **起头不丢字**。
  **档位（设置页 6 选 1）**：`只在按下时开麦`（最省）｜**`永不关麦`（默认）**｜`空闲 3 / 5 / 10 / 30 分钟关麦`。
  为什么默认永不关：关麦后**下一次按下必丢起头**（实测 `起头补 0ms`），用户体感就是“偶尔第一句少字”。
  **常开的真实代价**（同机 60s 进程 CPU，prewarm 开 vs 关）：**1.422s vs 1.625s**（2.4% vs 2.7% 单核）
  → 差异在噪声内；真正的代价只有麦克风指示灯常亮 + 隐私观感。
- **麦克风实时读数**（链路卡片）：`RMS 0.00066 ●有声`（不消耗环形缓冲，不影响会话切头）；
  若“有音频块但电平恒为 0.0”，卡片会黄灯提示——那是**数字静音**（系统/硬件把麦静音了），
  不是“房间安静”（实测本机安静底噪 RMS ≈ 0.0007）。
- **松手尾音**：松手后再收 300ms 再收尾（用户常常边说边松手，最后半句跟着按键一起丢）。
- **噪声底自适应（`gate_engine = "energy"` 时）**：分段器按会话估计噪声底（只跟随更安静的观测），
  阈值 = `max(min_speech_rms, 噪声底×2.5)`；固定阈值在本机（噪声底 ≥ 0.0015）会让静音断句**从未触发**
  （首段恒等 3s）。
- **门控引擎（`gate_engine`，默认 `vad`）**：默认用 Silero VAD 当主判据（端点驱动切分），
  能量门分不清「人声」和「键盘瞬变」——实测键盘噪声 4/4 全被送进 ASR（只是恰好吐空串）；
  VAD 模式下 `silence_seconds` = 尾部静音（说完就上屏）、`segment_seconds` = **最长段上限**，
  另有 `vad_threshold`（灵敏度）/`vad_min_speech_seconds`（最短语音）两个旋钮；
  能量门参数（`min_speech_rms` / `min_record_seconds`）仅在 `gate_engine = "energy"` 时生效。
  模型缺失/加载失败 → 自动回退能量门并记 WARN（不会“按了没反应”）。设置页按引擎只显示生效的旋钮。
- **门控实时读数**：说话时悬浮窗右侧显示 `语音 ●  电平 0.012`（是否判为语音 + 本块 RMS，
  节流 ≤4 次/秒、语音状态一变立刻更新）——“我说了但没出字”能一眼看出是门控没收还是识别没出。
- **悬浮窗停留时间可调**（2026-10-20）：设置页「悬浮窗 → 停留时间」五选一 —— **1 / 3 / 5 / 10 / 30 秒**（默认 **1**，一闪就走）。
  会话结束后悬浮窗再停留这么久就自动消失（老配置里是 30 秒，用户体感「太久不消失挡视线」）。
- **悬浮窗默认不显示文字**（2026-10-20）：设置页「悬浮窗 → 显示文字」默认**关**。
  文字已经注入到目标窗口了，悬浮窗再留着上一批就是干扰；关掉后只显示状态点 / 门控电平 / 计时，
  并在一批文字上屏时显示一句「已注入 N 字」然后按停留时间消失。要看识别原文再打开（调试用）。
- **提示不会无缘无故弹窗**（2026-10-20）：只有「和语音直接相关」的提示（未出字原因、续录中、录音设备异常…）
  才用悬浮窗；`设置未保存`、`上次运行异常退出` 这类走设置窗状态栏 + 运行日志，不弹悬浮窗。
- **悬浮窗右键复制**：右键任意一行 → 菜单「复制本行」（复制该行**完整**文本，不受显示截断 `max_chars` 影响）；
  鼠标拖选一段照旧 `Ctrl+C`（egui 原生，行为不变）。菜单不提供“复制选中”：egui 0.35 的 `Label`
  选中文本不对外暴露，且点菜单项会清空选中（细节见《项目调试过程.md》§22）。
- **悬浮窗位置会被钳到可见区**（2026-10-09）：记忆位置若被任务栏切掉（实测 `pos_y=989`、工作区只到 1040）
  会在创建时自动拉回（日志：`悬浮窗位置已从记忆值钳回可见区`）；拖动落点被切掉也会拉回并落盘。
  半透明（`WS_EX_LAYERED + LWA_ALPHA=165` + 毛玻璃）**失败会明确写日志**（以前静默失败 → “悬浮窗怎么不透明了”无从下手）。
- **纯噪声会话直接丢弃**：用户按了键但没说话时，SenseVoice 会幻觉出整句（实测 3.7s 环境音 → 一句假话）；
  会话内“从没有过语音窗”（动态范围+电平双条件）就不送 ASR。
- **没出字一定有反馈**：录音太短/过轻/后端未就绪都会在悬浮窗 + 日志里写明原因（不再静默丢）。
- **失败降级**：云端 ASR 失败 → 自动回退本地（若模型就绪）；整理失败 → 用原文；
  整理输出撞 max_tokens/ctx 被截断（`finish_reason=length`）→ 回退原文（不再静默吐半句）。
- **注入**：存旧剪贴板 → 写新文本 → 250ms → `Ctrl+V` → 600ms → 恢复旧剪贴板。
- **管理员窗口无法注入**（Windows UIPI 限制，与 Python 版一致）。
- **单实例**：命名互斥体 `Global\ListenRust`（NULL DACL，提权/非提权互相可见）；
  **任何拿不到单实例锁的情况一律退出**——旧实现拿到 `ACCESS_DENIED` 还继续跑，
  会出现两个实例各装一个 F9 钩子、各自录音注入（同一句话识别两遍/注入两遍）。
- **挂起（托管）只停会话**：钩子与识别模型常驻，按 F9 即恢复并录音（不再“按了没反应”）；
  默认不自动退出（`managed_suspend_exit_minutes = 0`），因为进程一退就只能等输入法激活重新拉起。
- **录音中收到挂起请求不打断录音**（2026-10-20）：fork 只要“输入法不在前台”就会发 `SUSPEND`，
  以前它会把正在录的这一段**直接砍掉**（用户体感“没松手却自己截止”，实测差 2.1s）。
  现在只记下“待挂起”，会话照常录到松手/再按一下/上限，收尾后才真正挂起
  （日志：`收到挂起请求：先把这一段录完…`）。
- **钩子看门狗**：每 60s 心跳一次并静默重装钩子，抵御 Windows 静默摘钩。
- **松手看门狗**：KEYUP 万一丢了，会话看物理键状态兜底收尾，不会一直录到 30 分钟上限。
- **假松开复核**（2026-10-20）：按住模式下收到 `Release` 还会用 `GetAsyncKeyState` 复核物理键——
  键仍按着（键盘抖动 / 别的程序注入 KEYUP / 钩子错位配对）就**无视这次松开**继续录，
  日志记 `忽略 Release(seq=N)：物理键仍处于按下状态…`。真松手由看门狗 700ms 兜底，最差晚 0.7s。
- **会话结束原因可查**：`会话结束：音频 9.8s → 出字 2 段 18 字（原因：松手；按住 9470ms）`，
  原因可能是 `松手 / 切换模式再次按下 / 松手（看门狗兜底）/ 达到会话上限`；非松手收尾会再补一条
  WARN（带录音方式、物理键状态、已无视的假松开次数）——“怎么自己停了”以后不用猜。
- **后端自愈**：识别后端加载失败/模型缺失 → 30s 退避自动重试；按下时若还在加载，等待至多 2.5s 再录
  （预热环形缓冲还在，按下那几秒的音频不会丢）。
- **崩溃自愈**（2026-10-20）：GUI 与录音引擎同进程，主线程一崩整个语音输入就没了。现在
  ①`panic` 时**同步**写 `logs\last_panic.txt`（线程名/位置/backtrace），不再“日志突然断掉查无实据”；
  ②主线程/worker 线程崩溃 → **自动拉起新实例**（`--retry-lock 5000` 等旧进程释放单实例锁，约 1~2s 回来）；
  ③只自启一轮（子进程带 `LISTEN_RESPAWN=1`），防崩溃死循环；
  ④新实例启动时提示「上次运行异常退出（已自动重启）」并把现场改名留档。
  注意：**被任务管理器/安全软件强杀**（没走到 panic）仍然只能等 fork 在输入法激活时拉起。
- **设置改完即生效、自动保存**（2026-10-21，取消「保存并应用」）：所有控件改动 → 标记脏 →
  0.6 秒后自动写回 `config.toml` + 发 `ReloadConfig` 热更新（连续改多个只写一次）；
  关窗前再强制存一次。热键写错（如敲到一半的 `f`）**不会把坏值写进配置**，
  保留磁盘上那个有效热键并在状态栏说明（`config::normalize_before_save`，有单测）。
- **字词联想（打前缀也出热词）**（2026-10-21 用户要求开启）：给 Rime 的 `custom_phrase` 打开
  `enable_completion`，于是打 `g` 就能看到 `GB / Gasket / GitHub…`。
  开关在 设置页 → 热词 → 「字词联想」（`[hotwords] prefix_completion`，默认开）。
  ⚠️ 这是**方案结构改动**：改完要到「输入法」页点「立即部署」（一次 UAC）才生效；
  候选会变多，嫌吵就关掉再部署一次。
- **热词条数没有天花板**（2026-10-21）：打字侧 = 全部启用包 + 我打过的词 + 「我的热词」，
  全量进 Rime 候选表；录音侧受**后端**约束 —— FunASR-Nano 走 prompt 热词
  （`asr_top_n` 默认 30，可调到 1~200，但受模型 512 token 上下文限制，实测 30~60 较稳）；
  Paraformer 离线版**不支持**热词偏置（只有术语后处理）。
  设置页「热词」页顶部会显示全量条数（如 `vibecoding 118 + keyboard 194 + 我打过的词 300 = 612 条`）
  与「送识别模型前 N 条」。
- **无控制台窗口**：exe 是 GUI 子系统；从终端带参数调用时会自动挂到父控制台，CLI 输出照常。

---

## 回归测试

```powershell
# ASR 精度（4 条内置参考 + 剪贴板读写）
.\target\release\listen.exe --selftest "K:\AI_Voice\models\asr\sherpa-onnx-funasr-nano-fp16-2025-12-30\test_wavs"
# 长语音分段与增量注入（带整理档位时会走完整整理链路）
.\target\release\listen.exe --simulate .cache\mixed_1200s.wav
# 单条文本走整理链路（不用麦克风）
.\target\release\listen.exe --polish "嗯 那个 我我我觉得这个 nRF52832 的 SAADC 采样 就是 有点问题"
# 输入法（Rime）环境体检 + 文件层校验 + 卸载残留检查
.\target\release\listen.exe --rime-status
powershell -ExecutionPolicy Bypass -File tools\verify_rime.ps1 -Lang zh
powershell -ExecutionPolicy Bypass -File tools\verify_rime.ps1 -AfterUninstall
# 注入链路 E2E（记事本靶窗口）
powershell -ExecutionPolicy Bypass -File tools\e2e_inject.ps1
# 热键/挂起链路 E2E（合成 F9，不用说话）：单实例护栏 + 挂起仍有响应 + 预热起头切片 + 未出字反馈
python tools\e2e_hotkey_suspend.py
# 以下 4 个 E2E 会**合成按键/鼠标**（SendInput/mouse_event）：按本机规则 agent 不得运行，需你手动跑
# 录音方式语义 E2E（合成 F9，不用说话）：toggle 语义 / 手改 config.toml 自动生效 /
# 「假松开」不许收尾 / 录音中 SUSPEND 不许截断 / 自动重复不许收尾
python tools\e2e_hotkey_mode.py
#   $env:LISTEN_EXE="K:\app\AIVoice\listen.exe"; python tools\e2e_hotkey_mode.py   # 测装机版
# 悬浮窗 E2E（不用说话）：启动不自动弹 / 会话期间可见 / 按「停留时间」消失
python tools\e2e_float_window.py
# 崩溃自愈 E2E（用 LISTEN_PANIC_TEST 让 worker 线程故意 panic）：
# 现场落盘 → 自动重启 → 新实例提示用户 → 第二次崩溃不再自启
python tools\e2e_crash_recovery.py
# 打字侧 E2E（不用手）：WinForms 靶窗口 + SendKeys，回读上屏文本
powershell -ExecutionPolicy Bypass -File tools\ime_type_test.ps1 -Seq 'gb',' '        # → GB
powershell -ExecutionPolicy Bypass -File tools\ime_type_test.ps1 -Seq 'shishi',' '    # → 学过的词（实施）
```

实测：

| 项 | 结果 |
|----|------|
| ASR 精度 | far_3 97% / noise_en 98% / ja_en_codeswitch 76% / rag_math 100%（4/4 PASS） |
| ASR 速度 | RTF **0.019**，模型加载 0.8s |
| 长语音 | 1200s → **124 段**（静音断句生效后不再恒等 25s）→ 21 次注入，总耗时 21.8s，RTF 0.018 |
| 长语音 + 本地整理 | 1200s → 50 段 → 21 次注入**全部整理成功**，总耗时 37.5s，RTF 0.031 |
| 长文本整理（不截断） | 1540 字输入 → 输出 1415 字，8.1s，`finish_reason=stop`（旧实现固定 `max_tokens=512` → `finish_reason=length`，只出 797/1540 字） |
| 本地大模型 | 冷启动 23.7s / 单次整理 283~938ms / 复用 0ms / 退出回收 ✅ |
| 输入法（Rime） | 中文部署 16.0s / 切日文 6.8s / 卸载零残留 / `verify_rime.ps1` **18/18** ✅（含「热词表/学习钩子已编译进方案」两项硬校验）|
| 热词包（打字） | 284 条进表；实测 `gb→GB`、`tkl→TKL`、`qmk→QMK`、`gasket→Gasket`、`kaituan→开团`、`shangche→上车`；冲突码反向对照 `sa→撒` 未被抢 |
| 用户词学习 | `shishi` 选第 2 个「实施」→ 自动记下 → 重新写入表后 `shishi`+空格 **直接出实施**；6 字长句不乱记 |
| 快捷输入 | 手加 `gds` → 改表后（**无需重新部署**）打字 `gds`+空格 → 完整地址上屏 |
| 单元测试 | 分段器切点吸附/自适应噪声底/前导静音裁剪/纯噪声判定、重采样器分块无关性、剥 think 兜底、方案表与开关索引、热词 tsv 解析/去重/优先级、学习增量导入与删除语义、录音方式取值规范化与 toml 存盘回读、出字间隔四档与自定义值、悬浮窗停留五档/默认不显示文字/老配置兼容、自动保存的热键兜底（**71/71 PASS**） |
| 注入链路 | 记事本 E2E 落盘成功 |
| 热键链路 | 合成 F9 → 钩子 → 录音设备打开 → 静音/过短正确忽略 |
| 热键/挂起 E2E | `tools\e2e_hotkey_suspend.py` **15/15 连过 2 轮**：单实例秒退 / 挂起仍响应 F9 / 起头补 600ms（滞后 0ms）/ 未出字有反馈 |
| 录音方式 E2E | `tools\e2e_hotkey_mode.py` 旧包 **17/28** → 新包 **28/28**：toggle 松手不收尾、再按才收尾、手改 config.toml 自动生效、假松开不收尾、录音中 SUSPEND 不截断（旧包实测被砍掉 2.1s）、30Hz 自动重复不收尾 |
| 出字间隔（4 档） | `--simulate` 同一段 180s 音频：间隔 10s → 注入在 12/22/32/46/56…s（≈每 10s）；间隔 60s → 60/120/180s 各一次。GUI 实测下拉选「每 10 秒」→ `flush_seconds = 10` 并热生效 |
| 悬浮窗 E2E | `tools\e2e_float_window.py` **6/6**（启动带崩溃现场也不弹窗；会话期间可见；停留 3s → 实测 3.2s 消失、停留 10s → 9.9s 消失） |
| 崩溃自愈 E2E | `tools\e2e_crash_recovery.py` **8/8**：panic → `logs\last_panic.txt` 有线程名/位置/信息 → 自动拉起新实例 → 新实例提示「上次运行异常退出」→ 第二次崩溃不再自启 |
| 开机自启 / 单实例 | 通过 |

---

## 打包分发

```powershell
# 绿色 zip（不含模型，首次运行下载）
powershell -ExecutionPolicy Bypass -File installer\build.ps1
# 连模型一起打包（离线 / U 盘分发）
powershell -ExecutionPolicy Bypass -File installer\build.ps1 -WithModel
# 二合一安装包（语音 + Rime 键盘输入，含 575MB 离线资源）
powershell -ExecutionPolicy Bypass -File installer\build.ps1 -Setup
powershell -ExecutionPolicy Bypass -File installer\build.ps1 -Setup -NoLM   # 省掉 400MB 语法模型
```

| 产物 | 大小 | 预算 |
|------|------|------|
| `listen.exe`（静态 sherpa-onnx + 静态 CRT，单文件无 DLL） | **38.6 MB** | — |
| 绿色 zip（不含模型） | **15.2 MB** | — |
| 二合一安装包 `dist\listen-*-setup.exe`（含 Rime 资源约 622MB，其中语法模型 400MB） | **417,513,301 B（≈398 MiB）**，2026-10-21 重打，SHA `4af99020…` | — |
| 模型（首次运行下载） | 230 MB | — |

> 体积构成：本体 26.1MB（ASR + 网络 + 托盘）+ GUI（egui/eframe）6.5MB。
> 后端为 **wgpu**（`Cargo.toml` 的 `eframe` features 实测为 `["default_fonts", "wgpu"]`）。
> ⚠️ 2026-10-07 更正：此前本文件写“glow 后端 / glow 比 wgpu 省 6.6MB”与 `Cargo.toml` 不符；且启动日志显示 wgpu 选中了
> **NVIDIA RTX 5070 Ti**（与 llama-server 同一块卡）—— 这正是“整理时 GUI 卡顿”的物理前提，见《项目调试过程.md》§17.3-B。

---

## 目录结构

```
K:\AI_Voice_rs\
├─ config.toml          # 运行时配置（exe 同级，绿色）
├─ config.default.toml  # 出厂默认配置（打包时改名成 config.toml）
├─ 调试计划.md / 项目调试过程.md / README.md   # 三文件协同（另有 docs\archive\ 留档）
├─ models\sense-voice\  # 本地 ASR 模型
├─ logs\app.log         # 按天轮转（另有 llama_server.log = 本地大模型输出）
├─ src\
│   ├─ main.rs          # 入口：CLI / 单实例 / 工作线程 / 会话循环
│   ├─ gui.rs           # 设置界面（eframe/egui）+ 托盘宿主 + 首启向导
│   ├─ config.rs        # 配置结构 + 读写
│   ├─ state.rs         # 状态机
│   ├─ hotkey\          # 热键（windows.rs = WH_KEYBOARD_LL，不吞键）
│   ├─ audio.rs         # cpal 流式采集 + 流式重采样 + RMS
│   ├─ segmenter.rs     # 长语音分段（静音边界吸附）
│   ├─ asr\             # trait + local(SenseVoice) / cloud(OpenAI 兼容)
│   ├─ polish.rs        # 文本整理（云端 / 本地 llama-server + 剥 think 兜底）
│   ├─ llm_server.rs    # llama-server 生命周期（懒加载/复用/回收）
│   ├─ rime\            # Rime 二合一（mod.rs = 交付/部署/卸载；schemes.rs = 语言映射）
│   ├─ hotwords.rs      # 热词包（一份词表 → 打字 Rime 置顶候选 + 录音 ASR 热词）
│   ├─ learn.rs         # 用户词学习（Rime lua 日志 → 增量导入 → 我的词表）
│   ├─ inject.rs        # 剪贴板 + Ctrl+V
│   ├─ tray.rs          # 托盘图标与菜单
│   ├─ models.rs        # ASR 模型下载（hf-mirror → huggingface）
│   └─ selftest.rs      # 音频回归
├─ resources\rime\      # Rime 离线资源（不入库，fetch_resources.ps1 取齐）
│   ├─ weasel-0.17.4.0-installer.exe   # 自建 fork 包（官方原包备份在 official\）
│   ├─ schemes\{rime-ice, easy_en, jaroomaji}\
│   ├─ lm\wanxiang-lts-zh-hans.gram
│   └─ weasel-fork\     # fork 留档（README + Boost patch + 构建说明 + SHA256）
├─ resources\hotwords\  # 热词包源（入 git；include_str! 进二进制，首次运行铺到 exe 同级 hotwords\）
├─ resources\rime\lua\listen_learn.lua  # 用户词学习钩子（部署时铺到 Rime 用户目录）
├─ installer\
│   ├─ fetch_resources.ps1  # 取资源（国内镜像优先，575MB）
│   ├─ listen.iss           # Inno Setup 二合一安装包脚本
│   └─ build.ps1            # 打包：zip / -Setup 安装包 / -NoLM 省语法模型
├─ tools\               # E2E / 截图 / GUI 探针 / Rime 校验 / 热词生成 / 窗口诊断
├─ docs\archive\        # 历史计划与交接文档留档（2026-09-28 归档）
├─ dist\                # 产物（zip + setup.exe）
└─ .cache\              # sherpa-onnx 预编译库归档 + 测试音频（可删）
```

---

## 构建环境（开发机参考配置）

| 项 | 值 |
|----|-----|
| Rust | stable ≥ 1.94（`x86_64-pc-windows-msvc`） |
| 工具链 | MSVC + Windows SDK（VS 2022/2026 均可） |
| crates 镜像 | 可选；国内可用 rsproxy.cn 之类写入 `~/.cargo/config.toml` |
| sherpa-onnx | 1.13.8，静态链接；**官方 no-tts 预编译库**（无 espeak-ng(GPL)/piper_phonemize），走 `SHERPA_ONNX_LIB_DIR` |
| GUI | eframe 0.35（**wgpu** 后端，无 accesskit）；中文用系统字体 |

> no-tts 静态库需先放到 `.cache/`（`sherpa-onnx-v1.13.8-win-x64-static-MT-Release-no-tts-lib.tar.bz2`，115MB，
> 解压后由 `.cargo/config.toml` 的 `SHERPA_ONNX_LIB_DIR` 指向）；`vendor/sherpa-onnx-sys` 为本地 patch，
> 去掉了 espeak-ng(GPL-3.0)/piper_phonemize/ucd 的静态链接（本程序只用 ASR，不用 TTS）。

---

## 未完成（二期）

排期与详细起点见 `调试计划.md`（P0 收尾 / P1 观察项 / P2 backlog）：
macOS / Linux 三端（M6）· 法文方案包（`Lang::Fr` 已预留）· FunASR-Nano 高精度 · Silero VAD 分段 · 方案包/LM 更新通道。

---

## 许可证

代码以 **MIT** 发布（见 [LICENSE](LICENSE)）。第三方组件的许可与再分发注意事项见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。
