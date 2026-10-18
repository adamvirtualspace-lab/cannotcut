//! Sound for cannotcut: the clips of a timeline mixed and played through the sound card.
//!
//! The sound card also keeps time for playback. It plays samples at its own steady rate, so
//! the playhead goes wherever the sound being heard is, and pictures follow it; sound and
//! pictures cannot drift apart.

mod mixer;

pub use mixer::{Mixer, frame_to_sample};

use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cannotcut_media::CHANNELS;
use cannotcut_timeline::Timeline;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, OutputCallbackInfo, SampleFormat, SizedSample, StreamConfig};

/// Sound is mixed this far ahead of the sound card. A clip's file is opened when the mix
/// reaches it, which is this long before it is heard, so opening files never interrupts the
/// sound.
const BUFFER_SECONDS: f64 = 1.0;
/// Sound mixed before playing starts, so that the first moments do not run dry.
const PREFILL_SECONDS: f64 = 0.1;
/// Samples mixed at a time.
const CHUNK: usize = 1024;
/// How long the mixer rests when the sound card has all it can take.
const IDLE: Duration = Duration::from_millis(5);
/// Messages to the sound card's callback that it has not picked up yet.
const HANDOFFS: usize = 8;
/// When what is playing is swapped, how long the new mix takes from being handed over to
/// reaching the sound card, on top of the card's own delay: about half a callback's wait.
const HANDOVER: Duration = Duration::from_millis(5);

/// The state of the sound output.
#[derive(Clone, Debug)]
pub enum Status {
    /// The sound card is being opened.
    Starting,
    Ready {
        device: String,
        /// Samples a second.
        rate: u32,
    },
    /// No sound can be played, and why.
    Unavailable(String),
}

enum Command {
    Play {
        id: u64,
        timeline: Timeline,
        from: Start,
        folder: Option<PathBuf>,
        /// The play this one takes over from, and the output's extra delay: it is to start
        /// where that play will have got to by the time it is heard.
        follows: Option<(u64, Duration)>,
    },
    Stop,
}

/// Where on the timeline a play begins.
enum Start {
    Frame(i64),
    /// At the sound card's rate.
    Sample(i64),
}

/// What the mixer hands the sound card's callback.
enum Handoff {
    Play {
        id: u64,
        start: i64,
        samples: rtrb::Consumer<f32>,
    },
    Stop,
}

/// Where the sound card had got to when its callback last ran.
#[derive(Clone, Copy)]
struct Anchor {
    play: u64,
    rate: u32,
    /// The timeline sample the play began on.
    start: i64,
    /// Samples of the play handed to the sound card before the latest callback's.
    before: u64,
    /// Samples of the play the latest callback handed over. Silence it filled up with when the
    /// mix fell behind does not count: the playhead waits for the sound.
    delivered: u64,
    /// When the first of those is heard.
    heard_at: Instant,
    /// When the callback ran.
    noted_at: Instant,
}

impl Anchor {
    /// Where on the timeline, in seconds, the sound of this play heard at `now` is, when the
    /// output is `delay` later than the card reports. None before any of it was handed over.
    fn position(&self, now: Instant, delay: Duration) -> Option<f64> {
        let handed = self.before + self.delivered;
        if handed == 0 {
            return None;
        }
        let rate = f64::from(self.rate);
        let heard_at = self.heard_at + delay;
        let since = match now.checked_duration_since(heard_at) {
            Some(after) => after.as_secs_f64(),
            None => -heard_at.duration_since(now).as_secs_f64(),
        };
        let played = (self.before as f64 + since * rate).clamp(0.0, handed as f64);
        Some((self.start as f64 + played) / rate)
    }

    /// When the play's first sample is heard.
    fn first_heard_at(&self, delay: Duration) -> Instant {
        let before = Duration::from_secs_f64(self.before as f64 / f64::from(self.rate));
        (self.heard_at + delay).checked_sub(before).unwrap_or(self.heard_at)
    }

    /// How long sound takes from the callback to being heard.
    fn lead(&self, delay: Duration) -> Duration {
        self.heard_at.saturating_duration_since(self.noted_at) + delay
    }
}

/// The latest anchor, and the one of the play before it, which is still being heard for a
/// moment after the next one was handed over.
#[derive(Clone, Copy, Default)]
struct Anchors {
    latest: Option<Anchor>,
    previous: Option<Anchor>,
}

#[derive(Default)]
struct Shared {
    status: Mutex<Option<Status>>,
    anchors: Mutex<Anchors>,
    problem: Mutex<Option<String>>,
}

