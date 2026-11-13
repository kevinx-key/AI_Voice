# ai_baseline.ps1 - NON-DESTRUCTIVE baseline for any AI/agent taking over this repo.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File tools\ai_baseline.ps1
#   powershell -ExecutionPolicy Bypass -File tools\ai_baseline.ps1 -SkipTest
#
# What it does: prints git/toolchain/test/artifact/runtime/Rime/log facts in one
# screen so a new session does not re-invent build & verification steps.
#
# Side effects: NONE on source/config. It runs short-lived `listen.exe --status`
# / `--rime-status` child processes (the resident app is never started/stopped).
# Like any listen startup, those sync the Rime typing table
# (%APPDATA%\Rime\listen_hotwords.txt) and append a line to
# <exe>\logs\app.log.<UTC date>. It does NOT touch the GUI.
#
# Exit code: 0 = tests passed (or skipped) and build prerequisites present; 1 = otherwise.
# A missing build artifact is reported but does not fail the baseline.
param(
    [switch]$SkipTest
)

$ErrorActionPreference = "Continue"
$root = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $root

function Section([string]$name) {
    Write-Host ""
    Write-Host ("=== " + $name + " ===") -ForegroundColor Cyan
}

function Try-Exe([string]$path) {
    if (Test-Path -LiteralPath $path) { return (Resolve-Path -LiteralPath $path).Path }
    return $null
}

# listen is a GUI-subsystem exe: `& exe args` can return before output is flushed.
# Always wait + redirect when capturing CLI output.
function Invoke-ExeCapture {
    param([string]$Exe, [string[]]$Arguments)
    $tag = [guid]::NewGuid().ToString('N')
    $so = Join-Path $env:TEMP ("ai_baseline_" + $tag + ".out")
    $se = Join-Path $env:TEMP ("ai_baseline_" + $tag + ".err")
    try {
        $p = Start-Process -FilePath $Exe -ArgumentList $Arguments -Wait -NoNewWindow -PassThru -RedirectStandardOutput $so -RedirectStandardError $se
        $out = ""
        $err = ""
        if (Test-Path -LiteralPath $so) { $out = Get-Content -LiteralPath $so -Raw -Encoding utf8 }
        if (Test-Path -LiteralPath $se) { $err = Get-Content -LiteralPath $se -Raw -Encoding utf8 }
        return [pscustomobject]@{ Out = $out; Err = $err; Code = $p.ExitCode }
    } finally {
        Remove-Item -LiteralPath $so, $se -Force -ErrorAction SilentlyContinue
    }
}

$fail = @()

Section "1. time / git"
Write-Host ("time        : " + (Get-Date -Format "yyyy-MM-dd HH:mm:ss zzz"))
Write-Host ("repo        : " + $root)
$branch = (& git branch --show-current 2>$null)
$head = (& git log -1 --format="%h %s" 2>$null)
$dirty = (& git status --porcelain 2>$null)
Write-Host ("branch      : " + $branch)
Write-Host ("head        : " + $head)
if ([string]::IsNullOrWhiteSpace(($dirty | Out-String).Trim())) {
    Write-Host "worktree    : clean"
} else {
    Write-Host "worktree    : DIRTY" -ForegroundColor Yellow
    $dirty | Select-Object -First 15 | ForEach-Object { Write-Host ("  " + $_) }
}

Section "2. toolchain"
$cargoExe = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargoExe -and (Test-Path -LiteralPath "K:\dev\.cargo\bin\cargo.exe")) {
    $cargoExe = "K:\dev\.cargo\bin\cargo.exe"
}
if ($cargoExe) {
    Write-Host ("cargo       : " + (& $cargoExe --version 2>&1) + "  [" + $cargoExe + "]")
} else {
    Write-Host "cargo       : NOT FOUND" -ForegroundColor Red
    $fail += "cargo not found"
}
Write-Host ("rustc       : " + (& rustc --version 2>&1))
Write-Host ("CARGO_HOME  : " + $env:CARGO_HOME)
Write-Host ("RUSTUP_HOME : " + $env:RUSTUP_HOME)
Write-Host ("python      : " + (& python --version 2>&1))

