use std::collections::BTreeMap;
use std::path::Path;
use std::ptr;
use std::sync::Once;

use ffmpeg::format::Pixel;
use ffmpeg::software::scaling;
use ffmpeg::{Packet, codec, decoder, ffi, format, frame, media, threading};
use ffmpeg_next as ffmpeg;

use crate::{ClipInfo, Colors, Error, Matrix, Options, Output, Pixels, Ratio};

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
/// Cached pictures stay in 4:2:0 YUV: 1.5 bytes per pixel against 4 for RGBA.
const PREVIEW_FORMAT: Pixel = Pixel::NV12;
const HD_MIN_HEIGHT: u32 = 720;

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
    pub pixels: Pixels,
    pub hardware: bool,
}

/// A GPU decode device. The decoders a session goes through share one.
struct Device(*mut ffi::AVBufferRef);

impl Device {
    /// The platform's decode device, when there is one and `codec` can use it.
    fn create(codec: &ffmpeg::Codec) -> Option<Self> {
        let hardware = HARDWARE?;
        unsafe {
            let supported = (0..)
                .map(|index| ffi::avcodec_get_hw_config(codec.as_ptr(), index))
                .take_while(|config| !config.is_null())
                .any(|config| {
                    (*config).device_type == hardware.device
                        && (*config).pix_fmt == hardware.pixel
                        && (*config).methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32
                            != 0
                });
            if !supported {
                return None;
            }
            let mut device = ptr::null_mut();
            let created = ffi::av_hwdevice_ctx_create(
                &mut device,
                hardware.device,
                ptr::null(),
                ptr::null_mut(),
                0,
            );
            (created >= 0).then_some(Self(device))
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe { ffi::av_buffer_unref(&mut self.0) };
    }
}

/// What it takes to open a decoder for the clip, which happens more than once: see
/// [`Session::seek_demuxer`].
struct DecoderSource {
    parameters: codec::Parameters,
    codec: ffmpeg::Codec,
    /// None when decoding in software.
    device: Option<Device>,
}

impl DecoderSource {
    fn open(&self) -> Result<decoder::Video, Error> {
        let mut context = codec::Context::from_parameters(self.parameters.clone())
            .map_err(|error| Error::new("read codec parameters", error))?;
        match &self.device {
            Some(device) => unsafe {
                // The codec context takes ownership of the reference made here.
                (*context.as_mut_ptr()).hw_device_ctx = ffi::av_buffer_ref(device.0);
                (*context.as_mut_ptr()).get_format = Some(pick_hardware_format);
            },
            None => context.set_threading(threading::Config::kind(threading::Type::Frame)),
        }
        context
            .decoder()
            .open_as(self.codec)
            .and_then(|opened| opened.video())
            .map_err(|error| Error::new("open decoder", error))
    }
}

pub(crate) struct Session {
    input: format::context::Input,
    decoder: decoder::Video,
    source: DecoderSource,
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
    /// Source frames to preview-sized YUV.
    preview_scaler: Option<scaling::Context>,
    /// Preview-sized YUV to RGBA.
    rgba_scaler: Option<scaling::Context>,
    /// Known once the first frame has been decoded.
    colors: Option<Colors>,
    /// Size of the pictures made from now on.
    preview_size: (u32, u32),
    cache: PictureCache,
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

        // A copy of its own, so the decoder can be opened again without the stream at hand.
        let parameters = stream.parameters().clone();
        let codec = decoder::find(parameters.id())
            .ok_or_else(|| Error(format!("no decoder for {:?}", parameters.id())))?;
        let source = DecoderSource {
            parameters,
            codec,
            device: options.hardware.then(|| Device::create(&codec)).flatten(),
        };
        let decoder = source.open()?;
        let hardware = HARDWARE
            .filter(|_| source.device.is_some())
            .map(|hardware| hardware.name);

