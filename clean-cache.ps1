#Requires -Version 5.1
<#
.SYNOPSIS
    一键清理 WindowsWallPaper 项目的**过程缓存**（构建产物 + 运行时缓存 + 临时数据）。

.DESCRIPTION
    清理的都是"跑一次就会自己回来"的东西，分三类：

      构建产物    src-tauri\target\（cargo 的 debug/release 中间产物，通常几十 GB）、
                  dist\、layout-check-dist\、layout-check-shots\、src-tauri\gen\、
                  node_modules\.vite\
      运行时缓存  src-tauri\ 与 release\WindowsWallPaper\ 下的 data\output\、
                  data\wallpaper-thumbs\、data\trash-thumbs\、data\lockscreen-probe\、
                  logs\、webview2\、wwp-data-location.txt
      临时数据    tmp\、项目根目录的 *.log

    **故意不清理**（删了要么重建不出来，要么是用户数据）：
      config\（设置）、data\index\（图库索引，重建要重扫几万个文件）、
      data\trash\（回收站里的图）、data\*.json（播放位置 / 窗口大小 / NSFW 状态）、
      ref\（只读参考项目）、release\rollback\（回滚用的旧版本 exe）。

    只删**下面那张清单里写着的**路径，并且每一条都先校验"在项目根内、不是受保护目录"。
    这是刻意的：清理是删除，不能靠通配符去"猜"哪些是垃圾。

    与程序里的「清理程序目录」（设置页那张卡片，`wwp-core::cleanup`，见 AGENTS.md §10.45）
    不是同一件事：那个在程序内部、只清运行时数据；这个在开发机上、还要清构建缓存。
    共同的原则只有一条——只删"跑一次就会自己回来"的东西。

.PARAMETER KeepTarget
    保留 src-tauri\target\（Rust 编译缓存，本机曾到 40 GB 上下）。只想快速清掉零碎缓存、
    又不想让下一次编译从头开始时用它。

.PARAMETER IncludeNodeModules
    连 node_modules\ 一起删。它**不是缓存**而是已安装的依赖，删掉后要 `bun install`
    （需要网络）才能继续开发，因此默认不动。

.PARAMETER Force
    即使检测到项目里的程序正在运行也继续。默认会停下来说明原因：运行中的实例锁着
    data\、logs\、webview2\ 与 exe，删一半只会得到一个坏状态（缓存目录残缺、日志句柄失效）。

.PARAMETER NoPause
    结束后不停下来等回车。默认只在**交互式控制台**里停（资源管理器右键
    「使用 PowerShell 运行」打开的窗口就是）；从脚本或管道调用时 stdin 是重定向的，
    不会停，因此一般不用加它。

.EXAMPLE
    pwsh -File .\clean-cache.ps1
    清理全部过程缓存（含 src-tauri\target\）。

.EXAMPLE
    pwsh -File .\clean-cache.ps1 -WhatIf
    只列出会删什么、能释放多少，不删任何东西。第一次跑建议先这样看一眼。

.EXAMPLE
    pwsh -File .\clean-cache.ps1 -KeepTarget
    保留 Rust 编译缓存，只清零碎产物、运行时缓存与临时数据。

.NOTES
    为什么不用 `git clean -fdx` 一把梭：它会连 ref\（只读参考项目，105 MB，删了得重新找）、
    src-tauri\config\（设置）、src-tauri\data\index\（图库索引，重建要重扫几万个文件）、
    data\trash\（回收站里的图）一起删掉——这些**都不是缓存**。
    所以这里只认一张写死的清单，且清单之外的东西一律不碰。

    这个脚本在 Windows PowerShell 5.1 与 PowerShell 7 下都能跑：资源管理器右键
    「使用 PowerShell 运行」用的是系统自带的 **5.1**（不是 pwsh 7），所以不能写
    `#Requires -Version 7.0`——那样脚本会被直接拒绝、窗口一闪就关，看起来像"没反应"。
