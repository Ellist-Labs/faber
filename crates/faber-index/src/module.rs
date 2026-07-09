//! The `IndexModule` contract plus the typed handle the engine hands back.
//!
//! A module declares what per-file inputs it needs (`InputNeeds`), decides which
//! files it `accepts`, turns each file into `(key_suffix, value)` pairs via
//! `index`, and reconstructs an immutable `Snapshot` from the whole key/value set
//! via `publish`. The engine owns scheduling, persistence, and the swap of the
//! published snapshot into a lock-free `SnapshotHandle`.

use std::{sync::Arc, time::SystemTime};

use bitflags::bitflags;
use faber_lang::LanguageId;

bitflags! {
    /// Per-file inputs a module wants materialized before `index` runs. The
    /// engine unions these across all modules and only reads file bodies / parses
    /// trees when at least one module asked for them.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct InputNeeds: u8 {
        /// File metadata only (path, size, mtime, language). Always available.
        const META = 0b001;
        /// Decoded UTF-8 file body. Skipped for binaries.
        const TEXT = 0b010;
        /// Parsed tree-sitter tree. Requires a supported language + `TEXT`.
        const SYNTAX = 0b100;
    }
}

/// Per-file metadata resolved once by the engine and shared with every module.
///
/// `rel_path` is kept as raw bytes, sorted bytewise, so non-UTF8 paths survive
/// the round-trip through the store's key layout unharmed.
#[derive(Debug, Clone)]
pub struct FileMeta {
    /// Path relative to the project root, raw bytes, bytewise-sorted.
    pub rel_path: Arc<[u8]>,
    pub size: u64,
    pub mtime: SystemTime,
    /// True when the file is only visible because ignored/hidden files were kept.
    pub is_ignored: bool,
    /// Resolved once by the engine from the path's extension; `None` = unsupported.
    pub language: Option<LanguageId>,
}

/// The materialized inputs handed to `IndexModule::index` for one file. `text`
/// is present iff some module declared `TEXT` and the file is not binary;
/// `syntax` is present iff some module declared `SYNTAX` and the language parsed.
pub struct FileInput<'a> {
    pub meta: &'a FileMeta,
    pub text: Option<&'a str>,
    pub syntax: Option<&'a tree_sitter::Tree>,
}

/// A module's key relative to its file: the stored key is `{rel_path}\0{suffix}`.
pub type KeySuffix = Vec<u8>;

/// A subsystem that derives a queryable snapshot from the project's files.
///
/// Implementations must be cheap to share (`Send + Sync`) and hold no per-run
/// state — the engine calls `index` from many worker threads concurrently.
pub trait IndexModule: Send + Sync + 'static {
    /// Immutable, shareable view this module publishes (e.g. a file list).
    type Snapshot: Send + Sync + 'static;

    /// Stable identifier; also the store DB namespace. Never rename in place —
    /// a rename is a new module and orphans the old one's data.
    fn name(&self) -> &'static str;

    /// Bumped whenever `index`/`publish` output format changes; a bump forces a
    /// full rebuild of this module (and only this module).
    fn version(&self) -> u32;

    /// Which per-file inputs `index` requires.
    fn needs(&self) -> InputNeeds;

    /// Whether this module indexes the given file at all.
    fn accepts(&self, meta: &FileMeta) -> bool;

    /// Turn one file into its `(key_suffix, value)` entries. Called concurrently.
    fn index(&self, input: &FileInput) -> anyhow::Result<Vec<(KeySuffix, Vec<u8>)>>;

    /// Fold every `(full_key, value)` entry this module owns into a `Snapshot`.
    /// Keys arrive in bytewise-sorted order (store iteration order).
    fn publish(
        &self,
        entries: &mut dyn Iterator<Item = (&[u8], &[u8])>,
    ) -> anyhow::Result<Self::Snapshot>;
}

/// Lifecycle state of a module's published snapshot, surfaced to the UI.
#[derive(Debug, Clone)]
pub enum ModuleState {
    /// Never built — no snapshot yet.
    Cold,
    /// A (re)index is in flight; `done`/`total` drive a progress bar.
    Building { done: usize, total: usize },
    /// A snapshot is live; `generation` increments on every swap.
    Ready { generation: u64 },
}

/// Typed, lock-free handle to a module's latest snapshot plus its state.
///
/// Returned by `IndexEngine::register`. Readers call `load()` on the hot path
/// (a single atomic load); the engine mutates it through the crate-private
/// setters. Cheap to clone.
pub struct SnapshotHandle<S: Send + Sync + 'static> {
    snapshot: Arc<arc_swap::ArcSwapOption<S>>,
    state: Arc<std::sync::Mutex<ModuleState>>,
}

