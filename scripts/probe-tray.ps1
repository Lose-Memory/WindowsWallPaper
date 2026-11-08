# 验证托盘：图标是否真的出现在通知区域，以及关闭窗口是否隐藏到托盘。
[CmdletBinding()]
param([string]$ProcessName = 'WindowsWallPaper')

Add-Type @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public class TrayProbe {
  public delegate bool EnumProc(IntPtr h, IntPtr p);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr p);
  [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr parent, EnumProc cb, IntPtr p);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindow(string cls, string win);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);

  public static List<string> ClassesOwnedBy(uint targetPid) {
    var list = new List<string>();
    EnumWindows((h, p) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (pid == targetPid) {
        var sb = new StringBuilder(256); GetClassName(h, sb, sb.Capacity);
        list.Add(sb.ToString() + (IsWindowVisible(h) ? " (可见)" : " (隐藏)"));
      }
      return true;
    }, IntPtr.Zero);
    return list;
  }
}
'@

[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$proc = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) { Write-Host "进程未运行"; exit 1 }

Write-Host "=== 本进程拥有的顶层窗口 ==="
foreach ($cls in [TrayProbe]::ClassesOwnedBy([uint32]$proc.Id)) { Write-Host "  $cls" }

Write-Host ""
Write-Host "=== 关闭主窗口，看进程是否还在（应隐藏到托盘）==="
$hwnd = $proc.MainWindowHandle
if ($hwnd -eq [IntPtr]::Zero) { Write-Host "  找不到主窗口句柄"; exit 1 }
[void][TrayProbe]::PostMessage($hwnd, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)   # WM_CLOSE
Start-Sleep -Seconds 3

$still = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1
if ($still) {
  Write-Host "  进程仍在运行（PID $($still.Id)）——关闭窗口没有退出程序"
  Write-Host "  主窗口句柄现在是：$($still.MainWindowHandle)（0 表示已隐藏）"
} else {
  Write-Host "  进程已退出——关闭窗口导致了退出"
}
