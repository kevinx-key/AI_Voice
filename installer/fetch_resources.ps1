# fetch_resources.ps1 — 取齐 Rime 二合一所需资源到 resources\（国内镜像优先，本地优先）
#
# 用法：
#   .\fetch_resources.ps1                 # 取全部（已存在的跳过）
#   .\fetch_resources.ps1 -Probe          # 只探测镜像连通性，不下载
#   .\fetch_resources.ps1 -Force          # 已存在也重下
#   .\fetch_resources.ps1 -Only weasel    # 只取某一项（weasel/rime-ice/easy_en/jaroomaji/lm/silero-vad）
#   .\fetch_resources.ps1 -LocalDir D:\rime-res  # 本地目录/U 盘优先
param(
  [string]$Dest = "K:\AI_Voice_rs\resources",
  [string]$LocalDir = "",
  [switch]$Probe,
  [switch]$Force,
  [string]$Only = ""
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

# ---------------------------------------------------------------- 镜像
# {u} = 原始 github URL；代理把原始 URL 拼在后面
$Proxies = @(
  "https://ghfast.top/",
  "https://gh-proxy.com/",
  "https://ghproxy.net/",
  "https://hub.gitmirror.com/",
  "https://gh.llkk.cc/",
  "https://github.moeyy.xyz/",
  "https://cdn.jsdelivr.net/gh/",   # 仅文件（不适用 release/codeload）
  ""                                 # 直连兜底
)

function Get-ProxyUrl {
  param([string]$RawUrl, [string]$Proxy)
  if ($Proxy -eq "") { return $RawUrl }
  if ($Proxy -like "*cdn.jsdelivr.net*") { return $null }  # jsdelivr 不支持 release/codeload
  return "$Proxy$RawUrl"
}

function Test-Url {
  param([string]$Url, [int]$TimeoutSec = 12)
  try {
    $req = [System.Net.HttpWebRequest]::Create($Url)
    $req.Method = "GET"
    $req.Timeout = $TimeoutSec * 1000
    $req.ReadWriteTimeout = $TimeoutSec * 1000
    $req.UserAgent = "ai-voice-fetch"
    $req.AddRange(0, 1024)          # 只取 1KB 验证可用性
    $resp = $req.GetResponse()
    $code = [int]$resp.StatusCode
    $resp.Close()
    return ($code -ge 200 -and $code -lt 300)
  } catch {
    return $false
  }
}

# ---------------------------------------------------------------- 资源清单
# kind: file（单文件）| repo（仓库 zip）
$Items = @(
  @{
    name = "weasel"; kind = "file"; out = "rime\weasel-0.17.4.0-installer.exe"
    raw = "https://github.com/rime/weasel/releases/download/0.17.4/weasel-0.17.4.0-installer.exe"
    size = 12060165; sha256 = "aa3947703ad43979990864494179553eb3f76cc188f723f467f9cdeeefa6a8e8"
    note = "倾听输入法 0.17.4 安装器（自建 fork 版，2026-09-24 替换；-Force 会因哈希不符而安全跳过，重建见 resources\rime\weasel-fork\README.md）"
  },
  @{
    name = "rime-ice"; kind = "repo"; owner = "iDvel"; repo = "rime-ice"; branch = "main"
    out = "rime\schemes\rime-ice"; zipdir = "rime-ice-main"
    size = 55000000
    note = "雾凇拼音（含 melt_eng 英文混输）"
  },
  @{
    name = "easy_en"; kind = "repo"; owner = "BlindingDark"; repo = "rime-easy-en"; branch = "master"
    out = "rime\schemes\easy_en"; zipdir = "rime-easy-en-master"
    size = 15000000
    note = "英文主力方案"
  },
  @{
    name = "jaroomaji"; kind = "repo"; owner = "lazyfoxchan"; repo = "rime-jaroomaji"; branch = "master"
    out = "rime\schemes\jaroomaji"; zipdir = "rime-jaroomaji-main"
    size = 95000000
    note = "日文罗马字方案（mozc + jmdict 词库）"
  },
  @{
    name = "silero-vad"; kind = "file"; out = "models\silero_vad.onnx"
    raw = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx"
    size = 643854; sha256 = "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6"
    note = "Silero VAD（C1 门控判据，~630KB）：随包分发 → 装机后 {app}\models\silero_vad.onnx；缺失时主程序也会自己下载兜底"
  },
  @{
    name = "lm"; kind = "file"; out = "rime\lm\wanxiang-lts-zh-hans.gram"
    raw = "https://github.com/amzxyz/RIME-LMDG/releases/download/LTS/wanxiang-lts-zh-hans.gram"
    size = 0; sha256 = ""
    note = "万象语法模型 LTS（CC-BY-4.0）"
  }
)

function Get-Candidates {
  param($Item)
  $urls = @()
  if ($Item.kind -eq "file") {
    foreach ($p in $Proxies) {
      $u = Get-ProxyUrl -RawUrl $Item.raw -Proxy $p
      if ($u) { $urls += [pscustomobject]@{ proxy = $p; url = $u } }
    }
  } else {
    $branches = @($Item.branch)
    if ($Item.branch -eq "main") { $branches += "master" } else { $branches += "main" }
    foreach ($b in $branches) {
      $codeload = "https://codeload.github.com/$($Item.owner)/$($Item.repo)/zip/refs/heads/$b"
      $archive = "https://github.com/$($Item.owner)/$($Item.repo)/archive/refs/heads/$b.zip"
      foreach ($p in $Proxies) {
        foreach ($raw in @($codeload, $archive)) {
          $u = Get-ProxyUrl -RawUrl $raw -Proxy $p
          if ($u) { $urls += [pscustomobject]@{ proxy = $p; url = $u; branch = $b } }
        }
      }
    }
  }
  return $urls
}

# ---------------------------------------------------------------- 下载
function Save-Url {
  param([string]$Url, [string]$OutFile, [string]$Label)
  $tmp = "$OutFile.part"
  if (Test-Path $tmp) { Remove-Item $tmp -Force }
  $req = [System.Net.HttpWebRequest]::Create($Url)
  $req.Method = "GET"
  $req.Timeout = 60000
  $req.ReadWriteTimeout = 300000
  $req.UserAgent = "ai-voice-fetch"
  $resp = $req.GetResponse()
  $total = $resp.ContentLength
  $in = $resp.GetResponseStream()
  $out = [System.IO.File]::Create($tmp)
  $buf = New-Object byte[] 1048576
  $done = 0
  $sw = [Diagnostics.Stopwatch]::StartNew()
  while (($n = $in.Read($buf, 0, $buf.Length)) -gt 0) {
    $out.Write($buf, 0, $n)
    $done += $n
    if ($total -gt 0) {
      $pct = [int]($done * 100 / $total)
      $mb = [math]::Round($done / 1MB, 1)
      $tot = [math]::Round($total / 1MB, 1)
      Write-Progress -Activity "下载 $Label" -Status "$mb / $tot MB" -PercentComplete $pct
    }
  }
  $out.Close(); $in.Close(); $resp.Close()
  Write-Progress -Activity "下载 $Label" -Completed
  Move-Item $tmp $OutFile -Force
  $secs = [math]::Round($sw.Elapsed.TotalSeconds, 1)
  $mb = [math]::Round($done / 1MB, 1)
  $speed = if ($secs -gt 0) { [math]::Round($mb / $secs, 2) } else { 0 }
  Write-Host ("    完成 {0} MB / {1}s = {2} MB/s" -f $mb, $secs, $speed)
  return $done
}

# ---------------------------------------------------------------- 解压
function Expand-ZipFlat {
  param([string]$Zip, [string]$Target, [string]$ZipDir)
  $tmp = "$Zip.extract"
  if (Test-Path $tmp) { Remove-Item $tmp -Recurse -Force }
  Expand-Archive -Path $Zip -DestinationPath $tmp -Force
  $src = Join-Path $tmp $ZipDir
  if (-not (Test-Path $src)) {
    # 顶层目录名兜底：取唯一目录
    $dirs = Get-ChildItem $tmp -Directory
    if ($dirs.Count -eq 1) { $src = $dirs[0].FullName } else { throw "解压目录结构异常: $Zip" }
  }
  if (Test-Path $Target) { Remove-Item $Target -Recurse -Force }
  New-Item -ItemType Directory -Path (Split-Path $Target -Parent) -Force | Out-Null
  Move-Item $src $Target -Force
  Remove-Item $tmp -Recurse -Force
}

# ---------------------------------------------------------------- 主流程
Write-Output "目标目录: $Dest"
if (-not (Test-Path $Dest)) { New-Item -ItemType Directory -Path $Dest -Force | Out-Null }

$todo = $Items
if ($Only -ne "") { $todo = $Items | Where-Object { $_.name -eq $Only } }
if (-not $todo) { throw "未知 -Only $Only" }

$report = @()
foreach ($it in $todo) {
  Write-Output ""
  Write-Output "== $($it.name)：$($it.note)"
  $target = Join-Path $Dest $it.out

  # 本地优先
  if ($LocalDir -ne "" -and -not $Force) {
    $cand = Join-Path $LocalDir $it.out
    if (Test-Path $cand) {
      Write-Output "    本地命中: $cand"
      $parent = Split-Path $target -Parent
      if (-not (Test-Path $parent)) { New-Item -ItemType Directory -Path $parent -Force | Out-Null }
      if (Test-Path $target) { Remove-Item $target -Recurse -Force }
      Copy-Item $cand $target -Recurse -Force
      $report += [pscustomobject]@{ 项 = $it.name; 结果 = "本地拷贝"; 大小MB = 0; 来源 = $LocalDir }
      continue
    }
  }

  if ((Test-Path $target) -and -not $Force) {
    $sz = if ((Get-Item $target).PSIsContainer) {
      (Get-ChildItem $target -Recurse -File | Measure-Object Length -Sum).Sum
    } else { (Get-Item $target).Length }
    Write-Output ("    已存在，跳过（{0} MB）" -f [math]::Round($sz / 1MB, 1))
    $report += [pscustomobject]@{ 项 = $it.name; 结果 = "已存在"; 大小MB = [math]::Round($sz / 1MB, 1); 来源 = "-" }
    continue
  }

  # 探测可用镜像
  $cands = Get-Candidates -Item $it
  $picked = $null
  foreach ($c in $cands) {
    if ($Probe) { Write-Output "    探测 $($c.url)" }
    if (Test-Url -Url $c.url) { $picked = $c; break }
  }
  if (-not $picked) {
    Write-Output "    !! 所有镜像都不可用"
    $report += [pscustomobject]@{ 项 = $it.name; 结果 = "失败（无可用镜像）"; 大小MB = 0; 来源 = "-" }
    continue
  }
  Write-Output "    镜像: $($picked.url)"
  if ($Probe) { continue }

  $parent = Split-Path $target -Parent
  if (-not (Test-Path $parent)) { New-Item -ItemType Directory -Path $parent -Force | Out-Null }

  if ($it.kind -eq "file") {
    $zipPath = "$target.download"
    $got = Save-Url -Url $picked.url -OutFile $zipPath -Label $it.name
    if ($it.sha256 -ne "") {
      $h = (Get-FileHash $zipPath -Algorithm SHA256).Hash.ToLower()
      if ($h -ne $it.sha256.ToLower()) {
        Write-Output "    !! SHA256 不匹配：$h"
        $report += [pscustomobject]@{ 项 = $it.name; 结果 = "SHA256 不匹配"; 大小MB = [math]::Round($got / 1MB, 1); 来源 = $picked.url }
        Remove-Item $zipPath -Force
        continue
      }
      Write-Output "    SHA256 校验通过"
    }
    Move-Item $zipPath $target -Force
    $report += [pscustomobject]@{ 项 = $it.name; 结果 = "OK"; 大小MB = [math]::Round($got / 1MB, 1); 来源 = $picked.url }
  } else {
    $zipPath = "$target.zip"
    $got = Save-Url -Url $picked.url -OutFile $zipPath -Label $it.name
    $zipdir = $it.zipdir
    if ($picked.PSObject.Properties.Name -contains "branch") {
      $zipdir = "$($it.repo)-$($picked.branch)"
    }
    try {
      Expand-ZipFlat -Zip $zipPath -Target $target -ZipDir $zipdir
      Remove-Item $zipPath -Force
      $sz = (Get-ChildItem $target -Recurse -File | Measure-Object Length -Sum).Sum
      $cnt = (Get-ChildItem $target -Recurse -File).Count
      Write-Output ("    解压完成：{0} 个文件 / {1} MB" -f $cnt, [math]::Round($sz / 1MB, 1))
      $report += [pscustomobject]@{ 项 = $it.name; 结果 = "OK"; 大小MB = [math]::Round($sz / 1MB, 1); 来源 = $picked.url }
    } catch {
      Write-Output "    !! 解压失败: $_"
      $report += [pscustomobject]@{ 项 = $it.name; 结果 = "解压失败"; 大小MB = [math]::Round($got / 1MB, 1); 来源 = $picked.url }
    }
  }
}

Write-Output ""
Write-Output "===== 汇总 ====="
$report | Format-Table -AutoSize | Out-String | Write-Output
$totalMB = ($report | Measure-Object 大小MB -Sum).Sum
Write-Output ("合计 {0} MB" -f [math]::Round($totalMB, 1))
