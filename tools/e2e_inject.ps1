$ErrorActionPreference = "Stop"
$target = "K:\AI_Voice_rs\.cache\inject_target.txt"
Set-Content -LiteralPath $target -Value "" -NoNewline
Start-Process -FilePath "notepad.exe" -ArgumentList $target
Start-Sleep -Seconds 3

Add-Type @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class Win {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  public static List<IntPtr> Find(string needle) {
    var list = new List<IntPtr>();
    EnumWindows((h, l) => {
      if (!IsWindowVisible(h)) return true;
      var sb = new StringBuilder(512);
      GetWindowText(h, sb, 512);
      if (sb.ToString().Contains(needle)) list.Add(h);
      return true;
    }, IntPtr.Zero);
    return list;
  }
}
"@
$found = [Win]::Find("inject_target")
if ($found.Count -eq 0) { Write-Output "NOTEPAD_NOT_FOUND"; exit 1 }
$hwnd = $found[0]
[void][Win]::SetForegroundWindow($hwnd)
Start-Sleep -Milliseconds 800

& "K:\AI_Voice_rs\target\release\listen.exe" --config "K:\AI_Voice_rs\config.toml" --inject "注入链路测试 OK 12345"
Start-Sleep -Seconds 2

Add-Type -AssemblyName System.Windows.Forms
[System.Windows.Forms.SendKeys]::SendWait("^s")
Start-Sleep -Seconds 2

Write-Output "---- 文件内容 ----"
Get-Content -LiteralPath $target -Raw
Write-Output ""
Write-Output "---- 完 ----"
Stop-Process -Name notepad -Force -ErrorAction SilentlyContinue
