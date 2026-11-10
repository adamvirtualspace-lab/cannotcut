use std::path::PathBuf;

use cannotcut_media::{AudioReader, CHANNELS};
pub use cannotcut_timeline::frame_to_sample;
use cannotcut_timeline::{ClipId, Sound, Timeline};

/// Mixes the sound of every clip on a timeline, from a starting frame onwards, in order. Clips
/// that play other projects are heard through, down to the media files inside them.
pub struct Mixer {
    /// Everything there is to hear, worked out once: the timeline does not change under a mix.
    sounds: Vec<Sound>,
    /// Where the project file is; relative media paths start from here.
    folder: Option<PathBuf>,
    rate: u32,
    /// The timeline sample mixed next.
    position: i64,
    end: i64,
    voices: Vec<Voice>,
    /// Sounds whose media has no sound, or sound that could not be read, by key. They are not
    /// tried again.
    silent: Vec<Vec<ClipId>>,
    scratch: Vec<f32>,
    /// What went wrong reading a clip's sound, most recently.
    pub problem: Option<String>,
}

/// A sound being heard: its media file, read from front to back.
struct Voice {
    /// The key of the sound it plays.
    sound: Vec<ClipId>,
    /// The timeline sample after the clip's last.
    end: i64,
    reader: AudioReader,
    /// For a sound played faster or slower: media samples read but not yet passed, and how
    /// far past the first of them the next one heard falls.
    held: Vec<f32>,
    phase: f64,
}

impl Mixer {
    /// A mix of `timeline` from frame `from`, at `rate` samples a second.
    pub fn new(timeline: Timeline, from: i64, rate: u32, folder: Option<PathBuf>) -> Self {
        let from = frame_to_sample(from, timeline.rate(), rate);
        Self::from_sample(timeline, from, rate, folder)
    }

