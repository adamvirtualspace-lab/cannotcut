//! Projects used as clips in other projects.
//!
//! A nested project is a kind of media, kept one of two ways. A linked one is another project's
//! file, which is loaded together with the project that uses it, so changes to it show up
//! wherever it is used. One kept inside the project (a compound clip) is written into the
//! project's own file and belongs to its clip alone. What plays at a frame is found by looking
//! through nested clips down to a media file:
//! [`Timeline::shot_at`] for the picture and [`Timeline::sounds`] for the sound.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::Error;
use crate::model::{Clip, ClipId, FrameRange, Item, Media, Rate, Timeline, Track, TrackKind};

/// The extension of project files, which a clip's media can be.
pub(crate) const PROJECT_EXTENSION: &str = "otio";

/// A project as the media of a clip.
#[derive(Clone, Debug, PartialEq)]
pub struct Nested {
    pub timeline: Timeline,
    /// The project's file, as an absolute path; None for one kept inside the project that uses
    /// it.
    pub path: Option<PathBuf>,
    /// The folder of the project's file, where the relative media paths inside it start; None
    /// for one kept inside another project, whose paths start where that project's do.
    pub folder: Option<PathBuf>,
}

impl Nested {
    /// Loads the project at `path` with every project it uses in turn.
    pub fn load(path: &Path) -> Result<Arc<Self>, Error> {
        Loader::default().load(path).map_err(Failure::into_error)
    }

    /// A linked project, from its file at `path` (absolute).
    pub fn linked(timeline: Timeline, path: PathBuf) -> Self {
        let folder = path.parent().map(Path::to_path_buf);
        Self {
            timeline,
            path: Some(path),
            folder,
        }
    }

    /// A project kept inside the one that uses it.
    pub fn inside(timeline: Timeline) -> Self {
        Self {
            timeline,
            path: None,
            folder: None,
        }
    }

    /// Whether the project at `path` is this one or is used anywhere inside it. A project
    /// cannot be put into one that it holds.
    pub fn holds(&self, path: &Path) -> bool {
        let Ok(path) = fs::canonicalize(path) else {
            return false;
        };
        let folder = self.folder.clone().unwrap_or_default();
        self.path.as_ref() == Some(&path) || self.timeline.uses(&path, &folder)
    }
}

impl Media {
    /// Another project, loaded, as media.
    pub fn from_project(path: &Path, nested: Arc<Nested>) -> Self {
        let mut media = Self::from_path(path, nested.timeline.rate(), 0);
        media.adopt(nested);
        media
    }

    /// A project kept inside the one whose clip plays it: a compound clip's media.
    pub fn inside(timeline: Timeline) -> Self {
        let mut media = Self {
            target: None,
            rate: timeline.rate(),
            available: None,
            name: String::new(),
            metadata: serde_json::Map::new(),
            project: None,
        };
        media.adopt(Arc::new(Nested::inside(timeline)));
        media
    }

    /// Whether this is a project rather than a media file. A linked project's `project` is None
    /// when its file could not be loaded.
    pub fn is_project(&self) -> bool {
        self.project.is_some()
            || self.path().is_some_and(|path| {
                path.extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case(PROJECT_EXTENSION))
            })
    }

    /// Whether this is a project kept inside the one that uses it, rather than a file.
    pub fn is_inside(&self) -> bool {
        self.project.as_ref().is_some_and(|nested| nested.path.is_none())
    }

    /// The media file's path, joined to `folder` when it is relative. None when the media is
    /// missing, and for a project, which is not a file a decoder can open.
    pub fn file(&self, folder: Option<&Path>) -> Option<PathBuf> {
        if self.is_project() {
            return None;
        }
        let path = self.path()?;
        Some(match folder {
            Some(folder) if path.is_relative() => folder.join(path),
            _ => path,
        })
    }

    /// Takes on a freshly loaded project: its rate and length are what the file holds now,
    /// whatever they were when the project using it was saved.
    fn adopt(&mut self, nested: Arc<Nested>) {
        self.rate = nested.timeline.rate();
        self.available = Some(FrameRange {
            start: 0,
            duration: nested.timeline.duration(),
        });
        self.project = Some(nested);
    }
}

/// Loads projects and the projects they use, each file once.
#[derive(Default)]
struct Loader {
    loaded: HashMap<PathBuf, Arc<Nested>>,
    /// The projects being loaded, outermost first: one of them showing up again is a loop.
    opening: Vec<PathBuf>,
}

/// Why a project did not load.
enum Failure {
    /// It uses itself, directly or through others. The whole load is refused.
    Loop(Error),
    /// It could not be read. Inside another project, it plays as missing media.
    Unreadable(Error),
}

impl Failure {
    fn into_error(self) -> Error {
        match self {
            Self::Loop(error) | Self::Unreadable(error) => error,
        }
    }
}

