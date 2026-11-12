//! How cannotcut looks: its colours, square panels, and buttons that answer the pointer.
//!
//! Panels, cards and clips have square corners; only buttons are rounded. A button the
//! pointer is over is much brighter and outlined in black. One being pressed is darker and
//! outlined in white, and its shading turns over, light at the bottom instead of the top, so
//! that it looks pushed in; its words stay as they were.
//!
//! The outline goes around a button's face, not over its edge: every button keeps a clear
//! ring of [`OUTLINE_WIDTH`] around its face, and the outline only colours that ring in. So
//! the face is the same size whatever the pointer does, and nothing around it moves.
//!
//! The accent colour and the fonts are the user's to choose (see `appearance`): they are read
//! from here as the window is drawn, so a change shows at once.

use std::f32::consts::PI;
use std::sync::RwLock;

use iced::gradient::Linear;
use iced::theme::Palette;
use iced::widget::button::{self};
use iced::widget::text::{IntoFragment, Text};
use iced::widget::{container, pane_grid, text};
use iced::font::Weight;
use iced::{Background, Border, Color, Element, Font, Shadow, Theme};

/// Behind everything.
pub const WINDOW: Color = Color::from_rgb8(0x10, 0x10, 0x14);
/// A panel's face.
pub const PANEL: Color = Color::from_rgb8(0x2d, 0x2e, 0x37);
/// Bars along a panel's top, and cards over the window.
pub const RAISED: Color = Color::from_rgb8(0x49, 0x4a, 0x53);
/// Lines between parts of a panel.
pub const LINE: Color = Color::from_rgb8(0x5c, 0x5d, 0x68);
/// The line in a divider between panels that the pointer is over, to drag.
pub const DIVIDER_HOVER: Color = Color::from_rgb8(0x84, 0x85, 0x90);
pub const TEXT: Color = Color::from_rgb8(0xe8, 0xe8, 0xeb);
/// Words that say less: hints, readouts, labels of what cannot be used now.
pub const TEXT_DIM: Color = Color::from_rgb8(0xa4, 0xa5, 0xae);
/// What is chosen or switched on, and the main way on (Export), unless another is chosen:
/// mint.
pub const DEFAULT_ACCENT: Color = Color::from_rgb8(0x1f, 0xc8, 0x8c);
/// The accent colours offered, by name; the first is the default.
pub const ACCENTS: [(&str, Color); 8] = [
    ("Mint", DEFAULT_ACCENT),
    ("Sky", Color::from_rgb8(0x3b, 0x9e, 0xf0)),
    ("Violet", Color::from_rgb8(0x9a, 0x6b, 0xff)),
    ("Pink", Color::from_rgb8(0xf0, 0x60, 0x9e)),
    ("Coral", Color::from_rgb8(0xff, 0x7a, 0x59)),
    ("Orange", Color::from_rgb8(0xf5, 0x9a, 0x23)),
    ("Yellow", Color::from_rgb8(0xf2, 0xc2, 0x30)),
    ("Lime", Color::from_rgb8(0x9b, 0xd4, 0x3b)),
];
pub const DANGER: Color = Color::from_rgb8(0xe0, 0x4b, 0x4f);
/// Words on the danger colour: dark, as on the accent, since both are bright.
pub const ON_DANGER: Color = Color::from_rgb8(0x1f, 0x08, 0x09);
/// The timeline under its lanes, and its ruler.
pub const TRACKS: Color = Color::from_rgb8(0x6a, 0x6a, 0x75);
/// Video clips on the timeline.
pub const VIDEO_CLIP: Color = Color::from_rgb8(0x2a, 0x7d, 0xb8);
/// Audio clips on the timeline.
pub const AUDIO_CLIP: Color = Color::from_rgb8(0x2f, 0x9e, 0x6a);
/// A face for ordinary buttons.
const BUTTON: Color = Color::from_rgb8(0x8c, 0x8d, 0x98);
/// Words on ordinary buttons: dark, as on every button with a face of its own, since the faces
/// are bright.
pub const ON_BUTTON: Color = Color::from_rgb8(0x15, 0x16, 0x1b);
/// Around a button the pointer is over.
const HOVER_OUTLINE: Color = Color::BLACK;
/// Around a button being pressed.
const PRESS_OUTLINE: Color = Color::WHITE;
const OUTLINE_WIDTH: f32 = 1.5;
pub const BUTTON_RADIUS: f32 = 4.0;
/// How much lighter a button's top is than its face, and its bottom darker: a little shading,
/// as if lit from above.
const SHADE: f32 = 0.07;
/// How much brighter a button is under the pointer: its colour is lit up by this much...
const BRIGHTER: f32 = 0.3;
/// ...and then taken this far towards white, so that a dark grey button shows it too.
const WHITER: f32 = 0.08;
/// How much darker a button is while pressed.
const DARKER: f32 = 0.22;
/// How much of the panel shows through a button that cannot be used now.
const DISABLED_FADE: f32 = 0.55;

