# 列出某个进程的所有顶层窗口（含不可见），用于诊断卡住的 GUI/对话框
param(
  [int]$ProcId = 0,
  [string]$Title = "",
  [switch]$Close
)
$ErrorActionPreference = "Continue"
Add-Type @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class WL {
  [DllImport("shcore.dll")] public static extern int SetProcessDpiAwareness(int value);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  public static IntPtr Last;
  public static uint LastPid;
  public static List<string> List(uint target, string titlePart) {
    var list = new List<string>();
    EnumWindows((h, l) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (target != 0 && pid != target) return true;
      var t = new StringBuilder(512); GetWindowText(h, t, 512);
      if (titlePart != "" && !t.ToString().Contains(titlePart)) return true;
      var c = new StringBuilder(256); GetClassName(h, c, 256);
      RECT r; GetWindowRect(h, out r);
      Last = h;
      LastPid = pid;
      list.Add(string.Format("hwnd={0} pid={1} vis={2} cls={3} rect={4},{5},{6},{7} title={8}",
        h, pid, IsWindowVisible(h), c, r.L, r.T, r.R, r.B, t));
      return true;
    }, IntPtr.Zero);
    return list;
  }
  public static bool CloseLast(uint expectPid) {
    if (Last == IntPtr.Zero || LastPid != expectPid) return false;
    return PostMessage(Last, 0x0010, IntPtr.Zero, IntPtr.Zero);
  }
}
"@

# 每显示器 DPI 感知：窗口 rect 与物理像素对齐（已设置过会返回错误，忽略即可）
[void][WL]::SetProcessDpiAwareness(2)

if ($ProcId -gt 0) {
  [WL]::List([uint32]$ProcId, $Title) | ForEach-Object { Write-Output $_ }
} else {
  [WL]::List(0, $Title) | ForEach-Object { Write-Output $_ }
}
if ($Close) {
  if ($ProcId -le 0) { Write-Output "-Close 需要同时指定 -ProcId（防止关错窗口）"; exit 1 }
  $r = [WL]::CloseLast([uint32]$ProcId)
  Write-Output "WM_CLOSE posted = $r"
}