        let (width, height) = (decoder.width(), decoder.height());
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
        };
        Ok(Self {
            input,
            decoder,
            source,
            stream: stream_index,
            time_base,
            frame_time,
            start_pts,
            keyframes,
            decoded: frame::Video::empty(),
            current: None,
            drained: false,
            transfer: frame::Video::empty(),
            preview_scaler: None,
            rgba_scaler: None,
            colors: None,
            preview_size: fit_inside(width, height, options.max_preview),
            cache: PictureCache::new(options.cache_bytes),
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
        if self.source.device.is_some() {
            // On the GPU a flush is not enough. A decoder that was flushed and then started in
            // the middle of an open GOP kept numbering its frames correctly while their pictures
            // stopped changing some hundred frames on, without an error (seen with D3D11VA on a
            // Radeon 760M). A decoder that has seen nothing else of the file does not do that.
            self.decoded = frame::Video::empty();
            self.decoder = self.source.open()?;
        } else {
            self.decoder.flush();
        }
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

    /// Start of the frame held by the decoder, if it holds one.
    pub(crate) fn position(&self) -> Option<i64> {
        self.current.map(|span| span.pts)
    }

    /// Timestamp `frames` before `pts`, clamped to the start of the clip.
    pub(crate) fn frames_before(&self, pts: i64, frames: u64) -> i64 {
        (pts - (self.frame_to_pts(frames) - self.start_pts)).max(self.start_pts)
    }

    /// Makes pictures fit inside `max` from now on. Pictures cached at another size are dropped.
    pub(crate) fn set_max_preview(&mut self, max: (u32, u32)) {
        let size = fit_inside(self.info.width, self.info.height, max);
        if size != self.preview_size {
            self.preview_size = size;
            self.cache.clear();
        }
    }

    /// Cached pictures are dropped farthest from this timestamp first.
    pub(crate) fn set_focus(&mut self, target: i64) {
        self.cache.focus = target;
    }

    /// Timestamp of the first frame.
    pub(crate) fn start(&self) -> i64 {
        self.start_pts
    }

    /// Whether a picture of the frame shown at `target` is in the cache.
    pub(crate) fn remembers(&self, target: i64) -> bool {
        self.cache.find(target).is_some()
    }

    /// Start of the cached frame shown at `target`.
    pub(crate) fn remembered_start(&self, target: i64) -> Option<i64> {
        self.cache.find(target).map(|(pts, _)| pts)
    }

    /// The picture of the frame shown at `target`, if it is in the cache.
    pub(crate) fn recall(
        &mut self,
        target: i64,
        output: Output,
    ) -> Result<Option<Converted>, Error> {
        let Some((pts, cached)) = self.cache.find(target) else {
            return Ok(None);
        };
        let pixels = to_pixels(output, &mut self.rgba_scaler, self.colors, &cached.picture)?;
        Ok(Some(Converted {
            frame: self.pts_to_frame(pts),
            width: cached.picture.width(),
            height: cached.picture.height(),
            pixels,
            hardware: cached.hardware,
        }))
    }

    /// Keeps a picture of the current frame in the cache.
    pub(crate) fn remember(&mut self) -> Result<(), Error> {
        let span = self
            .current
            .ok_or_else(|| Error("no decoded frame to keep".to_string()))?;
        if !self.cache.frames.contains_key(&span.pts) {
            let (picture, hardware) = self.preview()?;
            self.cache.insert(span, picture, hardware);
        }
        Ok(())
    }

    /// Turns the current frame into a preview-sized picture, and keeps it in the cache.
    pub(crate) fn picture(&mut self, output: Output) -> Result<Converted, Error> {
        let span = self
            .current
            .ok_or_else(|| Error("no decoded frame to show".to_string()))?;
        if let Some(converted) = self.recall(span.pts, output)? {
            return Ok(converted);
        }
        let (picture, hardware) = self.preview()?;
        let pixels = to_pixels(output, &mut self.rgba_scaler, self.colors, &picture)?;
        let (width, height) = (picture.width(), picture.height());
        self.cache.insert(span, picture, hardware);
        Ok(Converted {
            frame: self.pts_to_frame(span.pts),
            width,
            height,
            pixels,
            hardware,
        })
    }

    /// Scales the current frame down to preview size, staying in YUV so it is cheap to keep.
    fn preview(&mut self) -> Result<(frame::Video, bool), Error> {
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
        // Colour tags live on the decoder's frame; the copy from the GPU carries pixels only.
        let colors = *self.colors.get_or_insert_with(|| Colors::of(&self.decoded));
        let source = if hardware {
            &self.transfer
        } else {
            &self.decoded
        };

        let (format, width, height) = (source.format(), source.width(), source.height());
        let (preview_width, preview_height) = self.preview_size;
        let stale = self.preview_scaler.as_ref().is_none_or(|scaler| {
            let (input, output) = (scaler.input(), scaler.output());
            input.format != format
                || input.width != width
                || input.height != height
                || output.width != preview_width
                || output.height != preview_height
        });
        if stale {
            let mut scaler = scaling::Context::get(
                format,
                width,
                height,
                PREVIEW_FORMAT,
                preview_width,
                preview_height,
                scaling::Flags::BILINEAR,
            )
            .map_err(|error| Error::new("set up preview scaler", error))?;
            // Keep the sample values as they are; the range is applied once, on the way to RGB.
            colors.apply(&mut scaler, colors.full_range);
            self.preview_scaler = Some(scaler);
        }
        let mut picture = frame::Video::empty();
        self.preview_scaler
            .as_mut()
            .expect("preview scaler was just created")
            .run(source, &mut picture)
            .map_err(|error| Error::new("scale preview", error))?;
        Ok((picture, hardware))
    }
}

impl Colors {
    fn of(frame: &frame::Video) -> Self {
        use ffmpeg::color::{Range, Space};
        let matrix = match frame.color_space() {
            Space::BT709 => Matrix::Bt709,
            Space::BT2020NCL | Space::BT2020CL => Matrix::Bt2020,
            Space::BT470BG | Space::SMPTE170M => Matrix::Bt601,
            // Untagged video is BT.709 from HD upwards and BT.601 below, by convention.
            _ if frame.height() >= HD_MIN_HEIGHT => Matrix::Bt709,
            _ => Matrix::Bt601,
        };
        Self {
            matrix,
            full_range: frame.color_range() == Range::JPEG,
        }
    }

    /// Tells `scaler` how its input is encoded and whether its output is full range.
    fn apply(self, scaler: &mut scaling::Context, full_range_output: bool) {
        let matrix = match self.matrix {
            Matrix::Bt601 => ffi::SWS_CS_ITU601,
            Matrix::Bt709 => ffi::SWS_CS_ITU709,
            Matrix::Bt2020 => ffi::SWS_CS_BT2020,
        };
        unsafe {
            let coefficients = ffi::sws_getCoefficients(matrix);
            // Neutral brightness, contrast and saturation, in swscale's 16.16 fixed point.
            ffi::sws_setColorspaceDetails(
                scaler.as_mut_ptr(),
                coefficients,
                i32::from(self.full_range),
                coefficients,
                i32::from(full_range_output),
                0,
                1 << 16,
                1 << 16,
            );
        }
    }
}

/// Puts a cached picture in the form the viewer asked for.
fn to_pixels(
    output: Output,
    rgba_scaler: &mut Option<scaling::Context>,
    colors: Option<Colors>,
    picture: &frame::Video,
) -> Result<Pixels, Error> {
    match output {
        Output::Rgba => to_rgba(rgba_scaler, colors, picture).map(Pixels::Rgba),
        Output::Yuv => {
            let colors =
                colors.ok_or_else(|| Error("picture has no colour information".to_string()))?;
            let (width, height) = (picture.width() as usize, picture.height() as usize);
            Ok(Pixels::Nv12 {
                luma: packed_rows(picture, 0, width, height),
                // Half as many samples per row, two bytes each.
                chroma: packed_rows(picture, 1, width, height / 2),
                colors,
            })
        }
    }
}

/// Copies a plane without the padding FFmpeg leaves at the end of each row.
fn packed_rows(picture: &frame::Video, plane: usize, row_bytes: usize, rows: usize) -> Vec<u8> {
    let stride = picture.stride(plane);
    let data = picture.data(plane);
    let mut packed = Vec::with_capacity(row_bytes * rows);
    for row in 0..rows {
        packed.extend_from_slice(&data[row * stride..row * stride + row_bytes]);
    }
    packed
}

fn to_rgba(
    scaler: &mut Option<scaling::Context>,
    colors: Option<Colors>,
    picture: &frame::Video,
) -> Result<Vec<u8>, Error> {
    let (width, height) = (picture.width(), picture.height());
    // Pictures change size when the viewer does.
    let stale = scaler.as_ref().is_none_or(|scaler| {
        let input = scaler.input();
        input.width != width || input.height != height
    });
    if stale {
        let mut created = scaling::Context::get(
            picture.format(),
            width,
            height,
            Pixel::RGBA,
            width,
            height,
            // Without accurate rounding swscale's quick tables come out almost two levels dark.
            scaling::Flags::BILINEAR
                | scaling::Flags::ACCURATE_RND
                | scaling::Flags::FULL_CHR_H_INT,
        )
        .map_err(|error| Error::new("set up RGB conversion", error))?;
        if let Some(colors) = colors {
            colors.apply(&mut created, true);
        }
        *scaler = Some(created);
    }
    let mut converted = frame::Video::empty();
    scaler
        .as_mut()
        .expect("RGB conversion was just created")
        .run(picture, &mut converted)
        .map_err(|error| Error::new("convert picture to RGB", error))?;

    Ok(packed_rows(&converted, 0, width as usize * 4, height as usize))
}

struct Cached {
    end: i64,
    hardware: bool,
    picture: frame::Video,
}

/// Preview-sized pictures of frames near the playhead, keyed by presentation timestamp.
struct PictureCache {
    frames: BTreeMap<i64, Cached>,
    bytes: usize,
    budget: usize,
    focus: i64,
}

impl PictureCache {
    fn new(budget: usize) -> Self {
        Self {
            frames: BTreeMap::new(),
            bytes: 0,
            budget,
            focus: 0,
        }
    }

    /// The cached frame displayed at `target`, with its timestamp.
    fn find(&self, target: i64) -> Option<(i64, &Cached)> {
        self.frames
            .range(..=target)
            .next_back()
            .filter(|(_, cached)| cached.end > target)
            .map(|(pts, cached)| (*pts, cached))
    }

    fn clear(&mut self) {
        self.frames.clear();
        self.bytes = 0;
    }

    fn insert(&mut self, span: Span, picture: frame::Video, hardware: bool) {
        self.bytes += picture_bytes(&picture);
        let replaced = self.frames.insert(
            span.pts,
            Cached {
                end: span.end,
                hardware,
                picture,
            },
        );
        if let Some(replaced) = replaced {
            self.bytes -= picture_bytes(&replaced.picture);
        }
        while self.bytes > self.budget && self.frames.len() > 1 {
            let (Some((&first, _)), Some((&last, _))) =
                (self.frames.first_key_value(), self.frames.last_key_value())
            else {
                break;
            };
            let farthest = if self.focus - first >= last - self.focus {
                first
            } else {
                last
            };
            if let Some(dropped) = self.frames.remove(&farthest) {
                self.bytes -= picture_bytes(&dropped.picture);
            }
        }
    }
}

fn picture_bytes(picture: &frame::Video) -> usize {
    (0..picture.planes())
        .map(|plane| picture.data(plane).len())
        .sum()
}

pub(crate) fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        ffmpeg::init().expect("FFmpeg failed to initialise");
        // Seeking into an open GOP makes the H.264 decoder report the references it cannot find
        // for every skipped leading frame, at error level; that is expected and very noisy.
        ffmpeg::log::set_level(ffmpeg::log::Level::Fatal);
    });
}

pub(crate) fn raw_rational(value: ffmpeg::Rational) -> ffi::AVRational {
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
