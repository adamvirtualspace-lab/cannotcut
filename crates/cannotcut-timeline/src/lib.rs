//! The timeline cannotcut edits, and its file format.
//!
//! A [`Timeline`] is a stack of tracks; a track is a row of clips and gaps, measured in whole
//! frames at the timeline's rate. It only changes through [`Edit`]s, and a [`History`] of them
//! gives undo and redo. Projects are stored as OpenTimelineIO (`.otio`) files, and a project
//! can be a clip in another.
//!
//! Nothing here touches media files or the screen; project files are read and written here.

mod edit;
mod model;
mod nest;
mod otio;
mod title;
mod transform;
mod transition;

use std::fmt;

pub use edit::{Edge, Edit, EditError, History, NewClip, Placement};
pub use model::{
    Clip, ClipId, Extras, FrameRange, Item, Media, Rate, SPEEDS, Timeline, Track, TrackKind,
};
pub use nest::{Nested, Projects, Shot, Sound, frame_to_sample};
pub use transform::{Affine, Framing, Keyframe, Transform, transform_at};
pub use title::{Title, TitleFont};
pub use transition::{Reveal, Transition, TransitionKind, Wipe};

/// A timeline file could not be read or written.
#[derive(Debug)]
pub struct Error(String);

impl Error {
    fn new(context: &str, cause: impl fmt::Display) -> Self {
        Self(format!("{context}: {cause}"))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}
