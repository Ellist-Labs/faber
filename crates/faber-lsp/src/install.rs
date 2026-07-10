// Managed binary download: GitHub releases (.gz), SHA-256 verification,
// cache at ~/.cache/faber/lsp/, login-shell PATH sourcing.

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Platform
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Platform {
    Aarch64MacOs,
    X86_64MacOs,
    X86_64Linux,
}

impl Platform {
    pub fn current() -> Result<Platform, InstallError> {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => Ok(Platform::Aarch64MacOs),
            ("macos", "x86_64") => Ok(Platform::X86_64MacOs),
            ("linux", "x86_64") => Ok(Platform::X86_64Linux),
            _ => Err(InstallError::UnsupportedPlatform),
        }
    }

    pub fn artifact_name(&self) -> &'static str {
        match self {
            Platform::Aarch64MacOs => "rust-analyzer-aarch64-apple-darwin.gz",
            Platform::X86_64MacOs => "rust-analyzer-x86_64-apple-darwin.gz",
            Platform::X86_64Linux => "rust-analyzer-x86_64-unknown-linux-gnu.gz",
        }
    }
}

// ---------------------------------------------------------------------------
// InstallError
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum InstallError {
    UnsupportedPlatform,
    DownloadFailed(String),
    ChecksumMismatch { expected: String, actual: String },
    Io(std::io::Error),
    Http(Box<dyn std::error::Error + Send + Sync>),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::UnsupportedPlatform => {
                write!(
                    f,
                    "unsupported platform: {}/{}",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                )
            }
            InstallError::DownloadFailed(msg) => write!(f, "download failed: {msg}"),
            InstallError::ChecksumMismatch { expected, actual } => {
                write!(f, "checksum mismatch: expected {expected}, got {actual}")
            }
            InstallError::Io(e) => write!(f, "I/O error: {e}"),
            InstallError::Http(e) => write!(f, "HTTP error: {e}"),
        }
    }
}

impl std::error::Error for InstallError {}

impl From<std::io::Error> for InstallError {
    fn from(e: std::io::Error) -> Self {
        InstallError::Io(e)
    }
}

// ---------------------------------------------------------------------------
// Fetcher
// ---------------------------------------------------------------------------

/// Abstracts network access so downloads can be exercised in tests without a network.
pub trait Fetcher: Send {
    fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, InstallError>;

    /// Like `fetch_bytes` but calls `on_progress(bytes_downloaded_so_far)` after each chunk.
    /// Default impl fetches in full then calls the callback once at the end.
    fn fetch_bytes_with_progress(
        &self,
        url: &str,
        on_progress: &mut dyn FnMut(usize),
    ) -> Result<Vec<u8>, InstallError> {
        let buf = self.fetch_bytes(url)?;
        on_progress(buf.len());
        Ok(buf)
    }

    fn fetch_string(&self, url: &str) -> Option<String>;
}

/// Production fetcher backed by `ureq`.
pub struct UreqFetcher;

impl Fetcher for UreqFetcher {
    fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, InstallError> {
        self.fetch_bytes_with_progress(url, &mut |_| {})
    }

    fn fetch_bytes_with_progress(
        &self,
        url: &str,
        on_progress: &mut dyn FnMut(usize),
    ) -> Result<Vec<u8>, InstallError> {
        // No global timeout: binaries can be 40-80 MB and users may have slow
        // connections. We gate on connect (30s) and per-read-call (90s) so a
        // stalled transfer still fails rather than hanging indefinitely.
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(std::time::Duration::from_secs(30)))
            .timeout_per_call(Some(std::time::Duration::from_secs(90)))
            .build()
            .new_agent();
        let response = agent
            .get(url)
            .call()
            .map_err(|e| InstallError::Http(Box::new(e)))?;
        let mut reader = response.into_body().into_reader();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 65536];
        loop {
            let n = reader
                .read(&mut tmp)
                .map_err(|e: std::io::Error| InstallError::DownloadFailed(e.to_string()))?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            on_progress(buf.len());
        }
        Ok(buf)
    }

    fn fetch_string(&self, url: &str) -> Option<String> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .build()
            .new_agent();
        let mut body = agent.get(url).call().ok()?.into_body();
        let s = body.read_to_string().ok()?;
        Some(s.trim().to_owned())
    }
}

// ---------------------------------------------------------------------------
// Installer
// ---------------------------------------------------------------------------

pub struct Installer;

