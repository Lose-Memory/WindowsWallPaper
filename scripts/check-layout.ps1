<#
.SYNOPSIS
    界面排版自检：六个页面各截一张图，并跑十条客观判据。

.DESCRIPTION
    ## 为什么需要它

    这个程序是 Tauri + WebView2 的，核对界面排版本来只有两条路，都很贵：

      1. 把程序跑起来截图——但**单实例门闩**会让它在用户那份正跑着时起不来；
      2. 想切到"图库/列表/设置"页——**WebView2 不接受合成点击**（AGENTS §9），
         只能临时改 App.tsx 的默认页再重新构建，一轮几分钟。

    这里绕开两者：不启动 Rust 后端，而是在普通浏览器（Chrome/Edge）里把前端
    的 `App` 渲染出来，给 `window.__TAURI_INTERNALS__.invoke` 装一个返回固定
    数据的假后端，切页用 DOM 的 `click()`（真浏览器里有效）。

    ## 它证明什么、不证明什么

    **证明**：排版（有没有折行、溢出、列宽错位）、以及"按钮点下去界面上会发生
    什么"这类接线问题。十条判据是**数字**，不是人眼看图：

      1. 开关联图库之后名字输入框的宽度变化必须是 0（对照组会打印出 -15px 的
         真实差异，说明这条判据确实在守着一个真问题）；
      2. 拖拽排序：往下拖 / 往上拖之后列表顺序必须真的变，而且位置对；
      3. 「删除当前壁纸」按钮存在、可点、点了会弹后端返回的那句话；
      4. 回收站相册页：条目、两个按钮、缩略图、顶部「全部删除」都在，
         旧版条目（没有原位置）的「恢复」是禁用的；点缩略图会去打开原图、
          点文件名会打开它原来所在的文件夹并选中它（卡片上不再有"原位置"那一行），
          点「全部删除」会出现进度条并跟着后端推来的进度走（0% → 50% → 100%）；
      5. 播放页每块屏的壁纸预览（当前 / 下一张）：两格并排、宽度相等、
         缩略图真的解码出来了、「下一张」带倒计时、格子内部不溢出；
         另外两个对照组——藏起预览能看出它占了多少高度，某块屏"筛完一张不剩"时
         倒计时必须消失（否则就是"写着 21 秒后却永远不换纸"）；
         **2026-10 追加两条**：①预览生成不出来时那句说明必须**完整显示**
         （`ERR_CLIPPED=0`，对照组按改之前的 `nowrap` 写法必须测出被截断）；
         ②**右键**点那行路径 = 复制文件名（不含扩展名），左键那件事不受影响；
      6. 六个页面都没有横向溢出、没有被挤扁的不可折行文字（对照组把那些文字
         放大到 34px，必须能测出 8 处被挤扁的地方）；
      7. 播放页每屏设置的排版：三个方框（播放列表 / 固定单图 / NSFW 壁纸）等宽、
         数字块的数值跟在标签后面且不被裁；「已切换 / 候选 / 图像比例筛选」
          三条信息**并成一行**，而且那三个词已经从界面上消失（`STAT_KEYS=0`）；
         该模式下那个按钮写着 **NSFW**，整页文字里一个 **SAFE** 都不该剩
         （`NSFW_BUTTON=1` / `SAFE_TEXT_LEFT=0`）；
      8. 右上角浮层：两个提示（一次性提示 + 扫描状态）同时出现、都取最坏情况时，
         仍然**贴在窗口最右上角**，而且**没有一张压住「重新加载」**；
      9. 设置页「清理程序目录」：**默认展开**（一进页面就有清单）、点「收起」
         会**写回设置**（假后端里读得到）、再点一下又展开、不能清的那一项是禁用的；
         另一半是"记忆"——`?cleanupfolded=1`（相当于用户上次折叠过）时
         页面一打开就不该有清单；
     10. **点 NSFW 按钮真的调到了后端**：假后端收到的命令名必须是 `playback_nsfw`
         （`NSFW_CLICK_CALLS=1` / `NSFW_CLICK_CMD`），并且把后端那句话浮出来。
         这一条是 1.13.1 补的——1.13.0 改名时 `frontend/api.ts` 里那行命令名字符串
         漏改，点按钮只会得到 `Command playback_safe not found`，而当时的九条判据
         只数了"有一个写着 NSFW 的按钮"、没有点它。同一趟还顺带静态比对了
         "前端调用的每个命令名都在 `generate_handler!` 里"（构建脚本也有同一道闸）。

    **不证明**：真机行为（设壁纸、托盘、COM、WebView2 的渲染）。那些仍然只能
    跑真程序 + 看 `logs\app.log`。

.PARAMETER SkipBuild
    跳过前端构建（刚构建过、只想重截图时用）。

.PARAMETER NoShots
    只跑十条判据，不截图。
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [switch]$NoShots
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
$distHtml = Join-Path $root 'layout-check-dist\layout-check\index.html'
$shots = Join-Path $root 'layout-check-shots'
$profile = Join-Path $env:TEMP 'wwp-layout-check-profile'

<#
窗口尺寸**刻意按真机上那台显示器量出来的 CSS 视口**，不是窗口的逻辑尺寸。

实测（`logs/app.log` 里 App 启动时打的「[几何]」那一条）：
窗口客户区 2100×1425 物理 px、scaleFactor=1.5，而页面视口是
**1120×761 CSS px**、devicePixelRatio=1.875——WebView2 的换算与 Tauri 报的
缩放因子不一致（这就是设计规格 §14 记的那个"至今未解决"的异常）。

排版是按 CSS px 算的，因此自检必须用 1120×761。用窗口的逻辑尺寸 1400×950
去测会**宽出一大截**，正好把"这一行会不会挤到换行"这类问题测没。
#>
$windowSize = '1120,761'