impl Loader {
    fn load(&mut self, path: &Path) -> Result<Arc<Nested>, Failure> {
        let path = fs::canonicalize(path)
            .map_err(|error| Failure::Unreadable(Error::new("open project", error)))?;
        if let Some(loaded) = self.loaded.get(&path) {
            return Ok(loaded.clone());
        }
        if let Some(first) = self.opening.iter().position(|opening| *opening == path) {
            let chain: Vec<String> = self.opening[first..]
                .iter()
                .chain([&path])
                .map(|path| file_name(path))
                .collect();
            return Err(Failure::Loop(Error(format!(
                "projects use each other in a loop: {}",
                chain.join(" uses ")
            ))));
        }
        let folder = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let text = fs::read_to_string(&path)
            .map_err(|error| Failure::Unreadable(Error::new("read project", error)))?;
        let mut timeline = Timeline::from_otio(&text).map_err(|error| {
            Failure::Unreadable(Error(format!("{}: {error}", file_name(&path))))
        })?;
        self.opening.push(path.clone());
        let resolved = self.resolve(&mut timeline, &folder);
        self.opening.pop();
        resolved.map_err(Failure::Loop)?;
        let nested = Arc::new(Nested::linked(timeline, path.clone()));
        self.loaded.insert(path, nested.clone());
        Ok(nested)
    }

    /// Loads the projects `timeline` uses as media, looking inside the projects kept within it
    /// too. One that cannot be read stays unloaded and plays as missing media, as a missing
    /// video file does; a loop is refused.
    fn resolve(&mut self, timeline: &mut Timeline, folder: &Path) -> Result<(), Error> {
        for track in &mut timeline.tracks {
            if !track.clips().any(|(_, clip)| clip.media.is_project()) {
                continue;
            }
            for item in &mut Arc::make_mut(track).items {
                let Item::Clip(clip) = item else {
                    continue;
                };
                if clip.media.is_inside() {
                    let media = Arc::make_mut(&mut clip.media);
                    let nested = media.project.as_mut().expect("a project kept inside");
                    // Its paths start where this timeline's do.
                    self.resolve(&mut Arc::make_mut(nested).timeline, folder)?;
                    continue;
                }
                let Some(path) = clip.media.path().filter(|_| clip.media.is_project()) else {
                    continue;
                };
                // Joining keeps an absolute path as it is.
                match self.load(&folder.join(path)) {
                    Ok(nested) => Arc::make_mut(&mut clip.media).adopt(nested),
                    Err(Failure::Loop(error)) => return Err(error),
                    Err(Failure::Unreadable(_)) => Arc::make_mut(&mut clip.media).project = None,
                }
            }
        }
        Ok(())
    }
}

/// Projects loaded afresh from their files, to put in place of older copies once one of them has
/// been saved: see [`Timeline::adopt_projects`].
pub struct Projects(HashMap<PathBuf, Arc<Nested>>);

impl Projects {
    /// Loads again, from their files, every project `timeline` uses, however deep. `folder` is
    /// where its relative paths start.
    pub fn load(timeline: &Timeline, folder: &Path) -> Result<Self, Error> {
        let mut loader = Loader::default();
        loader.resolve(&mut timeline.clone(), folder)?;
        Ok(Self(loader.loaded))
    }
}

impl Timeline {
    /// Puts the fresh copies in place of the projects the timeline's clips play; a project that
    /// is gone from disk plays as missing media from now on. `folder` is where the timeline's
    /// relative paths start.
    pub fn adopt_projects(&mut self, projects: &Projects, folder: &Path) {
        self.adopt_projects_with(projects, folder, &mut HashMap::new());
    }

    /// The same, with the paths made canonical so far, which saves asking the disk again for
    /// every earlier state of a timeline. Whether anything changed.
    pub(crate) fn adopt_projects_with(
        &mut self,
        projects: &Projects,
        folder: &Path,
        canonical: &mut HashMap<PathBuf, Option<PathBuf>>,
    ) -> bool {
        let mut changed = false;
        for track in &mut self.tracks {
            if !track.clips().any(|(_, clip)| clip.media.is_project()) {
                continue;
            }
            // Worked on a copy, so that a track with nothing to take up stays shared.
            let mut fresh = Track::clone(track);
            let mut fresh_changed = false;
            for item in &mut fresh.items {
                if let Item::Clip(clip) = item {
                    fresh_changed |= clip.adopt_projects(projects, folder, canonical);
                }
            }
            if fresh_changed {
                *track = Arc::new(fresh);
                changed = true;
            }
        }
        changed
    }
}

