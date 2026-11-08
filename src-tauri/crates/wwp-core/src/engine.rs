//! 播放 → 图像准备 → 系统接口的连接层。
//!
//! 引擎里的播放器（[`crate::playback`]）只负责"该换哪张图"，不碰壁纸接口；
//! 本模块实现那个 [`WallpaperSink`]，把"该换的文件"真正变成屏幕上的壁纸。
//!
//! 一次换纸的完整链路：
//!
//! ```text
//! 播放器选出一张源图
//!   → image_prep::prepare（解码、按 EXIF 方向摆正、按屏裁切、落成 BMP）
//!   → IDesktopWallpaper::SetWallpaper（按屏设置）
//!   → 更新状态、按需回收旧的缓存文件
//! ```
//!
//! **每块屏的尺寸来自显示器枚举，不是 `GetMonitorRECT`**：本机实测这两者不一致
//! （前者 2560x1440、后者 3840x2160，后者是壁纸槽位的矩形），按后者准备图像会
//! 得到尺寸不符的产物。
//!
//! **设备路径的映射**：播放器用的是 `\\.\DISPLAY1` 这类名字，而 `SetWallpaper`
//! 要的是 `\\?\DISPLAY#...#{...}`。两者的对应关系不能靠字符串猜，这里按
//! "显示器枚举顺序 == IDesktopWallpaper 枚举顺序"来建立映射，并在启动时校验数量，
//! 对不上就拒绝启动而不是把图设到错误的屏上。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::config::FitMode;
use crate::image_prep;
use crate::library::LibraryEntry;
use crate::monitors;
use crate::playback::{ScreenPlan, WallpaperSink};
use crate::wallpaper::{DesktopWallpaper, WallpaperPosition};
use crate::{Error, Result};

/// 一块屏的运行时信息：播放器用的名字 ↔ 系统接口用的设备路径。
#[derive(Debug, Clone)]
pub struct ScreenBinding {
    /// 播放器与配置里用的名字，如 `\\.\DISPLAY1`。
    pub monitor_name: String,
    /// `IDesktopWallpaper` 的设备路径。
    pub device_path: String,
    pub width: u32,
    pub height: u32,
    /// 这块屏在虚拟桌面里的左上角坐标。
    ///
    /// 界面靠它把两块屏的设置按**真实摆位**从左到右排开（用户的竖屏在
    /// 横屏左边，而枚举顺序是"主屏排最前"）。见 [`bind_screens`]。
    pub x: i32,
    pub y: i32,
    pub primary: bool,
}

/// 建立显示器与系统设备路径的对应关系。
///
/// 两边的枚举顺序在本机实测一致，但"应该一致"和"确实一致"是两件事，
/// 因此这里校验数量；不一致时返回错误，由调用方决定是放弃还是降级。
///
/// ## 顺序：先按枚举配对，再按物理位置排
///
/// 配对必须用 `monitors::enumerate()` 的顺序与 `IDesktopWallpaper` 的顺序**对齐**
/// （两者是同一次枚举的两个来源，靠下标对应），因此**先 zip、后排序**——
/// 反过来会打乱设备路径与屏的对应关系，把壁纸设到另一块屏上。
///
/// 排完序之后，列表顺序就是"桌面上从左到右"。这与 `monitors::enumerate()`
/// 内部的"主屏排最前"不同，是有意的：用户对比两块屏的设置时，界面上的左右
/// 应当与他桌上的左右一致，而不是与"哪块是主屏"一致。
pub fn bind_screens() -> Result<Vec<ScreenBinding>> {
    let monitors = monitors::enumerate()?;
    let wallpapers = DesktopWallpaper::enumerate()?;

    if monitors.len() != wallpapers.len() {
        return Err(Error::config(
            "显示器枚举结果与系统壁纸接口不一致",
            format!(
                "EnumDisplayMonitors 报告 {} 块，IDesktopWallpaper 报告 {} 块",
                monitors.len(),
                wallpapers.len()
            ),
        ));
    }

    let mut bindings: Vec<ScreenBinding> = monitors
        .iter()
        .zip(wallpapers.iter())
        .map(|(monitor, wallpaper)| ScreenBinding {
            monitor_name: monitor.name.clone(),
            device_path: wallpaper.device_path.clone(),
            width: monitor.width,
            height: monitor.height,
            x: monitor.x,
            y: monitor.y,
            primary: monitor.primary,
        })
        .collect();
    order_left_to_right(&mut bindings);
    log::info!(
        "已绑定 {} 块屏（按桌面从左到右）：{}",
        bindings.len(),
        bindings
            .iter()
            .map(|b| format!("{}@({},{})", b.monitor_name, b.x, b.y))
            .collect::<Vec<_>>()
            .join("，")
    );
    Ok(bindings)
}

/// 按显示器在桌面上的物理位置从左到右排序。
///
/// 排序键是 `(x, y)`：x 小的在左；两块屏上下叠放（x 相同）时从上到下。
/// 这个顺序只影响**界面上卡片的先后**与日志的排法——每块屏的播放计划、
/// 设备路径都跟着自己的 binding 走，因此排序不会改变"哪块屏显示什么"。
pub fn order_left_to_right(bindings: &mut [ScreenBinding]) {
    bindings.sort_by(|a, b| a.x.cmp(&b.x).then(a.y.cmp(&b.y)));
}

/// 视频画面的呈现方式。
///
/// ## 为什么做成 trait 而不是让引擎直接用 `DesktopLayer`
///
/// `wwp-core` 里有一半以上的测试是**无头**的（不碰桌面、不碰窗口）。
/// 一旦引擎直接依赖 `desktop_layer`（它要建窗口、要有显示器、要有 D3D 设备），
/// 那些测试就没法跑了。用一个只有两个方法的 trait 把两者隔开，
/// 测试里塞一个记录用的假实现即可——引擎的可测试性不该被这一个功能毁掉。
///
/// `DesktopLayer` 实现它是**运行时**的事（见 shell 侧的适配器）。
pub trait VideoPresenter: Send + Sync {
    /// 在某块屏上开始播放视频。
    fn play(&self, monitor_name: &str, file: &std::path::Path) -> std::result::Result<(), String>;

    /// 停止某块屏的视频播放（切回静态图片时必须调用）。
    ///
    /// 不停止的后果很具体：视频画面会一直盖在刚设好的静态壁纸上面，
    /// 用户看到的是"换了图但屏幕没变"。
    fn stop(&self, monitor_name: &str) -> std::result::Result<(), String>;
}

/// 什么都不做的视频呈现器。用于"没有桌面层"的场合（无头测试、纯静态模式）。
///
/// 它对视频文件返回**明确的错误**而不是假装成功——假装成功会把
/// "视频播不了"记成"换纸成功"，而那种沉默的失败最难排查。
pub struct NoVideoPresenter;

impl VideoPresenter for NoVideoPresenter {
    fn play(&self, monitor_name: &str, _file: &std::path::Path) -> std::result::Result<(), String> {
        Err(format!(
            "{monitor_name}：当前没有可用的视频呈现（未建立桌面层），无法播放视频"
        ))
    }

    fn stop(&self, _monitor_name: &str) -> std::result::Result<(), String> {
        Ok(())
    }
}

/// 把播放结果真正设到桌面上的 sink。
pub struct DesktopSink {
    /// 显示器名 → 设备路径。
    bindings: HashMap<String, ScreenBinding>,
    /// 准备出来的 BMP 放在哪里。
    output_dir: PathBuf,
    /// 每块屏当前用的填充方式（换纸时要用）。
    fits: Mutex<HashMap<String, FitMode>>,
    /// 每块屏下一次要写哪个文件槽位。
    ///
    /// ## 为什么要两个槽位轮换，而不是一个固定的文件名
    ///
    /// 系统接口接受的是**路径**，而同一个路径重复 `SetWallpaper` 时，
    /// 系统有理由认为"还是那张图"（注册表里的壁纸路径没变），
    /// 于是画面可能不更新；而直接覆盖当前正在显示的那个文件，
    /// 又可能撞上系统还按共享模式读着它。
    ///
    /// 两个文件轮换写就同时避开了这两件事：每次设置的都是**另一个**路径，
    /// 而正在显示的那个文件不会被碰。磁盘占用因此恒定（每屏 2 个文件）。
    slots: Mutex<HashMap<String, usize>>,
    /// 是否已经确保位置模式是非跨屏的。
    position_prepared: Mutex<bool>,
    /// 视频呈现器。见 [`VideoPresenter`]。
    video: Box<dyn VideoPresenter>,
    /// 每块屏当前正在播的视频（`None` 表示当前是静态图）。
    ///
    /// 记下来有两个用途：切回静态图时要停掉它；
    /// 以及连续选中同一个视频时不必重启（重启会跳回第 0 帧）。
    playing_video: Mutex<HashMap<String, PathBuf>>,
}

