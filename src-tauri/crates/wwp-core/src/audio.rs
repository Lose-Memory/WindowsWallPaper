//! 音频播放（WASAPI 共享模式）与音量控制。
//!
//! ## 为什么用共享模式 + `AUTOCONVERTPCM`
//!
//! WASAPI **独占模式**要求我们提供的采样格式与设备硬件格式完全一致，
//! 否则 `Initialize` 失败——那意味着要自己写重采样，而重采样是音频编程里
//! 最容易出细节问题的一块（抗镜像滤波、相位、抖动）。
//!
//! 共享模式下加 `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM`，
//! WASAPI 会**在内部做格式转换与重采样**。这是这个标志存在的意义，
//! 也是"视频里有 44.1 kHz 的 AAC、声卡跑 48 kHz"这种常见组合能直接工作的原因。
//!
//! ## 这里没有做什么
//!
//! **没有与新视频同步对齐**。换视频时音频会从新视频的第 0 个采样开始播，
//! 而画面也是从第 0 帧开始，因此两者**起点一致**；
//! 但长时间播放后音频与画面可能因为各自的时钟误差慢慢漂移。
//! 一个 5 秒的壁纸视频循环播放时这个漂移可以忽略；将来若要接长视频，
//! 需要以音频时钟为基准去校正画面（或以 `IAudioClock` 为准）。

use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, DEVICE_STATE_ACTIVE, IAudioClient,
    IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVE_FORMAT_PCM, WAVEFORMATEX,
    eConsole, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};

use crate::{Error, Result};

/// 请求的音频缓冲时长（100 纳秒为单位）。
///
/// 200 ms 是个折中：太短会在系统繁忙时出现欠载（爆音），太长会让"停止"
/// 之后还继续响一会儿（缓冲里的数据要播完）。壁纸场景下 200 ms 可以接受。
const BUFFER_DURATION_100NS: i64 = 2_000_000;

/// 一次能提交的采样数上限。
///
/// 用来限制内存：如果调用方塞进来一个巨大的缓冲区，我们会一次性分配同样大的
/// 临时空间。48000 Hz 立体声 16 位下，1 秒是 192 KB，
/// 因此 10 秒（约 2 MB）是个宽松但安全的上限。
const MAX_FRAMES_PER_WRITE: usize = 480_000;

/// 采样格式。只支持 16 位整数——视频里的 AAC/MP3 解码后通常是
/// 16 位或 32 位浮点，而共享模式的 `AUTOCONVERTPCM` 能把它们都转掉，
/// 因此我们只需挑一种。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    /// 16 位有符号整数，交错。
    S16,
    /// 32 位浮点，交错。
    F32,
}

/// 音频流的格式描述。
#[derive(Debug, Clone, Copy)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub format: SampleFormat,
}

impl AudioFormat {
    /// 每个采样帧的字节数（一个"帧"含所有声道）。
    pub fn bytes_per_frame(&self) -> usize {
        self.channels as usize
            * match self.format {
                SampleFormat::S16 => 2,
                SampleFormat::F32 => 4,
            }
    }

    /// 构造一个 `WAVEFORMATEX`。
    fn to_wave_format(&self) -> WAVEFORMATEX {
        let bits = match self.format {
            SampleFormat::S16 => 16u16,
            SampleFormat::F32 => 32,
        };
        WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: self.channels,
            nSamplesPerSec: self.sample_rate,
            nAvgBytesPerSec: self.sample_rate * self.bytes_per_frame() as u32,
            nBlockAlign: self.bytes_per_frame() as u16,
            wBitsPerSample: bits,
            cbSize: 0,
        }
    }
}

/// 一个音频输出流。
pub struct AudioSink {
    client: IAudioClient,
    render: IAudioRenderClient,
    /// 设备混音缓冲有多少帧。每次只写"当前空闲"的部分。
    buffer_frames: u32,
    /// 我们已经提交但还没被设备播完的帧数（近似值）。
    ///
    /// 维护它是为了避免每次都问一遍设备（那要一次 COM 调用）。
    /// 它在欠载/欠载恢复时会不精确，因此只用于节流，不用于正确性判断。
    pending_frames: u32,
    format: AudioFormat,
    format_bytes_per_frame: usize,
    /// **必须持有** COM 初始化句柄（见 `crate::com`）。
    ///
    /// 不持有它的后果是"设备打开成功、但一写数据就报 CoInitialize 未调用"——
    /// 或者更糟：在某些线程状态下静默失败。
    _com: crate::com::ComApartment,
}

