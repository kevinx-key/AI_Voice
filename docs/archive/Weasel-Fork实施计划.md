# Weasel Fork 实施计划（仓库名：K3R-voice）

> **版本** v1.0 ｜ **定稿日期** 2026-09-23 ｜ **状态**：方案已 grill 定稿（Q1~Q7 全部确认）、环境预检已完成，待执行
> **新会话开工方式**：读完本文档 → 从「§4 第 1 步」开始执行 → 每完成一步回来更新 §3 进度表
> **关联项目**：`K:\AI_Voice_rs`（语音 + 键盘二合一）

---

## 1. 目标

让**小狼毫的「输入法设定」窗口和托盘右键菜单里能直接打开 AI Voice 设置页**（双向统一入口），结束"两个程序各有一个设置面"的现状。

**为什么必须 fork**（结论，不要再重新调研）：

- 「输入法设定」= 上游官方 Weasel 的 `WeaselSetup.exe`，**无插件/扩展机制**；本项目装的正是官方安装包（`resources\rime\weasel-0.17.4.0-installer.exe`）
- Weasel 对外只有三个控制面：YAML 配置 / 命令行（WeaselDeployer 等）/ 注册表 —— 只能外部控制，无法在它 UI 里挂第三方内容
- 要在它 UI 里加 AI Voice 入口，唯一路径 = 改源码重编译（GPLv3 允许；公开仓库即满足源码义务）

## 2. 决策汇总（grill 定稿，Q1~Q7）

| # | 项 | 决策 |
|---|----|------|
| 1 | 统一入口 | **C+D**：fork 侧「托盘菜单项」+「输入法设定窗口按钮」；AI Voice 侧反向三按钮（打开小狼毫设置 / 用户目录 / 重新部署）。入口只做"唤起 AI Voice 设置窗"，**不复制设置项** |
| 2 | 可行性策略 | **S1**：先编原版 0.17.4 spike（一行代码不改）→ 通过才动代码；不通过退回 A+C（无损失） |
| 3 | 仓库 | **R2**：GitHub **公开**仓库 `K3R-voice`；`master`=upstream 0.17.4 全量历史，`aivoice` 分支=单提交；AI_Voice_rs 留档 |
| 4 | 交付 | **D3**：spike 先覆盖 exe 验证；交付用**自建 NSIS 包**替换 `resources\rime\weasel-0.17.4.0-installer.exe` |
| 5 | 更新通道 | **A**：fork **关掉 WinSparkle 更新检查**；版本号保持 `0.17.4.0`（检测/NSIS 零改动） |
| 6 | 唤起机制 | 三件套：**命名事件 IPC + `--settings` CLI + App Paths 定位** |
| 7 | 验收 | 见 §6 清单（A/B/C/D 共 11 项） |

## 3. 进度表

