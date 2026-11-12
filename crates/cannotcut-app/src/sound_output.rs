//! Where the sound plays: the system's default output, or one picked from those it offers, as
//! in a call app's settings. The choice is kept in the settings; a picked output that is not
//! there plays through the default until it is back.
//!
//! Each output can be heard later than the sound card says (a speaker's own processing,
//! Bluetooth), which would put the sound behind the pictures. A test lines them up: a beep and
//! a flash each second, and a delay to set until they come together, or to measure with the
//! microphone.

use std::time::Instant;

use super::*;
use cannotcut_audio::{Listener, OutputDevice};
use iced::widget::{column, row, rule, scrollable};

/// The most room the list of outputs takes before it scrolls.
const LIST_HEIGHT: f32 = 220.0;
/// The most a delay is set to, in milliseconds: Bluetooth can take a third of a second.
const MOST_DELAY: u32 = 500;
const DELAY_STEP: u32 = 5;
/// How long the flash shows at each beep.
const FLASH_SECONDS: f64 = 0.1;
const FLASH_SIZE: f32 = 36.0;
/// How long the microphone listens: enough beeps for a steady middle value.
const LISTEN: Duration = Duration::from_secs(6);
/// About how long a picture takes from the app to the screen: two frames of a 60 Hz display,
/// the compositor's and the screen's own. The pictures are that late already, so the sound
/// is held back that much less.
const PICTURE_LATENCY_MS: u32 = 33;
/// The room the card leaves around itself in a small window.
const CARD_MARGIN: f32 = 16.0;

/// The sync test while it runs: the beeps, and the microphone while it measures.
pub(crate) struct SyncTest {
    rate: u32,
    listening: Option<(Listener, Instant)>,
    /// What the last measuring found, in words.
    result: Option<String>,
}

impl SyncTest {
    pub(crate) fn new(rate: u32) -> Self {
        Self {
            rate,
            listening: None,
            result: None,
        }
    }

    /// Starts listening on the microphone, or notes why it cannot.
    pub(crate) fn listen(&mut self) {
        match Listener::start() {
            Ok(listener) => {
                self.result = None;
                self.listening = Some((listener, Instant::now() + LISTEN));
            }
            Err(error) => self.result = Some(format!("No microphone to listen with: {error}")),
        }
    }

    /// Once listening is over: how much later the beeps were heard than the sound card said,
    /// `reported` being its report of when a frame is heard. A note on why nothing came of it
    /// otherwise.
    pub(crate) fn finished(
        &mut self,
        reported: impl Fn(i64) -> Option<Instant>,
    ) -> Option<Duration> {
        let (listener, until) = self.listening.as_ref()?;
        if Instant::now() < *until {
            return None;
        }
        let rate = i64::from(self.rate);
        let heard = listener.heard();
        let device = listener.device().to_string();
        self.listening = None;
        let lag = cannotcut_audio::measure(&heard, |beep| reported(beep as i64 * rate));
        if lag.is_none() {
            self.result = Some(format!(
                "{device} did not hear the beeps clearly ({} heard). Put it near the speakers, \
                 turn the sound up, and try again.",
                heard.len()
            ));
        }
        lag
    }

    pub(crate) fn note_result(&mut self, lag: Duration, delay: u32) {
        self.result = Some(format!(
            "The sound is heard {} ms after the sound card says. Pictures take about \
             {PICTURE_LATENCY_MS} ms to show, so the delay is set to {delay} ms. Check it with \
             the flash.",
            lag.as_millis()
        ));
    }
}

