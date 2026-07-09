//! Scan phase: walk (or stat) the project, resolve per-file metadata, and
//! merge-join against each module's persisted stamps to find the dirty set.
//!
//! Cheap gate first (mtime + size), blake3 only when the gate trips. A `Verify`
//! scope re-hashes unconditionally. Binary files are content-skipped but still
//! reported in `all_meta` so META-only modules index them.

use std::{collections::BTreeSet, path::Path, sync::Arc, time::SystemTime};

use anyhow::{Context, Result};
use ignore::WalkBuilder;

use crate::{files::INDEX_LIMIT, module::FileMeta, store::IndexStore, trigger::ScanScope};
use faber_lang::LanguageRegistry;

/// First-bytes window sniffed for a NUL to classify binaries.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// Why a file landed in the dirty set — drives logging and, later, priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtyReason {
    /// No stamp recorded — never indexed.
    New,
    /// mtime or size differs from the recorded stamp.
    MtimeOrSizeChanged,
    /// mtime/size matched but the content hash differs (e.g. `Verify`).
    HashChanged,
    /// A module bumped its version — everything it owns is stale.
    ModuleVersionBump,
}

/// Output of one scan: every live file (for META modules) plus the subset that
/// needs content (re)indexing.
pub struct ScanResult {
    /// Every file found this scan, bytewise-sorted on `rel_path`.
    pub all_meta: Vec<FileMeta>,
    /// Files whose content must be re-indexed, with the reason.
    pub dirty: Vec<(FileMeta, DirtyReason)>,
}