/// Plays the sound of a timeline, and tells where in the timeline the sound being heard is.
pub struct Player {
    commands: mpsc::Sender<Command>,
    shared: Arc<Shared>,
    plays: u64,
    /// The play under way.
    playing: Option<u64>,
    /// The play that [`replace`](Self::replace) took over from; its sound is heard until the
    /// new play's is.
    replaced: Option<u64>,
    /// How much later sound is heard than the sound card reports, for outputs whose
    /// processing it does not know about.
    delay: Duration,
}

impl Player {
    /// Opens the default sound output on a thread of its own and returns at once;
    /// [`status`](Self::status) says how it went.
    pub fn start() -> Self {
        let (commands, received) = mpsc::channel();
        let shared = Arc::new(Shared::default());
        let started = thread::Builder::new().name("cannotcut-sound".to_string()).spawn({
            let shared = shared.clone();
            move || run(received, shared)
        });
        if let Err(error) = started {
            *lock(&shared.status) = Some(Status::Unavailable(error.to_string()));
        }
        Self {
            commands,
            shared,
            plays: 0,
            playing: None,
            replaced: None,
            delay: Duration::ZERO,
        }
    }

    /// Sound is heard `delay` later than the sound card says; the position follows suit.
    pub fn set_delay(&mut self, delay: Duration) {
        self.delay = delay;
    }

    pub fn status(&self) -> Status {
        lock(&self.shared.status).clone().unwrap_or(Status::Starting)
    }

    /// Starts playing `timeline` from frame `from`, instead of whatever was playing.
    pub fn play(&mut self, timeline: Timeline, from: i64, folder: Option<PathBuf>) {
        self.replaced = None;
        self.send_play(timeline, Start::Frame(from), folder, None);
    }

    /// Plays `timeline` in place of what is playing, from the point the sound will have got
    /// to when the new mix reaches the sound card. The sound changes without a pause and the
    /// position runs on. False when no sound is being heard yet; [`play`](Self::play) then.
    pub fn replace(&mut self, timeline: Timeline, folder: Option<PathBuf>) -> bool {
        let now = Instant::now();
        let anchors = *lock(&self.shared.anchors);
        let Some(anchor) = anchors.latest.filter(|anchor| Some(anchor.play) == self.playing)
        else {
            return false;
        };
        let Some(position) = self.position(now) else {
            return false;
        };
        // A first guess; the sound thread makes it exact when the new mix is ready.
        let start = (position + anchor.lead(self.delay).as_secs_f64()) * f64::from(anchor.rate);
        self.replaced = self.playing;
        let follows = Some((anchor.play, self.delay));
        self.send_play(timeline, Start::Sample(start.round() as i64), folder, follows);
        true
    }

    fn send_play(
        &mut self,
        timeline: Timeline,
        from: Start,
        folder: Option<PathBuf>,
        follows: Option<(u64, Duration)>,
    ) {
        self.plays += 1;
        self.playing = Some(self.plays);
        let _ = self.commands.send(Command::Play {
            id: self.plays,
            timeline,
            from,
            folder,
            follows,
        });
    }

    pub fn stop(&mut self) {
        self.replaced = None;
        if self.playing.take().is_some() {
            let _ = self.commands.send(Command::Stop);
        }
    }

    /// Where on the timeline, in seconds, the sound heard at `now` is. None until the sound of
    /// the play has started.
    pub fn position(&self, now: Instant) -> Option<f64> {
        let anchors = *lock(&self.shared.anchors);
        let of = |play: Option<u64>| {
            [anchors.latest, anchors.previous]
                .into_iter()
                .flatten()
                .find(|anchor| play.is_some() && Some(anchor.play) == play)
        };
        // After a swap, the sound played before goes on being heard until the new one is.
        let latest = of(self.playing);
        let heard = |anchor: &Anchor| now >= anchor.first_heard_at(self.delay);
        let anchor = latest
            .filter(heard)
            .or_else(|| of(self.replaced))
            .or(latest)?;
        anchor.position(now, self.delay)
    }

