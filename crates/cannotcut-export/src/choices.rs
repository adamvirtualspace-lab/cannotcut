use std::fmt;

use cannotcut_media::AUDIO_BIT_RATE;
use cannotcut_timeline::Rate;

/// The most bits a second the pictures get, whatever the size and quality.
const MAX_BIT_RATE: f64 = 120_000_000.0;

/// How large the film's pictures are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Resolution {
    /// The size of the first clip.
    Original,
    /// This many pixels along the picture's shorter side, the longer side following the
    /// clips' shape: 1080 is 1920x1080 for a wide film and 1080x1920 for a tall one.
    Short(u32),
}

impl Resolution {
    pub const CHOICES: [Self; 6] = [
        Self::Original,
        Self::Short(480),
        Self::Short(720),
        Self::Short(1080),
        Self::Short(1440),
        Self::Short(2160),
    ];

    /// The film's size when its first clip is `source` in size. Sizes are even, as video
    /// encoders want them.
    pub fn film_size(self, source: (u32, u32)) -> (u32, u32) {
        let (width, height) = (source.0.max(2), source.1.max(2));
        let even = |side: f64| ((side / 2.0).round() as u32 * 2).max(2);
        match self {
            Self::Original => (width & !1, height & !1),
            Self::Short(short) if width >= height => {
                let long = f64::from(short) * f64::from(width) / f64::from(height);
                (even(long), short & !1)
            }
            Self::Short(short) => {
                let long = f64::from(short) * f64::from(height) / f64::from(width);
                (short & !1, even(long))
            }
        }
    }

    /// The name it is kept under in the settings.
    pub fn key(self) -> String {
        match self {
            Self::Original => "original".to_string(),
            Self::Short(short) => short.to_string(),
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "original" => Some(Self::Original),
            short => short.parse().ok().map(Self::Short),
        }
    }
}

impl fmt::Display for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Original => f.write_str("Original"),
            Self::Short(1440) => f.write_str("2K (1440p)"),
            Self::Short(2160) => f.write_str("4K (2160p)"),
            Self::Short(short) => write!(f, "{short}p"),
        }
    }
}

/// How many bits a second the pictures get: more is sharper where much moves, and a larger
/// file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Quality {
    Lower,
    Recommended,
    Higher,
}

impl Quality {
    pub const CHOICES: [Self; 3] = [Self::Lower, Self::Recommended, Self::Higher];

    /// Bits a pixel gets each frame. Recommended is about 9 Mbit/s for 1080p at 30 fps. On a
    /// recorded screen even Lower looks all but the same as the source; the difference shows
    /// where much moves.
    fn bits_per_pixel(self) -> f64 {
        match self {
            Self::Lower => 0.08,
            Self::Recommended => 0.15,
            Self::Higher => 0.25,
        }
    }

    /// The pictures' bit rate, in bits a second, for a film of `size` at `rate`.
    pub fn bit_rate(self, size: (u32, u32), rate: Rate) -> usize {
        let pixels = f64::from(size.0) * f64::from(size.1);
        (pixels * rate.as_f64() * self.bits_per_pixel()).min(MAX_BIT_RATE) as usize
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Lower => "lower",
            Self::Recommended => "recommended",
            Self::Higher => "higher",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::CHOICES.into_iter().find(|quality| quality.key() == key)
    }
}

impl fmt::Display for Quality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Lower => "Lower",
            Self::Recommended => "Recommended",
            Self::Higher => "Higher",
        })
    }
}

/// What the film is to be like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub resolution: Resolution,
    pub quality: Quality,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            resolution: Resolution::Short(1080),
            quality: Quality::Recommended,
        }
    }
}

impl Options {
    /// About how large the film can get, in bytes, when its first clip is `source` in size
    /// and it is `frames` frames at `rate` long. Pictures that change little, such as a
    /// recorded screen, come out smaller: the encoder needs fewer bits for them.
    pub fn estimated_bytes(self, source: (u32, u32), rate: Rate, frames: i64) -> u64 {
        let size = self.resolution.film_size(source);
        let seconds = frames.max(0) as f64 / rate.as_f64();
        let bits = (self.quality.bit_rate(size, rate) + AUDIO_BIT_RATE) as f64 * seconds;
        (bits / 8.0) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shorter_side_is_the_one_named() {
        let wide = (2560, 1440);
        assert_eq!(Resolution::Short(1080).film_size(wide), (1920, 1080));
        assert_eq!(Resolution::Short(720).film_size(wide), (1280, 720));
        assert_eq!(Resolution::Original.film_size(wide), (2560, 1440));
        // A phone held upright.
        assert_eq!(Resolution::Short(1080).film_size((720, 1280)), (1080, 1920));
        // An old 4:3 camera, and a size that needs evening.
        assert_eq!(Resolution::Short(480).film_size((1440, 1080)), (640, 480));
        assert_eq!(Resolution::Short(720).film_size((1001, 999)), (722, 720));
        assert_eq!(Resolution::Original.film_size((1001, 999)), (1000, 998));
    }

    #[test]
    fn choices_survive_being_kept_in_the_settings() {
        for resolution in Resolution::CHOICES {
            assert_eq!(Resolution::from_key(&resolution.key()), Some(resolution));
        }
        for quality in Quality::CHOICES {
            assert_eq!(Quality::from_key(quality.key()), Some(quality));
        }
        assert_eq!(Quality::from_key("best"), None);
    }

    #[test]
    fn more_quality_and_more_pixels_take_more_bits() {
        let rate = Rate::new(30, 1);
        let hd = Quality::Recommended.bit_rate((1920, 1080), rate);
        assert!((9_000_000..9_500_000).contains(&hd), "{hd}");
        assert!(Quality::Lower.bit_rate((1920, 1080), rate) < hd);
        assert!(Quality::Higher.bit_rate((1920, 1080), rate) > hd);
        let options = Options::default();
        // A minute of 1080p: about 70 MB.
        let bytes = options.estimated_bytes((1920, 1080), rate, 1800);
        assert!((60_000_000..80_000_000).contains(&bytes), "{bytes}");
    }
}
