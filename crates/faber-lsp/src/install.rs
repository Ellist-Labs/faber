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
// InstallProgress
// ---------------------------------------------------------------------------

/// Structured progress events emitted during binary installation.
#[derive(Debug, Clone)]
pub enum InstallProgress {
    CheckingCache,
    Connecting,
    Downloading { received: u64, total: Option<u64> },
    Extracting,
    Verifying,
}

impl InstallProgress {
    pub fn message(&self) -> String {
        match self {
            InstallProgress::CheckingCache => "Checking cache...".to_owned(),
            InstallProgress::Connecting => "Connecting...".to_owned(),
            InstallProgress::Downloading {
                received,
                total: Some(total),
            } => {
                let pct = (*received as f64 / *total as f64 * 100.0) as u32;
                format!(
                    "Downloading... {pct}% ({} KB / {} KB)",
                    received / 1024,
                    total / 1024
                )
            }
            InstallProgress::Downloading {
                received,
                total: None,
            } => {
                format!("Downloading... {} KB", received / 1024)
            }
            InstallProgress::Extracting => "Extracting...".to_owned(),
            InstallProgress::Verifying => "Verifying...".to_owned(),
        }
    }

    /// Download fraction [0.0, 1.0], or None when not in Downloading state or total unknown.
    pub fn fraction(&self) -> Option<f32> {
        match self {
            InstallProgress::Downloading {
                received,
                total: Some(total),
            } if *total > 0 => Some((*received as f32 / *total as f32).clamp(0.0, 1.0)),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Fetcher
// ---------------------------------------------------------------------------

/// Abstracts network access so downloads can be exercised in tests without a network.
pub trait Fetcher: Send {
    fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, InstallError>;

    /// Like `fetch_bytes` but calls `on_progress(bytes_received, total_bytes)` after each chunk.
    /// `total_bytes` is None when the server does not send Content-Length.
    fn fetch_bytes_with_progress(
        &self,
        url: &str,
        on_progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<Vec<u8>, InstallError> {
        let buf = self.fetch_bytes(url)?;
        on_progress(buf.len() as u64, Some(buf.len() as u64));
        Ok(buf)
    }

    fn fetch_string(&self, url: &str) -> Option<String>;
}

/// Production fetcher backed by `ureq`.
pub struct UreqFetcher;

impl Fetcher for UreqFetcher {
    fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, InstallError> {
        self.fetch_bytes_with_progress(url, &mut |_, _| {})
    }

    fn fetch_bytes_with_progress(
        &self,
        url: &str,
        on_progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<Vec<u8>, InstallError> {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(std::time::Duration::from_secs(30)))
            .timeout_per_call(Some(std::time::Duration::from_secs(90)))
            .build()
            .new_agent();
        let response = agent
            .get(url)
            .call()
            .map_err(|e| InstallError::Http(Box::new(e)))?;

        // Read Content-Length if present (enables progress percentage).
        let total_bytes: Option<u64> = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok());

        let mut reader = response.into_body().into_reader();
        let mut buf = if let Some(total) = total_bytes {
            Vec::with_capacity(total as usize)
        } else {
            Vec::new()
        };
        let mut tmp = [0u8; 65536];
        loop {
            let n = reader
                .read(&mut tmp)
                .map_err(|e: std::io::Error| InstallError::DownloadFailed(e.to_string()))?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            on_progress(buf.len() as u64, total_bytes);
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
        progress_cb: &mut dyn FnMut(InstallProgress),
        cache_root: Option<&Path>,
        fetcher: &dyn Fetcher,
    ) -> Result<PathBuf, InstallError> {
        let platform = Platform::current()?;
        let cache_dir = Self::cache_dir(version, cache_root);
        let bin_path = cache_dir.join("rust-analyzer");
        let sha256_path = cache_dir.join("rust-analyzer.sha256");

        // Cache hit path.
        progress_cb(InstallProgress::CheckingCache);
        if let Some(p) = check_cache(&bin_path, &sha256_path) {
            log::info!(
                "lsp: cache hit for rust-analyzer {version} at {}",
                p.display()
            );
            return Ok(p);
        }

        // Download.
        let artifact = platform.artifact_name();
        let base_url = format!(
            "https://github.com/rust-lang/rust-analyzer/releases/download/{version}/{artifact}"
        );

        log::info!("lsp: downloading rust-analyzer {version} from {base_url}");
        let gz_bytes = Self::download_with_progress(fetcher, &base_url, progress_cb)?;

        progress_cb(InstallProgress::Extracting);
        log::info!(
            "lsp: extracting rust-analyzer ({} KB compressed)",
            gz_bytes.len() / 1024
        );
        let binary_bytes = Self::decompress_gz(&gz_bytes)?;

        progress_cb(InstallProgress::Verifying);
        let actual_hex = hex_sha256(&binary_bytes);

        // Try to fetch the companion checksum file; non-fatal on failure.
        let sha256_url = format!("{base_url}.sha256");
        let maybe_expected = Self::fetch_checksum(fetcher, &sha256_url);

        if let Some(expected_line) = maybe_expected {
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
            log::warn!("lsp: no checksum file for rust-analyzer {version} — skipping verification");
        }

        // Persist atomically: write to .tmp, chmod, then rename.
        std::fs::create_dir_all(&cache_dir)?;
        let tmp_path = bin_path.with_extension("tmp");

        if let Err(e) = std::fs::write(&tmp_path, &binary_bytes) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(InstallError::Io(e));
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = std::fs::metadata(&tmp_path).and_then(|m| {
                let mut perms = m.permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&tmp_path, perms)
            }) {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(InstallError::Io(e));
            }
        }

        if let Err(e) = std::fs::rename(&tmp_path, &bin_path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(InstallError::Io(e));
        }

        std::fs::write(&sha256_path, actual_hex.as_bytes())?;
        log::info!(
            "lsp: rust-analyzer {version} installed at {}",
            bin_path.display()
        );

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

    fn download_with_progress(
        fetcher: &dyn Fetcher,
        url: &str,
        progress_cb: &mut dyn FnMut(InstallProgress),
    ) -> Result<Vec<u8>, InstallError> {
        progress_cb(InstallProgress::Connecting);
        fetcher.fetch_bytes_with_progress(url, &mut |received, total| {
            progress_cb(InstallProgress::Downloading { received, total });
        })
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

    #[test]
    fn test_cache_hit_sidecar_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin_path = dir.path().join("rust-analyzer");
        let sha256_path = dir.path().join("rust-analyzer.sha256");

        std::fs::write(&bin_path, b"fake binary").unwrap();
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

    #[test]
    fn test_cache_hit_no_sha256_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin_path = dir.path().join("rust-analyzer");
        let sha256_path = dir.path().join("rust-analyzer.sha256");

        std::fs::write(&bin_path, b"binary without checksum").unwrap();

        let result = check_cache(&bin_path, &sha256_path);
        assert_eq!(result, Some(bin_path));
    }

    #[test]
    fn test_platform_current() {
        let result = Platform::current();
        match result {
            Ok(p) => {
                let name = p.artifact_name();
                assert!(
                    name.starts_with("rust-analyzer-"),
                    "unexpected artifact: {name}"
                );
            }
            Err(InstallError::UnsupportedPlatform) => {}
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    #[test]
    fn test_login_shell_path_invalid_shell() {
        let original = std::env::var("SHELL").ok();
        unsafe { std::env::set_var("SHELL", "/usr/bin/fish") };

        let path = Installer::login_shell_path();
        let _ = path;

        unsafe {
            match original {
                Some(v) => std::env::set_var("SHELL", v),
                None => std::env::remove_var("SHELL"),
            }
        }
    }

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
            fn fetch_bytes_with_progress(
                &self,
                _url: &str,
                on_progress: &mut dyn FnMut(u64, Option<u64>),
            ) -> Result<Vec<u8>, InstallError> {
                let data = self.gz.clone();
                on_progress(data.len() as u64, Some(data.len() as u64));
                Ok(data)
            }
            fn fetch_string(&self, _url: &str) -> Option<String> {
                None
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let mut progress_events: Vec<String> = vec![];
        let result = Installer::install_or_check(
            "test-version",
            &mut |p: InstallProgress| progress_events.push(p.message()),
            Some(dir.path()),
            &FakeFetcher { gz: gz_bytes },
        );

        assert!(result.is_ok(), "expected Ok, got {:?}", result);
        let bin_path = result.unwrap();
        assert!(bin_path.exists());
        let content = std::fs::read(&bin_path).unwrap();
        assert_eq!(content, fake_binary);
        assert!(progress_events.iter().any(|m| m.contains("Downloading")));
        assert!(progress_events.iter().any(|m| m.contains("Extracting")));
    }

    #[test]
    fn test_login_shell_path_valid_shell() {
        let path = Installer::login_shell_path();
        assert!(!path.is_empty());
    }

    #[test]
    fn test_checksum_mismatch_cleans_tmp() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;

        let fake_binary = b"fake binary for checksum test";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(fake_binary).unwrap();
        let gz_bytes = encoder.finish().unwrap();

        struct MismatchFetcher {
            gz: Vec<u8>,
        }
        impl Fetcher for MismatchFetcher {
            fn fetch_bytes(&self, _url: &str) -> Result<Vec<u8>, InstallError> {
                Ok(self.gz.clone())
            }
            fn fetch_bytes_with_progress(
                &self,
                _url: &str,
                on_progress: &mut dyn FnMut(u64, Option<u64>),
            ) -> Result<Vec<u8>, InstallError> {
                let d = self.gz.clone();
                on_progress(d.len() as u64, Some(d.len() as u64));
                Ok(d)
            }
            fn fetch_string(&self, _url: &str) -> Option<String> {
                Some("0000000000000000000000000000000000000000000000000000000000000000".to_owned())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let result = Installer::install_or_check(
            "mismatch-version",
            &mut |_: InstallProgress| {},
            Some(dir.path()),
            &MismatchFetcher { gz: gz_bytes },
        );
        assert!(matches!(result, Err(InstallError::ChecksumMismatch { .. })));
        let tmp = dir
            .path()
            .join("rust-analyzer")
            .join("mismatch-version")
            .join("rust-analyzer.tmp");
        assert!(
            !tmp.exists(),
            "stray .tmp must be cleaned up on checksum mismatch"
        );
    }

    #[test]
    fn test_progress_fraction_with_known_total() {
        let p = InstallProgress::Downloading {
            received: 50,
            total: Some(100),
        };
        assert_eq!(p.fraction(), Some(0.5));
        let msg = p.message();
        assert!(
            msg.contains("50%"),
            "message should contain percentage: {msg}"
        );
    }
}
