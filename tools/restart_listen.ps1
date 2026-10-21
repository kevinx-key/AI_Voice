# 优雅重启装机版 listen（避免托盘"幽灵图标"）
#
# 为什么需要它（2026-10-21 排查结论）：
#   TrayIcon 只在**正常退出**（Drop）时才调用 Shell_NotifyIcon(NIM_DELETE)。
#   `Stop-Process` / 任务管理器结束 / 崩溃自愈 都会绕过它 → Explorer 里留下一个灰色圆点"幽灵图标"，
#   直到你点开托盘或悬停才被清掉。装机时反复 Stop-Process 会攒出"一托盘 listen"。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File tools\restart_listen.ps1            # 只重启
#   powershell -ExecutionPolicy Bypass -File tools\restart_listen.ps1 -Install   # 先装 target\release 再重启
param(
  [switch]$Install,
  [string]$Exe = "K:\app\AIVoice\listen.exe",
  [string]$Build = "K:\AI_Voice_rs\target\release\listen.exe"
)

$ErrorActionPreference = "Stop"

# 1) 优雅退出（走 ListenCtl 管道 → 主程序正常收尾 → TrayIcon Drop → 删图标）
$quit = "K:\AI_Voice_rs\tools\ctl_client.py"
if (Test-Path $quit) {
  try { & python $quit QUIT 2>&1 | Out-Null } catch { }
}
for ($i = 0; $i -lt 40; $i++) {
  if (-not (Get-Process listen -ErrorAction SilentlyContinue)) { break }
  Start-Sleep -Milliseconds 250
}
# 2) 还活着才强杀（这时已尽力避免幽灵图标）
$alive = Get-Process listen -ErrorAction SilentlyContinue
if ($alive) {
  Write-Host "!! 优雅退出超时，强制结束（会留一个幽灵图标）" -ForegroundColor Yellow
  $alive | Stop-Process -Force
  Start-Sleep -Milliseconds 800
}

if ($Install) {
  if (-not (Test-Path $Build)) { throw "找不到构建产物: $Build" }
  Copy-Item $Build $Exe -Force
  $sha = (Get-FileHash $Exe -Algorithm SHA256).Hash.Substring(0, 8).ToLower()
  Write-Host "已安装 $Exe (sha=$sha)"
}

Start-Sleep -Milliseconds 300
Start-Process -FilePath $Exe -ArgumentList '--managed' | Out-Null
Start-Sleep -Seconds 8
& python $quit STATUS 2>&1 | Select-Object -Last 1