function Write-Step([string]$text) {
    Write-Host ""
    Write-Host "==> $text" -ForegroundColor Cyan
}

# 找一个 Chromium 内核的浏览器。用的是**用户已经装好的**那一个：
# 不下载任何东西（本项目的约定：不在项目之外产生下载行为）。
function Find-Browser {
    $candidates = @(
        (Join-Path $env:ProgramFiles 'Google\Chrome\Application\chrome.exe'),
        (Join-Path ${env:ProgramFiles(x86)} 'Google\Chrome\Application\chrome.exe'),
        (Join-Path ${env:ProgramFiles(x86)} 'Microsoft\Edge\Application\msedge.exe'),
        (Join-Path $env:ProgramFiles 'Microsoft\Edge\Application\msedge.exe')
    )
    foreach ($path in $candidates) {
        if ($path -and (Test-Path -LiteralPath $path)) { return $path }
    }
    throw '找不到 Chrome 或 Edge，无法做排版自检。'
}

if (-not $SkipBuild) {
    Write-Step '构建排版自检页面（不碰交付用的 dist/）'
    Push-Location $root
    try {
        & bunx vite build --config vite.layout.config.ts
        if ($LASTEXITCODE -ne 0) { throw "vite 构建失败（退出码 $LASTEXITCODE）" }
    }
    finally {
        Pop-Location
    }
}

if (-not (Test-Path -LiteralPath $distHtml)) {
    throw "找不到自检页面：$distHtml（先不加 -SkipBuild 跑一次）"
}

$browser = Find-Browser
Write-Host "浏览器：$browser"

# `file:///` + 绝对路径（分隔符必须是 /）。
$baseUrl = 'file:///' + ($distHtml -replace '\\', '/')

if (-not (Test-Path -LiteralPath $profile)) {
    New-Item -ItemType Directory -Path $profile -Force | Out-Null
}

<#
跑一次无头浏览器，把 DOM 取回来。

两个坑（都踩过）：
  1. PowerShell 里 `"$url?page=x"` 会被解析成变量 `$url?page`，结果 URL 变成
     `=x` —— 必须写成 `"${url}?page=x"`；
  2. 用 `&` 直接调用时，Chrome 有时什么都不输出（也没有报错）。用
     `Start-Process -Wait -RedirectStandardOutput` 稳定可靠。
