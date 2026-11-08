//! 视频解码（Media Foundation）。
//!
//! ## 为什么用 Media Foundation 而不是 ffmpeg
//!
//! 用户最初的约束是"不能在项目外下载任何东西，所有资源要下载到项目本地"。
//! Media Foundation 是 **Windows 自带**的，零外部依赖、不需要往交付文件夹里塞
//! 几十 MB 的解码器二进制。代价是它只支持系统装了解码器的格式（H.264/HEVC/AV1
//! 在 Win11 上都有，但 MKV 容器、ProRes 之类需要另装）。
//!
//! ## 这一版的定位：解码到内存
//!
//! 输出去掉跨距填充后的**紧凑 BGRA 像素**，交给上层上传到合成表面。
//! 没有走 DXVA 硬件解码：那条路要 `MFCreateDXGIDeviceManager` 把 D3D 设备交给
//! 解码器，而 D3D 设备目前在桌面层合成线程上、与这里跨线程。先做成"CPU 解码 +
//! 上传"，把整条链跑通；性能不够时再把设备管理接起来（4K 30fps 的 CPU 解码
//! 实测约 10-20 ms/帧，单屏够用、多屏会吃紧）。
//!
//! ## 跨距（stride）必须从媒体类型读，不能猜
//!
//! 实测抓到的坑，值得写下来：1920x1080 的 RGB32 缓冲长度是 8355840，
//! 而"紧凑"尺寸是 8294400。我据此断定"每行有 64 字节填充"——**错了**。
//! `MF_MT_DEFAULT_STRIDE` 报的是 7680（正好等于 1920*4），没有填充；
//! 那 61440 字节的差值是 8 行的大小，来自**缓冲容量大于所需**。
//!
//! 所以规则是：信媒体类型报的跨距，拷贝时把长度夹到 `宽*4*高`。
//! 既不能按紧凑跨距直接读整块缓冲（会读到未定义内容），
//! 也不能把容量富余误当行填充去逐行抽取（那样反而把画面读斜）。

use std::time::{Duration, Instant};

use windows::core::HSTRING;
use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, IMFSourceReader, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS,
    MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
    MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_SOURCE_READER_FIRST_AUDIO_STREAM,
    MFAudioFormat_PCM, MFMediaType_Audio,
    MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MFMediaType_Video, MFVideoFormat_RGB32, MFCreateAttributes,
    MFCreateMediaType, MFCreateSourceReaderFromURL,
};

use crate::{Error, Result};

/// MFStartup 的版本号。`MF_VERSION = MF_SDK_VERSION << 16 | MF_API_VERSION`，
/// 即 `0x0002 << 16 | 0x0070`。传错会直接失败，因此把出处写在这里。
const MF_VERSION: u32 = 0x0002_0070;

/// 一帧解出来的图像。
#[derive(Clone)]
pub struct VideoFrame {
    /// 紧凑排列的 BGRA 像素（`RGB32` 在内存里的字节顺序实际是 B,G,R,A）。
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// 该帧在视频里的时间戳（100 纳秒为单位）。
    pub timestamp_100ns: i64,
    /// 帧时长（100 纳秒）。
    pub duration_100ns: i64,
}

impl VideoFrame {
    pub fn len(&self) -> usize {
        self.pixels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pixels.is_empty()
    }

    /// 该帧应当显示的时刻（秒）。
    pub fn seconds(&self) -> f64 {
        self.timestamp_100ns as f64 / 10_000_000.0
    }
}

/// 视频的静态信息。
#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    /// 帧率（分子/分母）。
    pub frame_rate: (u32, u32),
    /// 每行字节数（含可能的填充）。
    pub stride: usize,
    /// 是否自下而上存储。
    pub bottom_up: bool,
}

impl VideoInfo {
    /// 帧率换算成"每秒多少帧"。
    pub fn fps(&self) -> f64 {
        if self.frame_rate.1 == 0 {
            0.0
        } else {
            self.frame_rate.0 as f64 / self.frame_rate.1 as f64
        }
    }

    /// 紧凑排列一帧需要多少字节。
    pub fn tight_frame_bytes(&self) -> usize {
        self.width as usize * 4 * self.height as usize
    }
}

/// Media Foundation 运行时。
///
/// 持有它就是"MF 已初始化"。用 RAII 而不是让调用方记得配对
/// `MFStartup`/`MFShutdown`：漏掉 `MFShutdown` 会让进程退出时卡住或泄漏。
pub struct MediaFoundation {
    /// 引用计数：MFStartup 可以多次调用，但必须配对相同次数的 MFShutdown。
    /// 用全局计数避免"两次 startup 一次 shutdown"。
    _private: (),
}