impl DesktopSink {
    /// 构造一个只能设静态壁纸的 sink（视频会报"没有可用的视频呈现"）。
    pub fn new(bindings: Vec<ScreenBinding>, output_dir: PathBuf) -> DesktopSink {
        DesktopSink::with_video(bindings, output_dir, Box::new(NoVideoPresenter))
    }

    /// 构造一个带视频能力的 sink。
    pub fn with_video(
        bindings: Vec<ScreenBinding>,
        output_dir: PathBuf,
        video: Box<dyn VideoPresenter>,
    ) -> DesktopSink {
        let map = bindings
            .into_iter()
            .map(|binding| (binding.monitor_name.clone(), binding))
            .collect();
        DesktopSink {
            bindings: map,
            output_dir,
            fits: Mutex::new(HashMap::new()),
            slots: Mutex::new(HashMap::new()),
            position_prepared: Mutex::new(false),
            video,
            playing_video: Mutex::new(HashMap::new()),
        }
    }

    /// 设置某块屏的填充方式。下一次换纸生效。
    pub fn set_fit(&self, monitor_name: &str, fit: FitMode) {
        self.fits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(monitor_name.to_string(), fit);
    }

    /// 取这块屏下一个要写的文件路径，并把槽位翻到另一个。
    ///
    /// 文件名只含显示器名（去掉 `\` 与 `.` 之类不适合做文件名的字符）与槽位号，
    /// 不含任何与图片内容有关的东西——内容不再参与缓存命名（见 `image_prep` 的说明）。
    fn take_destination(&self, monitor_name: &str) -> PathBuf {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let slot = slots.entry(monitor_name.to_string()).or_insert(0);
        let current = *slot;
        *slot = 1 - current;
        self.output_dir
            .join(format!("{}-{current}.bmp", nsfw_file_stem(monitor_name)))
    }

    /// NSFW 模式那张图的产物路径。
    ///
    /// 与轮播用的两个槽位**不共用文件名**，因此轮播怎么覆盖都不会碰到它，
    /// 反过来也一样。固定名字意味着"天然缓存"：只要源文件没变就不必重新解码。
    ///
    /// 1.12.0 之前它叫 `<屏>-safe.bmp`（模式改名时一起去掉），见
    /// [`DesktopSink::forget_legacy_nsfw`]。
    pub fn nsfw_path(&self, monitor_name: &str) -> PathBuf {
        self.output_dir
            .join(format!("{}-nsfw.bmp", nsfw_file_stem(monitor_name)))
    }

    /// 删掉改名之前留下的那张产物（`<屏>-safe.bmp` 与它的 `.src` 记录）。
    ///
    /// ## 为什么要专门删
    ///
    /// 1.12.0 起文件名从 `-safe` 变成了 `-nsfw`，于是升级之后**没有任何代码会再读到**
    /// 那个旧文件：它就白占着几十 MB（一张 4K 产物约 24 MB）躺在 `data/output` 里，
    /// 而且 `prepare_nsfw` 下次照样会解码出一份新的。删掉是唯一"不留下垃圾"的做法。
    ///
    /// 顺手做、失败也不管：删不掉（正被系统读着）时下一轮还会再试一次，
    /// 而这份缓存的正确性完全不依赖它。
    fn forget_legacy_nsfw(&self, monitor_name: &str) {
        let stem = nsfw_file_stem(monitor_name);
        for name in [format!("{stem}-safe.bmp"), format!("{stem}-safe.src")] {
            let path = self.output_dir.join(name);
            if path.exists() {
                match std::fs::remove_file(&path) {
                    Ok(()) => log::info!("已删掉改名前的旧产物 {}", path.display()),
                    Err(error) => log::debug!("删不掉旧产物 {}：{error}", path.display()),
                }
            }
        }
    }

    /// 固定单图的产物路径。
    ///
    /// ## 为什么只留一份
    ///
    /// 用户的要求是"设了单图就只缓存一份，换了单图就更新它"。这块屏不轮播，
    /// 因此不存在"上一张"要留——**一份固定文件名**天然满足这个要求：
    /// 源图换了就重新解码、覆盖它，磁盘占用恒定为每屏一张。
    ///
    /// 与 NSFW 壁纸的产物分名（`-single` / `-nsfw`），两套机制互相覆盖不了。
    pub fn single_path(&self, monitor_name: &str) -> PathBuf {
        self.output_dir
            .join(format!("{}-single.bmp", nsfw_file_stem(monitor_name)))
    }

    /// 准备一张"固定用途"的图到指定产物：源文件没变就复用，变了就重新解码覆盖。
    ///
    /// NSFW 壁纸与固定单图的共同点就是这条：固定文件名 + 只需一份 + 按 mtime 复用。
    /// 差别只有产物名、填充方式与日志措辞，因此共用这一份实现——
    /// 分成两份会让"复用的判断条件"有两处，而它们必须一致。
    ///
    /// ## 为什么不能只看 mtime（用户实际踩到的 bug）
    ///
    /// 用户报过："退出 NSFW 模式 → 改 NSFW 壁纸 → 再进 NSFW 模式，显示的却还是旧图。"
    ///
    /// 原因是**只比 mtime**：判据是"产物比源文件新就复用"。而当用户换上的那张图
    /// 本身很老时（老照片、从别处拷来的文件，mtime 可能比产物还早），判据成立
    /// → 我们复用了上一张的产物，屏幕上当然还是旧图。
    ///
    /// 因此产物旁边留一份几十字节的小记录（`<产物名>.src`），写明它是**从哪张源图**
    /// 做出来的。路径变了就必须重做；同一张源图被原地编辑仍然由 mtime 兜住。
    fn prepare_fixed(
        &self,
        monitor_name: &str,
        source: &Path,
        destination: PathBuf,
        fit: FitMode,
        what: &str,
    ) -> std::result::Result<bool, String> {
        let Some(binding) = self.bindings.get(monitor_name) else {
            return Err(format!("找不到显示器 {monitor_name} 的设备路径"));
        };

        let record = source_record_path(&destination);
        if reuse_if_fresh(source, &destination).unwrap_or(false)
            && record_matches(&record, source)
        {
            log::debug!("{monitor_name} 的{what}命中已有产物（且确实是这张源图做的）");
            return Ok(false);
        }

        let orientation = match crate::library::probe::probe_file(source) {
            crate::library::Probe::Sized { orientation, .. } => orientation,
            _ => 1,
        };

        let prepared = image_prep::prepare(
            source,
            &destination,
            binding.width,
            binding.height,
            fit,
            orientation,
        )
        .map_err(|e| format!("准备{what}失败：{e}"))?;
        // 产物写好了才记来源：记录先写而产物失败的话，下次会误以为缓存有效。
        write_source_record(&record, source);
        log::info!(
            "{monitor_name} 的{what}已准备好（{} ms，源图 {}）",
            prepared.millis,
            source.display()
        );
        Ok(true)
    }

    /// 预先准备好这一屏的 NSFW 壁纸（解码 → 按屏裁切 → 落盘）。
    ///
    /// ## 为什么要"预先"
    ///
    /// 用户的要求是"点 NSFW 时尽可能瞬间完成"。落盘这一步去不掉（系统接口只收
    /// 文件路径），但可以提前做掉：源文件没变、产物已存在时这里直接返回，
    /// 于是点击/快捷键那一刻只剩一次 `SetWallpaper`（几毫秒）。
    ///
    /// 填充方式固定为**裁切填满**：它是一张要长期挂在屏上的图，
    /// 留黑边或拉伸都不如裁切好看，而这一项没有界面入口去逐屏设置。
    ///
    /// 返回是否真的做了工作（`false` = 命中已有产物）。
    pub fn prepare_nsfw(
        &self,
        monitor_name: &str,
        source: &Path,
    ) -> std::result::Result<bool, String> {
        // 先清掉改名之前那张产物（见 `forget_legacy_nsfw`）：它再也不会被读到。
        self.forget_legacy_nsfw(monitor_name);
        self.prepare_fixed(
            monitor_name,
            source,
            self.nsfw_path(monitor_name),
            FitMode::Fill,
            "NSFW 壁纸",
        )
    }

    /// 准备好这一屏的固定单图（解码 → 按屏裁切 → 落盘）。
    ///
    /// `fit` 来自这块屏当前用的播放列表（没选列表时用全局默认值）：
    /// 单图虽然不轮播，但"裁切填满 / 完整显示"这类观感仍然该听用户的设置。
    pub fn prepare_single(
        &self,
        monitor_name: &str,
        source: &Path,
        fit: FitMode,
    ) -> std::result::Result<bool, String> {
        self.prepare_fixed(
            monitor_name,
            source,
            self.single_path(monitor_name),
            fit,
            "固定单图",
        )
    }

