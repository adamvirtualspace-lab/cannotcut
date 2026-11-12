//! How the app looks, as the user chooses: the accent colour (what is chosen or switched on,
//! and the main way on) and the fonts of its words. A choice shows at once and is kept in the
//! settings; the look itself is held in `theme`, where the window reads it as it is drawn.

use super::*;
use crate::theme::{Look, UiFont};
use iced::widget::{Row, column, row, scrollable, text_input};
use iced::{Border, Theme};

/// A swatch of an accent colour to pick.
const SWATCH_SIZE: f32 = 30.0;
const SWATCH_GAP: f32 = 8.0;
/// The box to write a colour of one's own in.
const HEX_WIDTH: f32 = 110.0;
/// Under each font's name, what it is, in its own small words.
const FONT_ABOUT_SIZE: f32 = 13.0;
const SECTION_SIZE: f32 = 14.0;
/// The words on the buttons for sizes.
const STEP_TEXT_SIZE: f32 = 14.0;
/// The room the card leaves around itself in a small window.
const CARD_MARGIN: f32 = 16.0;

impl App {
    /// The look changed to `look`: shown from now on, and kept.
    pub(super) fn set_look(&mut self, look: Look) {
        theme::set_look(look);
        self.settings.look = look;
        self.settings.save_look();
    }

    /// The card to choose how the app looks; `hex` is what is written in the box for a colour
    /// of one's own.
    pub(super) fn appearance_dialog(&self, hex: &str, window: Size) -> Element<'_, Message> {
        let look = theme::look();
        let section = |words: &'static str| {
            ui_text(words)
                .size(theme::text_size(SECTION_SIZE))
                .font(theme::small_font())
                .color(theme::TEXT_DIM)
        };

        let swatches = theme::ACCENTS.map(|(name, color)| {
            let chosen = same_colour(look.accent, color);
            let swatch = button(space().width(SWATCH_SIZE).height(SWATCH_SIZE))
                .padding(0)
                .style(swatch(color, chosen))
                .on_press(Message::AccentChosen(color));
            hint_below(swatch, name)
        });
        let written = theme::from_hex(hex);
        let own = text_input("#1fc88c", hex)
            .on_input(Message::AccentWritten)
            .font(theme::body_font())
            .size(theme::default_text_size())
            .width(HEX_WIDTH)
            .padding([4, 8]);
        let note = match written {
            Some(_) => "Any colour, as #rrggbb",
            None => "Write it as #rrggbb, six hex digits",
        };
        let colours = column![
            section("Accent colour"),
            Row::with_children(swatches).spacing(SWATCH_GAP),
            row![
                own,
                ui_text(note)
                    .size(theme::text_size(STATUS_TEXT_SIZE))
                    .font(theme::small_font())
                    .color(if written.is_some() { theme::TEXT_DIM } else { theme::DANGER }),
            ]
            .spacing(ROW_SPACING)
            .align_y(Center),
        ]
        .spacing(10);

        // Each shown in its own fonts, so it can be judged before it is chosen.
        let fonts = UiFont::ALL.map(|font| {
            let lit = font == look.font;
            let about = ui_text(font.about())
                .size(theme::text_size(FONT_ABOUT_SIZE))
                .font(font.small());
            button(column![ui_text(font.name()).font(font.display()), about].spacing(2))
                .width(Fill)
                .padding([8, 12])
                .style(if lit { theme::on } else { theme::normal })
                .on_press(Message::UiFontChosen(font))
                .into()
        });
        let fonts = column![section("Font"), Column::with_children(fonts).spacing(8)].spacing(10);

        let sizes = column![
            section("Size of everything"),
            steps(&theme::UI_SCALES, look.ui_scale, Message::UiScaleChosen),
            section("Text size"),
            steps(&theme::TEXT_SCALES, look.text_scale, Message::TextScaleChosen),
        ]
        .spacing(10);

        let body = column![colours, fonts, sizes].spacing(20);
        let changed = look != Look::default();
        let card = column![
            ui_text("Appearance")
                .size(theme::text_size(DIALOG_TITLE_SIZE))
                .font(theme::display_font()),
            scrollable(container(body).padding(iced::Padding {
                right: 14.0,
                ..iced::Padding::ZERO
            }))
            .height(iced::Length::Shrink),
            row![
                button(theme::display("Reset"))
                    .style(theme::normal)
                    .on_press_maybe(changed.then_some(Message::AppearanceReset)),
                space().width(Fill),
                button(theme::display("Done"))
                    .style(theme::primary)
                    .on_press(Message::AppearanceClosed),
            ],
        ]
        .spacing(16);
        container(card)
            .padding(20)
            .width(DIALOG_WIDTH)
            .max_height((window.height - 2.0 * CARD_MARGIN).max(0.0))
            .style(theme::card)
            .into()
    }
}

/// A row of sizes to pick from, as percentages, the one in use lit up.
fn steps<'a>(
    offered: &[f32],
    now: f32,
    pick: fn(f32) -> Message,
) -> Element<'a, Message> {
    let buttons = offered.iter().map(|&scale| {
        let lit = (scale - now).abs() < 0.001;
        button(ui_text(format!("{:.0}%", scale * 100.0)).size(theme::text_size(STEP_TEXT_SIZE)))
            .padding([4, 8])
            .style(if lit { theme::on } else { theme::normal })
            .on_press(pick(scale))
            .into()
    });
    Row::with_children(buttons).spacing(6).into()
}

/// Whether two colours are the same to the eye: as stored, to the byte.
fn same_colour(a: Color, b: Color) -> bool {
    a.into_rgba8() == b.into_rgba8()
}

/// A swatch: its colour, ringed when chosen, outlined under the pointer.
fn swatch(color: Color, chosen: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let ring = match status {
            _ if chosen => theme::TEXT,
            button::Status::Hovered | button::Status::Pressed => theme::TEXT_DIM,
            _ => Color::TRANSPARENT,
        };
        button::Style {
            background: Some(iced::Background::Color(color)),
            border: Border {
                color: ring,
                width: if chosen { 3.0 } else { 2.0 },
                radius: theme::BUTTON_RADIUS.into(),
            },
            ..button::Style::default()
        }
    }
}
