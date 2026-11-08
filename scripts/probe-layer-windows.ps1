# 探测桌面层宿主窗口的实际状态：是否可见、在哪、z 序如何。
#
# ## 为什么必须查**顶层**窗口，而不能只查 Progman 的子窗口
#
# 曾经有一个只查 Progman 子窗口的探测脚本（已删除：旧版的桌面层探测脚本）。它在默认的 `toplevel` 模式下**永远报"找不到"**——因为那种模式
# 创建的是顶层窗口，不是 Progman 的子窗口。
#
# 后果很严重：那个脚本一直报"没有残留窗口"，看起来一切正常，
# 而实际上窗口可能正盖在屏幕上（这正是"灰屏"的成因）。
# **探测脚本查错了地方，比没有探测脚本更危险**，因为它会给出虚假的安心。
#
# 因此本脚本两种都查，并且把"可见性"与"位置"一起报出来——
# 只有"可见"且"覆盖屏幕"的窗口才会挡壁纸。
[CmdletBinding()]
param([string]$ClassName = 'WindowsWallPaperDesktopLayer')

Add-Type @'
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class LayerProbe {
  public delegate bool EnumProc(IntPtr h, IntPtr p);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string cls, string win);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr parent, IntPtr after, string cls, string win);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr p);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern IntPtr GetParent(IntPtr h);

  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L,T,R,B; }

  /// 找出所有顶层窗口里指定类名的，返回 "句柄|可见|矩形" 列表。
  public static List<string> FindTopLevel(string cls) {
    var found = new List<string>();
    EnumWindows((h, p) => {
      var sb = new StringBuilder(256);
      GetClassName(h, sb, 256);
      if (sb.ToString() == cls) {
        RECT r; GetWindowRect(h, out r);
        found.Add(string.Format("{0}|{1}|({2},{3}) {4}x{5}",
          h, IsWindowVisible(h) ? "可见" : "隐藏",
          r.L, r.T, r.R - r.L, r.B - r.T));
      }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
'@

[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

Write-Host "===== 顶层窗口里类名为 $ClassName 的窗口 ====="
$top = [LayerProbe]::FindTopLevel($ClassName)
if ($top.Count -eq 0) {
    Write-Host "  无（顶层模式下没有宿主窗口）"
} else {
    $top | ForEach-Object { Write-Host "  $_" }
}

Write-Host ""
Write-Host "===== Progman 的子窗口里同名类 ====="
$progman = [LayerProbe]::FindWindow('Progman', [NullString]::Value)
if ($progman -ne [IntPtr]::Zero) {
    $child = [LayerProbe]::FindWindowEx($progman, [IntPtr]::Zero, $ClassName, [NullString]::Value)
    if ($child -eq [IntPtr]::Zero) {
        Write-Host "  无（子窗口模式下没有宿主窗口）"
    } else {
        $r = New-Object LayerProbe+RECT
        [void][LayerProbe]::GetWindowRect($child, [ref]$r)
        Write-Host "  $child  矩形 ($($r.L),$($r.T)) $($r.R - $r.L)x$($r.B - $r.T)"
    }
} else {
    Write-Host "  找不到 Progman"
}

Write-Host ""
Write-Host "===== 结论 ====="
$visible = $top | Where-Object { $_ -match '可见' }
if ($visible) {
    Write-Host "  有 $($visible.Count) 个**可见**的顶层宿主窗口，它们会盖住系统壁纸。"
    Write-Host '  这就是「灰屏」的来源。'
} elseif ($top.Count -gt 0) {
    Write-Host "  有 $($top.Count) 个顶层宿主窗口，但都不可见（不会挡壁纸）。"
} else {
    Write-Host '  没有任何宿主窗口——此时屏幕应当显示正常的系统壁纸。'
}