| 步骤 | 状态 |
|------|------|
| 环境预检（磁盘/代理/Git） | ✅ 已完成（2026-09-23） |
| 第 1 步 · clone 上游源码 | ✅ `K:\K3R-voice`，tag 0.17.4（`9cc96e2`），子模块含 librime 1.13.1 |
| 第 2 步 · 锁定依赖版本 | ✅ Boost **1.84.0**；工具集 **v145 / msvc-14.5**（本机只有 MSVC 14.50 且**无管理员权限**，故不装 v143）；CMake 4.2.3（VS18 自带）；NSIS 3.10 便携版 |
| 第 3 步 · 装工具链 | ✅ 见 §10「与计划的偏差」——改用 14.50 并 patch Boost.Build 支持 14.5 |
| 第 4 步 · 编 Boost | ✅ `K:\Libraries\boost-1.84.0`，stage\lib 44 个 lib（x64+x86 / static / static-CRT） |
| 第 5 步 · 编 librime | ✅ **含 lua + octagram + predict 插件**（关键：用户用雾凇拼音，缺 lua 会报错） |
| 第 6 步 · 编 Weasel 原版（spike） | ✅ x64 + Win32 各 0 error，产物齐全（WeaselServer/Setup/Deployer + weasel(x64).ime） |
| 第 7 步 · 替换验证（验收 A） | ⏸ **需 kevin 提权执行**（脚本已备好）；已完成旁路冒烟测试，见 §10 |
| 第 8 步 · fork 代码改动（单提交） | ✅ 代码已改 + 编译通过（**尚未 git commit**，见 §10 待办） |
| 第 9 步 · AI Voice 侧改动 | ✅ `--settings` / 命名事件 IPC / App Paths / 反向按钮，cargo build 通过并实测唤起成功 |
| 第 10 步 · NSIS 出包替换资源（验收 D） | ✅ 自建包 `weasel-0.17.4.0-installer.exe`（12MB）已替换资源；官方包备份在 `resources\rime\official\`；AI Voice 安装包已重编（397MB） |
| 第 11 步 · GitHub 建仓 + 留档 | ✅ 公开仓库 **https://github.com/kevinx-key/K3R-voice**（`master`=0.17.4 @ `9cc96e2`，`aivoice`=单提交 `09b5dcb`（2026-09-24 amend，含改名），tag `0.17.4`）；本地留档 `resources\rime\weasel-fork\` |
| 第 12 步 · 改名 K星输入法（显示名） | ✅ 2026-09-24 代码/文案/打包全部完成，见 §11；待 kevin 重装 + 重启验收 |

## 4. 已查证的环境事实（勿重复调查）

| 项 | 事实 |
|----|------|
| 系统 | Windows 10 + PowerShell 5.1 |
| 网络 | `HTTPS_PROXY=http://127.0.0.1:7897` 已设且**已验证**（`curl github.com` → 200）；git 全局 `http.sslverify=false`；git 身份 `709208969 / 709208969@qq.com`；另有 ghproxy.net 凭据 |
| 磁盘 | K: 剩余 5.4TB（空间无忧） |
| VS | VS18 Community（`C:\Program Files\Microsoft Visual Studio\18\Community`），**MSVC 工具集只有 14.50.35717，无 v143** |
| 缺失依赖 | CMake 未装、NSIS 未装、Boost 无 |
| 小狼毫（现装） | `C:\Program Files\Rime\weasel-0.17.4\`（官方 stock 版，WeaselServer/WeaselSetup/WeaselDeployer 齐全；WeaselServer 正在运行） |
| Weasel 上游 | `github.com/rime/weasel`，最新 release **0.17.4（2025-06-04）**，GPLv3；构建入口 `build.bat` + `env.bat.template`（**BOOST_ROOT 必填**，示例 boost_1_78_0；默认 `BJAM_TOOLSET=msvc-14.2`/`PLATFORM_TOOLSET=v142`；`DEVTOOLS_PATH` 需 git/cmake）；子模块含 librime、plum；NSIS 出包 |
| 「输入法设定」 | = `WeaselSetup.exe`（窗口标题【小狼毫】安装选项；asInvoker 不需管理员、未签名） |
| AI Voice | `K:\AI_Voice_rs`；安装器 `installer\ai-voice.iss`（`DefaultDirName={autopf}\AIVoice`、`PrivilegesRequired=admin`、`AppMutex=Global\AIVoiceRust`、**无 [Registry] 段**） |
| AI Voice 单实例 | 互斥体 `Global\AIVoiceRust`（`src/main.rs:1058-1072`）；第二次启动命中后**只打印"已在运行"就退出**（`src/main.rs:174-185`，注释写了"唤起设置窗"但**未实现**） |
| AI Voice 其他锚点 | 托盘「打开设置…」`src/tray.rs:72`；托盘事件轮询 `src/gui.rs:184-187`；设置页「输入法」区 `src/gui.rs:934+` |
| Rime 用户目录 | `C:\Users\kevin-3080\AppData\Roaming\Rime`（**勿动用户词库/配置**） |
| 部署日志 | `%TEMP%\rime.weasel\` |

## 5. 执行步骤

### 第 1 步 · clone 上游源码（预期 10~30 分钟）

```powershell
git clone https://github.com/rime/weasel.git K:\K3R-voice
cd K:\K3R-voice
git checkout 0.17.4
git submodule update --init --recursive
```

- 先 `--init`（一级）也行，按需再递归；librime 的子模块（glog/leveldb/opencc/yaml-cpp/marisa/gtest 等）编 librime 前必须齐
- 完成后把 `git log -1` 与子模块清单记回本文档

### 第 2 步 · 锁定依赖版本（以 clone 下来的源码为准）

要确认的四件事（不要凭记忆）：

1. **Boost 版本** —— 读 `librime/CMakeLists.txt`、`env.bat.template`、`.github/workflows/`（上游示例 1.78.0）
2. **工具集** —— 上游默认 v142；本机只有 14.50 → 计划装 **v143**，env.bat 里设 `PLATFORM_TOOLSET=v143`、`BJAM_TOOLSET=msvc-14.3`
3. **CMake 最低版本**
4. **NSIS** —— 仅第 10 步出包需要

### 第 3 步 · 装工具链

```powershell
# CMake（走清华 PyPI，最快）
pip install cmake -i https://pypi.tuna.tsinghua.edu.cn/simple
# 或 winget install Kitware.CMake

