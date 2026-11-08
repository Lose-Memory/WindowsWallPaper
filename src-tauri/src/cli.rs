// 命令行入口：本程序的诊断、实测与验证模式。
//
// 这里用 `//` 而不是 `//!`：本文件是被 `main.rs` 用 `include!` 包含进来的，
// 而内部文档注释（`//!`）只能出现在模块开头——包含进来之后那段位置在文件中间，
// 编译器会报 E0753。因此这里写普通注释。
//
//
// ## 这个文件为什么存在（而不是把这些代码留在 `main.rs` 里）
//
// 这些入口绝大多数是**开发与排查工具**：验证"解码 → 像素上传 → 合成"这条链、
// 实测壁纸回读延迟、模拟崩溃后恢复、探测桌面层窗口等等。
// 它们加起来有两千多行，而应用本身（窗口、托盘、启动、设置）只有几百行。
// 混在一起时，读 `main.rs` 的人会先被淹没在工具代码里，找不到应用逻辑。
//
// ## 为什么用 `include!` 而不是普通模块
//
// 这些函数要用 `main.rs` 里的 `emit` / `has_flag` / `flag_value` / `settings_for`
// 等辅助函数。用普通模块就得给那些辅助函数加可见性、再写一堆 `use`，
// 改动面大而收益只是"看起来更像模块"。用 `include!` 则是**文本级包含**：
// 这些函数仍然是 `main` 这个 crate 根模块的成员，互访不加任何修饰。
//
// 代价是它们拿不到 `pub(crate)` 那种"只给内部用"的约束——
// 但本程序是个二进制 crate，本来就没有对外接口，因此这个代价不存在。
//
// ## 每个入口的用途
//
// 见各函数自己的文档注释，或 `README.md` 的「命令行入口」一章。

/// `--probe-scan-status <目录>`：跑一次真实扫描，并按界面那样的节奏轮询共享状态。
///
/// ## 为什么需要它
///
/// 界面上那条右上角浮窗依赖 `library_scan_status` 返回的状态。而"浮窗没出现"
/// 这个现象有三种完全不同的原因，从界面本身分不出来：
///
///   1. 后端没有把状态写进去（共享状态一直是空的）；
///   2. 命令本身调不通（IPC 或序列化层的问题）；
///   3. 前端没渲染，或者轮询逻辑有问题。
///
/// 这条入口把 1 和 2 单独测掉：它**在同一进程里**用与命令层完全相同的
/// 那份状态读写方式，并按界面的节奏（500 ms）打印。如果这里能看到
/// 随扫描推进而变化的数字，问题就只可能出在第 3 种。
fn run_probe_scan_status(paths: &AppPaths) {
    let Some(dir) = flag_value("--probe-scan-status") else {
        let message = "用法：--probe-scan-status <目录>\n";
        eprintln!("{message}");
        emit(message);
        return;
    };
    let root = std::path::PathBuf::from(&dir);
    if !root.is_dir() {
        emit(&format!("目录不存在：{dir}\n"));
        return;
    }

    let settings = settings_for(paths);
    let index_root = paths.data_dir().join("index");
    let status = std::sync::Arc::new(std::sync::Mutex::new(crate::ScanStatus {
        library: Some(dir.clone()),
        started: Some(std::time::Instant::now()),
        latest: None,
        finished: false,
        outcome: None,
        // 这个探针是单库的，不属于"一键重扫全部图库"那一批。
        batch: None,
    }));

    // 扫描线程：与 `commands::library_scan` 用同一套写法（在进度回调里写状态）。
    let status_writer = std::sync::Arc::clone(&status);
    let handle = std::thread::spawn(move || {
        let options = wwp_core::library::ScanOptions {
            incremental: false,
            ..Default::default()
        };
        let outcome = wwp_core::library::scan_library(
            &root,
            &index_root,
            &settings,
            &options,
            // 这条探针量的是"扫描这条路本身"，不掺任何沿用数据。
            &[],
            |progress: &wwp_core::library::ScanProgress| {
                if let Ok(mut s) = status_writer.lock() {
                    s.latest = Some(progress.clone());
                }
                true
            },
        );
        if let Ok(mut s) = status_writer.lock() {
            s.finished = true;
            s.outcome = Some(match &outcome {
                Ok(o) if o.cancelled => "已取消".to_string(),
                Ok(_) => "完成".to_string(),
                Err(error) => format!("失败：{error}"),
            });
        }
        outcome.is_ok()
    });

    // 主线程：按界面那样的节奏读状态并打印。
    let mut samples = 0usize;
    let mut text = format!("=== 扫描状态轮询：{dir} ===\n");
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let (finished, line) = {
            let s = status.lock().unwrap_or_else(|e| e.into_inner());
            let elapsed = s.started.map(|i| i.elapsed().as_secs()).unwrap_or(0);
            let line = match &s.latest {
                Some(p) => format!(
                    "{elapsed:>3}s  已看到 {} 文件 / 候选 {}，已处理 {}，当前 {}",
                    p.files_seen,
                    p.candidates,
                    p.processed,
                    // 路径太长，只留最后两段。
                    p.current
                        .rsplit('\\')
                        .take(2)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\\")
                ),
                None => format!("{elapsed:>3}s  还没有收到任何进度（界面此时显示「正在准备」）"),
            };
            (s.finished, line)
        };
        text.push_str(&format!("  {line}\n"));
        samples += 1;
        if finished || samples >= 600 {
            break;
        }
    }
    let _ = handle.join();
    text.push_str(&format!("\n共 {samples} 次采样\n"));
    log::info!("{text}");
    emit(&text);
}

/// `--reveal <路径>`：走一遍"在资源管理器里打开它所在的文件夹并选中它"。
///
/// ## 为什么需要它
///
/// 这件事 1.11.0 用的是 `explorer.exe /select,<路径>`，而它在真机上**有时候**
/// 会打开 Home（资源管理器首页）而不是目标目录：`explorer.exe` 只是把请求转交
/// 给已经在跑的那个 shell，转交失败时它自己开一个默认位置，参数被静默丢掉、
/// 命令行本身没有任何反馈。1.11.1 换成 shell 的 `SHOpenFolderAndSelectItems`
/// （见 `wwp_core::shell::reveal_file`）。
///
/// 这条入口的价值在于**命令行看得见结果**：`spawn` 成功不等于资源管理器去了
/// 对的地方，所以这里把"到底去哪了"写进日志，配合 UI Automation 读窗口标题
/// 就能逐条核对，"有时候"这种说法才能落成可复现的结论。
///
/// 它只读、不改任何东西，随便跑。
fn run_probe_reveal() {
    let Some(target) = flag_value("--reveal") else {
        let message = "用法：--reveal <文件或目录的完整路径>\n";
        eprintln!("{message}");
        emit(message);
        return;
    };
    let path = std::path::PathBuf::from(&target);
    let mut text = format!(
        "=== 打开并定位：{target}\n\
         文件存在      : {}\n\
         所在目录存在  : {}\n",
        path.is_file(),
        path.parent().map(|p| p.is_dir()).unwrap_or(false)
    );
    match wwp_core::shell::reveal_file(&path) {
        Ok(()) => text.push_str("结果          : 已交给资源管理器\n"),
        Err(error) => text.push_str(&format!("结果          : 失败 —— {error}\n")),
    }
    log::info!("{text}");
    emit(&text);
}