/// The fonts the app carries, for it to load at start (see `fonts/README.md`). Inter and
/// Roboto are variable fonts: one file draws every weight.
pub const FONTS: [&[u8]; 5] = [
    include_bytes!("../fonts/AntonSC-Regular.ttf"),
    include_bytes!("../fonts/Antonio-Regular.ttf"),
    include_bytes!("../fonts/Oswald-Regular.ttf"),
    include_bytes!("../fonts/Inter-Variable.ttf"),
    include_bytes!("../fonts/Roboto-Variable.ttf"),
];

/// The fonts the words of the UI can be shown in. Each gives three: for words on buttons and
/// dialog titles, for small words (hints, notes, readouts), and for every other word.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UiFont {
    /// Narrow, tall letters that fit a lot in: Anton SC on buttons (small capitals), Antonio for
    /// small words, Oswald for the rest. The same on every system.
    #[default]
    Condensed,
    /// Windows' own; elsewhere, the system's default font stands in.
    SegoeUi,
    Inter,
    Roboto,
}

impl UiFont {
    pub const ALL: [UiFont; 4] = [Self::Condensed, Self::SegoeUi, Self::Inter, Self::Roboto];

    pub fn key(self) -> &'static str {
        match self {
            Self::Condensed => "condensed",
            Self::SegoeUi => "segoe-ui",
            Self::Inter => "inter",
            Self::Roboto => "roboto",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|font| font.key() == key)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Condensed => "Condensed",
            Self::SegoeUi => "Segoe UI",
            Self::Inter => "Inter",
            Self::Roboto => "Roboto",
        }
    }

    /// What it is, under its name.
    pub fn about(self) -> &'static str {
        match self {
            Self::Condensed => "Narrow: fits the most in",
            Self::SegoeUi => "Windows' own",
            Self::Inter => "Clear at small sizes",
            Self::Roboto => "Android's own",
        }
    }

    /// For every other word: names, values, timecodes, the timeline.
    pub fn body(self) -> Font {
        match self {
            Self::Condensed => Font::with_name("Oswald"),
            Self::SegoeUi => Font::with_name("Segoe UI"),
            Self::Inter => Font::with_name("Inter"),
            Self::Roboto => Font::with_name("Roboto"),
        }
    }

    /// For small words: second lines, hints, notes, readouts.
    pub fn small(self) -> Font {
        match self {
            Self::Condensed => Font::with_name("Antonio"),
            other => other.body(),
        }
    }

    /// For words on buttons with a face of their own, and dialog titles.
    pub fn display(self) -> Font {
        match self {
            Self::Condensed => Font::with_name("Anton SC"),
            other => Font {
                weight: Weight::Semibold,
                ..other.body()
            },
        }
    }

    /// How much wider its words are than `fit::text_width` reckons, which is for Segoe UI and
    /// errs on the wide side.
    pub fn width_factor(self) -> f32 {
        match self {
            Self::Inter => 1.08,
            _ => 1.0,
        }
    }
}

/// The sizes everything can be shown at, as a share of its own size.
pub const UI_SCALES: [f32; 6] = [0.8, 0.9, 1.0, 1.1, 1.25, 1.5];
/// The sizes words can be shown at, on top of that.
pub const TEXT_SCALES: [f32; 4] = [0.9, 1.0, 1.1, 1.25];
/// Words with no size of their own, before scaling: iced's own default.
const DEFAULT_TEXT_SIZE: f32 = 16.0;