/// The delay to set for sound heard `lag` later than the sound card says.
pub(crate) fn delay_for(lag: Duration) -> u32 {
    let lag = u32::try_from(lag.as_millis()).unwrap_or(MOST_DELAY);
    let delay = lag.saturating_sub(PICTURE_LATENCY_MS).min(MOST_DELAY);
    delay / DELAY_STEP * DELAY_STEP
}
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
    pub(super) fn sound_output_dialog(
        &self,
        outputs: Option<&Outputs>,
        window: Size,
    ) -> Element<'_, Message> {
        let chosen = self.settings.sound_output.as_deref();
        let choice = |name: String, about: String, pick: Option<String>| {
            let lit = pick.as_deref() == chosen;
            let about = ui_text(about).size(theme::text_size(STATUS_TEXT_SIZE)).font(theme::small_font());
            button(column![ui_text(name), about].spacing(2))
                .width(Fill)
                .padding([8, 12])
                .style(if lit { theme::on } else { theme::normal })
                .on_press(Message::SoundOutputChosen(pick))
                .into()
        };
        let list: Element<'_, Message> = match outputs {
            None => ui_text("Looking for sound outputs...")
                .font(theme::small_font())
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
        let body = column![
            ui_text(note).size(theme::text_size(STATUS_TEXT_SIZE)).font(theme::small_font()).color(theme::TEXT_DIM),
            container(list).max_height(LIST_HEIGHT),
            rule::horizontal(1),
            self.sync_section(),
        ]
        .spacing(12);
        let card = column![
            ui_text("Sound output").size(theme::text_size(DIALOG_TITLE_SIZE)).font(theme::display_font()),
            scrollable(container(body).padding(iced::Padding {
                right: 14.0,
                ..iced::Padding::ZERO
            }))
            .height(iced::Length::Shrink),
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
            .max_height((window.height - 2.0 * CARD_MARGIN).max(0.0))
            .style(theme::card)
            .into()
    }

    /// Lining the sound up with the pictures, for the output in use.
    fn sync_section(&self) -> Element<'_, Message> {
        let SoundStatus::Ready { device, .. } = self.sound.status() else {
            return ui_text("No sound output to line up.")
                .size(theme::text_size(STATUS_TEXT_SIZE))
                .font(theme::small_font())
                .color(theme::TEXT_DIM)
                .into();
        };
        let delay = self.settings.sound_delays.get(&device).copied().unwrap_or(0);
        let delay = u32::try_from(delay).unwrap_or(MOST_DELAY).min(MOST_DELAY);
        let testing = self.sync_test.is_some();
        // Lit while the beep is heard, as the playhead would be on it.
        let lit = testing
            && self
                .sound
                .position(Instant::now())
                .is_some_and(|seconds| seconds.fract() < FLASH_SECONDS);
        let flash = container(space().width(FLASH_SIZE).height(FLASH_SIZE)).style(move |_| {
            container::background(if lit { Color::WHITE } else { Color::BLACK })
        });
        let test = button(theme::display(if testing { "Stop test" } else { "Test" }))
            .style(if testing { theme::on } else { theme::normal })
            .on_press(Message::SyncTestToggled);
        let listening = self
            .sync_test
            .as_ref()
            .is_some_and(|test| test.listening.is_some());
        let measure = button(theme::display(if listening {
            "Listening..."
        } else {
            "Measure with microphone"
        }))
        .style(theme::normal)
        .on_press_maybe((!listening).then_some(Message::SyncMeasure));
        let slider = slider(0..=MOST_DELAY, delay, Message::SyncDelayChanged).step(DELAY_STEP);
        let about = "A beep and a flash each second. Move the delay until they come \
                     together, or let the microphone measure it.";
        let mut section = column![
            ui_text("Line up with pictures").font(theme::display_font()),
            ui_text(about).size(theme::text_size(STATUS_TEXT_SIZE)).font(theme::small_font()).color(theme::TEXT_DIM),
            row![test, flash].spacing(12).align_y(Center),
            row![
                ui_text("Delay").font(theme::small_font()),
                slider,
                ui_text(format!("{delay} ms")).width(VOLUME_LABEL_WIDTH),
            ]
            .spacing(VOLUME_SPACING)
            .align_y(Center),
            measure,
        ]
        .spacing(10);
        if let Some(result) = self.sync_test.as_ref().and_then(|test| test.result.clone()) {
            section = section.push(ui_text(result).size(theme::text_size(STATUS_TEXT_SIZE)).font(theme::small_font()));
        }
        section.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_delay_is_what_the_sound_lags_the_pictures_by() {
        assert_eq!(delay_for(Duration::from_millis(120)), 85);
        assert_eq!(delay_for(Duration::from_millis(20)), 0);
        assert_eq!(delay_for(Duration::from_secs(3)), MOST_DELAY);
    }

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