/// `--probe-folder-tree <目录>`：打印目录树（界面"选择目录…"用的那个逻辑）。
///
/// 界面点击没法脚本化验证（WebView2 不接受合成点击），因此这条入口直接走
/// 同一套判断并打印结果。它验证的是：
///   - 只列目录、不列文件；
///   - `included` 与 `excluded_here` 在排除规则下的取值；
///   - "取消勾选父目录后子目录显示为随上级"——这正是界面复选框的语义。
fn run_probe_folder_tree(paths: &AppPaths) {
    let Some(target) = flag_value("--probe-folder-tree") else {
        let message = "用法：--probe-folder-tree <图库目录>\n";
        eprintln!("{message}");
        emit(message);
        return;
    };

    let settings = settings_for(paths);
    let mut text = format!("=== 目录树：{target} ===\n");
    text.push_str(&format!("排除规则 dirs：{:?}\n\n", settings.exclude.dirs));

    let root = std::path::PathBuf::from(&target);
    if !root.is_dir() {
        let failed = format!("{text}目录不存在\n");
        log::error!("{failed}");
        emit(&failed);
        return;
    }
    let matcher = wwp_core::library::exclude::ExcludeMatcher::new(&settings.exclude);

    /// 与 `commands::collect_folders` 相同的判断，只是直接打印而不用 State。
    fn walk(
        root: &std::path::Path,
        dir: &std::path::Path,
        depth: usize,
        max_depth: usize,
        matcher: &wwp_core::library::exclude::ExcludeMatcher,
        exclude: &wwp_core::config::ExcludeSettings,
        text: &mut String,
        count: &mut usize,
        excluded: &mut usize,
    ) {
        if depth > max_depth {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut subdirs: Vec<std::path::PathBuf> = entries
            .flatten()
            .filter(|e| {
                e.file_type()
                    .map(|k| k.is_dir() && !k.is_symlink())
                    .unwrap_or(false)
            })
            .map(|e| e.path())
            .collect();
        subdirs.sort();
        for sub in subdirs {
            let name = sub
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            // 与界面同一套判断：`excluded_here` 是**精确**比较，
            // `included` 才用会向上查找的匹配器。
            // 两者的差别正是"用户显式取消"与"随上级被取消"的差别。
            let normalize = |text: &str| -> String {
                text.replace('\\', "/").trim_end_matches('/').to_lowercase()
            };
            let target = normalize(&sub.to_string_lossy());
            let excluded_here = exclude
                .dirs
                .iter()
                .any(|rule| normalize(rule) == target);
            let rule_hits = matcher.should_skip_dir(&sub).is_some();
            let mut ancestor = false;
            let mut current = sub.parent();
            while let Some(parent) = current {
                if parent == root {
                    break;
                }
                if matcher.should_skip_dir(parent).is_some() {
                    ancestor = true;
                    break;
                }
                current = parent.parent();
            }
            let included = !rule_hits && !ancestor;
            *count += 1;
            if !included {
                *excluded += 1;
            }
            text.push_str(&format!(
                "{}{}{}{}\n",
                "  ".repeat(depth),
                if included { "[x] " } else { "[ ] " },
                name,
                if !included && !excluded_here {
                    "  ← 随上级被取消"
                } else {
                    ""
                }
            ));
            walk(
                root, &sub, depth + 1, max_depth, matcher, exclude, text, count, excluded,
            );
        }
    }

    let (mut count, mut excluded) = (0usize, 0usize);
    walk(
        &root,
        &root,
        0,
        6,
        &matcher,
        &settings.exclude,
        &mut text,
        &mut count,
        &mut excluded,
    );
    text.push_str(&format!("\n共 {count} 个目录，其中 {excluded} 个不参与\n"));

    log::info!("{text}");
    emit(&text);
}

/// `--scan --dir <目录> [--rescan]`：扫一个图库并打印统计。
///
/// 存在的意义是让"几万张图库能不能扫得动"这件事可以被实测，
/// 而不是靠估计；输出里每一项都能与目录里实际的文件数对上。
fn run_scan_mode(paths: &AppPaths) {
    let Some(dir) = flag_value("--dir") else {
        eprintln!("用法：WindowsWallPaper.exe --scan --dir <图库目录> [--rescan]");
        std::process::exit(2);
    };

    let settings = Settings::load_or_create(&paths.config_dir().join("settings.toml"))
        .unwrap_or_else(|error| {
            eprintln!("读取设置失败：{error}");
            std::process::exit(1);
        });

    let root = PathBuf::from(&dir);
    let options = wwp_core::library::ScanOptions {
        // --rescan 强制全量重探，用于对比"首次扫描"与"增量重扫"的耗时。
        incremental: !has_flag("--rescan"),
        // `--threads N` 覆盖并行度。
        //
        // 存在的理由是**对照实验**：优化并行度必须要能量出"几个线程最快"，
        // 而默认值写死在代码里就改不动了。实测用它比较过 8 / 12 / 16 线程
        // （机械盘、固态盘各一组），结论写在 `ScanOptions::threads` 的说明里。
        threads: flag_value("--threads")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or_else(|| wwp_core::library::ScanOptions::default().threads),
        ..Default::default()
    };

    let started = std::time::Instant::now();
    // `--inherit`：照界面那条路挑祖先图库的索引来沿用。默认关闭，因为它会
    // 改变扫描的输入——"这个图库自己扫要多久"与"能沿用多少"是两个问题。
    let inherited = if has_flag("--inherit") {
        let index_root = paths.data_dir().join("index");
        let ancestors = wwp_core::library::index::pick_ancestors(
            &root,
            &settings
                .libraries
                .iter()
                .map(std::path::PathBuf::from)
                .collect::<Vec<_>>(),
        );
        let (entries, without_index) = wwp_core::library::index::collect_entries(&index_root, &ancestors);
        log::info!(
            "祖先图库 {} 个（其中 {} 个还没有索引），读到 {} 条可复用记录",
            ancestors.len(),
            without_index,
            entries.len()
        );
        entries
    } else {
        Vec::new()
    };
    let outcome = wwp_core::library::scan_library(
        &root,
        &paths.data_dir().join("index"),
        &settings,
        &options,
        &inherited,
        |progress| {
            // 进度写日志：release 构建没有控制台，日志才是能事后翻看的记录。
            log::info!(
                "进度：已处理 {}/{} 候选（复用 {}，其中沿用其他图库 {}），当前 {}",
                progress.processed,
                progress.candidates,
                progress.reused,
                progress.reused_inherited,
                progress.current
            );
            true
        },
    );

    match outcome {
        Ok(outcome) => {
            let text = format_scan_report(&root, &outcome, started.elapsed());
            log::info!("扫描结果：\n{text}");
            emit(&text);
        }
        Err(error) => {
            eprintln!("扫描失败：{error}");
            log::error!("扫描失败：{error}");
            std::process::exit(1);
        }
    }
}

/// 把一次扫描的结果整理成可读报告。
///
/// 这份报告的形状是刻意的：先给"看得见的结论"（可播放多少、耗时多少），
/// 再给"少了什么、为什么"（不支持/读不了/被排除，并列出格式与样本）。
/// 图库里少几张图而没有任何解释，是最容易被误判成程序坏了的情况。
fn format_scan_report(
    root: &std::path::Path,
    outcome: &wwp_core::library::ScanOutcome,
    elapsed: std::time::Duration,
) -> String {
    let stats = &outcome.meta.stats;
    let mut text = String::new();

    if outcome.cancelled {
        text.push_str("扫描被取消，索引保持原样（未写盘）。\n");
        return text;
    }

    text.push_str(&format!("图库根目录     : {}\n", root.display()));
    text.push_str(&format!(
        "耗时           : {:.2} 秒（索引内部计时 {} ms）\n",
        elapsed.as_secs_f64(),
        outcome.meta.scan_millis
    ));
    text.push_str(&format!(
        "复用 / 重探    : {} / {}（复用表示这些文件没有被打开过）\n",
        outcome.meta.reused_entries, outcome.meta.probed_entries
    ));
    // 变化台账：这三个数回答的是"我的图库变了什么"，
    // 而"复用/重探"回答的是"这次扫描的开销"。两者都要有。
    text.push_str(&format!(
        "新增/更新/删除 : {} / {} / {}\n",
        outcome.meta.added, outcome.meta.updated, outcome.meta.removed
    ));
    text.push_str(&format!("索引条目       : {}\n", outcome.entries.len()));
    text.push_str(&format!("可播放         : {}\n", stats.playable));
    text.push_str(&format!("看到的文件     : {}\n", stats.files_seen));
    text.push_str(&format!("扩展名不符图片 : {}\n", stats.files_ignored));
    text.push_str(&format!("被排除规则挡下 : {}\n", stats.excluded));
    text.push_str(&format!("没有解码器     : {}\n", stats.unsupported));
    text.push_str(&format!("读不了 / 损坏  : {}\n", stats.unreadable));

    if !stats.unsupported_formats.is_empty() {
        text.push_str("跳过格式明细   : ");
        let mut first = true;
        for (format, count) in &stats.unsupported_formats {
            if !first {
                text.push('、');
            }
            text.push_str(&format!("{format} {count} 个"));
            first = false;
        }
        text.push('\n');
    }

    if !stats.unreadable_samples.is_empty() {
        text.push_str(&format!(
            "无法读取样本   : 共 {} 条，列出前 {} 条：\n",
            stats.unreadable,
            stats.unreadable_samples.len().min(10)
        ));
        for sample in stats.unreadable_samples.iter().take(10) {
            text.push_str(&format!("  {} —— {}\n", sample.path, sample.reason));
        }
    }

    // 逐屏筛选要用到尺寸，顺便报几项分布，便于核对索引是否合理。
    let playable: Vec<_> = outcome.entries.iter().filter(|e| e.is_playable()).collect();
    if !playable.is_empty() {
        let landscape = playable.iter().filter(|e| e.aspect() > 1.0).count();
        let portrait = playable.iter().filter(|e| e.aspect() < 1.0).count();
        let rotated = playable.iter().filter(|e| e.or != 1).count();
        text.push_str(&format!(
            "方向分布       : 横 {} / 竖 {} / 带 EXIF 旋转 {}\n",
            landscape, portrait, rotated
        ));
    }

    text
}

// ============================================================================
// 壁纸与轮播
// ============================================================================

/// `--rotate`：把每屏独立的轮播真正跑起来。
///
/// 这是 M3 的验收方式。行为刻意做得可预期、可中止：
///
/// - 启动前拍壁纸快照，`Ctrl+C` 或写入 `data/stop-rotate` 文件都会触发还原后退出；
/// - 默认**保持暂停**，只有外加 `--go` 才真的开始换纸——因为这会接管用户的桌面，
///   不该在一次手滑的调用里发生；
/// - `--seconds N` 覆盖间隔，用于短时间验证"两屏各自按自己的节奏走"。
fn run_rotate(paths: &AppPaths, settings: &Settings) {
    use wwp_core::engine::{bind_screens, build_plans, DesktopSink};
    use wwp_core::playback::Playback;
    use wwp_core::wallpaper::DesktopWallpaper;

    let mut text = String::new();

    let bindings = match bind_screens() {
        Ok(bindings) => bindings,
        Err(error) => {
            eprintln!("建立显示器映射失败：{error}");
            log::error!("建立显示器映射失败：{error}");
            return;
        }
    };
    text.push_str(&format!("显示器映射（{} 块）:\n", bindings.len()));
    for binding in &bindings {
        text.push_str(&format!(
            "  {} {}x{} -> {}\n",
            binding.monitor_name, binding.width, binding.height, binding.device_path
        ));
    }

    let snapshot = match DesktopWallpaper::snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            log::error!("无法读取当前壁纸设置，放弃轮播以免无法还原：{error}");
            return;
        }
    };
    let snapshot_file = paths.data_dir().join("wallpaper-snapshot.json");
    if let Some(parent) = snapshot_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(&snapshot) {
        let _ = std::fs::write(&snapshot_file, json);
    }

    // 按参数覆盖间隔：验证"每屏独立"时用短间隔最方便。
    let mut settings = settings.clone();
    if let Some(seconds) = flag_value("--seconds").and_then(|v| v.parse::<u64>().ok()) {
        settings.general.interval_seconds = seconds.max(1);
        // 间隔现在在**播放列表**上，因此要逐个列表覆盖。
        // 不覆盖的话 --seconds 会被列表里的值压回原样（列表的值优先）。
        for playlist in &mut settings.playlists {
            playlist.interval_seconds = seconds.max(1);
        }
    }

    let plans = build_plans(&settings, &bindings);
    let sink = std::sync::Arc::new(DesktopSink::new(
        bindings.clone(),
        paths.data_dir().join("output"),
    ));
    let sink_trait: std::sync::Arc<dyn wwp_core::playback::WallpaperSink> = sink.clone();

    // 新版播放器自己不起线程：计时由调用方的循环驱动（`tick`）。
    // 这正是运行时外壳的做法，因此这条实测入口与真实路径同构。
    let playback = Playback::new(plans);
    let paused_by_default = !has_flag("--go");
    if paused_by_default {
        playback.pause();
    } else {
        let outcomes = playback.apply_all(&sink_trait);
        text.push_str(&format!("首轮换纸：{} 块屏\n", outcomes.len()));
    }

    text.push_str(&format!(
        "\n轮播已启动：{}\n",
        if paused_by_default {
            "暂停中（加 --go 才会真的换纸）"
        } else {
            "运行中"
        }
    ));
    text.push_str("停止方式：Ctrl+C，或创建下列文件：\n");
    let stop_file = paths.data_dir().join("stop-rotate");
    text.push_str(&format!("  {}\n", stop_file.display()));
    log::info!("逐屏轮播：\n{text}");
    emit(&text);

    // 参数里的时限：便于自动化验证，不会永久占用桌面。
    let limit = flag_value("--for")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    let started = std::time::Instant::now();
    // 在飞的换纸线程。每块屏各一个，互不等待（与外壳的做法一致）。
    let mut in_flight: Vec<std::thread::JoinHandle<()>> = Vec::new();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if stop_file.exists() {
            log::info!("检测到停止文件，准备还原并退出");
            break;
        }
        if limit > 0 && started.elapsed().as_secs() >= limit {
            log::info!("达到 --for {limit} 秒，准备还原并退出");
            break;
        }
        // 到点就换纸：**每块屏各自一个线程**，谁也不等谁。
        //
        // 这里若还是"并行换、等齐"，就无法验证用户要求的那条
        // "两块屏各自独立"——工具跟被测对象走同一条路才有意义。
        for task in playback.claim_due() {
            let sink = Arc::clone(&sink_trait);
            let spawned = std::thread::Builder::new()
                .name(format!("wwp-rotate-{}", task.monitor_name()))
                .spawn(move || {
                    task.run(&sink);
                });
            match spawned {
                Ok(handle) => in_flight.push(handle),
                Err(error) => log::warn!("无法启动换纸线程：{error}"),
            }
        }
        // 回收已经做完的，避免列表无限增长。
        in_flight.retain(|handle| !handle.is_finished());
    }

    // 还原之前先把在飞的换纸做完：否则刚还原完又有一张设上来，
    // 桌面就停在半路（这是实测工具的自保，见下面的说明）。
    for handle in in_flight {
        let _ = handle.join();
    }

    let _ = std::fs::remove_file(&stop_file);

    // **实测工具自己负责还原**：它只在开发机上跑几秒，不该把桌面留在半路。
    // 区别要点：程序本体不再做任何还原（用户明确要求），这里是工具的自保。
    match DesktopWallpaper::restore(&snapshot) {
        Ok(outcomes) => {
            log::info!("已还原 {} 块屏的壁纸", outcomes.len());
        }
        Err(error) => log::error!("还原壁纸失败，快照留在 {}：{error}", snapshot_file.display()),
    }

    // 顺带报告每块屏换了多少次，用于验证"两屏各自独立"。
    let mut report = String::from("\n轮播期间各屏状态：\n");
    for state in playback.states() {
        report.push_str(&format!(
            "  {} 换了 {} 次，最后一张 {}（耗时 {} ms）",
            state.monitor_name,
            state.changes,
            state.current.as_deref().unwrap_or("（无）"),
            state.last_change_millis
        ));
        if let Some(error) = &state.last_error {
            report.push_str(&format!("，最近错误：{error}"));
        }
        report.push('\n');
    }
    log::info!("{report}");
    emit(&report);
}