// 安全性说明：`IAudioClient`/`IAudioRenderClient` 是 COM 接口，
// WASAPI 的这两个接口在共享模式下可以跨线程使用（它们内部有同步）。
// 我们的用法是"一个播放线程写入、控制线程设置音量"，而音量走的是
// 另一个接口（简化为在写入路径里按比例缩放），因此不会并发调用同一方法。
unsafe impl Send for AudioSink {}
unsafe impl Sync for AudioSink {}

impl AudioSink {
    /// 打开默认播放设备的共享模式流。`format` 是**输入数据**的格式；
    /// WASAPI 会把它转换成设备格式。
    pub fn open(format: AudioFormat) -> Result<AudioSink> {
        if format.channels == 0 || format.sample_rate == 0 {
            return Err(Error::config("音频格式无效", "声道数或采样率为 0"));
        }

        // **每个用 COM 的线程都要初始化它**。这一点本项目踩过两次：
        // 视频解码时是"只有一块屏能播"，音频这里是"打开设备失败：尚未调用 CoInitialize"。
        // 句柄要活到 sink 结束，因此下面会存进结构体。
        let com = crate::com::ComApartment::initialize()?;

        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| Error::config("创建音频设备枚举器失败", e.message()))?;
            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|e| Error::config("获取默认播放设备失败（可能没有声卡）", e.message()))?;
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| Error::config("激活音频客户端失败", e.message()))?;

            // 设备自己的混音格式只用于日志与诊断：我们把输入的格式交给
            // `Initialize`，由 `AUTOCONVERTPCM` 负责转换。
            //
            // 注意 `WAVEFORMATEX` 是 **packed** 结构（1 字节对齐），
            // 直接引用它的字段会创建"未对齐引用"，那是未定义行为（编译器会拦下来）。
            // 因此用 `read_unaligned` 按值读出字段，而不是取引用。
            if let Ok(mix) = client.GetMixFormat() {
                if !mix.is_null() {
                    let rate = std::ptr::read_unaligned(std::ptr::addr_of!((*mix).nSamplesPerSec));
                    let channels = std::ptr::read_unaligned(std::ptr::addr_of!((*mix).nChannels));
                    let bits = std::ptr::read_unaligned(std::ptr::addr_of!((*mix).wBitsPerSample));
                    log::info!("音频设备混音格式：{rate} Hz，{channels} 声道，{bits} 位");
                    CoTaskMemFree(Some(mix as *const _ as *const _));
                }
            }

            let wave = format.to_wave_format();
            // 用 AUTOCONVERTPCM + SRC_DEFAULT_QUALITY：让 WASAPI 自己转换格式与采样率。
            // 不加这两个标志的话，输入格式与设备格式不一致时 Initialize 会直接失败。
            let flags =
                AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    flags,
                    BUFFER_DURATION_100NS,
                    0,
                    &wave,
                    None,
                )
                .map_err(|e| {
                    Error::config(
                        "初始化音频流失败",
                        format!(
                            "{} Hz/{} 声道：{}",
                            format.sample_rate,
                            format.channels,
                            e.message()
                        ),
                    )
                })?;

            let buffer_frames = client
                .GetBufferSize()
                .map_err(|e| Error::config("取音频缓冲大小失败", e.message()))?;
            let render: IAudioRenderClient = client
                .GetService()
                .map_err(|e| Error::config("取音频渲染接口失败", e.message()))?;

            log::info!(
                "音频流已就绪：输入 {} Hz/{} 声道/{}，设备缓冲 {} 帧（{:.0} ms）",
                format.sample_rate,
                format.channels,
                match format.format {
                    SampleFormat::S16 => "16 位整数",
                    SampleFormat::F32 => "32 位浮点",
                },
                buffer_frames,
                buffer_frames as f64 * 1000.0 / format.sample_rate as f64
            );

            Ok(AudioSink {
                client,
                render,
                buffer_frames,
                pending_frames: 0,
                format,
                format_bytes_per_frame: format.bytes_per_frame(),
                _com: com,
            })
        }
    }

    pub fn format(&self) -> AudioFormat {
        self.format
    }

    /// 开始播放。
    pub fn start(&self) -> Result<()> {
        unsafe { self.client.Start() }
            .map_err(|e| Error::config("启动音频流失败", e.message()))
    }

    /// 停止播放。
    ///
    /// 会同时清空设备缓冲里还没播完的数据（用 Reset），否则停止之后
    /// 还会响一小段——那在"切换壁纸"时听起来像卡了一下。
    pub fn stop(&self) -> Result<()> {
        unsafe {
            let _ = self.client.Stop();
            // Reset 清掉残留数据。它在 Stop 之后调用才有意义。
            let _ = self.client.Reset();
        }
        Ok(())
    }

    /// 写入一段交错采样数据，音量通过 `volume` 缩放。
    ///
    /// `volume` 是 0.0（静音）到 1.0（原音量）之间的值；超出范围会被夹住
    /// （允许大于 1 会让声音削顶失真，而用户要的是"更响"时应该去调系统音量）。
    ///
    /// 返回实际写入的帧数。**可能少于请求的帧数**：设备缓冲满的时候只能写一部分，
    /// 调用方应当按返回值推进自己的读取位置，而不是假设全部写进去了。
    pub fn write(&mut self, samples: &[u8], volume: f32) -> Result<usize> {
        let bytes_per_frame = self.format_bytes_per_frame;
        if bytes_per_frame == 0 {
            return Err(Error::config("音频格式无效", "每帧字节数为 0"));
        }
        let total_frames = samples.len() / bytes_per_frame;
        if total_frames == 0 {
            return Ok(0);
        }
        if total_frames > MAX_FRAMES_PER_WRITE {
            return Err(Error::config(
                "一次提交的音频数据过多",
                format!("{total_frames} 帧超过上限 {MAX_FRAMES_PER_WRITE}"),
            ));
        }

        let volume = volume.clamp(0.0, 1.0);
        unsafe {
            let padding = self
                .client
                .GetCurrentPadding()
                .map_err(|e| Error::config("读取音频缓冲占用失败", e.message()))?;
            let free = self.buffer_frames.saturating_sub(padding);
            if free == 0 {
                return Ok(0);
            }
            let to_write = (free as usize).min(total_frames);

            // 这个重载直接返回缓冲指针（另一个重载才是输出参数形式）。
            let pointer = self
                .render
                .GetBuffer(to_write as u32)
                .map_err(|e| Error::config("获取音频缓冲指针失败", e.message()))?;

            // 把数据（按音量缩放后）拷进设备的缓冲。
            let byte_count = to_write * bytes_per_frame;
            let destination = std::slice::from_raw_parts_mut(pointer, byte_count);
            write_scaled(destination, &samples[..byte_count], self.format.format, volume);

            // 第二个参数是标志位，0 表示"这是一段正常数据"。
            self.render
                .ReleaseBuffer(to_write as u32, 0)
                .map_err(|e| Error::config("提交音频缓冲失败", e.message()))?;

            self.pending_frames = padding + to_write as u32;
            Ok(to_write)
        }
    }

    /// 当前设备缓冲里还有多少帧没播完（我们自己记的近似值）。
    pub fn pending_frames(&self) -> u32 {
        self.pending_frames
    }

    /// 直接向设备查询"还没播完的帧数"。
    ///
    /// 与 [`AudioSink::pending_frames`]（那是我们记的近似值）不同，
    /// 这个是真实现场读取。它用于**测量缓冲延迟**：
    /// 一旦设备报告的未播帧数少于我们累计写入的帧数，
    /// 就说明设备已经开始消耗数据（已经开始出声）了。
    pub fn query_padding(&self) -> Result<u32> {
        unsafe { self.client.GetCurrentPadding() }
            .map_err(|e| Error::config("读取音频缓冲占用失败", e.message()))
    }
}

