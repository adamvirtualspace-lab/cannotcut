//! Text clips: words laid over the picture, made by the editor rather than read from a file.

use serde_json::{Value, json};

/// The faces a title can be set in, all shipped with the editor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TitleFont {
    #[default]
    Oswald,
    Antonio,
    AntonSc,
}

impl TitleFont {
    pub const ALL: [Self; 3] = [Self::Oswald, Self::Antonio, Self::AntonSc];

    /// The face's name, as shown.
    pub fn name(self) -> &'static str {
        match self {
            Self::Oswald => "Oswald",
            Self::Antonio => "Antonio",
            Self::AntonSc => "Anton SC",
        }
    }

    /// How a project file names the face.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Oswald => "oswald",
            Self::Antonio => "antonio",
            Self::AntonSc => "anton_sc",
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|font| font.key() == key)
    }
}

/// Words to lay over the picture: one line or several, centred in the frame until the clip's
/// transform moves them.
#[derive(Clone, Debug, PartialEq)]
pub struct Title {
    pub text: String,
    pub font: TitleFont,
    /// How tall a line of it is, as a share of the frame's height.
    pub size: f32,
    /// Red, green, blue, and how opaque, 0 to 255.
    pub color: [u8; 4],
}

impl Title {
    /// The size a title starts at.
    pub const DEFAULT_SIZE: f32 = 0.12;
    /// The smallest and largest sizes it takes.
    pub const SIZES: std::ops::RangeInclusive<f32> = 0.02..=0.5;

    /// White words of the default size and face.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            font: TitleFont::default(),
            size: Self::DEFAULT_SIZE,
            color: [255, 255, 255, 255],
        }
    }

    /// A short name for the clip it is on: its first line.
    pub fn label(&self) -> String {
        let first = self.text.lines().find(|line| !line.trim().is_empty());
        first.unwrap_or("Text").trim().to_string()
    }

    /// As a generator's parameters in a project file.
    pub(crate) fn value(&self) -> Value {
        let [red, green, blue, alpha] = self.color;
        json!({
            "text": self.text,
            "font": self.font.key(),
            "size": self.size,
            "color": format!("#{red:02x}{green:02x}{blue:02x}{alpha:02x}"),
        })
    }

    /// From a generator's parameters; whatever is missing or unusable takes its default.
    pub(crate) fn from_value(value: &Value) -> Self {
        let mut title = Self::new(value.get("text").and_then(Value::as_str).unwrap_or_default());
        if let Some(font) = value.get("font").and_then(Value::as_str).and_then(TitleFont::from_key)
        {
            title.font = font;
        }
        if let Some(size) = value.get("size").and_then(Value::as_f64) {
            let size = size as f32;
            if size.is_finite() {
                title.size = size.clamp(*Self::SIZES.start(), *Self::SIZES.end());
            }
        }
        if let Some(color) = value.get("color").and_then(Value::as_str).and_then(parse_color) {
            title.color = color;
        }
        title
    }
}

/// `#rrggbb` or `#rrggbbaa`.
fn parse_color(text: &str) -> Option<[u8; 4]> {
    let digits = text.strip_prefix('#')?;
    if !matches!(digits.len(), 6 | 8) || !digits.is_ascii() {
        return None;
    }
    let byte = |index: usize| u8::from_str_radix(&digits[index * 2..index * 2 + 2], 16).ok();
    let alpha = if digits.len() == 8 { byte(3)? } else { 255 };
    Some([byte(0)?, byte(1)?, byte(2)?, alpha])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_survives_a_project_file() {
        let title = Title {
            text: "Hello\nthere".to_string(),
            font: TitleFont::AntonSc,
            size: 0.2,
            color: [255, 128, 0, 200],
        };
        assert_eq!(Title::from_value(&title.value()), title);
    }

    #[test]
    fn what_a_file_gets_wrong_takes_the_default() {
        let read = Title::from_value(&json!({ "text": "x", "font": "comic", "size": 9.0,
            "color": "red" }));
        assert_eq!(read.font, TitleFont::Oswald);
        assert_eq!(read.size, 0.5);
        assert_eq!(read.color, [255, 255, 255, 255]);
        assert_eq!(parse_color("#00ff00"), Some([0, 255, 0, 255]));
    }

    #[test]
    fn a_title_is_named_by_its_first_line() {
        assert_eq!(Title::new("\n  Big news \nmore").label(), "Big news");
        assert_eq!(Title::new("").label(), "Text");
    }
}
