use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};

/// No timeline is longer than this many frames; keeps frame arithmetic far from overflowing.
pub(crate) const MAX_FRAMES: i64 = 1 << 40;

/// Frames per second as an exact fraction, such as 30000/1001.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rate {
    pub num: u32,
    pub den: u32,
}

impl Rate {
    pub const fn new(num: u32, den: u32) -> Self {
        Self { num, den }
    }

    pub fn as_f64(self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }

    /// The exact rate a floating-point number from a file stands for. Whole rates and the NTSC
    /// ones (30000/1001 and its kin, however loosely written) are recognised; anything else is
    /// taken to the thousandth.
    pub fn from_f64(rate: f64) -> Option<Self> {
        if !rate.is_finite() || rate < 0.001 || rate > 100_000.0 {
            return None;
        }
        let whole = rate.round();
        if (rate - whole).abs() < 1e-6 {
            return Some(Self::new(whole as u32, 1));
        }
        let ntsc = (rate * 1.001).round();
        if (rate - ntsc / 1.001).abs() < 0.005 {
            return Some(Self::new(ntsc as u32 * 1000, 1001));
        }
        let (num, den) = ((rate * 1000.0).round() as u32, 1000);
        let divisor = gcd(num, den);
        Some(Self::new(num / divisor, den / divisor))
    }

    /// The frame at rate `to` that is showing when frame `frame` at this rate starts.
    pub fn rescale(self, frame: i64, to: Rate) -> i64 {
        self.scaled(frame, to).0
    }

    /// Like [`rescale`](Self::rescale), but to the nearest frame instead of the one showing.
    pub(crate) fn rescale_nearest(self, frame: i64, to: Rate) -> i64 {
        let (floor, remainder, divisor) = self.scaled(frame, to);
        floor + i64::from(remainder * 2 >= divisor)
    }

    /// `frame` converted to rate `to`: the quotient, the remainder, and the divisor.
    fn scaled(self, frame: i64, to: Rate) -> (i64, i128, i128) {
        let numerator = i128::from(frame) * i128::from(self.den) * i128::from(to.num);
        let divisor = i128::from(self.num.max(1)) * i128::from(to.den.max(1));
        (
            numerator.div_euclid(divisor) as i64,
            numerator.rem_euclid(divisor),
            divisor,
        )
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a.max(1) } else { gcd(b, a % b) }
}

/// A run of frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameRange {
    pub start: i64,
    pub duration: i64,
}

/// The parts of an OpenTimelineIO object cannotcut does not interpret. They are kept, so a file
/// from another tool does not lose them by passing through.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extras {
    pub metadata: Map<String, Value>,
    pub effects: Vec<Value>,
    pub markers: Vec<Value>,
}

/// A media file that clips play from.
#[derive(Clone, Debug, PartialEq)]
pub struct Media {
    /// Where the file is: a `file://` URL or a path, possibly relative to the project file.
    /// None when the project knows the file is missing.
    pub target: Option<String>,
    /// The file's own frame rate.
    pub rate: Rate,
    /// The frames the file holds, at `rate`, when known. Media with a start timecode begins
    /// above zero.
    pub available: Option<FrameRange>,
    pub name: String,
    pub metadata: Map<String, Value>,
}

impl Media {
    /// A file on disk with `frames` frames at `rate`.
    pub fn from_path(path: &Path, rate: Rate, frames: i64) -> Self {
        Self {
            target: Some(file_url(path)),
            rate,
            available: Some(FrameRange {
                start: 0,
                duration: frames,
            }),
            name: String::new(),
            metadata: Map::new(),
        }
    }

    /// The `target` that [`from_path`](Self::from_path) gives a file.
    pub fn target_for(path: &Path) -> String {
        file_url(path)
    }

    /// The file's path as written in the project; a relative one is relative to the project
    /// file.
    pub fn path(&self) -> Option<PathBuf> {
        let target = self.target.as_deref()?;
        let Some(rest) = target.strip_prefix("file:") else {
            return Some(PathBuf::from(target));
        };
        let decoded = percent_decode(rest);
        // `file:///C:/clips/a.mp4` is a path with a drive, `file:///home/a.mp4` an absolute one
        // and `file://server/share/a.mp4` one on the network.
        let local = decoded
            .strip_prefix("///")
            .or_else(|| decoded.strip_prefix("//localhost/"));
        let path = match local {
            Some(local) if has_drive(local) => local.to_string(),
            Some(local) => format!("/{local}"),
            None => match decoded.strip_prefix('/') {
                Some(local) if has_drive(local) => local.to_string(),
                _ => decoded,
            },
        };
        Some(PathBuf::from(path))
    }
}

