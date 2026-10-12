use std::path::Path;
use std::ptr;
use std::sync::Once;

use ffmpeg::format::Pixel;
use ffmpeg::software::scaling;
use ffmpeg::{Packet, codec, decoder, ffi, format, frame, media, threading};
use ffmpeg_next as ffmpeg;

use crate::{ClipInfo, Error, Options, Ratio};

const FALLBACK_FRAME_RATE: ffi::AVRational = ffi::AVRational { num: 30, den: 1 };
const MICROSECONDS: ffi::AVRational = ffi::AVRational {
    num: 1,
    den: 1_000_000,
};
const ONE_SECOND: ffi::AVRational = ffi::AVRational { num: 1, den: 1 };
/// Upper bound assumed for how many frames a keyframe's display time trails its decode time.
const REORDER_MARGIN_FRAMES: u64 = 16;
/// How many successively earlier keyframes a seek tries before settling for what it has.
const MAX_SEEK_ATTEMPTS: usize = 4;

struct Hardware {
    device: ffi::AVHWDeviceType,
    pixel: ffi::AVPixelFormat,
    name: &'static str,
}

#[cfg(windows)]
const HARDWARE: Option<Hardware> = Some(Hardware {
    device: ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
    pixel: ffi::AVPixelFormat::AV_PIX_FMT_D3D11,
    name: "D3D11VA",
});

#[cfg(target_os = "linux")]
const HARDWARE: Option<Hardware> = Some(Hardware {
    device: ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
    pixel: ffi::AVPixelFormat::AV_PIX_FMT_VAAPI,
    name: "VAAPI",
});

#[cfg(not(any(windows, target_os = "linux")))]
const HARDWARE: Option<Hardware> = None;

/// Presentation span of a decoded frame, in stream time base units.
#[derive(Clone, Copy)]
struct Span {
    pts: i64,
    end: i64,
}

pub(crate) struct Converted {
    pub frame: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub hardware: bool,
}

pub(crate) struct Session {
    input: format::context::Input,
    decoder: decoder::Video,
    stream: usize,
    time_base: ffi::AVRational,
    /// Duration of one frame at the nominal frame rate.
    frame_time: ffi::AVRational,
    start_pts: i64,
    /// Keyframe timestamps from the container index, ascending. These are decode timestamps, which
    /// can sit a few frames before the presentation time when the clip has B-frames; they are only
    /// used to decide between seeking and decoding forward.
    keyframes: Vec<i64>,
    decoded: frame::Video,
    /// Span of the frame held in `decoded`, if it holds one.
    current: Option<Span>,
    drained: bool,
    transfer: frame::Video,
    scaler: Option<scaling::Context>,
    info: ClipInfo,
}

impl Session {
    pub(crate) fn open(path: &Path, options: Options) -> Result<Self, Error> {
        init();
        let input = format::input(path).map_err(|error| Error::new("open clip", error))?;
        let stream = input
            .streams()
            .best(media::Type::Video)
            .ok_or_else(|| Error("clip has no video stream".to_string()))?;
        let stream_index = stream.index();
        let time_base = raw_rational(stream.time_base());
        let frame_rate = [stream.avg_frame_rate(), stream.rate()]
            .into_iter()
            .map(raw_rational)
            .find(|rate| rate.num > 0 && rate.den > 0)
            .unwrap_or(FALLBACK_FRAME_RATE);
        let frame_time = ffi::AVRational {
            num: frame_rate.den,
            den: frame_rate.num,
        };
        let start_pts = match stream.start_time() {
            ffi::AV_NOPTS_VALUE => 0,
            start => start,
        };
        let frame_count = match stream.frames() {
            count if count > 0 => count as u64,
            _ => unsafe { ffi::av_rescale_q(input.duration(), MICROSECONDS, frame_time) }.max(1)
                as u64,
        };
        let keyframes = keyframe_times(&stream);

        let mut context = codec::Context::from_parameters(stream.parameters())
            .map_err(|error| Error::new("read codec parameters", error))?;
        let codec = decoder::find(context.id())
            .ok_or_else(|| Error(format!("no decoder for {:?}", context.id())))?;
        let hardware = options
            .hardware
            .then(|| enable_hardware(&mut context, &codec))
            .flatten();
        if hardware.is_none() {
            context.set_threading(threading::Config::kind(threading::Type::Frame));
        }
        let decoder = context
            .decoder()
            .open_as(codec)
            .and_then(|opened| opened.video())
            .map_err(|error| Error::new("open decoder", error))?;

        let (width, height) = (decoder.width(), decoder.height());
        let (preview_width, preview_height) = fit_inside(width, height, options.max_preview);
        let info = ClipInfo {
            path: path.to_path_buf(),
            codec: codec.name().to_string(),
            width,
            height,
            frame_rate: Ratio {
                num: frame_rate.num,
                den: frame_rate.den,
            },
            frame_count,
            keyframe_count: keyframes.len(),
            hardware,
            preview_width,
            preview_height,
        };
        Ok(Self {
            input,
            decoder,
            stream: stream_index,
            time_base,
            frame_time,
            start_pts,
            keyframes,
            decoded: frame::Video::empty(),
            current: None,
            drained: false,
            transfer: frame::Video::empty(),
            scaler: None,
            info,
        })
    }

