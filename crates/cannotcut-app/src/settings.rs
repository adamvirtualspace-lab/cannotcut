//! What cannotcut remembers between runs, in `settings.json`: `%APPDATA%\cannotcut` on Windows,
//! `$XDG_CONFIG_HOME/cannotcut` or `~/.config/cannotcut` elsewhere.

use std::collections::HashMap;
use std::path::PathBuf;

use cannotcut_export::{Options as ExportOptions, Quality, Resolution};
use serde_json::{Map, Value, json};

const FILE: &str = "settings.json";
const SOUND_DELAYS: &str = "sound_delay_ms";
const EXPORT: &str = "export";

#[derive(Default)]
pub struct Settings {
    /// How many milliseconds after the sound card's own reckoning each sound output is really
    /// heard, by its name. Some outputs process sound on the way (laptop speakers with
    /// "enhancements", Bluetooth), which the card does not report; the pictures wait as long.
    pub sound_delays: HashMap<String, u64>,
    /// What the last film was exported as.
    pub export: ExportOptions,
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
        if let Some(export) = saved.get(EXPORT) {
            let key = |name: &str| export.get(name).and_then(Value::as_str);
            if let Some(resolution) = key("resolution").and_then(Resolution::from_key) {
                settings.export.resolution = resolution;
            }
            if let Some(quality) = key("quality").and_then(Quality::from_key) {
                settings.export.quality = quality;
            }
        }
        settings
    }

    /// Keeps the export choices in the file. Whatever else it holds stays as it is. Returns
    /// at once; the file is written on a thread of its own.
    pub fn save_export(&self) {
        let export = json!({
            "resolution": self.export.resolution.key(),
            "quality": self.export.quality.key(),
        });
        let written = std::thread::Builder::new()
            .name("cannotcut-settings".to_string())
            .spawn(move || {
                let Some(path) = path() else {
                    return;
                };
                let mut saved = read().unwrap_or_default();
                saved.insert(EXPORT.to_string(), export);
                if let Some(folder) = path.parent() {
                    let _ = std::fs::create_dir_all(folder);
                }
                let mut text = serde_json::to_string_pretty(&Value::Object(saved))
                    .expect("settings with text keys always serialize");
                text.push('\n');
                let _ = std::fs::write(path, text);
            });
        // Not remembering a choice for next time is no reason to stop anything now.
        drop(written);
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
