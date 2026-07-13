use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const RECENT_CAP: usize = 5;

// ── Serialized pane layout ────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SerializedPane {
    pub files: Vec<String>,
    pub active: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SerializedNode {
    Pane(SerializedPane),
    Axis {
        axis: String,
        members: Vec<SerializedNode>,
        flexes: Vec<f32>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SerializedLayout {
    pub root: SerializedNode,
}

fn push_recent(list: &mut Vec<String>, abs: &str) {
    list.retain(|p| p != abs);
    list.insert(0, abs.to_string());
    list.truncate(RECENT_CAP);
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LastSession {
    pub root: Option<String>,
    pub files: Vec<String>,
    pub layout: Option<SerializedLayout>,
}

/// Per-project persisted app state (not user preferences). Lives in its own
/// file so settings.toml stays hand-editable.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    /// Keyed by absolute project root path.
    pub finder_history: HashMap<String, ProjectHistory>,
    pub recent_projects: Vec<String>,
    pub recent_files: Vec<String>,
    pub last_session: Option<LastSession>,
    /// Projects the user has explicitly trusted to run language servers.
    /// Keys are canonical absolute path strings.
    #[serde(default)]
    pub trusted_projects: std::collections::HashSet<String>,
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

    pub fn record_recent_project(&mut self, abs: &str) {
        push_recent(&mut self.recent_projects, abs);
    }

    pub fn remove_recent_project(&mut self, abs: &str) {
        self.recent_projects.retain(|p| p != abs);
    }

    pub fn record_recent_file(&mut self, abs: &str) {
        push_recent(&mut self.recent_files, abs);
    }

    pub fn set_last_session(&mut self, root: Option<String>, files: Vec<String>) {
        let layout = self.last_session.as_ref().and_then(|s| s.layout.clone());
        self.last_session = Some(LastSession {
            root,
            files,
            layout,
        });
    }

    pub fn set_last_session_layout(&mut self, layout: Option<SerializedLayout>) {
        if let Some(ref mut s) = self.last_session {
            s.layout = layout;
        }
    }

    pub fn trust_project(&mut self, path: impl AsRef<std::path::Path>) {
        self.trusted_projects
            .insert(path.as_ref().to_string_lossy().into_owned());
    }

    pub fn is_trusted(&self, path: impl AsRef<std::path::Path>) -> bool {
        self.trusted_projects
            .contains(path.as_ref().to_string_lossy().as_ref())
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
    fn recent_projects_cap_and_dedup() {
        let mut s = AppState::default();
        for i in 0..7usize {
            s.record_recent_project(&format!("/p{i}"));
        }
        assert_eq!(s.recent_projects.len(), RECENT_CAP);
        assert_eq!(s.recent_projects[0], "/p6");
        s.record_recent_project("/p5");
        assert_eq!(s.recent_projects[0], "/p5");
        assert_eq!(s.recent_projects.len(), RECENT_CAP);
    }

    #[test]
    fn recent_files_cap_and_dedup() {
        let mut s = AppState::default();
        for i in 0..6usize {
            s.record_recent_file(&format!("/f{i}.rs"));
        }
        assert_eq!(s.recent_files.len(), RECENT_CAP);
        assert_eq!(s.recent_files[0], "/f5.rs");
        s.record_recent_file("/f3.rs");
        assert_eq!(s.recent_files[0], "/f3.rs");
    }

    #[test]
    fn last_session_roundtrip() {
        let path = tmp_path("session");
        let mut s = AppState::default();
        s.set_last_session(
            Some("/my/project".to_string()),
            vec!["src/main.rs".to_string()],
        );
        save_to(&s, &path).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded.last_session, s.last_session);
        std::fs::remove_file(&path).unwrap();
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

    #[test]
    fn trust_project_roundtrip() {
        let mut s = AppState::default();
        assert!(!s.is_trusted("/home/user/myproject"));
        s.trust_project("/home/user/myproject");
        assert!(s.is_trusted("/home/user/myproject"));
        assert!(!s.is_trusted("/home/user/other"));
        // idempotent
        s.trust_project("/home/user/myproject");
        assert_eq!(s.trusted_projects.len(), 1);
    }
}