/// How the UI looks, as chosen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub accent: Color,
    pub font: UiFont,
    /// How large everything is drawn: panels, buttons, icons and words alike. 1 is as made.
    pub ui_scale: f32,
    /// How large words are drawn, on top of `ui_scale`.
    pub text_scale: f32,
    /// Whether things move (see `motion`), or arrive at once.
    pub motion: bool,
}

impl Look {
    const START: Look = Look {
        accent: DEFAULT_ACCENT,
        font: UiFont::Condensed,
        ui_scale: 1.0,
        text_scale: 1.0,
        motion: true,
    };
}

impl Default for Look {
    fn default() -> Self {
        Self::START
    }
}

static LOOK: RwLock<Look> = RwLock::new(Look::START);

/// How the UI looks now.
pub fn look() -> Look {
    LOOK.read().map(|look| *look).unwrap_or_default()
}

/// Makes the UI look as `look` says, from the next time it is drawn.
pub fn set_look(look: Look) {
    if let Ok(mut now) = LOOK.write() {
        *now = look;
    }
}

/// A colour as `#rrggbb`.
pub fn hex(color: Color) -> String {
    let [red, green, blue, _] = color.into_rgba8();
    format!("#{red:02x}{green:02x}{blue:02x}")
}

/// A colour written as `#rrggbb` or `rrggbb` (or the short `#rgb`), in either case.
pub fn from_hex(written: &str) -> Option<Color> {
    let digits = written.trim().trim_start_matches('#');
    let digits: String = match digits.len() {
        3 => digits.chars().flat_map(|digit| [digit, digit]).collect(),
        6 => digits.to_string(),
        _ => return None,
    };
    let channel = |at: usize| u8::from_str_radix(digits.get(at..at + 2)?, 16).ok();
    Some(Color::from_rgb8(channel(0)?, channel(2)?, channel(4)?))
}

/// What is chosen or switched on, and the main way on (Export).
pub fn accent() -> Color {
    look().accent
}

/// Words on the accent: dark on a bright accent, light on a dark one, in its own hue.
pub fn on_accent() -> Color {
    ink_on(accent())
}

fn ink_on(face: Color) -> Color {
    // Relative luminance, from the sRGB channels made linear.
    let linear = |channel: f32| {
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * linear(face.r) + 0.7152 * linear(face.g) + 0.0722 * linear(face.b);
    // Where black and white words would read equally well against it.
    if luminance > 0.18 {
        darker(face, 0.86)
    } else {
        lighter(face, 0.92)
    }
}

/// Words on buttons with a face of their own, and dialog titles.
pub fn display_font() -> Font {
    look().font.display()
}

/// Small words: second lines, hints, notes, readouts.
pub fn small_font() -> Font {
    look().font.small()
}

/// Every other word.
pub fn body_font() -> Font {
    look().font.body()
}

/// How much wider words are than `fit::text_width` reckons, in the fonts and text size
/// chosen.
pub fn width_factor() -> f32 {
    let look = look();
    look.font.width_factor() * look.text_scale
}

/// How large everything is drawn, 1 being as made.
pub fn ui_scale() -> f32 {
    look().ui_scale
}

/// Words meant to be `size` pixels, at the text size chosen.
pub fn text_size(size: f32) -> f32 {
    size * look().text_scale
}

/// Words, in the font for every other word, at the text size chosen. Used in place of iced's
/// `text`, whose font and size are the ones the app started with.
pub fn ui_text<'a>(words: impl IntoFragment<'a>) -> Text<'a> {
    text(words).font(body_font()).size(text_size(DEFAULT_TEXT_SIZE))
}

/// Words for a button with a face of its own, in the font for those.
pub fn display<'a>(words: impl IntoFragment<'a>) -> Text<'a> {
    text(words).font(display_font()).size(text_size(DEFAULT_TEXT_SIZE))
}

