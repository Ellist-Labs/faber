use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Per-project persisted app state (not user preferences). Lives in its own
/// file so settings.toml stays hand-editable.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    /// Keyed by absolute project root path.
    pub finder_history: HashMap<String, ProjectHistory>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectHistory {
    /// Root-relative paths, most recently opened first. Unbounded.
    pub files: Vec<String>,
}

impl AppState {
    pub fn history_for(&self, root: &str) -> &[String] {
        self.finder_history
            .get(root)
            .map(|h| h.files.as_slice())
            .unwrap_or(&[])
    }

    /// Move `rel_path` to the front of the project's history.
    pub fn record_finder_file(&mut self, root: &str, rel_path: &str) {
        let files = &mut self
            .finder_history
            .entry(root.to_string())
            .or_default()
            .files;
        files.retain(|p| p != rel_path);
        files.insert(0, rel_path.to_string());
    }
}

/// `~/.config/faber/state.toml` on every platform.
pub fn state_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/faber/state.toml")
}

/// Missing or invalid files fall back to empty state — never panics.
pub fn load() -> AppState {
    load_from(&state_path())
}

fn load_from(path: &PathBuf) -> AppState {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|err| {
            eprintln!(
                "faber: invalid {}: {err}; using empty state",
                path.display()
            );
            AppState::default()
        }),
        Err(_) => AppState::default(),
    }
}

/// Write via temp file + rename so a crash can't truncate the state.
pub fn save(state: &AppState) -> io::Result<()> {
    save_to(state, &state_path())
}

fn save_to(state: &AppState, path: &PathBuf) -> io::Result<()> {
    let text = toml::to_string_pretty(state).map_err(io::Error::other)?;
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
        std::env::temp_dir().join(format!("faber_state_{}_{name}.toml", std::process::id()))
    }

    #[test]
    fn record_prepends_and_dedups() {
        let mut s = AppState::default();
        s.record_finder_file("/p", "a.rs");
        s.record_finder_file("/p", "b.rs");
        s.record_finder_file("/p", "a.rs");
        assert_eq!(s.history_for("/p"), ["a.rs", "b.rs"]);
        assert!(s.history_for("/other").is_empty());
    }

    #[test]
    fn roundtrip_with_path_keys() {
        let path = tmp_path("roundtrip");
        let mut s = AppState::default();
        s.record_finder_file("/Users/me/my project", "src/main.rs");
        s.record_finder_file("/Users/me/other", "lib.rs");
        save_to(&s, &path).unwrap();
        assert_eq!(load_from(&path), s);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn missing_or_invalid_file_gives_empty_state() {
        assert_eq!(load_from(&tmp_path("missing")), AppState::default());
        let path = tmp_path("invalid");
        std::fs::write(&path, "not toml {{{").unwrap();
        assert_eq!(load_from(&path), AppState::default());
        std::fs::remove_file(&path).unwrap();
    }
}
