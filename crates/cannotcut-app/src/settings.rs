//! What cannotcut remembers between runs, in `settings.json`: `%APPDATA%\cannotcut` on Windows,
//! `$XDG_CONFIG_HOME/cannotcut` or `~/.config/cannotcut` elsewhere.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use cannotcut_export::{Options as ExportOptions, Quality, Resolution};
use serde_json::{Map, Value, json};

use crate::theme::{self, Look, UiFont};

const FILE: &str = "settings.json";
const SOUND_DELAYS: &str = "sound_delay_ms";
const SOUND_OUTPUT: &str = "sound_output";
const EXPORT: &str = "export";
const LAYOUT: &str = "layout";
const APPEARANCE: &str = "appearance";
const MEDIA_WIDTH: &str = "media_width";
const DETAILS_WIDTH: &str = "details_width";
const TIMELINE_SHARE: &str = "timeline_share";
const DOCK: &str = "dock";
const AWAY: &str = "away";
const HIDDEN: &str = "hidden";

/// How the panels were sized and arranged; none where they were left as they start.
#[derive(Clone, Default)]
pub struct Layout {
    /// How wide the side panels slide out in a narrow window, in logical pixels.
    pub media_width: Option<f32>,
    pub details_width: Option<f32>,
    /// The share of a narrow window's height the timeline takes, from 0 to 1.
    pub timeline_share: Option<f32>,
    /// How the panels are docked in a wide window, as `dock::to_value` keeps it.
    pub dock: Option<Value>,
    /// The panels in windows of their own, and where those windows were: a list of
    /// `{"part", "placement", "full_screen"}`.
    pub away: Option<Value>,
    /// The side panels hidden from a wide window by their buttons in the top bar, by their
    /// keys: `["media"]`, say.
    pub hidden: Vec<String>,
}

#[derive(Default)]
pub struct Settings {
    /// How many milliseconds after the sound card's own reckoning each sound output is really
    /// heard, by its name. Some outputs process sound on the way (laptop speakers with
    /// "enhancements", Bluetooth), which the card does not report; the pictures wait as long.
    pub sound_delays: HashMap<String, u64>,
    /// The sound output picked by name; none for the system's default.
    pub sound_output: Option<String>,
    /// What the last film was exported as.
    pub export: ExportOptions,
    pub layout: Layout,
    /// The accent colour and fonts chosen; as they start, unless chosen.
    pub look: Look,
}

impl Settings {
    /// The settings as saved; anything missing or unreadable is left at its default.
    pub fn load() -> Self {
        let mut settings = Self::default();
        let Some(saved) = read() else {
            return settings;
        };
        if let Some(Value::Object(delays)) = saved.get(SOUND_DELAYS) {
            settings.sound_delays = delays
                .iter()
                .filter_map(|(device, delay)| Some((device.clone(), delay.as_u64()?)))
                .collect();
        }
        settings.sound_output = saved
            .get(SOUND_OUTPUT)
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(export) = saved.get(EXPORT) {
            let key = |name: &str| export.get(name).and_then(Value::as_str);
            if let Some(resolution) = key("resolution").and_then(Resolution::from_key) {
                settings.export.resolution = resolution;
            }
            if let Some(quality) = key("quality").and_then(Quality::from_key) {
                settings.export.quality = quality;
            }
        }
        if let Some(appearance) = saved.get(APPEARANCE) {
            let key = |name: &str| appearance.get(name).and_then(Value::as_str);
            if let Some(accent) = key("accent").and_then(theme::from_hex) {
                settings.look.accent = accent;
            }
            if let Some(font) = key("font").and_then(UiFont::from_key) {
                settings.look.font = font;
            }
            let scale = |name: &str, offered: &[f32]| {
                let kept = appearance.get(name).and_then(Value::as_f64)? as f32;
                offered.iter().copied().find(|scale| (scale - kept).abs() < 0.001)
            };
            if let Some(ui_scale) = scale("ui_scale", &theme::UI_SCALES) {
                settings.look.ui_scale = ui_scale;
            }
            if let Some(text_scale) = scale("text_scale", &theme::TEXT_SCALES) {
                settings.look.text_scale = text_scale;
            }
        }
        if let Some(layout) = saved.get(LAYOUT) {
            let size = |name: &str| {
                let size = layout.get(name).and_then(Value::as_f64)? as f32;
                (size.is_finite() && size > 0.0).then_some(size)
            };
            settings.layout = Layout {
                media_width: size(MEDIA_WIDTH),
                details_width: size(DETAILS_WIDTH),
                timeline_share: size(TIMELINE_SHARE).filter(|share| *share < 1.0),
                dock: layout.get(DOCK).cloned(),
                away: layout.get(AWAY).cloned(),
                hidden: layout
                    .get(HIDDEN)
                    .and_then(Value::as_array)
                    .map(|keys| keys.iter().filter_map(Value::as_str).map(str::to_string).collect())
                    .unwrap_or_default(),
            };
        }
        settings
    }

