# 报告显示器的真实显示模式与 EnumDisplayMonitors 在 DPI 感知下的结果。
# 用途：确认 GetMonitorInfoW 返回的到底是物理像素还是缩放后的逻辑像素。
Add-Type @'
using System;
using System.Runtime.InteropServices;
public class Dm {
  [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
  public struct DEVMODE {
    [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string dmDeviceName;
    public short dmSpecVersion; public short dmDriverVersion; public short dmSize;
    public short dmDriverExtra; public int dmFields;
    public int dmPositionX; public int dmPositionY;
    public int dmDisplayOrientation; public int dmDisplayFixedOutput;
    public short dmColor; public short dmDuplex; public short dmYResolution;
    public short dmTTOption; public short dmCollate;
    [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string dmFormName;
    public short dmLogPixels; public int dmBitsPerPel; public int dmPelsWidth;
    public int dmPelsHeight; public int dmDisplayFlags; public int dmDisplayFrequency;
    public int dmICMMethod; public int dmICMIntent; public int dmMediaType;
    public int dmDitherType; public int dmReserved1; public int dmReserved2;
    public int dmPanningWidth; public int dmPanningHeight;
  }
  [DllImport("user32.dll", CharSet = CharSet.Unicode)]
  public static extern bool EnumDisplaySettings(string device, int mode, ref DEVMODE dm);
  [DllImport("user32.dll")]
  public static extern bool SetProcessDPIAware();
  [DllImport("shcore.dll")]
  public static extern int SetProcessDpiAwareness(int value);
}
'@

[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

"=== 先设为 DPI 感知，再查询 ==="
try { [Dm]::SetProcessDpiAwareness(2) | Out-Null } catch { [Dm]::SetProcessDPIAware() | Out-Null }

foreach ($dev in @('\\.\DISPLAY1', '\\.\DISPLAY2')) {
  $dm = New-Object Dm+DEVMODE
  $dm.dmSize = [short][System.Runtime.InteropServices.Marshal]::SizeOf($dm)
  if ([Dm]::EnumDisplaySettings($dev, -1, [ref]$dm)) {
    $text = "{0}: DEVMODE {1}x{2} @ {3}Hz 位置({4},{5})" -f $dev, $dm.dmPelsWidth, $dm.dmPelsHeight, $dm.dmDisplayFrequency, $dm.dmPositionX, $dm.dmPositionY
    Write-Host $text
  } else {
    Write-Host "$dev DEVMODE 查询失败"
  }
}

Add-Type -AssemblyName System.Windows.Forms
foreach ($screen in [System.Windows.Forms.Screen]::AllScreens) {
  Write-Host ("{0} Screen.Bounds={1}" -f $screen.DeviceName, $screen.Bounds)
}
