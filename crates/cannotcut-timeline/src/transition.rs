//! Transitions: a clip coming in over the one before it on its track, instead of cutting to it.
//! For the first frames of the clip, the clip before carries on beneath it (past its end, as
//! far as its media goes) while the new clip fades in or is wiped across it. Where nothing comes
//! before, it comes in over whatever is below.

/// How a clip comes in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionKind {
    /// It fades in over the one before: a cross-dissolve.
    Dissolve,
    /// An edge sweeps across the frame, the new clip behind it.
    WipeFromLeft,
    WipeFromRight,
    WipeFromTop,
    WipeFromBottom,
}

impl TransitionKind {
    pub const ALL: [TransitionKind; 5] = [
        Self::Dissolve,
        Self::WipeFromLeft,
        Self::WipeFromRight,
        Self::WipeFromTop,
        Self::WipeFromBottom,
    ];

    /// What it is called where it is chosen.
    pub fn name(self) -> &'static str {
        match self {
            Self::Dissolve => "Cross dissolve",
            Self::WipeFromLeft => "Wipe from left",
            Self::WipeFromRight => "Wipe from right",
            Self::WipeFromTop => "Wipe from top",
            Self::WipeFromBottom => "Wipe from bottom",
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Dissolve => "dissolve",
            Self::WipeFromLeft => "wipe_left",
            Self::WipeFromRight => "wipe_right",
            Self::WipeFromTop => "wipe_top",
            Self::WipeFromBottom => "wipe_bottom",
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.key() == key)
    }

    /// The transition type OpenTimelineIO names: a dissolve is SMPTE's; the rest are ours.
    pub(crate) fn otio_type(self) -> &'static str {
        match self {
            Self::Dissolve => "SMPTE_Dissolve",
            _ => "Custom_Transition",
        }
    }

    /// How a clip coming in this way is seen, `progress` of the way in (0 to 1).
    pub fn reveal(self, progress: f32) -> Reveal {
        let progress = progress.clamp(0.0, 1.0);
        // Seen where the frame point's step along `direction` is at most the threshold, which
        // sweeps from -1 (nothing) to 1 (everything).
        let wipe = |direction: [f32; 2]| Reveal {
            fade: 1.0,
            wipe: Some(Wipe {
                direction,
                threshold: -1.0 + 2.0 * progress,
            }),
        };
        match self {
            Self::Dissolve => Reveal {
                fade: progress,
                wipe: None,
            },
            Self::WipeFromLeft => wipe([1.0, 0.0]),
            Self::WipeFromRight => wipe([-1.0, 0.0]),
            // The frame's y goes up.
            Self::WipeFromTop => wipe([0.0, -1.0]),
            Self::WipeFromBottom => wipe([0.0, 1.0]),
        }
    }
}

/// A clip's way in: how, and over how many timeline frames from its start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition {
    pub kind: TransitionKind,
    pub duration: i64,
}

/// How much of a picture is seen at a frame, beyond its transform: all of it, save during a
/// transition.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reveal {
    /// Its opacity is multiplied by this.
    pub fade: f32,
    /// Only part of the frame shows it.
    pub wipe: Option<Wipe>,
}

/// The part of the frame a picture shows in, during a wipe: where a point of the frame (from -1
/// to 1 across it, y up) has `direction · point <= threshold`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wipe {
    pub direction: [f32; 2],
    pub threshold: f32,
}

impl Reveal {
    /// All of it.
    pub const FULL: Reveal = Reveal {
        fade: 1.0,
        wipe: None,
    };

    pub fn is_full(&self) -> bool {
        *self == Self::FULL
    }

    /// Whether the point of the frame `point` (-1 to 1 across it, y up) shows the picture.
    pub fn shows(&self, point: [f32; 2]) -> bool {
        self.wipe.is_none_or(|wipe| {
            wipe.direction[0] * point[0] + wipe.direction[1] * point[1] <= wipe.threshold
        })
    }

    /// A picture inside a nested project, revealed as `self` there, as seen through the clip
    /// that plays the project, revealed as `outer`. Fades multiply; the outer wipe wins.
    pub fn within(&self, outer: &Reveal) -> Reveal {
        Reveal {
            fade: self.fade * outer.fade,
            wipe: outer.wipe.or(self.wipe),
        }
    }
}

/// How far a clip with `transition` is in, `offset` frames from its start: from just after 0
/// on its first frame to just before 1 on the transition's last.
pub(crate) fn progress(transition: &Transition, offset: i64) -> f32 {
    (offset + 1) as f32 / (transition.duration + 1) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wipe_from_the_left_shows_more_of_the_frame_as_it_goes() {
        let start = TransitionKind::WipeFromLeft.reveal(0.0);
        let middle = TransitionKind::WipeFromLeft.reveal(0.5);
        let end = TransitionKind::WipeFromLeft.reveal(1.0);
        assert!(!start.shows([-0.99, 0.0]));
        assert!(middle.shows([-0.5, 0.0]) && !middle.shows([0.5, 0.0]));
        assert!(end.shows([0.99, 0.5]));
        // From the top: the top of the frame (y up) shows first.
        let top = TransitionKind::WipeFromTop.reveal(0.25);
        assert!(top.shows([0.0, 0.9]) && !top.shows([0.0, 0.0]));
    }

    #[test]
    fn a_dissolve_fades_in() {
        assert_eq!(TransitionKind::Dissolve.reveal(0.25).fade, 0.25);
        let transition = Transition {
            kind: TransitionKind::Dissolve,
            duration: 3,
        };
        assert_eq!(progress(&transition, 0), 0.25);
        assert_eq!(progress(&transition, 2), 0.75);
        let nested = Reveal {
            fade: 0.5,
            wipe: None,
        }
        .within(&TransitionKind::WipeFromLeft.reveal(0.5));
        assert_eq!(nested.fade, 0.5);
        assert!(nested.wipe.is_some());
    }
}