    /// A mix of `timeline` from sample `from`, at `rate` samples a second.
    pub fn from_sample(timeline: Timeline, from: i64, rate: u32, folder: Option<PathBuf>) -> Self {
        Self {
            position: from.max(0),
            end: frame_to_sample(timeline.duration(), timeline.rate(), rate),
            sounds: timeline.sounds(rate),
            folder,
            rate,
            voices: Vec::new(),
            silent: Vec::new(),
            scratch: Vec::new(),
            problem: None,
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// The timeline sample mixed next.
    pub fn position(&self) -> i64 {
        self.position
    }

    pub fn finished(&self) -> bool {
        self.position >= self.end
    }

    /// Mixes the next stretch of sound into `out`, interleaved with [`CHANNELS`] values per
    /// sample. Returns how many samples it made: as many as fit, fewer only where the timeline
    /// ends.
    pub fn mix(&mut self, out: &mut [f32]) -> usize {
        let count = ((out.len() / CHANNELS) as i64).min(self.end - self.position).max(0);
        let (from, to) = (self.position, self.position + count);
        let out = &mut out[..count as usize * CHANNELS];
        out.fill(0.0);

        let heard: Vec<usize> = (0..self.sounds.len())
            .filter(|index| {
                let sound = &self.sounds[*index];
                sound.begins < to && sound.ends > from && !self.silent.contains(&sound.key)
            })
            .collect();
        for index in heard {
            let sound = &self.sounds[index];
            let (first, last) = (sound.begins.max(from), sound.ends.min(to));
            let voice = match self.voices.iter().position(|voice| voice.sound == sound.key) {
                Some(voice) => voice,
                None => {
                    // A sound is joined where it is first heard: at its start, or wherever
                    // the mix began if that was part-way into it.
                    let skipped = ((first - sound.begins) as f64 * sound.speed).round() as i64;
                    let media_sample = sound.media_sample + skipped;
                    match self.open(sound, media_sample) {
                        Ok(Some(reader)) => {
                            self.voices.push(Voice {
                                sound: sound.key.clone(),
                                end: sound.ends,
                                reader,
                                held: Vec::new(),
                                phase: 0.0,
                            });
                            self.voices.len() - 1
                        }
                        Ok(None) => {
                            self.silent.push(sound.key.clone());
                            continue;
                        }
                        Err(problem) => {
                            self.problem = Some(format!("{}: {problem}", sound.name));
                            self.silent.push(sound.key.clone());
                            continue;
                        }
                    }
                }
            };
            let offset = (first - from) as usize * CHANNELS;
            let gain = sound.gain;
            if sound.speed != 1.0 {
                let heard = (last - first) as usize;
                let playing = &mut self.voices[voice];
                if let Err(problem) = resampled(playing, sound.speed, heard, |at, value| {
                    out[offset + at] += value * gain;
                }) {
                    self.problem = Some(format!("{}: {problem}", sound.name));
                    self.silent.push(sound.key.clone());
                    self.voices.remove(voice);
                }
                continue;
            }
            let samples = (last - first) as usize * CHANNELS;
            self.scratch.resize(samples, 0.0);
            if let Err(problem) = self.voices[voice].reader.read(&mut self.scratch) {
                self.problem = Some(format!("{}: {problem}", sound.name));
                self.silent.push(sound.key.clone());
                self.voices.remove(voice);
                continue;
            }
            for (mixed, sample) in out[offset..offset + samples].iter_mut().zip(&self.scratch) {
                *mixed += sample * gain;
            }
        }
        self.voices.retain(|voice| voice.end > to);
        self.position = to;
        count as usize
    }

    /// The sound of a sound's media file, at `media_sample` samples after its first frame.
    fn open(&self, sound: &Sound, media_sample: i64) -> Result<Option<AudioReader>, String> {
        let Some(path) = sound.file(self.folder.as_deref()) else {
            return Ok(None);
        };
        let Some(mut reader) =
            AudioReader::open(&path, self.rate).map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        reader.seek(media_sample).map_err(|error| error.to_string())?;
        Ok(Some(reader))
    }
}

/// Plays `count` samples of a voice's media at `speed`, each value (by its place among the
/// interleaved values) to `put`: between two media samples, the line joining them. The pitch
/// goes up or down with the speed.
fn resampled(
    voice: &mut Voice,
    speed: f64,
    count: usize,
    mut put: impl FnMut(usize, f32),
) -> Result<(), cannotcut_media::Error> {
    // The media samples needed: up to the one after the last heard.
    let needed = (voice.phase + count as f64 * speed).floor() as usize + 2;
    let held = voice.held.len() / CHANNELS;
    if needed > held {
        let start = voice.held.len();
        voice.held.resize(needed * CHANNELS, 0.0);
        voice.reader.read(&mut voice.held[start..])?;
    }
    for sample in 0..count {
        let at = voice.phase + sample as f64 * speed;
        let base = at.floor() as usize;
        let fraction = (at - base as f64) as f32;
        for channel in 0..CHANNELS {
            let before = voice.held[base * CHANNELS + channel];
            let after = voice.held[(base + 1) * CHANNELS + channel];
            put(sample * CHANNELS + channel, before + (after - before) * fraction);
        }
    }
    voice.phase += count as f64 * speed;
    let passed = (voice.phase.floor() as usize).min(voice.held.len() / CHANNELS);
    voice.held.drain(..passed * CHANNELS);
    voice.phase -= passed as f64;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cannotcut_timeline::Rate;

    #[test]
    fn frames_start_on_the_sample_they_are_heard_on() {
        assert_eq!(frame_to_sample(30, Rate::new(30, 1), 48_000), 48_000);
        assert_eq!(frame_to_sample(1, Rate::new(30000, 1001), 48_000), 1601);
        assert_eq!(frame_to_sample(30000, Rate::new(30000, 1001), 44_100), 1001 * 44_100);
        assert_eq!(frame_to_sample(-1, Rate::new(25, 1), 48_000), -1920);
    }
}
