# 「K星输入法」改名 — 交接文档（2026-09-23）

> ⚠️ **2026-09-24：产品已改名「倾听输入法」**——第一波（K星输入法）已被取代，请转看《倾听输入法-改名交接.md》。本文仅存档。
>
> 本文件是改名任务的**唯一权威交接**。新会话读完本文 + `Weasel-Fork实施计划.md` §10 即可继续。
> 前序工作（Weasel fork + AI Voice 双向入口）已完成并通过验收，见 `Weasel-Fork实施计划.md`。

## 0. 已确认的改名规格（grill 定稿，勿再重新决策）

| # | 决策 |
|---|------|
| 1 | **范围**：只改**用户可见显示名**。内部标识全部不动：注册表键 `Software\Rime\Weasel`、`Uninstall\Weasel`、`Run\WeaselServer`、exe 名 `WeaselServer/Setup/Deployer.exe`、`weasel.ime/weaselx64.dll/weasel.dll`、安装目录 `Rime\weasel-0.17.4`、服务名 `WeaselInputService`、IPC `WeaselNamedPipe`、`WeaselDeployerMutex` 等 |
| 2 | **命名表**：语言栏/IME 列表/托盘 tooltip/气泡 = **K星输入法**；三个设置窗标题 = **【K星输入法】安装选项 / 方案选单设定 / 界面风格设定 / 用户词典管理**；部署等待·错误 MessageBox 标题 = **【K星输入法】**；开始菜单快捷方式 + 卸载列表 = **K星输入法**；安装包标题 = **K星输入法 0.17.4**；版本资源产品名/文件说明 = **K星输入法 / K星输入法算法服务 / K星输入法 TSF**；**英文 UI = KStar** |
| 3 | **简繁**：简体语言块用 `K星输入法`，繁体语言块用 `K星輸入法`，英文块用 `KStar` |
| 4 | **出处/许可（GPLv3）**：`LICENSE.txt` 与源码版权头原样保留；仓库 `README.md` 顶部 + 安装目录 `README.txt` 各加一行「本项目基于 rime/weasel 0.17.4（GPLv3）」；版本资源「版权」字段 = `基于 rime/weasel 0.17.4 (GPLv3)`；「公司名」= `K3R`；**UI 里不再出现"小狼毫"** |
| 5 | **AI Voice 侧文案跟着改**（48 处字符串），内部检测逻辑不动 |
| 6 | **版本号保持 `0.17.4.0`**（只改显示名） |
| 7 | **生效方式**：出新 NSIS 包 → 替换 `resources\rime\weasel-0.17.4.0-installer.exe` → 重编 AI Voice 安装包 → **用新包重装一次**（安装器注销旧 IME、注册新名字）→ **重启一次**（释放被占用的 IME dll） |

