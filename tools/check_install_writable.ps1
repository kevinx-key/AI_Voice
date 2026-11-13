# 检查「倾听输入法」安装目录是否可写（只读诊断，不改任何配置）
#
# 用途：新机器上出现「没有日志 / 设置不保存 / 小圆点反复出现」时，先跑这个。
# 为什么：程序把 config.toml / logs / hotwords / models / voice_off.flag 放在 exe 同级；
#         装到 C:\Program Files\Listen 且非提权运行时写不进去（见 项目调试过程.md §56）。
#
# 用法（在出问题的机器上，普通 PowerShell 即可，不需要管理员）：
#   powershell -ExecutionPolicy Bypass -File check_install_writable.ps1
#   powershell -ExecutionPolicy Bypass -File check_install_writable.ps1 -Dir "D:\Listen"
#
# 风险：只在目标目录建一个 .listen_write_test_<pid> 探针文件并立刻删除；不读隐私数据、不改配置。
# 退出码：0 = 检查完成（无论可写与否，结果看输出）；2 = 找不到安装目录且未指定 -Dir。

param(
    [string]$Dir = ""
)

$ErrorActionPreference = "Continue"

if (-not $Dir) {
    $appPathKey = "HKLM:\Software\Microsoft\Windows\CurrentVersion\App Paths\listen.exe"
    try {
        $exe = (Get-ItemProperty -Path $appPathKey -ErrorAction Stop).'(default)'
        if ($exe) { $Dir = Split-Path -Parent $exe }
    } catch {}
    if (-not $Dir) {
        $candidates = @(
            (Join-Path $env:ProgramFiles "Listen"),
            (Join-Path $env:LOCALAPPDATA "Programs\Listen")
        )
        foreach ($c in $candidates) {
            if (Test-Path (Join-Path $c "listen.exe")) { $Dir = $c; break }
        }
    }
}

if (-not $Dir -or -not (Test-Path $Dir)) {
    Write-Host "找不到安装目录。请用 -Dir 指定（例如：-Dir 'C:\Program Files\Listen'）"
    exit 2
}

Write-Host "== 安装目录 =="
Write-Host $Dir

Write-Host "`n== 目录 ACL（看 BUILTIN\Users 是否有 (M) 或 (F)）=="
icacls $Dir

Write-Host "`n== 写探针 =="
$probe = Join-Path $Dir (".listen_write_test_" + $PID)
try {
    Set-Content -Path $probe -Value "1" -Encoding ASCII -ErrorAction Stop
    Remove-Item $probe -Force -ErrorAction SilentlyContinue
    Write-Host "WRITABLE  —— 安装目录可写，Program Files 权限问题不成立"
} catch {
    Write-Host "NOT WRITABLE  —— 安装目录只读，这就是「无日志/设置不保存/圆点反复出现」的高概率原因"
    Write-Host ("  原因: " + $_.Exception.Message)
}

Write-Host "`n== 关键文件是否存在 =="
foreach ($f in @("listen.exe", "config.toml", "voice_off.flag")) {
    $p = Join-Path $Dir $f
    Write-Host ("  {0,-16} {1}" -f $f, (Test-Path $p))
}

Write-Host "`n== 环境 =="
Write-Host ("  Windows: " + [System.Environment]::OSVersion.VersionString)
Write-Host ("  Session: " + (Get-Process -Id $PID).SessionId)
try {
    $gpu = (Get-CimInstance Win32_VideoController -ErrorAction Stop | Select-Object -ExpandProperty Name) -join "; "
    Write-Host ("  显卡:    " + $gpu)
} catch {}
try {
    $lua = (Get-ItemProperty "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System" -Name EnableLUA -ErrorAction Stop).EnableLUA
    Write-Host ("  UAC:     EnableLUA=" + $lua)
} catch {}

Write-Host "`n== 如果 NOT WRITABLE =="
Write-Host "  A) 重新安装到可写目录（如 D:\Listen），或"
Write-Host "  B) 管理员执行：icacls `"$Dir`" /grant *S-1-5-32-545:(OI)(CI)M"
Write-Host "  C) 新版程序会自动把数据目录回退到 %LOCALAPPDATA%\Listen；用 listen.exe --status 看实际路径"