static MF_REFCOUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl MediaFoundation {
    /// 初始化 MF。可重复调用（内部按引用计数）。
    pub fn start() -> Result<MediaFoundation> {
        use std::sync::atomic::Ordering;
        let previous = MF_REFCOUNT.fetch_add(1, Ordering::SeqCst);
        if previous == 0 {
            // NOSOCKET：不初始化网络相关部分，桌面壁纸不需要它。
            unsafe {
                windows::Win32::Media::MediaFoundation::MFStartup(
                    MF_VERSION,
                    windows::Win32::Media::MediaFoundation::MFSTARTUP_NOSOCKET,
                )
                .map_err(|e| Error::config("Media Foundation 初始化失败", e.message()))?;
            }
            log::info!("Media Foundation 已初始化（版本 0x{MF_VERSION:08X}）");
        }
        Ok(MediaFoundation { _private: () })
    }
}

impl Drop for MediaFoundation {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        if MF_REFCOUNT.fetch_sub(1, Ordering::SeqCst) == 1 {
            unsafe {
                let _ = windows::Win32::Media::MediaFoundation::MFShutdown();
            }
            log::info!("Media Foundation 已关闭");
        }
    }
}

/// 从任意媒体文件里读**音频**，输出固定的 PCM 格式。
///
/// ## 为什么固定输出格式，而不是跟着源走
///
/// 视频里的音频可能是 44.1 kHz 单声道 AAC（实测本机的测试视频就是），
/// 也可能是 48 kHz 立体声 AAC、或 AC-3。若让输出跟着源走，调用方就要为
/// 每种组合准备不同路径，而音频设备也不一定接受那些格式。
///
/// 这里统一要求 **48 kHz / 立体声 / 16 位整数 PCM**，由 Media Foundation
/// 的音频处理器（`MF_SOURCE_READER_ENABLE_AUDIO_PROCESSING`）负责转换——
/// 这与 WASAPI 侧的 `AUTOCONVERTPCM` 是同一思路：**让系统库做它擅长的事**。
///
/// 实测：44.1 kHz 单声道 AAC → 48 kHz 立体声 PCM，转换正确。
pub struct AudioReader {
    reader: IMFSourceReader,
    ended: bool,
    /// 实际输出格式（回读确认过；解码器万一给了别的格式，按实际值处理）。
    format: crate::audio::AudioFormat,
    /// 必须持有 COM 与 MF 运行时（原因见各自的说明）。
    _com: crate::com::ComApartment,
    _media_foundation: MediaFoundation,
}

/// 音频输出的固定格式。
///
/// 选 48 kHz 立体声 16 位：这是 Windows 上最常见的设备格式，
/// 改成别的也不会带来好处（WASAPI 侧还会再转一次）。
pub const AUDIO_OUTPUT_FORMAT: crate::audio::AudioFormat = crate::audio::AudioFormat {
    sample_rate: 48_000,
    channels: 2,
    format: crate::audio::SampleFormat::S16,
};

impl AudioReader {
    /// 打开文件里的音频流。**没有音频流时返回 `Ok(None)`**——
    /// 没有音轨的视频是完全正常的，不该当成错误。
    pub fn open(path: &std::path::Path) -> Result<Option<AudioReader>> {
        let media_foundation = MediaFoundation::start()?;
        let com = crate::com::ComApartment::initialize()?;

        let path_text = path
            .to_str()
            .ok_or_else(|| Error::config("媒体路径不是合法 UTF-8", path.display().to_string()))?;

        // 不设额外属性：Source Reader 在格式协商时会自行插入音频转换器
        // （重采样、声道变换）。这一点是**实测确认**的——本机测试视频是
        // 44.1 kHz 单声道 AAC，我们要求 48 kHz 立体声 PCM，转换成功。
        //
        // 注意 SDK 里有个 `MF_SOURCE_READER_ENABLE_AUDIO_PROCESSING` 属性，
        // 但 windows crate 没有把它生出来；实测不需要它也能转换。
        let attributes = unsafe {
            let mut attrs: Option<windows::Win32::Media::MediaFoundation::IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 1)
                .map_err(|e| Error::config("创建 MF 属性失败", e.message()))?;
            attrs.ok_or_else(|| Error::config("创建 MF 属性失败", "返回空"))?
        };

        let reader: IMFSourceReader = unsafe {
            MFCreateSourceReaderFromURL(&HSTRING::from(path_text), &attributes)
        }
        .map_err(|e| {
            Error::config(
                "打开媒体文件失败",
                format!("{}：{}", path.display(), e.message()),
            )
        })?;

