# Build and package Listen (Windows x64)
# Usage:  powershell -ExecutionPolicy Bypass -File installer\build.ps1
#         ... -WithModel          # bundle the 230MB ASR model into the zip
#         ... -WithRime           # bundle resources\rime (倾听输入法+方案包+语法模型) into the zip
#         ... -Setup              # build the Inno Setup installer (needs resources\rime)
#         ... -Setup -NoLM        # same but skip the 400MB 语法模型（安装包小很多）

param(
    [switch]$WithModel,
    [switch]$WithRime,
    [switch]$Setup,
    [switch]$NoLM
)

$ErrorActionPreference = "Stop"

$env:CARGO_HOME   = "K:\dev\.cargo"
$env:RUSTUP_HOME  = "K:\dev\.rustup"
$CARGO = "K:\dev\.cargo\bin\cargo.exe"

$root = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $root

Write-Host "==> cargo build --release" -ForegroundColor Cyan
& $CARGO build --release
if ($LASTEXITCODE -ne 0) { throw "build failed" }

$exe = Join-Path $root "target\release\listen.exe"
# GUI 子系统程序的 stdout 抓不到，版本号直接从 Cargo.toml 读
$verLine = Select-String -LiteralPath (Join-Path $root "Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
if (-not $verLine) { throw "cannot read version from Cargo.toml" }
$ver = $verLine.Matches[0].Groups[1].Value
$name = "listen-$ver-win-x64"
$dist = Join-Path $root "dist"
$stage = Join-Path $dist $name

Write-Host "==> staging $stage" -ForegroundColor Cyan
if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
New-Item -ItemType Directory -Force -Path $stage | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $stage "models\sense-voice") | Out-Null

Copy-Item -LiteralPath $exe -Destination $stage
Copy-Item -LiteralPath (Join-Path $root "README.md") -Destination $stage

# Ship a pristine default config (do not ship the dev one)
$cfgTpl = Join-Path $root "config.default.toml"
if (Test-Path -LiteralPath $cfgTpl) {
    Copy-Item -LiteralPath $cfgTpl -Destination (Join-Path $stage "config.toml")
}

# Model is NOT bundled by default (keeps the zip small; first run downloads it).
# Pass -WithModel to bundle it (for offline / USB distribution).
$modelDir = Join-Path $root "models\sense-voice"
$modelFiles = @("model.int8.onnx", "tokens.txt")
if ($WithModel) {
    foreach ($f in $modelFiles) {
        $src = Join-Path $modelDir $f
        if (-not (Test-Path -LiteralPath $src)) { throw "missing model file: $src" }
        Copy-Item -LiteralPath $src -Destination (Join-Path $stage "models\sense-voice")
    }
    Write-Host "    (model included)"
} else {
    Write-Host "    (model NOT included - first run downloads it)" -ForegroundColor Yellow
}

# ---- Rime 资源（二合一）----
$rimeRes = Join-Path $root "resources\rime"
if ($WithRime -or $Setup) {
    if (-not (Test-Path -LiteralPath (Join-Path $rimeRes "weasel-0.17.4.0-installer.exe"))) {
        throw "缺少 Rime 资源：先跑 installer\fetch_resources.ps1"
    }
}
if ($WithRime) {
    Write-Host "==> bundling resources\rime into zip" -ForegroundColor Cyan
    $dst = Join-Path $stage "resources\rime"
    New-Item -ItemType Directory -Force -Path $dst | Out-Null
    Copy-Item -LiteralPath (Join-Path $rimeRes "weasel-0.17.4.0-installer.exe") -Destination $dst
    Copy-Item -LiteralPath (Join-Path $rimeRes "schemes") -Destination $dst -Recurse
    if (-not $NoLM -and (Test-Path -LiteralPath (Join-Path $rimeRes "lm"))) {
        Copy-Item -LiteralPath (Join-Path $rimeRes "lm") -Destination $dst -Recurse
    }
}

# ---- Inno Setup 安装包 ----
if ($Setup) {
    $iscc = "K:\app\InnoSetup\ISCC.exe"
    if (-not (Test-Path -LiteralPath $iscc)) { throw "找不到 Inno Setup 编译器: $iscc" }
    Write-Host "==> building installer (Inno Setup)" -ForegroundColor Cyan
    $iss = Join-Path $root "installer\listen.iss"
    if ($NoLM) {
        & $iscc "/DIncludeLM=0" "/DAppVer=$ver" $iss
    } else {
        & $iscc "/DIncludeLM=1" "/DAppVer=$ver" $iss
    }
    if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }
    Get-ChildItem (Join-Path $dist "listen-*-setup.exe") |
        Sort-Object LastWriteTime | Select-Object -Last 1 |
        ForEach-Object { Write-Host ("setup: {0}  ({1} MB)" -f $_.FullName, [math]::Round($_.Length / 1MB, 1)) -ForegroundColor Green }
}

$zip = Join-Path $dist "$name.zip"
if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
Write-Host "==> zipping $zip" -ForegroundColor Cyan
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip -CompressionLevel Optimal

$exeMb  = [math]::Round((Get-Item -LiteralPath $exe).Length / 1MB, 1)
$zipMb  = [math]::Round((Get-Item -LiteralPath $zip).Length / 1MB, 1)
Write-Host ""
Write-Host "===== DONE =====" -ForegroundColor Green
Write-Host ("exe : {0}  ({1} MB)" -f $exe, $exeMb)
Write-Host ("zip : {0}  ({1} MB)" -f $zip, $zipMb)
Write-Host ("stage: {0}" -f $stage)
