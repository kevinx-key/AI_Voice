# 安装「语言栏右键菜单」源文件到安装目录（Weasel fork 的 TSF DLL）
#
# 这一版 DLL（2026-10-26）包含：
#   右键语言栏在设置窗已开/前台不是 TSF 窗口时也能弹出菜单（owner 兜底 + SetForegroundWindow/WM_NULL）。
#
# ⚠️ 2026-10-30 更正：本脚本只更新 `K:\app\AIVoice\weasel-0.17.4` 里的**源文件**；
#   输入法注册的 CLSID 实际加载的是：
#     C:\Windows\System32\weasel.dll（x64）
#     C:\Windows\SysWOW64\weasel.dll（x86）
#   要真正生效，请用管理员运行 tools\install_weasel_tsf.ps1（会备份系统 DLL 并替换），再重启。
#   只跑本脚本不会改变任何已安装程序的行为。
#
# 只替换两个 TSF DLL（x64 / x86），不动 WeaselServer、不改注册表、不改 %APPDATA%\Rime。
# 已加载旧 DLL 的程序不会立刻切换：**重启资源管理器**（或重启电脑）后新开的程序才用新 DLL。
#
# 用法（普通 PowerShell，不需要管理员；K:\app 可写）：
#   powershell -ExecutionPolicy Bypass -File tools\install_weasel_menu_fix.ps1
#   powershell -ExecutionPolicy Bypass -File tools\install_weasel_menu_fix.ps1 -Src K:\K3R-voice\output -Dst K:\app\AIVoice\weasel-0.17.4
#
# 退出码：0 成功；1 失败（会打印原因；常见原因=文件被占用，重启后重跑即可）。

param(
  [string]$Src = "K:\K3R-voice\output",
  [string]$Dst = "K:\app\AIVoice\weasel-0.17.4"
)

$ErrorActionPreference = "Stop"

function Fail($msg) {
  Write-Host "失败：$msg" -ForegroundColor Red
  exit 1
}

if (-not (Test-Path $Dst)) { Fail "找不到安装目录：$Dst" }
if (-not (Test-Path (Join-Path $Dst "weaselx64.dll"))) { Fail "目标目录不像倾听输入法：$Dst" }

$files = @("weaselx64.dll", "weasel.dll")
foreach ($f in $files) {
  $s = Join-Path $Src $f
  if (-not (Test-Path $s)) { Fail "缺少构建产物：$s" }
}

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$done = @()

foreach ($f in $files) {
  $s = Join-Path $Src $f
  $d = Join-Path $Dst $f
  if (Test-Path $d) {
    $bak = "$d.bak-$stamp"
    try {
      Rename-Item -LiteralPath $d -NewName (Split-Path $bak -Leaf) -ErrorAction Stop
      Write-Host "已备份 $f -> $(Split-Path $bak -Leaf)"
    } catch {
      Fail "$f 正在被占用，无法改名（$($_.Exception.Message)）。`n请重启电脑后重跑本脚本；或先关闭所有正在使用输入法的程序。"
    }
  }
  Copy-Item -LiteralPath $s -Destination $d -Force
  $h = (Get-FileHash $d -Algorithm SHA256).Hash.Substring(0, 16)
  Write-Host "已安装 $f (sha256=$h…)"
  $done += $f
}

Write-Host ""
Write-Host "完成：$($done -join ', ')"
Write-Host "生效方式：重启资源管理器，或直接重启电脑（已运行的程序仍用旧 DLL）。"
Write-Host "验证：打开设置窗 → 右键语言栏「中/英」→ 应能看到菜单；再点「设置」→ 旧窗先关、新窗重开。"
