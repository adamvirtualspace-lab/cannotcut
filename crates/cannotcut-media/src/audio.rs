//! The sound of a media file in the form a mixer wants it: stereo 32-bit float samples at the
//! output's rate, from any point in the file.

use std::path::Path;
use std::ptr;

use ffmpeg::{Packet, codec, decoder, ffi, format, frame, media};
use ffmpeg_next as ffmpeg;

use crate::Error;
use crate::session::{init, raw_rational};

/// Samples are interleaved: left, right, left, right.
pub const CHANNELS: usize = 2;
/// Audio codecs build each frame partly from the one before, so after a seek the first frame
/// decoded is not right. Decoding starts this long before the sound that is wanted.
const PREROLL: ffi::AVRational = ffi::AVRational { num: 1, den: 10 };

/// Reads the sound of one media file from front to back, from wherever it was last sent.
pub struct AudioReader {
    input: format::context::Input,
    decoder: decoder::Audio,
    stream: usize,
    time_base: ffi::AVRational,
    /// Where the media's time starts, in the sound's time base: the timestamp of the first video
    /// frame, so that sound and pictures agree on what "frame 0" is.
    origin: i64,
    rate: i32,
    resampler: Option<Resampler>,
    decoded: frame::Audio,
    /// Sound decoded and resampled but not handed out yet, interleaved.
    pending: Vec<f32>,
    /// The output sample, counted from the origin, that `pending` begins with. None after a
    /// seek until the first frame has been decoded.
    pending_at: Option<i64>,
    /// The output sample handed out next.
    wanted: i64,
    /// The demuxer has run out of packets.
    drained: bool,
    /// The decoder has run out of frames: what is left is silence.
    ended: bool,
}

impl AudioReader {
    /// Opens the sound of `path`, to be read at `rate` samples a second. None when the file has
    /// no sound.
    pub fn open(path: &Path, rate: u32) -> Result<Option<Self>, Error> {
        init();
        let input = format::input(path).map_err(|error| Error::new("open clip", error))?;
        let Some(stream) = input.streams().best(media::Type::Audio) else {
            return Ok(None);
        };
        let stream_index = stream.index();
        let time_base = raw_rational(stream.time_base());
        let start = |stream: &ffmpeg::Stream| match stream.start_time() {
            ffi::AV_NOPTS_VALUE => 0,
            start => start,
        };
        let origin = match input.streams().best(media::Type::Video) {
            Some(video) => unsafe {
                ffi::av_rescale_q(start(&video), raw_rational(video.time_base()), time_base)
            },
            None => start(&stream),
        };
        let mut context = codec::Context::from_parameters(stream.parameters())
            .map_err(|error| Error::new("read audio parameters", error))?;
        // Lets the decoder keep timestamps right when it drops the codec's start-up samples.
        unsafe { (*context.as_mut_ptr()).pkt_timebase = time_base };
        let decoder = context
            .decoder()
            .audio()
            .map_err(|error| Error::new("open audio decoder", error))?;
        let mut reader = Self {
            input,
            decoder,
            stream: stream_index,
            time_base,
            origin,
            rate: rate as i32,
            resampler: None,
            decoded: frame::Audio::empty(),
            pending: Vec::new(),
            pending_at: None,
            wanted: 0,
            drained: false,
            ended: false,
        };
        reader.seek(0)?;
        Ok(Some(reader))
    }

    /// Goes to `sample` output samples after the media's first video frame. Before the sound
    /// starts, reading gives silence until it does.
    pub fn seek(&mut self, sample: i64) -> Result<(), Error> {
        let output = ffi::AVRational {
            num: 1,
            den: self.rate,
        };
        let target = self.origin + unsafe { ffi::av_rescale_q(sample, output, self.time_base) };
        let preroll = unsafe { ffi::av_rescale_q(1, PREROLL, self.time_base) };
        let result = unsafe {
            ffi::av_seek_frame(
                self.input.as_mut_ptr(),
                self.stream as i32,
                target - preroll,
                ffi::AVSEEK_FLAG_BACKWARD,
            )
        };
        // Seeking to before the first packet fails in some formats; reading from where the
        // file was opened is then right anyway.
        if result < 0 && sample > 0 {
            return Err(Error::new("seek in sound", ffmpeg::Error::from(result)));
        }
        self.decoder.flush();
        // The resampler holds samples from before the seek.
        self.resampler = None;
        self.pending.clear();
        self.pending_at = None;
        self.wanted = sample;
        self.drained = false;
        self.ended = false;
        Ok(())
    }

