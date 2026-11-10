//! Writing a film: full-size pictures read from clips, and an MP4 file they and the sound are
//! encoded into.

use std::path::Path;
use std::ptr;

use ffmpeg::format::Pixel;
use ffmpeg::software::scaling;
use ffmpeg::{Dictionary, Packet, codec, decoder, encoder, ffi, format, frame, media};
use ffmpeg_next as ffmpeg;

use crate::session::{DecoderSource, Device, HARDWARE, init, raw_rational};
use crate::{CHANNELS, Colors, Error, Matrix};

/// Video encoders tried in turn: the graphics card's own first, then the system's, then one
/// that runs on the processor and is always there.
#[cfg(windows)]
const VIDEO_ENCODERS: &[&str] = &["h264_amf", "h264_mf", "libopenh264"];
#[cfg(not(windows))]
const VIDEO_ENCODERS: &[&str] = &["libopenh264"];
/// Names a video encoder to use instead of trying them in turn, for checking each of them.
const ENCODER_VARIABLE: &str = "CANNOTCUT_VIDEO_ENCODER";
/// The sound's bit rate, in bits a second.
pub const AUDIO_BIT_RATE: usize = 192_000;
/// A seek lands on the keyframe before this much ahead of the frame wanted, so that the frame
/// is reached however the file's keyframes and reordered frames fall.
const SEEK_RUN_UP: ffi::AVRational = ffi::AVRational { num: 1, den: 1 };
/// Black in limited-range YUV.
const BLACK_LUMA: u8 = 16;
const BLACK_CHROMA: u8 = 128;

/// The size of the pictures in a media file.
pub fn picture_size(path: &Path) -> Result<(u32, u32), Error> {
    init();
    let input = format::input(path).map_err(|error| Error::new("open clip", error))?;
    let stream = input
        .streams()
        .best(media::Type::Video)
        .ok_or_else(|| Error("clip has no video stream".to_string()))?;
    let parameters = stream.parameters();
    let (width, height) = unsafe {
        let parameters = parameters.as_ptr();
        ((*parameters).width, (*parameters).height)
    };
    Ok((width.max(0) as u32, height.max(0) as u32))
}

/// What the film is like.
#[derive(Clone, Copy, Debug)]
pub struct FilmFormat {
    pub width: u32,
    pub height: u32,
    /// Frames per second as a fraction: numerator, denominator.
    pub rate: (u32, u32),
    /// Sound samples per second.
    pub sample_rate: u32,
    /// The pictures' bit rate, in bits a second; they may take half as much again for a
    /// moment where much happens. Encoders aiming at a quality instead were tried: on a
    /// recorded screen they either came out blurrier at their best or had no ceiling at all
    /// on busy pictures.
    pub bit_rate: usize,
}

/// A picture of the film, in the form its encoder takes.
pub struct FilmPicture {
    frame: frame::Video,
    /// How much of each pixel is seen when laid over another picture, 0 to 255, row by row
    /// with no padding; None when all of it is.
    alpha: Option<Vec<u8>>,
}

impl FilmPicture {
    fn new(format: Pixel, width: u32, height: u32) -> Self {
        let mut picture = Self {
            frame: frame::Video::new(format, width, height),
            alpha: None,
        };
        picture.clear();
        picture
    }

    /// Fills the picture with packed 4:2:0 planes of its own size: `luma`, a (Cb, Cr) pair per
    /// two by two pixels in `chroma`, and how much of each pixel is seen in `alpha`, all in
    /// the film's colours.
    pub fn fill(&mut self, luma: &[u8], chroma: &[u8], alpha: Vec<u8>) {
        let width = self.frame.width() as usize;
        let height = self.frame.height() as usize;
        let (chroma_width, chroma_height) = (width / 2, height / 2);
        let nv12 = self.frame.format() == Pixel::NV12;
        let stride = self.frame.stride(0);
        let data = self.frame.data_mut(0);
        for row in 0..height {
            data[row * stride..][..width].copy_from_slice(&luma[row * width..][..width]);
        }
        if nv12 {
            let stride = self.frame.stride(1);
            let data = self.frame.data_mut(1);
            for row in 0..chroma_height {
                let packed = &chroma[row * chroma_width * 2..][..chroma_width * 2];
                data[row * stride..][..chroma_width * 2].copy_from_slice(packed);
            }
        } else {
            for (plane, value) in [(1, 0), (2, 1)] {
                let stride = self.frame.stride(plane);
                let data = self.frame.data_mut(plane);
                for row in 0..chroma_height {
                    for column in 0..chroma_width {
                        data[row * stride + column] =
                            chroma[(row * chroma_width + column) * 2 + value];
                    }
                }
            }
        }
        self.alpha = Some(alpha);
    }