**Logo**：kevin 自制。规格：多尺寸 `.ico`（256/64/48/32/24/16，32 位带 Alpha），或给 6 个尺寸 PNG 由我打包；放 `K:\K3R-voice\resource\k3r\`。涉及 7 个图标：`resource\{weasel,zh,en,reload,full,half}.ico` + `WeaselSetup/WeaselSetup.ico`。**约束**：16×16 可辨、zh/en 状态可区分、小尺寸不放文字。

## 1. 进度

| 步骤 | 状态 |
|------|------|
| A1 `get_weasel_ime_name()` → K星输入法/KStar | ✅ 已完成（`include/WeaselUtility.h`） |
| A2 5 个 `.rc` 字符串替换 | ✅ 已完成（首轮 kevin 执行；**发现并修复简繁倒置**：第二遍 29 条定点替换 + 独立校验通过，见 §6） |
| A3 `output/install.nsi` 改名 | ✅ 已完成（标题/快捷方式/卸载列表/Publisher/版本资源） |
| A4 `README.txt` / `README.md` 加 GPLv3 出处 | ✅ 已完成 |
| A5 `include/WeaselConstants.h` 的 `WEASEL_CODE_NAME` | ✅ **结论：不改**（喂 librime `distribution_code_name` 内部标识；IME 显示名由 A1 覆盖，见 §6.1） |
| A6 `WeaselSetup/WeaselSetup.cpp` 帮助 MessageBox 标题 | ✅ 已完成（+ `WeaselServerApp.h` 更新检查提示） |
| B AI Voice 侧 48 处文案 | ✅ 已完成（src 44 处 + README 6 处 + .gitignore） |
| C 重编 Weasel x64+Win32 | ✅ 已完成（0 error，48 个原有弃用警告） |
| D 重出 NSIS 包 + 替换资源 + 重编 AI Voice 安装包 | ✅ 已完成（资源已替换；`dist\ai-voice-0.1.0-setup.exe` 416MB） |
| E 留档 SHA256 + git 提交推送 + 计划文档 | ✅ 已完成（留档/文档已更新；`aivoice` 单提交 amend 为 `09b5dcb` 并 force-with-lease 推送） |
| F 换图标 | 🟡 已生成+重编+出包+替换资源（2026-09-24 18:38，见 §6.4）；待重装验收 |

## 2. 被 dcg 拦截的操作 + 替代方案（kevin 需决策）

**被拦**：一条内联 PowerShell 批量替换 5 个 `.rc` 的命令（dcg 误判为 "POSIX command substitution"）。
按 dcg 规则不得绕过，故改为**kevin 手动执行**：

- 脚本：`K:\Libraries\rebrand_rc.ps1`（UTF-8 **带 BOM**，勿改编码）
- 一键：`K:\Libraries\rebrand_rc.bat`（GBK+CRLF，双击即可）
- 逻辑：按 `LANGUAGE` / `BLOCK "0804|0404|0409"` 判定简繁英 → 替换 `小狼毫`/版本资源产品名/公司名/版权；**不动** `InternalName`、`OriginalFilename`、文件名
- 跑完会打印每个文件的「残留小狼毫=N / K星=N」用于校验（残留应为 0）

**等 kevin 三选一**：① 手动跑（推荐）② 指定别的安全做法 ③ 跳过（后果：设置窗标题与版本资源仍是"小狼毫"）

## 3. 关键路径与工具链（已就绪）

| 用途 | 路径 |
|------|------|
| fork 源码 | `K:\K3R-voice`（分支 `aivoice`，远端 `origin`=kevinx-key/K3R-voice 公开仓库，`upstream`=rime/weasel） |
| Boost | `K:\Libraries\boost-1.84.0`（已打 msvc-14.5 补丁） |
| 构建脚本 | `K:\Libraries\weasel_build4.bat`（`RELEASE_BUILD=1`，输出日志 `weasel_build4.log`） |
| NSIS | `K:\Libraries\nsis_build.bat` → `K:\Libraries\nsis-3.10\Bin\makensis.exe` |
| 产物 | `K:\K3R-voice\output\`（x64）+ `output\Win32\` |
| AI Voice 安装包 | `K:\app\InnoSetup\ISCC.exe /DIncludeLM=1 K:\AI_Voice_rs\installer\ai-voice.iss` → `dist\ai-voice-0.1.0-setup.exe` |
| 留档 | `K:\AI_Voice_rs\resources\rime\weasel-fork\`（README + patch + 构建说明 + SHA256） |
| 验收脚本 | `K:\Libraries\verify_spike.bat` / `rollback_spike.bat` |

## 4. 环境坑（勿踩）

- **`.rc` 是 UTF-16 LE**：`edit`/`read` 工具读不了，只能用脚本改；改完**必须**保持 UTF-16（`[System.Text.Encoding]::Unicode`）
- **dcg 会拦**：内联复杂 PowerShell（`$(...)`、`ForEach-Object { $_ }`、`powershell -File`）。规则：被拦即停下问 kevin，**不得绕过**
- **WeaselServer 必须普通用户身份运行**：提权的话 ① 解析不到 `ai-voice.exe` ② 设置窗弹在后面且抢不到焦点。`verify_spike.bat` 已用 `explorer.exe` 代拉
- **AI Voice 模型路径**：裸 exe 按「exe 同级」找 `models/sense-voice`；已建 junction `K:\AI_Voice_rs\target\release\models` → `K:\AI_Voice_rs\models`（`cargo clean` 后会丢）
- **App Paths**：`HKCU\...\App Paths\ai-voice.exe` 有一条指向开发版 exe 的测试键（正式安装后应删）
- **`/deploy` 很慢**（rime-ice + 万象语法），不是卡死；日志"停在半行"是 glog 缓冲假象

## 5. 改完后要同步更新的东西

- `K:\AI_Voice_rs\resources\rime\weasel-fork\README.md` 的 SHA256（3 个 exe + rime.dll + 安装包）
- `K:\AI_Voice_rs\Weasel-Fork实施计划.md` §3 进度表 + 新增 §11「改名」小节
- git：`aivoice` 分支提交（按之前的做法，若要保持"单提交"需 `--force-with-lease`，**先问 kevin**）

---

## 6. 执行记录（2026-09-24，全部完成）

### 6.1 关键发现与修复

- **首轮 .rc 脚本的简繁倒置**：`BLOCK "0804"`（简体）被当成繁体、`"0404"`（繁体）当成简体，
  导致版本资源与部分字符串简繁混排（如「K星輸入法部署应用」）。已用第二遍定点替换修复：
  `K:\Libraries\rebrand_rc_fix.ps1` + `rebrand_rc_fix.txt`（kevin 手动执行，**29 条全部写入，0 不匹配**）。
  独立校验：38 处 `K星` 全部落在正确语言段；英文段 12 处 `KStar`；`.rc` 内无「小狼毫」。
- **A5 不改**：`WEASEL_CODE_NAME` 同时喂 librime `distribution_code_name`（内部标识，改动会触发
  一次性重部署）与 CLSID 注册描述；IME 显示名实际来自 `get_weasel_ime_name()`（A1 已覆盖）。
- **官方 exe 的版本信息本来就 `FileVersionInfo` 读不到**（从官方原包提取的 WeaselServer.exe 同样如此）
  ——Weasel 上游特性，非本次回归；已用字节级校验确认新字符串（K3R / K星输入法 / KStar）编入资源。
- dcg 误报处理：首轮批量 .rc 替换与本次的编码转换命令均被 dcg 误拦（反引号被判为 POSIX 命令替换）；
  按规则停下询问 kevin 后，第二遍改用「纯 ASCII 脚本 + 独立 UTF-8 数据文件」架构，从根上规避编码转换。

### 6.2 产物 SHA256（2026-09-24，改名+图标版）

| 产物 | SHA256 |
|------|--------|
| `weasel-0.17.4.0-installer.exe`（11,990,374 B） | `51F067B822B3BA42564CCD700F1BB7996893F9B8EAF91BC0584EF0DAFB340897` |
| WeaselServer.exe | `AA20C40BFAC4FC0F842DBE76C327C4B817C382C6A403EFC7CE6ACB6098410B4E` |
| WeaselSetup.exe | `F2B8736C1A469834D2C53AA0C43D6986AB28EEA958962238E951AB08ED5A0D52` |
| WeaselDeployer.exe | `7B692F57907F8E49DED71F92C62F2D97F85F70E4211F3641A45392168F8C1850` |
| `ai-voice-0.1.0-setup.exe`（416,365,193 B） | `617DB47F006F22BB1C6E38EF602D61292D1EC408CB13BCF787CE86299BFE22F0` |

- 资源已替换：`K:\AI_Voice_rs\resources\rime\weasel-0.17.4.0-installer.exe` = 上表第一个（2026-09-24 18:36）
- 改名前的自建包备份：`resources\rime\official\weasel-0.17.4.0-installer.exe.fork-prerename`
- 历史（改名版，未含图标，已被覆盖）：installer `AA3947703AD43979990864494179553EB3F76CC188F723F467F9CDEEEFA6A8E8`（12,060,165 B）、Server `0267E344…`、Setup `A9190592…`、Deployer `2EB7A4FB…`

### 6.3 待 kevin

1. **重装** `K:\AI_Voice_rs\dist\ai-voice-0.1.0-setup.exe`（图标版，416,365,193 B）→ **重启一次** → 验收（**7 个图标** / IME 列表 / 三个设置窗标题 / 托盘 tooltip / 卸载列表 / 文件属性）
2. 删 `HKCU\Software\Microsoft\Windows\CurrentVersion\App Paths\ai-voice.exe` 测试键（正式安装会写 HKLM）
3. **git 提交推送**：`aivoice` 单提交 amend 为 **`09b5dcb`**（含集成 + 改名）已推送 https://github.com/kevinx-key/K3R-voice；**图标 + 文档改动尚未提交**（工作区：7 个 `.ico` + `README.md` + `K3R-VOICE-BUILD.md` + 新增 `resource\k3r\`）——是否沿用「单提交 amend + `--force-with-lease`」等 kevin 定
4. 图标（F）：✅ 生成/替换/重编/出包 全部完成（见 §6.4）；剩余 = 重装验收

### 6.4 图标（F，2026-09-24 生成并替换）

**源文件**：`C:\Users\kevin-3080\Desktop\k星输入法logo.svg`（CorelDRAW 导出，画布 210mm×297mm）。
**处理**：裁到中心圆角矩形区（viewBox `7643.5 8906.65 3500 3500`）→ 母版 `K:\K3R-voice\resource\k3r\kstar-logo.svg`（2 条 #2C46FF 路径，与源文件逐字节一致；原圆角矩形是 `fill:none` 不可见，仅作边界参考）。

**设计规格（kevin 选定）**：

| 图标 | 设计 |
|------|------|
| weasel / zh / WeaselSetup | 蓝底 `#2C46FF` 圆角方（rx 20%）+ 白色图形（图形宽 = 底宽 80%） |
| en | 白底 + 蓝形 + 描边 `#C8CDD8`（颜色反转；16px 下与 zh 差异 98% 像素，可一眼区分） |
| full | 蓝底白形（图形 66%）+ 底部宽白条（70% 宽） |
| half | 蓝底白形（图形 66%）+ 底部窄白条（30% 宽） |
| reload | 蓝底 + 白色刷新箭头 |

