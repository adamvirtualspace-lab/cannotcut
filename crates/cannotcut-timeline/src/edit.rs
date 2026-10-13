use std::fmt;
use std::ops::RangeInclusive;
use std::sync::Arc;

use crate::model::{
    Clip, ClipId, Extras, Item, MAX_FRAMES, Media, Rate, Timeline, Track, TrackKind, new_id,
};

/// How many edits can be undone; older ones are forgotten.
const MAX_UNDO_STEPS: usize = 500;

/// A change to a timeline. Every change the user can make is one of these, so everything can be
/// undone the same way.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// Adds an empty track; `index` 0 is the bottom of the stack.
    AddTrack {
        index: usize,
        kind: TrackKind,
        name: String,
    },
    RemoveTrack {
        index: usize,
    },
    /// Puts a new clip on a track, starting at frame `at`.
    Place {
        track: usize,
        at: i64,
        clip: NewClip,
        mode: Placement,
    },
    /// Cuts the clip playing at frame `at` in two.
    Split {
        track: usize,
        at: i64,
    },
    /// Moves one edge of a clip by `frames`, later when positive. With `ripple` the clips after
    /// it move along and the clip stays where it starts; without, they stay put and the edge
    /// can only move through empty space.
    Trim {
        clip: ClipId,
        edge: Edge,
        frames: i64,
        ripple: bool,
    },
    /// Takes a clip off the timeline. With `ripple` what follows closes up, otherwise a gap is
    /// left.
    Remove {
        clip: ClipId,
        ripple: bool,
    },
    /// Takes a clip out as [`Edit::Remove`] does and puts it at frame `at` of a track as
    /// [`Edit::Place`] does. `at` counts after the clip was taken out.
    Move {
        clip: ClipId,
        track: usize,
        at: i64,
        mode: Placement,
        ripple: bool,
    },
}

impl Edit {
    /// What the edit is called in the undo menu.
    pub fn label(&self) -> &'static str {
        match self {
            Self::AddTrack { .. } => "Add track",
            Self::RemoveTrack { .. } => "Remove track",
            Self::Place { .. } => "Add clip",
            Self::Split { .. } => "Split",
            Self::Trim { .. } => "Trim",
            Self::Remove { .. } => "Delete",
            Self::Move { .. } => "Move",
        }
    }
}

/// A clip that is not on the timeline yet.
#[derive(Clone, Debug, PartialEq)]
pub struct NewClip {
    pub name: String,
    pub media: Arc<Media>,
    /// Where in the media it begins: timeline frames after the media's first frame.
    pub source_start: i64,
    /// Length in timeline frames.
    pub duration: i64,
}

impl NewClip {
    /// All of `media`, on a timeline running at `timeline_rate`. None when the media's length
    /// is unknown or less than one timeline frame.
    pub fn whole(name: impl Into<String>, media: Arc<Media>, timeline_rate: Rate) -> Option<Self> {
        let duration = media.rate.rescale(media.available?.duration, timeline_rate);
        (duration > 0).then(|| Self {
            name: name.into(),
            media,
            source_start: 0,
            duration,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// Everything from that frame on moves later to make room.
    Insert,
    /// Whatever was in the way is cut away.
    Overwrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
}

/// Why an edit was refused. The timeline is unchanged.
#[derive(Clone, Debug, PartialEq)]
pub enum EditError {
    NoSuchTrack(usize),
    NoSuchClip(ClipId),
    /// A frame or length outside what a timeline can hold.
    OutOfBounds,
    /// No clip plays at the frame to split at, or one begins exactly there.
    NothingToSplit,
    /// The edge cannot move that far; these are the amounts it can move.
    TrimTooFar { allowed: RangeInclusive<i64> },
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchTrack(index) => write!(f, "there is no track {index}"),
            Self::NoSuchClip(_) => f.write_str("the clip is no longer on the timeline"),
            Self::OutOfBounds => f.write_str("that is outside the timeline"),
            Self::NothingToSplit => f.write_str("there is no clip to split there"),
            Self::TrimTooFar { allowed } => write!(
                f,
                "the edge can move between {} and {} frames",
                allowed.start(),
                allowed.end()
            ),
        }
    }
}

impl std::error::Error for EditError {}

impl Timeline {
    /// Makes the change, or refuses and leaves the timeline as it was. Returns the clip the
    /// edit created, if it created one: the placed clip, or the second half of a split.
    pub fn apply(&mut self, edit: &Edit) -> Result<Option<ClipId>, EditError> {
        // Tracks are shared between copies, so working on a copy costs little and a refused
        // edit cannot leave half its work behind.
        let mut edited = self.clone();
        let created = edited.perform(edit)?;
        *self = edited;
        Ok(created)
    }