/// `--test-ui-playback-path [--for N]`：走"界面点开始轮播"那条路启停一次。
///
/// ## 为什么值得留着
///
/// 它是**最接近真实使用**的验证入口：用与界面命令完全相同的顺序
/// （`PlaybackManager::start` → 观察 → `stop`），而不是另写一条简化路径。
/// 真实流程与测试路径不同，是这个项目踩过的坑（测试入口自己制造了它要验证的 bug）。
///
/// 期间每 250 ms 打印各屏当前显示的图与换纸次数，因此"到底有没有在换"、
/// "哪块屏报错"一眼可见。收尾时**不还原壁纸**（与程序的实际行为一致）。
fn run_test_ui_playback_path(paths: &AppPaths) {
    #[cfg(not(windows))]
    {
        let _ = paths;
        eprintln!("此功能只在 Windows 上可用");
    }
    #[cfg(windows)]
    {
        let seconds = flag_value("--for")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(12);
        let settings = settings_for(paths);

        let mut text = String::from("=== 界面启停路径验证 ===\n");
        let state = AppState {
            paths: paths.clone(),
            settings: std::sync::Mutex::new(settings.clone()),
            scan_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            scanning: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            scan_status: std::sync::Arc::new(std::sync::Mutex::new(ScanStatus::default())),
            playback: playback_manager::PlaybackManager::new(),
        };
        // 与真实启动一样，先把数据与产物目录告诉管理器（NSFW 壁纸的预热要用）。
        state
            .playback
            .set_paths(paths.data_dir(), paths.data_dir().join("output"));

        // 与界面一致：启动前不做任何额外动作（不再挂载桌面层——视频壁纸已移除）。
        //
        // 把间隔压到 3 秒，好在观察期内看到几次换纸。
        let mut quick = settings.clone();
        quick.general.interval_seconds = 3;
        for playlist in &mut quick.playlists {
            playlist.interval_seconds = 3;
        }

        if let Err(error) = state.playback.start(&quick, true) {
            let failed = format!("{text}启动失败：{error}\n");
            log::error!("{failed}");
            emit(&failed);
            return;
        }
        text.push_str("轮播已启动\n");

        // 观察期内报告"换了多少张、当前是哪张、有没有报错"。
        //
        // 只在内容变化时打印：状态里的 `current` 在成功应用后会一直保留，
        // 不加这个判断就会每 250 ms 重复打印同一行（实测刷了上百行）。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        let mut last_reported: std::collections::HashMap<String, (String, u64)> =
            std::collections::HashMap::new();
        while std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let status = state.playback.status();
            for screen in &status.screens {
                let current = screen.current.clone().unwrap_or_default();
                let marker = (current.clone(), screen.changes);
                if last_reported.get(&screen.monitor_name) == Some(&marker) {
                    continue;
                }
                last_reported.insert(screen.monitor_name.clone(), marker);
                let name = std::path::Path::new(&current)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                text.push_str(&format!(
                    "  {} 第 {} 张：{}（候选 {} 张，最近一次 {} ms）\n",
                    screen.monitor_name,
                    screen.changes,
                    if name.is_empty() { "（还没有换过）" } else { &name },
                    screen.candidate_count,
                    screen.last_change_millis,
                ));
                if let Some(error) = &screen.last_error {
                    text.push_str(&format!("    报错：{error}\n"));
                }
            }
        }

        // 与 `playback_stop` 一样的收尾：停掉调度线程（**不还原壁纸**）。
        state.playback.stop();
        text.push_str("已停止轮播（程序不再还原壁纸，桌面留着的就是最后换上的那张）\n");

        log::info!("{text}");
        emit(&text);
    }
}

/// `--test-wallpaper-readback <图片>`：设置壁纸、等一会儿再回读。
///
/// ## 这条入口回答什么问题
///
/// "设了壁纸但屏幕上没变"有两种完全不同的原因：
///
/// 1. **只是慢**：Windows 要先转码再让桌面重绘，实测约 230 ms。
///    期间 `GetWallpaper` 返回旧路径。那么"多等几秒再读"就能读到新值。
/// 2. **有别的程序在改写**：例如 Wallpaper Engine 的服务会周期性
///    把壁纸改回它自己的 `WallpaperEngineOverride_*.jpg`。
///    那么"等越久越可能读到它的路径"。
///
/// 判据很直接：设置后读一次、等 3 秒再读一次、再等 5 秒读第三次。
/// 如果三次都是新值 → 属于情况 1（只是需要时间）。
/// 如果后来变成 WE 的路径 → 属于情况 2。
///
/// **实验结束会把原壁纸还原回去**，因此不会改变用户的桌面。
fn run_test_wallpaper_readback() {
    let Some(image) = flag_value("--test-wallpaper-readback") else {
        let message = "用法：--test-wallpaper-readback <图片路径>\n";
        eprintln!("{message}");
        emit(message);
        return;
    };

    let mut text = String::from("=== 壁纸读写对照实验 ===\n");

    // 先记下原状，实验结束要还原。
    let original = match wwp_core::wallpaper::DesktopWallpaper::snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let failed = format!("{text}读取原壁纸失败：{error}\n");
            log::error!("{failed}");
            emit(&failed);
            return;
        }
    };
    text.push_str("原状：\n");
    for monitor in &original.monitors {
        text.push_str(&format!(
            "  {} → {}\n",
            short_device(&monitor.device_path),
            std::path::Path::new(&monitor.current)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| monitor.current.clone())
        ));
    }

    // 设置壁纸。
    let position = wwp_core::wallpaper::WallpaperPosition::Fill;
    let _ = wwp_core::wallpaper::DesktopWallpaper::set_position(position);
    match wwp_core::wallpaper::DesktopWallpaper::set_wallpaper(None, &image) {
        Ok(()) => text.push_str(&format!("\n已设置壁纸：{image}\n")),
        Err(error) => {
            let failed = format!("{text}设置壁纸失败：{error}\n");
            log::error!("{failed}");
            emit(&failed);
            return;
        }
    }

    // 分三次回读，间隔递增。
    for (label, wait_ms) in [("立刻", 0u64), ("等 3 秒", 3000), ("再等 5 秒", 5000)] {
        if wait_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(wait_ms));
        }
        match wwp_core::wallpaper::DesktopWallpaper::snapshot() {
            Ok(now) => {
                text.push_str(&format!("\n回读（{label}）：\n"));
                for monitor in &now.monitors {
                    let name = std::path::Path::new(&monitor.current)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| monitor.current.clone());
                    text.push_str(&format!(
                        "  {} → {name}\n",
                        short_device(&monitor.device_path)
                    ));
                }
            }
            Err(error) => text.push_str(&format!("\n回读（{label}）失败：{error}\n")),
        }
    }

    // 还原。**必须做**：这个实验是诊断工具，不该改变用户的桌面。
    text.push_str("\n还原原壁纸：\n");
    match wwp_core::wallpaper::DesktopWallpaper::restore(&original) {
        Ok(outcomes) => {
            let failed = outcomes.iter().filter(|o| !o.ok).count();
            if failed == 0 {
                text.push_str(&format!("  已还原 {} 块屏\n", outcomes.len()));
            } else {
                text.push_str(&format!("  {} 块还原失败（请手工恢复）\n", failed));
            }
        }
        Err(error) => text.push_str(&format!("  还原失败：{error}\n")),
    }

    text.push_str(
        "\n判读：三次都是新路径 → 回读有延迟（需要等）；后来变成别程序的路径 → \
         有程序在改写壁纸。\n",
    );

    log::info!("{text}");
    emit(&text);
}

/// 采样屏幕中心像素，用于判断"画面是否真的变了"。
///
/// 为什么需要它：`SetWallpaper` 返回不代表画面已经变了——
/// 实测"立刻回读拿到旧值、3 秒后才拿到新值"（Windows 要转码图片）。
/// 因此要测**真实的可见延迟**，就必须直接看屏幕，而不是问 API。
///
/// 用 GDI 的 `GetPixel`：简单、无依赖。**它看不到 DirectComposition 的内容**
/// （那是硬件合成，不在 GDI 的位图里），因此这个方法只适用于判断
/// **静态壁纸**是否已经换过去，不能用来验证视频层的画面。
#[cfg(windows)]
fn sample_screen_center() -> Result<[(i32, i32, i32); 2], String> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetDC, GetPixel, MonitorFromPoint, MONITOR_DEFAULTTOPRIMARY, ReleaseDC,
    };
    use windows::Win32::UI::HiDpi::{
        GetDpiForMonitor, MDT_EFFECTIVE_DPI, SetProcessDpiAwarenessContext,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics;

    unsafe {
        // 先声明 DPI 感知，否则拿到的是被缩放后的虚拟坐标，
        // 而 GetPixel 用的是**物理**像素坐标。
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let virtual_width = GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CXVIRTUALSCREEN);
        let virtual_height = GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CYVIRTUALSCREEN);
        let virtual_left = GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_XVIRTUALSCREEN);
        let virtual_top = GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_YVIRTUALSCREEN);

        let mut result = [(0, 0, 0); 2];
        // 采样左半屏与右半屏的中心：两块屏分别是虚拟桌面的左半与右半
        // （实测布局：DISPLAY1 在 (0,0) 3840x2160，DISPLAY2 在 (-1440,-93)）。
        let points = [
            POINT {
                x: virtual_left + virtual_width / 4,
                y: virtual_top + virtual_height / 2,
            },
            POINT {
                x: virtual_left + virtual_width * 3 / 4,
                y: virtual_top + virtual_height / 2,
            },
        ];

        let hdc = GetDC(None);
        if hdc.is_invalid() {
            return Err("无法获取屏幕 DC".to_string());
        }
        for (index, point) in points.iter().enumerate() {
            // 顺带确认这个点属于哪块屏（诊断用，失败不影响采样）。
            let _monitor = MonitorFromPoint(*point, MONITOR_DEFAULTTOPRIMARY);
            let _ = GetDpiForMonitor;
            let _ = MDT_EFFECTIVE_DPI;
            let color = GetPixel(hdc, point.x, point.y);
            // COLORREF 是 0x00BBGGRR——注意不是 RGB 的顺序。
            // 这个顺序在本项目里错过一次（把红色标成了蓝色），因此这里写明。
            if color.0 == 0xFFFF_FFFF {
                result[index] = (-1, -1, -1);
            } else {
                let raw = color.0;
                result[index] = (
                    (raw & 0xFF) as i32,
                    ((raw >> 8) & 0xFF) as i32,
                    ((raw >> 16) & 0xFF) as i32,
                );
            }
        }
        ReleaseDC(None, hdc);
        Ok(result)
    }
}

