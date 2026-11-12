//! About cannotcut, from the menu: its version, and what it is built with under whose
//! licence. Each licence can be read in full, as most of them ask to travel with the app.

use super::*;
use iced::widget::{column, row, scrollable};

/// The most room the About card takes, and what it leaves around itself in a small window.
const ABOUT_WIDTH: f32 = 600.0;
const ABOUT_HEIGHT: f32 = 560.0;
const ABOUT_MARGIN: f32 = 16.0;
/// A licence's text: small, as it is long.
const LICENCE_TEXT_SIZE: f32 = 13.0;

/// What the About card shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum AboutPage {
    /// What cannotcut is built with.
    List,
    /// The full licence of one of [`CREDITS`].
    Licence(usize),
}

/// Something cannotcut is built with: what it is, what it does here, and its licence.
struct Credit {
    name: &'static str,
    does: &'static str,
    licence: &'static str,
    text: &'static str,
}

const CREDITS: [Credit; 9] = [
    Credit {
        name: "Anton SC",
        does: "Condensed fonts: words on buttons and dialog titles; its small capitals made the \n               default letters",
        licence: "SIL Open Font License 1.1",
        text: include_str!("../fonts/AntonSC-OFL.txt"),
    },
    Credit {
        name: "Antonio",
        does: "Condensed fonts: small words, as notes, hints, readouts",
        licence: "SIL Open Font License 1.1",
        text: include_str!("../fonts/Antonio-OFL.txt"),
    },
    Credit {
        name: "Oswald",
        does: "Condensed fonts: every other word",
        licence: "SIL Open Font License 1.1",
        text: include_str!("../fonts/Oswald-OFL.txt"),
    },
    Credit {
        name: "Inter",
        does: "The words, when Inter is chosen under Appearance",
        licence: "SIL Open Font License 1.1",
        text: include_str!("../fonts/Inter-OFL.txt"),
    },
    Credit {
        name: "Roboto",
        does: "The words, when Roboto is chosen under Appearance",
        licence: "SIL Open Font License 1.1",
        text: include_str!("../fonts/Roboto-OFL.txt"),
    },
    Credit {
        name: "Tabler Icons",
        does: "The icons",
        licence: "MIT",
        text: include_str!("../icons/LICENSE"),
    },
    Credit {
        name: "FFmpeg",
        does: "Reading and writing video and sound, from its own libraries beside the app \
               (source at ffmpeg.org)",
        licence: "GNU LGPL 3.0",
        text: include_str!("../licences/FFmpeg-LGPL-3.0.txt"),
    },
    Credit {
        name: "GNU GPL 3.0",
        does: "The licence FFmpeg's LGPL adds its permissions to",
        licence: "GNU GPL 3.0",
        text: include_str!("../licences/GPL-3.0.txt"),
    },
    Credit {
        name: "Rust libraries",
        does: "The rest of the app: its window, drawing, sound and more",
        licence: "MIT, Apache 2.0 and others, each named",
        text: include_str!("../licences/rust-libraries.txt"),
    },
];

impl App {
    /// The About card, at most [`ABOUT_WIDTH`] by [`ABOUT_HEIGHT`] in a `window` of this size.
    pub(super) fn about_dialog(&self, page: AboutPage, window: Size) -> Element<'_, Message> {
        let width = ABOUT_WIDTH.min(window.width - 2.0 * ABOUT_MARGIN);
        let height = ABOUT_HEIGHT.min(window.height - 2.0 * ABOUT_MARGIN);
        let close = button(theme::display("Close"))
            .style(theme::normal)
            .on_press(Message::AboutClosed);
        let (title, body, buttons) = match page {
            AboutPage::List => {
                let entries = CREDITS.iter().enumerate().map(|(index, credit)| {
                    let about = format!("{}. {}", credit.licence, credit.does);
                    let about = ui_text(about).size(theme::text_size(STATUS_TEXT_SIZE)).font(theme::small_font());
                    button(column![theme::display(credit.name), about].spacing(2))
                        .width(Fill)
                        .padding([8, 12])
                        .style(theme::normal)
                        .on_press(Message::AboutShown(AboutPage::Licence(index)))
                        .into()
                });
                let version = format!("Version {}", env!("CARGO_PKG_VERSION"));
                let intro = "Built with the work of others, under these licences. Tap one \
                             to read it in full.";
                let list = column![
                    ui_text(version).font(theme::small_font()).color(theme::TEXT_DIM),
                    ui_text(intro).font(theme::small_font()),
                    Column::with_children(entries).spacing(8),
                ]
                .spacing(12);
                ("About cannotcut".to_string(), list, row![close])
            }
            AboutPage::Licence(index) => {
                let credit = &CREDITS[index];
                let back = button(theme::display("Back"))
                    .style(theme::normal)
                    .on_press(Message::AboutShown(AboutPage::List));
                let body = column![
                    ui_text(format!("{}: {}", credit.licence, credit.does))
                        .font(theme::small_font())
                        .color(theme::TEXT_DIM),
                    ui_text(credit.text).size(theme::text_size(LICENCE_TEXT_SIZE)),
                ]
                .spacing(12);
                (credit.name.to_string(), body, row![back, close].spacing(12))
            }
        };
        // The list or text scrolls; the title and buttons stay put.
        let body = scrollable(container(body).padding(iced::Padding {
            right: 14.0,
            ..iced::Padding::ZERO
        }))
        .id(ABOUT_SCROLL)
        .height(Fill);
        let card = column![
            ui_text(title).size(theme::text_size(DIALOG_TITLE_SIZE)).font(theme::display_font()),
            body,
            buttons,
        ]
        .spacing(12);
        container(card)
            .padding(20)
            .width(width)
            .height(height)
            .style(theme::card)
            .into()
    }
}

/// The About card's scrolling part, put back to its top when the page changes.
pub(crate) const ABOUT_SCROLL: &str = "about";