    fn perform(&mut self, edit: &Edit) -> Result<Option<ClipId>, EditError> {
        match edit {
            Edit::AddTrack { index, kind, name } => {
                if *index > self.tracks.len() {
                    return Err(EditError::NoSuchTrack(*index));
                }
                self.tracks
                    .insert(*index, Arc::new(Track::new(*kind, name.clone())));
                Ok(None)
            }
            Edit::RemoveTrack { index } => {
                if *index >= self.tracks.len() {
                    return Err(EditError::NoSuchTrack(*index));
                }
                self.tracks.remove(*index);
                Ok(None)
            }
            Edit::Place {
                track,
                at,
                clip,
                mode,
            } => {
                check_span(*at, clip.duration)?;
                let clip = Clip {
                    id: new_id(&mut self.next_id),
                    name: clip.name.clone(),
                    media: clip.media.clone(),
                    source_start: clip.source_start,
                    duration: clip.duration,
                    enabled: true,
                    extras: Extras::default(),
                };
                let id = clip.id;
                self.place(*track, *at, clip, *mode)?;
                Ok(Some(id))
            }
            Edit::Split { track, at } => {
                let next_id = &mut self.next_id;
                let track = self
                    .tracks
                    .get_mut(*track)
                    .ok_or(EditError::NoSuchTrack(*track))?;
                let (index, start) = track
                    .placed()
                    .enumerate()
                    .find(|(_, (start, item))| (*start..*start + item.duration()).contains(at))
                    .map(|(index, (start, _))| (index, start))
                    .ok_or(EditError::NothingToSplit)?;
                if start == *at || matches!(track.items[index], Item::Gap(_)) {
                    return Err(EditError::NothingToSplit);
                }
                let track = Arc::make_mut(track);
                let rest = track.items[index].split_off(*at - start, next_id);
                let created = match &rest {
                    Item::Clip(clip) => Some(clip.id),
                    Item::Gap(_) => None,
                };
                track.items.insert(index + 1, rest);
                Ok(created)
            }
            Edit::Trim {
                clip,
                edge,
                frames,
                ripple,
            } => {
                let allowed = self
                    .trim_range(*clip, *edge, *ripple)
                    .ok_or(EditError::NoSuchClip(*clip))?;
                if !allowed.contains(frames) {
                    return Err(EditError::TrimTooFar { allowed });
                }
                let (track, index) = self.locate(*clip)?;
                Arc::make_mut(&mut self.tracks[track]).trim(index, *edge, *frames, *ripple);
                Ok(None)
            }
            Edit::Remove { clip, ripple } => {
                self.take(*clip, *ripple)?;
                Ok(None)
            }
            Edit::Move {
                clip,
                track,
                at,
                mode,
                ripple,
            } => {
                if *track >= self.tracks.len() {
                    return Err(EditError::NoSuchTrack(*track));
                }
                let moved = self.take(*clip, *ripple)?;
                check_span(*at, moved.duration)?;
                self.place(*track, *at, moved, *mode)?;
                Ok(None)
            }
        }
    }

    /// How far [`Edit::Trim`] can move an edge of a clip, in frames (negative is earlier). The
    /// limits are the media's own beginning and end, a length of one frame, and without
    /// `ripple` the neighbouring clips.
    pub fn trim_range(
        &self,
        clip: ClipId,
        edge: Edge,
        ripple: bool,
    ) -> Option<RangeInclusive<i64>> {
        let (track, index) = self.locate(clip).ok()?;
        let items = &self.tracks[track].items;
        let Item::Clip(clip) = &items[index] else {
            return None;
        };
        // How far a neighbour lets the edge come: all the way through a gap, not at all into a
        // clip.
        let room = |neighbour: Option<&Item>| match neighbour {
            Some(Item::Gap(length)) => *length,
            Some(Item::Clip(_)) => 0,
            None => MAX_FRAMES,
        };
        let shorter = clip.duration - 1;
        Some(match edge {
            Edge::Start => {
                let media = clip.source_start.max(0);
                let before = match index.checked_sub(1) {
                    // The first item starts on frame zero.
                    None => 0,
                    Some(index) => room(items.get(index)),
                };
                let earlier = if ripple { media } else { media.min(before) };
                -earlier..=shorter
            }
            Edge::End => {
                let media = clip.media_left(self.rate).unwrap_or(MAX_FRAMES);
                let after = room(items.get(index + 1));
                let later = if ripple { media } else { media.min(after) };
                -shorter..=later
            }
        })
    }