/// The size of words with no size of their own, at the text size chosen: for widgets that
/// take it apart from their words.
pub fn default_text_size() -> f32 {
    text_size(DEFAULT_TEXT_SIZE)
}

/// The theme of the whole app: the colours above, for the widgets drawn by iced.
pub fn theme() -> Theme {
    Theme::custom(
        "cannotcut",
        Palette {
            background: PANEL,
            text: TEXT,
            primary: accent(),
            success: AUDIO_CLIP,
            warning: Color::from_rgb8(0xe0, 0xa1, 0x3c),
            danger: DANGER,
        },
    )
}

/// `color` towards white by `amount` (0 to 1).
fn lighter(color: Color, amount: f32) -> Color {
    mix(color, Color::WHITE, amount)
}

/// `color` lit up by [`BRIGHTER`], keeping its hue, then taken [`WHITER`] towards white.
fn brighter(color: Color) -> Color {
    let lit = |channel: f32| (channel * (1.0 + BRIGHTER)).min(1.0);
    lighter(Color { r: lit(color.r), g: lit(color.g), b: lit(color.b), ..color }, WHITER)
}

/// `color` towards black by `amount` (0 to 1).
fn darker(color: Color, amount: f32) -> Color {
    mix(color, Color::BLACK, amount)
}

fn mix(from: Color, to: Color, amount: f32) -> Color {
    Color {
        r: from.r + (to.r - from.r) * amount,
        g: from.g + (to.g - from.g) * amount,
        b: from.b + (to.b - from.b) * amount,
        a: from.a,
    }
}

/// Top to bottom.
fn downwards(top: Color, bottom: Color) -> Background {
    Background::Gradient(Linear::new(PI).add_stop(0.0, top).add_stop(1.0, bottom).into())
}

/// A button's outline, in `color`; transparent leaves the ring around the face clear (the
/// renderer mixes a border towards its colour, alpha and all).
fn outlined(color: Color) -> Border {
    Border {
        color,
        width: OUTLINE_WIDTH,
        radius: BUTTON_RADIUS.into(),
    }
}

/// A button with a face of `face` and words in `ink`, shaded, answering the pointer.
fn shaded(face: Color, ink: Color, status: button::Status) -> button::Style {
    let top = lighter(face, SHADE);
    let bottom = darker(face, SHADE);
    let (background, border) = match status {
        button::Status::Active => (downwards(top, bottom), outlined(Color::TRANSPARENT)),
        button::Status::Hovered => (
            downwards(brighter(top), brighter(bottom)),
            outlined(HOVER_OUTLINE),
        ),
        // Turned over: dark at the top, as a dip in the panel is.
        button::Status::Pressed => (
            downwards(darker(bottom, DARKER), darker(top, DARKER)),
            outlined(PRESS_OUTLINE),
        ),
        button::Status::Disabled => {
            let faded = mix(face, PANEL, DISABLED_FADE);
            return button::Style {
                background: Some(Background::Color(faded)),
                text_color: TEXT_DIM,
                border: outlined(Color::TRANSPARENT),
                shadow: Shadow::default(),
                snap: true,
            };
        }
    };
    button::Style {
        background: Some(background),
        text_color: ink,
        border,
        shadow: Shadow::default(),
        snap: true,
    }
}

/// A button, ordinary until given another style. It eases into its looks (see `motion`).
pub fn button<'a, Message>(
    content: impl Into<Element<'a, Message>>,
) -> crate::motion::Button<'a, Message> {
    crate::motion::Button::new(content)
}

/// An ordinary button.
pub fn normal(_: &Theme, status: button::Status) -> button::Style {
    shaded(BUTTON, ON_BUTTON, status)
}

/// The main way on: Export, Save in a dialog.
pub fn primary(_: &Theme, status: button::Status) -> button::Style {
    shaded(accent(), on_accent(), status)
}

/// One that loses work: Don't save.
pub fn danger(_: &Theme, status: button::Status) -> button::Style {
    shaded(DANGER, ON_DANGER, status)
}

/// A switch that is on: Selecting, a muted track.
pub fn on(_: &Theme, status: button::Status) -> button::Style {
    shaded(accent(), on_accent(), status)
}

