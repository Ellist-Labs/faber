use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const DEFAULT_FONT_SIZE: f32 = 13.0;

/// When to automatically save dirty documents. Mirrors VS Code's
/// `files.autoSave` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum AutoSave {
    #[default]
    Off,
    AfterDelay,
    OnFocusChange,
    OnWindowChange,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct Settings {
    pub auto_save: AutoSave,
    pub auto_save_delay_ms: u64,
    /// Base UI font size in px; the whole application scales from it.
    pub font_size: f32,
    /// Whether to show the line-number gutter in the editor.
    pub line_numbers: bool,
    /// Whether to show the interactive scrollbar on scrollable views.
    pub show_scrollbar: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_save: AutoSave::Off,
            auto_save_delay_ms: 1000,
            font_size: DEFAULT_FONT_SIZE,
            line_numbers: true,
            show_scrollbar: true,
        }
    }
}

/// `~/.config/faber/settings.toml` on every platform.
pub fn settings_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/faber/settings.toml")
}

/// Missing or invalid files fall back to defaults — never panics.
pub fn load() -> Settings {
    load_from(&settings_path())
}

fn load_from(path: &PathBuf) -> Settings {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|err| {
            eprintln!("faber: invalid {}: {err}; using defaults", path.display());
            Settings::default()
        }),
        Err(_) => Settings::default(),
    }
}

/// Write via temp file + rename so a crash can't truncate the settings.
pub fn save(settings: &Settings) -> io::Result<()> {
    save_to(settings, &settings_path())
}

fn save_to(settings: &Settings, path: &PathBuf) -> io::Result<()> {
    let text = toml::to_string_pretty(settings).map_err(io::Error::other)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("faber_settings_{}_{name}.toml", std::process::id()))
    }

    #[test]
    fn roundtrip() {
        let path = tmp_path("roundtrip");
        let s = Settings {
            auto_save: AutoSave::AfterDelay,
            auto_save_delay_ms: 500,
            font_size: 16.0,
            line_numbers: true,
            show_scrollbar: false,
        };
        save_to(&s, &path).unwrap();
        assert_eq!(load_from(&path), s);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn missing_file_gives_defaults() {
        assert_eq!(load_from(&tmp_path("missing")), Settings::default());
    }

    #[test]
    fn partial_file_fills_defaults() {
        let path = tmp_path("partial");
        std::fs::write(&path, "font_size = 18.0\n").unwrap();
        let s = load_from(&path);
        assert_eq!(s.font_size, 18.0);
        assert_eq!(s.auto_save, AutoSave::Off);
        assert_eq!(s.auto_save_delay_ms, 1000);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn unknown_keys_tolerated_invalid_falls_back() {
        let path = tmp_path("unknown");
        std::fs::write(&path, "future_option = true\nfont_size = 14.0\n").unwrap();
        assert_eq!(load_from(&path).font_size, 14.0);

        std::fs::write(&path, "not toml at {{{").unwrap();
        assert_eq!(load_from(&path), Settings::default());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn autosave_serializes_camel_case() {
        let s = Settings { auto_save: AutoSave::OnFocusChange, ..Default::default() };
        let text = toml::to_string(&s).unwrap();
        assert!(text.contains("onFocusChange"), "{text}");
    }
}