# Boost 源码（清华镜像；版本以第 2 步为准）
#   下 https://mirrors.tuna.tsinghua.edu.cn/boost/<ver>/boost_<ver>.zip
#   解压到 K:\Libraries\boost_<ver>

# v143 工具集（二选一，需 UAC）：
#   a) VS Installer → 修改 VS18 → 单个组件搜 "v143" 勾选安装
#   b) 独立装 VS2022 Build Tools（不动现有 VS18）
# 验证：C:\...\VC\Tools\MSVC\ 下出现 14.3x 目录

# NSIS（第 10 步再装）：winget install NSIS.NSIS
```

### 第 4 步 · 编 Boost

```powershell
cd K:\Libraries\boost_<ver>
.\bootstrap.bat
.\b2.exe --toolset=msvc-14.3 address-model=64 runtime-link=shared -j16 `
  --with-filesystem --with-regex --with-system --with-thread --with-date_time ...
```

- **只编 librime 需要的库**（清单以 librime 文档/CI 为准，别全编）
- 产物在 `stage\lib`；后续 `BOOST_ROOT` 指向源码根

### 第 5 步 · 编 librime

```powershell
cd K:\K3R-voice\librime
git submodule update --init --recursive
cmake -B build -G "Visual Studio 17 2022" -A x64 -DBOOST_ROOT=K:\Libraries\boost_<ver>
cmake --build build --config Release -j16
```

- 产物：`rime.dll` + `rime.lib`（供 Weasel 链接）
- 具体参数以 librime 自带文档为准

### 第 6 步 · 编 Weasel 原版（spike，**不改一行代码**）

```powershell
cd K:\K3R-voice
copy env.bat.template env.bat
# 编辑 env.bat：BOOST_ROOT / PLATFORM_TOOLSET=v143 / BJAM_TOOLSET=msvc-14.3 / DEVTOOLS_PATH（git+cmake）
.\build.bat
```

- 目标产物：`WeaselServer.exe`、`WeaselSetup.exe`（+ WeaselDeployer 等）
- spike 通过标准 = 能编出来 + 第 7 步替换后功能正常

### 第 7 步 · 替换验证（验收 A）

```powershell
# 1) 备份原版（提权）
copy "C:\Program Files\Rime\weasel-0.17.4\WeaselServer.exe" "C:\Program Files\Rime\weasel-0.17.4\WeaselServer.exe.orig"
copy "C:\Program Files\Rime\weasel-0.17.4\WeaselSetup.exe"  "C:\Program Files\Rime\weasel-0.17.4\WeaselSetup.exe.orig"
# 2) 覆盖为 spike 产物（Program Files 写入需提权 → 提权 PowerShell / 让 kevin 点 UAC）
# 3) 重启服务
cd "C:\Program Files\Rime\weasel-0.17.4"
.\WeaselServer.exe /q
Start-Process .\WeaselServer.exe
```

- 验收 A：打字 / 候选 / 重新部署 / 托盘菜单正常
- 失败 → 还原 `.orig`，按 §7 处理

### 第 8 步 · fork 代码改动（单提交，hunk 最小化）

改动点（具体位置以 clone 源码为准，预期如下）：

1. **托盘菜单项**：`WeaselServer/WeaselServerApp.cpp` 菜单构建处 + 命令处理 → 新增「AI Voice 设置(&V)」→ `ShellExecuteW(NULL, L"open", L"ai-voice.exe", L"--settings", NULL, SW_SHOWNORMAL)`；失败弹 MessageBox（"未找到 AI Voice，请先安装"）
2. **设置窗按钮**：`WeaselSetup` 对话框加「AI Voice 设置」按钮，行为同上
3. **关更新检查**：搜 `WinSparkle` / `CheckForUpdates` 停用（fork 不跟随上游升级）
4. 版本号保持 `0.17.4.0`

提交策略：`master` 停在 upstream 0.17.4；新分支 `aivoice` 放单提交

### 第 9 步 · AI Voice 侧改动（`K:\AI_Voice_rs`）

