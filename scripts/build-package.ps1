<#
.SYNOPSIS
    构建 WindowsWallPaper 的可直接运行文件夹。

.DESCRIPTION
    按设计规格 §3.1：不产出安装包、不产出压缩包，只产出一个可以整体拷贝走的文件夹
    release/WindowsWallPaper/。

    必须用 `tauri build --no-bundle` 而不是 `cargo build --release`：
    后者不会把前端产物嵌进可执行文件，运行起来只会看到 WebView2 的"无法访问此页"。
    这一点已经实测确认过——裸 cargo build 的 exe 里搜不到前端 bundle 的文件名。

    步骤：
      1. bun run tauri build --no-bundle（内部会执行前端构建，并嵌入前端资源）
      2. 把 exe 与必要资源复制进 release/WindowsWallPaper/
      3. 校验 exe 里确实含前端资源，避免又产出"白窗口"版本
      4. 报告产物清单与大小

    脚本不会删除 release 目录下已有的 config/、data/、logs/、webview2/，
    以免把正在使用的配置和索引删掉。要一份干净的交付产物时加 -CleanData：
    它会清掉运行时数据（图库索引与壁纸缓存，可重建），但**保留 config/**，
    因为里面是用户自己调的设置（每屏筛选、间隔等），重建不出来。
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [switch]$CleanData
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
# 工程根就是前端工程根：package.json、index.html、src-tauri 都在这里。
# （`src-tauri` 这个名字是 Tauri 的约定，改不了；它不是"src 目录下的 tauri"。）
$desktop = $root
$srcTauri = Join-Path $root 'src-tauri'
$outDir = Join-Path $root 'release\WindowsWallPaper'
# Cargo 的产物名来自包名（kebab-case），交付时改名为产品名。
# cargo 不允许包名里出现大写，因此这一步重命名是必须的，不是可选的美化。
$cargoExe = 'windows-wallpaper.exe'
$exeName = 'WindowsWallPaper.exe'

function Write-Step([string]$text) {
    Write-Host ""
    Write-Host "==> $text" -ForegroundColor Cyan
}

function Assert-Path([string]$path, [string]$what) {
    if (-not (Test-Path -LiteralPath $path)) {
        throw "缺少$what：$path"
    }
}

if (-not (Test-Path -LiteralPath $srcTauri)) {
    throw "找不到 src-tauri 目录：$srcTauri"
}

# --- 1. 构建（前端由 tauri build 的 beforeBuildCommand 负责）--------------
if (-not $SkipBuild) {
    Write-Step '构建（bun run tauri build --no-bundle）'
    Push-Location $desktop
    try {
        & bun run tauri build --no-bundle
        if ($LASTEXITCODE -ne 0) { throw "tauri build 失败（退出码 $LASTEXITCODE）" }
    }
    finally {
        Pop-Location
    }
}

$builtExe = Join-Path $srcTauri "target\release\$cargoExe"
Assert-Path $builtExe '编译产物'

# mainBinaryName 必须与 Cargo 产物名一致，否则 Tauri 运行时会找不到自己的可执行文件。
$confPath = Join-Path $srcTauri 'tauri.conf.json'
$conf = Get-Content -LiteralPath $confPath -Raw | ConvertFrom-Json
if ($conf.mainBinaryName -ne [System.IO.Path]::GetFileNameWithoutExtension($cargoExe)) {
    throw "tauri.conf.json 的 mainBinaryName（$($conf.mainBinaryName)）与 Cargo 产物名（$cargoExe）不一致，请先修正。"
}

# --- 2. 校验前端确实被嵌入 ------------------------------------------------
Write-Step '校验前端资源已嵌入可执行文件'
$distDir = Join-Path $desktop 'dist\assets'
$bundleScript = Get-ChildItem -LiteralPath $distDir -Filter 'index-*.js' -ErrorAction SilentlyContinue |
    Select-Object -First 1
if ($null -eq $bundleScript) {
    throw "找不到前端 bundle（$distDir\index-*.js），前端没有构建成功。"
}
$bundleBase = [System.IO.Path]::GetFileNameWithoutExtension($bundleScript.Name)  # 例如 index-BUYRQ8j3

$contents = [System.IO.File]::ReadAllText($builtExe, [System.Text.Encoding]::UTF8)
if (-not $contents.Contains($bundleBase)) {
    throw @"
可执行文件里搜不到前端 bundle 名（$bundleBase），说明前端资源没有被嵌入。
这通常是因为用了 `cargo build --release` 而不是 `tauri build`。
后果：程序能启动，但窗口里只会显示 WebView2 的"无法访问此页"。
"@
}
Write-Host "已确认嵌入：$($bundleScript.Name)"

# --- 2a. 校验前端调用的每个命令名都在 generate_handler! 里 ------------------
#
# **这一步为什么必须有**（1.13.0 第一次交付就栽在这里）：
#
# `invoke("命令名")` 里的那个字符串，一边是前端的普通字符串、一边是后端
# `generate_handler![commands::xxx]` 注册的标识符。**两边没有任何编译期联系**：
#   - `tsc` 把它当普通字符串，改名漏了它照样通过；
#   - Rust 侧只看得到自己注册了什么，不知道谁在调；
#   - 两端不一致的表现是运行时一句 `Command xxx not found`——界面上的结果
#     是"点了没反应"或一句红字，只有用户点下去才会发现。
#
# 1.13.0 把模式从 SAFE 改名成 NSFW 时，后端命令改成了 `playback_nsfw`，
# 而 `frontend/api.ts` 里那一行漏改，于是"点 NSFW 按钮"整个失效（用户截图）。
# 那次两个闸都没有：`tsc` 查不出字符串，排版自检也没点到这个按钮。
# 因此这里加一道**构建期**检查（交付版必须经这个脚本，见文件头）。
Write-Step '校验前端命令名与后端注册表一致'
$apiPath = Join-Path $desktop 'frontend\api.ts'
Assert-Path $apiPath '前端 api.ts'
$apiText = Get-Content -LiteralPath $apiPath -Raw
# 只看 `invoke<类型>("名字")` 与 `invoke("名字")` 这两种写法——命令名只可能是它们。
$frontendCommands = [regex]::Matches($apiText, 'invoke(?:<[^>]*>)?\(\s*"([a-z0-9_]+)"') |
    ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique
$mainText = Get-Content -LiteralPath (Join-Path $srcTauri 'src\main.rs') -Raw
$handlerBlock = [regex]::Match($mainText, '(?s)generate_handler!\[(.*?)\]').Groups[1].Value
if ([string]::IsNullOrWhiteSpace($handlerBlock)) {
    throw '在 src-tauri/src/main.rs 里找不到 generate_handler![...] 注册表。'
}
$registeredCommands = [regex]::Matches($handlerBlock, 'commands::([a-z0-9_]+)') |
    ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique
$missing = @($frontendCommands | Where-Object { $_ -notin $registeredCommands })
if ($missing.Count -gt 0) {
    throw @"
前端调用了后端没有注册的命令（${($missing -join '、')}）。

后果：那个按钮/那一页点下去只会得到 `Command xxx not found`，而编译、类型检查、
打包都不会报错——只有用户点下去才会发现（1.13.0 的 NSFW 按钮就是这样失效的）。

改法：要么把 main.rs 的 generate_handler! 里的注册名改成一致，要么把
frontend/api.ts 里那一行改对。两边必须**逐字一致**（含下划线）。
前端共调用 $($frontendCommands.Count) 个命令，后端注册 $($registeredCommands.Count) 个。
"@
}
Write-Host "  已确认 $($frontendCommands.Count) 个前端命令全部已注册"

# --- 2b. 校验 capability 授权存在 -----------------------------------------
#
# **这一步为什么必须有**：Tauri 2 把"前端能做什么"分成两类，要求不同——
#   - 命令（invoke）：由 generate_handler! 注册，**不需要** capability；
#   - 事件（listen）：属于核心插件，**必须**在 capability 里授权。
#
# 缺了 capability 时，invoke 一切正常、listen 被静默拒绝。项目曾经因此出现
# "右上角扫描浮窗正常、图库页进度条永远停在正在准备"这种一半好一半坏的状态，
# 而且因为前端当时没写 catch，失败完全无声——从界面上根本看不出原因。
#
# 前端现在已经统一到一个数据来源、并补上了成败日志；这里再加一道构建期检查，
# 避免 capabilities 目录被误删后又不声不响地退化成同一个问题。
Write-Step '校验 capability 授权（事件订阅依赖它）'
$capDir = Join-Path $srcTauri 'capabilities'
$capFiles = @(Get-ChildItem -LiteralPath $capDir -Filter '*.json' -ErrorAction SilentlyContinue)
if ($capFiles.Count -eq 0) {
    throw @"
缺少 capabilities 目录（或其中没有任何 .json）。

后果：前端的事件订阅（listen）会被 Tauri 拒绝，而 invoke 命令照常工作。
表现为"有些界面元素正常、有些永远不更新"，且失败无声——很难从现象反推原因。
至少需要给主窗口授权 core:event:allow-listen（见 src-tauri/capabilities/default.json）。
"@
}
$capText = $capFiles | ForEach-Object { Get-Content -LiteralPath $_.FullName -Raw } | Out-String
if ($capText -notmatch 'core:event:allow-listen') {
    throw @"
capabilities 里没有 core:event:allow-listen。

后果：前端 listen() 被拒绝（invoke 不受影响），事件驱动的界面永远不更新。
请把这一条加进 $($capFiles[0].Name) 的 permissions。
"@
}
Write-Host "  已确认事件订阅权限：$($capFiles.Name -join '、')"

# --- 3. 组装交付文件夹 ----------------------------------------------------
Write-Step "组装交付文件夹 $outDir"
if (-not (Test-Path -LiteralPath $outDir)) {
    New-Item -ItemType Directory -Path $outDir -Force | Out-Null
}

# 可选：清掉运行时数据，得到一份干净的交付产物。
#
# `data/` 里是图库索引与壁纸缓存（每张 BMP 十几 MB，跑一轮几百 MB），
# 它们都能由程序自己重建；`logs/` 与 `webview2/` 同理。
# **`config/` 必须保留**：里面是用户自己调的设置（每屏筛选、间隔、
# 顺序），程序重建不出来——删掉等于把用户调好的东西清空。
if ($CleanData) {
    Write-Step '清理运行时数据（保留 config/）'
    foreach ($name in 'data', 'logs', 'webview2', 'wwp-data-location.txt') {
        $target = Join-Path $outDir $name
        if (Test-Path -LiteralPath $target) {
            $size = (Get-ChildItem -LiteralPath $target -Recurse -File -ErrorAction SilentlyContinue |
                Measure-Object -Property Length -Sum).Sum
            Remove-Item -LiteralPath $target -Recurse -Force
            Write-Host ("  已清理 {0}（{1:N1} MB）" -f $name, ($size / 1MB))
        }
    }
}

Copy-Item -LiteralPath $builtExe -Destination (Join-Path $outDir $exeName) -Force

# 图标：Windows 只用到 .ico 与几个 png，移动端与商店用不到的尺寸不带。
$iconSrc = Join-Path $srcTauri 'icons'
$iconDst = Join-Path $outDir 'icons'
if (Test-Path -LiteralPath $iconSrc) {
    if (-not (Test-Path -LiteralPath $iconDst)) {
        New-Item -ItemType Directory -Path $iconDst -Force | Out-Null
    }
    $keep = @('icon.ico', 'icon.png', '32x32.png', '64x64.png', '128x128.png', '128x128@2x.png')
    foreach ($name in $keep) {
        $from = Join-Path $iconSrc $name
        if (Test-Path -LiteralPath $from) {
            Copy-Item -LiteralPath $from -Destination (Join-Path $iconDst $name) -Force
        }
    }
}

# --- 4. 报告 --------------------------------------------------------------
Write-Step '产物清单'
$files = Get-ChildItem -LiteralPath $outDir -Recurse -File
$totalBytes = ($files | Measure-Object -Property Length -Sum).Sum
foreach ($file in $files | Sort-Object FullName) {
    $rel = $file.FullName.Substring($outDir.Length).TrimStart('\')
    '{0,-42} {1,10:N0} B' -f $rel, $file.Length | Write-Host
}
Write-Host ""
Write-Host ("共 {0} 个文件，{1:N1} MB" -f $files.Count, ($totalBytes / 1MB)) -ForegroundColor Green

Write-Host ""
Write-Host "下一步验证（M1 的硬验收项）：" -ForegroundColor Yellow
Write-Host "  1. 运行 $outDir\$exeName --diagnose，确认“数据位置来源”为 program-dir"
Write-Host "  2. 双击该 exe，界面应正常显示（不是 WebView2 的“无法访问此页”）"
Write-Host "  3. 把整个 release\WindowsWallPaper 文件夹拷到别的位置再运行，"
Write-Host "     确认仍然显示“程序目录（便携模式）”，且文件夹里出现 config\、logs\、webview2\"
Write-Host ""
Write-Host "注意：在 $outDir 里运行 exe 会让它在同目录生成 config/、data/、logs/、webview2/。" -ForegroundColor DarkYellow
Write-Host "     交付前若要保持文件夹干净，先删掉这四个目录与 wwp-data-location.txt。" -ForegroundColor DarkYellow
