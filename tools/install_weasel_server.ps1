# 安装「候选窗固定 7 列 + 单击直提」到 WeaselServer.exe（服务端 UI 路径）
#
# ⚠️ 2026-10-30 更正：TSF 模式下候选窗由系统 TSF DLL 绘制（`C:\Windows\System32\weasel.dll`），
#   不是 WeaselServer。本脚本只覆盖 IME/服务端 UI 路径；TSF 用户请用 `install_weasel_tsf.ps1`。
#
# 2026-10-28 修复安装方式：
#   旧版脚本先 `WeaselServer.exe /q` 再覆盖，但 `/q` 在本机会**永久卡住**（Start-Process -Wait 不返回），
#   导致文件根本没换成、重启后还是旧版。
#   新方式：**先改名（运行中的 exe 允许改名）→ 再写新件 → 最后停旧进程/起新进程**。
#   这样即使停进程失败，文件也已经就位，重启即生效；不会出现“卡在停止、什么都没装”。
#
# 不需要管理员（K:\app 可写）；不改注册表、不改 %APPDATA%\Rime。
# 用法：
#   powershell -ExecutionPolicy Bypass -File tools\install_weasel_server.ps1
#   powershell -ExecutionPolicy Bypass -File tools\install_weasel_server.ps1 -NoStart
#
# 退出码：0 成功；1 失败（会打印原因与回滚结果）。

param(
  [string]$Src = "K:\K3R-voice\output",
  [string]$Dst = "K:\app\AIVoice\weasel-0.17.4",
  [switch]$NoStart
)

$ErrorActionPreference = "Stop"
$name = "WeaselServer.exe"
$new = Join-Path $Src $name
$old = Join-Path $Dst $name

function Fail($msg) {
  Write-Host "失败：$msg" -ForegroundColor Red
  exit 1
}

if (-not (Test-Path $new)) { Fail "找不到新构建：$new" }
if (-not (Test-Path $old)) { Fail "找不到装机文件：$old" }

$hNew = (Get-FileHash $new -Algorithm SHA256).Hash
$hOld = (Get-FileHash $old -Algorithm SHA256).Hash
if ($hNew -eq $hOld) {
  Write-Host "装机版已经是这个构建（sha256=$($hNew.Substring(0,16))…），无需替换。"
  exit 0
}

Write-Host "新构建: $hNew"
Write-Host "旧装机: $hOld"

$oldPids = @(Get-Process WeaselServer -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id)
$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$bak = "$old.bak-$stamp"

# 1) 先把运行中的旧件改名（Windows 允许重命名已加载的 exe；进程继续从旧文件运行）
Write-Host "备份旧件（改名，不阻塞）…"
try {
  Rename-Item -LiteralPath $old -NewName (Split-Path $bak -Leaf) -ErrorAction Stop
} catch {
  Fail "无法重命名旧件（$($_.Exception.Message)）。请重启后立即重跑；或手动结束 WeaselServer 后重试。"
}
Write-Host "  -> $(Split-Path $bak -Leaf)"

# 2) 写入新件（此时原文件名已空出；即使旧进程还在跑也不影响）
try {
  Copy-Item -LiteralPath $new -Destination $old -Force -ErrorAction Stop
} catch {
  Write-Host "复制失败，回滚旧件…" -ForegroundColor Yellow
  Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue
  Rename-Item -LiteralPath $bak -NewName $name -ErrorAction SilentlyContinue
  Fail $_.Exception.Message
}
$hAfter = (Get-FileHash $old -Algorithm SHA256).Hash
if ($hAfter -ne $hNew) {
  Fail "写入后 hash 不一致（$($hAfter.Substring(0,16))… ≠ $($hNew.Substring(0,16))…）"
}
Write-Host "已写入 $name（sha256=$($hAfter.Substring(0,16))…）"

# 3) 停掉旧实例（它还在从 .bak 文件运行）
if ($oldPids.Count -gt 0) {
  Write-Host "停止旧实例（pid=$($oldPids -join ',')）…"
  try { Start-Process -FilePath $old -ArgumentList "/q" | Out-Null } catch {}
  $deadline = (Get-Date).AddSeconds(3)
  while ((Get-Date) -lt $deadline) {
    $alive = @($oldPids | Where-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue })
    if ($alive.Count -eq 0) { break }
    Start-Sleep -Milliseconds 150
  }
  $alive = @($oldPids | Where-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue })
  if ($alive.Count -gt 0) {
    Write-Host "  优雅退出超时 → 强制结束（输入法会短暂中断）" -ForegroundColor Yellow
    # /q 自身也可能是同名的 WeaselServer 进程，一并清掉；文件已是新版，重新启动即可
    Get-Process WeaselServer -ErrorAction SilentlyContinue | ForEach-Object {
      Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Milliseconds 600
  }
}

# 4) 起新实例（若 TSF 已经自动拉起，会自然复用/退出）
if (-not $NoStart) {
  Write-Host "启动 WeaselServer…"
  try { Start-Process -FilePath $old | Out-Null } catch {}
  Start-Sleep -Seconds 2
  $p = Get-Process WeaselServer -ErrorAction SilentlyContinue
  if ($p) {
    $pids = @($p | ForEach-Object { $_.Id }) -join ','
    Write-Host "已在运行（pid=$pids）。"
  } else {
    Write-Host "警告：未检测到 WeaselServer。运行 start_service.bat 或重启电脑。" -ForegroundColor Yellow
  }
}

Write-Host ""
Write-Host "完成（文件已是新版；旧件保留为 $(Split-Path $bak -Leaf)）。"
Write-Host "验证：输入拼音 → PageDown 展开 → 应严格 5 行 × 7 列、每行 1~7。"
Write-Host "如果仍是 9 个/行：确认 Get-Process WeaselServer 的 Path 是 $old，且文件 hash 为 $($hNew.Substring(0,16))…；必要时重启电脑。"