1. **`--settings` CLI**：参照现有 `--first-run`；打开设置窗；无参默认行为不变
2. **IPC 唤起**（三件套之一）：
   - 常驻实例：`CreateEventW` 建 `Global\AIVoiceShow` + 监听线程（`WaitForSingleObject`）→ 收到信号 → `ctx.send_viewport_cmd(Visible(true)/Focus)` + `request_repaint()`
   - 第二次启动（`src/main.rs:174-185` 的 `Ok(None)` 分支，注释本来就写着"唤起设置窗"）：`OpenEventW` + `SetEvent` 后退出
3. **App Paths**（`installer/ai-voice.iss` 新增 `[Registry]`）：`HKLM\Software\Microsoft\Windows\CurrentVersion\App Paths\ai-voice.exe` 默认值 `{app}\ai-voice.exe`
4. **反向三按钮**（设置页「输入法」区，`src/gui.rs` section_rime）：打开小狼毫设置（`{install_dir}\WeaselSetup.exe`）/ 打开用户目录 / 重新部署（复用现有 deploy 流程）
5. 文档更新（README 输入法章节 + 本文档进度表）

### 第 10 步 · NSIS 出包 + 替换资源（验收 D）

- 用 fork 源码出 NSIS 安装包（版本显示保持 0.17.4.0）
- 替换 `K:\AI_Voice_rs\resources\rime\weasel-0.17.4.0-installer.exe`（**先把官方原包备份到 `resources\rime\official\`**）
- 验收 D：全新流程 = 安装 → 首启向导 → 静默 `/S` 装我们的包 → 部署成功

### 第 11 步 · GitHub 建仓 + 留档

- 建**公开**仓库 `K3R-voice`（需要 kevin 的 GitHub 账号/凭据，推送前确认）
- `master` = upstream 0.17.4 全量历史；`aivoice` = 单提交
- AI_Voice_rs 留档：`resources\rime\weasel-fork\`（upstream 版本 + patch 文件 + 构建步骤 + 产物 SHA256）

## 6. 验收清单（定稿，同 grill Q7）

**A. 构建层**
1. 原版 0.17.4 编译通过，产出 `WeaselServer.exe` / `WeaselSetup.exe`
2. 手工替换后：打字、候选、重新部署、托盘菜单正常

**B. 集成层**
3. 托盘右键「AI Voice 设置」：未运行→启动并打开设置；已运行隐藏→置前；已在前台→无异常
4. 「输入法设定」窗口按钮，行为同上
5. 找不到 `ai-voice.exe` → 弹提示不崩
6. AI Voice 设置页反向三按钮可用

**C. 回归**
7. 打字 / F4 切方案 / Ctrl+Shift+4 简繁 / 重新部署 不受影响
8. Shift 误触不再切英文（配置已改，部署后验证）
9. 卸载干净；更新检查已关（不会提示升级官方版）

**D. 交付层**
10. NSIS 自建包替换后，全新流程（安装→向导→部署）成功
11. GitHub fork 公开 + AI_Voice_rs 留档

## 7. 风险与回退

| 风险 | 应对 |
|------|------|
| 新工具集（14.50）编老代码失败 | 装 v143（VS2022 工具集）规避 |
| Boost / librime 编译踩坑 | 以 librime 官方 CI 配置为准，别自由发挥；单点卡超半天 → 评估回退 |
| 网络（GitHub / 依赖下载） | 本机代理 `127.0.0.1:7897` 已验证；Boost 走清华镜像 |
| Program Files 写入需提权 | 覆盖/安装步骤提权执行，提示 kevin 点 UAC |
| **总回退** | 编不出来 → 退回 **A+C**（AI Voice 设置页做总控 + 反向按钮），零损失 |

## 8. 关联事项（本计划外，别忘）

- `%APPDATA%\Rime\default.custom.yaml` 已加 `Shift_L/Shift_R: noop`（防误触切英文）——**尚未重新部署**，找机会部署验证
- 上一轮"托盘右键打不开输入法设定"：已重启 WeaselServer，等 kevin 复验；fork 替换后一并观察
- Rime 用户目录 `C:\Users\kevin-3080\AppData\Roaming\Rime`：**只碰我们管理的键**，勿动用户词库/配置

## 9. 关键路径索引

| 用途 | 路径 / 地址 |
|------|------------|
| 上游源码 | https://github.com/rime/weasel （tag 0.17.4） |
| 本地 fork 工作目录（待建） | `K:\K3R-voice` |
| 小狼毫安装目录 | `C:\Program Files\Rime\weasel-0.17.4\` |
| 待替换的官方安装包 | `K:\AI_Voice_rs\resources\rime\weasel-0.17.4.0-installer.exe` |
| AI Voice 源码 | `K:\AI_Voice_rs\src\`（main.rs / tray.rs / gui.rs） |
| AI Voice 安装器 | `K:\AI_Voice_rs\installer\ai-voice.iss` |
| Rime 用户目录 | `C:\Users\kevin-3080\AppData\Roaming\Rime` |
| 部署日志 | `%TEMP%\rime.weasel\` |
| 本计划 | `K:\AI_Voice_rs\Weasel-Fork实施计划.md` |

---

## 10. 执行记录（2026-09-23 自主执行）

### 10.1 与计划的偏差（必须知悉）

| # | 计划原定 | 实际做法 | 原因 |
|---|---------|---------|------|
| 1 | 装 **v143** 工具集 | 改用本机已有的 **MSVC 14.50 / 工具集 v145** | 本机无 v143，装它需要管理员（UAC）；自主执行时无人点 UAC |
| 2 | Boost 官方 b2 直接编 | **patch 了 `tools/build/src/tools/msvc.jam` + `engine/config_toolset.bat` + `engine/guess_toolset.bat`**，加入 14.5 支持；并 patch `boost/config/auto_link.hpp` 让 autolink 用 `vc145` 标签 | Boost 1.84 的 Boost.Build 只认到 14.3（VS2022） |
| 3 | 直接编原版 spike | 额外**新增 `include/afxres.h` shim**（内容等价 `winres.h`） | 本机 VS18 未装 MFC，三个 `.rc` 的 `#include "afxres.h"` 会 RC1015 报错 |
| 4 | librime 按文档编 | **额外加 3 个插件**：`hchunhui/librime-lua`、`lotem/librime-octagram`、`rime/librime-predict`（+ `librime-lua` 的 `thirdparty` 分支提供 Lua 5.4 源码） | 官方 weasel 的 rime.dll 带这三插件；用户用**雾凇拼音**，缺 lua 会 `error creating processor: 'lua_processor'` |
| 5 | 依赖下载走国内镜像 | Boost 走 **GitHub release（代理）**；NSIS 走 **SourceForge** | 清华 boost 镜像 404、中科大 403，国内源不可用 |
| 6 | 输入法设定窗口加按钮 | **加了两个对话框**：`WeaselSetup`（【小狼毫】安装选项）+ `WeaselDeployer`（【小狼毫】方案选单设定，即托盘「输入法设定」真正打开的窗口） | 计划把「输入法设定」记成了 WeaselSetup.exe，实际托盘「输入法设定」= WeaselDeployer.exe；两处都加才覆盖验收 B4 |
| 7 | 改 `.rc` 资源加菜单项/按钮 | **全部用 C++ 动态创建**（托盘用上游预留的 `CustomizeMenu(HMENU)` 钩子；按钮用 `CreateWindowExW`） | `.rc` 是 UTF-16，本环境编辑工具链不支持直接改；且这两个 .rc 是上游文件，不动可减少冲突 |