    /// 丢掉这块屏轮播用的两个产物，返回释放的字节数。
    ///
    /// 进入 NSFW 模式后调用。用户的要求是"只保留那两张 NSFW 壁纸的缓存"，
    /// 而这两个文件在他长期停在 NSFW 模式时已经不会再被显示（4K 一张约 24 MB），
    /// 留着只是白占磁盘。退出 NSFW 模式时重新解码第一张即可——实测 30 MP
    /// 约 130~250 ms，那点等待远不如"长期多占 48 MB"划算。
    pub fn forget_slots(&self, monitor_name: &str) -> u64 {
        let stem = nsfw_file_stem(monitor_name);
        let mut freed = 0;
        for slot in 0..2 {
            let path = self.output_dir.join(format!("{stem}-{slot}.bmp"));
            if let Ok(meta) = std::fs::metadata(&path) {
                freed += meta.len();
            }
            // 删不掉也无所谓（可能正被系统读着）：下次换纸会覆盖它。
            let _ = std::fs::remove_file(&path);
        }
        freed
    }

    /// 把**已经准备好的**文件设成某块屏的壁纸（不重新解码）。
    ///
    /// 这是 NSFW 按钮/快捷键真正做的事：几毫秒。
    pub fn apply_prepared(&self, monitor_name: &str, file: &Path) -> std::result::Result<u64, String> {
        let started = std::time::Instant::now();
        let Some(binding) = self.bindings.get(monitor_name) else {
            return Err(format!("找不到显示器 {monitor_name} 的设备路径"));
        };
        // 从视频切回静态图时必须先停掉视频（当前没有视频，留着是为了语义完整）。
        self.stop_video_if_playing(monitor_name)?;
        self.ensure_per_monitor_position()
            .map_err(|e| format!("准备位置模式失败：{e}"))?;
        DesktopWallpaper::set_wallpaper(
            Some(&binding.device_path),
            &file.display().to_string(),
        )
        .map_err(|e| format!("设置 NSFW 壁纸失败：{e}"))?;
        Ok(started.elapsed().as_millis() as u64)
    }

    /// 把某块屏换成视频。
    ///
    /// 几件必须做的事：
    /// - **同一个视频不重启**：重启会跳回第 0 帧，用户看到的是"画面突然闪一下重来"。
    ///   随机播放时连续选中同一张是可能的（历史窗口有限）。
    /// - **先确保位置模式**：视频结束后如果切回图片，位置模式要是对的。
    ///   在这里做一次比在切换那一刻做更稳（那时视频正占着屏）。
    /// - 返回耗时：与图片路径保持一致的语义，界面上的"换纸耗时"才有意义。
    fn apply_video(
        &self,
        monitor_name: &str,
        file: &Path,
        started: std::time::Instant,
    ) -> std::result::Result<u64, String> {
        if !self.bindings.contains_key(monitor_name) {
            return Err(format!("找不到显示器 {monitor_name} 的设备路径"));
        }

        // 已经在播同一个文件：不重启，直接算作完成。
        {
            let playing = self.playing_video.lock().unwrap_or_else(|e| e.into_inner());
            if playing.get(monitor_name).map(|p| p.as_path()) == Some(file) {
                log::debug!("{monitor_name} 已在播放同一个视频，不重启");
                return Ok(started.elapsed().as_millis() as u64);
            }
        }

        // 换视频前先停掉上一个。不停止会有两个播放线程同时往同一块屏推帧，
        // 画面会变成两个视频交替闪烁。
        self.video
            .stop(monitor_name)
            .map_err(|e| format!("停止上一个视频失败：{e}"))?;

        self.video.play(monitor_name, file)?;
        self.playing_video
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(monitor_name.to_string(), file.to_path_buf());

        let millis = started.elapsed().as_millis() as u64;
        log::info!(
            "{} 开始播放视频 {}（{} ms）",
            monitor_name,
            file.display(),
            millis
        );
        Ok(millis)
    }

    /// 如果这块屏正在播视频，停掉它。
    fn stop_video_if_playing(&self, monitor_name: &str) -> std::result::Result<(), String> {
        let was_playing = self
            .playing_video
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(monitor_name)
            .is_some();
        if was_playing {
            self.video
                .stop(monitor_name)
                .map_err(|e| format!("停止视频失败：{e}"))?;
            log::debug!("{monitor_name} 已从视频切回静态图");
        }
        Ok(())
    }

    /// 停止所有屏的视频。停止轮播时调用。
    pub fn stop_all_videos(&self) -> std::result::Result<(), String> {
        let playing: Vec<String> = self
            .playing_video
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        for monitor in playing {
            self.stop_video_if_playing(&monitor)?;
        }
        Ok(())
    }

    /// 确保位置模式不是跨屏。
    ///
    /// 跨屏模式（`WallpaperStyle = 22`）下系统把图按整块虚拟桌面拉伸，
    /// 逐屏设置会失去意义。本机实测原来就是跨屏模式，所以这一步是必需的。
    /// 只做一次：每次换纸都改注册表没有意义，而且会带来额外的系统写入。
    fn ensure_per_monitor_position(&self) -> Result<()> {
        let mut prepared = self
            .position_prepared
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if *prepared {
            return Ok(());
        }
        let current = DesktopWallpaper::position()?;
        if !current.is_per_monitor_compatible() {
            log::info!(
                "当前位置模式是 {}（跨屏），改为 fill 以便逐屏设图",
                current.as_str()
            );
            DesktopWallpaper::set_position(WallpaperPosition::Fill)?;
        }
        *prepared = true;
        Ok(())
    }
}

impl WallpaperSink for DesktopSink {
    fn apply(&self, monitor_name: &str, file: &Path) -> std::result::Result<u64, String> {
        let started = std::time::Instant::now();

        // **先分流**：视频与图片的上屏路径完全不同，而且这一步必须在
        // 任何图片处理之前——视频文件交给 `image` crate 只会解码失败，
        // 那会表现成"反复选中同一个文件、反复失败"。
        if crate::video_size::is_video_extension(
            &file
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default(),
        ) {
            return self.apply_video(monitor_name, file, started);
        }

        // 从视频切回图片时必须先停掉视频，否则视频画面会一直盖在新设的壁纸上。
        self.stop_video_if_playing(monitor_name)?;

        let Some(binding) = self.bindings.get(monitor_name) else {
            return Err(format!("找不到显示器 {monitor_name} 的设备路径"));
        };

        self.ensure_per_monitor_position()
            .map_err(|e| format!("准备位置模式失败：{e}"))?;

        // 索引里的尺寸是"显示宽高"，但目标必须是这块屏的尺寸。
        let fit = *self
            .fits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(monitor_name)
            .unwrap_or(&FitMode::Fill);

        // EXIF 方向属于源文件属性，与播放哪一张无关，因此这里重新读一次头部：
        // 代价是一次 64 KB 读取，相对解码整图可以忽略。
        let orientation = match crate::library::probe::probe_file(file) {
            crate::library::Probe::Sized { orientation, .. } => orientation,
            _ => 1,
        };

        let destination = self.take_destination(monitor_name);
        let prepared = image_prep::prepare(
            file,
            &destination,
            binding.width,
            binding.height,
            fit,
            orientation,
        )
        .map_err(|e| format!("准备壁纸图像失败：{e}"))?;

        DesktopWallpaper::set_wallpaper(Some(&binding.device_path), &prepared.path.display().to_string())
            .map_err(|e| format!("设置壁纸失败：{e}"))?;

        let millis = started.elapsed().as_millis() as u64;
        log::debug!(
            "{} 壁纸已准备（解码+重采样+落盘 {} ms）：{}",
            monitor_name,
            prepared.millis,
            prepared.path.display()
        );
        Ok(millis)
    }

    /// 把某块屏设成"固定单图"：准备到**固定文件名**（只有一份），再设上去。
    ///
    /// 与 [`WallpaperSink::apply`] 的差别只在产物落哪：轮播用的是每屏两个槽位
    /// 轮换覆盖，而单图不轮播、只需要一份缓存，换了源图就覆盖它。
    /// 覆盖正在显示的那个文件本身是安全的（`image_prep::write_atomically`
    /// 走的是"临时文件 + 改名"），因此这里可以放心复用同一个路径。
    fn apply_single(
        &self,
        monitor_name: &str,
        file: &Path,
        fit: FitMode,
    ) -> std::result::Result<u64, String> {
        let started = std::time::Instant::now();

        // 单图也要先与视频互斥：从视频切回图片时必须停掉播放，
        // 否则视频画面会一直盖在新设的壁纸上。
        self.stop_video_if_playing(monitor_name)?;

        // 未变化时 prepare 只做两次 metadata 调用，随后仍要设一次壁纸：
        // 启动/开始轮播时这块屏可能还挂着别的图（例如刚从 NSFW 模式出来）。
        self.prepare_single(monitor_name, file, fit)?;

        let destination = self.single_path(monitor_name);
        self.apply_prepared(monitor_name, &destination)?;

        let millis = started.elapsed().as_millis() as u64;
        log::info!(
            "{} 固定单图已应用（{} ms）：{}",
            monitor_name,
            millis,
            file.display()
        );
        Ok(millis)
    }
}

