# 把 WindowsWallPaper 主窗口调整到指定大小，便于抓取完整界面。
[CmdletBinding()]
param(
    [string]$ProcessName = 'WindowsWallPaper',
    [int]$Width = 1400,
    [int]$Height = 900
)

Add-Type @'
using System;
using System.Runtime.InteropServices;
public class Resizer {
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
'@

$proc = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue |
    Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $proc) { throw "找不到 $ProcessName 窗口" }

$rect = New-Object Resizer+RECT
[Resizer]::GetWindowRect($proc.MainWindowHandle, [ref]$rect) | Out-Null
# SWP_NOSENDCHANGING = 0x0400，避免 Tauri 在处理尺寸变化时被我们的调用打断。
[Resizer]::SetWindowPos($proc.MainWindowHandle, [IntPtr]::Zero, $rect.Left, $rect.Top, $Width, $Height, 0x0400) | Out-Null
Start-Sleep -Milliseconds 900
Write-Host "窗口已调整为 ${Width}x${Height}"