        // 先确认有没有音频流。`GetNativeMediaType` 失败、或主类型不是音频，
        // 就说明这个文件没有音轨。
        let stream_index = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
        let has_audio = match unsafe { reader.GetNativeMediaType(stream_index, 0) } {
            Ok(native) => unsafe { native.GetGUID(&MF_MT_MAJOR_TYPE) }
                .map(|major| major == MFMediaType_Audio)
                .unwrap_or(false),
            Err(_) => false,
        };
        if !has_audio {
            log::info!("{} 没有音频流，只播画面", path.display());
            return Ok(None);
        }

        // 要求固定的 PCM 输出格式。
        let media_type: IMFMediaType = unsafe {
            let mt = MFCreateMediaType().map_err(|e| Error::config("创建媒体类型失败", e.message()))?;
            mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
                .map_err(|e| Error::config("设置音频主类型失败", e.message()))?;
            mt.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)
                .map_err(|e| Error::config("设置 PCM 子类型失败", e.message()))?;
            mt.SetUINT32(
                &MF_MT_AUDIO_NUM_CHANNELS,
                AUDIO_OUTPUT_FORMAT.channels as u32,
            )
            .map_err(|e| Error::config("设置声道数失败", e.message()))?;
            mt.SetUINT32(
                &MF_MT_AUDIO_SAMPLES_PER_SECOND,
                AUDIO_OUTPUT_FORMAT.sample_rate,
            )
            .map_err(|e| Error::config("设置采样率失败", e.message()))?;
            mt.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)
                .map_err(|e| Error::config("设置位深失败", e.message()))?;
            // 块对齐与平均字节率必须与其它字段自洽，否则某些解码器会拒绝这个格式。
            let block_align = AUDIO_OUTPUT_FORMAT.bytes_per_frame() as u32;
            mt.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, block_align)
                .map_err(|e| Error::config("设置块对齐失败", e.message()))?;
            mt.SetUINT32(
                &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                AUDIO_OUTPUT_FORMAT.sample_rate * block_align,
            )
            .map_err(|e| Error::config("设置平均字节率失败", e.message()))?;
            // 声明"每个采样独立"，让 MF 可以自由重新分块。
            let _ = mt.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1);
            mt
        };

        unsafe {
            reader
                .SetCurrentMediaType(stream_index, None, &media_type)
                .map_err(|e| {
                    Error::config(
                        "音频无法输出为 48kHz/立体声/16 位 PCM（可能缺少对应解码器）",
                        format!("{}：{}", path.display(), e.message()),
                    )
                })?;
        }

        // 回读确认解码器真的接受了（有些解码器会默默给别的格式，
        // 而那会让"按 48kHz 计算时长"这类推算全部错掉）。
        let current = unsafe { reader.GetCurrentMediaType(stream_index) }
            .map_err(|e| Error::config("读取音频输出格式失败", e.message()))?;
        let rate = unsafe { current.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND) }.unwrap_or(0);
        let channels = unsafe { current.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS) }.unwrap_or(0);
        if rate == 0 || channels == 0 {
            return Err(Error::config(
                "音频输出格式不可用",
                format!("采样率 {rate}、声道数 {channels}"),
            ));
        }
        if rate != AUDIO_OUTPUT_FORMAT.sample_rate || channels != AUDIO_OUTPUT_FORMAT.channels as u32
        {
            log::warn!(
                "音频输出格式与请求不一致：请求 {} Hz/{} 声道，得到 {rate} Hz/{channels} 声道；\
                 按实际值处理",
                AUDIO_OUTPUT_FORMAT.sample_rate,
                AUDIO_OUTPUT_FORMAT.channels
            );
        }

        log::info!(
            "音频流已打开：{}（输出 {rate} Hz/{channels} 声道/16 位）",
            path.display()
        );

        Ok(Some(AudioReader {
            reader,
            ended: false,
            format: crate::audio::AudioFormat {
                sample_rate: rate,
                channels: channels as u16,
                format: crate::audio::SampleFormat::S16,
            },
            _com: com,
            _media_foundation: media_foundation,
        }))
    }

    pub fn format(&self) -> crate::audio::AudioFormat {
        self.format
    }

    /// 读一段音频（紧凑 PCM）。到末尾返回空 `Vec`。
    pub fn read_chunk(&mut self) -> Result<Vec<u8>> {
        if self.ended {
            return Ok(Vec::new());
        }

        let mut empty_rounds = 0;
        loop {
            let mut actual_stream = 0u32;
            let mut flags = 0u32;
            let mut timestamp = 0i64;
            let mut sample: Option<windows::Win32::Media::MediaFoundation::IMFSample> = None;

            unsafe {
                self.reader
                    .ReadSample(
                        MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32,
                        0,
                        Some(&mut actual_stream),
                        Some(&mut flags),
                        Some(&mut timestamp),
                        Some(&mut sample),
                    )
                    .map_err(|e| Error::config("读取音频失败", e.message()))?;
            }

            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                self.ended = true;
                return Ok(Vec::new());
            }

            let Some(sample) = sample else {
                empty_rounds += 1;
                if empty_rounds > 16 {
                    log::warn!("连续多次读到无音频样本的通知，当作音频结束");
                    self.ended = true;
                    return Ok(Vec::new());
                }
                continue;
            };

            let buffer = unsafe { sample.ConvertToContiguousBuffer() }
                .map_err(|e| Error::config("取音频缓冲失败", e.message()))?;
            let mut data_ptr: *mut u8 = std::ptr::null_mut();
            let mut max_len = 0u32;
            let mut current_len = 0u32;
            unsafe {
                buffer
                    .Lock(&mut data_ptr, Some(&mut max_len), Some(&mut current_len))
                    .map_err(|e| Error::config("锁定音频缓冲失败", e.message()))?;
            }
            // 与视频路径同样的规矩：从这里起必须保证 Unlock 被调用，
            // 否则解码器的缓冲池会被耗尽（表现为"播一会儿就卡住"）。
            let data =
                unsafe { std::slice::from_raw_parts(data_ptr, current_len as usize).to_vec() };
            unsafe {
                let _ = buffer.Unlock();
            }
            return Ok(data);
        }
    }

    /// 回到开头。
    pub fn rewind(&mut self) -> Result<()> {
        let variant = timestamp_variant(0);
        let time_format = windows::core::GUID::zeroed();
        unsafe {
            self.reader
                .SetCurrentPosition(&time_format, &variant)
                .map_err(|e| Error::config("音频回到开头失败", e.message()))?;
        }
        self.ended = false;
        Ok(())
    }
}