### 10.2 代码改动清单

**fork 侧（`K:\K3R-voice`）**

| 文件 | 改动 |
|------|------|
| `WeaselServer/resource.h`、`include/resource.h` | 新增 `ID_WEASELTRAY_AIVOICE 40017` |
| `WeaselServer/WeaselTrayIcon.cpp` | `CustomizeMenu()` 在「输入法设定」下方插入「AI Voice 设置 (&V)」 |
| `WeaselServer/WeaselServerApp.cpp` | 注册菜单处理器 → `LaunchAIVoiceSettings()`；**注释掉 WinSparkle 初始化/清理**（关更新检查） |
| `WeaselServer/WeaselServerApp.h` | `check_update()` 改为弹提示「本版本已关闭更新检查（K3R-voice）」 |
| `include/WeaselUtility.h` | 新增 `LaunchAIVoiceSettings(HWND)`：`ShellExecuteW(NULL,"open","ai-voice.exe","--settings",…)`，失败弹「未找到 AI Voice，请先安装。」 |
| `WeaselSetup/resource.h`、`InstallOptionsDlg.h/.cpp` | 【小狼毫】安装选项 对话框底部动态加「AI Voice 设置 (&V)」按钮 |
| `WeaselDeployer/resource.h`、`SwitcherSettingsDialog.h/.cpp` | 【小狼毫】方案选单设定（托盘「输入法设定」）底部动态加同一按钮 |
| `include/afxres.h`（新增） | MFC 缺失时的资源头 shim |

