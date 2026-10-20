# verify_rime.ps1 — Rime 二合一「文件层」自动校验（装机验收用）
#
# 用法：
#   .\verify_rime.ps1                     # 用默认用户目录 %APPDATA%\Rime
#   .\verify_rime.ps1 -UserDir D:\Rime    # 指定用户目录
#   .\verify_rime.ps1 -Lang ja            # 指定期望的主力语言（zh/en/ja）
#   .\verify_rime.ps1 -AfterUninstall     # 卸载后残留检查模式
#
# 退出码：0 = 全部通过；1 = 有失败项

param(
  [string]$UserDir = "$env:APPDATA\Rime",
  [string]$Lang = "zh",
  [string]$ResDir = "K:\AI_Voice_rs\resources",
  [switch]$AfterUninstall
)

$ErrorActionPreference = "Continue"
$fail = 0
$pass = 0

function Ok([string]$msg)   { Write-Output ("  [OK]   " + $msg); $script:pass++ }
function Bad([string]$msg)  { Write-Output ("  [FAIL] " + $msg); $script:fail++ }
function Warn([string]$msg) { Write-Output ("  [WARN] " + $msg) }

Write-Output "用户目录: $UserDir"
Write-Output "期望语言: $Lang"
Write-Output ""

# 期望的方案列表
$expect = switch ($Lang) {
  "zh" { @("rime_ice", "melt_eng") }
  "en" { @("easy_en") }
  "ja" { @("jaroomaji", "easy_en") }
  default { Write-Output "未知语言: $Lang（支持 zh/en/ja）"; exit 1 }
}