impl<S: Send + Sync + 'static> Clone for SnapshotHandle<S> {
    fn clone(&self) -> Self {
        Self {
            snapshot: self.snapshot.clone(),
            state: self.state.clone(),
        }
    }
}

impl<S: Send + Sync + 'static> SnapshotHandle<S> {
    /// The current snapshot, or `None` until the first publish. One atomic load.
    pub fn load(&self) -> Option<Arc<S>> {
        self.snapshot.load_full()
    }

    /// The module's current lifecycle state.
    pub fn state(&self) -> ModuleState {
        self.state.lock().unwrap().clone()
    }

    pub(crate) fn new() -> Self {
        Self {
            snapshot: Arc::new(arc_swap::ArcSwapOption::empty()),
            state: Arc::new(std::sync::Mutex::new(ModuleState::Cold)),
        }
    }

    pub(crate) fn set_state(&self, s: ModuleState) {
        *self.state.lock().unwrap() = s;
    }

    /// Publish a new snapshot and mark the module `Ready { generation }`.
    pub(crate) fn swap(&self, snap: Arc<S>, generation: u64) {
        self.snapshot.store(Some(snap));
        self.set_state(ModuleState::Ready { generation });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial module that counts words per file and publishes the total.
    struct WordCount;

    impl IndexModule for WordCount {
        type Snapshot = usize;

        fn name(&self) -> &'static str {
            "wordcount"
        }
        fn version(&self) -> u32 {
            1
        }
        fn needs(&self) -> InputNeeds {
            InputNeeds::TEXT
        }
        fn accepts(&self, _meta: &FileMeta) -> bool {
            true
        }
        fn index(&self, input: &FileInput) -> anyhow::Result<Vec<(KeySuffix, Vec<u8>)>> {
            let count = input
                .text
                .map(|t| t.split_whitespace().count())
                .unwrap_or(0);
            Ok(vec![(Vec::new(), (count as u64).to_le_bytes().to_vec())])
        }
        fn publish(
            &self,
            entries: &mut dyn Iterator<Item = (&[u8], &[u8])>,
        ) -> anyhow::Result<usize> {
            let mut total = 0usize;
            for (_key, val) in entries {
                let arr: [u8; 8] = val.try_into()?;
                total += u64::from_le_bytes(arr) as usize;
            }
            Ok(total)
        }
    }

    fn meta(path: &[u8]) -> FileMeta {
        FileMeta {
            rel_path: Arc::from(path),
            size: 0,
            mtime: SystemTime::UNIX_EPOCH,
            is_ignored: false,
            language: None,
        }
    }

    #[test]
    fn dummy_module_indexes_and_publishes() {
        let m = WordCount;
        let meta_a = meta(b"a.txt");
        let input = FileInput {
            meta: &meta_a,
            text: Some("one two three"),
            syntax: None,
        };
        let entries = m.index(&input).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, 3u64.to_le_bytes());

        // Two files with 3 and 2 words -> published total is 5.
        let a = 3u64.to_le_bytes();
        let b = 2u64.to_le_bytes();
        let mut it = [
            (b"a.txt\0".as_slice(), a.as_slice()),
            (b"b.txt\0".as_slice(), b.as_slice()),
        ]
        .into_iter();
        assert_eq!(m.publish(&mut it).unwrap(), 5);
    }

    #[test]
    fn input_needs_union_flags() {
        let both = InputNeeds::TEXT | InputNeeds::SYNTAX;
        assert!(both.contains(InputNeeds::TEXT));
        assert!(both.contains(InputNeeds::SYNTAX));
        assert!(!both.contains(InputNeeds::META));
    }

    #[test]
    fn snapshot_handle_starts_cold_then_readies() {
        let h: SnapshotHandle<usize> = SnapshotHandle::new();
        assert!(h.load().is_none());
        assert!(matches!(h.state(), ModuleState::Cold));

        h.set_state(ModuleState::Building { done: 1, total: 4 });
        assert!(matches!(
            h.state(),
            ModuleState::Building { done: 1, total: 4 }
        ));

        h.swap(Arc::new(42usize), 1);
        assert_eq!(*h.load().unwrap(), 42);
        assert!(matches!(h.state(), ModuleState::Ready { generation: 1 }));

        // Clones observe the same underlying cell.
        let clone = h.clone();
        h.swap(Arc::new(99usize), 2);
        assert_eq!(*clone.load().unwrap(), 99);
    }
}