    pub(crate) fn info(&self) -> &ClipInfo {
        &self.info
    }

    // Frame numbers assume a constant frame rate. Variable frame rate clips need the per-frame
    // timestamp table instead; that comes with the real frame index.
    pub(crate) fn frame_to_pts(&self, frame: u64) -> i64 {
        self.start_pts + unsafe { ffi::av_rescale_q(frame as i64, self.frame_time, self.time_base) }
    }

    fn pts_to_frame(&self, pts: i64) -> u64 {
        unsafe { ffi::av_rescale_q(pts - self.start_pts, self.time_base, self.frame_time) }.max(0)
            as u64
    }

    /// Whether the frame already decoded is the one displayed at `target`.
    pub(crate) fn shows(&self, target: i64) -> bool {
        self.current
            .is_some_and(|span| span.pts <= target && target < span.end)
    }

    /// Whether decoding has arrived at (or passed) the frame displayed at `target`.
    pub(crate) fn reached(&self, target: i64) -> bool {
        self.current.is_some_and(|span| span.end > target)
    }

    /// Seeking pays off when the target is behind us, or when a keyframe lies between the decoder
    /// position and the target so the frames before it can be skipped.
    pub(crate) fn needs_seek(&self, target: i64) -> bool {
        let Some(current) = self.current else {
            return true;
        };
        if target < current.pts {
            return true;
        }
        // A keyframe is only a shortcut if it is displayed at or before the target. The index
        // holds decode times, so keyframes too close to the target to be sure are left out.
        let margin = self.frame_to_pts(REORDER_MARGIN_FRAMES) - self.start_pts;
        match self
            .keyframes
            .partition_point(|&keyframe| keyframe + margin <= target)
        {
            0 => {
                let one_second = unsafe { ffi::av_rescale_q(1, ONE_SECOND, self.time_base) };
                target - current.pts > one_second
            }
            after => self.keyframes[after - 1] > current.pts,
        }
    }

    /// Seeks to the last keyframe displayed at or before `target` and decodes it. Returns false
    /// when there is nothing to decode there.
    pub(crate) fn seek(&mut self, target: i64) -> Result<bool, Error> {
        // Containers index keyframes by decode time. With B-frames a keyframe is decoded before
        // the frames shown just ahead of it, so the demuxer can pick a keyframe that is displayed
        // after `target`. When that happens, try again from the keyframe before it.
        let mut seek_to = target;
        for _ in 0..MAX_SEEK_ATTEMPTS {
            if let Err(error) = self.seek_demuxer(seek_to) {
                if seek_to == target {
                    return Err(error);
                }
                // Nothing that early in the file: the target lies before the first frame.
                self.seek_demuxer(target)?;
                return self.advance();
            }
            if !self.advance()? {
                return Ok(false);
            }
            if self.current.is_none_or(|landed| landed.pts <= target) {
                return Ok(true);
            }
            seek_to = self.before_keyframe_at(seek_to);
        }
        Ok(true)
    }

