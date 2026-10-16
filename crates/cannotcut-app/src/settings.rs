//! What cannotcut remembers between runs, in `settings.json`: `%APPDATA%\cannotcut` on Windows,
//! `$XDG_CONFIG_HOME/cannotcut` or `~/.config/cannotcut` elsewhere.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::Value;

const FILE: &str = "settings.json";
const SOUND_DELAYS: &str = "sound_delay_ms";

#[derive(Default)]
pub struct Settings {
    /// How many milliseconds after the sound card's own reckoning each sound output is really
    /// heard, by its name. Some outputs process sound on the way (laptop speakers with
    /// "enhancements", Bluetooth), which the card does not report; the pictures wait as long.
    pub sound_delays: HashMap<String, u64>,
}

impl Settings {
    /// The settings as saved; anything missing or unreadable is left at its default.
    pub fn load() -> Self {
        let mut settings = Self::default();
        let Some(text) = path().and_then(|path| std::fs::read_to_string(path).ok()) else {
            return settings;
        };
        let Ok(Value::Object(saved)) = serde_json::from_str(&text) else {
            return settings;
        };
        if let Some(Value::Object(delays)) = saved.get(SOUND_DELAYS) {
            settings.sound_delays = delays
                .iter()
                .filter_map(|(device, delay)| Some((device.clone(), delay.as_u64()?)))
                .collect();
        }
        settings
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
