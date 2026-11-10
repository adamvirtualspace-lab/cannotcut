//! Where the sound plays: the system's default output, or one picked from those it offers, as
//! in a call app's settings. The choice is kept in the settings; a picked output that is not
//! there plays through the default until it is back.

use super::*;
use cannotcut_audio::OutputDevice;
use iced::widget::{column, row, scrollable};

/// The most room the list of outputs takes before it scrolls.
const LIST_HEIGHT: f32 = 300.0;
/// What the system's own choice is called.
const DEFAULT_NAME: &str = if cfg!(windows) { "Windows default" } else { "System default" };

/// An output's name as the system gives it, split for showing: the device, and what sort of
/// output it is. Windows names them "Headphones (WF-C510)"; a name without that shape is shown
/// whole, over `kind`.
fn shown_name(name: &str, kind: &str) -> (String, String) {
    if let Some(inside) = name.strip_suffix(')')
        && let Some((sort, device)) = inside.split_once(" (")
        && !device.is_empty()
    {
        return (device.to_string(), sort.to_string());
    }
    let kind = if kind.is_empty() { "Output" } else { kind };
    (name.to_string(), kind.to_string())
}

/// The outputs the system offers, as listed when the card opened.
#[derive(Clone, Debug, Default)]
pub(crate) struct Outputs {
    pub(crate) devices: Vec<OutputDevice>,
    /// The system's default output, by name.
    pub(crate) default: Option<String>,
}

/// Lists the outputs; it asks the system, which can take a moment, so not on the UI's thread.
pub(crate) async fn list_outputs() -> Outputs {
    Outputs {
        devices: cannotcut_audio::output_devices(),
        default: cannotcut_audio::default_output_name(),
    }
}

impl App {
    /// The card to pick the sound output: the default first, then each output, the one in use
    /// lit up. `outputs` is None while they are being listed.
    pub(super) fn sound_output_dialog(&self, outputs: Option<&Outputs>) -> Element<'_, Message> {
        let chosen = self.settings.sound_output.as_deref();
        let choice = |name: String, about: String, pick: Option<String>| {
            let lit = pick.as_deref() == chosen;
            let about = text(about).size(STATUS_TEXT_SIZE).font(theme::SMALL);
            button(column![text(name), about].spacing(2))
                .width(Fill)
                .padding([8, 12])
                .style(if lit { theme::on } else { theme::normal })
                .on_press(Message::SoundOutputChosen(pick))
                .into()
        };
        let list: Element<'_, Message> = match outputs {
            None => text("Looking for sound outputs...")
                .font(theme::SMALL)
                .color(theme::TEXT_DIM)
                .into(),
            Some(outputs) => {
                let default = match &outputs.default {
                    Some(name) => shown_name(name, "").0,
                    None => "No output at the moment".to_string(),
                };
                let mut entries = vec![choice(DEFAULT_NAME.to_string(), default, None)];
                entries.extend(outputs.devices.iter().map(|device| {
                    let (name, kind) = shown_name(&device.name, &device.kind);
                    choice(name, kind, Some(device.name.clone()))
                }));
                // A picked output that is not there now is still shown, so it can be seen.
                if let Some(chosen) = chosen
                    && !outputs.devices.iter().any(|device| device.name == chosen)
                {
                    let (name, _) = shown_name(chosen, "");
                    entries.push(choice(
                        name,
                        "Not connected: the default plays meanwhile".to_string(),
                        Some(chosen.to_string()),
                    ));
                }
                scrollable(
                    container(Column::with_children(entries).spacing(8)).padding(
                        iced::Padding {
                            right: 14.0,
                            ..iced::Padding::ZERO
                        },
                    ),
                )
                .height(iced::Length::Shrink)
                .into()
            }
        };
        let note = format!(
            "Where the sound plays. {DEFAULT_NAME} follows the system's own choice as it \
             changes."
        );
        let card = column![
            text("Sound output").size(DIALOG_TITLE_SIZE).font(theme::DISPLAY),
            text(note).size(STATUS_TEXT_SIZE).font(theme::SMALL).color(theme::TEXT_DIM),
            container(list).max_height(LIST_HEIGHT),
            row![
                button(theme::display("Close"))
                    .style(theme::normal)
                    .on_press(Message::SoundOutputClosed),
            ],
        ]
        .spacing(12);
        container(card)
            .padding(20)
            .width(DIALOG_WIDTH)
            .style(theme::card)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_windows_name_shows_the_device_over_its_sort() {
        assert_eq!(
            shown_name("Headphones (WF-C510)", "Headphones"),
            ("WF-C510".to_string(), "Headphones".to_string())
        );
        assert_eq!(
            shown_name("Speakers (Steam Streaming Microphone)", ""),
            ("Steam Streaming Microphone".to_string(), "Speakers".to_string())
        );
        assert_eq!(
            shown_name("Built-in Output", "Speakers"),
            ("Built-in Output".to_string(), "Speakers".to_string())
        );
        assert_eq!(shown_name("Odd ()", ""), ("Odd ()".to_string(), "Output".to_string()));
    }
}