if ($AfterUninstall) {
  Write-Output "== 卸载残留检查 =="
  foreach ($d in @("rime-ice", "easy_en", "jaroomaji")) {
    if (Test-Path (Join-Path $UserDir $d)) { Bad "目录残留: $d" } else { Ok "无目录 $d" }
  }
  if (Test-Path (Join-Path $UserDir "wanxiang-lts-zh-hans.gram")) { Bad "语法模型残留" } else { Ok "无语法模型残留" }
  if (Test-Path (Join-Path $UserDir "listen_hotwords.txt")) { Bad "热词表残留" } else { Ok "无热词表残留" }
  $manifestLeft = @(".listen-files.txt", ".ai-voice-files.txt") | Where-Object { Test-Path (Join-Path $UserDir $_) }
  if ($manifestLeft) { Bad "清单文件残留: $($manifestLeft -join ', ')" } else { Ok "无清单残留" }
  foreach ($f in @("default.custom.yaml", "rime_ice.custom.yaml", "easy_en.custom.yaml", "jaroomaji.custom.yaml")) {
    $p = Join-Path $UserDir $f
    if (Test-Path $p) {
      $c = Get-Content $p -Raw
      if ($c -match "listen managed|ai-voice managed") { Bad "$f 里还有我们的管理块" } else { Ok "$f 无管理块（用户自己的）" }
    } else { Ok "$f 已删除" }
  }
  # 用户数据必须保留
  foreach ($keep in @("build", "user.yaml")) {
    if (Test-Path (Join-Path $UserDir $keep)) { Ok "用户数据保留: $keep" } else { Warn "用户数据缺失: $keep（新装环境正常）" }
  }
} else {
  Write-Output "== 1. 方案包文件 =="
  $srcRoot = Join-Path $ResDir "rime\schemes"
  # 与 src\rime\mod.rs 的 SKIP_DIRS / SKIP_FILES 保持一致（纯文档/构建工具/备用方案不铺）
  $skipDirs = @(".github", "img4md", "dict_tools", "others")
  $skipFiles = @("README.md", "LICENSE", "AGENTS.md", "CHANGELOG.md", ".gitignore", ".gitmodules", "recipe.yaml", "customize.recipe.yaml")
  # 与 src\rime\mod.rs 的 SKIP_SCHEMA_FILES 保持一致（同语言的多余方案不铺）
  $skipSchemas = @("t9.schema.yaml", "radical_pinyin.schema.yaml", "double_pinyin.schema.yaml", "double_pinyin_abc.schema.yaml", "double_pinyin_mspy.schema.yaml", "double_pinyin_sogou.schema.yaml", "double_pinyin_flypy.schema.yaml", "double_pinyin_ziguang.schema.yaml", "double_pinyin_jiajia.schema.yaml", "default.yaml")
  foreach ($name in @("rime-ice", "easy_en", "jaroomaji")) {
    $src = Join-Path $srcRoot $name
    if (-not (Test-Path $src)) { Bad "资源里没有 $name（无法校验）"; continue }
    $srcFiles = Get-ChildItem $src -Recurse -File | Where-Object {
      $p = $_.FullName
      $rel = $p.Substring($src.Length + 1)
      $skip = $false
      foreach ($d in $skipDirs) { if ($p -like "*\$d\*") { $skip = $true } }
      if (-not $skip -and ($rel -notlike "*\*") -and ($skipFiles -contains $rel)) { $skip = $true }
      if (-not $skip -and ($skipSchemas -contains $rel)) { $skip = $true }
      -not $skip
    }
    $missing = 0
    foreach ($f in $srcFiles) {
      $rel = $f.FullName.Substring($src.Length + 1)
      if (-not (Test-Path (Join-Path $UserDir $rel))) { $missing++ }
    }
    if ($missing -eq 0) { Ok "$name 铺齐（$($srcFiles.Count) 个文件）" } else { Bad "$name 缺 $missing 个文件" }
  }

  Write-Output "== 2. 配置（default.custom.yaml）=="
  $dc = Join-Path $UserDir "default.custom.yaml"
  if (Test-Path $dc) {
    $c = Get-Content $dc -Raw
    if ($c -match "listen managed") { Ok "有管理块" } else { Bad "没有我们的管理块" }
    foreach ($s in $expect) {
      # 锚定到独立列表项（注释里的 / 前缀不同的都不算）；-match 默认非多行，必须带 (?m)
      if ($c -match "(?m)^\s*-\s*schema:\s+$s[ \t]*$") { Ok "schema_list 含 $s" } else { Bad "schema_list 缺 $s" }
    }
  } else { Bad "default.custom.yaml 不存在" }

  Write-Output "== 3. 部署产物 =="
  foreach ($s in $expect) {
    $b = Join-Path $UserDir "build\$s.schema.yaml"
    if (Test-Path $b) { Ok "build\$s.schema.yaml" } else { Bad "缺 build\$s.schema.yaml（部署没跑完）" }
  }
  if ($Lang -eq "zh") {
    # 热词表（打字侧 custom_phrase）：文件 + 表头 db_name + 方案 patch 三件套
    $ht = Join-Path $UserDir "listen_hotwords.txt"
    if (Test-Path $ht) {
      $c = Get-Content $ht -Raw
      $n = ($c -split "`n" | Where-Object { $_ -match "`t" -and $_ -notmatch "^#" }).Count
      if ($c -match "#@/db_name\s+listen_hotwords\.txt") { Ok "热词表已铺（$n 条，表头 db_name 正确）" } else { Bad "热词表表头 #@/db_name 不对" }
    } else { Warn "热词表不存在（热词全关或没部署过？）" }
    $rcCustom = Join-Path $UserDir "rime_ice.custom.yaml"
    if (Test-Path $rcCustom) {
      $c = Get-Content $rcCustom -Raw
      if ($c -match "custom_phrase/user_dict:\s*listen_hotwords") { Ok "rime_ice.custom.yaml 已指向热词表" } else { Warn "rime_ice.custom.yaml 没有热词表 patch" }
      # 用户词学习钩子：lua 脚本 + patch + **编译产物里真的挂上了**
      # （2026-10-09 踩过：`engine/processors/+` 值写成标量 → librime 拒绝整段 patch →
      #   整个 rime_ice 方案编译失败，但部署器退出码仍是 0，只能从编译产物/日志看出来）
      $lua = Join-Path $UserDir "lua\listen_learn.lua"
      if (Test-Path $lua) { Ok "用户词学习钩子已铺（lua\listen_learn.lua）" } else { Warn "没有学习钩子（未开学习或没部署）" }
      $bc = Join-Path $UserDir "build\rime_ice.schema.yaml"
      if (Test-Path $bc) {
        $cb = Get-Content $bc -Raw
        if ($c -match "lua_processor@\*listen_learn") {
          if ($cb -match "lua_processor@\*listen_learn") { Ok "学习钩子已编译进方案" } else { Bad "patch 没生效：编译产物里没有 lua_processor@*listen_learn" }
        }
        if ($cb -match "user_dict:\s*listen_hotwords") { Ok "热词表已编译进方案" } else { Bad "patch 没生效：编译产物里没有 custom_phrase/user_dict" }
      }
    }
    $g = Join-Path $UserDir "wanxiang-lts-zh-hans.gram"
    if (Test-Path $g) { Ok "语法模型已铺（$([math]::Round((Get-Item $g).Length/1MB,1)) MB）" } else { Warn "语法模型缺失（grammar=true 时应存在）" }
    $rc = Join-Path $UserDir "build\rime_ice.schema.yaml"
    if (Test-Path $rc) {
      $c = Get-Content $rc -Raw
      if ($c -match "grammar:") { Ok "编译后的 rime_ice 带 grammar 配置" } else { Warn "编译后的 rime_ice 没有 grammar（可能关了语法模型）" }
    }
  }

  Write-Output "== 4. 部署日志 =="
  $logDir = Join-Path $env:TEMP "rime.weasel"
  if (Test-Path $logDir) {
    $newest = Get-ChildItem $logDir -Filter "*.log" | Sort-Object LastWriteTime | Select-Object -Last 1
    if ($newest) {
      $bad = Select-String -Path $newest.FullName -Pattern "missing input schema" -Quiet
      if ($bad) {
        # 已知：rime_ice 声明依赖 radical_pinyin，但 SKIP_SCHEMA_FILES 刻意不铺那个方案文件
        # → 部件反查(u 模式)不可用；不是本轮部署失败（其余方案照样 2 success / 0 failure）
        $rp = Select-String -Path $newest.FullName -Pattern "unsatisfied dependency: radical_pinyin" -Quiet
        if ($rp) { Warn "日志有 radical_pinyin 依赖缺失（已知：不铺 radical_pinyin.schema.yaml → 部件反查不可用）" } else { Bad "日志里有 missing input schema（$($newest.Name)）" }
      } else { Ok "日志无 missing input schema（$($newest.Name)）" }
    } else { Warn "没有 weasel 日志" }
  } else { Warn "没有 rime.weasel 日志目录" }

  Write-Output "== 5. 用户数据未被破坏 =="
  foreach ($keep in @("build", "user.yaml")) {
    if (Test-Path (Join-Path $UserDir $keep)) { Ok "存在: $keep" } else { Warn "缺失: $keep" }
  }
}

Write-Output ""
Write-Output ("===== 通过 {0} 项，失败 {1} 项 =====" -f $pass, $fail)
if ($fail -gt 0) { exit 1 } else { exit 0 }