#[cfg(not(windows))]
fn sample_screen_center() -> Result<[(i32, i32, i32); 2], String> {
    Err("此功能只在 Windows 上可用".to_string())
}

/// 采样虚拟桌面坐标 `(x, y)` 处的一个像素。
///
/// 与 [`sample_screen_center`] 的关系：后者写死了"左半 / 右半的中心"两个点，
/// 那是"两块屏横着并排"这个假设下的取巧写法。本机是 `3840×2160` 横屏
/// 加一块 `1440×2560` 竖屏，左半中心落进竖屏、右半中心也不在两块屏各自的中心，
/// 于是那两点量不出"某一块屏变没变"。这个函数按**给定坐标**取点，
/// 调用方拿显示器矩形自己算中心（见 `center_of`）。
#[cfg(windows)]
fn sample_pixel(x: i32, y: i32) -> Result<(i32, i32, i32), String> {
    use windows::Win32::Graphics::Gdi::{GetDC, GetPixel, ReleaseDC};
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };

    unsafe {
        // 先声明 DPI 感知，否则拿到的是被缩放后的虚拟坐标，
        // 而 GetPixel 用的是**物理**像素坐标。重复调用是幂等的。
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let hdc = GetDC(None);
        if hdc.is_invalid() {
            return Err("无法获取屏幕 DC".to_string());
        }
        let color = GetPixel(hdc, x, y);
        ReleaseDC(None, hdc);
        // COLORREF 是 0x00BBGGRR——注意不是 RGB 的顺序。
        if color.0 == 0xFFFF_FFFF {
            return Err(format!("({x},{y}) 处取不到像素"));
        }
        let raw = color.0;
        Ok((
            (raw & 0xFF) as i32,
            ((raw >> 8) & 0xFF) as i32,
            ((raw >> 16) & 0xFF) as i32,
        ))
    }
}

#[cfg(not(windows))]
fn sample_pixel(_x: i32, _y: i32) -> Result<(i32, i32, i32), String> {
    Err("此功能只在 Windows 上可用".to_string())
}

/// `--test-wallpaper-latency`：测"换纸后画面真的变了"要多久。
///
/// ## 为什么必须测这个
///
/// 项目里一直写着"每次换纸 48~71 ms"。那个数字其实是**API 调用返回**的耗时，
/// 不代表画面上真的出现了新图。实测发现 `SetWallpaper` 之后立刻 `GetWallpaper`
/// 拿到的是**旧值**，大约 3 秒后才变成新值（Windows 要先把图片转码成
/// `TranscodedWallpaper`，再让桌面重绘）。
///
/// 也就是说"换纸 48 ms"这个说法**可能严重低估了用户实际等待的时间**。
/// 这条入口用**两种判据**分别测量：
///
/// - 回读判据：`GetWallpaper` 什么时候变成新路径；
/// - 截屏判据：屏幕中心像素什么时候变成新图的主色。
///
/// 两个数字都报出来，差值本身也是信息（转码完成 ≠ 画面已重绘）。
fn run_test_wallpaper_latency(paths: &AppPaths) {
    let snapshot_file = paths.data_dir().join("wallpaper-snapshot.json");
    let mut text = String::from("=== 换纸可见延迟 ===\n");

    let original = match wwp_core::wallpaper::DesktopWallpaper::snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let failed = format!("{text}读取原壁纸失败：{error}\n");
            log::error!("{failed}");
            emit(&failed);
            return;
        }
    };

    // 造两张主色差异极大的图，这样"画面变了没"一目了然。
    let prepared = paths.data_dir().join("latency-test");
    let _ = std::fs::create_dir_all(&prepared);
    let solid_red = prepared.join("solid-red.png");
    let solid_blue = prepared.join("solid-blue.png");
    for (path, color) in [
        (&solid_red, [255u8, 0, 0]),
        (&solid_blue, [0u8, 0, 255]),
    ] {
        if let Err(error) = write_solid_png(path, color, 320, 240) {
            let failed = format!("{text}生成测试图失败：{error}\n");
            log::error!("{failed}");
            emit(&failed);
            return;
        }
    }

    let _ = wwp_core::wallpaper::DesktopWallpaper::set_position(
        wwp_core::wallpaper::WallpaperPosition::Fill,
    );

    // 每一轮：设置一张图，然后按 0.2 秒步进观察两种判据什么时候变。
    for (label, image) in [("红色", &solid_red), ("蓝色", &solid_blue)] {
        let started = std::time::Instant::now();
        if let Err(error) = wwp_core::wallpaper::DesktopWallpaper::set_wallpaper(None, &image.to_string_lossy()) {
            text.push_str(&format!("\n{label}：设置失败（{error}）\n"));
            continue;
        }
        let api_millis = started.elapsed().as_millis();

        let mut readback_millis: Option<u128> = None;
        let mut pixel_millis: Option<u128> = None;
        let target = image.file_name().unwrap_or_default().to_string_lossy().to_string();

        // 最多看 12 秒——实测回读大约 3 秒，留足余量。
        while started.elapsed().as_secs() < 12 {
            let elapsed = started.elapsed().as_millis();
            std::thread::sleep(std::time::Duration::from_millis(200));

            if readback_millis.is_none() {
                if let Ok(now) = wwp_core::wallpaper::DesktopWallpaper::snapshot() {
                    if now
                        .monitors
                        .iter()
                        .all(|m| m.current.ends_with(&target))
                    {
                        readback_millis = Some(elapsed);
                    }
                }
            }

            if pixel_millis.is_none() {
                if let Ok([left, right]) = sample_screen_center() {
                    // 红色判定：红通道明显高于蓝；蓝色反之。
                    let is_red_round = label == "红色";
                    let looks_right = |p: (i32, i32, i32)| {
                        if is_red_round {
                            p.0 > 120 && p.2 < 100
                        } else {
                            p.2 > 120 && p.0 < 100
                        }
                    };
                    if looks_right(left) && looks_right(right) {
                        pixel_millis = Some(elapsed);
                    }
                }
            }

            if readback_millis.is_some() && pixel_millis.is_some() {
                break;
            }
        }

        text.push_str(&format!(
            "\n{label}：\n  API 调用返回      : {api_millis} ms\n  \
             回读变为新路径    : {}\n  屏幕像素变为新主色: {}\n",
            readback_millis
                .map(|m| format!("{m} ms"))
                .unwrap_or_else(|| "12 秒内未变（可能读取被别的程序干扰）".to_string()),
            pixel_millis
                .map(|m| format!("{m} ms"))
                .unwrap_or_else(|| "12 秒内未变".to_string()),
        ));
    }

    // 还原。诊断工具不该改变用户桌面。
    text.push_str("\n还原原壁纸：\n");
    match wwp_core::wallpaper::DesktopWallpaper::restore(&original) {
        Ok(outcomes) => text.push_str(&format!(
            "  已还原 {} 块屏（失败 {} 块）\n",
            outcomes.len(),
            outcomes.iter().filter(|o| !o.ok).count()
        )),
        Err(error) => text.push_str(&format!("  还原失败：{error}\n")),
    }
    let _ = std::fs::remove_dir_all(&prepared);
    let _ = snapshot_file;

    log::info!("{text}");
    emit(&text);
}

/// `--probe-nsfw-latency`：把点 NSFW 那一下拆成若干步，逐步计时。
///
/// ## 为什么需要它
///
/// 用户报"切换 NSFW 壁纸有几百毫秒延时"。这个数字可能来自四个完全不同的地方，
/// 而从界面上**分辨不出来**：
///
///   1. `SetWallpaper` 这个调用本身慢；
///   2. 调用返回快，但桌面上**真正画出来**要等一会儿（Windows 要转码图片）；
///   3. NSFW 壁纸的产物缓存没命中，现场解码（几十到几百毫秒，见 §10.x 的实测）；
///   4. 界面层（按钮 → 命令 → 重新拉一次状态 → 重画）自己花掉的时间。
///
/// 这条入口走**与界面完全相同的引擎调用**
/// （`DesktopSink::prepare_nsfw` + `apply_prepared`，用户自己的 NSFW 壁纸路径），
/// 因此 1、2、3 都能在这里分成三个数字报出来：
///
///   - "准备"= `prepare_nsfw`（命中的话只有几次 stat，没命中就是解码 + 落盘）；
///   - "设上去"= `apply_prepared` 的返回值（= `SetWallpaper` 返回耗时）；
///   - "画面上出现"= 从发出设置到**屏幕中心像素真的变成那张图**的间隔（GDI 采样）。
///
/// 界面上还多出来的那一层（第 4 条）从后端量不到，只能靠这条对照判断：
/// 若这里的"画面上出现"已经接近用户感知的延时，那么界面层不是主因。
fn run_probe_nsfw_latency(paths: &AppPaths) {
    #[cfg(not(windows))]
    {
        let _ = paths;
        emit("本探针只在 Windows 上可用\n");
    }
    #[cfg(windows)]
    {
        let settings = settings_for(paths);

        let original = match wwp_core::wallpaper::DesktopWallpaper::snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let failed = format!("读取原壁纸失败：{error}\n");
                log::error!("{failed}");
                emit(&failed);
                return;
            }
        };

        let bindings = match wwp_core::engine::bind_screens() {
            Ok(bindings) => bindings,
            Err(error) => {
                let failed = format!("枚举显示器失败：{error}\n");
                log::error!("{failed}");
                emit(&failed);
                return;
            }
        };

        let sink = wwp_core::engine::DesktopSink::new(
            bindings.clone(),
            paths.data_dir().join("output"),
        );

        let mut text = String::from("=== NSFW 切换延时（逐步计时）===\n");
        if settings.monitors.is_empty() {
            text.push_str(
                "注意：这份 settings.toml 里没有任何 per-monitor 设置，\
                 因此下面每块屏都会报「没有设置 NSFW 壁纸」——\n\
                 那种情况下点 NSFW 只会停轮播、不会改壁纸。\n",
            );
        }

        for binding in &bindings {
            let monitor = binding.monitor_name.clone();
            let source = settings
                .monitors
                .get(&monitor)
                .map(|m| m.nsfw_wallpaper.trim().to_string())
                .unwrap_or_default();

            text.push_str(&format!(
                "\n{monitor}  目标 {}×{}  设备 {}\n",
                binding.width,
                binding.height,
                short_device(&binding.device_path)
            ));
            if source.is_empty() {
                text.push_str("  没有设置 NSFW 壁纸（设置里这块屏的 nsfw_wallpaper 是空的）\n");
                continue;
            }
            text.push_str(&format!("  源图 {source}\n"));

            // ① 准备产物（界面走的就是这一句）。
            let prepare_started = std::time::Instant::now();
            let prepared = match sink.prepare_nsfw(&monitor, std::path::Path::new(&source)) {
                Ok(prepared) => prepared,
                Err(error) => {
                    text.push_str(&format!("  准备失败：{error}\n"));
                    continue;
                }
            };
            let prepare_millis = prepare_started.elapsed().as_millis();

            let file = sink.nsfw_path(&monitor);
            let size_mb = std::fs::metadata(&file)
                .map(|meta| meta.len() as f64 / 1_048_576.0)
                .unwrap_or(0.0);

            // ② 设上去 + ③ 画面上出现（两件事分开计时，见 `measure_apply`）。
            let (set_millis, screen_millis, sample_note) =
                measure_apply(&sink, &monitor, &file, center_of(binding));

            text.push_str(&format!(
                "  产物      : {}（{:.1} MB）\n  \
                 ① 准备    : {prepare_millis} ms（{}）\n  \
                 ② 设上去  : {set_millis} ms（SetWallpaper 返回耗时）\n  \
                 ③ 画面上出现: {}{sample_note}\n",
                file.display(),
                size_mb,
                if prepared { "现场解码并落盘" } else { "命中已有产物" },
                screen_millis
                    .map(|m| format!("{m} ms（自发出设置起算）"))
                    .unwrap_or_else(|| "12 秒内未看到变化".to_string()),
            ));
        }

        // --- 对照：源文件格式会不会影响"画面上出现" ---
        //
        // 用户的观感是"几百毫秒"，而上面量到的 API 调用只有十几毫秒，因此那几百
        // 毫秒几乎全在 Windows 那边（转码 + 桌面重绘）。能撬动的只有"喂给系统的
        // 文件有多大"这一个变量。这里用一个**显式开关**做对照，默认不跑：
        // 每多测一轮就多花十几秒、多改一次用户的壁纸。
        if has_flag("--compare-nsfw-formats") {
            text.push_str(&compare_nsfw_formats(&sink, &bindings));
        }

        // 还原：诊断工具不该改变用户桌面。
        text.push_str("\n还原原壁纸：\n");
        let restore_started = std::time::Instant::now();
        match wwp_core::wallpaper::DesktopWallpaper::restore(&original) {
            Ok(outcomes) => {
                text.push_str(&format!(
                    "  已还原 {} 块屏（失败 {} 块，{} ms）\n",
                    outcomes.len(),
                    outcomes.iter().filter(|o| !o.ok).count(),
                    restore_started.elapsed().as_millis()
                ));
                // 失败原因必须打出来：只报"失败 2 块"的话，看日志的人
                // 无法区分"文件没了"、"设备路径变了"与"权限问题"。
                for outcome in outcomes.iter().filter(|o| !o.ok) {
                    text.push_str(&format!(
                        "    {} 失败：{}（原壁纸 {}）\n",
                        short_device(&outcome.device_path),
                        outcome.error.as_deref().unwrap_or("原因未知"),
                        if outcome.path.is_empty() {
                            "（快照里是空的）"
                        } else {
                            &outcome.path
                        }
                    ));
                }
            }
            Err(error) => text.push_str(&format!("  还原失败：{error}\n")),
        }

        log::info!("{text}");
        emit(&text);
    }
}

