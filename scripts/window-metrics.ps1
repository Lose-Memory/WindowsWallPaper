# 报告 WindowsWallPaper 主窗口的窗口矩形与客户区尺寸。
# 用于核对"界面看起来被裁掉"到底是窗口尺寸问题还是页面横向溢出问题——
# 直接量，而不是靠肉眼看截图。
[CmdletBinding()]
param([string]$ProcessName = 'WindowsWallPaper')

Add-Type @'
using System;
using System.Runtime.InteropServices;
public class Metrics {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
'@

$proc = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue |
    Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $proc) { throw "找不到 $ProcessName 窗口" }
$hwnd = $proc.MainWindowHandle

$w = New-Object Metrics+RECT
$c = New-Object Metrics+RECT
[Metrics]::GetWindowRect($hwnd, [ref]$w) | Out-Null
[Metrics]::GetClientRect($hwnd, [ref]$c) | Out-Null
$dpi = [Metrics]::GetDpiForWindow($hwnd)

"窗口矩形 : ($($w.Left),$($w.Top))  $($w.Right - $w.Left) x $($w.Bottom - $w.Top) 物理像素"
"客户区   : $($c.Right - $c.Left) x $($c.Bottom - $c.Top) 物理像素"
"窗口 DPI : $dpi（缩放 $([math]::Round($dpi / 96 * 100))%）"
