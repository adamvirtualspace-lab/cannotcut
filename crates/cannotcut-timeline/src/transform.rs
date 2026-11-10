//! Where a clip's picture sits in the frame, when pictures are layered: moved, scaled, slanted,
//! turned and seen through; and how that changes over the clip, from keyframe to keyframe.

use serde_json::{Value, json};

/// A clip's picture in the frame. The picture is first fitted inside the frame, keeping its
/// shape, centred; then it is slanted, scaled and turned about its centre, and moved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    /// How far the centre moves right, as a share of the frame's width.
    pub x: f32,
    /// How far the centre moves down, as a share of the frame's height.
    pub y: f32,
    /// 1 as fitted; 0.5 half as big.
    pub scale: f32,
    /// Degrees, clockwise.
    pub rotation: f32,
    /// Degrees the picture's sides lean: its top goes right of its bottom.
    pub skew_x: f32,
    /// Degrees its top and bottom lean: its right side goes up from its left.
    pub skew_y: f32,
    /// 1 covers what is under it; 0 cannot be seen.
    pub opacity: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    /// Fitted, centred, and covering.
    pub const IDENTITY: Self = Self {
        x: 0.0,
        y: 0.0,
        scale: 1.0,
        rotation: 0.0,
        skew_x: 0.0,
        skew_y: 0.0,
        opacity: 1.0,
    };
    /// The smallest scale kept, so that a picture never vanishes into a point.
    pub const LEAST_SCALE: f32 = 0.01;
    /// The most a picture leans, either way: past it, it would lie flat.
    pub const MOST_SKEW: f32 = 75.0;

    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    /// Whether every number is one a picture can be drawn with.
    pub fn is_valid(&self) -> bool {
        let numbers = [
            self.x,
            self.y,
            self.scale,
            self.rotation,
            self.skew_x,
            self.skew_y,
            self.opacity,
        ];
        numbers.iter().all(|value| value.is_finite())
            && self.scale >= Self::LEAST_SCALE
            && self.skew_x.abs() <= Self::MOST_SKEW
            && self.skew_y.abs() <= Self::MOST_SKEW
            && (0.0..=1.0).contains(&self.opacity)
    }

    /// The way from `self` to `other`, `share` of it along (0 to 1).
    pub fn toward(&self, other: &Transform, share: f32) -> Transform {
        let mix = |from: f32, to: f32| from + (to - from) * share;
        Transform {
            x: mix(self.x, other.x),
            y: mix(self.y, other.y),
            scale: mix(self.scale, other.scale),
            rotation: mix(self.rotation, other.rotation),
            skew_x: mix(self.skew_x, other.skew_x),
            skew_y: mix(self.skew_y, other.skew_y),
            opacity: mix(self.opacity, other.opacity),
        }
    }

    /// Where a picture of `picture` pixels goes in a frame of `frame` pixels, fitted inside and
    /// then placed by this transform (see [`Affine`]). The viewer and the film both place
    /// pictures by it, so they agree.
    pub fn corners(&self, frame: (u32, u32), picture: (u32, u32)) -> Affine {
        let frame_aspect = frame.0 as f32 / frame.1.max(1) as f32;
        let picture_aspect = picture.0 as f32 / picture.1.max(1) as f32;
        // The picture fitted inside the frame, as shares of the frame's half sides.
        let fitted = if picture_aspect > frame_aspect {
            [1.0, frame_aspect / picture_aspect]
        } else {
            [picture_aspect / frame_aspect, 1.0]
        };
        // Slanting and turning are done where a step right is as long as a step up: in frame
        // heights. The half sides of the picture there:
        let half_width = fitted[0] * self.scale * frame_aspect;
        let half_height = fitted[1] * self.scale;
        let lean_x = self.skew_x.to_radians().tan();
        let lean_y = self.skew_y.to_radians().tan();
        // Clockwise on the screen, where y goes up here.
        let (sin, cos) = (-self.rotation.to_radians()).sin_cos();
        let turn = |[x, y]: [f32; 2]| [cos * x - sin * y, sin * x + cos * y];
        let across = turn([half_width, half_width * lean_y]);
        let up = turn([half_height * lean_x, half_height]);
        // The offsets are shares of the frame's sides, which run -1 to 1: two halves.
        Affine([
            across[0] / frame_aspect,
            across[1],
            up[0] / frame_aspect,
            up[1],
            2.0 * self.x,
            -2.0 * self.y,
        ])
    }

    pub(crate) fn value(&self) -> Value {
        json!({
            "x": self.x,
            "y": self.y,
            "scale": self.scale,
            "rotation": self.rotation,
            "skew_x": self.skew_x,
            "skew_y": self.skew_y,
            "opacity": self.opacity,
        })
    }

    /// A transform as kept in a file; what is missing is as it starts, and one that cannot be
    /// drawn is ignored.
    pub(crate) fn from_value(value: &Value) -> Self {
        let number = |name: &str, default: f32| {
            value
                .get(name)
                .and_then(Value::as_f64)
                .map_or(default, |number| number as f32)
        };
        let transform = Self {
            x: number("x", 0.0),
            y: number("y", 0.0),
            scale: number("scale", 1.0),
            rotation: number("rotation", 0.0),
            skew_x: number("skew_x", 0.0),
            skew_y: number("skew_y", 0.0),
            opacity: number("opacity", 1.0),
        };
        if transform.is_valid() {
            transform
        } else {
            Self::IDENTITY
        }
    }
}