/// 取某块屏的中心点（虚拟桌面坐标），由引擎的绑定信息算出来。
///
/// ## 为什么不用快照里的矩形
///
/// `WallpaperSnapshot.monitors[].rect` 来自 `IDesktopWallpaper::GetMonitorRECT`，
/// 而实测在本机它**一直返回空**（两块屏都是 None），因此那条路量不到任何像素。
/// 引擎自己的 `ScreenBinding` 有 `(x, y)` 与宽高（来自 `EnumDisplayMonitors`），
/// 用它算中心既可靠，又与应用用的是同一份几何。
///
/// ## 为什么不能用"虚拟桌面的中心"
///
/// 第一版探针采样虚拟桌面左半与右半的中心。本机两块屏是
/// `DISPLAY1 (0,0) 3840×2160` 与 `DISPLAY2 (-1440,-93) 1440×2560`，
/// 于是"左半中心"落进了竖屏、"右半中心"也不在另一块屏的中心附近——
/// 量出来的"画面上出现"一块屏有一次、另一块屏永远看不到变化。
fn center_of(binding: &wwp_core::engine::ScreenBinding) -> (i32, i32) {
    (
        binding.x + binding.width as i32 / 2,
        binding.y + binding.height as i32 / 2,
    )
}

/// 设一次壁纸，返回 `(SetWallpaper 耗时, 画面上出现的耗时, 备注)`。
///
/// 采样步进 50 ms：比这更密只会让 `GetPixel` 自己成为负担，而结论的精度
/// 本来就是"几十毫秒"这个量级。
fn measure_apply(
    sink: &wwp_core::engine::DesktopSink,
    monitor: &str,
    file: &std::path::Path,
    center: (i32, i32),
) -> (u64, Option<u128>, String) {
    let (x, y) = center;
    // 起点像素 + 采样坐标一起留着：下面每一轮都取**同一个点**。
    let start = sample_pixel(x, y).ok().map(|rgb| (rgb, x, y));
    let started = std::time::Instant::now();
    let set_millis = match sink.apply_prepared(monitor, file) {
        Ok(millis) => millis,
        Err(error) => return (0, None, format!("  设置失败：{error}")),
    };

    let Some((before, x, y)) = start else {
        let why = format!(
            "  （在 ({x},{y}) 取不到像素：{}）",
            sample_pixel(x, y)
                .err()
                .unwrap_or_else(|| "原因未知".to_string())
        );
        return (set_millis, None, why);
    };

    while started.elapsed().as_secs() < 12 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let Ok(now) = sample_pixel(x, y) else {
            continue;
        };
        // 判据：任意通道差值 > 20。足够区分两张不同的照片，
        // 又不会被缩放与色彩转换带来的小幅偏差骗过。
        let changed = (before.0 - now.0).abs() > 20
            || (before.1 - now.1).abs() > 20
            || (before.2 - now.2).abs() > 20;
        if changed {
            return (set_millis, Some(started.elapsed().as_millis()), String::new());
        }
    }
    (set_millis, None, String::new())
}

/// 对照实验：同一个屏幕内容，分别用 BMP 与两种质量的 JPEG 喂给系统，比"画面上出现"。
///
/// 用 `--compare-nsfw-formats` 显式开启（见调用处的说明）。原本的算法说明里写着
/// "BMP 落盘 52 ms / JPEG 120~138 ms"——那是**写**这一侧的成本；这里量的是
/// **系统读取并转码**那一侧的成本，两者是不同的量，因此值得单独测一次。
fn compare_nsfw_formats(
    sink: &wwp_core::engine::DesktopSink,
    bindings: &[wwp_core::engine::ScreenBinding],
) -> String {
    let Some(binding) = bindings.first() else {
        return String::new();
    };
    let monitor = binding.monitor_name.clone();
    let bmp = sink.nsfw_path(&monitor);
    if !bmp.exists() {
        return String::new();
    }

    let mut text = format!(
        "\n=== 源文件格式对照（{}，同样内容不同格式）===\n",
        monitor
    );
    let center = center_of(binding);

    // 先切到一张**纯灰占位图**，让下面每一轮的"起点画面"都一样。
    // 否则第一轮可能因为"壁纸本来就已经是它"而量不到变化。
    let filler = bmp.with_file_name("probe-filler.png");
    if write_solid_png(&filler, [128, 128, 128], 320, 240).is_err() {
        return text + "  无法生成占位图，跳过\n";
    }
    let _ = wwp_core::wallpaper::DesktopWallpaper::set_wallpaper(None, &filler.to_string_lossy());
    std::thread::sleep(std::time::Duration::from_millis(1500));

    for (label, path) in std::iter::once(("BMP（当前实现）", bmp.clone()))
        .chain(jpeg_variants(&bmp))
    {
        let size_mb = std::fs::metadata(&path)
            .map(|meta| meta.len() as f64 / 1_048_576.0)
            .unwrap_or(0.0);
        let (set_millis, screen_millis, note) = measure_apply(sink, &monitor, &path, center);
        text.push_str(&format!(
            "  {label:<16} {size_mb:>6.1} MB  设上去 {set_millis:>3} ms  画面上出现 {}{note}\n",
            screen_millis
                .map(|m| format!("{m} ms"))
                .unwrap_or_else(|| "12 秒内未变".to_string()),
        ));
        // 让这一轮的画面稳定下来再测下一轮，否则量到的是上一轮的延迟。
        std::thread::sleep(std::time::Duration::from_millis(1200));
    }
    text
}

/// 用现有产物做两张 JPEG（质量 85 / 95），返回 `(标签, 路径)`。
///
/// 编码走引擎自己的 `image_prep::write_jpeg`：外壳不直接依赖 `image` crate
/// （那是引擎的依赖，外壳只通过 `wwp-core` 的公开函数用它）。
fn jpeg_variants(bmp: &std::path::Path) -> Vec<(&'static str, std::path::PathBuf)> {
    let mut out = Vec::new();
    for (label, quality) in [("JPEG q85", 85u8), ("JPEG q95", 95u8)] {
        let path = bmp.with_file_name(format!(
            "{}-q{quality}.jpg",
            bmp.file_stem().unwrap_or_default().to_string_lossy()
        ));
        if wwp_core::image_prep::write_jpeg(bmp, &path, quality).is_ok() {
            out.push((label, path));
        } else {
            log::warn!("生成 JPEG 对照图失败（质量 {quality}）");
        }
    }
    out
}