impl Timeline {
    /// Makes every relative media path absolute, starting at `folder`: for a timeline that is
    /// to be saved in a file somewhere else. Projects kept inside are gone through too.
    pub fn make_paths_absolute(&mut self, folder: &Path) {
        let relative = |clip: &Clip| {
            clip.media.is_inside() || clip.media.path().is_some_and(|path| path.is_relative())
        };
        for track in &mut self.tracks {
            if !track.clips().any(|(_, clip)| relative(clip)) {
                continue;
            }
            for item in &mut Arc::make_mut(track).items {
                let Item::Clip(clip) = item else {
                    continue;
                };
                if clip.media.is_inside() {
                    let media = Arc::make_mut(&mut clip.media);
                    let nested = media.project.as_mut().expect("a project kept inside");
                    Arc::make_mut(nested).timeline.make_paths_absolute(folder);
                } else if let Some(path) = clip.media.path().filter(|path| path.is_relative()) {
                    let target = Media::target_for(&folder.join(path));
                    Arc::make_mut(&mut clip.media).target = Some(target);
                }
            }
        }
    }
}

impl Clip {
    /// Puts the fresh copy in place of the project the clip plays, or takes fresh copies up in
    /// the project it keeps inside. Whether anything changed.
    fn adopt_projects(
        &mut self,
        projects: &Projects,
        folder: &Path,
        canonical: &mut HashMap<PathBuf, Option<PathBuf>>,
    ) -> bool {
        if self.media.is_inside() {
            let nested = self.media.project.as_ref().expect("a project kept inside");
            let mut inner = nested.timeline.clone();
            // Its paths start where this timeline's do.
            if !inner.adopt_projects_with(projects, folder, canonical) {
                return false;
            }
            Arc::make_mut(&mut self.media).adopt(Arc::new(Nested::inside(inner)));
            return true;
        }
        let Some(path) = self.media.path().filter(|_| self.media.is_project()) else {
            return false;
        };
        let path = folder.join(path);
        let path = canonical
            .entry(path.clone())
            .or_insert_with(|| fs::canonicalize(&path).ok());
        let fresh = path.as_ref().and_then(|path| projects.0.get(path));
        let current = self.media.project.as_ref();
        match fresh {
            // Already the fresh copy: nothing to change, nothing to copy.
            Some(fresh) if current.is_some_and(|old| Arc::ptr_eq(old, fresh)) => false,
            Some(fresh) => {
                Arc::make_mut(&mut self.media).adopt(fresh.clone());
                true
            }
            None if current.is_none() => false,
            None => {
                Arc::make_mut(&mut self.media).project = None;
                true
            }
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into())
}

impl Timeline {
    /// Loads the project at `path`, and every project it uses as media.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let nested = Nested::load(path)?;
        // Once the loader is gone nothing else holds the outermost project, so it can be taken
        // out whole.
        Ok(Arc::try_unwrap(nested)
            .map(|nested| nested.timeline)
            .unwrap_or_else(|shared| shared.timeline.clone()))
    }

    /// Whether the project file at `path` (made canonical) is used in this timeline, or in a
    /// project it uses. `folder` is where this timeline's relative paths start.
    fn uses(&self, path: &Path, folder: &Path) -> bool {
        self.tracks().flat_map(|track| track.clips()).any(|(_, clip)| {
            let media = &clip.media;
            let target = media.path().map(|target| folder.join(target));
            let is_it = target
                .and_then(|target| fs::canonicalize(target).ok())
                .is_some_and(|target| target == path);
            is_it
                || media.project.as_ref().is_some_and(|nested| {
                    // One kept inside starts its paths where this timeline does.
                    let inner_folder = nested.folder.as_deref().unwrap_or(folder);
                    nested.timeline.uses(path, inner_folder)
                })
        })
    }