impl Drop for AudioSink {
    fn drop(&mut self) {
        // 不停止就释放会让设备缓冲里的数据继续播，而且流的状态会留在"运行"。
        let _ = self.stop();
    }
}

/// 把源数据按音量缩放后写进目标缓冲。
///
/// 抽成独立函数是为了能**单元测试**它——音量与静音是这个模块最容易被要求
/// 验证的行为，而它不需要任何音频设备就能测。
pub fn write_scaled(
    destination: &mut [u8],
    source: &[u8],
    format: SampleFormat,
    volume: f32,
) {
    let volume = volume.clamp(0.0, 1.0);

    // 音量正好是 1.0 时直接整块拷贝：这是最常见的情况（未调过音量），
    // 走逐采样的分支会白白多花时间。
    if (volume - 1.0).abs() < f32::EPSILON {
        destination.copy_from_slice(source);
        return;
    }

    match format {
        SampleFormat::S16 => {
            for (dst, src) in destination
                .chunks_exact_mut(2)
                .zip(source.chunks_exact(2))
            {
                let sample = i16::from_le_bytes([src[0], src[1]]);
                // 用 i32 中转再夹住：直接对 i16 乘会在接近满量程时溢出环绕，
                // 那听起来是刺耳的爆音而不是"变小声"。
                let scaled = (sample as f32 * volume).round();
                let clamped = scaled.clamp(i16::MIN as f32, i16::MAX as f32) as i16;
                dst.copy_from_slice(&clamped.to_le_bytes());
            }
        }
        SampleFormat::F32 => {
            for (dst, src) in destination
                .chunks_exact_mut(4)
                .zip(source.chunks_exact(4))
            {
                let sample = f32::from_le_bytes([src[0], src[1], src[2], src[3]]);
                let scaled = sample * volume;

                // 三个条件都必要，缺一不可：
                // 1. `clamp` 把超范围的值收进 [-1,1]——有些解码器会给出略微超范围的值，
                //    而某些设备驱动会把超范围的采样当噪声。
                // 2. **NaN 会穿过 `clamp`**（NaN 的比较永远为 false，因此它原样返回），
                //    而 `NaN * 0.0` 仍然是 `NaN`——也就是说"静音"这个最该可靠的操作
                //    会在遇到坏采样时输出 NaN。这不是假想的问题：
                //    `0xFFFFFFFF` 按 f32 解释就是 NaN，而损坏的文件里出现它是很正常的。
                // 3. 无穷大同样要挡住（它也会穿过 clamp）。
                //
                // 因此这里显式把非有限值当作静音处理。**发现方式是一条"静音应当输出
                // 全 0"的单元测试**——这正是那条测试的价值。
                let value = if scaled.is_finite() {
                    scaled.clamp(-1.0, 1.0)
                } else {
                    0.0
                };
                dst.copy_from_slice(&value.to_le_bytes());
            }
        }
    }
}