    /// What went wrong most recently, if anything: a clip whose sound could not be read, or
    /// trouble with the sound card.
    pub fn problem(&self) -> Option<String> {
        lock(&self.shared.problem).clone()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // The sound thread ends when it finds the other end of its commands gone.
        self.stop();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The sound card, opened, and the way to hand it sound.
struct Output {
    stream: cpal::Stream,
    handoffs: rtrb::Producer<Handoff>,
    rate: u32,
}

impl Output {
    fn open(shared: &Arc<Shared>) -> Result<(Self, String), String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "no sound output".to_string())?;
        let name = device
            .description()
            .map(|description| description.name().to_string())
            .unwrap_or_else(|_| "sound output".to_string());
        let supported = device
            .default_output_config()
            .map_err(|error| error.to_string())?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let (handoffs, received) = rtrb::RingBuffer::new(HANDOFFS);
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, &config, received, shared),
            SampleFormat::I16 => build::<i16>(&device, &config, received, shared),
            SampleFormat::I32 => build::<i32>(&device, &config, received, shared),
            SampleFormat::U16 => build::<u16>(&device, &config, received, shared),
            SampleFormat::F64 => build::<f64>(&device, &config, received, shared),
            other => return Err(format!("{name} wants {other} samples, which are not supported")),
        }
        .map_err(|error| error.to_string())?;
        // Some systems start a stream as soon as it is made; it waits for something to play.
        let _ = stream.pause();
        let rate = config.sample_rate;
        Ok((
            Self {
                stream,
                handoffs,
                rate,
            },
            name,
        ))
    }

    fn hand(&mut self, handoff: Handoff) {
        // The callback picks these up every few milliseconds, so the queue is never full in
        // practice; if it were, the play would simply not start.
        let _ = self.handoffs.push(handoff);
    }
}

/// The sound card's callback: it takes mixed sound as fast as the card plays it, fills up with
/// silence when there is none, and notes where it has got to for [`Player::position`].
fn build<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut handoffs: rtrb::Consumer<Handoff>,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    struct Playing {
        id: u64,
        start: i64,
        samples: rtrb::Consumer<f32>,
        handed: u64,
    }
    let channels = usize::from(config.channels);
    let rate = config.sample_rate;
    let mut playing: Option<Playing> = None;
    let callback_shared = shared.clone();
    let error_shared = shared.clone();
    device.build_output_stream::<T, _, _>(
        config.clone(),
        move |data: &mut [T], info: &OutputCallbackInfo| {
            // Dropping a play's samples frees memory here, which a sound callback should not
            // do; it happens only when a play is replaced, and takes microseconds.
            while let Ok(handoff) = handoffs.pop() {
                playing = match handoff {
                    Handoff::Play { id, start, samples } => Some(Playing {
                        id,
                        start,
                        samples,
                        handed: 0,
                    }),
                    Handoff::Stop => None,
                };
            }
            let frames = data.len() / channels;
            let Some(play) = &mut playing else {
                data.fill(T::EQUILIBRIUM);
                return;
            };
            let count = frames.min(play.samples.slots() / CHANNELS);
            let mut outputs = data.chunks_exact_mut(channels);
            if let Ok(chunk) = play.samples.read_chunk(count * CHANNELS) {
                let (first, second) = chunk.as_slices();
                let mut samples = first.iter().chain(second).copied();
                for output in outputs.by_ref().take(count) {
                    let left = samples.next().unwrap_or(0.0);
                    let right = samples.next().unwrap_or(0.0);
                    if channels == 1 {
                        output[0] = T::from_sample((left + right) * 0.5);
                    } else {
                        output[0] = T::from_sample(left);
                        output[1] = T::from_sample(right);
                        output[2..].fill(T::EQUILIBRIUM);
                    }
                }
                chunk.commit_all();
            }
            for output in outputs {
                output.fill(T::EQUILIBRIUM);
            }
            let timestamp = info.timestamp();
            let delay = timestamp
                .playback
                .checked_duration_since(timestamp.callback)
                .unwrap_or_default();
            // Never waits: if the UI happens to be reading it, this callback's news is skipped
            // and the next one's taken.
            if let Ok(mut anchors) = callback_shared.anchors.try_lock() {
                let noted_at = Instant::now();
                let anchor = Anchor {
                    play: play.id,
                    rate,
                    start: play.start,
                    before: play.handed,
                    delivered: count as u64,
                    heard_at: noted_at + delay,
                    noted_at,
                };
                if anchors.latest.is_some_and(|latest| latest.play != play.id) {
                    anchors.previous = anchors.latest;
                }
                anchors.latest = Some(anchor);
            }
            play.handed += count as u64;
        },
        move |error: cpal::Error| {
            // A moment the card ran dry is over by the time it is reported; the sound goes on.
            if error.kind() != cpal::ErrorKind::Xrun {
                *lock(&error_shared.problem) = Some(format!("sound output: {error}"));
            }
        },
        None,
    )
}