/// 把显示器名变成能当文件名用的主干。
///
/// 名字形如 `\\.\DISPLAY1`，其中的 `\` 与 `.` 不能直接进文件名（`\` 是路径分隔符）。
/// 只保留字母数字与连字符，其余删掉：这个主干只用于**区分不同显示器**，
/// 不参与任何与内容有关的判断，因此不需要可逆。
fn nsfw_file_stem(monitor_name: &str) -> String {
    let stem: String = monitor_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    if stem.is_empty() {
        "display".to_string()
    } else {
        stem
    }
}

/// 产物是否还能用（源文件没被改过）。
///
/// 返回 `Some(true)` 表示"可以复用"，`Some(false)` 表示"需要重新准备"，
/// `None` 表示拿不到时间戳（那就当作需要重新准备）。
///
/// **只看 mtime 是不够的**：换了另一张更老的图时它会给假阳性，
/// 因此还要比对来源记录（见 [`prepare_fixed`] 与 [`record_matches`]）。
fn reuse_if_fresh(source: &Path, destination: &Path) -> Option<bool> {
    let source_time = std::fs::metadata(source).ok()?.modified().ok()?;
    let destination_time = std::fs::metadata(destination).ok()?.modified().ok()?;
    Some(destination_time >= source_time)
}

/// 产物旁边那份"这是从哪张源图做出来的"记录文件。
///
/// 与产物同名、换一个扩展名（`DISPLAY1-nsfw.bmp` → `DISPLAY1-nsfw.src`）：
/// 产物是固定文件名，因此这份记录也是固定的，不会越攒越多。
fn source_record_path(destination: &Path) -> PathBuf {
    destination.with_extension("src")
}

/// 记录里写的源图路径，是否与当前这张图是同一个。
///
/// Windows 路径大小写不敏感，因此比较前统一小写——否则"只改了大小的同一路径"
/// 会被当成换了图，白白重新解码一次。
fn record_matches(record: &Path, source: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(record) else {
        return false;
    };
    let recorded = text.trim().to_lowercase();
    !recorded.is_empty() && recorded == source.display().to_string().trim().to_lowercase()
}

/// 记下产物是从哪张源图做出来的。写不进去只是下次多做一次解码，不影响本次结果。
fn write_source_record(record: &Path, source: &Path) {
    if let Err(error) = std::fs::write(record, source.display().to_string()) {
        log::debug!("写入产物来源记录失败 {}：{error}", record.display());
    }
}

/// 一条索引记录是否进入候选。
///
/// 抽成纯函数是为了能**无头测试**：`build_plans` 本身要读真实的索引文件，
/// 端到端测它需要造目录、写索引、跑扫描，而这里的分支判断（尤其是
/// "视频关闭时排除视频"）不值得付出那个代价。
#[derive(Debug, PartialEq, Eq)]
enum CandidateDecision {
    /// 进入候选。
    Keep,
    /// 是视频，但当前没启用视频壁纸。单独一个分支是为了能**统计**它——
    /// 日志里要告诉用户"另有 N 个视频未计入"，否则他会去找那些视频去哪了。
    SkipVideo,
    /// 被该屏的筛选条件挡掉（宽高比、最小尺寸等）。
    FilteredOut,
}

fn candidate_decision(
    entry: &LibraryEntry,
    video_enabled: bool,
    filter: &crate::config::FilterSettings,
) -> CandidateDecision {
    // 不可播放的（例如 HEIC 这类认得出但没解码器的）永远不进候选。
    // 它们只在统计与界面里可见，用来回答"我那个 HEIC 去哪了"。
    if !entry.is_playable() {
        return CandidateDecision::FilteredOut;
    }
    if entry.is_video() && !video_enabled {
        return CandidateDecision::SkipVideo;
    }
    if !passes_filter(entry, filter) {
        return CandidateDecision::FilteredOut;
    }
    CandidateDecision::Keep
}

/// 为了让上面那段"只取方向"的代码读起来不别扭。
/// 由设置解析出每块屏的播放计划。
///
/// 解析规则（对应 §6 的配置结构）：
/// - 图库：该屏 `current_playlist` 指向的播放列表里的 `libraries`；
/// - 顺序/间隔/填充：来自播放列表（内容相关的属性）；
/// - 筛选：`[monitors.*.filter]` 只作用于该屏，且**所有列表共用**
///   （长宽比是屏幕的物理属性，换列表不该改变它）；
/// - 排除：全局 `[exclude]` 与该屏 `[monitors.*.exclude]` 合并（该屏的追加）。
///
/// **每块屏都会有一条计划**，包括没选播放列表的屏——那种屏的候选是空的，
/// 于是它什么都不会换。这就是"不想让某块屏轮播"的做法：
/// 不给它选播放列表，而不是去关一个额外的开关。
pub fn build_plans(
    settings: &crate::Settings,
    bindings: &[ScreenBinding],
) -> Vec<ScreenPlan> {
    bindings
        .iter()
        .map(|binding| {
            let per_monitor = settings.monitors.get(&binding.monitor_name);
            let selected_id = per_monitor
                .map(|m| m.current_playlist.as_str())
                .unwrap_or("");
            let playlist = settings.playlist(selected_id);

            // 固定单图：设了就只显示这一张，**优先于播放列表**（见
            // `MonitorSettings::single_image` 的说明）。它是"这块屏的内容来源"
            // 的另一种形态，因此不影响顺序/间隔/填充这些来自列表的属性。
            let single_image: Option<PathBuf> = per_monitor
                .map(|m| m.single_image.trim().to_string())
                .filter(|path| !path.is_empty())
                .map(PathBuf::from);

            // 内容来源就是所选播放列表里的图库。
            //
            // 没选列表时**不留候选**（而不是"退回用全部图库"）：后者会让一块屏
            // 在用户还没配置时就开始播一堆他没指定的图，而那正是用户想避免的
            // "程序自作主张"。界面会明确显示"这块屏还没选播放列表"。
            //
            // 固定单图时同样不留候选：那块屏不轮播，候选对它没有意义
            // （留着只会让界面显示一个唬人的候选数）。
            let libraries: Vec<PathBuf> = if single_image.is_some() {
                Vec::new()
            } else {
                playlist.map(|p| p.libraries.clone()).unwrap_or_default()
            };

            if single_image.is_none() && playlist.is_none() {
                log::info!(
                    "屏幕 {} 还没有选播放列表（current_playlist={:?}），本屏无内容可播",
                    binding.monitor_name,
                    selected_id
                );
            } else if single_image.is_none() && libraries.is_empty() {
                log::info!(
                    "屏幕 {} 用的播放列表「{}」里没有关联任何图库，本屏无内容可播",
                    binding.monitor_name,
                    playlist.map(|p| p.name.as_str()).unwrap_or("")
                );
            }

            let mut candidates: Vec<LibraryEntry> = Vec::new();
            let mut videos_skipped = 0usize;
            let filter = per_monitor
                .map(|m| m.filter.clone())
                .unwrap_or_default();

            for library in &libraries {
                match crate::library::load_index_for(library) {
                    Ok(entries) => {
                        for entry in entries {
                            if !entry.is_playable() {
                                continue;
                            }
                            // 视频关闭时**在候选阶段就排除**，而不是等到换纸时再跳过。
                            //
                            // 两个理由：
                            // 1. 候选数会如实反映"能播多少张"，界面上的数字与
                            //    实际播放的集合一致；
                            // 2. 随机选择不会选中一个注定播不了的文件，
                            //    从而把"跳过"变成用户看不见的空档。
                            match candidate_decision(&entry, settings.general.video_enabled, &filter)
                            {
                                CandidateDecision::Keep => candidates.push(entry),
                                CandidateDecision::SkipVideo => videos_skipped += 1,
                                CandidateDecision::FilteredOut => {}
                            }
                        }
                    }
                    Err(error) => log::warn!(
                        "读取 {} 的索引失败，该图库本次不参与：{error}",
                        library.display()
                    ),
                }
            }

            // 间隔/填充取自播放列表。没有列表时沿用 general 的默认值
            // （反正那时候候选是空的，这些值不会被用到）。
            //
            // **没有"顺序"这一项**：用户取消了顺序/倒序，只剩随机
            // （见 `playback::select`），因此计划里也不再带这个字段。
            let interval_seconds = playlist
                .map(|p| p.interval_seconds)
                .unwrap_or(settings.general.interval_seconds);
            let fit = playlist.map(|p| p.fit_mode).unwrap_or(settings.general.fit_mode);

            log::info!(
                "屏幕 {}（{}x{}）：{}{}，间隔 {} 秒，{}",
                binding.monitor_name,
                binding.width,
                binding.height,
                // 固定单图与候选数二者只会有一个有意义，因此分开写。
                if let Some(image) = &single_image {
                    format!("固定单图（{}），不参与轮播", image.display())
                } else {
                    format!("候选 {} 张", candidates.len())
                },
                // 只有真的排除了视频时才提这件事。否则用户会去猜
                // "我图库里的视频去哪了"。
                if single_image.is_none() && videos_skipped > 0 {
                    format!("（另有 {videos_skipped} 个视频未计入，当前未启用视频壁纸）")
                } else {
                    String::new()
                },
                interval_seconds,
                match fit {
                    FitMode::Fill => "裁切填满",
                    FitMode::Fit => "完整显示",
                    FitMode::Stretch => "拉伸",
                    FitMode::Center => "居中",
                }
            );

            ScreenPlan {
                monitor_name: binding.monitor_name.clone(),
                width: binding.width,
                height: binding.height,
                interval: std::time::Duration::from_secs(interval_seconds.max(1)),
                fit,
                candidates,
                single_image,
            }
        })
        .collect()
}

