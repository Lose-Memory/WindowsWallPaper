<#
.SYNOPSIS
    生成 M2 用的合成图库测试集。

.DESCRIPTION
    在项目目录内生成一份用于验证扫描、索引、增量重扫与异常统计的图库，
    不触碰用户的真实图片，也不往项目外写任何东西。

    结构（默认 50000 张）：
      base/          少量真实尺寸不同的 PNG/JPEG 原图
      bulk/NNN/      用 base 复制出来的大量文件，制造"几万张图库"的规模
      odd/           异常样本：损坏文件、伪装扩展名、不支持的格式
      excluded/      用于验证排除规则的子树

    输出末尾会打印每个子树的文件数与用途，便于人工核对扫描结果。

.PARAMETER Root
    输出根目录，默认 <项目>\tmp\testlibrary

.PARAMETER BulkCount
    bulk 子树的目标文件数，默认 50000。设为 0 则只生成异常样本。

.PARAMETER Force
    已存在时先删除再重建。
#>
[CmdletBinding()]
param(
    [string]$Root,
    [int]$BulkCount = 50000,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

if (-not $Root) {
    $projectRoot = Split-Path -Parent $PSScriptRoot
    $Root = Join-Path $projectRoot 'tmp\testlibrary'
}

if (Test-Path -LiteralPath $Root) {
    if (-not $Force) { throw "目录已存在：$Root（加 -Force 可重建）" }
    Remove-Item -LiteralPath $Root -Recurse -Force
}

$baseDir = Join-Path $Root 'base'
$bulkDir = Join-Path $Root 'bulk'
$oddDir = Join-Path $Root 'odd'
$exclDir = Join-Path $Root 'excluded'
foreach ($d in @($Root, $baseDir, $bulkDir, $oddDir, $exclDir)) {
    New-Item -ItemType Directory -Path $d -Force | Out-Null
}

function New-TestImage {
    param(
        [string]$Path,
        [int]$Width,
        [int]$Height,
        [System.Drawing.Imaging.ImageFormat]$Format,
        [int]$Seed = 0
    )
    $bmp = New-Object System.Drawing.Bitmap($Width, $Height)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::None
    # 用颜色块而不是纯色：JPEG 压缩后大小才有区分度，也更像真实照片的体积分布。
    $rnd = New-Object System.Random($Seed)
    for ($i = 0; $i -lt 60; $i++) {
        $x = $rnd.Next(0, $Width)
        $y = $rnd.Next(0, $Height)
        $w = $rnd.Next([int]($Width / 12), [int]($Width / 3) + 1)
        $h = $rnd.Next([int]($Height / 12), [int]($Height / 3) + 1)
        $color = [System.Drawing.Color]::FromArgb(255, $rnd.Next(0, 256), $rnd.Next(0, 256), $rnd.Next(0, 256))
        $brush = New-Object System.Drawing.SolidBrush($color)
        $g.FillRectangle($brush, $x, $y, $w, $h)
        $brush.Dispose()
    }
    $g.Dispose()
    $bmp.Save($Path, $Format)
    $bmp.Dispose()
}

Write-Host "==> 生成基准图（不同尺寸，用于验证长宽比筛选）" -ForegroundColor Cyan
# 尺寸刻意跨越长宽比 1.3：横屏、方屏、竖屏各有，正好对应本机的一块竖屏显示器。
$bases = @(
    @{ Name = 'wide_4k.jpg'; W = 3840; H = 2160 },   # 16:9  ~1.78
    @{ Name = 'wide_1440p.png'; W = 2560; H = 1440 }, # 16:9  ~1.78
    @{ Name = 'mild_16_10.jpg'; W = 1920; H = 1200 }, # 1.6
    @{ Name = 'square.jpg'; W = 1000; H = 1000 },     # 1.0
    @{ Name = 'tall.jpg'; W = 1080; H = 1920 },       # 0.5625 竖屏
    @{ Name = 'tall_phone.jpg'; W = 1440; H = 2560 }, # 0.5625 竖屏（贴合本机副屏）
    @{ Name = 'panorama.jpg'; W = 3000; H = 900 },    # 3.33
    @{ Name = 'tiny_icon.png'; W = 48; H = 48 }       # 用于验证尺寸下限筛选
)
foreach ($b in $bases) {
    $path = Join-Path $baseDir $b.Name
    $fmt = if ($b.Name.EndsWith('.png')) { [System.Drawing.Imaging.ImageFormat]::Png } else { [System.Drawing.Imaging.ImageFormat]::Jpeg }
    New-TestImage -Path $path -Width $b.W -Height $b.H -Format $fmt -Seed ($b.W + $b.H)
    '{0,-20} {1,5}x{2,-5} {3,8:N0} B' -f $b.Name, $b.W, $b.H, (Get-Item $path).Length | Write-Host
}

Write-Host ""
Write-Host "==> 生成异常样本（验证「读不了的东西也要被报告」）" -ForegroundColor Cyan

# 1. 真正的损坏文件：扩展名是 .jpg，内容不是图片
$broken = Join-Path $oddDir 'broken_truncated.jpg'
[System.IO.File]::WriteAllBytes($broken, ([byte[]]@(0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00)))
# 2. 纯文本伪装成图片
$fake = Join-Path $oddDir 'fake_image.png'
[System.IO.File]::WriteAllText($fake, "这不是图片，只是一段文本。`r`n")
# 3. 扩展名与内容不符：PNG 内容却叫 .jpg（参考项目实测约一成图片如此）
$mismatch = Join-Path $oddDir 'actually_png.jpg'
New-TestImage -Path $mismatch -Width 800 -Height 600 -Format ([System.Drawing.Imaging.ImageFormat]::Png) -Seed 7
# 4. 不支持的格式：内容是真的 HEIC 头（image crate 无解码器）
$heic = Join-Path $oddDir 'sample.heic'
$heicBytes = New-Object byte[] 64
$heicBytes[0] = 0x00; $heicBytes[1] = 0x00; $heicBytes[2] = 0x00; $heicBytes[3] = 0x18
$ftyp = [System.Text.Encoding]::ASCII.GetBytes('ftyp')
$heicBytes[4] = $ftyp[0]; $heicBytes[5] = $ftyp[1]; $heicBytes[6] = $ftyp[2]; $heicBytes[7] = $ftyp[3]
$heicTag = [System.Text.Encoding]::ASCII.GetBytes('heic')
$heicBytes[8] = $heicTag[0]; $heicBytes[9] = $heicTag[1]; $heicBytes[10] = $heicTag[2]; $heicBytes[11] = $heicTag[3]
[System.IO.File]::WriteAllBytes($heic, $heicBytes)
# 5. 支持的格式但零字节（空文件）
$empty = Join-Path $oddDir 'empty_but_named.webp'
[System.IO.File]::WriteAllBytes($empty, (New-Object byte[] 0))
# 6. 非图片文件，必须完全不计入
[System.IO.File]::WriteAllText((Join-Path $oddDir 'readme.txt'), "文本文件不应被计入图库。`r`n")

Get-ChildItem $oddDir | ForEach-Object { '{0,-24} {1,8:N0} B' -f $_.Name, $_.Length | Write-Host }

Write-Host ""
Write-Host "==> 生成排除规则用的子树" -ForegroundColor Cyan
$exSub = Join-Path $exclDir 'screenshots'
New-Item -ItemType Directory -Path $exSub -Force | Out-Null
for ($i = 1; $i -le 5; $i++) {
    Copy-Item (Join-Path $baseDir 'square.jpg') (Join-Path $exSub ("screenshot_{0:D3}.jpg" -f $i))
}
# 文件名形态刻意做成"正则才拦得住"的样子
for ($i = 1; $i -le 3; $i++) {
    Copy-Item (Join-Path $baseDir 'mild_16_10.jpg') (Join-Path $exclDir ("IMG_2026010{0}_120000.jpg" -f $i))
}
"excluded\screenshots\ 5 个；excluded\IMG_2026... 3 个（共 8 个，用于验证目录/正则排除）" | Write-Host

if ($BulkCount -gt 0) {
    Write-Host ""
    Write-Host "==> 生成 bulk 子树（目标 $BulkCount 个文件）" -ForegroundColor Cyan
    $sources = Get-ChildItem $baseDir -File
    $subCount = 50
    $subs = @()
    for ($i = 0; $i -lt $subCount; $i++) {
        $sub = Join-Path $bulkDir ("set{0:D3}" -f $i)
        New-Item -ItemType Directory -Path $sub -Force | Out-Null
        $subs += $sub
    }

    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $made = 0
    $subIndex = 0
    $inSub = 0
    $perSub = [math]::Ceiling($BulkCount / $subCount)
    $srcIndex = 0
    while ($made -lt $BulkCount) {
        $src = $sources[$srcIndex % $sources.Count]
        $dest = Join-Path $subs[$subIndex] ("img_{0:D6}{1}" -f $made, $src.Extension)
        Copy-Item -LiteralPath $src.FullName -Destination $dest
        $made++
        $inSub++
        $srcIndex++
        if ($inSub -ge $perSub -and $subIndex -lt ($subCount - 1)) {
            $subIndex++
            $inSub = 0
        }
        if ($made % 5000 -eq 0) {
            Write-Host ("  已生成 {0:N0} / {1:N0}（{2:N0} 秒）" -f $made, $BulkCount, $sw.Elapsed.TotalSeconds)
        }
    }
    $sw.Stop()
    Write-Host ("  bulk 完成：{0:N0} 个文件，用时 {1:N1} 秒" -f $made, $sw.Elapsed.TotalSeconds) -ForegroundColor Green
}

Write-Host ""
Write-Host "==> 汇总" -ForegroundColor Cyan
$all = Get-ChildItem -LiteralPath $Root -Recurse -File
$imgLike = $all | Where-Object { $_.Extension -in '.jpg', '.jpeg', '.png', '.bmp', '.webp', '.heic' }
$totalBytes = ($all | Measure-Object -Property Length -Sum).Sum
"根目录        : $Root"
"全部文件      : $($all.Count) 个（$([math]::Round($totalBytes / 1MB, 1)) MB）"
"图片类扩展名  : $($imgLike.Count) 个"
"其中 base     : $((Get-ChildItem $baseDir -File).Count)"
"其中 bulk     : $((Get-ChildItem $bulkDir -Recurse -File).Count)"
"其中 odd      : $((Get-ChildItem $oddDir -File).Count)（含 1 个 .txt，应完全不计入）"
"其中 excluded : $((Get-ChildItem $exclDir -Recurse -File).Count)"
Write-Host ""
Write-Host "扫描预期（不设排除规则时）：" -ForegroundColor Yellow
"  候选文件 = 图片类扩展名合计 $($imgLike.Count) 个" | Write-Host
"  其中应被计入 ok=0 / skipped：broken_truncated.jpg、fake_image.png、empty_but_named.webp、sample.heic" | Write-Host
"  readme.txt 不应出现在任何统计里" | Write-Host
