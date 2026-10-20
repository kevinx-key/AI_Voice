# ime_type_test.ps1 — 打字侧 E2E：把按键序列送进倾听输入法，回读上屏文本
#
# 为什么需要它：`listen.exe --inject` 只能测「语音注入」，测不了「打字候选」
# （Rime 跑在 WeaselServer.exe 里，是独立进程）。这里开一个 WinForms 文本框当输入目标，
# 用 SendKeys 敲键、直接读控件文本，就能无脑回归「打某编码出什么候选」。
#
# ⚠️ 要**非提权**运行（提权窗口里 SendKeys 会被 UIPI 挡），且运行时会抢前台焦点。
# ⚠️ 第一次调用有可能丢首键（窗口还没抢到焦点）——脚本开头留了空档期，偶发失败就重跑一次。
#
# 用法：
#   .\ime_type_test.ps1 -Seq 'shishi','2'                    # 打 shishi 后按 2 选第二个候选
#   .\ime_type_test.ps1 -Seq 'gds',' '                       # 快捷输入：gds + 空格
#   .\ime_type_test.ps1 -Seq 'kaituan',' ' -IntervalMs 1500  # 慢一点的机器
#
# 输出：`上屏: [文本]`（就是 IME 真正提交出来的东西）
param(
  [string[]]$Seq = @('gb', ' '),
  [int]$IntervalMs = 1300
)

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$form = New-Object System.Windows.Forms.Form
$form.Text = 'RimeIME-Test'
$form.Size = New-Object System.Drawing.Size(760, 200)
$tb = New-Object System.Windows.Forms.TextBox
$tb.Multiline = $true
$tb.Dock = 'Fill'
$tb.Font = New-Object System.Drawing.Font('Microsoft YaHei', 16)
$form.Controls.Add($tb)
$form.StartPosition = 'CenterScreen'

$script:step = 0
$script:out = ''
$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = $IntervalMs
$timer.Add_Tick({
    if ($script:step -lt $Seq.Count) {
      [System.Windows.Forms.SendKeys]::SendWait($Seq[$script:step])
      $script:step++
    }
    else {
      $script:out = $tb.Text
      $form.Close()
    }
  })

$form.Add_Shown({
    $form.Activate()
    $tb.Focus()
    $timer.Start()
  })
$form.ShowDialog() | Out-Null
Write-Output ("上屏: [" + $script:out + "]")