#>
function Get-Dom([string]$query) {
    $outFile = Join-Path $env:TEMP "wwp-layout-$([guid]::NewGuid().ToString('N')).html"
    $errFile = "$outFile.err"
    $url = "${baseUrl}${query}"
    Start-Process -FilePath $browser -Wait -NoNewWindow `
        -ArgumentList @(
            '--headless=new', '--disable-gpu', '--no-sandbox',
            "--user-data-dir=$profile", '--allow-file-access-from-files',
            "--window-size=$windowSize", '--virtual-time-budget=9000',
            '--dump-dom', $url
        ) -RedirectStandardOutput $outFile -RedirectStandardError $errFile | Out-Null
    $text = Get-Content -LiteralPath $outFile -Raw
    Remove-Item -LiteralPath $outFile, $errFile -Force -ErrorAction SilentlyContinue
    return $text
}

function Get-Marker([string]$dom, [string]$pattern) {
    $match = [regex]::Match($dom, $pattern)
    if ($match.Success) { return $match.Value }
    return ''
}

$failures = @()

# --- 判据 1：点开「关联图库」不该让左边的名字输入框变窄 -------------------
Write-Step '判据 1：布局不跳动（scrollbar-gutter）'
# `rows=4` 不是随手挑的：这个行数下页面**本来不滚动、展开面板之后才滚动**，
# 正好跨过"滚动条出现"那个阈值——这也正是用户报的那个场景。
# 行数再多（5 行以上）时页面本来就滚着，对照组也就测不出差异了。
$stable = Get-Marker (Get-Dom '?page=playlists&rows=4&measure=1') 'GUTTER=[^<]*'
$control = Get-Marker (Get-Dom '?page=playlists&rows=4&measure=1&nogutter=1') 'GUTTER=[^<]*'
Write-Host "  修好后：$stable"
Write-Host "  对照组（关掉那条 CSS）：$control"
if ($stable -notmatch 'DELTA=0\.00') {
    $failures += "布局仍然会跳：$stable"
}
if ($control -notmatch 'DELTA=-15') {
    Write-Host "  注意：对照组这次也没测出差异（页面高度没跨过滚动条阈值），判据的灵敏度下降。" -ForegroundColor Yellow
}

# --- 判据 2：拖拽排序真的改了顺序 ----------------------------------------
Write-Step '判据 2：拖拽排序'
$below = Get-Marker (Get-Dom '?page=playlists&dragtest=below') 'DRAG_BELOW_BEFORE=[^<]*'
$above = Get-Marker (Get-Dom '?page=playlists&dragtest=above') 'DRAG_ABOVE_BEFORE=[^<]*'
Write-Host "  往下拖：$below"
Write-Host "  往上拖：$above"
foreach ($probe in @(@{ text = $below; expect = 2; label = '往下拖' }, @{ text = $above; expect = 1; label = '往上拖' })) {
    $m = [regex]::Match($probe.text, 'DRAG_\w+_BEFORE=(?<before>[^|]*) \| .* DRAG_AFTER=(?<after>.*)$')
    if (-not $m.Success) {
        $failures += "$($probe.label)：没拿到结果"
        continue
    }
    $first = $m.Groups['before'].Value.Trim()
    $after = $m.Groups['after'].Value.Split('|') | ForEach-Object { $_.Trim() }
    $position = [array]::IndexOf($after, $first)
    Write-Host "    「$first」拖动后落在第 $($position + 1) 位（期望第 $($probe.expect + 1) 位）"
    if ($position -ne $probe.expect) {
        $failures += "$($probe.label)：顺序不对（落在第 $($position + 1) 位）"
    }
}

# --- 判据 3：「删除当前壁纸」按钮 -----------------------------------------
Write-Step '判据 3：删除当前壁纸按钮'
$delete = Get-Marker (Get-Dom '?page=playback&deletetest=1') 'DELETE_BUTTON_FOUND=[^<]*'
Write-Host "  $delete"
if ($delete -notmatch 'DELETE_BUTTON_FOUND=true' -or $delete -notmatch 'DELETE_ENABLED=true') {
    $failures += "删除按钮不存在或不可点：$delete"
}
if ($delete -notmatch 'TOAST=已把') {
    $failures += "点了按钮没有弹出后端返回的提示：$delete"
}

# --- 判据 4：回收站相册页的接线 -------------------------------------------
Write-Step '判据 4：回收站相册页'
$trash = Get-Marker (Get-Dom '?page=trash&trashprobe=1') 'TRASH_CARDS=[^<]*'
Write-Host "  $trash"
$expectedCards = 4   # 假数据里就这 4 条（见 layout-check/main.tsx）
if ($trash -notmatch "TRASH_CARDS=$expectedCards") {
    $failures += "回收站没有列出全部条目：$trash"
}
if ($trash -notmatch "RESTORE_BUTTONS=$expectedCards") {
    $failures += "不是每张卡片都有「恢复」按钮：$trash"
}
if ($trash -notmatch "DISCARD_BUTTONS=$expectedCards") {
    $failures += "不是每张卡片都有「删除」按钮：$trash"
}
if ($trash -notmatch "THUMBNAILS=$expectedCards") {
    $failures += "有卡片的缩略图位置没被填上：$trash"
}
if ($trash -notmatch 'ALL_DELETE_BUTTON=true') {
    $failures += "顶部没有「全部删除」按钮：$trash"
}
# 假数据里第 4 条是"旧版删除的"（没有原位置），它的「恢复」必须是禁用的。
if ($trash -notmatch 'DISABLED_RESTORE=1') {
    $failures += "旧版条目（没有原位置）的「恢复」没有被禁用：$trash"
}
# 缩略图必须能看见**整张图**（用户要求：相册那一格是"决定留不留"的依据）：
# 显示的宽高比与原图一致 = 没有被 cover 裁掉，而且不超出格子。
if ($trash -notmatch 'THUMBS_MEASURED=4') {
    $failures += "没能量到 4 张缩略图：$trash"
}
if ($trash -notmatch 'THUMBS_CROPPED=0') {
    $failures += "有缩略图被裁掉了（看不到全貌）：$trash"
}
if ($trash -notmatch 'THUMBS_OVERSIZED=0') {
    $failures += "有缩略图超出了格子：$trash"
}
# 回收站的路径要显示出来（用户 2026-10 的要求），而且诊断页也要有一行——
# 两处同源（后端的 `trash_info.path` 与 `StartupInfo::trash_dir`）。
if ($trash -notmatch 'TRASH_PATH_SHOWN=true') {
    $failures += "回收站页没有显示回收站路径：$trash"
}
if ($trash -notmatch 'TRASH_PATH=[^<]*trash') {
    $failures += "回收站页显示的路径看着不像回收站目录：$trash"
}
# 「点缩略图 = 用系统默认程序打开这张图」「点文件名 = 打开原位置并选中该文件」
# （用户 2026-10；点文件名这一条 1.11.0 从"复制文件名"改成了"定位文件"）。
# 两件事都要**点真的元素**，所以探针点的是卡片里的缩略图与
# 文件名，再看后端命令收到了什么：id 必须是这一条的 id、定位的必须是
# `item.original`（原位置完整路径，不是文件名）。卡片上的"原位置"那一行已经整行删掉
# （`TRASH_ORIGIN_ROWS=0`）——原位置仍然记在 `origin.json` 里，只是不再显示。
if ($trash -notmatch 'TRASH_ORIGIN_ROWS=0') {
    $failures += "回收站卡片上还在显示原位置那一行：$trash"
}
if ($trash -notmatch "OPENABLE=$expectedCards") {
    $failures += "不是每张卡片的缩略图都能点开原图：$trash"
}
if ($trash -notmatch "COPYABLE=$expectedCards") {
    $failures += "不是每张卡片的文件名都能点着打开原位置：$trash"
}
if ($trash -notmatch 'TRASH_OPEN_CALLS=1') {
    $failures += "点一次缩略图没有（或多次）去打开原图：$trash"
}
if ($trash -notmatch 'TRASH_OPEN_ID_OK=true') {
    $failures += "打开的不是被点的那一张（id 对不上）：$trash"
}
if ($trash -notmatch 'TRASH_REVEAL_CALLS=1') {
    $failures += "点一次文件名没有（或多次）去打开原位置：$trash"
}
if ($trash -notmatch 'TRASH_REVEAL_ORIGINAL_OK=true') {
    $failures += "打开的不是这一条原来的完整路径：$trash"
}
# 1.11.0 起点文件名不再复制文件名：这条判据把旧行为钉死在"不允许回退"上。
if ($trash -notmatch 'TRASH_COPY_CALLS=0') {
    $failures += "点文件名还在复制（用户已改为「打开原来所在的文件夹并选中」）：$trash"
}
# 「全部删除」的进度条（用户 2026-10：删除大量图像时程序会长时间没有反应，
# 用户以为卡死）。这条判据要的不是"有个进度条"，而是它**跟着后端走**：
# 假后端在这里故意不返回（把"删除中"留在屏幕上），探针手动推两次进度事件，
# 宽度与 aria-valuenow 必须依次是 0% → 50% → 100%。
if ($trash -notmatch 'EMPTY_ROLE=progressbar') {
    $failures += "「全部删除」点下去没有出现进度条：$trash"
}
if ($trash -notmatch 'EMPTY_START=0%/0') {
    $failures += "进度条一开始不是 0%：$trash"
}
if ($trash -notmatch 'EMPTY_HALF=50%/50') {
    $failures += "进度条没有跟着后端推来的「2 / 4」走：$trash"
}
if ($trash -notmatch 'EMPTY_FULL=100%/100') {
    $failures += "进度条没有走到底：$trash"
}
if ($trash -notmatch 'EMPTY_CAPTION=正在送入系统回收站：0 / 4 项') {
    $failures += "进度条下面没有说明在做什么、共几项：$trash"
}
$diag = Get-Marker (Get-Dom '?page=diagnostics&diagprobe=1') 'DIAG_ROWS=[^<]*'
Write-Host "  $diag"
if ($diag -notmatch 'TRASH_ROW=[^<]*trash') {
    $failures += "诊断页没有「回收站目录」那一行：$diag"
}
if ($diag -notmatch 'CONFIG_ROW=[^<]*config') {
    $failures += "诊断页的路径表看着不对（配置目录那一行没拿到）：$diag"
}

# --- 判据 5：播放页每块屏的壁纸预览（当前 / 下一张） -----------------------
#
# 用户 2026-10 的要求：每屏能**看见**正在播的那张与下一张即将切换的壁纸。
# 这里验的是接线与排版：两格都在、缩略图解码出来了、倒计时在、格子没被挤扁。
# 假数据里两块屏各两张图（见 layout-check/main.tsx），因此 4 格都要有内容。
Write-Step '判据 5：播放页的壁纸预览'
$preview = Get-Marker (Get-Dom '?page=playback&shotprobe=1') 'SHOTS=[^<]*'
Write-Host "  $preview"
if ($preview -notmatch 'SHOTS=4') {
    $failures += "每块屏不是两格预览（当前 / 下一张）：$preview"
}
if ($preview -notmatch 'LOADED=4') {
    $failures += "有预览格的缩略图没被解码出来：$preview"
}
if ($preview -notmatch 'OVERFLOWING=0') {
    $failures += "预览格内部溢出了（内容比格子宽）：$preview"
}
if ($preview -notmatch 'BAD_FRAMES=0') {
    $failures += "有预览框被挤得太小：$preview"
}
if ($preview -notmatch 'COUNTDOWNS=2') {
    $failures += "「下一张」那格没有倒计时：$preview"
}
# 暂停时**数值与「已暂停」同时显示**（用户 2026-10 的要求：暂停字样不能把数值
# 顶掉，要排在旁边）。假数据里第二块屏是"已单独暂停"，因此恰好一个状态字样，
# 而且它那一格的倒计时必须还在（这就是"不覆盖数值"）。
if ($preview -notmatch 'PAUSED_TAGS=1') {
    $failures += "暂停中的那块屏没有显示「已暂停」字样（或显示多处）：$preview"
}
if ($preview -notmatch 'PAUSED_WITH_COUNTDOWN=1') {
    $failures += "暂停时倒计时被状态字样顶掉了（应当并排显示）：$preview"
}
if ($preview -notmatch 'PATHS=4') {
    $failures += "有预览格没显示路径：$preview"
}
if ($preview -notmatch 'WIDTH_MIN=(1[6-9][0-9]|[2-9][0-9]{2})\.') {
    $failures += "预览格太窄，图片看不清：$preview"
}
if ($preview -notmatch 'WIDTH_SPREAD=0\.0') {
    $failures += "同一页里预览格的宽度不一致（排版不整齐）：$preview"
}
# --- 预览生成不出来时那句说明必须**完整显示**（用户 2026-10）-----------------
#
# 用户原话："现在当壁纸过大时，不会展示预览，这一点我接受，但是此时的文字说明
# 看不全，本来应该是展示预览图的地方现在是半句话，这里的文字可以折行，
# 完整显示相关信息。"
#
# 假后端在 `?shoterror=1` 下让四格的缩略图**全都失败**（用后端那句原话），
# 于是四格都在显示说明。判据要的是三件事：说明折了行（`ERR_LINES`）、
# 没有被裁掉（`ERR_CLIPPED=0`，这是"看不全"的客观证据）、
# 显示的确实是后端那一整句（`ERR_COMPLETE`）。
$err = Get-Marker (Get-Dom '?page=playback&shoterror=1') 'ERR_BOXES=[^<]*'
Write-Host "  $err"
if ($err -notmatch 'ERR_BOXES=4') {
    $failures += "四格预览没有都显示「生成不出来」的说明：$err"
}
if ($err -notmatch 'ERR_LINES=4') {
    $failures += "预览框里的说明没有折行（一句话放不进一行，不折行就必然看不全）：$err"
}
if ($err -notmatch 'ERR_CLIPPED=0') {
    $failures += "预览框里的说明被裁掉了（用户要的是完整显示）：$err"
}
if ($err -notmatch 'ERR_NOWRAP=0') {
    $failures += "预览框里的说明还写着 white-space: nowrap：$err"
}
if ($err -notmatch 'ERR_COMPLETE=4') {
    $failures += "预览框里显示的不是后端那一整句说明：$err"
}
# 对照组：按改之前的写法（nowrap + 省略号）重排一次，探针**必须**测出被截断。
# 只报"一切正常"的判据自证不了什么（与判据 1、判据 6 的对照组同一个道理）。
$errControl = Get-Marker (Get-Dom '?page=playback&shoterror=1&shotnowrap=1') 'ERR_BOXES=[^<]*'
Write-Host "  对照组（按改之前的 nowrap + 省略号）：$errControl"
if ($errControl -notmatch 'ERR_CLIPPED=[1-9]') {
    Write-Host "  注意：对照组这次也没测出被截断，判据 5 这条的灵敏度下降。" -ForegroundColor Yellow
}
# --- 右键点那行路径 = 复制文件名（不含扩展名）（用户 2026-10）------------------
#
# 用户原话："我希望使用右键点击这个位置时，能够复制对应图像的文件名，
# 只复制文件名即可，不需要后缀。"左键那件事（打开所在文件夹并选中）仍由
# `?copyprobe=1` / `?nextprobe=1` 守着（那里要求 `COPY_TEXT_CALLS=0`）。
$name = Get-Marker (Get-Dom '?page=playback&nameprobe=1') 'NAME_CELLS=[^<]*'
Write-Host "  $name"
if ($name -notmatch 'NAME_CALLS=1') {
    $failures += "右键点那行路径没有（或多次）去复制：$name"
}
if ($name -notmatch 'NAME_STEM_OK=true') {
    $failures += "复制的不是被点那一格的图像文件名：$name"
}
if ($name -notmatch 'NAME_NO_EXT=true') {
    $failures += "复制的文件名还带着扩展名（用户要求不要后缀）：$name"
}
if ($name -notmatch 'NAME_NO_REVEAL=true') {
    $failures += "右键顺带把资源管理器也打开了（那是左键的事）：$name"
}
if ($name -notmatch 'NAME_TOAST=已复制文件名：') {
    $failures += "右键之后没有给出「已复制文件名」的反馈：$name"
}
# 这一条只打印不判定：预览块必然让卡片变高（两块屏并排 → 两张卡片一起变高），
# 而播放页本来是"不用滚动就能看全"的一页（设计规格 §10.18）。
# 把"多出去多少"记在这里，将来改这块时能一眼看出代价。
$noPreview = Get-Marker (Get-Dom '?page=playback&shotprobe=1&nopreview=1') 'SHOTS=[^<]*'
Write-Host "  对照组（藏起预览）：$noPreview"
# 对照组：某块屏"筛完一张不剩"时**不该**再有倒计时——写着"21 秒后"却永远不换纸，
# 比不写更糟。正常情况下是两个倒计时，这个情况下只能有一个、路径也只有三条。
$noCand = Get-Marker (Get-Dom '?page=playback&shotprobe=1&nocand=1') 'SHOTS=[^<]*'
Write-Host "  对照组（有一块屏筛选后 0 张）：$noCand"
if ($noCand -notmatch 'COUNTDOWNS=1') {
    $failures += "筛选后一张不剩的那块屏仍然显示倒计时：$noCand"
}
if ($noCand -notmatch 'PATHS=3') {
    $failures += "0 候选的屏应当显示「暂无下一张」而不是路径：$noCand"
}
# --- 「点下一张 = 只切这块屏」（用户 2026-10 的要求）---------------------
#
# 用户原话："当我点击展示的下一张的壁纸时，能够直接将对应屏幕的壁纸切换到
# 下一张，另一个屏幕不要动。"这里验接线：那一格可点、点下去只带**这一块屏**
# 的显示器名、提示语说清了另一块屏不动。真换哪张纸由 `cargo test` 与真机验收。
#
# 1.11.0 起这一格里有两个各做各事的点击目标：缩略图切纸（NEXT_TOAST 那句），
# 下面那行路径只打开所在文件夹、**不再切纸**（NEXT_REVEAL_* 那几条）。
$next = Get-Marker (Get-Dom '?page=playback&nextprobe=1') 'NEXT_SHOTS=[^<]*'
Write-Host "  $next"
if ($next -notmatch 'NEXT_OPENABLE=2') {
    $failures += "「下一张」那格不可点（两块屏都该可点）：$next"
}
if ($next -notmatch 'NEXT_CALLS=1') {
    $failures += "点一次「下一张」不该调用多次（或根本没调用）：$next"
}
if ($next -notmatch 'NEXT_ONLY_FIRST=true') {
    $failures += "点第一块屏的「下一张」影响到了别的屏：$next"
}
# 1.11.0 起「下一张」下面那行路径只打开所在文件夹，**不再切换壁纸**。
if ($next -notmatch 'NEXT_REVEAL_OK=true') {
    $failures += "点「下一张」下面那行路径没有打开它所在的文件夹：$next"
}
if ($next -notmatch 'NEXT_REVEAL_NO_SWITCH=true') {
    $failures += "点「下一张」下面那行路径把壁纸也切了（用户要求不切）：$next"
}
if ($next -notmatch 'NEXT_REVEAL_TOAST=已在资源管理器中打开并选中：') {
    $failures += "点「下一张」下面那行路径没有给出反馈：$next"
}
if ($next -notmatch 'NEXT_TOAST=已把这块屏切到下一张（另一块屏不动）') {
    $failures += "点了「下一张」没有给出正确反馈：$next"
}
# --- 「当前显示」那格：点图 = 系统默认程序打开，点路径 = 打开所在文件夹 --------
#
# 1.11.0 起这两个点击目标各做各的事（用户 2026-10 时是"点图即复制路径"）：
# 点缩略图用系统默认看图程序打开这张图，点下面那行路径打开它所在的文件夹
# 并选中它。因此这里两条都不能少，而且"不再复制路径"要作为判据钉住。
$copy = Get-Marker (Get-Dom '?page=playback&copyprobe=1') 'COPY_SHOTS=[^<]*'
Write-Host "  $copy"
if ($copy -notmatch 'COPY_OPENABLE=2') {
    $failures += "「当前显示」那格不可点（两块屏都该可点）：$copy"
}
if ($copy -notmatch 'COPY_VIA=img') {
    $failures += "「当前显示」那格里点的应当是图像本身：$copy"
}
if ($copy -notmatch 'COPY_OPEN_OK=true') {
    $failures += "点缩略图没有用系统默认程序打开这张图：$copy"
}
if ($copy -notmatch 'COPY_REVEAL_OK=true') {
    $failures += "点路径行没有打开它所在的文件夹并选中它：$copy"
}
if ($copy -notmatch 'COPY_TEXT_CALLS=0') {
    $failures += "点路径行还在复制路径（用户已改为「打开所在文件夹并选中」）：$copy"
}

# --- 判据 6：没有横向溢出、没有被挤扁的不可折行文字 ------------------------#
# "文字折行/挤在一起"是用户反复报过的一类问题。那些文字几乎都写着
# `white-space: nowrap`，因此挤不下时不会折行、而是把盒子撑出内容——
# `scrollWidth > clientWidth` 就是客观判据（见 layout-check/main.tsx）。
Write-Step '判据 6：六个页面都不得有横向溢出或被挤扁的文字'
$pages = 'playback', 'library', 'playlists', 'trash', 'settings', 'diagnostics'
$squeezedTotal = 0
foreach ($page in $pages) {
    $result = Get-Marker (Get-Dom "?page=$page&overflowprobe=1") 'PAGE=[^<]*'
    Write-Host "  $result"
    if ($result -notmatch "PAGE=$page") {
        $failures += "$page：没拿到排版探针的结果（页面可能没渲染出来）"
        continue
    }
    if ($result -notmatch 'DOC_OVERFLOW=0') {
        $failures += "$page：页面横向溢出（右侧内容会被裁掉）：$result"
    }
    if ($result -match 'SQUEEZED=(?<count>\d+)') {
        $squeezedTotal += [int]$Matches['count']
    }
}
if ($squeezedTotal -gt 0) {
    $failures += "共有 $squeezedTotal 处文字被挤扁（详见上面每一页的 DETAIL）"
}
# 对照组：把那些文字放大到 34px，探针**必须**测出问题来。
# 这不是装饰——只报"一切正常"的判据是自证不了什么的（见判据 1 同一个道理）。
$control = Get-Marker (Get-Dom '?page=playback&overflowprobe=1&squeezecontrol=1') 'PAGE=[^<]*'
Write-Host "  对照组（把不可折行的文字放大到 34px）：$control"
if ($control -notmatch 'SQUEEZED=[1-9]') {
    Write-Host "  注意：对照组这次也没测出被挤扁的文字，判据 6 的灵敏度下降。" -ForegroundColor Yellow
}

# --- 判据 7：每屏设置的三条排版要求（用户 2026-10）--------------------------
#
# 用户这一轮对播放页提了四条（按钮间距那条靠肉眼看，判据 6 保证它没把行挤爆）：
#   ① 三个方框（播放列表 / 固定单图 / NSFW 壁纸）等宽；
#   ② 数字块（当时是「已切换」「候选」）的数值跟在标签后面，不折行；
#   ③ 「长宽比区间」改名「图像比例筛选」并右对齐（空格留左侧）。
#
# 2026-10 又加了两条（用户："现在的播放列表等三项和右侧的方框的距离太远了，
# 而且这三个方框太短了……播放列表后面的方框太短了，根本看不到任何有用的信息，
# 一定要和下面两个一样长"）：
#   ④ 标签与方框紧紧挨着（`LABEL_GAP`，旧实现白空着 60 多像素）；
#   ⑤ 三个方框不但等宽，而且要**够宽**（`FIELD_W`；旧实现只有 70px）。
#
# 2026-10 再改一条（用户："将…已切换几次、候选这一行以及图像比例筛选这一行放在
# 一起，只保留现在的数据内容，形式为 66 次·切换用时 多少张 下限~上限，
# 将已切换、候选、图像比例筛选这几个词去掉，使两行的信息能够集中到一行"）——
# ② ③ 两条的**形式**因此变了，判据也跟着改成：
#   ②′ 数字行不折行（`MERGED_SAME_LINE`：六块同一条基线）；
#   ③′ 那三个词从界面上消失（`STAT_KEYS=0`），比例方框搬进数字行（`ASPECT_INPUTS=4`）。
# 右对齐那条判据随字段名一起取消：比例筛选不再是独立的一行，也就没有"左空右齐"。
Write-Step '判据 7：每屏设置的排版'
$layout = Get-Marker (Get-Dom '?page=playback&shotprobe=1') 'SHOTS=[^<]*'
Write-Host "  $layout"
if ($layout -notmatch 'FIELDS=6') {
    $failures += "每块屏的三个方框没凑齐（应当 3 个 × 2 块屏）：$layout"
}
if ($layout -notmatch 'FIELD_SPREAD=0\.0') {
    $failures += "三个方框的宽度不一致：$layout"
}
if ($layout -notmatch 'FIELD_W=(1[2-9][0-9]|[2-9][0-9]{2,})\.') {
    $failures += "方框太短了（至少要 120px 才看得见列表名）：$layout"
}
if ($layout -notmatch 'LABEL_GAP=([0-9]|1[0-2]) ') {
    $failures += "文字与方框之间空了太多（应当是网格的 8px 间距）：$layout"
}
if ($layout -notmatch 'STATS_STACKED=0') {
    $failures += "数字块的标签与数值又变成上下两行了（或这一块折行了）：$layout"
}
if ($layout -notmatch 'STATS_CUT=0') {
    $failures += "有数字块被裁掉了：$layout"
}
# 「66 次 · 12 ms」「520 张」与末尾那两个比例方框必须在**同一行**
# （用户 2026-10："使两行的信息能够集中到一行，精简页面"）。
# 两块屏各三块，六块必须落在同一条基线上。
if ($layout -notmatch 'MERGED_BLOCKS=6') {
    $failures += "数字行的内容块没凑齐（应当 3 块 × 2 块屏）：$layout"
}
if ($layout -notmatch 'MERGED_SAME_LINE=6') {
    $failures += "数字行折行了（不是所有内容块都在同一条基线上）：$layout"
}
if ($layout -notmatch 'MERGED_SPREAD=0') {
    $failures += "数字行里各块的高低不一致：$layout"
}
# "已切换 / 候选 / 图像比例筛选"这三个词必须真的从界面上消失：
# 这一行里一个标签都不该剩（只有"固定单图"和"最近错误"才带标签，
# 而这两个状态在默认假数据里都不出现）。
if ($layout -notmatch 'STAT_KEYS=0') {
    $failures += "数字行里还留着字段名（已切换 / 候选 / 图像比例筛选）：$layout"
}
# 比例区间的两个方框合并之后搬进了数字行：两块屏各两个，一共 4 个，
# 每个 88px、一样宽（要填的是 1.3 这种两三个字符的比值）。
if ($layout -notmatch 'ASPECT_INPUTS=4') {
    $failures += "比例区间的方框不在数字行里（或数量不对）：$layout"
}
if ($layout -notmatch 'ASPECT_W=(6[0-4]|5[0-9]|4[4-9])\.') {
    $failures += "比例方框的宽度不对（64px，最窄可缩到 44px）：$layout"
}
if ($layout -notmatch 'ASPECT_SPREAD=0\.0') {
    $failures += "比例方框的宽度不一致：$layout"
}
# 模式从 SAFE 更名为 NSFW（用户 2026-10："程序里所有相关的表述都要改掉"）。
# 两个数字守住它：控制行里那个按钮写着 NSFW；整页文字里一个 SAFE 都不剩。
# （漏掉的那一处不会报错、不会有警告，只会在界面上留着旧名字——所以要判据。）
if ($layout -notmatch 'NSFW_BUTTON=1') {
    $failures += "控制行里没有写着 NSFW 的按钮：$layout"
}
if ($layout -notmatch 'SAFE_TEXT_LEFT=0') {
    $failures += "界面上还留着 SAFE 字样（改名没改干净）：$layout"
}

# --- 判据 8：右上角浮层不许盖住「重新加载」（用户 2026-10）------------------
#
# 用户原话："检查所有右上角的悬浮提示，使其一定出现在程序 UI 界面的最右上角，
# 不要覆盖掉重新加载几个字。"
#
# 「重新加载」就在控制条卡片的右端、窗口右上角的正下方（1120 CSS px 视口下
# 按钮上边缘约在 y=120），因此这条判据必须**同时**守住三件事：
#   ① `POP_CORNER_OK=true`：两张卡都贴在距窗口右上角 8px 的位置；
#   ② `POP_OVERLAP=0`：两张卡的矩形与按钮矩形一个都不相交。这一条是"并排而不
#      堆叠"的直接原因——两个卡片纵向堆起来必然压住按钮；
#   ③ `POP_HIT_RELOAD=true`：按钮中心点用 `elementFromPoint` 探到的还是按钮本身，
#      证明整层浮层没把右上角的鼠标事件拦下来。
# 探针同时让两件最坏情况出现：一条很长的定位提示（提示越高越宽越容易压到按钮）
# 与一张正在扫描的进度卡。
Write-Step '判据 8：右上角浮层不覆盖「重新加载」'
$pop = Get-Marker (Get-Dom '?page=playback&toastprobe=1') 'POP_TOAST=[^<]*'
Write-Host "  $pop"
if ($pop -notmatch 'POP_CARDS=2') {
    $failures += "右上角的两个提示没有同时出现（探针没造出该有的场景）：$pop"
}
if ($pop -notmatch 'POP_CORNER_OK=true') {
    $failures += "右上角的悬浮提示没有贴在窗口最右上角：$pop"
}
if ($pop -notmatch 'POP_OVERLAP=0') {
    $failures += "右上角的悬浮提示盖住了「重新加载」：$pop"
}
if ($pop -notmatch 'POP_HIT_RELOAD=true') {
    $failures += "右上角的浮层挡住了「重新加载」的点击（鼠标点不到按钮）：$pop"
}
if ($pop -notmatch 'POP_GAP=8') {
    $failures += "两个提示不是并排的（间距应当是 8px）：$pop"
}

# --- 判据 9：设置页「清理程序目录」默认展开、折叠会被记住（用户 2026-10）-------
#
# 用户原话："设置页清理程序目录默认展开，用户可以手动选择折叠，记忆这个选项。"
# 三件事各要一个数字：
#   ① 一进设置页（设置里 `cleanupExpanded = true`）就该有清单
#      （假数据三项：壁纸缓存 / 日志 / 界面缓存）；
#   ② 点「收起」→ 清单消失，而且那一次切换**写回了设置**
#      （假后端的 `settings_update` 会把 `cleanupExpanded` 记下来，探针读它）；
#   ③ 再点「查看可清理项」→ 又展开（说明它是"跟着设置走的开关"，
#      而不是一次性的本地状态）。
# 另外：那一项"现在不能清理"的（界面缓存）复选框必须是**禁用**的。
# 另一半是"记忆"：`?cleanupfolded=1` 让假设置一开始就是折叠的（相当于用户
# 上次折叠过、这次重新进页面），那时**一打开就不该有清单**。
Write-Step '判据 9：设置页「清理程序目录」默认展开、折叠会被记住'
$clean = Get-Marker (Get-Dom '?page=settings&cleanupprobe=1') 'CLEAN_OPEN_AT_START=[^<]*'
Write-Host "  $clean"
if ($clean -notmatch 'CLEAN_OPEN_AT_START=3') {
    $failures += "设置页一打开「清理程序目录」没有展开（用户要求默认展开）：$clean"
}
if ($clean -notmatch 'CLEAN_START_LABEL=收起') {
    $failures += "展开着的时候按钮不是「收起」：$clean"
}
if ($clean -notmatch 'CLEAN_COLLAPSED=0') {
    $failures += "点了「收起」清单还在：$clean"
}
if ($clean -notmatch 'CLEAN_SETTING_AFTER=false') {
    $failures += "折叠那一下没有写回设置（下次进设置页又会展开）：$clean"
}
if ($clean -notmatch 'CLEAN_REOPENED=3') {
    $failures += "再点「查看可清理项」没有重新展开（或没有重新盘清单）：$clean"
}
if ($clean -notmatch 'CLEAN_BLOCKED=1') {
    $failures += "「现在不能清理」的那一项没有被禁用：$clean"
}
# 折叠被记住的那一半：`cleanupprobe=1` 也要在（探针才跑得起来），
# 靠 `cleanupfolded=1` 把假设置改成"上次折叠过"。
$folded = Get-Marker (Get-Dom '?page=settings&cleanupprobe=1&cleanupfolded=1') 'CLEAN_FOLDED_AT_START=[^<]*'
Write-Host "  折叠被记住时（相当于上次折叠过）：$folded"
if ($folded -notmatch 'CLEAN_FOLDED_AT_START=0') {
    $failures += "设置里记着「折叠」，可页面一打开清单还是展开了：$folded"
}
if ($folded -notmatch 'CLEAN_FOLDED_LABEL=查看可清理项') {
    $failures += "折叠状态下按钮不是「查看可清理项」：$folded"
}

# --- 判据 10：点 NSFW 按钮，前端送的命令名必须是后端注册的那个（1.13.1）--------
#
# **这条判据是补出来的**：1.13.0 把模式从 SAFE 改名成 NSFW 时，后端命令改成了
# `playback_nsfw`，而 `frontend/api.ts` 里 `invoke("playback_safe")` 那一行漏改。
# `tsc` 把命令名当普通字符串、查不出来；当时那九条判据里也没有一条点过这个按钮
# （判据 7 只数了"有一个写着 NSFW 的按钮"，没点它）。
# 于是交付出去的那一版**点 NSFW 按钮只会得到 `Command playback_safe not found`**，
# 由用户点下去才发现。
#
# 现在真的点一次，并读假后端收到的命令名。后端的注册名由
# `scripts/build-package.ps1` 的构建期闸逐个核对——两头合起来才是完整证据。
Write-Step '判据 10：点 NSFW 按钮真的调到了后端'
$nsfw = Get-Marker (Get-Dom '?page=playback&nsfwprobe=1') 'NSFW_BUTTON_FOUND=[^<]*'
Write-Host "  $nsfw"
if ($nsfw -notmatch 'NSFW_BUTTON_FOUND=true') {
    $failures += "找不到写着 NSFW 的按钮：$nsfw"
}
if ($nsfw -notmatch 'NSFW_BUTTON_DISABLED=false') {
    $failures += "NSFW 按钮是禁用的（默认假数据下它应当可点）：$nsfw"
}
if ($nsfw -notmatch 'NSFW_CLICK_CALLS=1') {
    $failures += "点一次 NSFW 按钮没有（或多次）调用后端：$nsfw"
}
if ($nsfw -notmatch 'NSFW_CLICK_CMD=playback_nsfw') {
    $failures += "NSFW 按钮送出的命令名不是后端注册的 playback_nsfw（真机上会报 Command not found）：$nsfw"
}
if ($nsfw -notmatch 'NSFW_CLICK_TOAST=已进入 NSFW 模式') {
    $failures += "点 NSFW 按钮之后没有把后端那句话浮出来：$nsfw"
}

# 构建期那道闸（`frontend/api.ts` 的命令名 vs `generate_handler!`）也在这里跑一遍：
# 排版自检本来就是"改完先自己验一遍"，而这类名字不一致要等用户点下去才会暴露。
Write-Step '前端命令名与后端注册表一致（与 build-package.ps1 同一套比对）'
$apiText = Get-Content -LiteralPath (Join-Path $root 'frontend\api.ts') -Raw
$mainText = Get-Content -LiteralPath (Join-Path $root 'src-tauri\src\main.rs') -Raw
$frontendCommands = [regex]::Matches($apiText, 'invoke(?:<[^>]*>)?\(\s*"([a-z0-9_]+)"') |
    ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique
$handlerBlock = [regex]::Match($mainText, '(?s)generate_handler!\[(.*?)\]').Groups[1].Value
$registeredCommands = [regex]::Matches($handlerBlock, 'commands::([a-z0-9_]+)') |
    ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique
$missing = @($frontendCommands | Where-Object { $_ -notin $registeredCommands })
Write-Host "  前端 $($frontendCommands.Count) 个命令 / 后端注册 $($registeredCommands.Count) 个；未注册：$(if ($missing.Count -gt 0) { $missing -join '、' } else { '无' })"
foreach ($name in $missing) {
    $failures += "前端调用了后端没注册的命令：$name（真机上会报 Command not found）"
}

# --- 截图 -----------------------------------------------------------------
if (-not $NoShots) {
    Write-Step '六个页面各截一张图'
    if (-not (Test-Path -LiteralPath $shots)) {
        New-Item -ItemType Directory -Path $shots -Force | Out-Null
    }
    foreach ($page in 'playback', 'library', 'playlists', 'trash', 'settings', 'diagnostics') {
        $png = Join-Path $shots "$page.png"
        $url = "${baseUrl}?page=$page"
        Start-Process -FilePath $browser -Wait -NoNewWindow `
            -ArgumentList @(
                '--headless=new', '--disable-gpu', '--no-sandbox',
                "--user-data-dir=$profile", '--allow-file-access-from-files',
                "--window-size=$windowSize", '--virtual-time-budget=6000',
                "--screenshot=$png", $url
            ) | Out-Null
        Write-Host ("  {0,-12} {1,8:N0} B" -f "$page.png", (Get-Item -LiteralPath $png).Length)
    }
    Write-Host "  截图目录：$shots" -ForegroundColor DarkYellow
}

Write-Host ""
if ($failures.Count -gt 0) {
    Write-Host '排版自检未通过：' -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    exit 1
}
Write-Host '排版自检通过（十条判据全部成立）。' -ForegroundColor Green
Write-Host '注意：这只证明界面排版与接线，真机行为仍然要跑真程序 + 看日志。' -ForegroundColor DarkYellow
