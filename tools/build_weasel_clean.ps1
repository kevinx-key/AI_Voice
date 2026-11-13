# 构建 Weasel fork（干净环境版）
#
# 为什么需要它（2026-10-26 实测）：
#   本机/DSH 环境里同时存在 NO_PROXY 与 no_proxy（大小写两份）。MSBuild 18 的 ToolTask
#   把环境变量复制给 CL.exe 时用 Hashtable，同键不同大小写会抛
#   `MSB6001: “CL.exe”的命令行开关无效。System.ArgumentException: 已添加项。`
#   本脚本用 .NET 建一个"去重 + 剔除 proxy"的环境再跑 vcvarsall + build.bat。
#
# 用法（普通 PowerShell；不需要管理员）：
#   powershell -ExecutionPolicy Bypass -File tools\build_weasel_clean.ps1
#   powershell -ExecutionPolicy Bypass -File tools\build_weasel_clean.ps1 -Target weasel
#
# 产物：K:\K3R-voice\output\weaselx64.dll / weasel.dll / WeaselServer.exe ...
# 只构建，不安装、不改输入法、不动 %APPDATA%\Rime。
# 退出码：构建成功 0；失败非 0（输出末尾有 EXIT=）。

param(
  [string]$Target = "weasel",
  [string]$Repo = "K:\K3R-voice",
  [string]$VcVars = "C:\Program Files\Microsoft Visual Studio\18\Community\VC\Auxiliary\Build\vcvarsall.bat"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $VcVars)) { throw "找不到 vcvarsall.bat: $VcVars" }
if (-not (Test-Path (Join-Path $Repo "build.bat"))) { throw "找不到 $Repo\build.bat" }

# cmd /c 的引号：/c ""C:\path with space\vcvarsall.bat" x64 && ..."
$cmdline = '/c ""' + $VcVars + '" x64 >nul && cd /d ' + $Repo +
           ' && set RELEASE_BUILD=1 && set CMAKE_POLICY_VERSION_MINIMUM=3.5 && call build.bat ' + $Target + '"'

$psi = [System.Diagnostics.ProcessStartInfo]::new()
$psi.FileName = $env:ComSpec
$psi.Arguments = $cmdline
$psi.UseShellExecute = $false
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$psi.Environment.Clear()

# GetEnvironmentVariables() 返回大小写不敏感字典 → 天然去重；再剔除 proxy（本机构建不需要）
$envs = [System.Environment]::GetEnvironmentVariables()
foreach ($k in $envs.Keys) {
  if ([string]$k -match '(?i)proxy') { continue }
  $psi.Environment[[string]$k] = [string]$envs[$k]
}
foreach ($need in @('ComSpec', 'SystemRoot', 'windir', 'TEMP', 'TMP', 'PATH', 'PATHEXT')) {
  if (-not $psi.Environment.ContainsKey($need)) {
    $v = [System.Environment]::GetEnvironmentVariable($need)
    if ($v) { $psi.Environment[$need] = $v }
  }
}

$p = [System.Diagnostics.Process]::Start($psi)
$out = $p.StandardOutput.ReadToEnd()
$err = $p.StandardError.ReadToEnd()
$p.WaitForExit()

$out
if ($err) { "--- stderr ---"; $err }
"EXIT=$($p.ExitCode)"
if ($p.ExitCode -ne 0) { exit $p.ExitCode }