    /// Lays a layer over this picture: the part `area` of `layer` (left, top, width, height in
    /// pixels: where a [`ClipReader`] drew its picture, fitted inside the frame), placed by
    /// `corners` (a 2x2 matrix column by column and an offset, from -1 to 1 across the picture
    /// to -1 to 1 across the frame, y up; see `Transform::corners`), and seen as much as
    /// `opacity` says, 0 to 1; with `wipe` (a direction and a threshold), only where a point of
    /// the frame (-1 to 1 across it, y up) has `direction · point <= threshold`. Both pictures
    /// are the film's size and format.
    pub fn lay_over(
        &mut self,
        layer: &FilmPicture,
        area: [u32; 4],
        corners: [f32; 6],
        opacity: f32,
        wipe: Option<([f32; 2], f32)>,
    ) {
        let [column_x0, column_x1, column_y0, column_y1, offset_x, offset_y] = corners;
        let determinant = column_x0 * column_y1 - column_y0 * column_x1;
        if determinant.abs() < 1e-9 || opacity <= 0.0 || area[2] == 0 || area[3] == 0 {
            return;
        }
        // From a point of the frame back to the picture: the matrix's inverse, row by row.
        let inverse = [
            column_y1 / determinant,
            -column_y0 / determinant,
            -column_x1 / determinant,
            column_x0 / determinant,
        ];
        let (width, height) = (self.frame.width() as f32, self.frame.height() as f32);
        let opacity = opacity.min(1.0);
        // How much of the layer is seen at a point of it, in luma pixels.
        let seen = |x: f32, y: f32| -> f32 {
            let Some(alpha) = &layer.alpha else {
                return opacity;
            };
            let row_length = layer.frame.width() as usize;
            let [left, top] = [area[0] as f32, area[1] as f32];
            let [right, bottom] = [left + area[2] as f32 - 1.0, top + area[3] as f32 - 1.0];
            let read = |x: f32, y: f32| {
                let at = y.clamp(top, bottom) as usize * row_length + x.clamp(left, right) as usize;
                f32::from(alpha.get(at).copied().unwrap_or(0))
            };
            opacity * bilinear(read, x, y) / 255.0
        };
        // `scale` is 1 for luma, 2 for chroma: a chroma sample covers two pixels each way.
        let lay = |target: &mut frame::Video, plane: usize, scale: f32, interleaved: bool| {
            let stride = target.stride(plane);
            let rows = (height / scale).ceil() as usize;
            let columns = (width / scale).ceil() as usize;
            let source_stride = layer.frame.stride(plane);
            let source = layer.frame.data(plane);
            // The area, in this plane's samples.
            let [left, top, area_width, area_height] = area.map(|side| side as f32 / scale);
            // Only where the placed picture can be: the box round its four corners.
            let mut low = [f32::MAX; 2];
            let mut high = [f32::MIN; 2];
            for corner in [[-1.0, -1.0], [1.0, -1.0], [-1.0, 1.0], [1.0, 1.0f32]] {
                let x = column_x0 * corner[0] + column_y0 * corner[1] + offset_x;
                let y = column_x1 * corner[0] + column_y1 * corner[1] + offset_y;
                let pixel = [(x + 1.0) / 2.0 * width / scale, (1.0 - y) / 2.0 * height / scale];
                for axis in 0..2 {
                    low[axis] = low[axis].min(pixel[axis]);
                    high[axis] = high[axis].max(pixel[axis]);
                }
            }
            let first_column = low[0].floor().max(0.0) as usize;
            let last_column = (high[0].ceil().max(0.0) as usize).min(columns);
            let first_row = low[1].floor().max(0.0) as usize;
            let last_row = (high[1].ceil().max(0.0) as usize).min(rows);
            let data = target.data_mut(plane);
            let values = if interleaved { 2 } else { 1 };
            for row in first_row..last_row {
                let frame_y = 1.0 - (row as f32 + 0.5) * scale / height * 2.0;
                let y = frame_y - offset_y;
                for column in first_column..last_column {
                    let frame_x = (column as f32 + 0.5) * scale / width * 2.0 - 1.0;
                    if wipe.is_some_and(|(direction, threshold)| {
                        direction[0] * frame_x + direction[1] * frame_y > threshold
                    }) {
                        continue;
                    }
                    let x = frame_x - offset_x;
                    let local_x = inverse[0] * x + inverse[1] * y;
                    let local_y = inverse[2] * x + inverse[3] * y;
                    if local_x.abs() > 1.0 || local_y.abs() > 1.0 {
                        continue;
                    }
                    // The sample of the layer it comes from, between four of them.
                    let source_x = left + (local_x + 1.0) / 2.0 * area_width - 0.5;
                    let source_y = top + (1.0 - local_y) / 2.0 * area_height - 0.5;
                    let seen = seen((source_x + 0.5) * scale - 0.5, (source_y + 0.5) * scale - 0.5);
                    if seen <= 0.0 {
                        continue;
                    }
                    for value in 0..values {
                        let read = |sample_x: f32, sample_y: f32| {
                            let sample_x = sample_x.clamp(left, left + area_width - 1.0);
                            let sample_y = sample_y.clamp(top, top + area_height - 1.0);
                            let at = sample_y as usize * source_stride
                                + sample_x as usize * values
                                + value;
                            f32::from(source[at])
                        };
                        let sampled = bilinear(read, source_x, source_y);
                        let at = row * stride + column * values + value;
                        let under = f32::from(data[at]);
                        let laid = under + (sampled - under) * seen;
                        data[at] = laid.round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
        };
        let nv12 = self.frame.format() == Pixel::NV12;
        lay(&mut self.frame, 0, 1.0, false);
        if nv12 {
            lay(&mut self.frame, 1, 2.0, true);
        } else {
            lay(&mut self.frame, 1, 2.0, false);
            lay(&mut self.frame, 2, 2.0, false);
        }
    }

    /// Makes it all black, and all of it seen.
    pub fn clear(&mut self) {
        self.alpha = None;
        let height = self.frame.height() as usize;
        for plane in 0..self.frame.planes() {
            let value = if plane == 0 { BLACK_LUMA } else { BLACK_CHROMA };
            let rows = if plane == 0 { height } else { height.div_ceil(2) };
            let stride = self.frame.stride(plane);
            let data = self.frame.data_mut(plane);
            let end = (rows * stride).min(data.len());
            data[..end].fill(value);
        }
    }
}

/// The value between four samples at `(x, y)`, read by `read` at whole positions.
fn bilinear(read: impl Fn(f32, f32) -> f32, x: f32, y: f32) -> f32 {
    let (base_x, base_y) = (x.floor(), y.floor());
    let (fraction_x, fraction_y) = (x - base_x, y - base_y);
    let top = read(base_x, base_y) * (1.0 - fraction_x) + read(base_x + 1.0, base_y) * fraction_x;
    let below = base_y + 1.0;
    let bottom = read(base_x, below) * (1.0 - fraction_x) + read(base_x + 1.0, below) * fraction_x;
    top * (1.0 - fraction_y) + bottom * fraction_y
}

/// Reads a clip's pictures at full size, in order, as a film needs them.
pub struct ClipReader {
    input: format::context::Input,
    decoder: decoder::Video,
    stream: usize,
    time_base: ffi::AVRational,
    frame_time: ffi::AVRational,
    start_pts: i64,
    decoded: frame::Video,
    transfer: frame::Video,
    /// Start and end of the frame in `decoded`, in stream time.
    current: Option<(i64, i64)>,
    drained: bool,
    scaler: *mut ffi::SwsContext,
    /// What the scaler was made for: source format and size, destination size.
    scaler_for: Option<(i32, u32, u32, u32, u32)>,
    /// Where the last picture went in the film's frame: left, top, width, height.
    drawn: Option<[u32; 4]>,
}

// The scaler is only used by the reader that owns it, on one thread at a time.
unsafe impl Send for ClipReader {}

impl ClipReader {
    /// Opens a clip's pictures, decoded on the graphics card when `hardware` and it can.
    pub fn open(path: &Path, hardware: bool) -> Result<Self, Error> {
        init();
        let input = format::input(path).map_err(|error| Error::new("open clip", error))?;
        let stream = input
            .streams()
            .best(media::Type::Video)
            .ok_or_else(|| Error("clip has no video stream".to_string()))?;
        let index = stream.index();
        let time_base = raw_rational(stream.time_base());
        let frame_rate = [stream.avg_frame_rate(), stream.rate()]
            .into_iter()
            .map(raw_rational)
            .find(|rate| rate.num > 0 && rate.den > 0)
            .unwrap_or(ffi::AVRational { num: 30, den: 1 });
        let start_pts = match stream.start_time() {
            ffi::AV_NOPTS_VALUE => 0,
            start => start,
        };
        let parameters = stream.parameters().clone();
        let codec = decoder::find(parameters.id())
            .ok_or_else(|| Error(format!("no decoder for {:?}", parameters.id())))?;
        let source = DecoderSource {
            parameters,
            codec,
            device: hardware.then(|| Device::create(&codec)).flatten(),
        };
        let decoder = source.open()?;
        let mut input = input;
        // Only the pictures are wanted here; the sound is mixed separately.
        unsafe {
            let context = input.as_mut_ptr();
            for other in 0..(*context).nb_streams as usize {
                if other != index {
                    (**(*context).streams.add(other)).discard = ffi::AVDiscard::AVDISCARD_ALL;
                }
            }
        }
        Ok(Self {
            input,
            decoder,
            stream: index,
            time_base,
            frame_time: ffi::AVRational {
                num: frame_rate.den,
                den: frame_rate.num,
            },
            start_pts,
            decoded: frame::Video::empty(),
            transfer: frame::Video::empty(),
            current: None,
            drained: false,
            scaler: ptr::null_mut(),
            scaler_for: None,
            drawn: None,
        })
    }

    /// Where the last picture drawn went in the film's frame: left, top, width and height, in
    /// pixels. None before the first.
    pub fn drawn_area(&self) -> Option<[u32; 4]> {
        self.drawn
    }

    fn frame_to_pts(&self, frame: u64) -> i64 {
        self.start_pts + unsafe { ffi::av_rescale_q(frame as i64, self.frame_time, self.time_base) }
    }

    /// Draws media frame `frame` into `picture`, scaled to fit with black around it. Frames
    /// must be asked for in order; asking for one again draws it again. False when the clip has
    /// no such frame (past its end); `picture` is then left as it was.
    pub fn draw(&mut self, frame: u64, picture: &mut FilmPicture) -> Result<bool, Error> {
        let target = self.frame_to_pts(frame);
        if self.current.is_none_or(|(start, _)| target < start) {
            self.seek(target)?;
        }
        // On to the frame showing at the target.
        while self.current.is_none_or(|(_, end)| end <= target) {
            if !self.advance()? {
                return Ok(false);
            }
        }
        self.convert(picture)?;
        Ok(true)
    }

    fn seek(&mut self, target: i64) -> Result<(), Error> {
        let run_up = unsafe { ffi::av_rescale_q(1, SEEK_RUN_UP, self.time_base) };
        let result = unsafe {
            ffi::av_seek_frame(
                self.input.as_mut_ptr(),
                self.stream as i32,
                target - run_up,
                ffi::AVSEEK_FLAG_BACKWARD,
            )
        };
        if result < 0 && target - run_up > self.start_pts {
            return Err(Error::new("seek", ffmpeg::Error::from(result)));
        }
        self.decoder.flush();
        self.current = None;
        self.drained = false;
        Ok(())
    }

    /// Decodes the next frame. False at the end of the clip.
    fn advance(&mut self) -> Result<bool, Error> {
        loop {
            match self.decoder.receive_frame(&mut self.decoded) {
                Ok(()) => {
                    let start = self
                        .decoded
                        .timestamp()
                        .or(self.decoded.pts())
                        .or(self.current.map(|(_, end)| end))
                        .unwrap_or(self.start_pts);
                    let length = match unsafe { (*self.decoded.as_ptr()).duration } {
                        length if length > 0 => length,
                        _ => self.frame_to_pts(1) - self.start_pts,
                    };
                    self.current = Some((start, start + length.max(1)));
                    return Ok(true);
                }
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => {}
                Err(ffmpeg::Error::Eof) => return Ok(false),
                Err(error) => return Err(Error::new("decode", error)),
            }
            if self.drained {
                return Ok(false);
            }
            let mut packet = Packet::empty();
            match packet.read(&mut self.input) {
                Ok(()) if packet.stream() == self.stream => {
                    match self.decoder.send_packet(&packet) {
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

    /// Scales the decoded frame into the middle of `picture`, as large as fits.
    fn convert(&mut self, picture: &mut FilmPicture) -> Result<(), Error> {
        let on_gpu =
            HARDWARE.is_some_and(|hardware| self.decoded.format() == hardware.pixel.into());
        if on_gpu {
            let result = unsafe {
                ffi::av_frame_unref(self.transfer.as_mut_ptr());
                ffi::av_hwframe_transfer_data(self.transfer.as_mut_ptr(), self.decoded.as_ptr(), 0)
            };
            if result < 0 {
                return Err(Error::new("copy frame from the GPU", ffmpeg::Error::from(result)));
            }
        }
        let colors = Colors::of(&self.decoded);
        let source = if on_gpu { &self.transfer } else { &self.decoded };
        let (width, height) = (source.width(), source.height());
        let target = &mut picture.frame;
        let (out_width, out_height) = (target.width(), target.height());
        let (fit_width, fit_height) = fit(width, height, out_width, out_height);
        let (left, top) = (
            (out_width - fit_width) / 2 & !1,
            (out_height - fit_height) / 2 & !1,
        );
        self.drawn = Some([left, top, fit_width, fit_height]);
        let source_format = unsafe { (*source.as_ptr()).format };
        let wanted = (source_format, width, height, fit_width, fit_height);
        unsafe {
            if self.scaler_for != Some(wanted) {
                ffi::sws_freeContext(self.scaler);
                self.scaler = ffi::sws_getContext(
                    width as i32,
                    height as i32,
                    std::mem::transmute::<i32, ffi::AVPixelFormat>(source_format),
                    fit_width as i32,
                    fit_height as i32,
                    target.format().into(),
                    (scaling::Flags::BICUBIC | scaling::Flags::ACCURATE_RND).bits(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null(),
                );
                if self.scaler.is_null() {
                    self.scaler_for = None;
                    return Err(Error("cannot scale the clip's pictures".to_string()));
                }
                self.scaler_for = Some(wanted);
                // The film is BT.709 in limited range, whatever the clip was.
                let matrix = match colors.matrix {
                    Matrix::Bt601 => ffi::SWS_CS_ITU601,
                    Matrix::Bt709 => ffi::SWS_CS_ITU709,
                    Matrix::Bt2020 => ffi::SWS_CS_BT2020,
                };
                ffi::sws_setColorspaceDetails(
                    self.scaler,
                    ffi::sws_getCoefficients(matrix),
                    i32::from(colors.full_range),
                    ffi::sws_getCoefficients(ffi::SWS_CS_ITU709),
                    0,
                    0,
                    1 << 16,
                    1 << 16,
                );
            }
            if (fit_width, fit_height) != (out_width, out_height) {
                picture.clear();
            }
            let target = &mut picture.frame;
            // Where the picture goes in each plane of the film's frame.
            let raw = target.as_mut_ptr();
            let mut destination = [ptr::null_mut::<u8>(); 4];
            let mut strides = [0i32; 4];
            for plane in 0..target.planes().min(4) {
                let stride = (*raw).linesize[plane];
                let (x, y) = match (plane, target.format()) {
                    (0, _) => (left, top),
                    // NV12 keeps both chroma samples side by side: two bytes a pixel pair.
                    (_, Pixel::NV12) => (left, top / 2),
                    _ => (left / 2, top / 2),
                };
                let offset = y as usize * stride as usize + x as usize;
                destination[plane] = (*raw).data[plane].add(offset);
                strides[plane] = stride;
            }
            let source = source.as_ptr();
            let scaled = ffi::sws_scale(
                self.scaler,
                (*source).data.as_ptr() as *const *const u8,
                (*source).linesize.as_ptr(),
                0,
                height as i32,
                destination.as_ptr(),
                strides.as_ptr(),
            );
            if scaled < 0 {
                return Err(Error::new("scale picture", ffmpeg::Error::from(scaled)));
            }
        }
        Ok(())
    }
}

impl Drop for ClipReader {
    fn drop(&mut self) {
        unsafe { ffi::sws_freeContext(self.scaler) };
    }
}

/// The largest even size with the source's shape that fits inside the film's.
fn fit(width: u32, height: u32, out_width: u32, out_height: u32) -> (u32, u32) {
    let (width, height) = (u64::from(width.max(1)), u64::from(height.max(1)));
    let (out_width, out_height) = (u64::from(out_width), u64::from(out_height));
    let (fit_width, fit_height) = if width * out_height > height * out_width {
        (out_width, height * out_width / width)
    } else {
        (width * out_height / height, out_height)
    };
    ((fit_width as u32 & !1).max(2), (fit_height as u32 & !1).max(2))
}

/// An MP4 file being written: H.264 pictures and AAC sound.
pub struct FilmWriter {
    output: format::context::Output,
    video: encoder::Video,
    video_stream: usize,
    video_time_base: ffmpeg::Rational,
    audio: encoder::Audio,
    audio_stream: usize,
    audio_time_base: ffmpeg::Rational,
    /// Sound waiting to fill the encoder's next frame, interleaved.
    audio_pending: Vec<f32>,
    audio_samples: usize,
    audio_written: i64,
    audio_frame: frame::Audio,
    picture: FilmPicture,
    /// The video encoder in use.
    pub encoder: &'static str,
}

impl FilmWriter {
    /// Starts writing `path`.
    pub fn create(path: &Path, film: FilmFormat) -> Result<Self, Error> {
        init();
        let mut output = format::output_as(path, "mp4")
            .map_err(|error| Error::new("create film file", error))?;
        let global_header = output
            .format()
            .flags()
            .contains(format::Flags::GLOBAL_HEADER);

        let (rate_num, rate_den) = (film.rate.0.max(1) as i32, film.rate.1.max(1) as i32);
        let video_time_base = ffmpeg::Rational::new(rate_den, rate_num);
        let mut tried = Vec::new();
        let mut opened = None;
        let forced = std::env::var(ENCODER_VARIABLE).ok();
        let names = VIDEO_ENCODERS
            .iter()
            .filter(|name| forced.as_deref().is_none_or(|forced| forced == **name));
        for name in names {
            let Some(codec) = encoder::find_by_name(name) else {
                continue;
            };
            match open_video(codec, film, video_time_base, global_header) {
                Ok((video, format)) => {
                    opened = Some((*name, codec, video, format));
                    break;
                }
                Err(error) => tried.push(format!("{name}: {error}")),
            }
        }
        let Some((name, codec, video, format)) = opened else {
            return Err(Error(format!("no video encoder works ({})", tried.join("; "))));
        };
        let video_stream = {
            let mut stream = output
                .add_stream(codec)
                .map_err(|error| Error::new("add video", error))?;
            stream.set_parameters(&video);
            stream.set_time_base(video_time_base);
            stream.index()
        };

        let audio_codec = encoder::find(codec::Id::AAC)
            .ok_or_else(|| Error("no AAC encoder".to_string()))?;
        let audio = open_audio(audio_codec, film.sample_rate, global_header)?;
        let audio_time_base = ffmpeg::Rational::new(1, film.sample_rate as i32);
        let audio_stream = {
            let mut stream = output
                .add_stream(audio_codec)
                .map_err(|error| Error::new("add sound", error))?;
            stream.set_parameters(&audio);
            stream.set_time_base(audio_time_base);
            stream.index()
        };
        let audio_samples = match audio.frame_size() {
            0 => 1024,
            size => size as usize,
        };
        let audio_frame = audio_frame(audio_samples, film.sample_rate)?;

        let mut options = Dictionary::new();
        // The index goes at the front, so the film plays as soon as it starts loading.
        options.set("movflags", "+faststart");
        output
            .write_header_with(options)
            .map_err(|error| Error::new("write film header", error))?;
        Ok(Self {
            output,
            video,
            video_stream,
            video_time_base,
            audio,
            audio_stream,
            audio_time_base,
            audio_pending: Vec::new(),
            audio_samples,
            audio_written: 0,
            audio_frame,
            picture: FilmPicture::new(format, film.width, film.height),
            encoder: name,
        })
    }

    /// The picture the next frame is drawn into.
    pub fn picture(&mut self) -> &mut FilmPicture {
        &mut self.picture
    }

    /// A picture of the film's size and format, to draw a layer in before it is laid over the
    /// film's own (see [`FilmPicture::lay_over`]).
    pub fn spare_picture(&self) -> FilmPicture {
        let frame = &self.picture.frame;
        FilmPicture::new(frame.format(), frame.width(), frame.height())
    }

    /// Encodes the picture as frame `index` of the film.
    pub fn write_picture(&mut self, index: i64) -> Result<(), Error> {
        self.picture.frame.set_pts(Some(index));
        self.video
            .send_frame(&self.picture.frame)
            .map_err(|error| Error::new("encode picture", error))?;
        self.write_video_packets()
    }

    /// Adds sound, interleaved with [`CHANNELS`] values per sample.
    pub fn write_sound(&mut self, samples: &[f32]) -> Result<(), Error> {
        self.audio_pending.extend_from_slice(samples);
        while self.audio_pending.len() >= self.audio_samples * CHANNELS {
            self.encode_sound_frame(self.audio_samples)?;
        }
        Ok(())
    }

    /// Writes what is still held back and closes the file.
    pub fn finish(mut self) -> Result<(), Error> {
        let left = self.audio_pending.len() / CHANNELS;
        if left > 0 {
            self.encode_sound_frame(left)?;
        }
        self.video
            .send_eof()
            .map_err(|error| Error::new("finish pictures", error))?;
        self.write_video_packets()?;
        self.audio
            .send_eof()
            .map_err(|error| Error::new("finish sound", error))?;
        self.write_audio_packets()?;
        self.output
            .write_trailer()
            .map_err(|error| Error::new("finish film file", error))
    }

    fn encode_sound_frame(&mut self, samples: usize) -> Result<(), Error> {
        unsafe {
            let frame = self.audio_frame.as_mut_ptr();
            (*frame).nb_samples = samples as i32;
            // The encoder wants each channel on its own.
            for channel in 0..CHANNELS {
                let plane = (*frame).extended_data.add(channel).read() as *mut f32;
                for sample in 0..samples {
                    *plane.add(sample) = self.audio_pending[sample * CHANNELS + channel];
                }
            }
            (*frame).pts = self.audio_written;
        }
        self.audio_pending.drain(..samples * CHANNELS);
        self.audio_written += samples as i64;
        self.audio
            .send_frame(&self.audio_frame)
            .map_err(|error| Error::new("encode sound", error))?;
        self.write_audio_packets()
    }

    fn write_video_packets(&mut self) -> Result<(), Error> {
        let mut packet = Packet::empty();
        while self.video.receive_packet(&mut packet).is_ok() {
            // Some encoders leave it out, and the last frame would then last no time at all:
            // the file would end before it.
            if packet.duration() <= 0 {
                packet.set_duration(1);
            }
            let stream = self.output.stream(self.video_stream).expect("video stream");
            packet.rescale_ts(self.video_time_base, stream.time_base());
            packet.set_stream(self.video_stream);
            packet
                .write_interleaved(&mut self.output)
                .map_err(|error| Error::new("write picture", error))?;
        }
        Ok(())
    }

    fn write_audio_packets(&mut self) -> Result<(), Error> {
        let mut packet = Packet::empty();
        while self.audio.receive_packet(&mut packet).is_ok() {
            let stream = self.output.stream(self.audio_stream).expect("sound stream");
            packet.rescale_ts(self.audio_time_base, stream.time_base());
            packet.set_stream(self.audio_stream);
            packet
                .write_interleaved(&mut self.output)
                .map_err(|error| Error::new("write sound", error))?;
        }
        Ok(())
    }
}

/// Opens a video encoder for the film, in NV12 when it takes that and planar YUV otherwise.
fn open_video(
    codec: ffmpeg::Codec,
    film: FilmFormat,
    time_base: ffmpeg::Rational,
    global_header: bool,
) -> Result<(encoder::Video, Pixel), Error> {
    let takes = |format: Pixel| {
        codec
            .video()
            .ok()
            .and_then(|video| video.formats())
            .is_none_or(|mut formats| formats.any(|offered| offered == format))
    };
    let format = if takes(Pixel::NV12) {
        Pixel::NV12
    } else {
        Pixel::YUV420P
    };
    let mut video = codec::Context::new_with_codec(codec)
        .encoder()
        .video()
        .map_err(|error| Error::new("set up video encoder", error))?;
    video.set_width(film.width);
    video.set_height(film.height);
    video.set_format(format);
    video.set_time_base(time_base);
    video.set_frame_rate(Some(ffmpeg::Rational::new(film.rate.0 as i32, film.rate.1 as i32)));
    let fps = f64::from(film.rate.0) / f64::from(film.rate.1.max(1));
    video.set_bit_rate(film.bit_rate);
    video.set_max_bit_rate(film.bit_rate * 3 / 2);
    // A keyframe every two seconds, so the film can be scrubbed.
    video.set_gop((fps * 2.0).round().max(1.0) as u32);
    if global_header {
        video.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    unsafe {
        let context = video.as_mut_ptr();
        (*context).color_primaries = ffi::AVColorPrimaries::AVCOL_PRI_BT709;
        (*context).color_trc = ffi::AVColorTransferCharacteristic::AVCOL_TRC_BT709;
        (*context).colorspace = ffi::AVColorSpace::AVCOL_SPC_BT709;
        (*context).color_range = ffi::AVColorRange::AVCOL_RANGE_MPEG;
    }
    let mut options = Dictionary::new();
    match codec.name() {
        "h264_amf" => {
            options.set("usage", "transcoding");
            options.set("quality", "quality");
            options.set("rc", "vbr_peak");
        }
        "h264_mf" => options.set("hw_encoding", "1"),
        _ => {}
    }
    let opened = video
        .open_as_with(codec, options)
        .map_err(|error| Error::new("open video encoder", error))?;
    Ok((opened, format))
}

fn open_audio(
    codec: ffmpeg::Codec,
    sample_rate: u32,
    global_header: bool,
) -> Result<encoder::Audio, Error> {
    let mut audio = codec::Context::new_with_codec(codec)
        .encoder()
        .audio()
        .map_err(|error| Error::new("set up sound encoder", error))?;
    audio.set_rate(sample_rate as i32);
    audio.set_format(format::Sample::F32(format::sample::Type::Planar));
    audio.set_bit_rate(AUDIO_BIT_RATE);
    audio.set_time_base(ffmpeg::Rational::new(1, sample_rate as i32));
    if global_header {
        audio.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    unsafe {
        let context = audio.as_mut_ptr();
        ffi::av_channel_layout_uninit(&mut (*context).ch_layout);
        ffi::av_channel_layout_default(&mut (*context).ch_layout, CHANNELS as i32);
    }
    audio
        .open_as(codec)
        .map_err(|error| Error::new("open sound encoder", error))
}

/// A frame of planar float stereo sound with room for `samples` samples.
fn audio_frame(samples: usize, sample_rate: u32) -> Result<frame::Audio, Error> {
    let mut frame = frame::Audio::empty();
    unsafe {
        let raw = frame.as_mut_ptr();
        (*raw).format = ffi::AVSampleFormat::AV_SAMPLE_FMT_FLTP as i32;
        (*raw).nb_samples = samples as i32;
        (*raw).sample_rate = sample_rate as i32;
        ffi::av_channel_layout_default(&mut (*raw).ch_layout, CHANNELS as i32);
        let result = ffi::av_frame_get_buffer(raw, 0);
        if result < 0 {
            return Err(Error::new("make sound frame", ffmpeg::Error::from(result)));
        }
    }
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn clips_fit_the_film_whole_and_centred_sizes_stay_even() {
        // The same shape fills it, larger or smaller.
        assert_eq!(fit(2560, 1440, 1280, 720), (1280, 720));
        assert_eq!(fit(640, 360, 1280, 720), (1280, 720));
        // Taller and wider shapes leave bars at the sides or above and below.
        assert_eq!(fit(1440, 1080, 1280, 720), (960, 720));
        assert_eq!(fit(1080, 1920, 1280, 720), (404, 720));
        assert_eq!(fit(2560, 1080, 1920, 1080), (1920, 810));
        assert_eq!(fit(1001, 999, 640, 480), (480, 480));
    }
}