    /// A timestamp that seeks to the keyframe before the one a seek to `timestamp` picks.
    fn before_keyframe_at(&self, timestamp: i64) -> i64 {
        match self.keyframes.partition_point(|&keyframe| keyframe <= timestamp) {
            // No index to consult, so step back by a fixed amount instead.
            0 => timestamp - unsafe { ffi::av_rescale_q(1, ONE_SECOND, self.time_base) },
            after => self.keyframes[after - 1] - 1,
        }
    }

    fn seek_demuxer(&mut self, target: i64) -> Result<(), Error> {
        let result = unsafe {
            ffi::av_seek_frame(
                self.input.as_mut_ptr(),
                self.stream as i32,
                target,
                ffi::AVSEEK_FLAG_BACKWARD,
            )
        };
        if result < 0 {
            return Err(Error::new("seek", ffmpeg::Error::from(result)));
        }
        self.decoder.flush();
        self.current = None;
        self.drained = false;
        Ok(())
    }

    /// Decodes the next frame in presentation order. Returns false at the end of the stream.
    pub(crate) fn advance(&mut self) -> Result<bool, Error> {
        loop {
            match self.decoder.receive_frame(&mut self.decoded) {
                Ok(()) => {
                    let pts = self
                        .decoded
                        .timestamp()
                        .or(self.decoded.pts())
                        .or(self.current.map(|span| span.end))
                        .unwrap_or(self.start_pts);
                    let duration = match unsafe { (*self.decoded.as_ptr()).duration } {
                        duration if duration > 0 => duration,
                        _ => self.frame_to_pts(1) - self.start_pts,
                    };
                    self.current = Some(Span {
                        pts,
                        end: pts + duration.max(1),
                    });
                    return Ok(true);
                }
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => {}
                Err(ffmpeg::Error::Eof) => {
                    self.current = None;
                    return Ok(false);
                }
                Err(error) => return Err(Error::new("decode", error)),
            }
            if self.drained {
                self.current = None;
                return Ok(false);
            }
            let mut packet = Packet::empty();
            match packet.read(&mut self.input) {
                Ok(()) if packet.stream() == self.stream => {
                    match self.decoder.send_packet(&packet) {
                        // A damaged packet costs a frame, not the whole clip.
                        Ok(()) | Err(ffmpeg::Error::InvalidData) => {}
                        Err(error) => return Err(Error::new("decode", error)),
                    }
                }
                Ok(()) => {}
                Err(ffmpeg::Error::Eof) => {
                    let _ = self.decoder.send_eof();
                    self.drained = true;
                }
                Err(error) => return Err(Error::new("read clip", error)),
            }
        }
    }

    /// Converts the current frame to a preview-sized RGBA picture.
    pub(crate) fn picture(&mut self) -> Result<Converted, Error> {
        let span = self
            .current
            .ok_or_else(|| Error("no decoded frame to show".to_string()))?;
        let hardware = HARDWARE.is_some_and(|hardware| self.decoded.format() == hardware.pixel.into());
        if hardware {
            let result = unsafe {
                ffi::av_frame_unref(self.transfer.as_mut_ptr());
                ffi::av_hwframe_transfer_data(self.transfer.as_mut_ptr(), self.decoded.as_ptr(), 0)
            };
            if result < 0 {
                return Err(Error::new(
                    "copy frame from the GPU",
                    ffmpeg::Error::from(result),
                ));
            }
        }
        let source = if hardware {
            &self.transfer
        } else {
            &self.decoded
        };

        let (format, width, height) = (source.format(), source.width(), source.height());
        let (out_width, out_height) = (self.info.preview_width, self.info.preview_height);
        let stale = self.scaler.as_ref().is_none_or(|scaler| {
            let input = scaler.input();
            input.format != format || input.width != width || input.height != height
        });
        if stale {
            self.scaler = Some(
                scaling::Context::get(
                    format,
                    width,
                    height,
                    Pixel::RGBA,
                    out_width,
                    out_height,
                    scaling::Flags::BILINEAR,
                )
                .map_err(|error| Error::new("set up picture scaler", error))?,
            );
        }
        let mut scaled = frame::Video::empty();
        self.scaler
            .as_mut()
            .expect("scaler was just created")
            .run(source, &mut scaled)
            .map_err(|error| Error::new("scale picture", error))?;

        let stride = scaled.stride(0);
        let row = out_width as usize * 4;
        let data = scaled.data(0);
        let mut rgba = Vec::with_capacity(row * out_height as usize);
        for line in 0..out_height as usize {
            rgba.extend_from_slice(&data[line * stride..line * stride + row]);
        }
        Ok(Converted {
            frame: self.pts_to_frame(span.pts),
            width: out_width,
            height: out_height,
            rgba,
            hardware,
        })
    }
}

fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        ffmpeg::init().expect("FFmpeg failed to initialise");
        // Seeking into an open GOP makes the H.264 decoder report the references it cannot find
        // for every skipped leading frame, at error level; that is expected and very noisy.
        ffmpeg::log::set_level(ffmpeg::log::Level::Fatal);
    });
}

fn raw_rational(value: ffmpeg::Rational) -> ffi::AVRational {
    ffi::AVRational {
        num: value.numerator(),
        den: value.denominator(),
    }
}

fn keyframe_times(stream: &ffmpeg::Stream) -> Vec<i64> {
    let mut keyframes = Vec::new();
    unsafe {
        let stream = stream.as_ptr() as *mut ffi::AVStream;
        for index in 0..ffi::avformat_index_get_entries_count(stream) {
            let entry = ffi::avformat_index_get_entry(stream, index);
            if !entry.is_null() && (*entry).flags() & ffi::AVINDEX_KEYFRAME != 0 {
                keyframes.push((*entry).timestamp);
            }
        }
    }
    keyframes.sort_unstable();
    keyframes
}

/// Attaches a GPU decode device to `context` when this platform has one and `codec` supports it.
fn enable_hardware(context: &mut codec::Context, codec: &ffmpeg::Codec) -> Option<&'static str> {
    let hardware = HARDWARE?;
    unsafe {
        let supported = (0..)
            .map(|index| ffi::avcodec_get_hw_config(codec.as_ptr(), index))
            .take_while(|config| !config.is_null())
            .any(|config| {
                (*config).device_type == hardware.device
                    && (*config).pix_fmt == hardware.pixel
                    && (*config).methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 != 0
            });
        if !supported {
            return None;
        }
        let mut device = ptr::null_mut();
        if ffi::av_hwdevice_ctx_create(&mut device, hardware.device, ptr::null(), ptr::null_mut(), 0)
            < 0
        {
            return None;
        }
        // The codec context takes ownership of the device reference.
        (*context.as_mut_ptr()).hw_device_ctx = device;
        (*context.as_mut_ptr()).get_format = Some(pick_hardware_format);
    }
    Some(hardware.name)
}

/// FFmpeg offers the formats it can output; taking the GPU one keeps frames on the GPU. If the
/// driver rejects the stream FFmpeg calls again without it, and we fall back to software.
unsafe extern "C" fn pick_hardware_format(
    context: *mut ffi::AVCodecContext,
    offered: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    unsafe {
        if let Some(hardware) = HARDWARE {
            let mut format = offered;
            while *format != ffi::AVPixelFormat::AV_PIX_FMT_NONE {
                if *format == hardware.pixel {
                    return hardware.pixel;
                }
                format = format.add(1);
            }
        }
        ffi::avcodec_default_get_format(context, offered)
    }
}

/// Largest even-sized picture with the source aspect ratio that fits inside `max`.
fn fit_inside(width: u32, height: u32, max: (u32, u32)) -> (u32, u32) {
    let (width, height) = (width.max(2) as u64, height.max(2) as u64);
    let (max_width, max_height) = (max.0.max(2) as u64, max.1.max(2) as u64);
    let (out_width, out_height) = if width * max_height > height * max_width {
        let out_width = width.min(max_width);
        (out_width, height * out_width / width)
    } else {
        let out_height = height.min(max_height);
        (width * out_height / height, out_height)
    };
    (
        (out_width as u32 & !1).max(2),
        (out_height as u32 & !1).max(2),
    )
}
