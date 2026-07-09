// LspAdapter trait + RustAnalyzerAdapter: binary resolution, init options.

use std::collections::HashMap;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// AdapterError
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum AdapterError {
    NotFound,
    Install(crate::install::InstallError),
    Io(std::io::Error),
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterError::NotFound => write!(f, "language server binary not found"),
            AdapterError::Install(e) => write!(f, "install error: {e}"),
            AdapterError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for AdapterError {}

impl From<crate::install::InstallError> for AdapterError {
    fn from(e: crate::install::InstallError) -> Self {
        AdapterError::Install(e)
    }
}

impl From<std::io::Error> for AdapterError {
    fn from(e: std::io::Error) -> Self {
        AdapterError::Io(e)
    }
}

// ---------------------------------------------------------------------------
// LspAdapter trait
// ---------------------------------------------------------------------------

pub trait LspAdapter: Send + Sync {
    /// Unique identifier for this server (e.g. "rust-analyzer").
    fn server_id(&self) -> &'static str;

    /// Language IDs this adapter handles (e.g. ["rust"]).
    fn languages(&self) -> &[&'static str];

    /// Resolve the binary path: user override > PATH search > managed download.
    /// `progress_cb` is called with human-readable status strings during download.
    fn resolve_binary(
        &self,
        settings: &faber_settings::LspSettings,
        progress_cb: &mut dyn FnMut(&str),
    ) -> Result<PathBuf, AdapterError>;

    /// Optional initialization options passed in the LSP `initialize` request.
    fn init_options(&self) -> Option<serde_json::Value> {
        None
    }

    /// Extra environment variables to set when spawning the server process.
    fn server_env(&self) -> HashMap<String, String> {
        HashMap::new()
    }
}

// ---------------------------------------------------------------------------
// RustAnalyzerAdapter
// ---------------------------------------------------------------------------

pub struct RustAnalyzerAdapter;

// Pinned version — bump this to trigger re-download on next start.
const RA_VERSION: &str = "2025-07-07";

impl LspAdapter for RustAnalyzerAdapter {
    fn server_id(&self) -> &'static str {
        "rust-analyzer"
    }

    fn languages(&self) -> &[&'static str] {
        &["rust"]
    }

    fn resolve_binary(
        &self,
        settings: &faber_settings::LspSettings,
        progress_cb: &mut dyn FnMut(&str),
    ) -> Result<PathBuf, AdapterError> {
        // 1. User-configured override.
        if let Some(path) = settings
            .servers
            .get("rust-analyzer")
            .and_then(|c| c.binary_path.as_ref())
        {
            if path.exists() {
                log::info!("Using user-configured rust-analyzer at {}", path.display());
                return Ok(path.clone());
            }
            log::warn!(
                "User-configured rust-analyzer path does not exist: {}; falling through",
                path.display()
            );
        }

        // 2. Search PATH.
        let path_var = std::env::var("PATH").unwrap_or_default();
        for dir in std::env::split_paths(&path_var) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            let candidate = dir.join("rust-analyzer");
            let is_executable = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    candidate
                        .metadata()
                        .map(|m| m.permissions().mode() & 0o111 != 0)
                        .unwrap_or(false)
                }
                #[cfg(not(unix))]
                {
                    candidate.metadata().is_ok()
                }
            };
            if is_executable {
                log::info!("Found rust-analyzer in PATH at {}", candidate.display());
                return Ok(candidate);
            }
        }

        // 3. Managed download.
        progress_cb("Checking rust-analyzer cache…");
        let path = crate::install::Installer::install_or_check(
            RA_VERSION,
            progress_cb,
            None,
            &crate::install::UreqFetcher,
        )?;
        Ok(path)
    }

    fn init_options(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "checkOnSave": { "command": "clippy" } }))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use faber_settings::{LanguageServerConfig, LspSettings};
    use std::collections::HashMap;

    fn settings_with_binary(path: Option<PathBuf>) -> LspSettings {
        let mut servers = HashMap::new();
        servers.insert(
            "rust-analyzer".to_owned(),
            LanguageServerConfig {
                binary_path: path,
                enabled: true,
                initialization_options: None,
            },
        );
        LspSettings { servers }
    }

    // 1. User override exists: resolve_binary returns it without downloading.
    #[test]
    fn test_user_override_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("rust-analyzer");
        std::fs::write(&bin, b"fake").expect("write fake binary");

        let settings = settings_with_binary(Some(bin.clone()));
        let adapter = RustAnalyzerAdapter;
        let mut cb_called = false;
        let result = adapter.resolve_binary(&settings, &mut |_| {
            cb_called = true;
        });

        assert_eq!(result.unwrap(), bin);
        assert!(
            !cb_called,
            "progress_cb must not be called for a cached user path"
        );
    }

    // 2. User override points to nonexistent path: falls through to PATH search
    //    (no error returned for a missing user-configured path).
    #[test]
    fn test_user_override_missing_falls_through() {
        let nonexistent = PathBuf::from("/nonexistent/path/rust-analyzer-fake-test-binary");
        let settings = settings_with_binary(Some(nonexistent));
        let adapter = RustAnalyzerAdapter;

        // We expect either Ok (found in PATH) or a download error/NotFound —
        // never an Io/panic just because the user path doesn't exist.
        let result = adapter.resolve_binary(&settings, &mut |_| {});
        match result {
            // Found in PATH or managed cache — valid.
            Ok(_) => {}
            // Install/NotFound is acceptable — what matters is it didn't return Io
            // for the missing user-configured path.
            Err(AdapterError::Install(_)) | Err(AdapterError::NotFound) => {}
            Err(AdapterError::Io(e)) => {
                // Should not surface an Io error for the missing override path.
                panic!("unexpected Io error from missing user override: {e}");
            }
        }
    }

    // 3. Server ID and languages are correct.
    #[test]
    fn test_server_id_and_languages() {
        let adapter = RustAnalyzerAdapter;
        assert_eq!(adapter.server_id(), "rust-analyzer");
        assert_eq!(adapter.languages(), &["rust"]);
    }
}