    /// Track and item index of a clip.
    fn locate(&self, clip: ClipId) -> Result<(usize, usize), EditError> {
        self.tracks
            .iter()
            .enumerate()
            .find_map(|(track, items)| items.locate(clip).map(|(index, _)| (track, index)))
            .ok_or(EditError::NoSuchClip(clip))
    }

    fn take(&mut self, clip: ClipId, ripple: bool) -> Result<Clip, EditError> {
        let (track, index) = self.locate(clip)?;
        let track = Arc::make_mut(&mut self.tracks[track]);
        let gap = Item::Gap(if ripple { 0 } else { track.items[index].duration() });
        let Item::Clip(taken) = std::mem::replace(&mut track.items[index], gap) else {
            return Err(EditError::NoSuchClip(clip));
        };
        track.tidy();
        Ok(taken)
    }

    fn place(
        &mut self,
        track: usize,
        at: i64,
        clip: Clip,
        mode: Placement,
    ) -> Result<(), EditError> {
        let next_id = &mut self.next_id;
        let track = self
            .tracks
            .get_mut(track)
            .ok_or(EditError::NoSuchTrack(track))?;
        let track = Arc::make_mut(track);
        let index = track.cut_at(at, next_id);
        if mode == Placement::Overwrite {
            let end = track.cut_at(at + clip.duration, next_id);
            track.items.drain(index..end);
        }
        track.items.insert(index, Item::Clip(clip));
        track.tidy();
        Ok(())
    }
}

fn check_span(at: i64, duration: i64) -> Result<(), EditError> {
    if (0..=MAX_FRAMES).contains(&at) && (1..=MAX_FRAMES).contains(&duration) {
        Ok(())
    } else {
        Err(EditError::OutOfBounds)
    }
}

impl Track {
    /// Moves an edge of the clip at `index` by `frames`; the caller has checked there is room.
    fn trim(&mut self, index: usize, edge: Edge, frames: i64, ripple: bool) {
        let Item::Clip(clip) = &mut self.items[index] else {
            return;
        };
        match edge {
            Edge::Start => {
                clip.source_start += frames;
                clip.duration -= frames;
                // Without ripple the clip's end stays where it is, so the space before it makes
                // up the difference.
                if !ripple && frames > 0 {
                    self.items.insert(index, Item::Gap(frames));
                } else if !ripple && index > 0 {
                    if let Item::Gap(before) = &mut self.items[index - 1] {
                        *before += frames;
                    }
                }
            }
            Edge::End => {
                clip.duration += frames;
                if !ripple && index + 1 < self.items.len() {
                    if frames < 0 {
                        self.items.insert(index + 1, Item::Gap(-frames));
                    } else if let Item::Gap(after) = &mut self.items[index + 1] {
                        *after -= frames;
                    }
                }
            }
        }
        self.tidy();
    }
}

/// A timeline together with the edits made to it, for undo and redo.
#[derive(Clone, Debug)]
pub struct History {
    timeline: Timeline,
    /// Earlier states, oldest first, each with the name of the edit that led away from it.
    undo: Vec<(&'static str, Timeline)>,
    /// States that were undone, the next one to redo last.
    redo: Vec<(&'static str, Timeline)>,
}

impl History {
    pub fn new(timeline: Timeline) -> Self {
        Self {
            timeline,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    /// Makes an edit that can be undone. See [`Timeline::apply`].
    pub fn apply(&mut self, edit: &Edit) -> Result<Option<ClipId>, EditError> {
        let before = self.timeline.clone();
        let created = self.timeline.apply(edit)?;
        self.undo.push((edit.label(), before));
        if self.undo.len() > MAX_UNDO_STEPS {
            self.undo.remove(0);
        }
        self.redo.clear();
        Ok(created)
    }

    /// Name of the edit [`undo`](Self::undo) would take back.
    pub fn undo_label(&self) -> Option<&'static str> {
        self.undo.last().map(|(label, _)| *label)
    }

    /// Name of the edit [`redo`](Self::redo) would make again.
    pub fn redo_label(&self) -> Option<&'static str> {
        self.redo.last().map(|(label, _)| *label)
    }

    /// Takes the last edit back. False when there is none.
    pub fn undo(&mut self) -> bool {
        let Some((label, earlier)) = self.undo.pop() else {
            return false;
        };
        let undone = std::mem::replace(&mut self.timeline, earlier);
        self.redo.push((label, undone));
        true
    }

    /// Makes the edit that was last undone again. False when there is none.
    pub fn redo(&mut self) -> bool {
        let Some((label, later)) = self.redo.pop() else {
            return false;
        };
        let before = std::mem::replace(&mut self.timeline, later);
        self.undo.push((label, before));
        true
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    const RATE: Rate = Rate::new(30, 1);

    fn media(frames: i64) -> Arc<Media> {
        Arc::new(Media::from_path(Path::new("/clips/a.mp4"), RATE, frames))
    }

    fn clip(source_start: i64, duration: i64) -> NewClip {
        NewClip {
            name: "a".to_string(),
            media: media(1000),
            source_start,
            duration,
        }
    }

    fn place(timeline: &mut Timeline, at: i64, new: NewClip, mode: Placement) -> ClipId {
        let edit = Edit::Place {
            track: 0,
            at,
            clip: new,
            mode,
        };
        timeline.apply(&edit).unwrap().unwrap()
    }

    /// A track as text: `c` plus source start and length for clips, `g` plus length for gaps.
    fn layout(timeline: &Timeline, track: usize) -> String {
        let items = timeline.track(track).unwrap().items();
        let described: Vec<String> = items
            .iter()
            .map(|item| match item {
                Item::Clip(clip) => format!("c{}+{}", clip.source_start, clip.duration),
                Item::Gap(duration) => format!("g{duration}"),
            })
            .collect();
        described.join(" ")
    }

    fn assert_tidy(timeline: &Timeline) {
        let mut ids = std::collections::HashSet::new();
        for track in timeline.tracks() {
            let items = track.items();
            assert!(!matches!(items.last(), Some(Item::Gap(_))), "{items:?}");
            for pair in items.windows(2) {
                assert!(
                    !matches!(pair, [Item::Gap(_), Item::Gap(_)]),
                    "gaps touch: {items:?}"
                );
            }
            for item in items {
                assert!(item.duration() > 0, "{items:?}");
                if let Item::Clip(clip) = item {
                    assert!(ids.insert(clip.id), "{:?} is on the timeline twice", clip.id);
                }
            }
        }
    }

    #[test]
    fn placing_inserts_or_overwrites() {
        let mut timeline = Timeline::new("test", RATE);
        place(&mut timeline, 0, clip(0, 100), Placement::Insert);
        place(&mut timeline, 150, clip(0, 50), Placement::Overwrite);
        assert_eq!(layout(&timeline, 0), "c0+100 g50 c0+50");

        // Inserting in the middle of a clip cuts it and pushes the rest along.
        place(&mut timeline, 40, clip(500, 10), Placement::Insert);
        assert_eq!(layout(&timeline, 0), "c0+40 c500+10 c40+60 g50 c0+50");

        // Overwriting cuts away what it covers, across several items.
        place(&mut timeline, 30, clip(700, 100), Placement::Overwrite);
        assert_eq!(layout(&timeline, 0), "c0+30 c700+100 g30 c0+50");
        assert_eq!(timeline.duration(), 210);
        assert_tidy(&timeline);
    }

    #[test]
    fn splitting_makes_two_clips_that_play_the_same() {
        let mut timeline = Timeline::new("test", RATE);
        let first = place(&mut timeline, 10, clip(200, 100), Placement::Overwrite);
        let second = timeline
            .apply(&Edit::Split { track: 0, at: 40 })
            .unwrap()
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(layout(&timeline, 0), "g10 c200+30 c230+70");
        let (clip, offset) = timeline.picture_at(45).unwrap();
        assert_eq!((clip.id, clip.media_frame(offset, RATE)), (second, 235));

        for at in [5, 10, 40, 110, 500] {
            assert_eq!(
                timeline.apply(&Edit::Split { track: 0, at }),
                Err(EditError::NothingToSplit),
                "split at {at}"
            );
        }
    }

    #[test]
    fn trimming_without_ripple_leaves_the_neighbours_alone() {
        let mut timeline = Timeline::new("test", RATE);
        let first = place(&mut timeline, 0, clip(50, 100), Placement::Insert);
        let second = place(&mut timeline, 120, clip(0, 100), Placement::Overwrite);
        let trim = |clip, edge, frames| Edit::Trim {
            clip,
            edge,
            frames,
            ripple: false,
        };

        // The first clip starts on frame zero, so its start cannot move earlier; its end can
        // move through the gap but not into the next clip.
        assert_eq!(timeline.trim_range(first, Edge::Start, false), Some(0..=99));
        assert_eq!(timeline.trim_range(first, Edge::End, false), Some(-99..=20));
        assert_eq!(
            timeline.apply(&trim(first, Edge::End, 21)),
            Err(EditError::TrimTooFar { allowed: -99..=20 })
        );

        timeline.apply(&trim(first, Edge::End, 20)).unwrap();
        assert_eq!(layout(&timeline, 0), "c50+120 c0+100");
        timeline.apply(&trim(first, Edge::End, -30)).unwrap();
        assert_eq!(layout(&timeline, 0), "c50+90 g30 c0+100");
        timeline.apply(&trim(first, Edge::Start, 10)).unwrap();
        assert_eq!(layout(&timeline, 0), "g10 c60+80 g30 c0+100");

        // The second clip begins at the media's first frame, so only the gap is left to shrink.
        assert_eq!(timeline.trim_range(second, Edge::Start, false), Some(0..=99));
        timeline.apply(&trim(second, Edge::Start, 25)).unwrap();
        assert_eq!(layout(&timeline, 0), "g10 c60+80 g55 c25+75");
        timeline.apply(&trim(second, Edge::Start, -25)).unwrap();
        assert_eq!(layout(&timeline, 0), "g10 c60+80 g30 c0+100");
        assert_tidy(&timeline);
    }

    #[test]
    fn trimming_with_ripple_moves_what_follows() {
        let mut timeline = Timeline::new("test", RATE);
        let first = place(&mut timeline, 0, clip(50, 100), Placement::Insert);
        place(&mut timeline, 100, clip(0, 100), Placement::Insert);
        let trim = |edge, frames| Edit::Trim {
            clip: first,
            edge,
            frames,
            ripple: true,
        };

        // The media is 1000 frames long and the clip ends on its frame 150.
        assert_eq!(timeline.trim_range(first, Edge::End, true), Some(-99..=850));
        assert_eq!(timeline.trim_range(first, Edge::Start, true), Some(-50..=99));

        timeline.apply(&trim(Edge::End, -40)).unwrap();
        assert_eq!(layout(&timeline, 0), "c50+60 c0+100");
        timeline.apply(&trim(Edge::Start, -50)).unwrap();
        assert_eq!(layout(&timeline, 0), "c0+110 c0+100");
        timeline.apply(&trim(Edge::Start, 109)).unwrap();
        assert_eq!(layout(&timeline, 0), "c109+1 c0+100");
    }

    #[test]
    fn removing_closes_up_or_leaves_a_gap() {
        let mut timeline = Timeline::new("test", RATE);
        let first = place(&mut timeline, 0, clip(0, 10), Placement::Insert);
        let second = place(&mut timeline, 10, clip(0, 20), Placement::Insert);
        let third = place(&mut timeline, 30, clip(0, 30), Placement::Insert);

        let mut lifted = timeline.clone();
        let lift = |clip| Edit::Remove {
            clip,
            ripple: false,
        };
        lifted.apply(&lift(second)).unwrap();
        assert_eq!(layout(&lifted, 0), "c0+10 g20 c0+30");
        lifted.apply(&lift(third)).unwrap();
        assert_eq!(layout(&lifted, 0), "c0+10");
        assert_eq!(
            lifted.apply(&lift(third)),
            Err(EditError::NoSuchClip(third))
        );

        let close = Edit::Remove {
            clip: first,
            ripple: true,
        };
        timeline.apply(&close).unwrap();
        assert_eq!(layout(&timeline, 0), "c0+20 c0+30");
    }

    #[test]
    fn moving_keeps_the_clip_and_its_id() {
        let mut timeline = Timeline::new("test", RATE);
        let first = place(&mut timeline, 0, clip(0, 10), Placement::Insert);
        place(&mut timeline, 10, clip(100, 20), Placement::Insert);
        let add_track = Edit::AddTrack {
            index: 1,
            kind: TrackKind::Video,
            name: "Overlay".to_string(),
        };
        timeline.apply(&add_track).unwrap();

        // Reordering on a magnetic track: the hole closes, the clip goes in after the other.
        let reorder = Edit::Move {
            clip: first,
            track: 0,
            at: 20,
            mode: Placement::Insert,
            ripple: true,
        };
        timeline.apply(&reorder).unwrap();
        assert_eq!(layout(&timeline, 0), "c100+20 c0+10");

        // Up to a free track: a gap stays behind, which is dropped at the end of a track.
        let lift = Edit::Move {
            clip: first,
            track: 1,
            at: 5,
            mode: Placement::Overwrite,
            ripple: false,
        };
        timeline.apply(&lift).unwrap();
        assert_eq!(layout(&timeline, 0), "c100+20");
        assert_eq!(layout(&timeline, 1), "g5 c0+10");
        let (track, clip, start) = timeline.clip(first).unwrap();
        assert_eq!((track, clip.duration, start), (1, 10, 5));

        // The upper track covers the lower one where it has a clip.
        assert_eq!(timeline.picture_at(4).unwrap().0.source_start, 100);
        assert_eq!(timeline.picture_at(5).unwrap().0.id, first);

        let nowhere = Edit::Move {
            clip: first,
            track: 7,
            at: 0,
            mode: Placement::Insert,
            ripple: false,
        };
        let before = timeline.clone();
        assert_eq!(timeline.apply(&nowhere), Err(EditError::NoSuchTrack(7)));
        assert_eq!(timeline, before);
    }

    #[test]
    fn free_space_is_found_next_to_the_clips_in_the_way() {
        let mut timeline = Timeline::new("test", RATE);
        place(&mut timeline, 100, clip(0, 50), Placement::Overwrite);
        place(&mut timeline, 170, clip(0, 30), Placement::Overwrite);
        let track = timeline.track(0).unwrap();
        assert_eq!(layout(&timeline, 0), "g100 c0+50 g20 c0+30");
        assert_eq!(track.borders().collect::<Vec<_>>(), [0, 100, 150, 170, 200]);

        // Free as asked, before the start, and far beyond the end.
        assert_eq!(track.free_start(40, 60), 40);
        assert_eq!(track.free_start(-5, 10), 0);
        assert_eq!(track.free_start(900, 10), 900);
        // Overlapping the first clip from the left: right before it is nearest.
        assert_eq!(track.free_start(60, 60), 40);
        // Twenty frames fit exactly between the two clips, thirty do not.
        assert_eq!(track.free_start(145, 20), 150);
        assert_eq!(track.free_start(145, 30), 200);
        assert_eq!(track.free_start(120, 200), 200);
    }

    #[test]
    fn clips_follow_the_medias_own_frame_rate() {
        let sixty = Arc::new(Media::from_path(
            Path::new("/clips/b.mp4"),
            Rate::new(60, 1),
            600,
        ));
        let whole = NewClip::whole("b", sixty, RATE).unwrap();
        assert_eq!(whole.duration, 300);
        let mut timeline = Timeline::new("test", RATE);
        let id = place(&mut timeline, 0, whole, Placement::Insert);
        let (_, clip, _) = timeline.clip(id).unwrap();
        assert_eq!(clip.media_frame(7, RATE), 14);
        assert_eq!(timeline.trim_range(id, Edge::End, true), Some(-299..=0));
    }

    #[test]
    fn undo_and_redo_walk_through_the_edits() {
        let mut history = History::new(Timeline::new("test", RATE));
        let empty = history.timeline().clone();
        let placed = Edit::Place {
            track: 0,
            at: 0,
            clip: clip(0, 100),
            mode: Placement::Insert,
        };
        history.apply(&placed).unwrap();
        let one_clip = history.timeline().clone();
        history.apply(&Edit::Split { track: 0, at: 30 }).unwrap();
        let two_clips = history.timeline().clone();

        // A refused edit is not something to undo.
        assert!(history.apply(&Edit::Split { track: 0, at: 30 }).is_err());
        assert_eq!(history.undo_label(), Some("Split"));

        assert!(history.undo());
        assert_eq!(history.timeline(), &one_clip);
        assert_eq!(history.redo_label(), Some("Split"));
        assert!(history.undo());
        assert_eq!(history.timeline(), &empty);
        assert!(!history.undo());
        assert!(history.redo());
        assert!(history.redo());
        assert_eq!(history.timeline(), &two_clips);
        assert!(!history.redo());

        // A new edit after an undo drops what could have been redone.
        history.undo();
        history.apply(&Edit::Split { track: 0, at: 50 }).unwrap();
        assert_eq!(history.redo_label(), None);
    }

    /// Throws a few thousand arbitrary edits at a timeline: whatever is accepted must leave it
    /// well formed, whatever is refused must leave it untouched, and undoing everything must
    /// lead back to the start.
    #[test]
    fn arbitrary_edits_keep_the_timeline_well_formed() {
        let mut seed = 0x5eed_u64;
        let mut random = move |below: u64| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((seed >> 33) % below) as i64
        };
        let mut history = History::new(Timeline::new("test", RATE));
        // The state each accepted edit started from.
        let mut states = Vec::new();
        for _ in 0..4000 {
            let timeline = history.timeline();
            let ids: Vec<ClipId> = timeline
                .tracks()
                .flat_map(|track| track.clips().map(|(_, clip)| clip.id))
                .collect();
            let any_clip = |pick: i64| ids.get(pick as usize % ids.len().max(1)).copied();
            let track = random(timeline.tracks().len() as u64 + 1) as usize;
            let mode = [Placement::Insert, Placement::Overwrite][random(2) as usize];
            let ripple = random(2) == 0;
            let edit = match random(8) {
                0 => Edit::AddTrack {
                    index: track,
                    kind: TrackKind::Video,
                    name: String::new(),
                },
                1 if random(6) == 0 => Edit::RemoveTrack { index: track },
                1 | 2 => Edit::Place {
                    track,
                    at: random(400),
                    clip: clip(random(300), random(120)),
                    mode,
                },
                3 => Edit::Split {
                    track,
                    at: random(400),
                },
                4 | 5 => Edit::Trim {
                    clip: any_clip(random(64)).unwrap_or(ClipId(0)),
                    edge: [Edge::Start, Edge::End][random(2) as usize],
                    frames: random(81) - 40,
                    ripple,
                },
                6 => Edit::Remove {
                    clip: any_clip(random(64)).unwrap_or(ClipId(0)),
                    ripple,
                },
                _ => Edit::Move {
                    clip: any_clip(random(64)).unwrap_or(ClipId(0)),
                    track,
                    at: random(400),
                    mode,
                    ripple,
                },
            };
            let before = history.timeline().clone();
            let frames_before: i64 = clip_frames(&before);
            // Splitting, and moving a clip in between others, neither lose nor invent a frame.
            let keeps_frames = matches!(
                edit,
                Edit::Split { .. }
                    | Edit::Move {
                        mode: Placement::Insert,
                        ..
                    }
            );
            match history.apply(&edit) {
                Ok(_) => {
                    assert_tidy(history.timeline());
                    if keeps_frames {
                        assert_eq!(clip_frames(history.timeline()), frames_before, "{edit:?}");
                    }
                    states.push(before);
                }
                Err(_) => assert_eq!(history.timeline(), &before, "{edit:?}"),
            }
        }
        assert!(states.len() > 1000, "only {} edits were accepted", states.len());
        let mut undone = 0;
        while history.undo() {
            assert_eq!(Some(history.timeline()), states.pop().as_ref());
            undone += 1;
        }
        assert_eq!(undone, MAX_UNDO_STEPS);
    }

    fn clip_frames(timeline: &Timeline) -> i64 {
        timeline
            .tracks()
            .flat_map(|track| track.clips().map(|(_, clip)| clip.duration))
            .sum()
    }
}
