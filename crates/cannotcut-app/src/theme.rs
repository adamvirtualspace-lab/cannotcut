//! How cannotcut looks: its colours, square panels, and buttons that answer the pointer.
//!
//! Panels, cards and clips have square corners; only buttons are rounded. A button the
//! pointer is over is brighter and outlined in dark grey. One being pressed is darker and
//! outlined in white, and its shading turns over, light at the bottom instead of the top, so
//! that it looks pushed in; its words stay as they were.

use std::f32::consts::PI;

use iced::gradient::Linear;
use iced::theme::Palette;
use iced::widget::button::{self, Button};
use iced::widget::container;
use iced::{Background, Border, Color, Element, Shadow, Theme};

/// Behind everything.
pub const WINDOW: Color = Color::from_rgb8(0x12, 0x12, 0x14);
/// A panel's face.
pub const PANEL: Color = Color::from_rgb8(0x1d, 0x1d, 0x20);
/// Bars along a panel's top, and cards over the window.
pub const RAISED: Color = Color::from_rgb8(0x26, 0x26, 0x2a);
/// Lines between parts of a panel.
pub const LINE: Color = Color::from_rgb8(0x34, 0x34, 0x3a);
/// The line in a divider between panels that the pointer is over, to drag.
pub const DIVIDER_HOVER: Color = Color::from_rgb8(0x6a, 0x6a, 0x72);
pub const TEXT: Color = Color::from_rgb8(0xe8, 0xe8, 0xeb);
/// Words that say less: hints, readouts, labels of what cannot be used now.
pub const TEXT_DIM: Color = Color::from_rgb8(0x8e, 0x8e, 0x96);
/// What is chosen or switched on, and the main way on (Export).
pub const ACCENT: Color = Color::from_rgb8(0x1f, 0xc8, 0x8c);
/// Words on the accent.
pub const ON_ACCENT: Color = Color::from_rgb8(0x08, 0x1c, 0x14);
pub const DANGER: Color = Color::from_rgb8(0xe0, 0x4b, 0x4f);
/// Video clips on the timeline.
pub const VIDEO_CLIP: Color = Color::from_rgb8(0x2a, 0x7d, 0xb8);
/// Audio clips on the timeline.
pub const AUDIO_CLIP: Color = Color::from_rgb8(0x2f, 0x9e, 0x6a);
/// A face for ordinary buttons.
const BUTTON: Color = Color::from_rgb8(0x36, 0x36, 0x3c);
/// Around a button the pointer is over.
const HOVER_OUTLINE: Color = Color::from_rgb8(0x5c, 0x5c, 0x64);
/// Around a button being pressed.
const PRESS_OUTLINE: Color = Color::WHITE;
const OUTLINE_WIDTH: f32 = 1.0;
pub const BUTTON_RADIUS: f32 = 4.0;
/// How much lighter a button's top is than its face, and its bottom darker: a little shading,
/// as if lit from above.
const SHADE: f32 = 0.07;
/// How much brighter a button is under the pointer.
const BRIGHTER: f32 = 0.12;
/// How much darker a button is while pressed.
const DARKER: f32 = 0.22;
/// How much of the panel shows through a button that cannot be used now.
const DISABLED_FADE: f32 = 0.55;

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
        button::Status::Active => (downwards(top, bottom), outlined(lighter(face, SHADE * 2.0))),
        button::Status::Hovered => (
            downwards(lighter(top, BRIGHTER), lighter(bottom, BRIGHTER)),
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
                border: outlined(faded),
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
    shaded(BUTTON, TEXT, status)
}

/// The main way on: Export, Save in a dialog.
pub fn primary(_: &Theme, status: button::Status) -> button::Style {
    shaded(ACCENT, ON_ACCENT, status)
}

/// One that loses work: Don't save.
pub fn danger(_: &Theme, status: button::Status) -> button::Style {
    shaded(DANGER, Color::WHITE, status)
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
            Some(Background::Color(Color { a: 0.12, ..Color::WHITE })),
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