/// 一个打开的视频文件（顺序读帧）。
pub struct VideoReader {
    reader: IMFSourceReader,
    info: VideoInfo,
    /// 已经读到流的末尾。
    ended: bool,
    /// **必须持有** COM 初始化句柄（见 [`ComApartment`]）。
    _com: crate::com::ComApartment,
    /// **必须持有** MF 运行时。
    ///
    /// 这一条踩过真实的坑：最初写成 `let _mf = MediaFoundation::start()?;`，
    /// 那个局部变量在 `open()` 返回时就 drop 了，于是 `MFShutdown` 在读取器还活着时
    /// 就被调用——日志顺序暴露了它（"Media Foundation 已关闭"出现在诊断输出之前）。
    /// MF 被关掉之后解码行为是未定义的：可能正常、可能崩溃、可能默默出错。
    /// 因此把运行时句柄存进结构体，让它的生命周期覆盖整个读取器。
    _media_foundation: MediaFoundation,
}

// COM 初始化已提到 crate::com，因为音频路径也需要它（同一个坑出现过两次）。
// VideoReader 持有的句柄类型是 crate::com::ComApartment。

impl VideoReader {
    /// 打开视频。`loop_playback` 为真时读到末尾会自动回到开头（循环播放）。
    pub fn open(path: &std::path::Path) -> Result<VideoReader> {
        // 这个句柄必须活到读取器结束，因此下面会把它存进结构体，
        // 而不是留成局部变量（见 VideoReader::_media_foundation 的说明）。
        let media_foundation = MediaFoundation::start()?;

        // COM 必须在本线程初始化，而且句柄要跟着读取器一起活到结束
        // （见 ComApartment 的说明）。这一条是被真实失败逼出来的：
        // 两块屏各有一个解码线程，第一个能打开、第二个报"尚未调用 CoInitialize"——
        // 因为只有主线程做过初始化。它看起来像"只有一块屏能播"的显示器问题，
        // 实际原因与显示器无关。
        let com = crate::com::ComApartment::initialize()?;

        let path_text = path
            .to_str()
            .ok_or_else(|| Error::config("视频路径不是合法 UTF-8", path.display().to_string()))?;

        // 属性：打开"基本视频处理"，这样解码器会把画面转成我们要的 RGB32，
        // 我们就不必自己写 YUV→RGB。关掉"高级视频处理"，因为它会尝试用 D3D 输出，
        // 而这一版要的是内存帧。
        let attributes = unsafe {
            let mut attrs: Option<windows::Win32::Media::MediaFoundation::IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 2)
                .map_err(|e| Error::config("创建 MF 属性失败", e.message()))?;
            let attrs = attrs.ok_or_else(|| Error::config("创建 MF 属性失败", "返回空"))?;
            let _ = attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1);
            let _ = attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 0);
            attrs
        };

        let reader: IMFSourceReader =
            unsafe { MFCreateSourceReaderFromURL(&HSTRING::from(path_text), &attributes) }.map_err(
                |e| {
                    Error::config(
                        "打开视频失败",
                        format!("{}：{}", path.display(), e.message()),
                    )
                },
            )?;

        // 要求 RGB32 输出。
        //
        // 为什么不是 NV12：NV12 是 GPU 原生的，但要在解码后做色彩转换才成图像；
        // 这一版的目标是"把帧送到合成表面上"，先用 RGB32 把链路跑通，
        // 等性能不够时再考虑 NV12 + GPU 转换。
        let media_type: IMFMediaType = unsafe {
            let mt = MFCreateMediaType().map_err(|e| Error::config("创建媒体类型失败", e.message()))?;
            mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| Error::config("设置视频主类型失败", e.message()))?;
            mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
                .map_err(|e| Error::config("设置 RGB32 子类型失败", e.message()))?;
            mt
        };
        unsafe {
            reader
                .SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, None, &media_type)
                .map_err(|e| {
                    Error::config(
                        "视频无法输出为 RGB32（可能缺少对应解码器）",
                        format!("{}：{}", path.display(), e.message()),
                    )
                })?;
        }

        let current = unsafe {
            reader
                .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
                .map_err(|e| Error::config("读取视频格式失败", e.message()))?
        };
        let size = unsafe { current.GetUINT64(&MF_MT_FRAME_SIZE) }
            .map_err(|e| Error::config("读取帧尺寸失败", e.message()))?;
        let width = (size >> 32) as u32;
        let height = (size & 0xFFFF_FFFF) as u32;

        let stride_raw = unsafe { current.GetUINT32(&MF_MT_DEFAULT_STRIDE) }
            .map(|s| s as i32)
            // 读不到就假设紧凑排列。这是本机实测的实际情况。
            .unwrap_or(width as i32 * 4);
        let bottom_up = stride_raw < 0;
        let stride = stride_raw.unsigned_abs() as usize;

        let frame_rate = unsafe { current.GetUINT64(&MF_MT_FRAME_RATE) }
            .map(|v| ((v >> 32) as u32, (v & 0xFFFF_FFFF) as u32))
            .unwrap_or((0, 1));

        let info = VideoInfo {
            width,
            height,
            frame_rate,
            stride,
            bottom_up,
        };

        log::info!(
            "已打开视频 {}：{}x{}，{:.2} fps，跨距 {} 字节/行{}",
            path.display(),
            info.width,
            info.height,
            info.fps(),
            info.stride,
            if info.stride == (info.width * 4) as usize {
                ""
            } else {
                "（有填充，读取时会逐行去掉）"
            }
        );

        Ok(VideoReader {
            reader,
            info,
            ended: false,
            _com: com,
            _media_foundation: media_foundation,
        })
    }

    pub fn info(&self) -> &VideoInfo {
        &self.info
    }

    /// 回到视频开头。循环播放时用。
    pub fn rewind(&mut self) -> Result<()> {
        self.seek_to(0)
    }

    /// 跳到指定时刻（100 纳秒为单位）。
    ///
    /// `IMFSourceReader::SetCurrentPosition` 要的是 `(时间格式 GUID, PROPVARIANT)` 两个参数，
    /// 而不是一个裸的 64 位位置——它和 `IMFPresentationClock` 上同名的那个方法是两回事。
    /// 时间格式传零 GUID 表示"按 100 纳秒为单位的位置"（MF 的默认时间格式）。
    ///
    /// 这个方法的声明还被 `Win32_System_Com_StructuredStorage` 与
    /// `Win32_System_Variant` 两个 feature 门控着——不开 feature 时它**不是签名不匹配，
    /// 而是方法压根不存在**（报 "no method named SetCurrentPosition"），
    /// 很容易让人以为这个 API 没有。
    pub fn seek_to(&mut self, position_100ns: i64) -> Result<()> {
        let variant = timestamp_variant(position_100ns);
        let time_format = windows::core::GUID::zeroed();

        unsafe {
            self.reader
                .SetCurrentPosition(&time_format, &variant)
                .map_err(|e| Error::config("视频跳转失败", e.message()))?;
        }
        self.ended = false;
        log::debug!("视频已跳转到 {:.3} s", position_100ns as f64 / 10_000_000.0);
        Ok(())
    }

    /// 读下一帧。到末尾时返回 `Ok(None)`；再次调用会继续返回 `None`
    /// （要重播请显式调用 [`VideoReader::rewind`]，这样调用方对循环有完全控制权）。
    pub fn next_frame(&mut self) -> Result<Option<VideoFrame>> {
        if self.ended {
            return Ok(None);
        }

        // ReadSample 有时会给"有标志但无样本"的通知（例如格式变更），
        // 因此这里循环直到拿到样本、到末尾、或连续空转次数过多。
        let mut empty_rounds = 0;
        loop {
            let mut actual_stream = 0u32;
            let mut flags = 0u32;
            let mut timestamp = 0i64;
            let mut sample: Option<windows::Win32::Media::MediaFoundation::IMFSample> = None;

            unsafe {
                self.reader
                    .ReadSample(
                        MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                        0,
                        Some(&mut actual_stream),
                        Some(&mut flags),
                        Some(&mut timestamp),
                        Some(&mut sample),
                    )
                    .map_err(|e| Error::config("读取视频帧失败", e.message()))?;
            }

            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                self.ended = true;
                return Ok(None);
            }

            let Some(sample) = sample else {
                empty_rounds += 1;
                if empty_rounds > 16 {
                    // 连续这么多次拿不到样本，说明有我们没处理的流事件，
                    // 比如"只换格式不给数据"。当作到末尾，避免死循环。
                    log::warn!("连续多次读到无样本的通知，当作视频结束");
                    self.ended = true;
                    return Ok(None);
                }
                continue;
            };

            let duration = unsafe { sample.GetSampleDuration() }.unwrap_or(0);
            let buffer = unsafe { sample.ConvertToContiguousBuffer() }
                .map_err(|e| Error::config("取样缓冲失败", e.message()))?;

            let mut data_ptr: *mut u8 = std::ptr::null_mut();
            let mut max_len = 0u32;
            let mut current_len = 0u32;
            unsafe {
                buffer
                    .Lock(&mut data_ptr, Some(&mut max_len), Some(&mut current_len))
                    .map_err(|e| Error::config("锁定视频缓冲失败", e.message()))?;
            }
            // 从这个点开始必须保证 Unlock 一定被调用（否则解码器的缓冲池会被耗尽，
            // 表现为"解了几帧之后卡住"）。因此把拷贝放进一个闭包，统一在下面解锁。
            let pixels = unsafe {
                let source = std::slice::from_raw_parts(data_ptr, current_len as usize);
                copy_tight_frame(
                    source,
                    self.info.width,
                    self.info.height,
                    self.info.stride,
                    self.info.bottom_up,
                )
            };
            unsafe {
                let _ = buffer.Unlock();
            }

            let Some(pixels) = pixels else {
                // 缓冲比一帧还小：这是异常情况，报出来而不是给一张半截图。
                return Err(Error::config(
                    "视频缓冲小于一帧所需",
                    format!(
                        "需要 {} 字节，实际有效 {} 字节",
                        self.info.tight_frame_bytes(),
                        current_len
                    ),
                ));
            };

            return Ok(Some(VideoFrame {
                pixels,
                width: self.info.width,
                height: self.info.height,
                timestamp_100ns: timestamp,
                duration_100ns: duration,
            }));
        }
    }

    /// 把整个视频解码成帧序列（用于诊断与小视频）。
    ///
    /// 大视频会吃很多内存（1920x1080 一帧约 8 MB），因此设了上限。
    pub fn decode_all(&mut self, limit: usize) -> Result<Vec<VideoFrame>> {
        let mut frames = Vec::new();
        while frames.len() < limit {
            match self.next_frame()? {
                Some(frame) => frames.push(frame),
                None => break,
            }
        }
        Ok(frames)
    }
}

