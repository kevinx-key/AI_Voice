$ErrorActionPreference = "Continue"
Add-Type @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class W {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  public static List<string> Find(uint targetPid) {
    var list = new List<string>();
    EnumWindows((h, l) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (pid != targetPid) return true;
      var t = new StringBuilder(512); GetWindowText(h, t, 512);
      var c = new StringBuilder(256); GetClassName(h, c, 256);
      list.Add(string.Format("vis={0} class={1} title={2}", IsWindowVisible(h), c, t));
      return true;
    }, IntPtr.Zero);
    return list;
  }
}
"@

$p = Start-Process -FilePath "K:\AI_Voice_rs\target\release\listen.exe" -ArgumentList '--config','K:\AI_Voice_rs\config.toml' -PassThru
Start-Sleep -Seconds 8
if ($p.HasExited) { Write-Output "PROCESS_EXITED code=$($p.ExitCode)"; exit 1 }
Write-Output "PID=$($p.Id)  alive"
Write-Output "---- windows ----"
[W]::Find([uint32]$p.Id) | ForEach-Object { Write-Output $_ }
Write-Output "---- console check ----"
$con = [W]::Find([uint32]$p.Id) | Where-Object { $_ -match 'ConsoleWindowClass' }
if ($con) { Write-Output "CONSOLE_WINDOW_PRESENT" } else { Write-Output "NO_CONSOLE_WINDOW" }
Stop-Process -Id $p.Id -Force