/// `--probe-nsfw-timing`：走**完整的那条路**（先真的把轮播跑起来），
/// 把点一次 NSFW 拆成四步分别计时。
///
/// ## 为什么与 `--probe-nsfw-latency` 分开
///
/// 那条只量引擎的两句调用（准备 / 设上去），起点是"轮播没在跑"。
/// 而界面上的 NSFW 前面还有一步 `stop`：它要等调度线程收尾。如果那一刻
/// **正好有一块屏在换纸**（解码 + 重采样，实测一张 30 MP 是几百毫秒），
/// 这个等待就会被算进用户的等待时间里。
///
/// 因此这条入口故意**先把轮播跑起来、还给每块屏立刻换一张**，
/// 让"正在换纸"这件事真的发生，然后立刻切 NSFW，量出：
///
///   - ① 停轮播（`stop` 阻塞了多久）；
///   - ② 刷新显示器绑定；
///   - ③ 准备（这几张图刚才刚写过，因此这一步是"抢一个正在被写的产物"）；
///   - ④ 设上去 + 画面上出现。
fn run_probe_nsfw_timing(paths: &AppPaths) {
    #[cfg(not(windows))]
    {
        let _ = paths;
        emit("本探针只在 Windows 上可用\n");
    }
    #[cfg(windows)]
    {
        let settings = settings_for(paths);
        let mut text = String::from("=== NSFW 切换全路径计时（轮播运行中点 NSFW）===\n");

        let original = match wwp_core::wallpaper::DesktopWallpaper::snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let failed = format!("读取原壁纸失败：{error}\n");
                log::error!("{failed}");
                emit(&failed);
                return;
            }
        };

        let manager = crate::playback_manager::PlaybackManager::new();
        manager.set_paths(paths.data_dir(), paths.data_dir().join("output"));
        if let Err(error) = manager.refresh_bindings() {
            text.push_str(&format!("枚举显示器失败：{error}\n"));
            log::info!("{text}");
            emit(&text);
            return;
        }

        // 起点：把轮播跑起来并**立刻换纸**——这正是"点 NSFW 时可能正在发生"的状态。
        let started = std::time::Instant::now();
        if let Err(error) = manager.start(&settings, true) {
            text.push_str(&format!("启动轮播失败：{error}\n"));
            log::info!("{text}");
            emit(&text);
            return;
        }
        text.push_str(&format!(
            "轮播已启动并立刻换纸（这一句阻塞了 {} ms）\n",
            started.elapsed().as_millis()
        ));

        // 给一点时间让"到点自动换纸"也可能落在下面那一步里。
        std::thread::sleep(std::time::Duration::from_millis(300));

        let stop_started = std::time::Instant::now();
        manager.stop();
        let stop_millis = stop_started.elapsed().as_millis();

        // 这里**故意不**自己调 `refresh_bindings`：要量的就是 `enter_nsfw` 里面
        // 那一段（它自己会判断手里的显示器列表够不够新）。探针多刷一次
        // 就等于替它把缓存做好了，量出来的数字会比用户实际遇到的更好看。
        let safe_started = std::time::Instant::now();
        let results = manager.enter_nsfw(&settings);
        let enter_millis = safe_started.elapsed().as_millis();

        text.push_str(&format!(
            "① 停轮播      : {stop_millis} ms\n\
             ②③ 进入 NSFW 模式: {enter_millis} ms（枚举显示器 + 准备 + 设上去）\n"
        ));
        match results {
            Ok(results) => {
                for result in &results {
                    text.push_str(&format!(
                        "  {}：{}（{}）\n",
                        result.monitor_name,
                        if result.ok { "成功" } else { "失败" },
                        result.outcome
                    ));
                }
            }
            Err(error) => text.push_str(&format!("  进入 NSFW 模式失败：{error}\n")),
        }

        // 还原：诊断工具不该改变用户桌面。
        text.push_str("\n还原原壁纸：\n");
        match wwp_core::wallpaper::DesktopWallpaper::restore(&original) {
            Ok(outcomes) => {
                text.push_str(&format!(
                    "  已还原 {} 块屏（失败 {} 块）\n",
                    outcomes.len(),
                    outcomes.iter().filter(|o| !o.ok).count()
                ));
                for outcome in outcomes.iter().filter(|o| !o.ok) {
                    text.push_str(&format!(
                        "    {} 失败：{}（原壁纸 {}）\n",
                        short_device(&outcome.device_path),
                        outcome.error.as_deref().unwrap_or("原因未知"),
                        if outcome.path.is_empty() {
                            "（快照里是空的）"
                        } else {
                            &outcome.path
                        }
                    ));
                }
            }
            Err(error) => text.push_str(&format!("  还原失败：{error}\n")),
        }

        log::info!("{text}");
        emit(&text);
    }
}

/// `--probe-nsfw-cycle`：真机验收"进入 NSFW 再退出，屏幕上回到进入之前那张"。
///
/// ## 为什么这条必须上真机
///
/// 这条行为有 8 条单元测试守着（`apply_remembered` / 记忆的交接 / 记忆的来源），
/// 但单元测试用的是**假 sink**：它们证明"该把哪张设上去"，证明不了
/// "真的设上去了"、也证明不了"原来那张还在候选里、文件还在"。
///
/// ## 探针做什么
///
/// 1. 拍下桌面原样（`DesktopWallpaper::snapshot`），结束时还原——探针不改变用户桌面；
/// 2. 把轮播跑起来并立刻换纸，读一次状态 → 得到每块屏的「当前 / 下一张」（记为 A）；
/// 3. **进入 NSFW**（这一步会记下 A 那两张）；
/// 4. **退出 NSFW** → 再读一次状态（记为 B），逐个比对：
///    - B 的「当前」必须还是 A 那张 —— 这正是用户要的"不重新加载壁纸"；
///    - B 的「下一张」必须还是 A 预告的那张 —— 第一次切换不重新抽签；
/// 5. 另外看一眼**桌面上真正的壁纸文件**：进入 NSFW 后应当是 `*-nsfw.bmp`，
///    退出之后又回到轮播产物（`*-0.bmp` / `*-1.bmp`）。
///
/// 它**不是只读**探针（要真的设壁纸），因此与 `--probe-nsfw-timing` 一样，
/// 只在用户那份程序没在跑的时候用。
fn run_probe_nsfw_cycle(paths: &AppPaths) {
    #[cfg(not(windows))]
    {
        let _ = paths;
        emit("本探针只在 Windows 上可用\n");
    }
    #[cfg(windows)]
    {
        let settings = settings_for(paths);
        let mut text = String::from("=== 进入 / 退出 NSFW：屏幕上是哪张 ===\n");

        let original = match wwp_core::wallpaper::DesktopWallpaper::snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let failed = format!("读取原壁纸失败：{error}\n");
                log::error!("{failed}");
                emit(&failed);
                return;
            }
        };
        // 进来时用户处于哪种状态，结束时恢复成哪种（探针不该改状态文件）。
        let was_active = crate::nsfw::is_active(&paths.data_dir());

        let manager = crate::playback_manager::PlaybackManager::new();
        manager.set_paths(paths.data_dir(), paths.data_dir().join("output"));
        if let Err(error) = manager.refresh_bindings() {
            text.push_str(&format!("枚举显示器失败：{error}\n"));
            log::info!("{text}");
            emit(&text);
            return;
        }

        // 起点：轮播跑起来并立刻换纸。只有"轮播在跑"时，进入 NSFW 才有东西可记
        // （`memory_snapshot` 要读调度线程发布的实时状态）。
        if let Err(error) = manager.start(&settings, true) {
            text.push_str(&format!("启动轮播失败：{error}\n"));
            log::info!("{text}");
            emit(&text);
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));

        let before = manager.status();
        text.push_str("A（进入 NSFW 之前）：\n");
        text.push_str(&describe_cycle_state(&before));

        if let Err(error) = manager.enter_nsfw(&settings) {
            text.push_str(&format!("进入 NSFW 模式失败：{error}\n"));
        }
        text.push_str(&format!(
            "进入 NSFW 之后桌面上的壁纸：{}\n",
            describe_nsfw_artifacts()
        ));

        let started = std::time::Instant::now();
        if let Err(error) = manager.leave_nsfw(&settings) {
            text.push_str(&format!("退出 NSFW 模式失败：{error}\n"));
        }
        text.push_str(&format!(
            "退出 NSFW 花了 {} ms\n",
            started.elapsed().as_millis()
        ));

        let after = manager.status();
        text.push_str("B（退出 NSFW 之后）：\n");
        text.push_str(&describe_cycle_state(&after));
        text.push_str(&compare_cycle_state(&before, &after));
        text.push_str(&format!(
            "退出 NSFW 之后桌面上的壁纸：{}\n",
            describe_nsfw_artifacts()
        ));

        // --- 收尾：停轮播、还原桌面、把状态文件恢复成进来时的样子 ---
        manager.stop();
        text.push_str("\n还原原壁纸：\n");
        match wwp_core::wallpaper::DesktopWallpaper::restore(&original) {
            Ok(outcomes) => text.push_str(&format!(
                "  已还原 {} 块屏（失败 {} 块）\n",
                outcomes.len(),
                outcomes.iter().filter(|o| !o.ok).count()
            )),
            Err(error) => text.push_str(&format!("  还原失败：{error}\n")),
        }
        crate::nsfw::set_active(&paths.data_dir(), was_active);
        text.push_str(&format!("  状态文件已恢复为：active={was_active}\n"));

        log::info!("{text}");
        emit(&text);
    }
}

/// 把一份播放状态里每块屏的「当前 / 下一张」写成几行（`--probe-nsfw-cycle` 用）。
#[cfg(windows)]
fn describe_cycle_state(status: &crate::playback_manager::PlaybackStatus) -> String {
    let mut text = String::new();
    for screen in &status.screens {
        text.push_str(&format!(
            "  {} 当前 {} 预告 {}\n",
            screen.monitor_name,
            screen.current.as_deref().unwrap_or("（没有）"),
            screen.next.as_deref().unwrap_or("（没有）"),
        ));
    }
    text
}

/// 逐个比对"进入之前"与"退出之后"：当前那张与预告那张都必须一模一样。
#[cfg(windows)]
fn compare_cycle_state(
    before: &crate::playback_manager::PlaybackStatus,
    after: &crate::playback_manager::PlaybackStatus,
) -> String {
    let (mut same_current, mut same_next, mut checked) = (0, 0, 0);
    for screen in &before.screens {
        let Some(after_screen) = after
            .screens
            .iter()
            .find(|s| s.monitor_name.eq_ignore_ascii_case(&screen.monitor_name))
        else {
            continue;
        };
        checked += 1;
        if screen.current == after_screen.current {
            same_current += 1;
        }
        if screen.next == after_screen.next {
            same_next += 1;
        }
    }
    format!(
        "比对：{checked} 块屏；当前一致 {same_current}，预告一致 {same_next}\n\
         （两个数字都等于 {checked} 才算通过）\n"
    )
}

/// 桌面上每块屏现在用的是哪个产物文件（只看文件名，路径太长）。
#[cfg(windows)]
fn describe_nsfw_artifacts() -> String {
    match wwp_core::wallpaper::DesktopWallpaper::enumerate() {
        Ok(monitors) => monitors
            .iter()
            .map(|monitor| {
                let name = std::path::Path::new(&monitor.current)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "（没有）".to_string());
                name
            })
            .collect::<Vec<_>>()
            .join("、"),
        Err(error) => format!("（读不到：{error}）"),
    }
}

/// `--cleanup`：列出程序目录里可以清理的东西；加 `--run` 才真的删。
///
/// ## 为什么默认只看不删
///
/// 界面上那条路是"列清单 → 用户勾选 → 确认"。命令行里没有勾选这一步，
/// 因此这里让"看"成为默认动作：不带 `--run` 时一个字节都不会动。
/// 想真的清就显式写 `--run`——这是**删除**，不该有一个手滑就执行完的默认值。
///
/// 走的是与界面完全相同的两个函数（`wwp_core::cleanup::list` / `run`），
/// 因此这里看到的清单就是界面上会看到的清单。
fn run_cleanup(paths: &AppPaths) {
    let items = wwp_core::cleanup::list(paths);
    let mut text = String::from("=== 可清理的内容 ===\n");

    if items.is_empty() {
        text.push_str("（没有可清理的内容）\n");
    }
    let mut total = 0u64;
    for item in &items {
        total += item.bytes;
        text.push_str(&format!(
            "  {:<12} {:>10}  {:>5} 个文件  {}\n      {}\n      {}\n",
            item.label,
            wwp_core::cleanup::human_bytes(item.bytes),
            item.files,
            item.path,
            item.detail,
            if item.removable {
                String::new()
            } else {
                format!("现在不能清理：{}", item.blocked_reason.as_deref().unwrap_or("原因未知"))
            }
        ));
    }
    text.push_str(&format!(
        "\n合计 {}（{} 项）\n",
        wwp_core::cleanup::human_bytes(total),
        items.len()
    ));

    if !has_flag("--run") {
        text.push_str("（只列出，没有删除。要真的清理请加 --run）\n");
    } else {
        let ids: Vec<String> = items.iter().map(|item| item.id.clone()).collect();
        match wwp_core::cleanup::run(paths, &ids) {
            Ok(report) => {
                text.push_str(&format!(
                    "\n已清理：释放 {}\n",
                    wwp_core::cleanup::human_bytes(report.freed)
                ));
                for outcome in &report.outcomes {
                    text.push_str(&format!(
                        "  {} {}\n",
                        if outcome.ok { "✓" } else { "✗" },
                        if outcome.ok {
                            format!(
                                "{} 释放 {}",
                                outcome.label,
                                wwp_core::cleanup::human_bytes(outcome.freed)
                            )
                        } else {
                            format!(
                                "{} 失败：{}",
                                outcome.label,
                                outcome.error.as_deref().unwrap_or("原因未知")
                            )
                        }
                    ));
                }
            }
            Err(error) => text.push_str(&format!("\n清理失败：{error}\n")),
        }
    }

    log::info!("{text}");
    emit(&text);
}