/// 把一块带可能填充的缓冲拷成紧凑的一帧。
///
/// 返回 `None` 表示缓冲比一帧所需还小（调用方应当把它当错误报出去，
/// 而不是显示一张半截图）。
fn copy_tight_frame(
    source: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    bottom_up: bool,
) -> Option<Vec<u8>> {
    let tight_stride = width as usize * 4;
    let required = tight_stride * height as usize;
    if source.len() < required {
        return None;
    }

    // 跨距就是紧凑的时候直接整块拷，省掉逐行的分支。
    if stride == tight_stride && !bottom_up {
        return Some(source[..required].to_vec());
    }

    let mut pixels = vec![0u8; required];
    for row in 0..height as usize {
        // bottom-up 时第 0 行在缓冲末尾。
        let source_row = if bottom_up { height as usize - 1 - row } else { row };
        let from = source_row * stride;
        let to = row * tight_stride;
        if from + tight_stride > source.len() {
            // 行数据不全：保留已拷出的部分（其余为 0），并让调用方从日志看出异常。
            log::warn!("视频第 {row} 行超出缓冲范围，该行以黑填充");
            break;
        }
        pixels[to..to + tight_stride].copy_from_slice(&source[from..from + tight_stride]);
    }
    Some(pixels)
}

/// 估算一段视频能否在给定时间内解码完（用于界面提示，不参与控制流）。
pub fn is_probably_smooth(fps: f64, millis_per_frame: Duration) -> bool {
    if fps <= 0.0 {
        return true;
    }
    let budget_ms = 1000.0 / fps;
    millis_per_frame.as_secs_f64() * 1000.0 < budget_ms
}