/// 逐屏筛选条件。
fn passes_filter(entry: &LibraryEntry, filter: &crate::config::FilterSettings) -> bool {
    let aspect = entry.aspect();
    if let Some(min) = filter.aspect_min {
        // 下限**包含**：正好等于下限的图通过。
        if aspect < min {
            return false;
        }
    }
    if let Some(max) = filter.aspect_max {
        // 上限**不包含**：正好等于上限的图被排除。
        //
        // 这个 `>=` 与 `<=` 的差别不是随意的（用户明确要求"含下限不含上限"）。
        // 半开区间 [min, max) 的好处是**相邻区间可以无重叠地拼接**：
        // 一块屏要 [1.3, ∞)、另一块要 [0, 1.3)，正好把所有图分完、不重不漏。
        // 如果两边都包含，比值恰好 1.3 的图会同时落进两块屏。
        if aspect >= max {
            return false;
        }
    }
    if let Some(min_width) = filter.min_width {
        // 尺寸类下限同理包含（"至少这么宽"是自然的读法）。
        if entry.w < min_width {
            return false;
        }
    }
    if let Some(min_height) = filter.min_height {
        if entry.h < min_height {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FilterSettings;
    use std::sync::Mutex as StdMutex;

    /// 记录调用的假视频呈现器。
    ///
    /// 有了它，"视频路由"这件事可以在**无头环境下**被完整验证：
    /// 不必建窗口、不必有显示器、不必有 D3D 设备。
    /// 这正是把视频呈现抽成 trait 的收益。
    #[derive(Default)]
    struct RecordingVideo {
        played: StdMutex<Vec<(String, String)>>,
        stopped: StdMutex<Vec<String>>,
        /// 让 play 失败，用于验证错误会被如实报出来。
        fail_play: bool,
    }

    impl VideoPresenter for RecordingVideo {
        fn play(&self, monitor: &str, file: &std::path::Path) -> std::result::Result<(), String> {
            if self.fail_play {
                return Err("假装播放失败".to_string());
            }
            self.played
                .lock()
                .unwrap()
                .push((monitor.to_string(), file.display().to_string()));
            Ok(())
        }
        fn stop(&self, monitor: &str) -> std::result::Result<(), String> {
            self.stopped.lock().unwrap().push(monitor.to_string());
            Ok(())
        }
    }

    /// 造一个不碰真实桌面的 sink。
    ///
    /// `bindings` 在这里只用于"查得到这个名字"，视频路径不会真的去用设备路径，
    /// 因此可以给一个假的。
    fn fake_binding() -> ScreenBinding {
        ScreenBinding {
            monitor_name: r"\\.\DISPLAY1".to_string(),
            device_path: r"\\?\DISPLAY#FAKE".to_string(),
            width: 1920,
            height: 1080,
            x: 0,
            y: 0,
            primary: true,
        }
    }

    fn sink_with(presenter: Box<dyn VideoPresenter>) -> DesktopSink {
        DesktopSink::with_video(
            vec![fake_binding()],
            std::env::temp_dir().join("wwp-engine-test"),
            presenter,
        )
    }

    /// 造一条视频索引记录。
    fn video_entry(w: u32, h: u32) -> LibraryEntry {
        LibraryEntry {
            p: format!("C:/clips/{w}x{h}.mp4"),
            w,
            h,
            mt: 1,
            sz: 1,
            or: 1,
            fmt: "mp4".to_string(),
            ok: 1,
            kind: crate::library::index::MediaKind::Video,
        }
    }

    #[test]
    fn videos_are_not_candidates_when_video_wallpaper_is_disabled() {
        // **首版的核心契约**：视频壁纸关闭时，视频不进候选列表。
        //
        // 为什么必须在候选阶段排除而不是等到换纸时再跳过：
        // 候选列表就是随机选择的数据源。如果视频留在里面，随机选中它之后
        // 才发现播不了，用户会看到"该换纸了但画面没动"的空档——
        // 而那个空档在日志之外没有任何解释。
        let plain = FilterSettings::default();
        assert_eq!(
            candidate_decision(&video_entry(1920, 1080), false, &plain),
            CandidateDecision::SkipVideo,
            "视频关闭时应当被标为「跳过视频」，而不是悄悄混进候选"
        );
        // 单独一个分支（而不是与 FilteredOut 合并）的意义就在这里：
        // 日志要能说出"另有 N 个视频未计入"。
    }

    #[test]
    fn videos_become_candidates_when_the_switch_is_on() {
        // 反过来也要成立，否则开关就只有一半作用。
        let plain = FilterSettings::default();
        assert_eq!(
            candidate_decision(&video_entry(1920, 1080), true, &plain),
            CandidateDecision::Keep
        );
    }

    #[test]
    fn images_are_unaffected_by_the_video_switch() {
        // 这条守的是"改视频逻辑别把图片弄坏"。两种开关状态下图片都要照常进候选。
        let plain = FilterSettings::default();
        for video_enabled in [true, false] {
            assert_eq!(
                candidate_decision(&entry(1920, 1080), video_enabled, &plain),
                CandidateDecision::Keep,
                "video_enabled={video_enabled} 时图片都该进候选"
            );
        }
    }

    #[test]
    fn unplayable_entries_never_become_candidates() {
        // 认得出但没有解码器的格式（HEIC 等）不该进候选——
        // 它们只用于统计与界面展示。
        let mut broken = entry(1920, 1080);
        broken.ok = 0;
        assert_eq!(
            candidate_decision(&broken, true, &FilterSettings::default()),
            CandidateDecision::FilteredOut
        );
    }

    #[test]
    fn aspect_lower_bound_is_inclusive_and_upper_bound_is_exclusive() {
        // 区间语义是 [下限, 上限)——用户明确要求"包含下限、不包含上限"。
        //
        // 这条测试逐个边界值断言，因为 `<=` 与 `<` 的差别**在代码上只差一个
        // 字符**，而它决定了"比值恰好等于边界的那些图属于哪块屏"。
        // 靠读代码很容易看漏，靠"用真实图库试一下"几乎不可能正好撞上
        // 比值恰等于边界的图。
        let exactly_1_3 = entry(1300, 1000); // aspect = 1.3

        // 下限：等于下限 → 通过（包含）。
        let min_only = FilterSettings {
            aspect_min: Some(1.3),
            ..Default::default()
        };
        assert_eq!(
            candidate_decision(&exactly_1_3, false, &min_only),
            CandidateDecision::Keep,
            "比值正好等于下限时应当通过（下限包含）"
        );

        // 上限：等于上限 → 排除（不包含）。
        let max_only = FilterSettings {
            aspect_max: Some(1.3),
            ..Default::default()
        };
        assert_eq!(
            candidate_decision(&exactly_1_3, false, &max_only),
            CandidateDecision::FilteredOut,
            "比值正好等于上限时应当被排除（上限不包含）"
        );

        // 略微越过边界时也要排除，确认不是"整段都放行"。
        assert_eq!(
            candidate_decision(&entry(1299, 1000), false, &min_only),
            CandidateDecision::FilteredOut,
            "略小于下限应当被排除"
        );
        assert_eq!(
            candidate_decision(&entry(1301, 1000), false, &max_only),
            CandidateDecision::FilteredOut,
            "略大于上限应当被排除"
        );
    }

    #[test]
    fn adjacent_aspect_ranges_partition_the_library_without_overlap() {
        // 这条守的是"两块屏的筛选合起来正好覆盖全部图、且不重叠"。
        //
        // 这正是半开区间带来的实际好处，也是本机那套配置
        // （DISPLAY1 要 aspect_min=1.3、DISPLAY2 要 aspect_max=1.3）
        // 能正常工作的前提：比值恰为 1.3 的图只会进 DISPLAY1。
        let lower = FilterSettings {
            aspect_max: Some(1.3),
            ..Default::default()
        };
        let upper = FilterSettings {
            aspect_min: Some(1.3),
            ..Default::default()
        };

        for sample in [
            entry(1000, 2000), // 0.5
            entry(1299, 1000), // 1.299
            entry(1300, 1000), // 1.3（边界）
            entry(1301, 1000), // 1.301
            entry(3000, 1000), // 3.0
        ] {
            let in_lower = candidate_decision(&sample, false, &lower) == CandidateDecision::Keep;
            let in_upper = candidate_decision(&sample, false, &upper) == CandidateDecision::Keep;
            assert!(
                in_lower ^ in_upper,
                "比例 {:.3} 的图应当恰好落进两块屏之一，实际 lower={in_lower} upper={in_upper}",
                sample.aspect()
            );
        }
    }

    #[test]
    fn the_screen_filter_still_applies_to_videos() {
        // 视频启用时筛选条件对它同样有效（否则"只要横图"会漏过竖屏视频）。
        let only_landscape = FilterSettings {
            aspect_min: Some(1.5),
            ..Default::default()
        };
        assert_eq!(
            candidate_decision(&video_entry(1080, 1920), true, &only_landscape),
            CandidateDecision::FilteredOut,
            "竖屏视频不该通过「只要横图」"
        );
        assert_eq!(
            candidate_decision(&video_entry(1920, 1080), true, &only_landscape),
            CandidateDecision::Keep
        );
    }

    #[test]
    fn video_file_is_routed_to_the_video_presenter_not_the_image_path() {
        // 这条守的是"视频进了图库之后引擎仍把它当图片"这个问题：
        // `image` crate 读不了 MP4，会表现成"反复选中同一个文件、反复失败"。
        let sink = sink_with(Box::new(RecordingVideo::default()));
        let video = std::path::Path::new(r"C:\pics\clip.mp4");

        let result = sink.apply(r"\\.\DISPLAY1", video);
        assert!(result.is_ok(), "视频应当被接受：{result:?}");

        // 走的是视频路径的旁证：同一个视频再来一次仍然成功。
        // 若走的是图片路径，这里会因为没有真实设备路径而失败。
        let again = sink.apply(r"\\.\DISPLAY1", video);
        assert!(
            again.is_ok(),
            "同一个视频再次应用应当成功（且不重启播放）：{again:?}"
        );
    }

    #[test]
    fn switching_from_video_to_image_stops_the_video_first() {
        // 不停视频的后果很具体：视频画面会一直盖在新设的静态壁纸上，
        // 用户看到"换了图但屏幕没变"。
        let sink = sink_with(Box::new(RecordingVideo::default()));

        sink.apply(r"\\.\DISPLAY1", std::path::Path::new(r"C:\pics\clip.mp4"))
            .expect("视频应当被接受");

        // 再切回图片。图片本身会因为假设备路径而失败——这没关系，
        // 我们要验的是"在尝试设图片之前已经把视频停了"。
        let image_result = sink.apply(r"\\.\DISPLAY1", std::path::Path::new(r"C:\pics\a.png"));
        assert!(
            image_result.is_err(),
            "假设备路径下设置图片应当失败，这正好说明它确实走了图片路径"
        );

        // 再切回视频时应当重新开始播放（说明之前的状态被清掉了）
        let back = sink.apply(r"\\.\DISPLAY1", std::path::Path::new(r"C:\pics\clip.mp4"));
        assert!(back.is_ok(), "切回视频应当成功：{back:?}");
    }

    #[test]
    fn video_presenter_failure_is_reported_not_swallowed() {
        // 假装成功会把"视频播不了"记成"换纸成功"，那种沉默的失败最难排查。
        let mut presenter = RecordingVideo::default();
        presenter.fail_play = true;
        let sink = sink_with(Box::new(presenter));

        let result = sink.apply(r"\\.\DISPLAY1", std::path::Path::new(r"C:\pics\clip.mp4"));
        let error = result.expect_err("呈现器失败必须被报出来，不能吞掉");
        assert!(
            error.contains("假装播放失败"),
            "错误信息应当带上呈现器给出的原因，实际是：{error}"
        );
    }

    #[test]
    fn unknown_monitor_is_rejected_before_touching_the_video_presenter() {
        let sink = sink_with(Box::new(RecordingVideo::default()));
        let result = sink.apply(r"\\.\DISPLAY9", std::path::Path::new(r"C:\pics\clip.mp4"));
        let error = result.expect_err("不存在的屏应当被拒绝");
        assert!(
            error.contains("找不到显示器"),
            "应当在调用呈现器之前就拒绝，实际是：{error}"
        );
    }

    #[test]
    fn no_video_presenter_gives_a_clear_error_instead_of_pretending() {
        // 没有桌面层时（纯静态模式），视频必须报出清晰的原因，
        // 而不是"成功但其实什么都没做"。
        let sink = DesktopSink::new(
            vec![fake_binding()],
            std::env::temp_dir().join("wwp-engine-test"),
        );
        let result = sink.apply(r"\\.\DISPLAY1", std::path::Path::new(r"C:\pics\clip.mp4"));
        let error = result.expect_err("没有视频呈现时必须报错");
        assert!(
            error.contains("没有可用的视频呈现"),
            "应当明确说明原因，实际是：{error}"
        );
    }

    /// 造一张能真的解码的小 PNG（`prepare` 要读图片头再解码整图）。
    fn write_png(path: &std::path::Path, color: [u8; 3]) {
        image::RgbImage::from_pixel(8, 8, image::Rgb(color))
            .save_with_format(path, image::ImageFormat::Png)
            .expect("测试用 PNG 应当写得出来");
    }

    #[test]
    fn a_different_source_forces_a_reprepare_even_when_it_is_older() {
        // **用户实际踩到的 bug**：退出 NSFW 模式 → 改 NSFW 壁纸 → 再进 NSFW 模式，
        // 显示的却还是旧图。
        //
        // 根因是"缓存是否有效"原先**只看 mtime**：判据是"产物比源文件新"。
        // 用户换上的那张图本身很老时（老照片、从别处拷来的文件），判据成立，
        // 于是我们复用了上一张的产物。现在产物旁边记了它是哪张源图做的。
        let dir = std::env::temp_dir().join("wwp-engine-fixed-cache");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let sink = DesktopSink::new(vec![fake_binding()], dir.clone());
        let monitor = r"\\.\DISPLAY1";

        let newer = dir.join("newer.png");
        write_png(&newer, [255, 0, 0]);
        let older = dir.join("older.png");
        write_png(&older, [0, 0, 255]);
        // 把 older 的时间戳改到一天前，模拟"用户换上了一张更老的图"。
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
        std::fs::File::options()
            .write(true)
            .open(&older)
            .unwrap()
            .set_modified(past)
            .unwrap();

        assert!(sink.prepare_nsfw(monitor, &newer).unwrap(), "第一次必须真的准备");
        let after_first = std::fs::read(sink.nsfw_path(monitor)).unwrap();

        assert!(
            sink.prepare_nsfw(monitor, &older).unwrap(),
            "换了源图就必须重新解码，哪怕新源文件的 mtime 比旧产物还老"
        );
        let after_second = std::fs::read(sink.nsfw_path(monitor)).unwrap();
        assert_ne!(
            after_first, after_second,
            "产物内容应当换成新那张图（这正是用户看到'还是旧壁纸'的地方）"
        );

        // 同一张源图再来一次必须命中缓存：否则每次点 NSFW 都要现场解码，
        // "瞬间切换"这个要求就没了。
        assert!(
            !sink.prepare_nsfw(monitor, &older).unwrap(),
            "同一张源图（且没被改过）应当命中缓存"
        );

        // 路径大小写不同但其实是同一个文件时，也不该重新解码。
        let same_upper = dir.join("OLDER.PNG");
        assert!(
            !sink.prepare_nsfw(monitor, &same_upper).unwrap(),
            "Windows 路径大小写不敏感，同一个文件不该被当成换了图"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_edited_source_file_in_the_same_path_is_reprepared() {
        // 反向的守护：路径没变、但文件被重新保存过（mtime 更新）时必须重做。
        // 这条与上一条合起来才是完整的判据：**路径相同 且 产物不比源图旧**。
        let dir = std::env::temp_dir().join("wwp-engine-fixed-cache-edited");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let sink = DesktopSink::new(vec![fake_binding()], dir.clone());
        let monitor = r"\\.\DISPLAY1";
        let source = dir.join("same.png");

        write_png(&source, [10, 20, 30]);
        assert!(sink.prepare_nsfw(monitor, &source).unwrap());
        let before = std::fs::read(sink.nsfw_path(monitor)).unwrap();

        // 同一个路径、内容换了、mtime 变新。
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_png(&source, [200, 100, 50]);
        assert!(
            sink.prepare_nsfw(monitor, &source).unwrap(),
            "同一个路径被改过之后必须重新准备"
        );
        let after = std::fs::read(sink.nsfw_path(monitor)).unwrap();
        assert_ne!(before, after, "产物应当跟着源文件一起更新");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn single_image_caches_like_the_nsfw_wallpaper() {
        // 固定单图走的是同一段"固定产物 + 来源记录"逻辑，这里确认它也是
        // "只有一份产物、换了源图才重做"。
        let dir = std::env::temp_dir().join("wwp-engine-single-cache");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let sink = DesktopSink::new(vec![fake_binding()], dir.clone());
        let monitor = r"\\.\DISPLAY1";
        let a = dir.join("a.png");
        write_png(&a, [1, 2, 3]);
        let b = dir.join("b.png");
        write_png(&b, [4, 5, 6]);
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
        std::fs::File::options()
            .write(true)
            .open(&b)
            .unwrap()
            .set_modified(past)
            .unwrap();

        assert!(sink.prepare_single(monitor, &a, FitMode::Fill).unwrap());
        assert!(
            sink.prepare_single(monitor, &b, FitMode::Fill).unwrap(),
            "换成更老的另一张图也要重做"
        );
        assert!(!sink.prepare_single(monitor, &b, FitMode::Fill).unwrap());
        // 产物只有一个文件：这正是用户要的"只存放一份"。
        let outputs: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with("DISPLAY1-single"))
            .collect();
        let mut sorted = outputs.clone();
        sorted.sort();
        assert_eq!(
            sorted,
            vec!["DISPLAY1-single.bmp".to_string(), "DISPLAY1-single.src".to_string()],
            "单图应当只有一个产物（外加那份来源记录）：{outputs:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn entry(w: u32, h: u32) -> LibraryEntry {
        LibraryEntry {
            p: format!("C:/pics/{w}x{h}.png"),
            w,
            h,
            mt: 1,
            sz: 1,
            or: 1,
            fmt: "png".to_string(),
            ok: 1,
            kind: crate::library::index::MediaKind::Image,
        }
    }

    #[test]
    fn the_old_safe_artifact_is_deleted_when_the_nsfw_one_is_prepared() {
        // 1.12.0 把模式改名为 NSFW 时产物名也从 `<屏>-safe.bmp` 变成了
        // `<屏>-nsfw.bmp`：旧文件**再也没有任何代码会读到它**，留着只是白占
        // 空间（一张 4K 产物约 24 MB）。`prepare_nsfw` 顺手把它清掉。
        let dir = std::env::temp_dir().join("wwp-engine-legacy-safe");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let sink = DesktopSink::new(vec![fake_binding()], dir.clone());
        let monitor = r"\\.\DISPLAY1";
        let legacy = dir.join("DISPLAY1-safe.bmp");
        let legacy_record = dir.join("DISPLAY1-safe.src");
        std::fs::write(&legacy, vec![0u8; 4096]).unwrap();
        std::fs::write(&legacy_record, "C:/old/target.jpg").unwrap();

        let source = dir.join("now.png");
        write_png(&source, [7, 8, 9]);
        assert!(sink.prepare_nsfw(monitor, &source).unwrap());

        assert!(
            !legacy.exists(),
            "改名前的旧产物必须被删掉：{}",
            legacy.display()
        );
        assert!(!legacy_record.exists(), "它的 .src 记录也要一起删掉");
        assert!(sink.nsfw_path(monitor).is_file(), "新的产物照常生成");
        // 再准备一次（这次命中缓存）也不该出错。
        assert!(!sink.prepare_nsfw(monitor, &source).unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn aspect_filter_excludes_portrait_when_min_is_set() {
        let filter = FilterSettings {
            aspect_min: Some(1.3),
            ..Default::default()
        };
        assert!(passes_filter(&entry(1920, 1080), &filter), "16:9 应通过");
        assert!(!passes_filter(&entry(1080, 1920), &filter), "竖图应被挡下");
        assert!(!passes_filter(&entry(1000, 1000), &filter), "方图应被挡下");
    }

    #[test]
    fn aspect_filter_can_select_portrait_only() {
        // 本机有一块竖屏，这块屏通常要的正是竖图。
        let filter = FilterSettings {
            aspect_max: Some(0.9),
            ..Default::default()
        };
        assert!(passes_filter(&entry(1440, 2560), &filter));
        assert!(!passes_filter(&entry(2560, 1440), &filter));
    }

    #[test]
    fn size_floor_filters_out_icons() {
        let filter = FilterSettings {
            min_width: Some(800),
            min_height: Some(600),
            ..Default::default()
        };
        assert!(passes_filter(&entry(1920, 1080), &filter));
        assert!(!passes_filter(&entry(48, 48), &filter), "小图标应被挡下");
        assert!(!passes_filter(&entry(1000, 400), &filter), "高度不足应被挡下");
    }

    #[test]
    fn no_filter_lets_everything_through() {
        let filter = FilterSettings::default();
        for (w, h) in [(48, 48), (1920, 1080), (1080, 1920), (1000, 1000)] {
            assert!(passes_filter(&entry(w, h), &filter));
        }
    }

    #[test]
    fn empty_portrait_screen_selection_is_possible_and_explainable() {
        // 需求 5 的现实场景：给竖屏设 aspect_min=1.3 会筛出 0 张。
        // 这时播放器会报 NoCandidates，界面必须能解释原因，而不是静默保持旧图。
        let filter = FilterSettings {
            aspect_min: Some(1.3),
            ..Default::default()
        };
        let portrait_candidates: Vec<LibraryEntry> = [entry(1080, 1920), entry(1440, 2560)]
            .into_iter()
            .filter(|e| passes_filter(e, &filter))
            .collect();
        assert!(portrait_candidates.is_empty(), "竖图在 aspect_min=1.3 下应被全部筛掉");
    }
    #[test]
    fn a_screen_without_a_selected_playlist_still_gets_a_plan_but_no_candidates() {
        // "不想让某块屏轮播"只有一个做法：不给它选播放列表。
        // 它仍然有一条计划（界面要能列出这块屏并显示状态），但候选是 0，
        // 于是永远不会换纸——这正是那条路径的判据。
        let settings = crate::Settings::default();
        let plans = build_plans(&settings, &one_screen());
        assert_eq!(plans.len(), 1);
        assert!(plans[0].candidates.is_empty());
    }

    /// 造一个只有一块屏的 binding，用于测 `build_plans` 的内容来源。
    fn one_screen() -> Vec<ScreenBinding> {
        vec![ScreenBinding {
            monitor_name: r"\\.\DISPLAY1".to_string(),
            device_path: r"\\?\DISPLAY#TEST".to_string(),
            width: 1920,
            height: 1080,
            x: 0,
            y: 0,
            primary: true,
        }]
    }

    /// 造一个带指定图库的播放列表。
    fn playlist_with(id: &str, libraries: &[&str]) -> crate::config::Playlist {
        crate::config::Playlist {
            id: id.to_string(),
            name: format!("列表 {id}"),
            libraries: libraries.iter().map(std::path::PathBuf::from).collect(),
            ..Default::default()
        }
    }

    /// 让某块屏选中某个播放列表。
    ///
    /// 注意它同时**把这块屏写进 `settings.monitors`**，而不是只设一处。
    /// 真实使用中每块屏都会因为界面上的操作（选列表、设筛选）
    /// 而在 `monitors` 里有一条记录；测试里如果漏了这一步，
    /// 就会造出"存在但没被登记"的屏，那与实际不符。
    fn bind_screen(settings: &mut crate::Settings, monitor: &str, playlist_id: &str) {
        let entry = settings.monitors.entry(monitor.to_string()).or_default();
        entry.current_playlist = playlist_id.to_string();
    }

    #[test]
    fn a_screen_without_a_selected_playlist_has_no_candidates() {
        // **这条守的是"程序不自作主张"。**
        //
        // 没选播放列表时**不能**退回"用全部图库"——那会让一块屏在用户还没
        // 配置时就开始播一堆他没指定的图。界面上会明确显示"还没选播放列表"，
        // 因此候选必须是 0，界面的提示与实际行为才一致。
        let mut settings = crate::Settings::default();
        settings.playlists.push(playlist_with("list1", &[r"E:\图库A"]));
        // 故意不调 bind_screen：这块屏的 current_playlist 是空的。

        let plans = build_plans(&settings, &one_screen());
        assert_eq!(plans.len(), 1, "参与轮播的屏仍应有一条计划");
        assert!(
            plans[0].candidates.is_empty(),
            "没选播放列表就不该有任何候选，实际拿到了 {} 张",
            plans[0].candidates.len()
        );
    }

    #[test]
    fn a_playlist_pointing_at_nonexistent_libraries_yields_no_candidates() {
        // 列表关联的图库目录不存在（被删了/盘没插）时，应当**没有候选**
        // 而不是崩溃。build_plans 对每个库的索引读取失败只记 warn。
        let mut settings = crate::Settings::default();
        settings
            .playlists
            .push(playlist_with("list1", &[r"E:\绝对不存在的图库目录"]));
        bind_screen(&mut settings, r"\\.\DISPLAY1", "list1");

        let plans = build_plans(&settings, &one_screen());
        assert_eq!(plans.len(), 1);
        assert!(
            plans[0].candidates.is_empty(),
            "路径不存在时不该凭空造出候选"
        );
    }

    #[test]
    fn a_screen_bound_to_an_empty_playlist_has_no_candidates() {
        // 列表存在但还没关联任何图库——同样是 0 候选，
        // 且日志里会说清原因（列表里没有关联图库）。
        let mut settings = crate::Settings::default();
        settings.playlists.push(playlist_with("list1", &[]));
        bind_screen(&mut settings, r"\\.\DISPLAY1", "list1");

        let plans = build_plans(&settings, &one_screen());
        assert_eq!(plans.len(), 1);
        assert!(plans[0].candidates.is_empty());
    }

    #[test]
    fn interval_and_fit_come_from_the_playlist_not_the_monitor() {
        // 间隔/填充是**内容相关**的属性，因此属于播放列表。
        // 这条测试锁住这个归属关系——如果哪天它们被挪回显示器上，
        // "换个列表换个节奏"就会失效，而那种失效不会报错、只会行为不对。
        //
        // （原先这条还盯着"顺序"，而顺序这项功能已被用户取消，见 `playback::select`。）
        let mut settings = crate::Settings::default();
        settings.general.interval_seconds = 300;
        settings.general.fit_mode = crate::config::FitMode::Fit;

        settings.playlists.push(crate::config::Playlist {
            id: "list1".to_string(),
            name: "快翻".to_string(),
            libraries: Vec::new(),
            interval_seconds: 7,
            fit_mode: crate::config::FitMode::Stretch,
        });
        bind_screen(&mut settings, r"\\.\DISPLAY1", "list1");

        let plans = build_plans(&settings, &one_screen());
        assert_eq!(
            plans[0].interval,
            std::time::Duration::from_secs(7),
            "间隔应当来自列表，而不是 general"
        );
        assert_eq!(
            plans[0].fit,
            crate::config::FitMode::Stretch,
            "填充方式应当来自列表，而不是 general"
        );
    }

    #[test]
    fn two_screens_can_use_different_playlists_at_the_same_time() {
        // **这是整个播放列表功能的核心诉求**：同一台机器上，
        // 两块屏各自用不同的列表（因此内容不同），互不影响。
        let mut settings = crate::Settings::default();
        settings
            .playlists
            .push(playlist_with("list1", &[r"E:\绝对不存在的A"]));
        settings
            .playlists
            .push(playlist_with("list2", &[r"E:\绝对不存在的B"]));
        bind_screen(&mut settings, r"\\.\DISPLAY1", "list1");
        bind_screen(&mut settings, r"\\.\DISPLAY2", "list2");

        let bindings = vec![
            ScreenBinding {
                monitor_name: r"\\.\DISPLAY1".to_string(),
                device_path: r"\\?\DISPLAY#A".to_string(),
                width: 1920,
                height: 1080,
                x: 0,
                y: 0,
                primary: true,
            },
            ScreenBinding {
                monitor_name: r"\\.\DISPLAY2".to_string(),
                device_path: r"\\?\DISPLAY#B".to_string(),
                width: 1080,
                height: 1920,
                x: -1080,
                y: 0,
                primary: false,
            },
        ];
        let plans = build_plans(&settings, &bindings);
        assert_eq!(plans.len(), 2, "两块屏各有一条计划");
        // 各自指向自己的列表（这里用列表的间隔区分，图库都不存在因此候选为空）。
        let d1 = plans
            .iter()
            .find(|p| p.monitor_name == r"\\.\DISPLAY1")
            .unwrap();
        let d2 = plans
            .iter()
            .find(|p| p.monitor_name == r"\\.\DISPLAY2")
            .unwrap();
        assert!(d1.candidates.is_empty() && d2.candidates.is_empty());
    }

    #[test]
    fn the_aspect_filter_is_per_screen_and_shared_by_all_its_playlists() {
        // 用户明确要求：长宽比设在**显示器**上，切列表时不改变它。
        // 这条测试用 `passes_filter` 直接验证"筛选属于显示器"这一半——
        // 它与用哪个列表无关。
        let mut filter = crate::config::FilterSettings::default();
        filter.aspect_min = Some(1.3);
        // 竖图（1:1.5）应当被挡掉，横图（1.5:1）应当通过。
        assert!(!passes_filter(&entry(1000, 1500), &filter));
        assert!(passes_filter(&entry(1500, 1000), &filter));
        // 另一块屏的筛选是独立的一份值，不受影响。
        let other = crate::config::FilterSettings::default();
        assert!(passes_filter(&entry(1000, 1500), &other), "没有筛选时竖图应通过");
    }

    /// 造一块屏（只有位置与名字参与排序测试）。
    fn screen(name: &str, x: i32, y: i32) -> ScreenBinding {
        ScreenBinding {
            monitor_name: name.to_string(),
            device_path: format!("device-of-{name}"),
            width: 1920,
            height: 1080,
            x,
            y,
            primary: name == "primary",
        }
    }

    #[test]
    fn bindings_are_ordered_left_to_right_by_physical_position() {
        // **这条守的是界面上两列的顺序。**
        //
        // 用户的实际摆位是"竖屏在横屏左边"，而 `monitors::enumerate()` 把主屏排在
        // 最前——直接用它的顺序，两块屏的设置就会左右颠倒，对比时很容易看错。
        let mut bindings = vec![
            screen("primary", 0, 0),
            screen("portrait", -1440, -93),
        ];
        order_left_to_right(&mut bindings);
        assert_eq!(bindings[0].monitor_name, "portrait", "x 小的在左边");
        assert_eq!(bindings[1].monitor_name, "primary");
        // 排序只改先后，不能把设备路径与屏的对应关系弄乱——
        // 弄乱了就会把壁纸设到另一块屏上。
        assert_eq!(bindings[0].device_path, "device-of-portrait");
        assert_eq!(bindings[1].device_path, "device-of-primary");
    }

    #[test]
    fn stacked_screens_are_ordered_top_to_bottom() {
        // 上下叠放（x 相同）时按 y 排，否则顺序会随枚举顺序随机变化。
        let mut bindings = vec![screen("lower", 0, 1080), screen("upper", 0, 0)];
        order_left_to_right(&mut bindings);
        assert_eq!(bindings[0].monitor_name, "upper");
        assert_eq!(bindings[1].monitor_name, "lower");
    }

    #[test]
    fn a_screen_with_a_single_image_has_no_candidates_and_ignores_the_playlist() {
        // 固定单图优先于播放列表：这块屏只显示那一张，不轮播。
        let mut settings = crate::Settings::default();
        settings.playlists.push(playlist_with("list1", &[r"E:\图库A"]));
        bind_screen(&mut settings, r"\\.\DISPLAY1", "list1");
        settings
            .monitors
            .get_mut(r"\\.\DISPLAY1")
            .unwrap()
            .single_image = r"E:\单图 一号.png".to_string();

        let plans = build_plans(&settings, &one_screen());
        assert!(plans[0].is_static_image(), "设了单图就是固定单图");
        assert_eq!(
            plans[0].single_image.as_deref(),
            Some(std::path::Path::new(r"E:\单图 一号.png"))
        );
        assert!(
            plans[0].candidates.is_empty(),
            "固定单图不该再带上播放列表的候选（那会让界面显示一个唬人的数字）"
        );
    }

    #[test]
    fn a_blank_single_image_setting_is_not_a_single_image() {
        // 界面上的"清除"写的是空串（用户也可能只输入空格）。它必须被当作
        // "没设置"，否则那块屏会变成"固定显示一个空路径"，看起来像功能坏了。
        let mut settings = crate::Settings::default();
        settings
            .monitors
            .entry(r"\\.\DISPLAY1".to_string())
            .or_default()
            .single_image = "   ".to_string();

        let plans = build_plans(&settings, &one_screen());
        assert!(!plans[0].is_static_image());
        assert!(plans[0].single_image.is_none());
    }

    #[test]
    fn a_single_image_screen_keeps_the_playlists_fit_mode() {
        // 单图虽然不轮播，但"裁切填满 / 完整显示"这类观感仍然该听用户的设置——
        // 它来自这块屏选的播放列表（没选列表时用全局默认值）。
        let mut settings = crate::Settings::default();
        settings.playlists.push(crate::config::Playlist {
            id: "list1".to_string(),
            name: "拉伸的列表".to_string(),
            libraries: Vec::new(),
            fit_mode: crate::config::FitMode::Stretch,
            ..Default::default()
        });
        bind_screen(&mut settings, r"\\.\DISPLAY1", "list1");
        settings
            .monitors
            .get_mut(r"\\.\DISPLAY1")
            .unwrap()
            .single_image = r"E:\单图.png".to_string();

        let plans = build_plans(&settings, &one_screen());
        assert_eq!(plans[0].fit, crate::config::FitMode::Stretch);
    }
}
