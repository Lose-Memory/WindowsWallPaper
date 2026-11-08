# 在 WindowsWallPaper 窗口的客户区坐标处模拟一次鼠标左键点击。
#
# 为什么不用 UI 自动化：本项目没有引入自动化依赖，而这里只需要验证
# "点一下按钮 -> 扫描跑起来 -> 结果显示出来"这条链路。
# 用 SendMessage 直接投递 WM_*BUTTON 消息，坐标是客户区坐标，不依赖窗口在屏幕上的位置。
[CmdletBinding()]
param(
    [string]$ProcessName = 'WindowsWallPaper',
    [int]$X = 820,
    [int]$Y = 250
)

Add-Type @'
using System;
using System.Runtime.InteropServices;
public class Clicker {
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }

  public const uint WM_MOUSEMOVE = 0x0200;
  public const uint WM_LBUTTONDOWN = 0x0201;
  public const uint WM_LBUTTONUP = 0x0202;
  public const int MK_LBUTTON = 0x0001;

  public static IntPtr MakeLParam(int x, int y) {
    return (IntPtr)((y << 16) | (x & 0xFFFF));
  }
}
'@

$proc = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue |
    Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $proc) { throw "找不到 $ProcessName 窗口" }
$hwnd = $proc.MainWindowHandle

$rect = New-Object Clicker+RECT
[Clicker]::GetClientRect($hwnd, [ref]$rect) | Out-Null
Write-Host "客户区: $($rect.Right - $rect.Left) x $($rect.Bottom - $rect.Top)"

# 把鼠标真的移过去：SendMessage 不会移动光标，但界面上的 hover 态与
# 某些控件依赖真实光标位置。
$origin = New-Object Clicker+POINT
$origin.X = 0; $origin.Y = 0
[Clicker]::ClientToScreen($hwnd, [ref]$origin) | Out-Null
[Clicker]::SetCursorPos($origin.X + $X, $origin.Y + $Y) | Out-Null
Start-Sleep -Milliseconds 200

$lparam = [Clicker]::MakeLParam($X, $Y)
[Clicker]::SendMessage($hwnd, [Clicker]::WM_MOUSEMOVE, [IntPtr]::Zero, $lparam) | Out-Null
Start-Sleep -Milliseconds 80
[Clicker]::SendMessage($hwnd, [Clicker]::WM_LBUTTONDOWN, [IntPtr][Clicker]::MK_LBUTTON, $lparam) | Out-Null
Start-Sleep -Milliseconds 60
[Clicker]::SendMessage($hwnd, [Clicker]::WM_LBUTTONUP, [IntPtr]::Zero, $lparam) | Out-Null

Write-Host "已在客户区 ($X,$Y) 投递一次左键点击"