impl Installer {
    /// Returns the path to a ready-to-execute `rust-analyzer` binary for `version`.
    ///
    /// Cache layout:
    ///   `~/.cache/faber/lsp/rust-analyzer/<version>/rust-analyzer`
    ///   `~/.cache/faber/lsp/rust-analyzer/<version>/rust-analyzer.sha256`
    pub fn install_or_check(
        version: &str,
        progress_cb: &mut dyn FnMut(&str),
        cache_root: Option<&Path>,
        fetcher: &dyn Fetcher,
    ) -> Result<PathBuf, InstallError> {
        let platform = Platform::current()?;
        let cache_dir = Self::cache_dir(version, cache_root);
        let bin_path = cache_dir.join("rust-analyzer");
        let sha256_path = cache_dir.join("rust-analyzer.sha256");

        // Cache hit path.
        if let Some(p) = check_cache(&bin_path, &sha256_path) {
            return Ok(p);
        }

        // Download.
        let artifact = platform.artifact_name();
        let base_url = format!(
            "https://github.com/rust-lang/rust-analyzer/releases/download/{version}/{artifact}"
        );

        let gz_bytes = Self::download_with_progress(fetcher, &base_url, progress_cb)?;

        progress_cb("Extracting...");
        let binary_bytes = Self::decompress_gz(&gz_bytes)?;

        progress_cb("Verifying...");
        let actual_hex = hex_sha256(&binary_bytes);

        // Try to fetch the companion checksum file; non-fatal on failure.
        let sha256_url = format!("{base_url}.sha256");
        let maybe_expected = Self::fetch_checksum(fetcher, &sha256_url);

        if let Some(expected_line) = maybe_expected {
            // The .sha256 file may be "<hash>  filename" or just "<hash>".
            let expected_hex = expected_line
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_lowercase();
            let actual_lower = actual_hex.to_lowercase();
            if expected_hex != actual_lower {
                return Err(InstallError::ChecksumMismatch {
                    expected: expected_hex,
                    actual: actual_lower,
                });
            }
        } else {
            log::warn!("rust-analyzer: no checksum file available — skipping verification");
        }

        // Persist atomically: write the binary to a temp path, chmod it, then rename
        // into place. The rename is atomic on POSIX, so a crash before it leaves only a
        // stray `.tmp` (no `.bin`, no `.sha256`) and the next start re-downloads cleanly.
        std::fs::create_dir_all(&cache_dir)?;
        let tmp_path = bin_path.with_extension("tmp");
        std::fs::write(&tmp_path, &binary_bytes)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&tmp_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&tmp_path, perms)?;
        }

        std::fs::rename(&tmp_path, &bin_path)?;

        // Cache the checksum for future runs.
        std::fs::write(&sha256_path, actual_hex.as_bytes())?;

        Ok(bin_path)
    }

    /// Returns the PATH that a login shell would expose.
    ///
    /// Needed on macOS where GUI-launched processes inherit a truncated PATH.
    pub fn login_shell_path() -> String {
        let shell_raw = std::env::var("SHELL").unwrap_or_default();
        let shell = if shell_raw.is_empty() {
            "/bin/sh".to_owned()
        } else {
            shell_raw
        };

        // Whitelist: only known safe POSIX shells.
        let basename = Path::new(&shell)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let allowed = matches!(basename, "bash" | "zsh" | "sh");
        let shell = if allowed { shell } else { "/bin/sh".to_owned() };

        // Spawn with a 5-second timeout.
        let mut child = match std::process::Command::new(&shell)
            .args(["-l", "-c", "echo $PATH"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return std::env::var("PATH").unwrap_or_default(),
        };

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        return std::env::var("PATH").unwrap_or_default();
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(_) => return std::env::var("PATH").unwrap_or_default(),
            }
        }

        let output = match child.wait_with_output() {
            Ok(o) => o,
            Err(_) => return std::env::var("PATH").unwrap_or_default(),
        };

        let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if path.is_empty() {
            std::env::var("PATH").unwrap_or_default()
        } else {
            path
        }
    }

    // ------------------------------------------------------------------
    // Private helpers
    // ------------------------------------------------------------------

    fn cache_dir(version: &str, cache_root: Option<&Path>) -> PathBuf {
        let base = match cache_root {
            Some(root) => root.to_owned(),
            None => {
                let home = std::env::var("HOME").unwrap_or_else(|_| "~".to_owned());
                PathBuf::from(home).join(".cache").join("faber").join("lsp")
            }
        };
        base.join("rust-analyzer").join(version)
    }

    /// Download `url` via the fetcher, calling `progress_cb` with KB-count during the transfer.
    fn download_with_progress(
        fetcher: &dyn Fetcher,
        url: &str,
        progress_cb: &mut dyn FnMut(&str),
    ) -> Result<Vec<u8>, InstallError> {
        progress_cb("Connecting...");
        let buf = fetcher.fetch_bytes_with_progress(url, &mut |bytes| {
            progress_cb(&format!("Downloading... {} KB", bytes / 1024));
        })?;
        Ok(buf)
    }

    /// Fetch a checksum URL; returns `None` if not found or on error.
    fn fetch_checksum(fetcher: &dyn Fetcher, url: &str) -> Option<String> {
        fetcher.fetch_string(url)
    }

    /// Decompress a gzip stream into raw bytes.
    fn decompress_gz(gz_bytes: &[u8]) -> Result<Vec<u8>, InstallError> {
        let cursor = std::io::Cursor::new(gz_bytes);
        let mut decoder = GzDecoder::new(cursor);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out)?;
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Standalone helpers (also used in tests)
// ---------------------------------------------------------------------------

