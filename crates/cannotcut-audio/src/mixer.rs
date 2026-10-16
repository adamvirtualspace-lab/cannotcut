use std::path::PathBuf;

use cannotcut_media::{AudioReader, CHANNELS};
use cannotcut_timeline::{Clip, ClipId, Rate, Timeline};

/// Mixes the sound of every clip on a timeline, from a starting frame onwards, in order.
pub struct Mixer {
    timeline: Timeline,
    /// Where the project file is; relative media paths start from here.
    folder: Option<PathBuf>,
    rate: u32,
    /// The timeline sample mixed next.
    position: i64,
    end: i64,
    voices: Vec<Voice>,
    /// Clips found to have no sound, or sound that could not be read. They are not tried again.
    silent: Vec<ClipId>,
    scratch: Vec<f32>,
    /// What went wrong reading a clip's sound, most recently.
    pub problem: Option<String>,
}

/// A clip being heard: its media file, read from front to back.
struct Voice {
    clip: ClipId,
    /// The timeline sample after the clip's last.
    end: i64,
    reader: AudioReader,
}

impl Mixer {
    /// A mix of `timeline` from frame `from`, at `rate` samples a second.
    pub fn new(timeline: Timeline, from: i64, rate: u32, folder: Option<PathBuf>) -> Self {
        let to_sample = |frame| frame_to_sample(frame, timeline.rate(), rate);
        Self {
            position: to_sample(from),
            end: to_sample(timeline.duration()),
            timeline,
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
        let (timeline_rate, rate) = (self.timeline.rate(), self.rate);
        let to_sample = |frame| frame_to_sample(frame, timeline_rate, rate);

        let mut heard = Vec::new();
        for track in self.timeline.tracks().filter(|track| track.enabled) {
            for (start, clip) in track.clips().filter(|(_, clip)| clip.enabled) {
                let (begins, ends) = (to_sample(start), to_sample(start + clip.duration));
                if begins < to && ends > from && !self.silent.contains(&clip.id) {
                    heard.push((begins, ends, clip.id));
                }
            }
        }
        for (begins, ends, id) in heard {
            let (first, last) = (begins.max(from), ends.min(to));
            let voice = match self.voices.iter().position(|voice| voice.clip == id) {
                Some(index) => index,
                None => {
                    let Some((_, clip, _)) = self.timeline.clip(id) else {
                        continue;
                    };
                    // A clip is joined where it is first heard: at its start, or wherever
                    // the mix began if that was part-way into it.
                    let media_sample = to_sample(clip.source_start) + (first - begins);
                    match self.open(clip, media_sample) {
                        Ok(Some(reader)) => {
                            self.voices.push(Voice {
                                clip: id,
                                end: ends,
                                reader,
                            });
                            self.voices.len() - 1
                        }
                        Ok(None) => {
                            self.silent.push(id);
                            continue;
                        }
                        Err(problem) => {
                            self.problem = Some(format!("{}: {problem}", clip.name));
                            self.silent.push(id);
                            continue;
                        }
                    }
                }
            };
            let samples = (last - first) as usize * CHANNELS;
            self.scratch.resize(samples, 0.0);
            if let Err(problem) = self.voices[voice].reader.read(&mut self.scratch) {
                let name = self.timeline.clip(id).map(|(_, clip, _)| clip.name.as_str());
                self.problem = Some(format!("{}: {problem}", name.unwrap_or("a clip")));
                self.silent.push(id);
                self.voices.remove(voice);
                continue;
            }
            let offset = (first - from) as usize * CHANNELS;
            for (mixed, sample) in out[offset..offset + samples].iter_mut().zip(&self.scratch) {
                *mixed += sample;
            }
        }
        self.voices.retain(|voice| voice.end > to);
        self.position = to;
        count as usize
    }

    /// The sound of a clip's media, at `media_sample` samples after its first frame.
    fn open(&self, clip: &Clip, media_sample: i64) -> Result<Option<AudioReader>, String> {
        let Some(path) = clip.media.path() else {
            return Ok(None);
        };
        let path = match &self.folder {
            Some(folder) if path.is_relative() => folder.join(path),
            _ => path,
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

/// The sample, at `sample_rate`, on which timeline frame `frame` starts.
pub fn frame_to_sample(frame: i64, rate: Rate, sample_rate: u32) -> i64 {
    let samples = i128::from(frame) * i128::from(rate.den) * i128::from(sample_rate);
    samples.div_euclid(i128::from(rate.num.max(1))) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_start_on_the_sample_they_are_heard_on() {
        assert_eq!(frame_to_sample(30, Rate::new(30, 1), 48_000), 48_000);
        assert_eq!(frame_to_sample(1, Rate::new(30000, 1001), 48_000), 1601);
        assert_eq!(frame_to_sample(30000, Rate::new(30000, 1001), 44_100), 1001 * 44_100);
        assert_eq!(frame_to_sample(-1, Rate::new(25, 1), 48_000), -1920);
    }
}