/// 测量解码速度（诊断用）。
pub fn measure_decode_speed(path: &std::path::Path, frames: usize) -> Result<(f64, VideoInfo)> {
    let mut reader = VideoReader::open(path)?;
    let info = reader.info().clone();
    let started = Instant::now();
    let mut count = 0;
    while count < frames {
        match reader.next_frame()? {
            Some(_) => count += 1,
            None => {
                reader.rewind()?;
                if count == 0 {
                    break;
                }
            }
        }
    }
    let elapsed = started.elapsed();
    let per_frame = if count > 0 {
        elapsed / count as u32
    } else {
        Duration::ZERO
    };
    Ok((per_frame.as_secs_f64() * 1000.0, info))
}

/// 把"100 纳秒为单位的位置"包成一个 `VT_I8` 的 `PROPVARIANT`。
///
/// ## 为什么必须显式设置 `vt`
///
/// 我最初写成"零值初始化的 `PROPVARIANT` 就等于 `VT_I8(0)`，所以不必碰 union"。
/// **这是错的**，而且是那种不报错的错：零值初始化的 `vt` 是 `VT_EMPTY(0)`，
/// 不是 `VT_I8(20)`。MF 收到 `VT_EMPTY` 的变体不知道要跳到哪里，
/// 于是**什么都不做也不返回错误**，继续从当前位置读——表现是
/// "rewind() 说成功了，但下一帧的时间戳是 1.067 s"。
///
/// `PROPVARIANT` 的手写 union 属于危险区域，因此集中在这一个函数里做，
/// 并配一条单元测试守住"vt 必须被设成 VT_I8"这个不变量。
fn timestamp_variant(
    position_100ns: i64,
) -> windows::Win32::System::Com::StructuredStorage::PROPVARIANT {
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::System::Variant::VT_I8;

    // 先整体清零：union 里其余字节必须是确定的，否则会带上栈上的残留数据。
    let mut variant = PROPVARIANT::default();

    // 布局（windows crate 生成）：PROPVARIANT { Anonymous: PROPVARIANT_0 (union) }
    //   -> PROPVARIANT_0.Anonymous: ManuallyDrop<PROPVARIANT_0_0>
    //      -> PROPVARIANT_0_0 { vt: VARENUM, wReserved1..3: u16, Anonymous: PROPVARIANT_0_0_0 (union) }
    //         -> PROPVARIANT_0_0_0 里的 64 位成员叫 `hVal`（**不叫** `llVal`）
    //
    // 写入 ManuallyDrop 字段要显式解引用 `*`，否则编译器会警告"不会自动 DerefMut"。
    unsafe {
        (*variant.Anonymous.Anonymous).vt = VT_I8;
        (*variant.Anonymous.Anonymous).Anonymous.hVal = position_100ns;
    }

    // 自检：设完之后 vt 必须真的是 VT_I8。写错了这里会立刻炸，
    // 而不是等到"seek 静默失效"再去查（那正是我踩过的坑）。
    debug_assert_eq!(
        unsafe { (*variant.Anonymous.Anonymous).vt },
        VT_I8,
        "PROPVARIANT 的 vt 没有设成 VT_I8，seek 会静默失效"
    );

    variant
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_variant_really_sets_vt_to_i8() {
        // 这条测试守的是一个**静默失效**的 bug。
        //
        // 最初我写成"零值初始化的 PROPVARIANT 等于 VT_I8(0)，不必碰 union"。
        // 实际上零值的 vt 是 VT_EMPTY(0)，MF 收到它不知道要跳到哪里，
        // 于是什么都不做也不报错。表现是 rewind() 报告成功，
        // 但下一帧时间戳是 1.067 s 而不是 0.067 s。
        //
        // 这类 bug 不会崩、不会报错，只会"行为不对"，因此必须有测试盯着。
        use windows::Win32::System::Variant::VT_I8;

        let variant = timestamp_variant(0);
        let vt = unsafe { (*variant.Anonymous.Anonymous).vt };
        assert_eq!(
            vt, VT_I8,
            "vt 必须是 VT_I8(20)，零值默认的 VT_EMPTY(0) 会让 seek 静默失效"
        );

        // 数值也要真的写进去：非零位置要能读回来。
        let variant = timestamp_variant(5_000_000); // 0.5 秒
        let value = unsafe { (*variant.Anonymous.Anonymous).Anonymous.hVal };
        assert_eq!(value, 5_000_000, "位置数值没有写进 union 的 hVal");
    }

    #[test]
    fn timestamp_variant_zero_is_a_valid_position_zero() {
        // 位置 0 必须能用（循环播放靠它回到开头）。
        use windows::Win32::System::Variant::VT_I8;
        let variant = timestamp_variant(0);
        assert_eq!(unsafe { (*variant.Anonymous.Anonymous).vt }, VT_I8);
        assert_eq!(unsafe { (*variant.Anonymous.Anonymous).Anonymous.hVal }, 0);
    }

    #[test]
    fn tight_copy_without_padding_is_a_plain_slice() {
        // 无填充、自上而下：应当就是原样拷贝。
        let width = 2u32;
        let height = 2u32;
        let source: Vec<u8> = (0..(width * height * 4) as u8).collect();
        let out = copy_tight_frame(&source, width, height, (width * 4) as usize, false).unwrap();
        assert_eq!(out, source);
    }

    #[test]
    fn tight_copy_removes_row_padding() {
        // 有填充时，填充字节不能被拷进结果——否则画面会斜切错位。
        let width = 2u32;
        let height = 3u32;
        let tight = (width * 4) as usize; // 8
        let stride = tight + 4; // 12，每行多 4 字节填充
        let mut source = vec![0xEEu8; stride * height as usize];
        for row in 0..height as usize {
            for byte in 0..tight {
                source[row * stride + byte] = (row as u8) * 10 + byte as u8;
            }
        }
        let out = copy_tight_frame(&source, width, height, stride, false).unwrap();
        assert_eq!(out.len(), tight * height as usize);
        // 结果里不应出现填充用的 0xEE
        assert!(
            !out.contains(&0xEE),
            "填充字节被拷进了结果：{out:?}"
        );
        // 行序应当保持
        assert_eq!(out[0], 0);
        assert_eq!(out[tight], 10);
        assert_eq!(out[tight * 2], 20);
    }

    #[test]
    fn bottom_up_frames_are_flipped() {
        // 自下而上存储时，结果的第一行应当来自缓冲的最后一行。
        let width = 1u32;
        let height = 3u32;
        let stride = 4usize;
        let mut source = vec![0u8; stride * height as usize];
        // 缓冲里第 0 行是"最下面"那行，值 0xAA；最后一行是"最上面"，值 0xBB
        source[0] = 0xAA;
        source[stride * 2] = 0xBB;
        let out = copy_tight_frame(&source, width, height, stride, true).unwrap();
        assert_eq!(out[0], 0xBB, "结果第一行应当来自缓冲最后一行");
        assert_eq!(out[stride * 2], 0xAA, "结果最后一行应当来自缓冲第一行");
    }

    #[test]
    fn buffer_smaller_than_one_frame_is_reported_not_truncated() {
        // 缓冲不足时必须返回 None，让调用方报错；
        // 悄悄给一张半截图会让用户看到莫名其妙的画面，而不知道视频有问题。
        let source = vec![0u8; 10];
        assert!(copy_tight_frame(&source, 100, 100, 400, false).is_none());
    }

    #[test]
    fn frame_count_check_uses_the_fps_budget() {
        // 30 fps 的预算是 33.3 ms/帧
        assert!(is_probably_smooth(30.0, Duration::from_millis(10)));
        assert!(!is_probably_smooth(30.0, Duration::from_millis(40)));
        // 帧率未知时不应误报"不流畅"
        assert!(is_probably_smooth(0.0, Duration::from_millis(40)));
    }

    #[test]
    fn video_info_reports_fps_and_frame_bytes() {
        let info = VideoInfo {
            width: 1920,
            height: 1080,
            frame_rate: (30, 1),
            stride: 7680,
            bottom_up: false,
        };
        assert_eq!(info.fps(), 30.0);
        assert_eq!(info.tight_frame_bytes(), 1920 * 4 * 1080);

        let fractional = VideoInfo {
            frame_rate: (30000, 1001),
            ..info
        };
        assert!((fractional.fps() - 29.97).abs() < 0.01);
    }
}