/// 写一张纯色 PNG。
///
/// 自己生成而不是依赖一个测试图库：这个实验需要**主色差异极大**的两张图，
/// 才能靠"屏幕中心像素"判断画面是否变了。用现成的照片做不到这一点。
///
/// 用项目里已有的 `png` crate（它本来只被 `--dump-frame` 用来写解码结果），
/// 而不是手写 PNG 编码与 zlib——那种代码容易出错，而且这里完全没有必要。
fn write_solid_png(
    path: &std::path::Path,
    color: [u8; 3],
    width: u32,
    height: u32,
) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;

    // 一整行像素重复 height 次。
    let mut row = Vec::with_capacity(width as usize * 3);
    for _ in 0..width {
        row.extend_from_slice(&color);
    }
    let mut data = Vec::with_capacity(row.len() * height as usize);
    for _ in 0..height {
        data.extend_from_slice(&row);
    }
    writer.write_image_data(&data).map_err(|e| e.to_string())?;
    Ok(())
}
/// 把设备路径缩短成 `...UID4353` 这样的尾部，便于阅读。
///
/// 完整的设备路径有 90 多个字符，每行都打全会让对照实验的输出没法看。
fn short_device(device_path: &str) -> String {
    match device_path.rsplit_once("&UID") {
        Some((_, tail)) => format!("...UID{tail}"),
        None => device_path.to_string(),
    }
}


/// `--probe-desktop`：只读报告当前桌面的窗口拓扑。
///
/// 不改任何窗口关系、不创建窗口、不碰壁纸，因此可以随时运行。
/// 输出用于回答两个问题：本机属于哪种拓扑（决定挂载方式），
/// 以及壁纸层窗口到底挂在哪里。
fn run_probe_desktop() {
    #[cfg(windows)]
    {
        let probe = wwp_core::desktop::probe_desktop();
        let text = probe.describe();
        log::info!("桌面拓扑探测：\n{text}");
        emit(&text);
    }
    #[cfg(not(windows))]
    {
        eprintln!("桌面拓扑探测只在 Windows 上可用");
    }
}

/// `--probe-fullscreen`：逐屏打印全屏检测的原始数据与判定结果。
///
/// ## 这个入口本身就是本功能的验收手段
///
/// 上一版全屏检测被删掉，根因不是"Windows 做不到"，而是**验证是假的**：
/// 那条提交自己写着"我原先的测试窗口也从没真正全屏过，因此'检测正常'的结论
/// 并不成立"。因此这一版把"每个候选窗口的矩形、覆盖率、是否最大化、判给谁"
/// 全部打印出来，再由 `scripts\probe-fullscreen.ps1` 造出**真的全屏窗口**对照。
///
/// 只读：不设壁纸、不改任何窗口。
fn run_probe_fullscreen() {
    #[cfg(windows)]
    {
        let detection = wwp_core::fullscreen::detect();
        let text = detection.describe();
        log::info!("全屏检测：\n{text}");
        emit(&text);

        let busy = detection.busy_names();
        let conclusion = if busy.is_empty() {
            "当前没有任何屏幕被判为「有程序全屏」。".to_string()
        } else {
            format!("有程序全屏的屏：{}", busy.join("、"))
        };
        log::info!("全屏检测结论：{conclusion}");
        emit(&format!("结论：{conclusion}\n"));
    }
    #[cfg(not(windows))]
    {
        emit("全屏检测只在 Windows 上可用\n");
    }
}

/// `--probe-wallpaper`：只读报告静态壁纸能力与各屏当前设置。
fn run_probe_wallpaper(_paths: &AppPaths) {
    let capability = wwp_core::wallpaper::probe_capability();
    let mut text = String::new();
    text.push_str(&format!(
        "IDesktopWallpaper 可用 : {}\n",
        if capability.available { "是" } else { "否" }
    ));
    if let Some(error) = &capability.error {
        text.push_str(&format!("失败原因               : {error}\n"));
    }
    text.push_str(&format!("显示器数量             : {}\n", capability.monitor_count));
    text.push_str(&format!(
        "位置模式（COM）        : {}\n",
        capability.position
    ));
    text.push_str(&format!(
        "注册表 WallpaperStyle  : {}\n",
        capability
            .registry_wallpaper_style
            .map(|v| v.to_string())
            .unwrap_or_else(|| "未设置".to_string())
    ));
    text.push_str(&format!(
        "是否允许逐屏不同图     : {}\n",
        if capability.per_monitor_compatible {
            "是"
        } else {
            "否（处于跨屏模式，逐屏设置不会生效）"
        }
    ));

    text.push_str("\n各屏当前壁纸：\n");
    match wwp_core::wallpaper::DesktopWallpaper::enumerate() {
        Ok(monitors) => {
            for monitor in &monitors {
                text.push_str(&format!("  设备路径: {}\n", monitor.device_path));
                text.push_str(&format!(
                    "    矩形  : {}\n",
                    monitor
                        .rect
                        .map(|(l, t, r, b)| format!("({l},{t}) {}x{}", r - l, b - t))
                        .unwrap_or_else(|| "未知".to_string())
                ));
                text.push_str(&format!(
                    "    壁纸  : {}\n",
                    if monitor.current.is_empty() {
                        "（未设置）"
                    } else {
                        &monitor.current
                    }
                ));
            }
        }
        Err(error) => text.push_str(&format!("  枚举失败：{error}\n")),
    }

    text.push_str("\n显示器枚举（EnumDisplayMonitors）：\n");
    match wwp_core::monitors::enumerate() {
        Ok(monitors) => {
            for monitor in &monitors {
                text.push_str(&format!("  {}\n", monitor.describe()));
            }
            let (x, y, w, h) = wwp_core::monitors::virtual_desktop_layout(&monitors);
            text.push_str(&format!("  虚拟桌面: ({x},{y}) {w}x{h}\n"));
        }
        Err(error) => text.push_str(&format!("  枚举失败：{error}\n")),
    }

    log::info!("壁纸能力探测：\n{text}");
    emit(&text);
}

/// `--spike-wallpaper`：逐屏设图实测。
///
/// 这是设计规格 §13 R1 要求的那个验证，回答三个问题：
/// 1. `MonitorID` 用哪一族标识符（`GetMonitorDevicePathAt` 的返回值）能否直接设图；
/// 2. 逐屏设图在 Win11 26H2 上是否真的生效（设置后回读是否一致）；
/// 3. 位置模式是否需要先改成非跨屏。
///
/// **安全网**：先拍快照，再设图，验证后无论成败都回滚，并把快照落盘留档。
/// 本机还运行着 Wallpaper Engine，它会周期性重写壁纸，因此这里专门做了
/// "延迟后二次回读"来判断是不是被别的程序覆盖了。
fn run_spike_wallpaper(paths: &AppPaths) {
    use wwp_core::wallpaper::{DesktopWallpaper, WallpaperPosition};

    let mut text = String::new();

    // --- 快照 ---
    let snapshot = match DesktopWallpaper::snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            eprintln!("无法读取当前壁纸设置，为避免不可预期的影响，不进行任何改动：{error}");
            return;
        }
    };
    text.push_str("=== 改动前快照（用户原有设置）===\n");
    text.push_str(&snapshot.describe());

    // 快照落盘留档，便于事后人工恢复。
    let snapshot_file = paths.data_dir().join("wallpaper-snapshot.json");
    if let Some(parent) = snapshot_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(&snapshot) {
        Ok(json) => {
            if std::fs::write(&snapshot_file, json).is_ok() {
                text.push_str(&format!("快照已留档 : {}\n", snapshot_file.display()));
            }
        }
        Err(error) => text.push_str(&format!("快照序列化失败（不影响回滚）：{error}\n")),
    }

    // --- 挑两张图：一张横、一张竖，正好对应本机的横屏与竖屏 ---
    let Some(dir) = flag_value("--dir") else {
        eprintln!("用法：WindowsWallPaper.exe --spike-wallpaper --dir <图库目录>");
        return;
    };
    let monitors = match wwp_core::monitors::enumerate() {
        Ok(monitors) => monitors,
        Err(error) => {
            eprintln!("枚举显示器失败：{error}");
            return;
        }
    };
    let Some((landscape, portrait)) = pick_spike_images(std::path::Path::new(&dir)) else {
        eprintln!("在 {dir} 里找不到合适的图片（需要一张长宽比 >1.2 与一张 <0.9）");
        return;
    };

    text.push_str("\n=== 将要设置的内容 ===\n");
    text.push_str(&format!("横图: {}\n", landscape.display()));
    text.push_str(&format!("竖图: {}\n", portrait.display()));
    for monitor in &monitors {
        let chosen = if monitor.is_portrait() { &portrait } else { &landscape };
        text.push_str(&format!(
            "  {} {}x{} -> {}\n",
            monitor.name,
            monitor.width,
            monitor.height,
            chosen.display()
        ));
    }

    // --- 位置模式：跨屏模式下逐屏设图没有意义，先确认 ---
    let position = DesktopWallpaper::position().unwrap_or(WallpaperPosition::Fill);
    text.push_str(&format!("\n改动前位置模式: {}\n", position.as_str()));
    if !position.is_per_monitor_compatible() {
        text.push_str("  处于跨屏模式，先改为 fill（填充）再逐屏设图\n");
        if let Err(error) = DesktopWallpaper::set_position(WallpaperPosition::Fill) {
            text.push_str(&format!("  改为填充模式失败：{error}\n"));
        }
    }

    // --- 逐屏设图 ---
    let wallpapers = DesktopWallpaper::enumerate().unwrap_or_default();
    text.push_str("\n=== 逐屏设置 ===\n");
    let mut expected: Vec<(String, String)> = Vec::new();
    for (index, monitor) in monitors.iter().enumerate() {
        let image = if monitor.is_portrait() {
            &portrait
        } else {
            &landscape
        };
        // 用设备路径做键；若枚举数量对不上则跳过，避免把图设到错误的屏。
        let Some(device) = wallpapers.get(index) else {
            text.push_str(&format!(
                "  跳过 {}：IDesktopWallpaper 只报告了 {} 块屏\n",
                monitor.name,
                wallpapers.len()
            ));
            continue;
        };
        let image_text = image.display().to_string();
        match DesktopWallpaper::set_wallpaper(Some(&device.device_path), &image_text) {
            Ok(()) => {
                text.push_str(&format!(
                    "  设置成功: {} -> {}\n",
                    device.device_path, image_text
                ));
                expected.push((device.device_path.clone(), image_text));
            }
            Err(error) => text.push_str(&format!(
                "  设置失败: {} -> {}\n    原因: {error}\n",
                device.device_path, image_text
            )),
        }
    }

    // --- 立即回读 ---
    text.push_str("\n=== 立即回读（判断 API 是否真的写进去了）===\n");
    let immediate = verify(&expected);
    text.push_str(&immediate);

    // --- 延迟回读：本机有 Wallpaper Engine 在跑，它会周期性重写壁纸 ---
    let delay = flag_value("--delay")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(6);
    text.push_str(&format!("\n等待 {delay} 秒后二次回读（检测是否被其他壁纸程序覆盖）…\n"));
    std::thread::sleep(std::time::Duration::from_secs(delay));
    let later = verify(&expected);
    text.push_str(&later);

    // --- 回滚 ---
    text.push_str("\n=== 回滚到改动前的设置 ===\n");
    match DesktopWallpaper::restore(&snapshot) {
        Ok(outcomes) => {
            for outcome in &outcomes {
                text.push_str(&format!(
                    "  {} {} {}\n",
                    if outcome.ok { "已恢复" } else { "恢复失败" },
                    outcome.device_path,
                    outcome.path
                ));
                if let Some(error) = &outcome.error {
                    text.push_str(&format!("      原因: {error}\n"));
                }
            }
            if outcomes.is_empty() {
                text.push_str("  快照里没有可恢复的逐屏壁纸（原来没有单独设置过）\n");
            }
        }
        Err(error) => text.push_str(&format!("  回滚失败：{error}\n")),
    }
    text.push_str(&format!(
        "  位置模式已恢复为: {}\n",
        snapshot.position
    ));

    text.push_str("\n=== 回滚后回读（确认已还原）===\n");
    let after = verify(&snapshot
        .monitors
        .iter()
        .filter(|m| !m.current.is_empty())
        .map(|m| (m.device_path.clone(), m.current.clone()))
        .collect::<Vec<_>>());
    text.push_str(&after);

    log::info!("逐屏设图实测：\n{text}");
    emit(&text);
}