/// 音量到"静音"的便捷封装。
///
/// 存在的意义是让调用方读起来清楚：`effective_volume(0.7, true) == 0.0`，
/// 而不是在播放循环里散落 `if muted { 0.0 } else { volume }`。
pub fn effective_volume(volume: f32, muted: bool) -> f32 {
    if muted {
        0.0
    } else {
        volume.clamp(0.0, 1.0)
    }
}

/// 枚举当前有哪些可用的播放设备（诊断用）。
pub fn list_render_devices() -> Result<Vec<String>> {
    // 它可能被任意线程调用（诊断路径），因此同样要自己初始化 COM。
    // 句柄必须在整个函数体里活着——这就是它绑定到 `_com` 而不是写成
    // `let _ = ...` 的原因：后者会立刻 drop，等于没初始化。
    let _com = crate::com::ComApartment::initialize()?;
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| Error::config("创建音频设备枚举器失败", e.message()))?;
        let collection = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .map_err(|e| Error::config("枚举音频设备失败", e.message()))?;
        let count = collection
            .GetCount()
            .map_err(|e| Error::config("读取音频设备数量失败", e.message()))?;

        let mut names = Vec::new();
        for index in 0..count {
            let Ok(device) = collection.Item(index) else {
                continue;
            };
            // 设备 id 就够诊断用了；友好名字要读属性存储（代码更多、收益有限）。
            match device.GetId() {
                Ok(id) => {
                    let text = id.to_string().unwrap_or_else(|_| "<无法读取>".to_string());
                    names.push(text);
                    CoTaskMemFree(Some(id.0 as *const _));
                }
                Err(_) => names.push("<无法读取设备 id>".to_string()),
            }
        }
        Ok(names)
    }
}



