//! A clip's look: its colour (brightness, contrast, saturation, warmth, tint) and its filters
//! (blur, sharpen, vignette). The viewer draws it on the GPU and an export on the CPU, both from
//! the sums here, worked on the picture's own Y'CbCr values: luma and chroma 0 to 1, chroma
//! centred on a half.

/// How a clip's picture is changed before it is laid over the tracks below. Neutral leaves it
/// as it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grade {
    /// Lighter or darker: -1 to 1, 0 as recorded.
    pub brightness: f32,
    /// How far light and dark are apart: 0 (flat grey) to 2, 1 as recorded.
    pub contrast: f32,
    /// How strong the colours are: 0 (black and white) to 2, 1 as recorded.
    pub saturation: f32,
    /// Warmer (towards orange) or cooler (towards blue): -1 to 1, 0 as recorded.
    pub temperature: f32,
    /// Towards magenta or towards green: -1 to 1, 0 as recorded.
    pub tint: f32,
    /// How soft the picture is: 0 to 1, 0 sharp as recorded.
    pub blur: f32,
    /// How crisp its edges are made: 0 to 1, 0 as recorded.
    pub sharpen: f32,
    /// How dark its corners go: 0 to 1, 0 not at all.
    pub vignette: f32,
}

impl Default for Grade {
    fn default() -> Self {
        Self::NEUTRAL
    }
}

/// How far a full turn of brightness moves luma.
const BRIGHTNESS_REACH: f32 = 0.3;
/// How far a full turn of warmth or tint moves each chroma channel.
const TEMPERATURE_REACH: f32 = 0.08;
const TINT_REACH: f32 = 0.06;
/// The widest blur, as a share of the picture's height (a radius).
const BLUR_REACH: f32 = 0.02;
/// How strongly a full sharpen pushes luma away from its surroundings.
const SHARPEN_REACH: f32 = 1.5;
/// A vignette starts darkening this far out from the middle (1 is an edge's middle), and is
/// at its darkest this far out.
const VIGNETTE_FROM: f32 = 0.35;
const VIGNETTE_TO: f32 = 1.35;

impl Grade {
    /// As recorded.
    pub const NEUTRAL: Grade = Grade {
        brightness: 0.0,
        contrast: 1.0,
        saturation: 1.0,
        temperature: 0.0,
        tint: 0.0,
        blur: 0.0,
        sharpen: 0.0,
        vignette: 0.0,
    };

    pub fn is_neutral(&self) -> bool {
        *self == Self::NEUTRAL
    }

    /// The settings, each with its name, its range and its neutral value, to show and to change.
    pub const SETTINGS: [Setting; 8] = [
        Setting::new("Brightness", -1.0, 1.0, 0.0, |grade| &mut grade.brightness),
        Setting::new("Contrast", 0.0, 2.0, 1.0, |grade| &mut grade.contrast),
        Setting::new("Saturation", 0.0, 2.0, 1.0, |grade| &mut grade.saturation),
        Setting::new("Warmth", -1.0, 1.0, 0.0, |grade| &mut grade.temperature),
        Setting::new("Tint", -1.0, 1.0, 0.0, |grade| &mut grade.tint),
        Setting::new("Blur", 0.0, 1.0, 0.0, |grade| &mut grade.blur),
        Setting::new("Sharpen", 0.0, 1.0, 0.0, |grade| &mut grade.sharpen),
        Setting::new("Vignette", 0.0, 1.0, 0.0, |grade| &mut grade.vignette),
    ];

    /// Whether every setting is within its range.
    pub fn is_valid(&self) -> bool {
        let mut grade = *self;
        Self::SETTINGS.iter().all(|setting| {
            let value = *(setting.field)(&mut grade);
            value.is_finite() && (setting.min..=setting.max).contains(&value)
        })
    }

    /// A pixel's luma and chroma (`y`, `cb`, `cr`, 0 to 1) with the colour settings applied.
    pub fn color(&self, y: f32, cb: f32, cr: f32) -> (f32, f32, f32) {
        let y = (y - 0.5) * self.contrast + 0.5 + self.brightness * BRIGHTNESS_REACH;
        let (mut blue, mut red) = ((cb - 0.5) * self.saturation, (cr - 0.5) * self.saturation);
        // Warmer: less blue, more red. Towards magenta: more of both.
        blue += -self.temperature * TEMPERATURE_REACH + self.tint * TINT_REACH;
        red += self.temperature * TEMPERATURE_REACH + self.tint * TINT_REACH;
        (
            y.clamp(0.0, 1.0),
            (blue + 0.5).clamp(0.0, 1.0),
            (red + 0.5).clamp(0.0, 1.0),
        )
    }

    /// How much of a pixel is left by the vignette, 0 to 1, at `x`, `y` across the picture (-1 to
    /// 1 each way, from its middle).
    pub fn vignette_at(&self, x: f32, y: f32) -> f32 {
        if self.vignette <= 0.0 {
            return 1.0;
        }
        let out = (x * x + y * y).sqrt();
        let t = ((out - VIGNETTE_FROM) / (VIGNETTE_TO - VIGNETTE_FROM)).clamp(0.0, 1.0);
        1.0 - self.vignette * t * t * (3.0 - 2.0 * t)
    }

    /// The blur's radius, in pixels of a picture `height` tall.
    pub fn blur_radius(&self, height: u32) -> f32 {
        self.blur * BLUR_REACH * height as f32
    }

