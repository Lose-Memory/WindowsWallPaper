# 模拟系统广播：向所有顶层窗口发送 WM_DISPLAYCHANGE。
# 用途：验证"显示器变化后自动重新适配"这条链路，而不必真的拔插显示器。
#
# 注意这与真实拔插不完全等价（真实拔插会改变显示器枚举结果，这个不会），
# 因此它只能证明"广播能被收到、处理器会跑"，不能替代真实的热插拔测试。
[CmdletBinding()]
param(
    [int]$Width = 3840,
    [int]$Height = 2160,
    [int]$Bpp = 32
)

Add-Type @'
using System;
using System.Runtime.InteropServices;
public class Broadcaster {
  public delegate bool EnumProc(IntPtr h, IntPtr p);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr p);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)]
  public static extern IntPtr SendMessageTimeout(IntPtr h, uint msg, IntPtr w, IntPtr l,
      uint flags, uint timeout, out IntPtr result);

  public const uint WM_DISPLAYCHANGE = 0x007E;
  public const uint SMTO_ABORTIFHUNG = 0x0002;

  public static int Broadcast(uint msg, IntPtr w, IntPtr l) {
    int sent = 0;
    EnumWindows((h, p) => {
      IntPtr result;
      SendMessageTimeout(h, msg, w, l, SMTO_ABORTIFHUNG, 200, out result);
      sent++;
      return true;
    }, IntPtr.Zero);
    return sent;
  }
}
'@

[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

# lParam: 低 16 位是宽、高 16 位是高（与 Win32 约定一致）。
$lparam = [IntPtr](($Height -shl 16) -bor $Width)
$sent = [Broadcaster]::Broadcast([Broadcaster]::WM_DISPLAYCHANGE, [IntPtr]::Zero, $lparam)
Write-Host "已向 $sent 个顶层窗口广播 WM_DISPLAYCHANGE（$Width x $Height）"