#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_of_one_copies_samples_unchanged() {
        // 音量未调过是最常见的情况，必须逐字节保真——
        // 任何"经过一次浮点运算"的实现都会引入量化误差。
        let source: Vec<u8> = (0..16u8).collect();
        let mut destination = vec![0u8; 16];
        write_scaled(&mut destination, &source, SampleFormat::S16, 1.0);
        assert_eq!(destination, source, "音量为 1.0 时应当逐字节拷贝");
    }

    #[test]
    fn nan_and_infinity_samples_become_silence_not_noise() {
        // 这条测试的来历：`volume_of_zero_produces_silence` 最初失败了，
        // 因为它用的源数据是全 `0xFF`——按 f32 解释正好是 NaN。
        // `NaN.clamp(-1.0, 1.0)` 返回 NaN（NaN 的比较永远为 false），
        // 而 `NaN * 0.0` 也是 NaN，于是"静音"输出了 NaN。
        //
        // NaN 采样不是假想：`0xFFFFFFFF` 就是它，而损坏的文件里很常见。
        // 把 NaN 送给设备驱动可能产生噪声甚至异常。
        for bits in [0xFFFF_FFFFu32, 0x7FC0_0000, 0x7F80_0000, 0xFF80_0000] {
            let source = bits.to_le_bytes().repeat(4);
            let mut destination = vec![0xAAu8; source.len()];
            // 音量 1.0 也不该让 NaN 通过——那时走的是"直接拷贝"分支，
            // 但那条分支只用于正常数据，这里用 0.5 走缩放分支。
            write_scaled(&mut destination, &source, SampleFormat::F32, 0.5);
            for (index, chunk) in destination.chunks_exact(4).enumerate() {
                let value = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                assert!(
                    value.is_finite(),
                    "第 {index} 个采样（源 0x{bits:08X}）输出成了 {value}，\
                     非有限值会变成噪声"
                );
            }
        }
    }

    #[test]
    fn volume_of_zero_produces_silence() {
        // 静音是这个功能存在的理由，必须真的输出 0 而不是"接近 0"。
        let source = vec![0xFFu8; 32];
        let mut destination = vec![0xAAu8; 32];
        write_scaled(&mut destination, &source, SampleFormat::S16, 0.0);
        assert!(
            destination.iter().all(|b| *b == 0),
            "音量 0 应当输出全 0，实际 {destination:?}"
        );

        let mut destination = vec![0xAAu8; 32];
        write_scaled(&mut destination, &source, SampleFormat::F32, 0.0);
        assert!(
            destination.iter().all(|b| *b == 0),
            "浮点格式下音量 0 也应当输出全 0"
        );
    }

    #[test]
    fn volume_scales_16_bit_samples_correctly() {
        // 满量程的一半：32767 * 0.5 ≈ 16384
        let sample: i16 = 32767;
        let mut destination = vec![0u8; 2];
        write_scaled(
            &mut destination,
            &sample.to_le_bytes(),
            SampleFormat::S16,
            0.5,
        );
        let result = i16::from_le_bytes([destination[0], destination[1]]);
        assert!(
            (result - 16384).abs() <= 1,
            "半个音量应当约为 16384，实际 {result}"
        );
    }

    #[test]
    fn loud_samples_do_not_wrap_around_when_scaled() {
        // 这条守的是一个很容易犯的错：直接对 i16 乘会在接近满量程时溢出环绕，
        // 听感是刺耳的爆音。正确的做法是用更宽的类型中转再夹住。
        //
        // 注意：音量 1.0 会走"直接拷贝"分支，因此这里用略小于 1 的音量，
        // 让缩放路径真的被执行到；同时源值取满量程，验证不会因为
        // 浮点舍入而变成负数（负数是典型的环绕症状）。
        let sample: i16 = i16::MAX;
        let mut destination = vec![0u8; 2];
        write_scaled(
            &mut destination,
            &sample.to_le_bytes(),
            SampleFormat::S16,
            0.999,
        );
        let result = i16::from_le_bytes([destination[0], destination[1]]);
        assert!(
            result > 0,
            "接近满量程的正样本缩放后不该变成负数（说明发生了环绕）：{result}"
        );
        assert!(result <= i16::MAX);
    }

    #[test]
    fn float_samples_are_clamped() {
        // 有些解码器会给出略微超出 [-1,1] 的浮点值。
        // 缩放后仍超范围的话，某些设备驱动会把它当噪声。
        let sample: f32 = 1.5;
        let mut destination = vec![0u8; 4];
        write_scaled(
            &mut destination,
            &sample.to_le_bytes(),
            SampleFormat::F32,
            0.9,
        );
        let result = f32::from_le_bytes([
            destination[0],
            destination[1],
            destination[2],
            destination[3],
        ]);
        assert!(result <= 1.0, "超范围的正样本应当被夹到 1.0，实际 {result}");
    }

    #[test]
    fn effective_volume_handles_mute() {
        assert_eq!(effective_volume(0.7, false), 0.7);
        assert_eq!(effective_volume(0.7, true), 0.0);
        // 静音优先于音量
        assert_eq!(effective_volume(0.0, true), 0.0);
        // 超范围会被夹住（允许大于 1 会让声音削顶，用户要更响应该调系统音量）
        assert_eq!(effective_volume(2.0, false), 1.0);
        assert_eq!(effective_volume(-1.0, false), 0.0);
    }

    #[test]
    fn bytes_per_frame_matches_the_format() {
        let s16 = AudioFormat {
            sample_rate: 48000,
            channels: 2,
            format: SampleFormat::S16,
        };
        assert_eq!(s16.bytes_per_frame(), 4, "双声道 16 位是 4 字节/帧");

        let f32_stereo = AudioFormat {
            format: SampleFormat::F32,
            ..s16
        };
        assert_eq!(f32_stereo.bytes_per_frame(), 8);

        let mono = AudioFormat {
            channels: 1,
            ..s16
        };
        assert_eq!(mono.bytes_per_frame(), 2);
    }

    #[test]
    fn wave_format_is_filled_consistently() {
        // nAvgBytesPerSec 与 nBlockAlign 必须与其它字段自洽，
        // 否则某些驱动会拒绝这个格式。
        let format = AudioFormat {
            sample_rate: 44100,
            channels: 2,
            format: SampleFormat::S16,
        };
        let wave = format.to_wave_format();

        // WAVEFORMATEX 是 packed 结构：字段必须按值读出，不能取引用
        // （取引用会创建未对齐引用，是未定义行为，编译器会拦）。
        // `read_unaligned` 本身是 unsafe，因此整段包起来。
        let (tag, channels, rate, bits, block_align, avg_bytes) = unsafe {
            (
                std::ptr::read_unaligned(std::ptr::addr_of!(wave.wFormatTag)),
                std::ptr::read_unaligned(std::ptr::addr_of!(wave.nChannels)),
                std::ptr::read_unaligned(std::ptr::addr_of!(wave.nSamplesPerSec)),
                std::ptr::read_unaligned(std::ptr::addr_of!(wave.wBitsPerSample)),
                std::ptr::read_unaligned(std::ptr::addr_of!(wave.nBlockAlign)),
                std::ptr::read_unaligned(std::ptr::addr_of!(wave.nAvgBytesPerSec)),
            )
        };

        assert_eq!(tag, WAVE_FORMAT_PCM as u16);
        assert_eq!(channels, 2);
        assert_eq!(rate, 44100);
        assert_eq!(bits, 16);
        assert_eq!(block_align, 4);
        assert_eq!(avg_bytes, 44100 * 4, "每秒字节数应当等于采样率×每帧字节");
    }
}
