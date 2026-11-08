# 抓取 WindowsWallPaper 主窗口。
#
# 不用 SetForegroundWindow + 屏幕拷贝：Windows 的前台锁会静默拒绝这种切换，
# 结果抓到的是别的窗口（实测抓到过终端）。这里改用 PrintWindow(PW_RENDERFULLCONTENT)，
# 它直接把窗口内容渲染进位图，即使窗口被遮挡也能拿到正确画面。
[CmdletBinding()]
param(
    [string]$OutFile = (Join-Path $PSScriptRoot '..\docs\m1-screenshot.png'),
    [string]$ProcessName = 'WindowsWallPaper'
)

Add-Type -AssemblyName System.Drawing
# 让本进程 **per-monitor DPI 感知**再取窗口矩形。
#
# 不调这一下的坑（AGENTS §9.3 记过）：pwsh 默认不是 DPI 感知进程，
# `GetWindowRect` 返回的是被系统虚拟化过的尺寸（本机 150% 缩放下
# 2100x1425 的窗口只报 1415x984），于是位图只画进**左上角那一块**，
# 右边整块丢掉——看上去像"界面横向溢出"，其实界面没问题。
Add-Type @'
using System;
using System.Runtime.InteropServices;
public class WwpDpi {
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr value);
}
'@
# -4 = DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
[void][WwpDpi]::SetProcessDpiAwarenessContext([IntPtr](-4))
Add-Type @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public class WinShot {
  public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr p);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr h);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }

  public const uint PW_RENDERFULLCONTENT = 0x00000002;

  public class Info {
    public IntPtr Hwnd;
    public string Title = "";
    public string ClassName = "";
    public int Width;
    public int Height;
  }

  public static List<Info> ForProcess(uint targetPid) {
    var list = new List<Info>();
    EnumWindows((h, p) => {
      uint pid;
      GetWindowThreadProcessId(h, out pid);
      if (pid != targetPid || !IsWindowVisible(h)) return true;
      RECT r;
      if (!GetWindowRect(h, out r)) return true;
      int w = r.Right - r.Left, ht = r.Bottom - r.Top;
      if (w < 200 || ht < 200) return true;
      var title = new StringBuilder(512);
      GetWindowText(h, title, title.Capacity);
      var cls = new StringBuilder(256);
      GetClassName(h, cls, cls.Capacity);
      list.Add(new Info { Hwnd = h, Title = title.ToString(), ClassName = cls.ToString(), Width = w, Height = ht });
      return true;
    }, IntPtr.Zero);
    return list;
  }
}
'@

$proc = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) { throw "进程 $ProcessName 未运行" }

$candidates = [WinShot]::ForProcess([uint32]$proc.Id)
if ($candidates.Count -eq 0) { throw "找不到 $ProcessName 的可见窗口" }

Write-Host "该进程的可见窗口："
$candidates | ForEach-Object { "  hwnd=$($_.Hwnd)  $($_.Width)x$($_.Height)  class='$($_.ClassName)'  title='$($_.Title)'" | Write-Host }

# 取面积最大的一个，即主窗口。
$target = $candidates | Sort-Object { $_.Width * $_.Height } -Descending | Select-Object -First 1
Write-Host "选定: hwnd=$($target.Hwnd) $($target.Width)x$($target.Height) class='$($target.ClassName)'"

$bmp = New-Object System.Drawing.Bitmap($target.Width, $target.Height)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
$ok = [WinShot]::PrintWindow($target.Hwnd, $hdc, [WinShot]::PW_RENDERFULLCONTENT)
$g.ReleaseHdc($hdc)
$g.Dispose()

$bmp.Save($OutFile, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()

if ($ok) {
    Write-Host "已保存：$OutFile"
} else {
    Write-Warning "PrintWindow 返回失败，位图可能是空白：$OutFile"
}
