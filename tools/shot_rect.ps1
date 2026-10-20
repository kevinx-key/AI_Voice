# shot_rect.ps1 —— 截屏幕右下角一块区域（默认托盘/通知区），用于验证托盘图标状态
# 用法: powershell -NoProfile -File tools\shot_rect.ps1 -Out K:\AI_Voice_rs\.cache\tray.png -W 420 -H 60
#
# ⚠️ 本机是 200% 缩放（AppliedDPI=192）：**必须先 SetProcessDPIAware**，否则
#    Screen.Bounds 与 CopyFromScreen 用的是虚拟化坐标，截出来的区域完全错位
#    （踩过：420x40 的托盘区截出一片空白灰）。
param(
  [Parameter(Mandatory = $true)][string]$Out,
  [int]$W = 420,
  [int]$H = 60,
  [int]$X = -1,
  [int]$Y = -1
)
$ErrorActionPreference = "Stop"
Add-Type -TypeDefinition 'using System.Runtime.InteropServices; public class DpiAware { [DllImport("user32.dll")] public static extern bool SetProcessDPIAware(); }'
[void][DpiAware]::SetProcessDPIAware()
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$x = if ($X -ge 0) { $X } else { $b.Right - $W }
$y = if ($Y -ge 0) { $Y } else { $b.Bottom - $H }
$dir = Split-Path -Parent $Out
if (!(Test-Path $dir)) { New-Item -ItemType Directory -Path $dir | Out-Null }
$bmp = New-Object System.Drawing.Bitmap $W, $H
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($x, $y, 0, 0, $bmp.Size)
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Output "OK $Out rect=$x,$y ${W}x${H} screen=$($b.Width)x$($b.Height)"