/// A button with no face of its own until the pointer is over it: in the path, in menus, on
/// tools. Pressed, the place it sits on darkens under it.
pub fn quiet(_: &Theme, status: button::Status) -> button::Style {
    let (background, border, ink) = match status {
        button::Status::Active => (None, outlined(Color::TRANSPARENT), TEXT),
        button::Status::Hovered => (
            Some(Background::Color(Color { a: 0.18, ..Color::WHITE })),
            outlined(HOVER_OUTLINE),
            TEXT,
        ),
        button::Status::Pressed => (
            Some(Background::Color(Color { a: 0.35, ..Color::BLACK })),
            outlined(PRESS_OUTLINE),
            TEXT,
        ),
        button::Status::Disabled => (None, outlined(Color::TRANSPARENT), TEXT_DIM),
    };
    button::Style {
        background,
        text_color: ink,
        border,
        shadow: Shadow::default(),
        snap: true,
    }
}

/// A tab: words alone until the pointer is over it; the chosen one in the accent colour, on a
/// faint face.
pub fn tab(chosen: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let mut style = quiet(theme, status);
        if chosen {
            style.text_color = accent();
            if status == button::Status::Active {
                style.background = Some(Background::Color(Color { a: 0.06, ..Color::WHITE }));
            }
        }
        style
    }
}

/// Docked panels: where a panel being moved would go, in the accent colour, and the gaps
/// between panels as the dividers of a narrow window show them.
pub fn dock(_: &Theme) -> pane_grid::Style {
    pane_grid::Style {
        hovered_region: pane_grid::Highlight {
            background: Background::Color(Color { a: 0.25, ..accent() }),
            border: Border {
                color: accent(),
                width: 2.0,
                radius: 0.0.into(),
            },
        },
        picked_split: pane_grid::Line {
            color: accent(),
            width: 2.0,
        },
        hovered_split: pane_grid::Line {
            color: DIVIDER_HOVER,
            width: 2.0,
        },
    }
}

/// A bar along the top of a panel.
pub fn bar(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(RAISED)),
        text_color: Some(TEXT),
        ..container::Style::default()
    }
}

/// A square card over the window: menus, dialogs, hints.
pub fn card(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(RAISED)),
        text_color: Some(TEXT),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: 0.0.into(),
        },
        ..container::Style::default()
    }
}

/// A panel: square, on the window.
pub fn panel(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(PANEL)),
        text_color: Some(TEXT),
        ..container::Style::default()
    }
}

/// The window behind the panels.
pub fn window(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(WINDOW)),
        text_color: Some(TEXT),
        ..container::Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_on_the_accent_stand_out_from_it() {
        // Mint keeps the dark words it always had, near enough.
        let on_mint = ink_on(DEFAULT_ACCENT).into_rgba8();
        assert!(on_mint[1] < 0x30, "{on_mint:?}");
        for (name, face) in ACCENTS {
            let ink = ink_on(face);
            let gap = (face.r - ink.r).abs() + (face.g - ink.g).abs() + (face.b - ink.b).abs();
            assert!(gap > 1.2, "{name}: {gap}");
        }
        // A dark accent gets light words.
        assert!(ink_on(Color::from_rgb8(0x20, 0x20, 0x80)).r > 0.8);
    }

    #[test]
    fn colours_are_written_and_read_as_hex() {
        assert_eq!(hex(DEFAULT_ACCENT), "#1fc88c");
        assert_eq!(from_hex("#1FC88C"), Some(DEFAULT_ACCENT));
        assert_eq!(from_hex(" 1fc88c "), Some(DEFAULT_ACCENT));
        assert_eq!(from_hex("#fff"), Some(Color::WHITE));
        for wrong in ["", "#12345", "#12345g", "1fc88c00", "#"] {
            assert_eq!(from_hex(wrong), None, "{wrong}");
        }
    }

    #[test]
    fn every_ui_font_comes_back_from_its_key() {
        for font in UiFont::ALL {
            assert_eq!(UiFont::from_key(font.key()), Some(font));
        }
    }
}