    /// The picture at `frame`, looked for through nested projects down to a media file. None
    /// where nothing plays, including a part of a nested project where nothing plays.
    pub fn shot_at(&self, frame: i64) -> Option<Shot<'_>> {
        let (clip, offset) = self.picture_at(frame)?;
        shot_through(clip, offset, self.rate)
    }

    /// The first clip with a media file, in track order and looking into nested projects: the
    /// one a film takes its shape from.
    pub fn first_shot(&self) -> Option<Shot<'_>> {
        self.tracks()
            .flat_map(|track| track.clips())
            .find_map(|(_, clip)| match &clip.media.project {
                None if clip.media.is_project() => None,
                None => Some(Shot {
                    key: vec![clip.id],
                    clip,
                    media_frame: clip.media_frame(0, self.rate),
                    folder: None,
                }),
                Some(nested) => {
                    let mut shot = nested.timeline.first_shot()?;
                    shot.key.insert(0, clip.id);
                    shot.folder = shot.folder.or(nested.folder.as_deref());
                    Some(shot)
                }
            })
    }

    /// The frames where the picture can change: every start and end of a video clip, and the
    /// same inside the nested projects that clips play, all as frames of this timeline.
    pub(crate) fn picture_edges(&self) -> Vec<i64> {
        let mut edges = Vec::new();
        let video = self.tracks().filter(|track| track.kind == TrackKind::Video);
        for (start, clip) in video.flat_map(|track| track.clips()) {
            edges.extend([start, start + clip.duration]);
            if let Some(nested) = &clip.media.project {
                let inner_rate = nested.timeline.rate();
                for edge in nested.timeline.picture_edges() {
                    // The first frame of this timeline on which the inner edge is reached.
                    let offset = inner_rate.rescale_up(edge, self.rate) - clip.source_start;
                    if (1..clip.duration).contains(&offset) {
                        edges.push(start + offset);
                    }
                }
            }
        }
        edges
    }

    /// Everything heard, looked for through nested projects down to media files: where each
    /// stretch of sound goes, in samples at `sample_rate`, and where its media is heard from.
    /// Disabled clips and tracks, and muted tracks, are left out; volumes are multiplied
    /// through nested clips.
    pub fn sounds(&self, sample_rate: u32) -> Vec<Sound> {
        let to_sample = |frame| frame_to_sample(frame, self.rate, sample_rate);
        let mut sounds = Vec::new();
        let sounding = self
            .tracks()
            .filter(|track| track.enabled && !track.muted);
        for (start, clip) in sounding
            .flat_map(|track| track.clips())
            .filter(|(_, clip)| clip.enabled)
        {
            let (begins, ends) = (to_sample(start), to_sample(start + clip.duration));
            // The sample of the media, or of the nested project, heard at `begins`.
            let from = to_sample(clip.source_start);
            match &clip.media.project {
                None if clip.media.is_project() => {}
                None => sounds.push(Sound {
                    key: vec![clip.id],
                    name: clip.name.clone(),
                    media: clip.media.clone(),
                    folder: None,
                    begins,
                    ends,
                    media_sample: from,
                    gain: clip.volume,
                }),
                Some(nested) => {
                    let until = from + (ends - begins);
                    for inner in nested.timeline.sounds(sample_rate) {
                        let (first, last) = (inner.begins.max(from), inner.ends.min(until));
                        if first >= last {
                            continue;
                        }
                        let mut key = vec![clip.id];
                        key.extend(inner.key);
                        sounds.push(Sound {
                            key,
                            name: inner.name,
                            media: inner.media,
                            folder: inner.folder.or_else(|| nested.folder.clone()),
                            begins: first - from + begins,
                            ends: last - from + begins,
                            media_sample: inner.media_sample + (first - inner.begins),
                            gain: inner.gain * clip.volume,
                        });
                    }
                }
            }
        }
        sounds
    }
}

/// The clip a picture comes from at `offset` frames into `clip`, on a timeline running at
/// `rate`, looking through it when it plays a nested project.
fn shot_through(clip: &Clip, offset: i64, rate: Rate) -> Option<Shot<'_>> {
    let frame = clip.media_frame(offset, rate);
    let Some(nested) = &clip.media.project else {
        return Some(Shot {
            key: vec![clip.id],
            clip,
            media_frame: frame,
            folder: None,
        });
    };
    let (inner, inner_offset) = nested.timeline.picture_at(frame)?;
    let mut shot = shot_through(inner, inner_offset, nested.timeline.rate())?;
    shot.key.insert(0, clip.id);
    // The innermost project file the media sits in decides where its relative path starts.
    shot.folder = shot.folder.or(nested.folder.as_deref());
    Some(shot)
}

/// A frame of a media file, as the picture at some timeline frame.
#[derive(Debug, PartialEq)]
pub struct Shot<'a> {
    /// The clips it is seen through, from the one on the timeline asked inwards; the same key
    /// means the same picture source.
    pub key: Vec<ClipId>,
    /// The clip whose media file plays.
    pub clip: &'a Clip,
    /// The frame of that file, counted from its first.
    pub media_frame: i64,
    /// Where the media's relative path starts when the clip sits in a nested project; None for
    /// the folder of the timeline asked.
    pub folder: Option<&'a Path>,
}

impl Shot<'_> {
    /// The media file to decode, with relative paths of the timeline asked starting at
    /// `project_folder`. None when the media is missing.
    pub fn file(&self, project_folder: Option<&Path>) -> Option<PathBuf> {
        self.clip.media.file(self.folder.or(project_folder))
    }
}

/// A stretch of one media file's sound, placed on a timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct Sound {
    /// The clips it is heard through, from the timeline's own inwards. Unique among the
    /// timeline's sounds.
    pub key: Vec<ClipId>,
    /// The name of the clip whose media plays, for messages.
    pub name: String,
    pub media: Arc<Media>,
    /// Where the media's relative path starts when it sits in a nested project; None for the
    /// timeline's own folder.
    pub folder: Option<PathBuf>,
    /// The first timeline sample it is heard on.
    pub begins: i64,
    /// The timeline sample after its last.
    pub ends: i64,
    /// The sample of the media heard at `begins`, counted from the media's first frame.
    pub media_sample: i64,
    /// How loud: the volumes of the clips it is heard through, multiplied.
    pub gain: f32,
}

impl Sound {
    /// The media file to read, with the timeline's relative paths starting at
    /// `project_folder`. None when the media is missing.
    pub fn file(&self, project_folder: Option<&Path>) -> Option<PathBuf> {
        self.media.file(self.folder.as_deref().or(project_folder))
    }
}

