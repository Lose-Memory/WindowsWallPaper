# 换图标：给一张正方形 PNG，其余全自动。
#
# 用法（在项目根目录）：
#   pwsh -File scripts\make-icon.ps1                     # 用根目录的 app-icon.png
#   pwsh -File scripts\make-icon.ps1 -Source 我的图.png   # 或者指定别的文件
#
# 它做三件事：
#   1. 校验源图是正方形（不是正方形就停下并说明该怎么裁——系统会把非正方形的
#      图标拉伸变形，那种问题在任务栏上很难看，而且很难联想到是源图的问题）；
#   2. 调 `tauri icon` 生成 `src-tauri/icons/` 全套（托盘图标、exe 图标、
#      Windows 磁贴图、icns…），并删掉其中与 Windows 无关的 android/ios 目录；
#   3. 把 128x128 那档拷成 `frontend/assets/app-icon.png`——界面左上角那个
#      品牌标记用的就是它。
#
# 换完之后要重新构建才看得到：`pwsh -File scripts\build-package.ps1`。
[CmdletBinding()]
param(
    [string]$Source = 'app-icon.png'
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    if (-not (Test-Path $Source)) {
        throw "找不到源图：$Source（把一张正方形 PNG 放到项目根目录，或加 -Source 指定）"
    }

    # --- 1. 校验是正方形 ---
    #
    # 用 .NET 的 System.Drawing 读尺寸：只为了拿宽高，不需要额外的图像库。
    Add-Type -AssemblyName System.Drawing
    $image = [System.Drawing.Image]::FromFile((Resolve-Path $Source).Path)
    try {
        $width = $image.Width
        $height = $image.Height
    } finally {
        $image.Dispose()
    }

    if ($width -ne $height) {
        throw @"
源图不是正方形：${width}x${height}。
图标必须是正方形，否则 Windows 会把它拉伸变形。
自己裁一下（保留中间的正方形区域），或者告诉我，我按"居中裁剪"帮你处理：
  - 宽 > 高：左右各裁掉 (宽-高)/2
  - 高 > 宽：上下各裁掉 (高-宽)/2
"@
    }

    if ($width -lt 256) {
        Write-Warning "源图只有 ${width}x${height}，小于 256x256。放大后的小尺寸图标会发糊，建议换一张更大的。"
    }

    # --- 2. 生成全套图标 ---
    Write-Host "==> 生成图标（tauri icon $Source）" -ForegroundColor Cyan
    & bun run tauri icon $Source
    if ($LASTEXITCODE -ne 0) { throw "tauri icon 失败（退出码 $LASTEXITCODE）" }

    # android/ios 那两套与这个 Windows 桌面程序无关，删掉免得仓库里多出几百个文件。
    foreach ($unused in @('src-tauri\icons\android', 'src-tauri\icons\ios')) {
        if (Test-Path $unused) {
            Remove-Item $unused -Recurse -Force
            Write-Host "    已删除与 Windows 无关的 $unused"
        }
    }

    # --- 3. 界面左上角的品牌标记 ---
    $brand = 'frontend\assets\app-icon.png'
    New-Item -ItemType Directory -Path (Split-Path -Parent $brand) -Force | Out-Null
    Copy-Item 'src-tauri\icons\128x128.png' $brand -Force
    Write-Host "==> 已更新 $brand" -ForegroundColor Cyan

    Write-Host ""
    Write-Host "图标已就绪。重新构建后生效：" -ForegroundColor Green
    Write-Host "  pwsh -File scripts\build-package.ps1"
    Write-Host "（只改了图标的话，构建脚本会重新编译并把新图标嵌进 exe 与托盘。）"
} finally {
    Pop-Location
}