#>
[CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = 'Medium')]
param(
    [switch]$KeepTarget,
    [switch]$IncludeNodeModules,
    [switch]$Force,
    [switch]$NoPause
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
# 删几万个文件时进度条本身就是开销，而且没人盯着看。
$ProgressPreference = 'SilentlyContinue'

# --- 让"右键 → 使用 PowerShell 运行"看得见结果 -----------------------------
# 那个右键动词是 powershell.exe（5.1）跑 `-file 脚本`，脚本一结束窗口立刻关掉：
# 出错信息、清理汇总都来不及看。因此在**交互式控制台**里停一下等回车。
# 从别的脚本/管道调用时 stdin 是重定向的，不会停——自动化照样能跑。
function Stop-IfInteractive {
    if ($NoPause) { return }
    if ($Host.Name -ne 'ConsoleHost') { return }
    $redirected = $true
    try { $redirected = [Console]::IsInputRedirected } catch { $redirected = $true }
    if ($redirected) { return }
    Write-Host ''
    Read-Host '按回车键关闭窗口' | Out-Null
}

# 顶层出错（找不到项目根、清单校验没过等）也要看得见：兜住、显示、停住、以 1 退出。
trap {
    Write-Host ''
    Write-Host ("出错了：{0}" -f $_.Exception.Message) -ForegroundColor Red
    Stop-IfInteractive
    exit 1
}

# --- 定位项目根 -----------------------------------------------------------
# 脚本就放在项目根目录，所以项目根 = 脚本所在目录。用三个入口文件核对一遍：
# 万一有人把它拷到别处运行，下面那套"必须在项目根内"的校验就保护不到任何东西了。
$projectRoot = $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($projectRoot)) {
    throw '拿不到脚本所在目录（$PSScriptRoot 为空），无法定位项目根。'
}
$projectRoot = [System.IO.Path]::GetFullPath($projectRoot).TrimEnd(
    [System.IO.Path]::DirectorySeparatorChar,
    [System.IO.Path]::AltDirectorySeparatorChar)
foreach ($marker in 'package.json', 'AGENTS.md', 'src-tauri\Cargo.toml') {
    if (-not (Test-Path -LiteralPath (Join-Path $projectRoot $marker))) {
        throw "这个脚本必须放在项目根目录运行：找不到 $marker（$projectRoot）。"
    }
}
$rootPrefix = $projectRoot + [System.IO.Path]::DirectorySeparatorChar

# 这些路径段一旦出现在待删路径里就拒绝——它们是源码、设置或用户数据。
# 它与下面那张清单是两道**独立**的闸：清单哪天写错了，这里还能挡住。
# 注意：判断的是**整段**，因此 `trash-thumbs`（缩略图，可重建）不会被 `trash` 误伤。
$protectedSegments = @('config', 'index', 'trash', 'ref', '.git')

function Write-Step([string]$Text) {
    Write-Host ''
    Write-Host "==> $Text" -ForegroundColor Cyan
}

function Format-Size([long]$Bytes) {
    if ($Bytes -lt 1KB) { return "$Bytes B" }
    if ($Bytes -lt 1MB) { return ('{0:N1} KB' -f ($Bytes / 1KB)) }
    if ($Bytes -lt 1GB) { return ('{0:N1} MB' -f ($Bytes / 1MB)) }
    return ('{0:N2} GB' -f ($Bytes / 1GB))
}