/// The sample, at `sample_rate`, on which frame `frame` of a timeline at `rate` starts.
pub fn frame_to_sample(frame: i64, rate: Rate, sample_rate: u32) -> i64 {
    let samples = i128::from(frame) * i128::from(rate.den) * i128::from(sample_rate);
    samples.div_euclid(i128::from(rate.num.max(1))) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Edit, EditError, NewClip, Placement};

    const OUTER_RATE: Rate = Rate::new(30, 1);
    const INNER_RATE: Rate = Rate::new(60, 1);
    const SAMPLE_RATE: u32 = 48_000;

    /// A folder of its own for each test, removed when the test is done.
    struct Folder(PathBuf);

    impl Folder {
        fn new(test: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("cannotcut-nest-{}-{test}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(path.join("parts")).unwrap();
            Self(path)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn file(path: &str, rate: Rate, frames: i64) -> Arc<Media> {
        Arc::new(Media::from_path(Path::new(path), rate, frames))
    }

    fn place(timeline: &mut Timeline, track: usize, at: i64, clip: NewClip) -> ClipId {
        let edit = Edit::Place {
            track,
            at,
            clip,
            mode: Placement::Overwrite,
        };
        timeline.apply(&edit).unwrap().unwrap()
    }

    fn part(name: &str, media: Arc<Media>, source_start: i64, duration: i64) -> NewClip {
        NewClip {
            name: name.to_string(),
            media,
            source_start,
            duration,
        }
    }

    /// `parts/inner.otio` at 60 fps: two seconds of `a.mp4`, then one second of `b.mp4`, both
    /// next to it. Returns the ids of the two clips.
    fn save_inner(folder: &Folder) -> (ClipId, ClipId) {
        let mut inner = Timeline::new("inner", INNER_RATE);
        let a = place(&mut inner, 0, 0, part("a", file("a.mp4", INNER_RATE, 600), 0, 120));
        let b = place(&mut inner, 0, 120, part("b", file("b.mp4", INNER_RATE, 600), 0, 60));
        inner.save(&folder.0.join("parts/inner.otio")).unwrap();
        (a, b)
    }

    /// `outer.otio` at 30 fps: a second of `x.mp4`, then two seconds of the inner project
    /// from its second second on, at half volume.
    fn outer(folder: &Folder) -> (Timeline, ClipId) {
        let nested = Nested::load(&folder.0.join("parts/inner.otio")).unwrap();
        let project = Arc::new(Media::from_project(Path::new("parts/inner.otio"), nested));
        let mut outer = Timeline::new("outer", OUTER_RATE);
        place(&mut outer, 0, 0, part("x", file("x.mp4", OUTER_RATE, 300), 0, 30));
        let nest = place(&mut outer, 0, 30, part("inner", project, 30, 60));
        let volume = Edit::SetVolume {
            clip: nest,
            volume: 0.5,
        };
        outer.apply(&volume).unwrap();
        (outer, nest)
    }

    #[test]
    fn pictures_come_from_inside_nested_projects() {
        let folder = Folder::new("pictures");
        let (a, b) = save_inner(&folder);
        let (outer, nest) = outer(&folder);
        outer.save(&folder.0.join("outer.otio")).unwrap();
        let outer = Timeline::load(&folder.0.join("outer.otio")).unwrap();

        let top = outer.shot_at(10).unwrap();
        assert_eq!((top.clip.name.as_str(), top.media_frame, top.folder), ("x", 10, None));
        assert_eq!(top.file(Some(&folder.0)), Some(folder.0.join("x.mp4")));

        // A second into the inner project, at twice the rate.
        let shot = outer.shot_at(30).unwrap();
        assert_eq!(shot.key, vec![nest, a]);
        assert_eq!(shot.media_frame, 60);
        let inner_folder = fs::canonicalize(folder.0.join("parts")).unwrap();
        assert_eq!(shot.file(Some(&folder.0)), Some(inner_folder.join("a.mp4")));
        assert_eq!(outer.shot_at(59).unwrap().media_frame, 118);
        let later = outer.shot_at(60).unwrap();
        assert_eq!((later.key.clone(), later.media_frame), (vec![nest, b], 0));
        assert_eq!(outer.shot_at(90), None);

        // Cuts inside the nested project are cuts outside it.
        assert_eq!(outer.next_picture_change(0), Some(30));
        assert_eq!(outer.next_picture_change(30), Some(60));
        assert_eq!(outer.next_picture_change(60), Some(90));
        assert_eq!(outer.next_picture_change(90), None);
    }

    #[test]
    fn sound_comes_from_inside_nested_projects() {
        let folder = Folder::new("sounds");
        save_inner(&folder);
        let (mut outer, nest) = outer(&folder);
        let sounds = outer.sounds(SAMPLE_RATE);
        let placed: Vec<(&str, i64, i64, i64, f32)> = sounds
            .iter()
            .map(|sound| {
                let at = (sound.begins, sound.ends, sound.media_sample);
                (sound.name.as_str(), at.0, at.1, at.2, sound.gain)
            })
            .collect();
        assert_eq!(
            placed,
            [
                ("x", 0, 48_000, 0, 1.0),
                // Clips heard through the nested one are cut to it and take its volume.
                ("a", 48_000, 96_000, 48_000, 0.5),
                ("b", 96_000, 144_000, 0, 0.5),
            ]
        );
        assert_eq!(sounds[1].key[0], nest);

        let mute = Edit::SetMuted {
            track: 0,
            muted: true,
        };
        outer.apply(&mute).unwrap();
        assert!(outer.sounds(SAMPLE_RATE).is_empty());
    }

    #[test]
    fn changes_to_a_nested_project_show_where_it_is_used() {
        let folder = Folder::new("linked");
        save_inner(&folder);
        let (outer, nest) = outer(&folder);
        let outer_path = folder.0.join("outer.otio");
        outer.save(&outer_path).unwrap();

        // The inner project loses its second clip, so the outer one now runs past its end.
        let inner_path = folder.0.join("parts/inner.otio");
        let mut inner = Timeline::load(&inner_path).unwrap();
        let second = inner.track(0).unwrap().clips().nth(1).unwrap().1.id;
        let remove = Edit::Remove {
            clip: second,
            ripple: true,
        };
        inner.apply(&remove).unwrap();
        inner.save(&inner_path).unwrap();

        let outer = Timeline::load(&outer_path).unwrap();
        let (_, clip, _) = outer.clip(nest).unwrap();
        assert_eq!(clip.media.available.unwrap().duration, 120);
        assert!(outer.shot_at(59).is_some());
        // Past the inner project's end there is nothing to see or hear.
        assert_eq!(outer.shot_at(60), None);
        let last = outer.sounds(SAMPLE_RATE).last().unwrap().clone();
        assert_eq!((last.name.as_str(), last.ends), ("a", 96_000));
    }

    #[test]
    fn a_saved_nested_project_is_taken_up_by_every_state_of_the_film() {
        let folder = Folder::new("adopt");
        save_inner(&folder);
        let (outer, nest) = outer(&folder);
        let mut history = crate::History::new(outer);
        let split = Edit::Split { track: 0, at: 45 };
        history.apply(&split).unwrap();

        // The inner project is edited and saved, as stepping inside and back out does.
        let inner_path = folder.0.join("parts/inner.otio");
        let mut inner = Timeline::load(&inner_path).unwrap();
        let first = inner.track(0).unwrap().clips().next().unwrap().1.id;
        let shorter = Edit::Trim {
            clip: first,
            edge: crate::Edge::End,
            frames: -60,
            ripple: true,
        };
        inner.apply(&shorter).unwrap();
        inner.save(&inner_path).unwrap();

        let projects = Projects::load(history.timeline(), &folder.0).unwrap();
        history.adopt_projects(&projects, &folder.0);
        // At film frame 31 the inner project is past its first clip's new end: the second one.
        let now = history.timeline().shot_at(31).unwrap();
        assert_eq!(now.clip.name, "b");
        assert!(history.undo());
        assert_eq!(history.timeline().shot_at(31).unwrap().clip.name, "b");
        let (_, clip, _) = history.timeline().clip(nest).unwrap();
        assert_eq!(clip.media.available.unwrap().duration, 120);
    }

    #[test]
    fn a_missing_nested_project_plays_as_missing_media() {
        let folder = Folder::new("missing");
        save_inner(&folder);
        let (outer, nest) = outer(&folder);
        let outer_path = folder.0.join("outer.otio");
        outer.save(&outer_path).unwrap();
        fs::remove_file(folder.0.join("parts/inner.otio")).unwrap();

        let outer = Timeline::load(&outer_path).unwrap();
        let (_, clip, _) = outer.clip(nest).unwrap();
        assert!(clip.media.is_project() && clip.media.project.is_none());
        let shot = outer.shot_at(30).unwrap();
        assert_eq!(shot.file(Some(&folder.0)), None);
        assert_eq!(outer.sounds(SAMPLE_RATE).len(), 1);
    }

    #[test]
    fn a_project_cannot_hold_itself() {
        let folder = Folder::new("loop");
        // first.otio uses second.otio, which uses first.otio.
        let reference = |path: &str| part(path, file(path, OUTER_RATE, 30), 0, 30);
        let mut first = Timeline::new("first", OUTER_RATE);
        place(&mut first, 0, 0, reference("second.otio"));
        first.save(&folder.0.join("first.otio")).unwrap();
        let mut second = Timeline::new("second", OUTER_RATE);
        place(&mut second, 0, 0, reference("first.otio"));
        second.save(&folder.0.join("second.otio")).unwrap();

        let error = Timeline::load(&folder.0.join("first.otio")).unwrap_err();
        assert!(
            error.to_string().contains("first.otio uses second.otio uses first.otio"),
            "{error}"
        );

        // What the app asks before putting a project into another.
        let inner_path = folder.0.join("parts/inner.otio");
        save_inner(&folder);
        let inner = Nested::load(&inner_path).unwrap();
        assert!(inner.holds(&inner_path));
        assert!(!inner.holds(&folder.0.join("first.otio")));
        let (outer, _) = outer(&folder);
        outer.save(&folder.0.join("outer.otio")).unwrap();
        let outer = Nested::load(&folder.0.join("outer.otio")).unwrap();
        assert!(outer.holds(&inner_path));
    }

    #[test]
    fn a_project_used_twice_is_loaded_once() {
        let folder = Folder::new("twice");
        save_inner(&folder);
        let (mut outer, nest) = outer(&folder);
        let (_, clip, _) = outer.clip(nest).unwrap();
        let again = part("again", clip.media.clone(), 0, 30);
        place(&mut outer, 0, 90, again);
        outer.save(&folder.0.join("outer.otio")).unwrap();

        let outer = Timeline::load(&folder.0.join("outer.otio")).unwrap();
        let projects: Vec<&Arc<Nested>> = outer
            .track(0)
            .unwrap()
            .clips()
            .filter_map(|(_, clip)| clip.media.project.as_ref())
            .collect();
        assert_eq!(projects.len(), 2);
        assert!(Arc::ptr_eq(projects[0], projects[1]));
    }

    /// What the timeline shows and plays, frame by frame and sound by sound, as text.
    fn what_plays(timeline: &Timeline) -> (Vec<String>, Vec<String>) {
        let pictures = (0..timeline.duration())
            .map(|frame| match timeline.shot_at(frame) {
                Some(shot) => format!("{}@{}", shot.clip.name, shot.media_frame),
                None => "-".to_string(),
            })
            .collect();
        let mut sounds: Vec<String> = timeline
            .sounds(SAMPLE_RATE)
            .iter()
            .map(|sound| {
                let at = (sound.begins, sound.ends, sound.media_sample);
                format!("{} {}..{} from {} x{}", sound.name, at.0, at.1, at.2, sound.gain)
            })
            .collect();
        // Mixed together, so the order they are found in does not matter.
        sounds.sort();
        (pictures, sounds)
    }

    /// A film at 30 fps: on the main track `x` 0..30 and `y` 30..60, then `z` 60..90; above
    /// them an overlay `o` at 40..50 at half volume. Returns the ids of x, y, z and o.
    fn film() -> (Timeline, [ClipId; 4]) {
        let mut film = Timeline::new("film", OUTER_RATE);
        let x = place(&mut film, 0, 0, part("x", file("x.mp4", OUTER_RATE, 300), 0, 30));
        let y = place(&mut film, 0, 30, part("y", file("y.mp4", OUTER_RATE, 300), 100, 30));
        let z = place(&mut film, 0, 60, part("z", file("z.mp4", OUTER_RATE, 300), 0, 30));
        let overlay = Edit::AddTrack {
            index: 1,
            kind: TrackKind::Video,
            name: "Overlay".to_string(),
        };
        film.apply(&overlay).unwrap();
        let o = place(&mut film, 1, 40, part("o", file("o.mp4", OUTER_RATE, 300), 5, 10));
        film.apply(&Edit::SetVolume { clip: o, volume: 0.5 }).unwrap();
        (film, [x, y, z, o])
    }

    #[test]
    fn a_compound_clip_plays_what_its_clips_played() {
        let (mut film, [_, y, z, o]) = film();
        let before = what_plays(&film);
        let chosen = [y, o];
        let inner = film.compound(&chosen, "Compound 1").unwrap();
        assert_eq!(inner.tracks().len(), 2);
        let media = Arc::new(Media::inside(inner));
        let compound = Edit::Compound {
            clips: chosen.to_vec(),
            media,
        };
        let id = film.apply(&compound).unwrap().unwrap();

        // One clip in place of two, on the main track, from y's start to y's end.
        let (track, clip, start) = film.clip(id).unwrap();
        assert_eq!((track, start, clip.duration), (0, 30, 30));
        assert_eq!(clip.name, "Compound 1");
        assert!(clip.media.is_inside() && clip.media.is_project());
        assert!(film.clip(y).is_none() && film.clip(o).is_none());
        assert!(film.track(1).unwrap().items().is_empty());
        assert_eq!(what_plays(&film), before);
        assert_eq!(film.next_picture_change(30), Some(40));
        assert_eq!(film.next_picture_change(40), Some(50));

        // Kept in the film's own file, and read back the same.
        let text = film.to_otio();
        assert!(text.contains("\"Stack.1\""), "{text}");
        let read = Timeline::from_otio(&text).unwrap();
        // Read back, clips are numbered afresh; all else is as it was.
        assert_eq!(read.to_otio(), text);
        assert_eq!(what_plays(&read), before);
        // The compound clip still sits before z.
        assert!(film.clip(z).is_some());
    }

    #[test]
    fn a_compound_clip_needs_room_on_its_track() {
        let (film, [x, _, z, _]) = film();
        // x and z, but not y between them on the same track.
        let refused = film.compound(&[x, z], "c");
        assert!(matches!(refused, Err(EditError::InTheWay(_))), "{refused:?}");
        assert_eq!(film.compound(&[], "c"), Err(EditError::NoClips));
    }

    #[test]
    fn edits_inside_a_compound_clip_are_one_undo_step() {
        let (film, [x, y, ..]) = film();
        let mut history = crate::History::new(film);
        let inner = history.timeline().compound(&[x, y], "c").unwrap();
        let compound = Edit::Compound {
            clips: vec![x, y],
            media: Arc::new(Media::inside(inner)),
        };
        let id = history.apply(&compound).unwrap().unwrap();
        let made = history.timeline().clone();

        // Stepping inside, taking the first clip out, and back out again.
        let (_, clip, _) = made.clip(id).unwrap();
        let mut inside = clip.media.project.as_ref().unwrap().timeline.clone();
        let first = inside.track(0).unwrap().clips().next().unwrap().1.id;
        inside.apply(&Edit::Remove { clip: first, ripple: true }).unwrap();
        let edited = Edit::SetMedia {
            clip: id,
            media: Arc::new(Media::inside(inside)),
        };
        history.apply(&edited).unwrap();
        // Where x was, y plays; the compound clip keeps its length, so its end shows nothing
        // (but the overlay, at 40..50, which is not in it).
        let shot = history.timeline().shot_at(0).unwrap();
        assert_eq!((shot.clip.name.as_str(), shot.media_frame), ("y", 100));
        assert_eq!(history.timeline().shot_at(35), None);
        assert_eq!(history.undo_label(), Some("Edit inside"));
        assert!(history.undo());
        assert_eq!(history.timeline(), &made);
        assert!(history.undo());
        assert!(history.timeline().clip(x).is_some());
    }

    #[test]
    fn linked_projects_inside_a_compound_clip_are_loaded_and_taken_up() {
        let folder = Folder::new("inside");
        save_inner(&folder);
        let (outer, nest) = outer(&folder);
        // The linked project and the clip before it become a compound clip in the file.
        let first = outer.track(0).unwrap().clips().next().unwrap().1.id;
        let inner = outer.compound(&[first, nest], "both").unwrap();
        let mut film = outer.clone();
        let compound = Edit::Compound {
            clips: vec![first, nest],
            media: Arc::new(Media::inside(inner)),
        };
        film.apply(&compound).unwrap();
        let before = what_plays(&outer);
        assert_eq!(what_plays(&film), before);
        let path = folder.0.join("outer.otio");
        film.save(&path).unwrap();
        let film = Timeline::load(&path).unwrap();
        assert_eq!(what_plays(&film), before);

        // The linked project changes: the compound clip shows the change once taken up.
        let inner_path = folder.0.join("parts/inner.otio");
        let mut inner = Timeline::load(&inner_path).unwrap();
        let a = inner.track(0).unwrap().clips().next().unwrap().1.id;
        inner.apply(&Edit::Remove { clip: a, ripple: true }).unwrap();
        inner.save(&inner_path).unwrap();
        let mut history = crate::History::new(film);
        let shot = history.timeline().shot_at(30).unwrap();
        assert_eq!((shot.clip.name.as_str(), shot.media_frame), ("a", 60));
        let projects = Projects::load(history.timeline(), &folder.0).unwrap();
        history.adopt_projects(&projects, &folder.0);
        // The film plays the inner project from its second second on, which is now past its
        // end: b alone, a second long, is left.
        assert_eq!(history.timeline().shot_at(30), None);
        let (_, compound) = history.timeline().track(0).unwrap().clips().next().unwrap();
        let inside = &compound.media.project.as_ref().unwrap().timeline;
        let (_, linked) = inside.track(0).unwrap().clips().nth(1).unwrap();
        assert_eq!(linked.media.available.unwrap().duration, 60);
        // A project kept inside still cannot hold the one it is in.
        let holder = Nested::load(&path).unwrap();
        assert!(holder.holds(&inner_path));
    }

    #[test]
    fn relative_paths_are_made_absolute_for_a_file_elsewhere() {
        let (mut film, [x, y, ..]) = film();
        let inner = film.compound(&[x], "c").unwrap();
        film.apply(&Edit::Compound {
            clips: vec![x],
            media: Arc::new(Media::inside(inner)),
        })
        .unwrap();
        let home = std::env::temp_dir();
        film.make_paths_absolute(&home);
        let (_, clip, _) = film.clip(y).unwrap();
        assert_eq!(clip.media.path(), Some(home.join("y.mp4")));
        let shot = film.shot_at(0).unwrap();
        assert_eq!(shot.file(None), Some(home.join("x.mp4")));
    }
}
