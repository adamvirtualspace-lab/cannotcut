//! What it takes to line sound up with pictures on a given output: a tone that beeps at the
//! start of every second, to watch against a flash, and an ear on the microphone that notes
//! when each beep is really heard, to measure how much later than the sound card says that is.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cannotcut_media::CHANNELS;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{InputCallbackInfo, SampleFormat, StreamConfig};

/// The beep: short, so its start is sharp, and high enough to stand out from room noise.
const BEEP_SECONDS: f64 = 0.03;
const BEEP_HZ: f64 = 1000.0;
const BEEP_LOUDNESS: f32 = 0.5;
/// Rises and falls this fast, so the beep does not click.
const RAMP_SECONDS: f64 = 0.002;
/// After a beep is heard, the next one is listened for only this much later: echoes and the
/// rest of the beep are not new beeps.
const HOLD_OFF: Duration = Duration::from_millis(500);
/// A beep is this many times louder than the room, and at least `LEAST_LEVEL`.
const ABOVE_ROOM: f32 = 6.0;
const LEAST_LEVEL: f32 = 0.02;
/// How fast the room's level is followed while there is no beep.
const ROOM_FOLLOW: f32 = 0.001;

/// Beeps at the start of every second, from sample 0.
pub(crate) struct Tone {
    rate: u32,
    /// Stereo frames made so far.
    position: i64,
}

impl Tone {
    pub(crate) fn new(rate: u32) -> Self {
        Self { rate, position: 0 }
    }

    #[cfg(test)]
    fn position(&self) -> i64 {
        self.position
    }

    /// Fills `out` with stereo frames; how many.
    pub(crate) fn mix(&mut self, out: &mut [f32]) -> usize {
        let rate = f64::from(self.rate);
        let frames = out.len() / CHANNELS;
        for frame in out.chunks_exact_mut(CHANNELS) {
            let into_second = (self.position % i64::from(self.rate)) as f64 / rate;
            let value = if into_second < BEEP_SECONDS {
                let ramp = (into_second / RAMP_SECONDS)
                    .min((BEEP_SECONDS - into_second) / RAMP_SECONDS)
                    .min(1.0);
                let wave = (into_second * BEEP_HZ * std::f64::consts::TAU).sin();
                (wave * ramp) as f32 * BEEP_LOUDNESS
            } else {
                0.0
            };
            frame.fill(value);
            self.position += 1;
        }
        frames
    }
}

/// Listens on the system's default microphone and notes when each beep is heard.
pub struct Listener {
    _stream: cpal::Stream,
    heard: Arc<Mutex<Vec<Instant>>>,
    device: String,
}

impl Listener {
    /// Starts listening.
    pub fn start() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "no microphone".to_string())?;
        let name = device
            .description()
            .map(|description| description.name().to_string())
            .unwrap_or_else(|_| "microphone".to_string());
        let supported = device
            .default_input_config()
            .map_err(|error| error.to_string())?;
        if supported.sample_format() != SampleFormat::F32 {
            return Err(format!("{name} gives {} samples", supported.sample_format()));
        }
        let config: StreamConfig = supported.into();
        let channels = usize::from(config.channels);
        let rate = f64::from(config.sample_rate);
        let heard = Arc::new(Mutex::new(Vec::new()));
        let noted = heard.clone();
        let mut room = LEAST_LEVEL / ABOVE_ROOM;
        let mut last: Option<Instant> = None;
        let stream = device
            .build_input_stream::<f32, _, _>(
                config,
                move |data: &[f32], info: &InputCallbackInfo| {
                    let timestamp = info.timestamp();
                    let waited = timestamp.callback.duration_since(timestamp.capture);
                    // When the first of these samples reached the microphone.
                    let captured = Instant::now().checked_sub(waited).unwrap_or_else(Instant::now);
                    for (index, frame) in data.chunks_exact(channels).enumerate() {
                        let level = frame[0].abs();
                        let at = captured + Duration::from_secs_f64(index as f64 / rate);
                        let held_off = last.is_some_and(|last| at.duration_since(last) < HOLD_OFF);
                        if !held_off && level > (room * ABOVE_ROOM).max(LEAST_LEVEL) {
                            last = Some(at);
                            if let Ok(mut heard) = noted.lock() {
                                heard.push(at);
                            }
                        } else if !held_off {
                            room += (level - room) * ROOM_FOLLOW;
                        }
                    }
                },
                |_| {},
                None,
            )
            .map_err(|error| error.to_string())?;
        stream.play().map_err(|error| error.to_string())?;
        Ok(Self {
            _stream: stream,
            heard,
            device: name,
        })
    }

    /// The microphone's name.
    pub fn device(&self) -> &str {
        &self.device
    }

    /// When each beep was heard, so far.
    pub fn heard(&self) -> Vec<Instant> {
        self.heard.lock().map(|heard| heard.clone()).unwrap_or_default()
    }
}

/// How much later than the sound card reported the beeps were heard: the middle of the
/// differences between each beep heard and the report of when the beep before it would be,
/// `reported(k)` being that for beep `k`. None with fewer than three beeps heard.
pub fn measure(heard: &[Instant], reported: impl Fn(u64) -> Option<Instant>) -> Option<Duration> {
    let first = reported(0)?;
    let mut lags: Vec<Duration> = heard
        .iter()
        .filter_map(|at| {
            let beep = at.checked_duration_since(first)?.as_secs();
            let lag = at.checked_duration_since(reported(beep)?)?;
            // A beep is heard well within the second it starts.
            (lag < Duration::from_millis(900)).then_some(lag)
        })
        .collect();
    if lags.len() < 3 {
        return None;
    }
    lags.sort();
    Some(lags[lags.len() / 2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tone_beeps_at_the_start_of_every_second() {
        let rate = 48_000;
        let mut tone = Tone::new(rate as u32);
        let mut out = vec![0.0; 2 * rate * CHANNELS];
        assert_eq!(tone.mix(&mut out), 2 * rate);
        let loud = |from: f64, to: f64| {
            let frame = |seconds: f64| (seconds * rate as f64) as usize * CHANNELS;
            out[frame(from)..frame(to)].iter().any(|value| value.abs() > 0.1)
        };
        assert!(loud(0.0, 0.03) && loud(1.0, 1.03));
        assert!(!loud(0.04, 1.0) && !loud(1.04, 2.0));
        assert_eq!(tone.position(), 2 * rate as i64);
    }

    #[test]
    fn the_lag_is_the_middle_one_of_the_beeps_heard() {
        let start = Instant::now();
        let at = |seconds: f64| start + Duration::from_secs_f64(seconds);
        let reported = |beep: u64| Some(at(beep as f64));
        let heard = [at(0.120), at(1.118), at(2.125), at(3.121)];
        let lag = measure(&heard, reported).unwrap();
        assert_eq!(lag.as_millis(), 121);
        assert!(measure(&heard[..2], reported).is_none());
    }
}