/// Walk/stat per `scope`, then decide dirtiness against `module_names`' stamps.
///
/// A file is dirty when *any* content module lacks a matching stamp for it. The
/// mtime+size gate is checked first; blake3 runs only when the gate trips, or
/// always under `ScanScope::Verify`.
pub(crate) fn scan_tree(
    root: &Path,
    scope: &ScanScope,
    store: &IndexStore,
    module_names: &[&'static str],
    registry: &LanguageRegistry,
) -> Result<ScanResult> {
    let raw = collect_files(root, scope)?;
    let verify = matches!(scope, ScanScope::Verify);

    let mut all_meta = Vec::with_capacity(raw.len());
    let mut dirty = Vec::new();

    for rf in raw {
        let meta = FileMeta {
            rel_path: Arc::from(rf.rel_path.into_boxed_slice()),
            size: rf.size,
            mtime: rf.mtime,
            is_ignored: rf.is_ignored,
            language: registry
                .language_for_path(&rf.abs_path)
                .map(|l| l.id.clone()),
        };

        if let Some(reason) = dirtiness(store, module_names, &meta, &rf.abs_path, verify)
            .context("dirtiness check")?
        {
            dirty.push((meta.clone(), reason));
        }
        all_meta.push(meta);
    }

    Ok(ScanResult { all_meta, dirty })
}

/// Decide whether `meta` is dirty for any content module. Returns the first
/// applicable reason, or `None` when every module already has a fresh stamp.
fn dirtiness(
    store: &IndexStore,
    module_names: &[&'static str],
    meta: &FileMeta,
    abs_path: &Path,
    verify: bool,
) -> Result<Option<DirtyReason>> {
    // A hash is computed at most once and reused across modules.
    let mut hash: Option<[u8; 32]> = None;

    for name in module_names {
        let stamp = store.get_stamp(name, &meta.rel_path)?;
        let Some(stamp) = stamp else {
            return Ok(Some(DirtyReason::New));
        };

        // Gate: mtime + size. Cheap, filesystem-only.
        if stamp.mtime != meta.mtime || stamp.size != meta.size {
            return Ok(Some(DirtyReason::MtimeOrSizeChanged));
        }

        // Under Verify, or when a module stored a hash, confirm by content.
        if verify || stamp.hash.is_some() {
            let h = match hash {
                Some(h) => h,
                None => {
                    let h = hash_file(abs_path)?;
                    hash = Some(h);
                    h
                }
            };
            // Dirty when the recorded hash is absent (can't confirm under Verify)
            // or differs from the freshly computed one.
            if stamp.hash != Some(h) {
                return Ok(Some(DirtyReason::HashChanged));
            }
        }
    }
    Ok(None)
}

/// blake3 of a file's full contents.
fn hash_file(path: &Path) -> Result<[u8; 32]> {
    let bytes = std::fs::read(path).with_context(|| format!("read for hash {path:?}"))?;
    Ok(*blake3::hash(&bytes).as_bytes())
}

/// Raw filesystem row before language resolution / dirtiness.
struct RawFile {
    rel_path: Vec<u8>,
    abs_path: std::path::PathBuf,
    size: u64,
    mtime: SystemTime,
    is_ignored: bool,
}

/// Gather the candidate file set for `scope`: a tree walk for `Walk`/`Verify`,
/// or targeted stats for `Paths`. Result is bytewise-sorted on `rel_path`.
fn collect_files(root: &Path, scope: &ScanScope) -> Result<Vec<RawFile>> {
    let mut out = match scope {
        ScanScope::Paths(paths) => collect_paths(root, paths)?,
        ScanScope::Walk | ScanScope::Verify => collect_walk(root)?,
    };
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(out)
}

/// Full walk honoring the same ignore rules as the editor's file index.
fn collect_walk(root: &Path) -> Result<Vec<RawFile>> {
    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(true)
        .git_ignore(true)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|e| e.file_name() != ".git");

    let mut out = Vec::new();
    for entry in walker.build().filter_map(|e| e.ok()) {
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        if let Some(rf) = raw_file(root, entry.path(), false) {
            out.push(rf);
        }
        if out.len() >= INDEX_LIMIT {
            log::warn!("scan hit INDEX_LIMIT ({INDEX_LIMIT}); truncating");
            break;
        }
    }
    Ok(out)
}

/// Stat exactly the requested paths (watcher / save scope). Missing paths are
/// silently skipped — deletions are handled elsewhere.
fn collect_paths(root: &Path, paths: &BTreeSet<std::path::PathBuf>) -> Result<Vec<RawFile>> {
    let mut out = Vec::new();
    for p in paths {
        let abs = if p.is_absolute() {
            p.clone()
        } else {
            root.join(p)
        };
        if abs.is_file()
            && let Some(rf) = raw_file(root, &abs, false)
        {
            out.push(rf);
        }
    }
    Ok(out)
}

/// Build a `RawFile` from an absolute path, computing its root-relative bytes.
fn raw_file(root: &Path, abs: &Path, is_ignored: bool) -> Option<RawFile> {
    let rel = abs.strip_prefix(root).ok()?;
    let meta = std::fs::metadata(abs).ok()?;
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    Some(RawFile {
        rel_path: rel.as_os_str().as_encoded_bytes().to_vec(),
        abs_path: abs.to_path_buf(),
        size: meta.len(),
        mtime,
        is_ignored,
    })
}

/// True if the file's first [`BINARY_SNIFF_BYTES`] contain a NUL byte. Content
/// modules skip these; the engine calls this in the pipeline before reading.
pub fn is_binary(bytes: &[u8]) -> bool {
    let window = &bytes[..bytes.len().min(BINARY_SNIFF_BYTES)];
    window.contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::with_project;
    use std::time::Duration;

    fn write(root: &Path, rel: &str, body: &[u8]) -> std::path::PathBuf {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn binary_sniff_detects_nul() {
        assert!(is_binary(b"abc\0def"));
        assert!(!is_binary(b"plain text"));
        assert!(!is_binary(b""));
    }

    #[test]
    fn cold_scan_finds_all_files_dirty() {
        with_project(|root| {
            write(root, "src/main.rs", b"fn main() {}");
            write(root, "README.md", b"# hi");

            let store = IndexStore::open(root).unwrap();
            let reg = LanguageRegistry::with_defaults();
            let res = scan_tree(root, &ScanScope::Walk, &store, &["symbols"], &reg).unwrap();

            assert_eq!(res.all_meta.len(), 2);
            // Nothing stamped yet -> every file is New.
            assert_eq!(res.dirty.len(), 2);
            assert!(res.dirty.iter().all(|(_, r)| *r == DirtyReason::New));
            // Language resolved for the .rs file.
            let rs = res
                .all_meta
                .iter()
                .find(|m| m.rel_path.as_ref() == b"src/main.rs")
                .unwrap();
            assert_eq!(rs.language.as_ref().map(|l| l.0.as_str()), Some("rust"));
        });
    }

    #[test]
    fn warm_scan_with_matching_stamps_finds_nothing_dirty() {
        with_project(|root| {
            let f = write(root, "a.rs", b"fn a() {}");
            let store = IndexStore::open(root).unwrap();
            let reg = LanguageRegistry::with_defaults();

            // First cold scan: one dirty file.
            let cold = scan_tree(root, &ScanScope::Walk, &store, &["symbols"], &reg).unwrap();
            assert_eq!(cold.dirty.len(), 1);

            // Persist a stamp matching the file's current mtime+size (no hash).
            let fs_meta = std::fs::metadata(&f).unwrap();
            let stamp = crate::store::Stamp {
                mtime: fs_meta.modified().unwrap(),
                size: fs_meta.len(),
                hash: None,
            };
            store
                .write_batch("symbols", &[(b"a.rs".to_vec(), stamp, vec![])], false)
                .unwrap();

            // Warm scan: mtime+size gate passes -> not dirty.
            let warm = scan_tree(root, &ScanScope::Walk, &store, &["symbols"], &reg).unwrap();
            assert!(warm.dirty.is_empty(), "warm scan should find nothing dirty");
        });
    }

    #[test]
    fn mtime_change_marks_dirty() {
        with_project(|root| {
            let f = write(root, "a.rs", b"fn a() {}");
            let store = IndexStore::open(root).unwrap();
            let reg = LanguageRegistry::with_defaults();
            // Stamp with a stale mtime one hour in the past.
            let fs_meta = std::fs::metadata(&f).unwrap();
            let stale = crate::store::Stamp {
                mtime: fs_meta.modified().unwrap() - Duration::from_secs(3600),
                size: fs_meta.len(),
                hash: None,
            };
            store
                .write_batch("symbols", &[(b"a.rs".to_vec(), stale, vec![])], false)
                .unwrap();

            let res = scan_tree(root, &ScanScope::Walk, &store, &["symbols"], &reg).unwrap();
            assert_eq!(res.dirty.len(), 1);
            assert_eq!(res.dirty[0].1, DirtyReason::MtimeOrSizeChanged);
        });
    }

    #[test]
    fn verify_rehashes_even_when_mtime_matches() {
        with_project(|root| {
            let f = write(root, "a.rs", b"fn a() {}");
            let store = IndexStore::open(root).unwrap();
            let reg = LanguageRegistry::with_defaults();
            let fs_meta = std::fs::metadata(&f).unwrap();
            // Stamp has matching mtime+size but a WRONG hash -> Verify catches it.
            let stamp = crate::store::Stamp {
                mtime: fs_meta.modified().unwrap(),
                size: fs_meta.len(),
                hash: Some([0u8; 32]),
            };
            store
                .write_batch("symbols", &[(b"a.rs".to_vec(), stamp, vec![])], false)
                .unwrap();

            let res = scan_tree(root, &ScanScope::Verify, &store, &["symbols"], &reg).unwrap();
            assert_eq!(res.dirty.len(), 1);
            assert_eq!(res.dirty[0].1, DirtyReason::HashChanged);
        });
    }

    #[test]
    fn paths_scope_stats_only_named_files() {
        with_project(|root| {
            write(root, "a.rs", b"a");
            write(root, "b.rs", b"b");
            let store = IndexStore::open(root).unwrap();
            let reg = LanguageRegistry::with_defaults();
            let scope = ScanScope::Paths(BTreeSet::from([std::path::PathBuf::from("a.rs")]));
            let res = scan_tree(root, &scope, &store, &["symbols"], &reg).unwrap();
            // Only a.rs was stat'd.
            assert_eq!(res.all_meta.len(), 1);
            assert_eq!(res.all_meta[0].rel_path.as_ref(), b"a.rs");
        });
    }
}