Section "3. cargo test"
$testCode = 0
$testLine = "(skipped)"
if ($SkipTest) {
    Write-Host "result      : skipped (-SkipTest)"
} elseif (-not $cargoExe) {
    Write-Host "result      : skipped (cargo not found)" -ForegroundColor Red
    $testCode = 1
} else {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $out = & $cargoExe test 2>&1
    $testCode = $LASTEXITCODE
    $sw.Stop()
    $testLine = ($out | Select-String -Pattern "test result:" | Select-Object -Last 1)
    if ($null -eq $testLine) { $testLine = ($out | Select-Object -Last 1) }
    Write-Host ("result      : " + $testLine)
    Write-Host ("exit        : " + $testCode + "  (" + [math]::Round($sw.Elapsed.TotalSeconds, 1) + "s)")
    if ($testCode -ne 0) {
        Write-Host "last errors:" -ForegroundColor Red
        $out | Select-String -Pattern "^error|panicked|FAILED" | Select-Object -First 10 | ForEach-Object { Write-Host ("  " + $_) }
        $fail += "cargo test failed"
    }
}

Section "4. build artifacts"
$devExe = "target\release\listen.exe"
$instExe = "K:\app\AIVoice\listen.exe"
$artifactMissing = $false
foreach ($p in @($devExe, $instExe)) {
    if (Test-Path -LiteralPath $p) {
        $f = Get-Item -LiteralPath $p
        $h = (Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash
        Write-Host ("{0,-34} {1,12} B  {2}  sha={3}" -f $p, $f.Length, $f.LastWriteTime.ToString("yyyy-MM-dd HH:mm:ss"), $h.Substring(0, 12))
    } else {
        Write-Host ("{0,-34} MISSING" -f $p) -ForegroundColor Yellow
        if ($p -eq $devExe) { $artifactMissing = $true }
    }
}
$relCfg = "target\release\config.toml"
if (Test-Path -LiteralPath $relCfg) {
    Write-Host ("note        : " + $relCfg + " is the config used by the dev exe (NOT repo-root config.toml)")
}
if (Test-Path -LiteralPath "target\release\models") {
    $j = Get-Item -LiteralPath "target\release\models" -Force
    Write-Host ("models link : " + $j.LinkType + " -> " + $j.Target)
} else {
    Write-Host "models link : MISSING (recreate junction target\release\models -> models)" -ForegroundColor Yellow
}

Section "4b. build prerequisites"
# These files are compiled in via include_str! but the whole resources/ dir is
# gitignored, so a fresh clone fails to build until they are restored.
$required = @(
    "resources\hotwords\keyboard.tsv",
    "resources\hotwords\vibecoding.tsv",
    "resources\rime\lua\listen_learn.lua"
)
foreach ($r in $required) {
    if (Test-Path -LiteralPath $r) {
        Write-Host ("ok          : " + $r)
    } else {
        Write-Host ("MISSING     : " + $r + "   (include_str! -> compile will fail)") -ForegroundColor Red
        $fail += ("missing " + $r)
    }
}
$sherpaLib = ".cache\sherpa-onnx-v1.13.8-win-x64-static-MT-Release-no-tts-lib\lib"
if (Test-Path -LiteralPath $sherpaLib) {
    Write-Host ("ok          : " + $sherpaLib)
} else {
    Write-Host ("MISSING     : " + $sherpaLib + "   (link will fail; see README build env)") -ForegroundColor Red
    $fail += "missing sherpa-onnx static lib"
}

Section "5. runtime status (listen --status --json)"
# Prefer the installed exe: it reads the real config and (when an instance runs)
# answers through the ListenCtl pipe. Fall back to the dev build.
$statusExe = Try-Exe $instExe
if (-not $statusExe) { $statusExe = Try-Exe $devExe }
if ($statusExe) {
    Write-Host ("using       : " + $statusExe)
    $res = Invoke-ExeCapture $statusExe @('--status', '--json')
    $raw = $res.Out
    if ([string]::IsNullOrWhiteSpace($raw)) {
        Write-Host ("no output (exit " + $res.Code + ")") -ForegroundColor Yellow
        if ($res.Err) { Write-Host $res.Err }
    } else {
        try {
            $o = $raw | ConvertFrom-Json
            if ($null -ne $o.running -and -not $o.running) {
                # Static probe schema (listen not running): no healthy/levels/mic.
                Write-Host "running     : false (static probe; no live chain)"
                Write-Host ("voice       : " + $o.voice_enabled + "   gate=" + $o.gate_engine)
                Write-Host ("asr         : " + $o.asr_backend + " / " + $o.asr_model + "  ready=" + $o.asr_model_ready)
                Write-Host ("polish      : " + $o.polish)
                $procs = @(Get-Process listen -ErrorAction SilentlyContinue)
                Write-Host ("process     : " + $procs.Count + " listen.exe")
            } else {
                Write-Host ("healthy     : " + $o.healthy)
                Write-Host ("state       : " + $o.state + "   managed=" + $o.managed + "   voice=" + $o.voice_enabled)
                Write-Host ("gate        : cfg=" + $o.gate_cfg + " used=" + $o.gate)
                Write-Host ("mic         : opened=" + $o.mic.opened + " flowing=" + $o.mic.flowing + " rms=" + $o.mic.rms + " silent=" + $o.mic.silent)
                Write-Host ("asr         : " + $o.asr.phase + "  " + $o.asr.backend)
                Write-Host ("polish      : tier=" + $o.polish.tier + " llm=" + $o.polish.llm)
                $procs = @(Get-Process listen -ErrorAction SilentlyContinue)
                Write-Host ("process     : " + $procs.Count + " listen.exe")
                foreach ($p in $procs) {
                    Write-Host ("  pid=" + $p.Id + "  path=" + $p.Path + "  ws=" + [math]::Round($p.WorkingSet64 / 1MB) + " MB")
                }
            }
        } catch {
            Write-Host "could not parse JSON; raw output:" -ForegroundColor Yellow
            Write-Host $raw
        }
    }
} else {
    Write-Host "skipped: exe missing"
}

Section "6. Rime status (listen --rime-status)"
if ($statusExe) {
    $r = Invoke-ExeCapture $statusExe @('--rime-status')
    if ([string]::IsNullOrWhiteSpace($r.Out)) {
        Write-Host ("no output (exit " + $r.Code + ")") -ForegroundColor Yellow
        if ($r.Err) { Write-Host $r.Err }
    } else {
        ($r.Out -split "`r?`n") | Select-Object -First 12 | ForEach-Object { Write-Host $_ }
    }
} else {
    Write-Host "skipped: exe missing"
}

Section "7. logs (newest app.log.*)"
$logDirs = @("K:\app\AIVoice\logs", (Join-Path $root "logs"))
foreach ($d in $logDirs) {
    if (-not (Test-Path -LiteralPath $d)) { continue }
    $latest = Get-ChildItem -LiteralPath $d -Filter "app.log.*" -File -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if ($null -eq $latest) { continue }
    Write-Host ("file        : " + $latest.FullName + "  (" + $latest.Length + " B, " + $latest.LastWriteTime.ToString("yyyy-MM-dd HH:mm:ss") + ")")
    $tail = @(Get-Content -LiteralPath $latest.FullName -Tail 200 -Encoding utf8 -ErrorAction SilentlyContinue)
    if ($tail.Count -gt 0) {
        Write-Host ("  tail      : " + $tail[$tail.Count - 1].Trim())
        $warn = @($tail | Where-Object { $_ -match "WARN|ERROR" })
        Write-Host ("  warn/err  : " + $warn.Count + " in last 200 lines")
        foreach ($w in ($warn | Select-Object -Last 2)) { Write-Host ("  " + $w.Trim()) }
    } else {
        Write-Host "  (empty)"
    }
}
$panic = "K:\app\AIVoice\logs\last_panic.txt"
if (Test-Path -LiteralPath $panic) {
    Write-Host ("last_panic  : " + $panic + "  (" + (Get-Item -LiteralPath $panic).LastWriteTime + ")") -ForegroundColor Yellow
} else {
    Write-Host "last_panic  : none"
}

Section "8. summary"
if ($testCode -eq 0 -and -not $SkipTest) { Write-Host "tests       : PASS" -ForegroundColor Green } elseif ($SkipTest) { Write-Host "tests       : SKIPPED" -ForegroundColor Yellow } else { Write-Host "tests       : FAIL" -ForegroundColor Red }
if ($artifactMissing) { Write-Host "artifact    : MISSING (run: cargo build --release)" -ForegroundColor Yellow } else { Write-Host "artifact    : PRESENT" -ForegroundColor Green }
if ($fail.Count -eq 0) {
    Write-Host "baseline    : OK (non-destructive; no config/source changes)" -ForegroundColor Green
    exit 0
} else {
    Write-Host ("baseline    : ATTENTION - " + ($fail -join "; ")) -ForegroundColor Red
    exit 1
}