/// The sound thread: opens the sound card, then mixes whatever is playing ahead of it.
fn run(commands: mpsc::Receiver<Command>, shared: Arc<Shared>) {
    let mut output = match Output::open(&shared) {
        Ok((output, device)) => {
            *lock(&shared.status) = Some(Status::Ready {
                device,
                rate: output.rate,
            });
            output
        }
        Err(error) => {
            *lock(&shared.status) = Some(Status::Unavailable(error));
            return;
        }
    };
    let capacity = (BUFFER_SECONDS * f64::from(output.rate)) as usize * CHANNELS;
    let prefill = (PREFILL_SECONDS * f64::from(output.rate)) as usize * CHANNELS;
    let mut chunk = vec![0.0; CHUNK * CHANNELS];
    let mut mixing: Option<(Mixer, rtrb::Producer<f32>)> = None;
    loop {
        let command = match &mixing {
            // There is room for more sound: look for news without waiting.
            Some((mixer, samples)) if !mixer.finished() && samples.slots() >= chunk.len() => {
                match commands.try_recv() {
                    Ok(command) => Some(command),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            // Mixed as far ahead as the buffer goes: wait for the card to take some.
            Some((mixer, _)) if !mixer.finished() => match commands.recv_timeout(IDLE) {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            },
            // Nothing to mix: wait for something to do.
            _ => match commands.recv() {
                Ok(command) => Some(command),
                Err(_) => return,
            },
        };
        match command {
            Some(Command::Play {
                id,
                timeline,
                from,
                folder,
                follows,
            }) => {
                // What went wrong in an earlier play is old news.
                *lock(&shared.problem) = None;
                let mut mixer = match from {
                    Start::Frame(frame) => Mixer::new(timeline, frame, output.rate, folder),
                    Start::Sample(sample) => {
                        Mixer::from_sample(timeline, sample, output.rate, folder)
                    }
                };
                let mut start = mixer.position();
                let (mut samples, mut consumer) = rtrb::RingBuffer::new(capacity);
                let mut fill = |mixer: &mut Mixer,
                                samples: &mut rtrb::Producer<f32>,
                                wanted: usize| {
                    while consumer_len(samples, capacity) < wanted.min(capacity - chunk.len())
                        && !mixer.finished()
                    {
                        let count = mixer.mix(&mut chunk);
                        let _ = samples.push_entire_slice(&chunk[..count * CHANNELS]);
                    }
                };
                fill(&mut mixer, &mut samples, prefill);
                // Taking over from another play: the new sound is to follow on from where that
                // one will have got to when it is heard. Opening the clips' files took a while
                // just now, so what was mixed too early for that is dropped.
                if let Some((before, delay)) = follows {
                    let latest = lock(&shared.anchors).latest;
                    let anchor = latest.filter(|anchor| anchor.play == before);
                    let now = Instant::now();
                    if let Some((anchor, position)) =
                        anchor.and_then(|anchor| Some((anchor, anchor.position(now, delay)?)))
                    {
                        let lead = (anchor.lead(delay) + HANDOVER).as_secs_f64();
                        let wanted = ((position + lead) * f64::from(output.rate)).round() as i64;
                        let behind = (wanted - start).max(0) as usize * CHANNELS;
                        fill(&mut mixer, &mut samples, behind + prefill);
                        let dropped = behind.min(consumer.slots());
                        if let Ok(early) = consumer.read_chunk(dropped) {
                            early.commit_all();
                        }
                        start += (dropped / CHANNELS) as i64;
                    }
                }
                note_problem(&shared, &mut mixer);
                output.hand(Handoff::Play {
                    id,
                    start,
                    samples: consumer,
                });
                if let Err(error) = output.stream.play() {
                    *lock(&shared.problem) = Some(format!("sound output: {error}"));
                }
                mixing = Some((mixer, samples));
            }
            Some(Command::Stop) => {
                mixing = None;
                output.hand(Handoff::Stop);
                let _ = output.stream.pause();
            }
            None => {
                if let Some((mixer, samples)) = &mut mixing
                    && samples.slots() >= chunk.len()
                {
                    let count = mixer.mix(&mut chunk);
                    let _ = samples.push_entire_slice(&chunk[..count * CHANNELS]);
                    note_problem(&shared, mixer);
                }
            }
        }
    }
}

/// How many values are waiting in the ring that `samples` fills.
fn consumer_len(samples: &rtrb::Producer<f32>, capacity: usize) -> usize {
    capacity - samples.slots()
}

fn note_problem(shared: &Shared, mixer: &mut Mixer) {
    if let Some(problem) = mixer.problem.take() {
        *lock(&shared.problem) = Some(problem);
    }
}