/// Check whether the cached binary is present.
/// Returns `Some(path)` on a cache hit, `None` otherwise.
///
/// A cached binary is trusted on subsequent starts without re-hashing: verification
/// happens only once, right after a fresh download. Presence of the `.sha256` sidecar
/// signals a completed, verified install; its absence is treated as a legacy/manual install.
pub fn check_cache(bin_path: &Path, sha256_path: &Path) -> Option<PathBuf> {
    if !bin_path.exists() {
        return None;
    }

    // Whether or not the sidecar exists, the binary is present and accepted as-is:
    //   - sidecar present  → completed, previously-verified install
    //   - sidecar absent   → legacy / manual install
    let _ = sha256_path;
    Some(bin_path.to_owned())
}

fn hex_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // 1. Cache hit: binary + matching .sha256 file → returns path without HTTP.
    #[test]
    fn test_cache_hit_matching_checksum() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin_path = dir.path().join("rust-analyzer");
        let sha256_path = dir.path().join("rust-analyzer.sha256");

        let fake_binary = b"fake binary content for testing";
        std::fs::write(&bin_path, fake_binary).unwrap();

        let expected_hash = hex_sha256(fake_binary);
        std::fs::write(&sha256_path, expected_hash.as_bytes()).unwrap();

        let result = check_cache(&bin_path, &sha256_path);
        assert_eq!(result, Some(bin_path));
    }

    // Cache hit: sidecar present → binary trusted without re-hashing, regardless of the
    // stored hash value. Verification runs only once, right after a fresh download.
    #[test]
    fn test_cache_hit_sidecar_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin_path = dir.path().join("rust-analyzer");
        let sha256_path = dir.path().join("rust-analyzer.sha256");

        std::fs::write(&bin_path, b"fake binary").unwrap();
        // A non-matching hash: `check_cache` no longer re-verifies, so this is still a hit.
        std::fs::write(
            &sha256_path,
            b"0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();

        let result = check_cache(&bin_path, &sha256_path);
        assert_eq!(result, Some(bin_path.clone()));
        assert!(
            bin_path.exists(),
            "binary must not be touched on cache check"
        );
    }

    // Cache hit: no .sha256 file → accepted as-is.
    #[test]
    fn test_cache_hit_no_sha256_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin_path = dir.path().join("rust-analyzer");
        let sha256_path = dir.path().join("rust-analyzer.sha256");

        std::fs::write(&bin_path, b"binary without checksum").unwrap();

        let result = check_cache(&bin_path, &sha256_path);
        assert_eq!(result, Some(bin_path));
    }

    // 2. Platform detection: must not panic or return UnsupportedPlatform on CI.
    #[test]
    fn test_platform_current() {
        let result = Platform::current();
        // On macOS/Linux CI this should succeed; other platforms get UnsupportedPlatform.
        match result {
            Ok(p) => {
                let name = p.artifact_name();
                assert!(
                    name.starts_with("rust-analyzer-"),
                    "unexpected artifact: {name}"
                );
            }
            Err(InstallError::UnsupportedPlatform) => {
                // Acceptable — just make sure it didn't panic.
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    // 3. Shell whitelist: unknown shell path falls back without panicking.
    #[test]
    fn test_login_shell_path_invalid_shell() {
        // Point $SHELL at a non-whitelisted name; login_shell_path must not panic.
        let original = std::env::var("SHELL").ok();
        // SAFETY: single-threaded test; no other thread reads $SHELL concurrently.
        unsafe { std::env::set_var("SHELL", "/usr/bin/fish") };

        let path = Installer::login_shell_path();
        // Should return something (the current PATH at minimum).
        let _ = path; // just ensure no panic

        // Restore.
        // SAFETY: same as above.
        unsafe {
            match original {
                Some(v) => std::env::set_var("SHELL", v),
                None => std::env::remove_var("SHELL"),
            }
        }
    }

    // Download path is exercised end-to-end without a network via a fake fetcher.
    #[test]
    fn test_install_with_fake_fetcher() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;

        let fake_binary = b"fake rust-analyzer binary";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(fake_binary).unwrap();
        let gz_bytes = encoder.finish().unwrap();

        struct FakeFetcher {
            gz: Vec<u8>,
        }
        impl Fetcher for FakeFetcher {
            fn fetch_bytes(&self, _url: &str) -> Result<Vec<u8>, InstallError> {
                Ok(self.gz.clone())
            }
            fn fetch_string(&self, _url: &str) -> Option<String> {
                None // no checksum file
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let mut progress_msgs = vec![];
        let result = Installer::install_or_check(
            "test-version",
            &mut |msg| progress_msgs.push(msg.to_owned()),
            Some(dir.path()),
            &FakeFetcher { gz: gz_bytes },
        );

        assert!(result.is_ok(), "expected Ok, got {:?}", result);
        let bin_path = result.unwrap();
        assert!(bin_path.exists());
        let content = std::fs::read(&bin_path).unwrap();
        assert_eq!(content, fake_binary);
    }

    // login_shell_path with a valid shell (zsh/bash) returns a non-empty PATH.
    #[test]
    fn test_login_shell_path_valid_shell() {
        let path = Installer::login_shell_path();
        // Should be non-empty on any developer machine.
        assert!(!path.is_empty());
    }
}