    /// Fills `out`, interleaved with [`CHANNELS`] values per sample, with the sound from where
    /// the last read or seek left off. Past the end of the sound it is silence.
    pub fn read(&mut self, out: &mut [f32]) -> Result<(), Error> {
        let mut out = out;
        while !out.is_empty() {
            let available = (self.pending.len() / CHANNELS) as i64;
            match self.pending_at {
                // Sound from before the wanted point, left by the seek's run-up, is dropped.
                Some(at) if at < self.wanted => {
                    let dropped = (self.wanted - at).min(available);
                    self.pending.drain(..dropped as usize * CHANNELS);
                    self.pending_at = Some(at + dropped);
                    if dropped == available {
                        self.decode_more()?;
                    }
                }
                // The sound starts later than asked: silence until it does.
                Some(at) if at > self.wanted || (available == 0 && self.ended) => {
                    let silent = if self.ended {
                        out.len() / CHANNELS
                    } else {
                        ((at - self.wanted) as usize).min(out.len() / CHANNELS)
                    };
                    out[..silent * CHANNELS].fill(0.0);
                    out = &mut out[silent * CHANNELS..];
                    self.wanted += silent as i64;
                }
                Some(at) if available > 0 => {
                    let taken = (available as usize).min(out.len() / CHANNELS);
                    out[..taken * CHANNELS].copy_from_slice(&self.pending[..taken * CHANNELS]);
                    self.pending.drain(..taken * CHANNELS);
                    out = &mut out[taken * CHANNELS..];
                    self.pending_at = Some(at + taken as i64);
                    self.wanted += taken as i64;
                }
                _ if self.ended => {
                    out.fill(0.0);
                    self.wanted += (out.len() / CHANNELS) as i64;
                    return Ok(());
                }
                _ => self.decode_more()?,
            }
        }
        Ok(())
    }

