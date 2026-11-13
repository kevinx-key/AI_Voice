# 安装「语言栏菜单 + 候选窗固定 7 列 + 单击直提」到系统实际加载的 TSF DLL
#
# 2026-10-30 定位：
#   输入法注册的 CLSID {A3F4CDED-B1E9-41EE-9CA6-7B4D0DE6CB0A} 指向
#     C:\Windows\System32\weasel.dll   (x64 TSF，64 位程序加载)
#     C:\Windows\SysWOW64\weasel.dll   (x86 TSF，32 位程序加载)
#   而不是 K:\app\AIVoice\weasel-0.17.4\weaselx64.dll（那只是安装目录里的源文件）。
#   之前只改了安装目录，实际加载的 System32 副本仍是 10-05 旧件，所以候选窗还是 9 个/行。
#
# 本脚本把新构建复制到**系统实际加载的位置**：
#   K:\K3R-voice\output\weaselx64.dll  ->  %WINDIR%\System32\weasel.dll
#   K:\K3R-voice\output\weasel.dll     ->  %WINDIR%\SysWOW64\weasel.dll
#   旧件先改名为 weasel.dll.bak-<时间>（运行中的 dll 允许改名）。
#
# 需要管理员（写 System32/SysWOW64）。不改注册表、不改 %APPDATA%\Rime。
# 生效需要**重启电脑**（旧 DLL 已被各程序加载；重启后新程序才用新件）。
#
# 用法（管理员 PowerShell）：
#   powershell -ExecutionPolicy Bypass -File tools\install_weasel_tsf.ps1
#
# 退出码：0 成功；1 失败（会打印原因，并尽量回滚）。

#Requires -RunAsAdministrator

param(
  [string]$Src = "K:\K3R-voice\output"
)

$ErrorActionPreference = "Stop"
$stamp = Get-Date -Format "yyyyMMdd-HHmmss"

$pairs = @(
  @{ Name = "x64"; Src = (Join-Path $Src "weaselx64.dll"); Dst = (Join-Path $env:WINDIR "System32\weasel.dll") },
  @{ Name = "x86"; Src = (Join-Path $Src "weasel.dll");     Dst = (Join-Path $env:WINDIR "SysWOW64\weasel.dll") }
)

function Fail($msg) {
  Write-Host "失败：$msg" -ForegroundColor Red
  exit 1
}

foreach ($p in $pairs) {
  if (-not (Test-Path $p.Src)) { Fail "缺少构建产物：$($p.Src)" }
}

foreach ($p in $pairs) {
  $hNew = (Get-FileHash $p.Src -Algorithm SHA256).Hash
  if (-not (Test-Path $p.Dst)) { Fail "找不到系统 DLL：$($p.Dst)" }
  $hOld = (Get-FileHash $p.Dst -Algorithm SHA256).Hash
  if ($hNew -eq $hOld) {
    Write-Host "[$($p.Name)] 已是新件（$($hNew.Substring(0,16))…），跳过。"
    continue
  }

  Write-Host "[$($p.Name)] 旧件 $($hOld.Substring(0,16))… -> 新件 $($hNew.Substring(0,16))…"
  $bak = "$($p.Dst).bak-$stamp"
  try {
    Rename-Item -LiteralPath $p.Dst -NewName (Split-Path $bak -Leaf) -ErrorAction Stop
  } catch {
    Fail "无法重命名 $($p.Dst)（$($_.Exception.Message)）。请重启后立即重跑本脚本。"
  }
  try {
    Copy-Item -LiteralPath $p.Src -Destination $p.Dst -Force -ErrorAction Stop
  } catch {
    Write-Host "  复制失败，回滚…" -ForegroundColor Yellow
    Remove-Item -LiteralPath $p.Dst -Force -ErrorAction SilentlyContinue
    Rename-Item -LiteralPath $bak -NewName (Split-Path $p.Dst -Leaf) -ErrorAction SilentlyContinue
    Fail $_.Exception.Message
  }
  $hAfter = (Get-FileHash $p.Dst -Algorithm SHA256).Hash
  if ($hAfter -ne $hNew) { Fail "写入后 hash 不一致：$($p.Dst)" }
  Write-Host "  已写入 $($p.Dst)（备份 $(Split-Path $bak -Leaf)）"
}

Write-Host ""
Write-Host "完成。请**重启电脑**后验证：" -ForegroundColor Green
Write-Host "  1) 输入拼音 → PageDown 展开 → 应严格 5 行 × 7 列、每行 1~7；"
Write-Host "  2) 按 3 选当前行第 3 个；←/→ 同行走、↑/↓ 跨行；单击第 20 个立即上屏。"
Write-Host ""
Write-Host "回滚：管理员 PowerShell 下，把 System32/SysWOW64 的 weasel.dll.bak-$stamp 改回 weasel.dll，再重启。"