fn has_drive(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn file_url(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    let prefix = if has_drive(&path) {
        "file:///"
    } else if path.starts_with("//") {
        "file:"
    } else if path.starts_with('/') {
        "file://"
    } else {
        // A relative path stays one; it is resolved against the project file.
        ""
    };
    let mut url = String::from(prefix);
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/:".contains(&byte) {
            url.push(char::from(byte));
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escaped = (bytes[index] == b'%')
            .then(|| text.get(index + 1..index + 3))
            .flatten()
            .and_then(|digits| u8::from_str_radix(digits, 16).ok());
        match escaped {
            Some(byte) => {
                decoded.push(byte);
                index += 3;
            }
            None => {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Names a clip for as long as it is on the timeline, whatever happens around it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClipId(pub(crate) u64);

#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub id: ClipId,
    pub name: String,
    pub media: Arc<Media>,
    /// Where in the media the clip begins: timeline frames after the media's first frame.
    pub source_start: i64,
    /// Length on the timeline, in timeline frames.
    pub duration: i64,
    pub enabled: bool,
    pub extras: Extras,
}

impl Clip {
    /// The frame of the media file, counted from its first, that shows `offset` timeline
    /// frames into the clip.
    pub fn media_frame(&self, offset: i64, timeline_rate: Rate) -> i64 {
        timeline_rate.rescale(self.source_start + offset, self.media.rate)
    }

    /// How far, in timeline frames, the clip's end can still move later before the media runs
    /// out; None when the media's length is unknown.
    pub(crate) fn media_left(&self, timeline_rate: Rate) -> Option<i64> {
        let available = self.media.available?;
        let length = self.media.rate.rescale(available.duration, timeline_rate);
        Some((length - self.source_start - self.duration).max(0))
    }
}

/// What a track holds: clips, and gaps where nothing plays.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Clip(Clip),
    /// Its length in timeline frames.
    Gap(i64),
}

impl Item {
    pub fn duration(&self) -> i64 {
        match self {
            Self::Clip(clip) => clip.duration,
            Self::Gap(duration) => *duration,
        }
    }

    /// Cuts the item `at` frames in: this keeps the first part, the rest is returned. A clip's
    /// second part is a clip of its own and gets the next id.
    pub(crate) fn split_off(&mut self, at: i64, next_id: &mut u64) -> Item {
        match self {
            Self::Gap(duration) => {
                let rest = *duration - at;
                *duration = at;
                Self::Gap(rest)
            }
            Self::Clip(clip) => {
                let rest = Clip {
                    id: new_id(next_id),
                    source_start: clip.source_start + at,
                    duration: clip.duration - at,
                    ..clip.clone()
                };
                clip.duration = at;
                Self::Clip(rest)
            }
        }
    }
}

pub(crate) fn new_id(next_id: &mut u64) -> ClipId {
    *next_id += 1;
    ClipId(*next_id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
}

/// A row of clips and gaps, back to back from frame zero. Gaps never touch each other, nothing
/// is empty, and a track never ends in a gap.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub name: String,
    pub kind: TrackKind,
    pub enabled: bool,
    pub extras: Extras,
    pub(crate) items: Vec<Item>,
}

impl Track {
    pub fn new(kind: TrackKind, name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind,
            enabled: true,
            extras: Extras::default(),
            items: Vec::new(),
        }
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Every item with the frame it starts on.
    pub fn placed(&self) -> impl Iterator<Item = (i64, &Item)> {
        self.items.iter().scan(0, |start, item| {
            let at = *start;
            *start += item.duration();
            Some((at, item))
        })
    }

    /// Every clip with the frame it starts on.
    pub fn clips(&self) -> impl Iterator<Item = (i64, &Clip)> {
        self.placed().filter_map(|(start, item)| match item {
            Item::Clip(clip) => Some((start, clip)),
            Item::Gap(_) => None,
        })
    }

    pub fn duration(&self) -> i64 {
        self.items.iter().map(Item::duration).sum()
    }

    /// The frames where one item ends and the next begins, and both ends of the track.
    pub fn borders(&self) -> impl Iterator<Item = i64> {
        self.placed()
            .map(|(start, _)| start)
            .chain(std::iter::once(self.duration()))
    }

    /// The start nearest to `wanted` at which something `duration` frames long covers no clip:
    /// `wanted` itself when that is free, otherwise right before or right after a clip.
    pub fn free_start(&self, wanted: i64, duration: i64) -> i64 {
        let wanted = wanted.max(0);
        let fits = |start: i64| {
            start >= 0
                && self
                    .clips()
                    .all(|(at, clip)| start + duration <= at || start >= at + clip.duration)
        };
        if fits(wanted) {
            return wanted;
        }
        self.clips()
            .flat_map(|(at, clip)| [at - duration, at + clip.duration])
            .filter(|start| fits(*start))
            .min_by_key(|start| (start - wanted).abs())
            .unwrap_or_else(|| self.duration())
    }

    /// The clip playing at `frame`, with how far into it that is.
    pub fn clip_at(&self, frame: i64) -> Option<(&Clip, i64)> {
        self.clips()
            .find(|(start, clip)| (*start..*start + clip.duration).contains(&frame))
            .map(|(start, clip)| (clip, frame - start))
    }

    /// Index and starting frame of the item with this clip.
    pub(crate) fn locate(&self, id: ClipId) -> Option<(usize, i64)> {
        self.placed()
            .enumerate()
            .find(|(_, (_, item))| matches!(item, Item::Clip(clip) if clip.id == id))
            .map(|(index, (start, _))| (index, start))
    }

    /// Makes `frame` the border between two items and returns the index of the item that
    /// starts there. Past the end, a gap is added to reach it.
    pub(crate) fn cut_at(&mut self, frame: i64, next_id: &mut u64) -> usize {
        let mut start = 0;
        for index in 0..self.items.len() {
            if start == frame {
                return index;
            }
            let end = start + self.items[index].duration();
            if frame < end {
                let rest = self.items[index].split_off(frame - start, next_id);
                self.items.insert(index + 1, rest);
                return index + 1;
            }
            start = end;
        }
        if frame > start {
            self.items.push(Item::Gap(frame - start));
        }
        self.items.len()
    }

    /// Restores what the type promises about gaps after items were moved around.
    pub(crate) fn tidy(&mut self) {
        let mut tidy: Vec<Item> = Vec::with_capacity(self.items.len());
        for item in self.items.drain(..) {
            if item.duration() <= 0 {
                continue;
            }
            if let (Some(Item::Gap(before)), Item::Gap(more)) = (tidy.last_mut(), &item) {
                *before += more;
            } else {
                tidy.push(item);
            }
        }
        while matches!(tidy.last(), Some(Item::Gap(_))) {
            tidy.pop();
        }
        self.items = tidy;
    }
}

/// A stack of tracks, the first at the bottom: where video overlaps, later tracks cover
/// earlier ones.
#[derive(Clone, Debug, PartialEq)]
pub struct Timeline {
    pub name: String,
    pub metadata: Map<String, Value>,
    pub(crate) rate: Rate,
    /// Timecode of the first frame, in frames.
    pub(crate) start: i64,
    /// Shared, so keeping an earlier state of the timeline for undo copies only the tracks an
    /// edit touched.
    pub(crate) tracks: Vec<Arc<Track>>,
    /// Kept parts of the stack object that holds the tracks in the file.
    pub(crate) stack: Extras,
    /// The id given out last.
    pub(crate) next_id: u64,
}

impl Timeline {
    /// An empty timeline with one video track, as a new project starts out.
    pub fn new(name: impl Into<String>, rate: Rate) -> Self {
        Self {
            name: name.into(),
            metadata: Map::new(),
            rate,
            start: 0,
            tracks: vec![Arc::new(Track::new(TrackKind::Video, "Main"))],
            stack: Extras::default(),
            next_id: 0,
        }
    }

    pub fn rate(&self) -> Rate {
        self.rate
    }

    pub fn tracks(&self) -> impl DoubleEndedIterator<Item = &Track> + ExactSizeIterator {
        self.tracks.iter().map(Arc::as_ref)
    }

    pub fn track(&self, index: usize) -> Option<&Track> {
        self.tracks.get(index).map(Arc::as_ref)
    }

    /// Length of the longest track.
    pub fn duration(&self) -> i64 {
        self.tracks().map(Track::duration).max().unwrap_or(0)
    }

    /// The track a clip is on, the clip, and the frame it starts on.
    pub fn clip(&self, id: ClipId) -> Option<(usize, &Clip, i64)> {
        self.tracks().enumerate().find_map(|(index, track)| {
            track
                .clips()
                .find(|(_, clip)| clip.id == id)
                .map(|(start, clip)| (index, clip, start))
        })
    }

    /// The picture at `frame`: the clip on the topmost enabled video track that has one there,
    /// with how far into the clip that is.
    pub fn picture_at(&self, frame: i64) -> Option<(&Clip, i64)> {
        self.tracks()
            .rev()
            .filter(|track| track.enabled && track.kind == TrackKind::Video)
            .find_map(|track| track.clip_at(frame).filter(|(clip, _)| clip.enabled))
    }
}

impl Timeline {
    /// The first frame after `frame` on which the picture comes from another clip than it does
    /// on `frame`, or from none: the next cut as the viewer sees it. None when the picture stays
    /// as it is to the end.
    pub fn next_picture_change(&self, frame: i64) -> Option<i64> {
        let shown = |frame: i64| self.picture_at(frame).map(|(clip, _)| clip.id);
        let now = shown(frame);
        // The picture can only change where a clip begins or ends.
        let mut edges: Vec<i64> = self
            .tracks()
            .filter(|track| track.kind == TrackKind::Video)
            .flat_map(|track| track.clips())
            .flat_map(|(start, clip)| [start, start + clip.duration])
            .filter(|edge| *edge > frame)
            .collect();
        edges.sort_unstable();
        edges.into_iter().find(|edge| shown(*edge) != now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_from_files_come_out_exact() {
        assert_eq!(Rate::from_f64(24.0), Some(Rate::new(24, 1)));
        assert_eq!(Rate::from_f64(29.97002997002997), Some(Rate::new(30000, 1001)));
        assert_eq!(Rate::from_f64(29.97), Some(Rate::new(30000, 1001)));
        assert_eq!(Rate::from_f64(23.976), Some(Rate::new(24000, 1001)));
        assert_eq!(Rate::from_f64(23.98), Some(Rate::new(24000, 1001)));
        assert_eq!(Rate::from_f64(59.94), Some(Rate::new(60000, 1001)));
        assert_eq!(Rate::from_f64(12.5), Some(Rate::new(25, 2)));
        assert_eq!(Rate::from_f64(0.0), None);
        assert_eq!(Rate::from_f64(f64::NAN), None);
    }

    #[test]
    fn frames_convert_between_rates() {
        let (thirty, sixty) = (Rate::new(30, 1), Rate::new(60, 1));
        assert_eq!(thirty.rescale(10, sixty), 20);
        assert_eq!(sixty.rescale(21, thirty), 10);
        assert_eq!(sixty.rescale_nearest(21, thirty), 11);
        assert_eq!(Rate::new(30000, 1001).rescale(30000, Rate::new(1, 1)), 1001);
        assert_eq!(Rate::new(24, 1).rescale(-1, Rate::new(48, 1)), -2);
    }

    #[test]
    fn media_paths_survive_as_urls() {
        for path in [
            r"C:\clips\my clip #1.mp4",
            "/home/adam/clips/é.mkv",
            "//server/share/a.mov",
            "media/a.mp4",
        ] {
            let media = Media::from_path(Path::new(path), Rate::new(30, 1), 10);
            assert_eq!(
                media.path(),
                Some(PathBuf::from(path.replace('\\', "/"))),
                "{:?}",
                media.target
            );
        }
        let media = Media::from_path(Path::new(r"C:\a b.mp4"), Rate::new(30, 1), 10);
        assert_eq!(media.target.as_deref(), Some("file:///C:/a%20b.mp4"));
    }
}