**AI Voice 侧（`K:\AI_Voice_rs`）**

| 文件 | 改动 |
|------|------|
| `src/main.rs` | 新增 `--settings`；新增 `create_show_event()` / `signal_show_settings()`（`Global\AIVoiceShow` 命名事件）；第二实例命中时 `SetEvent` 后退出（原来只打印"已在运行"） |
| `src/gui.rs` | `Gui` 新增 `show_event`/`show_requested`；`start_show_watcher()` 起监听线程（`WaitForSingleObject` → `request_repaint`）；每帧消费标志 → `show_window(ctx)`；「输入法」区新增「小狼毫设置」「小狼毫安装选项」两个反向按钮 |
| `installer/ai-voice.iss` | 新增 `[Registry]`：`HKLM\Software\Microsoft\Windows\CurrentVersion\App Paths\ai-voice.exe` = `{app}\ai-voice.exe`（卸载时删除） |

### 10.3 工具链与产物（关键路径）

| 用途 | 路径 |
|------|------|
| fork 源码 | `K:\K3R-voice`（分支 `master` @ 0.17.4，**改动未提交**） |
| Boost | `K:\Libraries\boost-1.84.0`（`stage\lib` 44 libs） |
| 构建脚本 | `K:\Libraries\weasel_build4.bat`（Release 版，`RELEASE_BUILD=1`） |
| Weasel 产物 | `K:\K3R-voice\output\`（x64）+ `output\Win32\`（x86） |
| 自建 NSIS 包 | `K:\K3R-voice\output\archives\weasel-0.17.4.0-installer.exe`（12,058,129 B，SHA256 `7BC94E4B…E950`） |
| 官方包备份 | `K:\AI_Voice_rs\resources\rime\official\weasel-0.17.4.0-installer.exe.official`（SHA256 `CF509534…5C29`） |
| 已替换的资源包 | `K:\AI_Voice_rs\resources\rime\weasel-0.17.4.0-installer.exe` |
| 重编的 AI Voice 安装包 | `K:\AI_Voice_rs\dist\ai-voice-0.1.0-setup.exe`（397 MB，含 App Paths 注册） |
| spike 冒烟测试目录 | `K:\K3R-voice\spike-test\`（data 目录是指向已装小狼毫 data 的 junction） |
| NSIS 便携版 | `K:\Libraries\nsis-3.10\Bin\makensis.exe` |

### 10.4 已验证（自主执行期间）

1. **构建**：Weasel x64 + Win32 均 0 error；librime 含 lua/octagram/predict（日志 `-- rime_plugins_modules: lua;octagram;predict`）。
2. **冒烟测试**（旁路，不动 Program Files）：把产物拷到 `K:\K3R-voice\spike-test\` 直接跑 `WeaselServer.exe` → 进程存活、建立命名管道 `\\.\pipe\kevin-3080\WeaselNamedPipe`、加载全部 schema、`lua_translator/lua_filter/predictor/octagram` 全部注册成功、无 ERROR。
3. **唤起链路实测**：`ai-voice.exe --settings`（经 App Paths 解析）→ 常驻实例日志出现「收到唤起请求 → 显示设置窗口」。
4. **自建 NSIS 包内容核对**：与官方包逐项比对（WeaselServer/Setup/Deployer、weasel(x64).ime、data\essay.txt、data\opencc），功能等价；差异仅：无 ARM 文件（本机 x64 不需要）、二进制用 14.50 编译故体积略大。

### 10.5 待 kevin 处理

| # | 事项 | 命令/说明 |
|---|------|----------|
| 1 | **验收 A**：提权替换已装小狼毫的 exe（脚本已备） | 双击 `K:\Libraries\verify_spike.bat`（会弹 UAC）；失败可跑 `K:\Libraries\rollback_spike.bat` 还原 |
| 2 | **`/deploy` 卡住问题复核** | 官方版与自建版**同样卡住**（日志停在同一处、CPU 0），判断为环境问题（疑似无交互桌面/托盘通知阻塞）。请在正常使用状态下点一次「重新部署」确认 |
| 3 | ~~**git commit + GitHub 建仓（第 11 步）**~~ | ✅ 已完成：`aivoice` 分支单提交 **`09b5dcb`**（2026-09-24 amend 改名版，原 `748c62c`）；已 `--force-with-lease` 推送公开仓库 https://github.com/kevinx-key/K3R-voice（gh 账号 kevinx-key）；本地 `origin`=K3R-voice、`upstream`=rime/weasel |
| 4 | **App Paths 测试键** | 我在 `HKCU\Software\Microsoft\Windows\CurrentVersion\App Paths\ai-voice.exe` 写了一条指向 `K:\AI_Voice_rs\target\release\ai-voice.exe` 的测试键（**无管理员权限下的等效验证**）。若你之后用新安装包装了 AI Voice（会写 HKLM），**请删掉这条 HKCU 键**，否则 HKCU 优先级更高会一直指向开发版 |
| 5 | 回归清单 §6 C7/C8 | Shift 误触（`default.custom.yaml` 已加 `Shift_L/Shift_R: noop`）仍需重新部署后验证 |

### 10.6 首轮验收发现的问题与修复（2026-09-23 晚）

**现象**：验收 A 替换后托盘菜单/按钮都出现了，但点「AI Voice 设置」提示「未找到 AI Voice，请先安装。」

**定位**：写了个最小复现（`K:\Libraries\apppaths_test.cpp` → `apppaths_test.exe`）：
- **普通权限**下 `ShellExecuteW(NULL,"open","ai-voice.exe","--settings")` → 返回 42（成功），HKCU App Paths 读得到；
- 但 `verify_spike.bat` 是**提权**运行的，它 `start` 出来的 WeaselServer 继承了管理员令牌，
  **提权上下文里 ShellExecute 对裸文件名的 App Paths 解析不可靠** → 返回 SE_ERR_FNF → 弹提示。

**修复（两处）**：
1. `include/WeaselUtility.h` 的 `LaunchAIVoiceSettings()` 不再把裸文件名交给 ShellExecute：
   改为**自己查 App Paths 注册表**（HKLM/HKCU × 64/32 四个视图）拿到完整路径再 ShellExecute；
   再兜底裸文件名 + `%ProgramFiles%\AIVoice` / `%LOCALAPPDATA%\Programs\AIVoice`。
2. `verify_spike.bat` 第 4 步改用 **`explorer.exe` 代拉 WeaselServer**（原始用户身份），
   避免服务被提权——否则不仅定位不到 ai-voice，连 AI Voice 本体也会被提权拉起。

**注意**：若当前正跑着提权版 WeaselServer，重跑一次 `verify_spike.bat` 即可换成普通用户身份。

### 10.7 「输入法设定 没反应」与「识别后端未初始化」（同日继续）

**现象 A：托盘「输入法设定」点了没反应**
- 用 `wininfo.exe` 枚举进程窗口发现：对话框**其实早就开出来了**
  （`#32770` / 标题【小狼毫】方案选单设定 / visible=1 / rect=(709,469)-(1236,849)，1920×1080 屏内）