/// A placement in the frame: a 2x2 matrix, column by column, and an offset, taking a point
/// from -1 to 1 across a picture to -1 to 1 across the frame, both with y up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine(pub [f32; 6]);

impl Affine {
    /// Leaves everything where it is.
    pub const IDENTITY: Self = Self([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    pub fn apply(&self, [x, y]: [f32; 2]) -> [f32; 2] {
        let [a, b, c, d, e, f] = self.0;
        [a * x + c * y + e, b * x + d * y + f]
    }

    /// `inner` first, then this.
    pub fn after(&self, inner: &Affine) -> Affine {
        let [a, b, c, d, _, _] = self.0;
        let [ia, ib, ic, id, ie, if_] = inner.0;
        let linear = |x: f32, y: f32| [a * x + c * y, b * x + d * y];
        let [ca, cb] = linear(ia, ib);
        let [cc, cd] = linear(ic, id);
        let [ce, cf] = self.apply([ie, if_]);
        Affine([ca, cb, cc, cd, ce, cf])
    }
}

/// Where a picture sits: the transforms of the clips it is seen through, the outermost first
/// and its own clip's last. Each nested project's whole frame is placed in the frame around it
/// as a picture is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Framing(pub Vec<Transform>);

impl Framing {
    /// Where a picture of `picture` pixels goes in a frame of `frame` pixels. Nested projects
    /// are taken to have the frame's shape.
    pub fn corners(&self, frame: (u32, u32), picture: (u32, u32)) -> Affine {
        let mut transforms = self.0.iter().rev();
        let Some(own) = transforms.next() else {
            return Transform::IDENTITY.corners(frame, picture);
        };
        transforms.fold(own.corners(frame, picture), |placed, outer| {
            outer.corners(frame, frame).after(&placed)
        })
    }

    /// How much of the picture is seen: what all its clips let through.
    pub fn opacity(&self) -> f32 {
        self.0.iter().map(|transform| transform.opacity).product()
    }

    pub fn is_identity(&self) -> bool {
        self.0.iter().all(Transform::is_identity)
    }

    /// The picture's own clip's transform.
    pub fn own(&self) -> Transform {
        self.0.last().copied().unwrap_or(Transform::IDENTITY)
    }
}

/// A transform a clip reaches at one of its media's frames; between two keyframes it goes
/// from one to the other.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Keyframe {
    /// The frame of the clip's media, counted in timeline frames from its first, as
    /// `Clip::source_start` is: trimming or cutting the clip leaves it where it is.
    pub at: i64,
    pub transform: Transform,
}

/// The transform at media frame `at`, from `keyframes` (in order, none at the same frame); the
/// first one's before them all and the last one's after. `base` with no keyframes.
pub fn transform_at(base: Transform, keyframes: &[Keyframe], at: i64) -> Transform {
    let after = keyframes.partition_point(|keyframe| keyframe.at <= at);
    match (after.checked_sub(1).map(|index| keyframes[index]), keyframes.get(after)) {
        (None, None) => base,
        (Some(before), None) => before.transform,
        (None, Some(next)) => next.transform,
        (Some(before), Some(next)) => {
            let share = (at - before.at) as f32 / (next.at - before.at).max(1) as f32;
            before.transform.toward(&next.transform, share)
        }
    }
}

pub(crate) fn keyframes_value(keyframes: &[Keyframe]) -> Value {
    Value::Array(
        keyframes
            .iter()
            .map(|keyframe| json!({ "at": keyframe.at, "transform": keyframe.transform.value() }))
            .collect(),
    )
}

/// Keyframes as kept in a file, put in order; those without a frame are left out.
pub(crate) fn keyframes_from_value(value: &Value) -> Vec<Keyframe> {
    let mut keyframes: Vec<Keyframe> = value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|keyframe| {
            Some(Keyframe {
                at: keyframe.get("at")?.as_i64()?,
                transform: keyframe
                    .get("transform")
                    .map_or(Transform::IDENTITY, Transform::from_value),
            })
        })
        .collect();
    keyframes.sort_by_key(|keyframe| keyframe.at);
    keyframes.dedup_by_key(|keyframe| keyframe.at);
    keyframes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_near(actual: [f32; 2], expected: [f32; 2]) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-4, "{actual} is not {expected}");
        }
    }

    #[test]
    fn pictures_are_fitted_scaled_turned_and_moved_in_the_frame() {
        let frame = (1920, 1080);
        // As it comes: corner to corner.
        let plain = Transform::IDENTITY.corners(frame, frame);
        assert_near(plain.apply([1.0, 1.0]), [1.0, 1.0]);
        // A tall picture fits the frame's height, in the middle.
        let tall = Transform::IDENTITY.corners(frame, (1080, 1920));
        assert_near(tall.apply([1.0, 1.0]), [1080.0 / 1920.0 * 1080.0 / 1920.0, 1.0]);
        // Half as big, a quarter of the width to the right, a quarter of the height down.
        let moved = Transform {
            x: 0.25,
            y: 0.25,
            scale: 0.5,
            ..Transform::IDENTITY
        };
        let moved = moved.corners(frame, frame);
        assert_near(moved.apply([1.0, 1.0]), [1.0, 0.0]);
        assert_near(moved.apply([-1.0, -1.0]), [0.0, -1.0]);
        // A quarter turn clockwise: the top right corner goes to the bottom right, keeping its
        // distance from the centre in pixels.
        let turned = Transform {
            rotation: 90.0,
            ..Transform::IDENTITY
        };
        let turned = turned.corners(frame, frame);
        let aspect = 1920.0 / 1080.0;
        assert_near(turned.apply([1.0, 1.0]), [1.0 / aspect, -aspect]);
    }

    #[test]
    fn a_slanted_picture_leans_by_its_skew() {
        let frame = (1000, 1000);
        // Its top goes right by as much as half its height, at 45 degrees; its bottom left.
        let leaning = Transform {
            skew_x: 45.0,
            ..Transform::IDENTITY
        };
        let leaning = leaning.corners(frame, frame);
        assert_near(leaning.apply([0.0, 1.0]), [1.0, 1.0]);
        assert_near(leaning.apply([0.0, -1.0]), [-1.0, -1.0]);
        assert_near(leaning.apply([1.0, 0.0]), [1.0, 0.0]);
        // Its right side goes up.
        let rising = Transform {
            skew_y: 45.0,
            ..Transform::IDENTITY
        };
        assert_near(rising.corners(frame, frame).apply([1.0, 0.0]), [1.0, 1.0]);
        assert!(!Transform { skew_x: 80.0, ..Transform::IDENTITY }.is_valid());
    }

    #[test]
    fn placements_nest() {
        let frame = (1920, 1080);
        let outer = Transform {
            x: 0.25,
            scale: 0.5,
            opacity: 0.5,
            ..Transform::IDENTITY
        };
        let inner = Transform {
            x: 0.5,
            scale: 0.5,
            opacity: 0.8,
            ..Transform::IDENTITY
        };
        let placement = Framing(vec![outer, inner]);
        // The inner picture's centre is half a frame right in the nested project; that project
        // is half as big and a quarter of the width right: a half plus a quarter of the width.
        let corners = placement.corners(frame, frame);
        assert_near(corners.apply([0.0, 0.0]), [1.0, 0.0]);
        assert_near(corners.apply([1.0, 1.0]), [1.25, 0.25]);
        assert!((placement.opacity() - 0.4).abs() < 1e-6);
        assert_eq!(placement.own(), inner);
        assert!(Framing(vec![Transform::IDENTITY]).is_identity());
    }

    #[test]
    fn keyframes_lead_from_one_transform_to_the_next() {
        let small = Transform {
            scale: 0.5,
            ..Transform::IDENTITY
        };
        let big = Transform {
            scale: 1.5,
            opacity: 0.0,
            ..Transform::IDENTITY
        };
        let keyframes = [
            Keyframe {
                at: 10,
                transform: small,
            },
            Keyframe {
                at: 20,
                transform: big,
            },
        ];
        assert_eq!(transform_at(Transform::IDENTITY, &[], 5), Transform::IDENTITY);
        assert_eq!(transform_at(Transform::IDENTITY, &keyframes, 0), small);
        assert_eq!(transform_at(Transform::IDENTITY, &keyframes, 10), small);
        let middle = transform_at(Transform::IDENTITY, &keyframes, 15);
        assert!((middle.scale - 1.0).abs() < 1e-6 && (middle.opacity - 0.5).abs() < 1e-6);
        assert_eq!(transform_at(Transform::IDENTITY, &keyframes, 99), big);
        let kept = keyframes_from_value(&keyframes_value(&keyframes));
        assert_eq!(kept, keyframes);
    }

    #[test]
    fn a_transform_comes_back_from_its_file_form() {
        let transform = Transform {
            x: -0.1,
            y: 0.2,
            scale: 1.5,
            rotation: -30.0,
            skew_x: 10.0,
            skew_y: -5.0,
            opacity: 0.75,
        };
        assert_eq!(Transform::from_value(&transform.value()), transform);
        let broken = json!({ "scale": 0.0 });
        assert_eq!(Transform::from_value(&broken), Transform::IDENTITY);
    }
}