    /// Luma `y` sharpened against `around`, the luma of its surroundings.
    pub fn sharpened(&self, y: f32, around: f32) -> f32 {
        (y + self.sharpen * SHARPEN_REACH * (y - around)).clamp(0.0, 1.0)
    }

    /// The numbers the GPU viewer takes, in this order.
    pub fn uniforms(&self) -> [f32; 8] {
        [
            self.contrast,
            self.brightness * BRIGHTNESS_REACH,
            self.saturation,
            -self.temperature * TEMPERATURE_REACH + self.tint * TINT_REACH,
            self.temperature * TEMPERATURE_REACH + self.tint * TINT_REACH,
            self.sharpen * SHARPEN_REACH,
            self.vignette,
            self.blur * BLUR_REACH,
        ]
    }
}

/// One setting of a [`Grade`]: its name, range and neutral value, and where it is kept.
#[derive(Clone, Copy)]
pub struct Setting {
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub neutral: f32,
    pub field: fn(&mut Grade) -> &mut f32,
}

impl Setting {
    const fn new(
        name: &'static str,
        min: f32,
        max: f32,
        neutral: f32,
        field: fn(&mut Grade) -> &mut f32,
    ) -> Self {
        Self {
            name,
            min,
            max,
            neutral,
            field,
        }
    }

    /// Its value in `grade`.
    pub fn get(&self, grade: &Grade) -> f32 {
        let mut grade = *grade;
        *(self.field)(&mut grade)
    }

    /// `grade` with this setting at `value`, kept within its range.
    pub fn set(&self, grade: &Grade, value: f32) -> Grade {
        let mut grade = *grade;
        *(self.field)(&mut grade) = value.clamp(self.min, self.max);
        grade
    }
}

/// Ready-made looks, by name, to start from.
pub const LOOKS: [(&str, Grade); 8] = [
    ("Original", Grade::NEUTRAL),
    (
        "Black & white",
        Grade {
            saturation: 0.0,
            contrast: 1.15,
            ..Grade::NEUTRAL
        },
    ),
    (
        "Warm",
        Grade {
            temperature: 0.5,
            saturation: 1.1,
            ..Grade::NEUTRAL
        },
    ),
    (
        "Cool",
        Grade {
            temperature: -0.5,
            tint: -0.1,
            ..Grade::NEUTRAL
        },
    ),
    (
        "Vivid",
        Grade {
            saturation: 1.5,
            contrast: 1.15,
            sharpen: 0.3,
            ..Grade::NEUTRAL
        },
    ),
    (
        "Faded",
        Grade {
            contrast: 0.75,
            brightness: 0.12,
            saturation: 0.7,
            ..Grade::NEUTRAL
        },
    ),
    (
        "Film",
        Grade {
            contrast: 1.1,
            saturation: 0.85,
            temperature: 0.2,
            vignette: 0.45,
            ..Grade::NEUTRAL
        },
    ),
    (
        "Dream",
        Grade {
            blur: 0.25,
            brightness: 0.1,
            saturation: 1.15,
            vignette: 0.3,
            ..Grade::NEUTRAL
        },
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_leaves_a_pixel_as_it_is() {
        for (y, cb, cr) in [(0.2, 0.5, 0.5), (0.7, 0.3, 0.8), (1.0, 0.0, 1.0)] {
            let (a, b, c) = Grade::NEUTRAL.color(y, cb, cr);
            assert!((a - y).abs() < 1e-6 && (b - cb).abs() < 1e-6 && (c - cr).abs() < 1e-6);
        }
        assert_eq!(Grade::NEUTRAL.vignette_at(1.0, 1.0), 1.0);
        assert_eq!(Grade::NEUTRAL.sharpened(0.4, 0.2), 0.4);
    }

    #[test]
    fn each_setting_does_what_it_says() {
        let grey = (0.5, 0.5, 0.5);
        let set = |name: &str, value: f32| {
            let setting = Grade::SETTINGS.iter().find(|s| s.name == name).unwrap();
            setting.set(&Grade::NEUTRAL, value)
        };
        assert!(set("Brightness", 0.5).color(grey.0, grey.1, grey.2).0 > 0.6);
        let (_, cb, cr) = set("Saturation", 0.0).color(0.5, 0.2, 0.9);
        assert_eq!((cb, cr), (0.5, 0.5), "black and white");
        let (_, cb, cr) = set("Warmth", 1.0).color(grey.0, grey.1, grey.2);
        assert!(cb < 0.5 && cr > 0.5, "warmer: less blue, more red");
        assert!(set("Contrast", 2.0).color(0.75, 0.5, 0.5).0 > 0.95);
        let vignette = set("Vignette", 1.0);
        assert_eq!(vignette.vignette_at(0.0, 0.0), 1.0, "the middle untouched");
        assert!(vignette.vignette_at(1.0, 1.0) < 0.2, "corners dark");
        assert!(set("Sharpen", 1.0).sharpened(0.6, 0.5) > 0.6);
        assert!(set("Blur", 1.0).blur_radius(1080) > 20.0);
        // Out of range is kept in it.
        assert_eq!(set("Contrast", 9.0).contrast, 2.0);
    }

    #[test]
    fn every_look_is_valid() {
        for (name, look) in LOOKS {
            assert!(look.is_valid(), "{name}");
        }
        assert!(!Grade { contrast: -1.0, ..Grade::NEUTRAL }.is_valid());
    }
}
