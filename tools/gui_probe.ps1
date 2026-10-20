# GUI 探针：截图 / 点击 / 滚轮（窗口坐标与截图 1:1 对应）
param(
  [string]$Title = "倾听输入法",
  [int]$ProcId = 0,
  [string]$Shot = "",
  [int]$ClickX = -1,
  [int]$ClickY = -1,
  [int]$Wheel = 0,
  [switch]$Focus
)

$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class GP {
  [DllImport("shcore.dll")] public static extern int SetProcessDpiAwareness(int value);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, int data, IntPtr extra);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  public static IntPtr Find(uint targetPid, string titlePart) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (targetPid != 0 && pid != targetPid) return true;
      if (!IsWindowVisible(h)) return true;
      var t = new StringBuilder(512); GetWindowText(h, t, 512);
      if (t.ToString().Contains(titlePart)) { found = h; return false; }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
"@

# 每显示器 DPI 感知：点击坐标与截图按物理像素对齐（已设置过会返回错误，忽略即可）
[void][GP]::SetProcessDpiAwareness(2)

$hwnd = [GP]::Find([uint32]$ProcId, $Title)
if ($hwnd -eq [IntPtr]::Zero) { Write-Output "WINDOW_NOT_FOUND"; exit 1 }

$r = New-Object GP+RECT
[void][GP]::GetWindowRect($hwnd, [ref]$r)
$w = $r.R - $r.L
$h = $r.B - $r.T
Write-Output "hwnd=$hwnd rect=$($r.L),$($r.T) size=${w}x${h}"

if ($Focus) { [void][GP]::SetForegroundWindow($hwnd); Start-Sleep -Milliseconds 800 }

if ($Shot -ne "") {
  $bmp = $null; $g = $null
  try {
    $bmp = New-Object System.Drawing.Bitmap $w, $h
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
    $bmp.Save($Shot, [System.Drawing.Imaging.ImageFormat]::Png)
    Write-Output "saved $Shot"
  } finally {
    if ($g) { $g.Dispose() }
    if ($bmp) { $bmp.Dispose() }
  }
}

if ($Wheel -ne 0) {
  [void][GP]::SetCursorPos($r.L + [int]($w / 2), $r.T + [int]($h / 2))
  Start-Sleep -Milliseconds 200
  $notches = [Math]::Abs($Wheel)
  for ($i = 0; $i -lt $notches; $i++) {
    $d = if ($Wheel -gt 0) { 120 } else { -120 }
    [GP]::mouse_event(0x0800, 0, 0, $d, [IntPtr]::Zero)
    Start-Sleep -Milliseconds 60
  }
  Write-Output "wheel $Wheel"
}

if ($ClickX -ge 0 -and $ClickY -ge 0) {
  # 客户端坐标（= egui 点，100% DPI）→ 屏幕坐标
  $origin = New-Object GP+POINT
  $origin.X = 0; $origin.Y = 0
  [void][GP]::ClientToScreen($hwnd, [ref]$origin)
  $cr = New-Object GP+RECT
  [void][GP]::GetClientRect($hwnd, [ref]$cr)
  Write-Output "client size=$($cr.R)x$($cr.B) origin=$($origin.X),$($origin.Y)"
  [void][GP]::SetCursorPos($origin.X + $ClickX, $origin.Y + $ClickY)
  Start-Sleep -Milliseconds 250
  [GP]::mouse_event(0x0002, 0, 0, 0, [IntPtr]::Zero)
  Start-Sleep -Milliseconds 60
  [GP]::mouse_event(0x0004, 0, 0, 0, [IntPtr]::Zero)
  Write-Output "clicked client $ClickX,$ClickY"
}