    /// Keeps how late each output is heard in the file.
    pub fn save_sound_delays(&self) {
        save(SOUND_DELAYS, json!(self.sound_delays));
    }

    /// Keeps the sound output picked in the file.
    pub fn save_sound_output(&self) {
        save(SOUND_OUTPUT, json!(self.sound_output));
    }

    /// Keeps the accent colour and fonts chosen in the file.
    pub fn save_look(&self) {
        save(
            APPEARANCE,
            json!({
                "accent": theme::hex(self.look.accent),
                "font": self.look.font.key(),
                "ui_scale": self.look.ui_scale,
                "text_scale": self.look.text_scale,
            }),
        );
    }

    /// Keeps the export choices in the file.
    pub fn save_export(&self) {
        save(
            EXPORT,
            json!({
                "resolution": self.export.resolution.key(),
                "quality": self.export.quality.key(),
            }),
        );
    }

    /// Keeps the panels' sizes in the file.
    pub fn save_layout(&self) {
        let mut layout = Map::new();
        let widths = [
            (MEDIA_WIDTH, self.layout.media_width),
            (DETAILS_WIDTH, self.layout.details_width),
        ];
        for (name, width) in widths {
            if let Some(width) = width {
                layout.insert(name.to_string(), json!(width.round()));
            }
        }
        if let Some(share) = self.layout.timeline_share {
            let share = (f64::from(share) * 1000.0).round() / 1000.0;
            layout.insert(TIMELINE_SHARE.to_string(), json!(share));
        }
        if let Some(dock) = &self.layout.dock {
            layout.insert(DOCK.to_string(), dock.clone());
        }
        if let Some(away) = &self.layout.away {
            layout.insert(AWAY.to_string(), away.clone());
        }
        if !self.layout.hidden.is_empty() {
            layout.insert(HIDDEN.to_string(), json!(self.layout.hidden));
        }
        save(LAYOUT, Value::Object(layout));
    }
}

/// Puts `value` in the file under `key`. Whatever else it holds stays as it is. Returns at
/// once; the file is written on a thread of its own.
fn save(key: &'static str, value: Value) {
    static WRITING: Mutex<()> = Mutex::new(());
    let writer = std::thread::Builder::new()
        .name("cannotcut-settings".to_string())
        .spawn(move || {
            let Some(path) = path() else {
                return;
            };
            // One at a time, so that two keys saved together both stay.
            let _writing = WRITING.lock();
            let mut saved = read().unwrap_or_default();
            saved.insert(key.to_string(), value);
            if let Some(folder) = path.parent() {
                let _ = std::fs::create_dir_all(folder);
            }
            let mut text = serde_json::to_string_pretty(&Value::Object(saved))
                .expect("settings with text keys always serialize");
            text.push('\n');
            let _ = std::fs::write(path, text);
        });
    // Not remembering a choice for next time is no reason to stop anything now.
    if let Ok(writer) = writer {
        lock_writers().push(writer);
    }
}

fn lock_writers() -> std::sync::MutexGuard<'static, Vec<std::thread::JoinHandle<()>>> {
    static WRITERS: Mutex<Vec<std::thread::JoinHandle<()>>> = Mutex::new(Vec::new());
    WRITERS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Waits for what is being written to the file, before the app ends.
pub fn finish_writing() {
    let writers = std::mem::take(&mut *lock_writers());
    for writer in writers {
        let _ = writer.join();
    }
}

fn read() -> Option<Map<String, Value>> {
    let text = std::fs::read_to_string(path()?).ok()?;
    match serde_json::from_str(&text) {
        Ok(Value::Object(saved)) => Some(saved),
        _ => None,
    }
}

fn path() -> Option<PathBuf> {
    let folder = if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA")?)
    } else if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(config)
    } else {
        PathBuf::from(std::env::var_os("HOME")?).join(".config")
    };
    Some(folder.join("cannotcut").join(FILE))
}