**规格**：6 尺寸 16/24/32/48/64/256，32 位带 Alpha；每尺寸由 Chromium 直接矢量渲染（非缩放降采样），Pillow 打包 ICO。
**产物**：`resource\{weasel,zh,en,full,half,reload}.ico` + `WeaselSetup\WeaselSetup.ico`（各 8.6–11 KB）；PNG 源 42 张在 `resource\k3r\icons\`。
**验收预览**：`C:\Users\kevin-3080\Desktop\K星图标成品预览.png`（明/暗底 × 256/48/32/24/16px，含 16px 4 倍放大）。
**重编记录（2026-09-24 18:35–18:38）**：Weasel x64+Win32 重编成功（0 警告 0 错误，21.9s 增量）→ NSIS 出包 11,990,374 B → 已替换 `resources\rime\weasel-0.17.4.0-installer.exe` → Inno Setup 重编 `dist\ai-voice-0.1.0-setup.exe`（416,365,193 B，28.9s）。图标嵌入已二进制验证（exe/dll 内含新 ICO 的 PNG 数据，全部产物 0 处「小狼毫」）。
**剩余步骤**：重装 `dist\ai-voice-0.1.0-setup.exe` + 重启一次 → 验收。

### 6.5 改名完整性扫描（2026-09-24）

全仓（K3R-voice + AI_Voice_rs）按 UTF-16 / UTF-8 / GBK 三种编码扫描「小狼毫」。

**已确认替换**：5 个 `.rc`（0 残留、K星 38 处）、`output\install.nsi`（K星 29 处）、编译产物 x64+Win32 全部 exe/dll（三种编码均 0 处「小狼毫」，K星/KStar 已编入）、AI Voice 文案（README / ai-voice.iss）。

**本轮补改**（kevin 选定）：
- `README.md` + `output\README.txt`：标题【小狼毫】輸入法 → 【K星輸入法】；正文 3 处提及改 K星輸入法（保留 GPLv3 出处行与上游链接）。`output\README.txt` 是 build.bat 从 README.md 复制生成的构建产物（.gitignore 已忽略），已同步。
- `K3R-VOICE-BUILD.md` ×2 份（`K3R-voice\` + `AI_Voice_rs\resources\rime\weasel-fork\`）：3 处旧对话框标题改 K星输入法。

**保留不改**（157 处，均为合理保留）：
- 上游历史/数据：`CHANGELOG.md`(59)、librime / output / plum / spike-test 的 essay 词典词条(6)、plum README(1)、`update\appcast*.xml`(6)、雾凇拼音方案包（词库/文档，19 处）
- 源码注释 5 处（被注释掉的旧代码：`Configurator.cpp` / `SwitcherSettingsDialog.cpp` / `imesetup.cpp`）— kevin 选保留
- 测试字符串 1 处（`TestWeaselIPC.cpp`）— kevin 选保留
- 历史计划文档（AI_Voice_rs 三份计划 + 本文档，40 处）
- 构建陈旧缓存 `msbuild\Release\x64\*\RCa*.rc`(12) — 9-23 旧构建残留，重编自动覆盖