# 项目里正在跑的程序（只认**可执行文件在项目内**的）。release\WindowsWallPaper
# 被整体拷到别处去运行时不在项目内，那种情况不影响清理项目里的缓存。
function Get-ProjectRunningPrograms {
    $found = [System.Collections.Generic.List[object]]::new()
    foreach ($name in 'WindowsWallPaper', 'windows-wallpaper') {
        foreach ($process in @(Get-Process -Name $name -ErrorAction SilentlyContinue)) {
            $path = $null
            try { $path = $process.Path } catch { $path = $null }  # 权限不足时 Path 会抛
            if ($null -ne $path -and
                $path.StartsWith($rootPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
                $found.Add([pscustomobject]@{ Id = $process.Id; Path = $path })
            }
        }
    }
    return $found
}

# 把相对路径解析成绝对路径，并确认它"可以删"。
function Assert-DeletablePath([string]$RelativePath) {
    $full = [System.IO.Path]::GetFullPath((Join-Path $projectRoot $RelativePath))
    if (-not $full.StartsWith($rootPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "拒绝删除项目目录之外的路径：$RelativePath（解析为 $full）"
    }
    $segments = $RelativePath -split '[\\/]'
    foreach ($segment in $segments) {
        if ($segment -eq '..' -or $segment -eq '.') {
            throw "删除路径里不允许出现 . 或 ..：$RelativePath"
        }
        if ($segment -in $protectedSegments) {
            throw "拒绝删除受保护的目录「$segment」（设置 / 索引 / 回收站 / 参考项目）：$RelativePath"
        }
    }
    if ($segments[0] -eq 'node_modules' -and -not $IncludeNodeModules -and $RelativePath -ne 'node_modules\.vite') {
        throw "没有加 -IncludeNodeModules，却要删 node_modules 里的东西：$RelativePath"
    }
    return $full
}

# 量一项的体积与文件数。不存在的返回 $null（调用方据此跳过，不显示"清理 0 B"）。
function Measure-CachePath([string]$FullPath, [string]$Kind) {
    if ($Kind -eq 'RootLogs') {
        $logs = @(Get-ChildItem -LiteralPath $projectRoot -File -Filter '*.log' -Force -ErrorAction SilentlyContinue)
        $bytes = [long]0
        foreach ($log in $logs) { $bytes += $log.Length }
        return [pscustomobject]@{ Bytes = $bytes; Files = [long]$logs.Count }
    }
    if (-not (Test-Path -LiteralPath $FullPath)) { return $null }
    if ($Kind -eq 'File') {
        $file = Get-Item -LiteralPath $FullPath -Force
        return [pscustomobject]@{ Bytes = [long]$file.Length; Files = [long]1 }
    }
    $files = @(Get-ChildItem -LiteralPath $FullPath -Recurse -File -Force -ErrorAction SilentlyContinue)
    $bytes = [long]0
    foreach ($file in $files) { $bytes += $file.Length }
    return [pscustomobject]@{ Bytes = $bytes; Files = [long]$files.Count }
}

# --- 可清理清单（**唯一**的定义处；界面、文档都从这里抄）-------------------
$items = [System.Collections.Generic.List[object]]::new()

function Add-CacheItem([string]$Group, [string]$Label, [string]$RelativePath, [string]$Kind) {
    $items.Add([pscustomobject]@{
            Group = $Group
            Label = $Label
            Path  = $RelativePath
            Kind  = $Kind
        })
}

# ① 构建产物。target 是本机最大的一项；`-KeepTarget` 只跳过它，别的照清。
if (-not $KeepTarget) {
    Add-CacheItem '构建产物' 'Rust 编译缓存' 'src-tauri\target' 'Dir'
}
Add-CacheItem '构建产物' '前端构建产物' 'dist' 'Dir'
Add-CacheItem '构建产物' '排版自检产物' 'layout-check-dist' 'Dir'
Add-CacheItem '构建产物' '排版自检截图' 'layout-check-shots' 'Dir'
Add-CacheItem '构建产物' 'Tauri 生成的 schema' 'src-tauri\gen' 'Dir'
Add-CacheItem '构建产物' 'Vite 依赖预构建缓存' 'node_modules\.vite' 'Dir'

# ② 运行时缓存。两个"程序目录"：开发时用的 src-tauri\、交付版的 release\WindowsWallPaper\。
#    只删这些**能重建**的子目录；data\index\ 与 config\ 一个字节都不碰。
foreach ($programDir in 'src-tauri', 'release\WindowsWallPaper') {
    if (-not (Test-Path -LiteralPath (Join-Path $projectRoot $programDir))) { continue }
    Add-CacheItem '运行时缓存' '壁纸缓存' "$programDir\data\output" 'Dir'
    Add-CacheItem '运行时缓存' '预览缩略图' "$programDir\data\wallpaper-thumbs" 'Dir'
    Add-CacheItem '运行时缓存' '回收站缩略图（不碰回收站里的图）' "$programDir\data\trash-thumbs" 'Dir'
    Add-CacheItem '运行时缓存' '锁屏探针残留' "$programDir\data\lockscreen-probe" 'Dir'
    Add-CacheItem '运行时缓存' '运行日志与崩溃报告' "$programDir\logs" 'Dir'
    Add-CacheItem '运行时缓存' 'WebView2 缓存' "$programDir\webview2" 'Dir'
    Add-CacheItem '运行时缓存' '数据位置提示文件' "$programDir\wwp-data-location.txt" 'File'
}

# ③ 临时数据。
Add-CacheItem '临时数据' '测试图库等临时目录' 'tmp' 'Dir'
Add-CacheItem '临时数据' '根目录临时日志' '*.log' 'RootLogs'
if ($IncludeNodeModules) {
    Add-CacheItem '临时数据' '前端依赖（删后需 bun install）' 'node_modules' 'Dir'
}

# --- 预检：程序在跑就先停 ------------------------------------------------
$running = @(Get-ProjectRunningPrograms)
if ($running.Count -gt 0) {
    $list = ($running | ForEach-Object { "PID $($_.Id)  $($_.Path)" }) -join '; '
    if ($Force -or $WhatIfPreference) {
        Write-Warning "项目里有程序正在运行（$list）。它锁着 data\、logs\、webview2\ 与 exe，运行时删除会失败或留下坏状态。"
    }
    else {
        throw @"
检测到项目里的程序正在运行：$list
运行中的实例锁着 data\、logs\、webview2\ 与 exe，删到一半只会得到一个坏状态。
请先退出程序再重跑；确实要强行清理时加 -Force。
"@
    }
}

# --- 统计 ----------------------------------------------------------------
Write-Step '统计过程缓存'
$plan = [System.Collections.Generic.List[object]]::new()
foreach ($item in $items) {
    if ($item.Kind -eq 'RootLogs') {
        $size = Measure-CachePath '' 'RootLogs'
        if ($size.Files -eq 0) { continue }
        $plan.Add([pscustomobject]@{ Item = $item; FullPath = $null; Bytes = $size.Bytes; Files = $size.Files })
        continue
    }
    $full = Assert-DeletablePath $item.Path
    $size = Measure-CachePath $full $item.Kind
    if ($null -eq $size) { continue }
    $plan.Add([pscustomobject]@{ Item = $item; FullPath = $full; Bytes = $size.Bytes; Files = $size.Files })
}

$groupOrder = '构建产物', '运行时缓存', '临时数据'
foreach ($group in $groupOrder) {
    $entries = @($plan | Where-Object { $_.Item.Group -eq $group })
    if ($entries.Count -eq 0) { continue }
    Write-Host ''
    Write-Host "  [$group]" -ForegroundColor DarkGray
    foreach ($entry in $entries) {
        Write-Host ("    {0}  ·  {1}  ·  {2:N0} 个文件  ·  {3}" -f
            $entry.Item.Label, (Format-Size $entry.Bytes), $entry.Files, $entry.Item.Path)
    }
}

$totalBytes = [long]0
$totalFiles = [long]0
foreach ($entry in $plan) {
    $totalBytes += $entry.Bytes
    $totalFiles += $entry.Files
}

if ($plan.Count -eq 0) {
    Write-Host ''
    Write-Host '没有找到可清理的过程缓存（项目已经挺干净了）。' -ForegroundColor Green
    Stop-IfInteractive
    return
}

Write-Host ''
Write-Host ("  共 {0} 项，{1}，{2:N0} 个文件" -f $plan.Count, (Format-Size $totalBytes), $totalFiles) -ForegroundColor Yellow
Write-Host '  保留：config\（设置）、data\index\（图库索引）、data\trash\（回收站）、data\*.json（播放位置等）、ref\、release\rollback\' -ForegroundColor DarkGray

if ($KeepTarget -and (Test-Path -LiteralPath (Join-Path $projectRoot 'src-tauri\target'))) {
    Write-Host '  -KeepTarget：src-tauri\target\ 保留不动（下一次编译仍是增量的）。' -ForegroundColor DarkGray
}

if ($WhatIfPreference) {
    Write-Host ''
    Write-Host '（-WhatIf：以上只列出，不删除任何文件。）' -ForegroundColor Yellow
    Stop-IfInteractive
    return
}

# --- 删除 ----------------------------------------------------------------
Write-Step '删除'
$drive = [System.IO.DriveInfo]::new([System.IO.Path]::GetPathRoot($projectRoot))
$freeBefore = $drive.AvailableFreeSpace
$freed = [long]0
$failed = [System.Collections.Generic.List[object]]::new()
$removedTarget = $false

foreach ($entry in $plan) {
    if (-not $PSCmdlet.ShouldProcess($entry.Item.Path, '删除缓存')) { continue }
    Write-Host ("  删除 {0} … " -f $entry.Item.Label) -NoNewline
    try {
        switch ($entry.Item.Kind) {
            'RootLogs' {
                # 只删项目根**这一层**的 *.log，不递归——src-tauri\logs 是单独一项。
                $logs = @(Get-ChildItem -LiteralPath $projectRoot -File -Filter '*.log' -Force -ErrorAction SilentlyContinue)
                foreach ($log in $logs) {
                    Remove-Item -LiteralPath $log.FullName -Force -ErrorAction Stop
                }
            }
            'File' {
                Remove-Item -LiteralPath $entry.FullPath -Force -ErrorAction Stop
            }
            default {
                Remove-Item -LiteralPath $entry.FullPath -Recurse -Force -ErrorAction Stop
            }
        }
        $freed += $entry.Bytes
        if ($entry.Item.Path -eq 'src-tauri\target') { $removedTarget = $true }
        Write-Host ('释放 {0}' -f (Format-Size $entry.Bytes)) -ForegroundColor Green
    }
    catch {
        $failed.Add([pscustomobject]@{
                Label = $entry.Item.Label
                Path  = $entry.Item.Path
                Error = $_.Exception.Message
            })
        Write-Host ("失败：{0}" -f $_.Exception.Message) -ForegroundColor Red
    }
}

# --- 汇总 ----------------------------------------------------------------
$freeAfter = $drive.AvailableFreeSpace
Write-Step '汇总'
Write-Host ("  已清理 {0} 项，释放 {1}" -f ($plan.Count - $failed.Count), (Format-Size $freed)) -ForegroundColor Green
if ($freeAfter -ge $freeBefore) {
    Write-Host ("  磁盘可用空间：{0} → {1}（+{2}）" -f
        (Format-Size $freeBefore), (Format-Size $freeAfter), (Format-Size ($freeAfter - $freeBefore)))
}
else {
    # 期间有别的东西在写盘时会出现负数——照样报出来，但不冒充"释放了"。
    Write-Host ("  磁盘可用空间：{0} → {1}" -f (Format-Size $freeBefore), (Format-Size $freeAfter))
}

if ($failed.Count -gt 0) {
    Write-Host ''
    Write-Host '  以下项没能删掉（通常是文件正被占用或权限不足）：' -ForegroundColor Yellow
    foreach ($failure in $failed) {
        Write-Host ("    {0}  ·  {1}" -f $failure.Label, $failure.Path) -ForegroundColor Yellow
        Write-Host ("      {0}" -f $failure.Error) -ForegroundColor DarkYellow
    }
    Write-Host ''
    Write-Host '  先退出占用它的程序，再重跑一次即可（已经删掉的部分不用重来）。' -ForegroundColor Yellow
    Stop-IfInteractive
    exit 1
}

if ($removedTarget) {
    Write-Host '  提示：target\ 已删，下一次 cargo build / cargo test 会从头编译（几分钟），这是正常的。' -ForegroundColor DarkYellow
}
Write-Host '  完成。' -ForegroundColor Green
Stop-IfInteractive