/// 回读各屏壁纸并与期望值比对，返回可读报告。
fn verify(expected: &[(String, String)]) -> String {
    let mut text = String::new();
    if expected.is_empty() {
        text.push_str("  （没有需要比对的屏）\n");
        return text;
    }
    match wwp_core::wallpaper::DesktopWallpaper::enumerate() {
        Ok(actual) => {
            for (device, want) in expected {
                let got = actual
                    .iter()
                    .find(|m| m.device_path.eq_ignore_ascii_case(device))
                    .map(|m| m.current.clone())
                    .unwrap_or_default();
                let matches = got.eq_ignore_ascii_case(want);
                text.push_str(&format!(
                    "  {} {}\n    期望: {}\n    实际: {}\n",
                    if matches { "一致" } else { "不一致" },
                    device,
                    want,
                    if got.is_empty() { "（空）" } else { &got }
                ));
            }
        }
        Err(error) => text.push_str(&format!("  回读失败：{error}\n")),
    }
    text
}

/// 从图库目录里挑一张横图与一张竖图（递归、只看扩展名，够用即可）。
fn pick_spike_images(root: &std::path::Path) -> Option<(PathBuf, PathBuf)> {
    let mut landscape = None;
    let mut portrait = None;
    for entry in walkdir::WalkDir::new(root)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if !matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "bmp" | "webp") {
            continue;
        }
        // 用文件名里的尺寸线索挑选：测试图库的基准图名带 wide/tall 前缀。
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let lower = name.to_ascii_lowercase();
        if landscape.is_none() && (lower.starts_with("wide") || lower.contains("16_10")) {
            landscape = Some(path.to_path_buf());
        }
        if portrait.is_none() && (lower.starts_with("tall")) {
            portrait = Some(path.to_path_buf());
        }
        if landscape.is_some() && portrait.is_some() {
            break;
        }
    }
    match (landscape, portrait) {
        (Some(l), Some(p)) => Some((l, p)),
        _ => None,
    }
}

// ============================================================================
// 图库只读保证
// ============================================================================

/// `--test-library-readonly [--dir <图库>]`：验证**图库目录全程只被读取**。
/// ## 为什么值得单独做一次实测
///
/// 用户明确要求"程序对所有图库路径仅保留读取权限，禁止修改、写入图库目录下的
/// 任何文件内容"。这个保证靠读代码证明不了——扫描、索引、图像预处理、
/// 设置壁纸分散在四个模块里，任何一处新增一个写操作都会被漏掉。
///
/// 因此这里用**你自己的真实图库**跑完整条链，前后各拍一次目录快照
/// （递归列出所有文件的名字、大小、修改时间），然后逐项比对。
/// 合成目录做不到这一点：它里面的文件少、种类单一，漏掉的写操作
/// 可能碰不到它。
///
/// 快照包含**修改时间**而不只是文件名：程序若只是改写某个已有文件的
/// 内容（例如给图片加个 EXIF 标记），只比名字是发现不了的。
fn run_test_library_readonly(paths: &AppPaths) {
    let settings = settings_for(paths);

    // 图库清单来自配置（图库是独立配置项，增删入口在「图库」页）；
    // 也允许用 --dir 直接指定一个。
    let libraries: Vec<std::path::PathBuf> = match flag_value("--dir") {
        Some(dir) => vec![std::path::PathBuf::from(dir)],
        None => settings.libraries.clone(),
    };

    if libraries.is_empty() {
        let text = "没有可测的图库（配置里没有任何图库，可用 --dir 指定）\n";
        log::warn!("{text}");
        emit(text);
        return;
    }

    let mut text = String::from("=== 图库只读验证 ===\n");
    let mut failures = 0usize;

    for library in &libraries {
        if !library.is_dir() {
            text.push_str(&format!("跳过（不存在）：{}\n", library.display()));
            continue;
        }

        let before = snapshot_tree(library);
        text.push_str(&format!(
            "\n图库：{}\n  快照：{} 个文件、{} 个目录\n",
            library.display(),
            before.files.len(),
            before.dirs
        ));

        // --- 跑完整条链 ---
        let index_root = paths.data_dir().join("index");
        let options = wwp_core::library::ScanOptions {
            // 强制全量：跳过"复用"分支，让每个文件都被真正打开读一遍。
            // 复用分支不打开文件，因此它证明不了"读取是只读的"。
            incremental: false,
            ..Default::default()
        };
        let scan = wwp_core::library::scan_library(
            library,
            &index_root,
            &settings,
            &options,
            // 只读保证要覆盖"真正打开文件"那条路，因此不沿用任何外部记录。
            &[],
            |_| true,
        );
        let entries = match scan {
            Ok(outcome) => outcome.entries.len(),
            Err(error) => {
                text.push_str(&format!("  扫描失败：{error}\n"));
                failures += 1;
                continue;
            }
        };
        text.push_str(&format!("  扫描：{entries} 条\n"));

        // 读索引（这一步会反序列化所有条目）
        let index_paths = wwp_core::library::IndexPaths::new(
            &index_root,
            &wwp_core::library::library_id(library),
        );
        let loaded = wwp_core::library::load_index(&index_paths)
            .map(|(_, e)| e.len())
            .unwrap_or(0);
        text.push_str(&format!("  读索引：{loaded} 条\n"));

        // 图像预处理（真正解码 + 写出壁纸文件）。取前几个条目就够——
        // 这一步是唯一会"写出文件"的环节，而它的目标必须是缓存目录。
        let out_dir = paths.data_dir().join("readonly-probe-output");
        let _ = std::fs::create_dir_all(&out_dir);
        let mut prepared = 0usize;
        if let Ok((_, entries)) = wwp_core::library::load_index(&index_paths) {
            for (index, entry) in entries.iter().filter(|e| e.is_playable()).take(3).enumerate() {
                // 与真实路径一致：目标文件由调用方指定（这里用一个固定名字）。
                let destination = out_dir.join(format!("probe-{index}.bmp"));
                if wwp_core::image_prep::prepare(
                    std::path::Path::new(&entry.p),
                    &destination,
                    640,
                    480,
                    wwp_core::config::FitMode::Fill,
                    entry.or,
                )
                .is_ok()
                {
                    prepared += 1;
                }
            }
        }
        text.push_str(&format!("  预处理（写到缓存目录）：{prepared} 张\n"));
        let _ = std::fs::remove_dir_all(&out_dir);

        // --- 比对 ---
        let after = snapshot_tree(library);
        if before.files == after.files && before.dirs == after.dirs {
            text.push_str("  结果：**未被修改**（文件数、目录数、每个文件的大小与修改时间全部一致）\n");
        } else {
            failures += 1;
            text.push_str("  结果：**被修改了！**\n");
            // 列出差异，指明到底哪个文件被动过——只说"不一致"没法排查。
            for (path, meta) in &after.files {
                match before.files.get(path) {
                    None => text.push_str(&format!("    新增：{path}\n")),
                    Some(old) if old != meta => {
                        // (大小, 修改时间) 是个元组，没有 Display 实现，
                        // 因此显式解构再打印。
                        let (old_size, old_mtime) = old;
                        let (new_size, new_mtime) = meta;
                        text.push_str(&format!(
                            "    改动：{path}\n      前：{old_size} 字节，mtime {old_mtime}\n      后：{new_size} 字节，mtime {new_mtime}\n"
                        ))
                    }
                    Some(_) => {}
                }
            }
            for path in before.files.keys() {
                if !after.files.contains_key(path) {
                    text.push_str(&format!("    删除：{path}\n"));
                }
            }
            if before.dirs != after.dirs {
                text.push_str(&format!(
                    "    目录数变化：{} → {}\n",
                    before.dirs, after.dirs
                ));
            }
        }
    }

    text.push_str(&format!(
        "\n总结：{} 个图库，{} 个被修改\n",
        libraries.len(),
        failures
    ));
    log::info!("{text}");
    emit(&text);
}

/// 一个目录树的快照：每个文件的名字 → (大小, 修改时间)，以及目录数。
#[derive(Debug, PartialEq, Eq)]
struct TreeSnapshot {
    files: std::collections::BTreeMap<String, (u64, i64)>,
    dirs: usize,
}

/// 递归拍一张快照。
///
/// `mtime` 用"Unix 秒"的整数：纳秒精度在 FAT 等文件系统上不稳定，
/// 会因为与写入无关的原因变化（例如访问时间被更新），从而造成假报警。
fn snapshot_tree(root: &std::path::Path) -> TreeSnapshot {
    let mut files = std::collections::BTreeMap::new();
    let mut dirs = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => {
                    dirs += 1;
                    stack.push(path);
                }
                Ok(kind) if kind.is_file() => {
                    let meta = entry.metadata().ok();
                    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                    let mtime = meta
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    files.insert(path.display().to_string(), (size, mtime));
                }
                _ => {}
            }
        }
    }
    TreeSnapshot { files, dirs }
}