- 根因两条，都是**上一轮旧脚本的提权服务**造成：
  1. 提权进程的窗口**不能抢焦点** → 对话框开在当前窗口后面，看起来"没反应"；
  2. `WeaselDeployer` 有 `WeaselDeployerExclusiveMutex`，卡住的那个实例在，再点只会静默退出。
- 处理：手动结束提权版 `WeaselDeployer.exe` → 重跑已修好的 `verify_spike.bat`（`explorer.exe` 拉普通用户服务）。
- **修正 §10.4 的一条误判**：日志"停在半行"是 glog 缓冲写的假象，不代表卡死；
  之前 `/deploy` 观察到的"卡住"很可能只是**部署耗时长**（rime-ice + 万象语法），待重测。

**现象 B：AI Voice 报「识别失败: 识别后端未初始化」**
- 原因：我留下的 HKCU App Paths 测试键指向**裸编译产物** `K:\AI_Voice_rs\target\release\ai-voice.exe`，
  而 AI Voice 按「exe 同级」解析 `asr.local_model`（`models/sense-voice`），
  模型实际在仓库根的 `K:\AI_Voice_rs\models\sense-voice\`（你平时是带 `--config` 跑的）。
- 修复：建目录 junction `K:\AI_Voice_rs\target\release\models` → `K:\AI_Voice_rs\models`；
  重启后日志 `识别后端就绪: 本地 SenseVoice`，`ai-voice.exe --settings` 唤起链路复测通过。
- 提醒：`cargo clean` 或删 `target\` 会让 junction 消失，需要重建；
  正式做法还是用 `dist\ai-voice-0.1.0-setup.exe` 装到 `C:\Program Files\AIVoice`（config + models 都在 `{app}` 下）。

---

## 11. 改名（K星输入法，2026-09-24）

> 规格见 `K星输入法-改名交接.md` §0（grill 定稿）。**只改用户可见显示名**，内部标识一律不动。

### 11.1 改动清单（已完成）

| 侧 | 文件 | 改动 |
|----|------|------|
| fork | `include/WeaselUtility.h` | `get_weasel_ime_name()` → `K星输入法` / `KStar`（覆盖语言栏 / IME 列表 / 托盘 tooltip / 气泡 / TSF 描述 / 安装器 IME 注册名） |
| fork | 5 个 `.rc`（IME/TSF/Setup/Deployer/Server） | 设置窗标题、版本资源产品名/说明/公司名 K3R/版权 GPLv3；**修正首轮脚本的简繁倒置**（29 条定点替换，kevin 手动执行） |
| fork | `output/install.nsi` | 安装包标题 `K星输入法 0.17.4`、快捷方式、卸载列表、Publisher=K3R、版本资源（英文段 KStar） |
| fork | `README.md` / `output/README.txt` | 顶部加「本项目基于 rime/weasel 0.17.4（GPLv3）」 |
| fork | `WeaselSetup/WeaselSetup.cpp` | 帮助框标题 →【K星输入法】+ 英文 fallback KStar |
| fork | `WeaselServer/WeaselServerApp.h` | 更新检查提示框标题「AI Voice / K星输入法」 |
| AI Voice | `src/*.rs`（gui/main/rime/config）+ `installer/*` + `README.md` + `.gitignore` | 全部文案（小狼毫 → K星输入法）；内部检测逻辑不动 |

### 11.2 未改（有意为之）

- `WEASEL_CODE_NAME`（`include/WeaselConstants.h`）：喂 librime `distribution_code_name`（内部标识）+ CLSID 注册描述；IME 显示名实际由 `get_weasel_ime_name()` 覆盖 → 不动
- 注册表键 / exe 名 / dll 名 / 安装目录 / 服务名 / IPC / mutex / `InternalName` / `OriginalFilename`：全部不动
- 源码注释里的「小狼毫」/ 上游 CHANGELOG / 第三方方案包文档：不动（非 UI）

### 11.3 产物（2026-09-24）

| 产物 | SHA256 |
|------|--------|
| `weasel-0.17.4.0-installer.exe`（12,060,165 B） | `AA3947703AD43979990864494179553EB3F76CC188F723F467F9CDEEEFA6A8E8` |
| WeaselServer.exe | `0267E344052268596895D3A3EDF2BB53F0F564F50705AE236A27ED0CB134DA8B` |
| WeaselSetup.exe | `A91905928D7611E0BDF44F117AAB172822DC87DE8E1186163708D77D3618CD8F` |
| WeaselDeployer.exe | `2EB7A4FB10C9DE4E48BCF1F8084D5D6717D8501C36DE286CD2FD37F97C3E17CC` |
| `ai-voice-0.1.0-setup.exe`（416,407,983 B） | `65E336AE8408DCDA5298EB04DB6FDCFD1C332740C565144F1C687C4B57CE61D1` |

（已替换 `resources\rime\weasel-0.17.4.0-installer.exe`；改名前的自建包备份在 `resources\rime\official\weasel-0.17.4.0-installer.exe.fork-prerename`）

### 11.4 生效步骤（待 kevin）

1. 用新包 `K:\AI_Voice_rs\dist\ai-voice-0.1.0-setup.exe` **重装一次**（安装器注销旧 IME、注册新名字）
2. **重启一次**（释放被占用的 IME dll）
3. 验收：语言栏 / IME 列表显示「K星输入法」；三个设置窗标题【K星输入法】；托盘 tooltip；卸载列表；文件属性
4. 删掉 `HKCU\Software\Microsoft\Windows\CurrentVersion\App Paths\ai-voice.exe` 测试键（正式安装会写 HKLM）
5. 顺手回归：Shift 误触不再切英文（`default.custom.yaml` 已改，部署后验证）