    /// Decodes the next frame and adds its samples to `pending`. Sets `ended` when there are no
    /// more.
    fn decode_more(&mut self) -> Result<(), Error> {
        loop {
            match self.decoder.receive_frame(&mut self.decoded) {
                Ok(()) => return self.take_frame(),
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => {}
                Err(ffmpeg::Error::Eof) => {
                    self.flush_resampler()?;
                    self.ended = true;
                    self.pending_at.get_or_insert(self.wanted);
                    return Ok(());
                }
                Err(error) => return Err(Error::new("decode sound", error)),
            }
            if self.drained {
                // Nothing more will come out.
                self.flush_resampler()?;
                self.ended = true;
                self.pending_at.get_or_insert(self.wanted);
                return Ok(());
            }
            let mut packet = Packet::empty();
            match packet.read(&mut self.input) {
                Ok(()) if packet.stream() == self.stream => {
                    match self.decoder.send_packet(&packet) {
                        // A damaged packet costs a moment of sound, not the whole clip.
                        Ok(()) | Err(ffmpeg::Error::InvalidData) => {}
                        Err(error) => return Err(Error::new("decode sound", error)),
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

    /// Resamples the frame just decoded onto the end of `pending`.
    fn take_frame(&mut self) -> Result<(), Error> {
        let frame = unsafe { self.decoded.as_ptr() };
        if self.pending_at.is_none() {
            // The first frame after a seek says where the sound is.
            let pts = self.decoded.timestamp().or(self.decoded.pts()).unwrap_or(self.origin);
            let output = ffi::AVRational {
                num: 1,
                den: self.rate,
            };
            let at = unsafe { ffi::av_rescale_q(pts - self.origin, self.time_base, output) };
            self.pending_at = Some(at - (self.pending.len() / CHANNELS) as i64);
        }
        let stale = self
            .resampler
            .as_ref()
            .is_none_or(|resampler| !resampler.takes(frame));
        if stale {
            self.flush_resampler()?;
            self.resampler = Some(Resampler::new(frame, self.rate)?);
        }
        let resampler = self.resampler.as_mut().expect("resampler was just made");
        unsafe {
            resampler.convert(
                (*frame).extended_data as *const *const u8,
                (*frame).nb_samples,
                &mut self.pending,
            )
        }
    }

    /// Takes what the resampler still holds.
    fn flush_resampler(&mut self) -> Result<(), Error> {
        match &mut self.resampler {
            Some(resampler) => unsafe { resampler.convert(ptr::null(), 0, &mut self.pending) },
            None => Ok(()),
        }
    }
}

/// Turns decoded frames of any layout, format and rate into interleaved stereo float samples
/// at the output rate.
struct Resampler {
    context: *mut ffi::SwrContext,
    format: i32,
    rate: i32,
    channels: i32,
}

// The context is only ever used by the reader that owns it.
unsafe impl Send for Resampler {}

impl Resampler {
    fn new(frame: *const ffi::AVFrame, rate: i32) -> Result<Self, Error> {
        unsafe {
            let mut input_layout = std::mem::zeroed::<ffi::AVChannelLayout>();
            if (*frame).ch_layout.order == ffi::AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC {
                // Only the count is known: take the usual layout for it.
                ffi::av_channel_layout_default(&mut input_layout, (*frame).ch_layout.nb_channels);
            } else {
                ffi::av_channel_layout_copy(&mut input_layout, &(*frame).ch_layout);
            }
            let mut output_layout = std::mem::zeroed::<ffi::AVChannelLayout>();
            ffi::av_channel_layout_default(&mut output_layout, CHANNELS as i32);
            let mut context = ptr::null_mut();
            let mut result = ffi::swr_alloc_set_opts2(
                &mut context,
                &output_layout,
                ffi::AVSampleFormat::AV_SAMPLE_FMT_FLT,
                rate,
                &input_layout,
                std::mem::transmute::<i32, ffi::AVSampleFormat>((*frame).format),
                (*frame).sample_rate,
                0,
                ptr::null_mut(),
            );
            if result >= 0 {
                result = ffi::swr_init(context);
            }
            ffi::av_channel_layout_uninit(&mut input_layout);
            ffi::av_channel_layout_uninit(&mut output_layout);
            if result < 0 {
                ffi::swr_free(&mut context);
                return Err(Error::new("set up resampling", ffmpeg::Error::from(result)));
            }
            Ok(Self {
                context,
                format: (*frame).format,
                rate: (*frame).sample_rate,
                channels: (*frame).ch_layout.nb_channels,
            })
        }
    }

    /// Whether frames like this one can go through as they are.
    fn takes(&self, frame: *const ffi::AVFrame) -> bool {
        unsafe {
            (*frame).format == self.format
                && (*frame).sample_rate == self.rate
                && (*frame).ch_layout.nb_channels == self.channels
        }
    }

    /// Resamples `count` samples from `input` (null to empty the resampler) onto `out`.
    unsafe fn convert(
        &mut self,
        input: *const *const u8,
        count: i32,
        out: &mut Vec<f32>,
    ) -> Result<(), Error> {
        unsafe {
            let room = ffi::swr_get_out_samples(self.context, count).max(0) as usize;
            let start = out.len();
            out.resize(start + room * CHANNELS, 0.0);
            let mut destination = out[start..].as_mut_ptr() as *mut u8;
            let made = ffi::swr_convert(self.context, &mut destination, room as i32, input, count);
            if made < 0 {
                out.truncate(start);
                return Err(Error::new("resample sound", ffmpeg::Error::from(made)));
            }
            out.truncate(start + made as usize * CHANNELS);
            Ok(())
        }
    }
}

impl Drop for Resampler {
    fn drop(&mut self) {
        unsafe { ffi::swr_free(&mut self.context) };
    }
}
