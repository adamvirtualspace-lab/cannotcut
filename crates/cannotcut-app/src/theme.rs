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

use std::f32::consts::PI;

use iced::gradient::Linear;
use iced::theme::Palette;
use iced::widget::button::{self, Button};
use iced::widget::text::{IntoFragment, Text};
use iced::widget::{container, pane_grid, text};
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
/// What is chosen or switched on, and the main way on (Export).
pub const ACCENT: Color = Color::from_rgb8(0x1f, 0xc8, 0x8c);
/// Words on the accent.
pub const ON_ACCENT: Color = Color::from_rgb8(0x08, 0x1c, 0x14);
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

/// Words on buttons with a face of their own, and dialog titles: Anton SC, Anton with small
/// capitals for its lower-case letters.
pub const DISPLAY: Font = Font::with_name("Anton SC");
/// Small words: second lines, hints, notes, readouts. Antonio.
pub const SMALL: Font = Font::with_name("Antonio");
/// Every other word: names, values, timecodes, the timeline. Oswald, the app's default font,
/// so that it looks the same on every system (without it, Windows shows Segoe UI).
pub const BODY: Font = Font::with_name("Oswald");
/// The fonts above, for the app to load at start (see `fonts/README.md`).
pub const FONTS: [&[u8]; 3] = [
    include_bytes!("../fonts/AntonSC-Regular.ttf"),
    include_bytes!("../fonts/Antonio-Regular.ttf"),
    include_bytes!("../fonts/Oswald-Regular.ttf"),
];

/// Words for a button with a face of its own, in [`DISPLAY`].
pub fn display<'a>(words: impl IntoFragment<'a>) -> Text<'a> {
    text(words).font(DISPLAY)
}

/// The theme of the whole app: the colours above, for the widgets drawn by iced.
pub fn theme() -> Theme {
    Theme::custom(
        "cannotcut",
        Palette {
            background: PANEL,
            text: TEXT,
            primary: ACCENT,
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

/// A button, ordinary until given another style.
pub fn button<'a, Message>(content: impl Into<Element<'a, Message>>) -> Button<'a, Message> {
    iced::widget::button(content).style(normal)
}

/// An ordinary button.
pub fn normal(_: &Theme, status: button::Status) -> button::Style {
    shaded(BUTTON, ON_BUTTON, status)
}

/// The main way on: Export, Save in a dialog.
pub fn primary(_: &Theme, status: button::Status) -> button::Style {
    shaded(ACCENT, ON_ACCENT, status)
}

/// One that loses work: Don't save.
pub fn danger(_: &Theme, status: button::Status) -> button::Style {
    shaded(DANGER, ON_DANGER, status)
}

/// A switch that is on: Selecting, a muted track.
pub fn on(_: &Theme, status: button::Status) -> button::Style {
    shaded(ACCENT, ON_ACCENT, status)
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
            style.text_color = ACCENT;
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
            background: Background::Color(Color { a: 0.25, ..ACCENT }),
            border: Border {
                color: ACCENT,
                width: 2.0,
                radius: 0.0.into(),
            },
        },
        picked_split: pane_grid::Line {
            color: ACCENT,
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
