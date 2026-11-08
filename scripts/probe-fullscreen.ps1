# 全屏检测的端到端验收：造出**真的全屏窗口**，再让 --probe-fullscreen 去认。
#
# ## 为什么必须由脚本造窗口
#
# 上一版全屏检测（commit de5e6f4→993027a，已在 ea1b8d9 删除）失败的直接原因是
# **验证是假的**：那条提交自己写着"我原先的测试窗口也从没真正全屏过，因此
# '检测正常'的结论并不成立"——当时用的是 PotPlayer，它一直停在窗口模式
# (600,171)-(3239,1989)，而单元测试里的 `covers_screen` 全绿。
#
# 因此这个脚本用 WinForms 造出**边界精确**的窗口（无边框铺满显示器矩形 /
# 铺满工作区 / 最大化 / 普通窗口），逐个跑探针并自动比对。
#
# ## 断言为什么是"相对基线"的
#
# 桌面上本来就可能有最大化的窗口（编辑器、浏览器）。**那会被正确判成"全屏"**，
# 而不是 bug——第一版脚本按"绝对预期"断言，于是在这种再正常不过的情况下
# 报一堆假失败。现在先测一次**基线**（不造任何窗口），每个场景只断言
# "相对基线**多了**哪些屏"，并且对普通窗口额外断言"它自己没被判成全屏"。
#
# ## 为什么读日志而不是读 stdout
#
# 程序里的 `emit()` 只在 stdout 是终端时打印（release 构建没有控制台，
# 看日志才是常态）。脚本捕获输出时 stdout 是管道，因此这里读日志。
#
# ## 用法
#
#   pwsh -File scripts\probe-fullscreen.ps1
#   pwsh -File scripts\probe-fullscreen.ps1 -Exe src-tauri\target\debug\windows-wallpaper.exe
#
# 运行期间屏幕上会短暂出现一个深蓝窗口（每个场景约 1 秒）。它**会抢焦点**，
# 这不影响结论——检测本来就不依赖"谁是前台窗口"，这正是与上一版的关键区别。

[CmdletBinding()]
param(
    # 要测的可执行文件。默认依次找交付版与调试版。
    [string]$Exe = "",
    # 每个场景里窗口停留多久（毫秒）。给足时间让它被映射出来。
    [int]$HoldMilliseconds = 900
)

$ErrorActionPreference = "Stop"

function Resolve-Exe {
    param([string]$Given)
    if ($Given) {
        $full = [System.IO.Path]::GetFullPath($Given)
        if (-not (Test-Path $full)) { throw "找不到可执行文件：$full" }
        return $full
    }
    $root = Split-Path -Parent $PSScriptRoot
    $candidates = @(
        (Join-Path $root "release\WindowsWallPaper\WindowsWallPaper.exe"),
        # cargo build 的产物名是**包名**（小写），只有 tauri build 才会按
        # productName 改名成 WindowsWallPaper.exe。两个都找。
        (Join-Path $root "src-tauri\target\debug\windows-wallpaper.exe"),
        (Join-Path $root "src-tauri\target\debug\WindowsWallPaper.exe"),
        (Join-Path $root "src-tauri\target\release\windows-wallpaper.exe"),
        (Join-Path $root "src-tauri\target\release\WindowsWallPaper.exe")
    )
    foreach ($candidate in $candidates) {
        if (Test-Path $candidate) { return $candidate }
    }
    throw "找不到 WindowsWallPaper.exe。先构建，或用 -Exe 指定路径。"
}

<#
日志在哪，取决于"数据根"是怎么解析出来的（wwp-core/src/paths.rs）：
  WWP_HOME → 程序目录 → 开发期项目根（向上找 tauri.conf.json）→ %LOCALAPPDATA%

交付版（便携）走"程序目录"，日志就在 exe 旁边；但**调试版走的是开发期项目根**，
日志落在 `src-tauri\logs\`——而 `src-tauri\target\debug` 这个程序目录下什么都没有。
第一版脚本写死了"exe 旁边"，于是读不到日志、所有场景都判成失败。
#>
function Resolve-LogPath {
    param([string]$ExePath)
    $candidates = New-Object System.Collections.Generic.List[string]
    if ($env:WWP_HOME) { $candidates.Add((Join-Path $env:WWP_HOME "logs\app.log")) }

    $exeDir = Split-Path -Parent $ExePath
    $candidates.Add((Join-Path $exeDir "logs\app.log"))

    $probe = $exeDir
    for ($i = 0; $i -lt 5 -and $probe; $i++) {
        if (Test-Path (Join-Path $probe "tauri.conf.json")) {
            $candidates.Add((Join-Path $probe "logs\app.log"))
        }
        $probe = Split-Path -Parent $probe
    }

    if ($env:LOCALAPPDATA) {
        $candidates.Add((Join-Path $env:LOCALAPPDATA "WindowsWallPaper\logs\app.log"))
    }

    foreach ($candidate in $candidates) {
        if (Test-Path $candidate) { return $candidate }
    }
    return $candidates[0]
}

$Exe = Resolve-Exe -Given $Exe
$LogPath = Resolve-LogPath -ExePath $Exe
Write-Host "可执行文件：$Exe"
Write-Host "日志      ：$LogPath"
if (-not (Test-Path $LogPath)) {
    throw "日志不存在（$LogPath）——先手工跑一次程序让它建出来，再跑本脚本。"
}

# --- DPI 感知 ---------------------------------------------------------------
#
# 不设的话，本进程是"DPI 未感知"的，`Screen.Bounds` 拿到的是被系统虚拟化过的
# 尺寸（本机 4K@150% 会报成 2560x1440），于是"无边框铺满"的窗口其实没铺满，
# 测试就会得出"检测不到全屏"的错误结论。
# 这与 AGENTS.md 第 9 节里 capture-window.ps1 踩的是同一个坑。
Add-Type -Namespace WWP -Name Dpi -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(System.IntPtr value);
'@
# -4 = DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
[void][WWP.Dpi]::SetProcessDpiAwarenessContext([System.IntPtr](-4))

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

# --- 一次探测 ---------------------------------------------------------------

function Invoke-Probe {
    # 记录运行前的日志行数，只解析**这一次**新增的行。
    $before = @(Get-Content -LiteralPath $LogPath -Encoding UTF8)
    & $Exe --probe-fullscreen | Out-Null
    $after = @(Get-Content -LiteralPath $LogPath -Encoding UTF8)

    $fresh = @()
    if ($after.Count -gt $before.Count) { $fresh = $after[$before.Count..($after.Count - 1)] }

    # 屏幕判定行形如：  \\.\DISPLAY1 (0,0) 3840x2160  【全屏中】
    $verdicts = @($fresh | Where-Object { $_ -match '\\\\.\\DISPLAY\d+\s+\(' })
    $conclusion = $fresh | Where-Object { $_ -match '全屏检测结论：' } | Select-Object -Last 1

    $busy = @()
    if ($conclusion) {
        foreach ($match in [regex]::Matches($conclusion, '\\\\\.\\DISPLAY\d+')) {
            $busy += $match.Value
        }
    }

    return [pscustomobject]@{
        Busy       = @($busy | Sort-Object -Unique)
        Verdicts   = @($verdicts)
        Conclusion = $conclusion
        Fresh      = @($fresh)
    }
}

function Show-ProbeResult {
    param($Probe)
    foreach ($line in $Probe.Verdicts) { Write-Host ("    " + $line.Trim()) }
    if (-not $Probe.Conclusion) {
        Write-Host "    （日志里没有结论行——检查 $LogPath）" -ForegroundColor Yellow
    }
}

<#
在候选窗口表里找出**我们造的**那个窗口的判定行。

表中每个窗口是两行：判定行（含 `覆盖率=`）紧跟一行 `标题: ...`。
我们靠标题认自己，因此要成对读；返回的是判定行（含覆盖率与是否最大化）。
找不到返回 $null——那说明窗口被某个排除条件挡掉了，本身就是问题。
#>
function Find-TestWindowVerdict {
    param($Probe)
    $lastVerdict = $null
    foreach ($line in $Probe.Fresh) {
        if ($line -match '覆盖率=') { $lastVerdict = $line }
        elseif ($line -match '标题:.*WindowsWallPaper 全屏检测验收') { return $lastVerdict }
    }
    return $null
}

# --- 场景窗口 ---------------------------------------------------------------

function New-TestWindow {
    param(
        [string]$Kind,
        [System.Windows.Forms.Screen]$Target
    )
    $form = New-Object System.Windows.Forms.Form
    $form.Text = "WindowsWallPaper 全屏检测验收（$Kind）"
    $form.BackColor = [System.Drawing.Color]::DarkSlateBlue
    $form.ShowInTaskbar = $true

    switch ($Kind) {
        "fullscreen" {
            # 无边框、铺满显示器的**整个**矩形（含任务栏区域）。
            $form.FormBorderStyle = "None"
            $form.StartPosition = "Manual"
            $form.Bounds = $Target.Bounds
        }
        "borderless-taskbar" {
            # 无边框、只铺满工作区（留出任务栏）：实测里最容易漏判的一类。
            #
            # ⚠️ 这里**不能**用 `$Target.WorkingArea`：本机任务栏是**自动隐藏**的，
            # WorkingArea 与 Bounds 完全相等，于是这个场景退化成"再测一遍铺满"，
            # 什么都没测到（覆盖率打印出 1.000 才露馅——留出任务栏本该是 0.97）。
            # 因此显式减掉一条任务栏高度：老提交里实测过 (0,0)-(3840,2088) 就是这一种，
            # 150% 缩放下任务栏约 72 物理像素。
            $taskbar = [int]($Target.Bounds.Height / 30)
            $form.FormBorderStyle = "None"
            $form.StartPosition = "Manual"
            $form.Bounds = New-Object System.Drawing.Rectangle(
                $Target.Bounds.X, $Target.Bounds.Y,
                $Target.Bounds.Width, ($Target.Bounds.Height - $taskbar))
        }
        "maximized" {
            # 普通窗口 + 最大化：用户明确要求"最大化也算全屏"。
            $form.StartPosition = "Manual"
            $form.Location = $Target.Bounds.Location
            $form.Size = New-Object System.Drawing.Size(800, 600)
            $form.WindowState = "Maximized"
        }
        "windowed" {
            # 普通窗口，铺不满：它自己**必须不被**判成全屏。
            $form.StartPosition = "Manual"
            $x = $Target.Bounds.X + [int]($Target.Bounds.Width * 0.15)
            $y = $Target.Bounds.Y + [int]($Target.Bounds.Height * 0.15)
            $form.Location = New-Object System.Drawing.Point($x, $y)
            $form.Size = New-Object System.Drawing.Size(900, 600)
        }
        default { throw "未知场景：$Kind" }
    }

    $form.Show()
    [System.Windows.Forms.Application]::DoEvents()
    Start-Sleep -Milliseconds $HoldMilliseconds
    [System.Windows.Forms.Application]::DoEvents()
    return $form
}

function Close-TestWindow {
    param($Form)
    try {
        $Form.Close()
        $Form.Dispose()
    } catch {
        Write-Verbose "关窗失败（无所谓）：$_"
    }
    [System.Windows.Forms.Application]::DoEvents()
    Start-Sleep -Milliseconds 250
}

function Format-Set {
    param($Names)
    if (-not $Names -or @($Names).Count -eq 0) { return "无" }
    return (@($Names) -join "、")
}

# --- 基线 -------------------------------------------------------------------

$primary = [System.Windows.Forms.Screen]::PrimaryScreen
$all = [System.Windows.Forms.Screen]::AllScreens
$secondary = $all | Where-Object { $_.DeviceName -ne $primary.DeviceName } | Select-Object -First 1

Write-Host ""
Write-Host "显示器：$((@($all | ForEach-Object { $_.DeviceName })) -join '、')"
Write-Host ""
Write-Host "[基线] 不造任何窗口，看桌面上本来有哪些屏被判成「有程序全屏」" -ForegroundColor Cyan
$baselineProbe = Invoke-Probe
Show-ProbeResult -Probe $baselineProbe
$baseline = @($baselineProbe.Busy)
Write-Host ("    基线忙屏：" + (Format-Set $baseline))
if ($baseline.Count -gt 0) {
    Write-Host "    注：桌面上本来就有最大化的窗口，这是**正确**判定。下面主屏相关的场景" -ForegroundColor DarkYellow
    Write-Host "    只能验证「不会变少」；想更严格地验证请先取消那个窗口的最大化再跑。" -ForegroundColor DarkYellow
}
Write-Host ""

# --- 场景表 -----------------------------------------------------------------
#
# `Added` 是"这个场景**额外**应该让哪些屏变忙"（相对基线）。
#
# 每个场景还对**我们造的那个窗口自己**做两条断言：
#   OwnShouldCover      它自己该不该被算成"盖满这块屏"（覆盖率 >= 0.95 或最大化）
#   OwnShouldBeMaximized 它自己该不该带"最大化"标记（$null = 不检查）
#
# ⚠️ 这里能证明什么、不能证明什么：本机任务栏不高，所以**最大化的窗口覆盖率也有
# 0.97**，于是"最大化也算全屏"这条在这里是靠**覆盖率**分支成立的，证明不了
# `IsZoomed` 那条分支。那条分支由单元测试
# `a_maximized_window_counts_even_when_a_tall_taskbar_drops_the_coverage` 守
# （伪造一个高任务栏、覆盖率 0.9375 的屏）。两层合起来才覆盖两条判据。

$scenarios = New-Object System.Collections.Generic.List[object]
$scenarios.Add([pscustomobject]@{
    Kind = "windowed"; Screen = $primary; Added = @()
    OwnShouldCover = $false; OwnShouldBeMaximized = $false
    OwnCoverageMin = $null; OwnCoverageMax = 0.95
    Title = "普通窗口（铺不满）→ 它自己不该被判成全屏"
})
$scenarios.Add([pscustomobject]@{
    Kind = "maximized"; Screen = $primary; Added = @($primary.DeviceName)
    OwnShouldCover = $true; OwnShouldBeMaximized = $true
    OwnCoverageMin = $null; OwnCoverageMax = $null
    Title = "最大化窗口 → 用户要求算全屏"
})
$scenarios.Add([pscustomobject]@{
    Kind = "borderless-taskbar"; Screen = $primary; Added = @($primary.DeviceName)
    OwnShouldCover = $true; OwnShouldBeMaximized = $false
    # 必须是"真有缺口、但仍算全屏"：上限 0.999 才有意义，
    # 否则窗口要是又被摆成铺满，这个场景等于没测。
    OwnCoverageMin = 0.95; OwnCoverageMax = 0.999
    Title = "无边框、留出任务栏 → 算全屏（实测里最容易漏判的一类）"
})
$scenarios.Add([pscustomobject]@{
    Kind = "fullscreen"; Screen = $primary; Added = @($primary.DeviceName)
    OwnShouldCover = $true; OwnShouldBeMaximized = $false
    OwnCoverageMin = 0.999; OwnCoverageMax = $null
    Title = "无边框铺满主屏 → 算全屏"
})
if ($secondary) {
    $scenarios.Add([pscustomobject]@{
        Kind = "fullscreen"; Screen = $secondary; Added = @($secondary.DeviceName)
        OwnShouldCover = $true; OwnShouldBeMaximized = $false
        OwnCoverageMin = 0.999; OwnCoverageMax = $null
        Title = "无边框铺满副屏 → **只有副屏**忙（上一版用前台窗口做不到）"
    })
}

# --- 跑 ---------------------------------------------------------------------

$failures = 0

foreach ($scenario in $scenarios) {
    Write-Host "[$($scenario.Kind)] $($scenario.Title)" -ForegroundColor Cyan
    $form = $null
    try {
        $form = New-TestWindow -Kind $scenario.Kind -Target $scenario.Screen

        $probe = Invoke-Probe
        Show-ProbeResult -Probe $probe

        $actual = @($probe.Busy)
        $expected = @(($baseline + $scenario.Added) | Sort-Object -Unique)
        $same = ((@($actual | Sort-Object)) -join ",") -eq ((@($expected | Sort-Object)) -join ",")
        if ($same) {
            Write-Host "    => 符合预期（忙屏：$(Format-Set $actual)）" -ForegroundColor Green
        } else {
            $failures++
            Write-Host "    => 不符合预期：期望「$(Format-Set $expected)」，实际「$(Format-Set $actual)」" -ForegroundColor Red
        }

        if ($null -ne $scenario.OwnShouldCover) {
            # 这条断言独立于上面：即使那块屏因为别的原因已经忙，
            # "我们造的窗口自己有没有被判成全屏"仍然必须正确。
            $verdict = Find-TestWindowVerdict -Probe $probe
            if (-not $verdict) {
                $failures++
                Write-Host "    => 我们造的窗口没出现在候选表里（被某个排除条件挡掉了？）" -ForegroundColor Red
            } else {
                $isMaximized = $verdict -match '最大化'
                $coverage = 0.0
                if ($verdict -match '覆盖率=([0-9.]+)') { $coverage = [double]$matches[1] }
                $rect = ""
                if ($verdict -match '\(([-0-9,]+)\)\s+(\d+x\d+)') { $rect = "$($matches[1]) $($matches[2])" }
                $covered = $isMaximized -or ($coverage -ge 0.95)
                $label = "$rect 覆盖率={0:N3}{1}" -f $coverage, $(if ($isMaximized) { " 最大化" } else { "" })

                $problems = @()
                if ($scenario.OwnShouldCover -ne $covered) {
                    $problems += $(if ($scenario.OwnShouldCover) { "它该被算成盖满，但没有" } else { "它不该被算成盖满，但被算了" })
                }
                if ($null -ne $scenario.OwnShouldBeMaximized -and $scenario.OwnShouldBeMaximized -ne $isMaximized) {
                    $problems += "「最大化」标记不对"
                }
                if ($null -ne $scenario.OwnCoverageMin -and $coverage -lt $scenario.OwnCoverageMin) {
                    $problems += ("覆盖率应当 >= {0:N3}" -f $scenario.OwnCoverageMin)
                }
                if ($null -ne $scenario.OwnCoverageMax -and $coverage -gt $scenario.OwnCoverageMax) {
                    $problems += ("覆盖率应当 <= {0:N3}" -f $scenario.OwnCoverageMax)
                }

                if ($problems.Count -eq 0) {
                    Write-Host "    => 这个窗口自己：$label（符合预期）" -ForegroundColor Green
                } else {
                    $failures++
                    Write-Host "    => 这个窗口自己判错了：$label —— $($problems -join '；')" -ForegroundColor Red
                }
            }
        }
    } finally {
        if ($form) { Close-TestWindow -Form $form }
    }
    Write-Host ""
}

if ($failures -eq 0) {
    Write-Host "全部 $($scenarios.Count) 个场景通过（基线 $(Format-Set $baseline)）。" -ForegroundColor Green
    exit 0
}

Write-Host "$failures 项不符合预期。先看每个场景打印的候选窗口表：" -ForegroundColor Red
Write-Host "  - 覆盖率不足 → 窗口没真的铺满（本脚本已设 DPI 感知；再看表里 Bounds 是多少）"
Write-Host "  - 窗口根本没出现在候选列表里 → 被某个排除条件挡掉了（外壳类名？工具窗口？本进程？）"
exit 1
