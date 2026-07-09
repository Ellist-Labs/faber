//! The engine facade: the built-in `FilesModule`, the `IndexEngine` that owns the
//! store/registry/modules, and the single-threaded run loop that coalesces
//! triggers, scans, indexes, and publishes.
//!
//! META-only modules (like `files`) are indexed inline from the scan's `all_meta`
//! — no pipeline. Content modules go through the worker pool. Each module's
//! snapshot is published into its typed `SnapshotHandle` as its writes land.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::Result;

use crate::{
    files::{FileEntry, FileIndexSnapshot, extension_of},
    module::{
        FileInput, FileMeta, IndexModule, InputNeeds, KeySuffix, ModuleState, SnapshotHandle,
    },
    pipeline::{ErasedModule, Pipeline, WorkItem, WorkResult},
    progress::{Phase, ProgressEmitter, ProgressReceiver, progress_channel},
    scanner::{ScanResult, scan_tree},
    store::{IndexStore, Stamp},
    trigger::{IndexTrigger, ScanScope},
};
use faber_lang::LanguageRegistry;

// ── FilesModule ──────────────────────────────────────────────────────────────

/// The built-in file-list module. META-only: it turns every file's metadata into
/// one entry and publishes a `FileIndexSnapshot` for the finder.
pub struct FilesModule;

impl FilesModule {
    /// Encode a file row: len-prefixed path, `name_off` (u32 LE), `is_ignored`
    /// flag. Hand-rolled and fixed-width to avoid a serde dependency on the hot
    /// per-file path.
    fn encode_row(rel_path: &str, name_off: u32, is_ignored: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + rel_path.len() + 4 + 1);
        out.extend_from_slice(&(rel_path.len() as u32).to_le_bytes());
        out.extend_from_slice(rel_path.as_bytes());
        out.extend_from_slice(&name_off.to_le_bytes());
        out.push(is_ignored as u8);
        out
    }

    fn decode_row(bytes: &[u8]) -> Option<(String, u32, bool)> {
        if bytes.len() < 4 {
            return None;
        }
        let plen = u32::from_le_bytes(bytes[0..4].try_into().ok()?) as usize;
        let rest = &bytes[4..];
        if rest.len() < plen + 5 {
            return None;
        }
        let rel_path = String::from_utf8(rest[..plen].to_vec()).ok()?;
        let name_off = u32::from_le_bytes(rest[plen..plen + 4].try_into().ok()?);
        let is_ignored = rest[plen + 4] != 0;
        Some((rel_path, name_off, is_ignored))
    }
}

impl IndexModule for FilesModule {
    type Snapshot = FileIndexSnapshot;

    fn name(&self) -> &'static str {
        "files"
    }
    fn version(&self) -> u32 {
        1
    }
    fn needs(&self) -> InputNeeds {
        InputNeeds::META
    }
    fn accepts(&self, _meta: &FileMeta) -> bool {
        true
    }

    fn index(&self, input: &FileInput) -> Result<Vec<(KeySuffix, Vec<u8>)>> {
        // rel_path is bytewise; the finder needs a String. Non-UTF8 paths are
        // lossily rendered (they can't participate in text search anyway).
        let rel_path = String::from_utf8_lossy(&input.meta.rel_path).into_owned();
        let name_off = rel_path.rfind('/').map(|i| i + 1).unwrap_or(0) as u32;
        let row = Self::encode_row(&rel_path, name_off, input.meta.is_ignored);
        // One entry per file: empty key suffix.
        Ok(vec![(Vec::new(), row)])
    }

    fn publish(
        &self,
        entries: &mut dyn Iterator<Item = (&[u8], &[u8])>,
    ) -> Result<FileIndexSnapshot> {
        let mut file_entries: Vec<FileEntry> = Vec::new();
        let mut ext_counts: HashMap<String, u32> = HashMap::new();

        for (_key, value) in entries {
            let Some((rel_path, name_off, is_ignored)) = Self::decode_row(value) else {
                continue;
            };
            if !is_ignored && let Some(ext) = extension_of(&rel_path[name_off as usize..]) {
                *ext_counts.entry(ext).or_insert(0) += 1;
            }
            file_entries.push(FileEntry {
                rel_path,
                name_off,
                is_ignored,
            });
        }
        // Keys are bytewise-sorted, but the String rel_path may reorder under
        // lossy decode; re-sort to satisfy the finder's binary-search invariant.
        file_entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

        let mut extensions: Vec<(String, u32)> = ext_counts.into_iter().collect();
        extensions.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        Ok(FileIndexSnapshot {
            root: PathBuf::new(),
            entries: file_entries,
            extensions,
            truncated: false,
        })
    }
}

// ── IndexEngine ──────────────────────────────────────────────────────────────

/// Publishes a module's store data into its typed `SnapshotHandle`, erasing the
/// `Snapshot` type. Takes the generation to stamp on the swap.
type PublishFn = Box<dyn Fn(&IndexStore, u64) -> Result<()> + Send + Sync>;

/// Inline META indexer for one file, erasing the concrete module.
type MetaIndexFn = Box<dyn Fn(&FileMeta) -> Result<Vec<(KeySuffix, Vec<u8>)>> + Send + Sync>;

/// Sets a module's lifecycle state, erasing the `Snapshot` type.
type SetStateFn = Box<dyn Fn(ModuleState) + Send + Sync>;

/// A registered module plus everything the engine needs to drive it generically:
/// the concrete `IndexModule` (for META inline indexing + publish) behind a
/// type-erasing publisher, and its typed handle.
struct RegisteredModule {
    name: &'static str,
    version: u32,
    needs: InputNeeds,
    /// Publishes from the store into the module's `SnapshotHandle`.
    publish_into_handle: PublishFn,
    /// Runs META inline indexing for one file (META-only modules use this instead
    /// of the pipeline). `None` for content modules.
    index_meta: Option<MetaIndexFn>,
    /// Set the module's lifecycle state.
    set_state: SetStateFn,
    /// The erased module for the content pipeline (only for content modules).
    erased: Option<Arc<Mutex<Option<ErasedModule>>>>,
}

/// The indexing engine. Owns the store, registry, module set, and the trigger
/// channel feeding the run loop.
pub struct IndexEngine {
    root: PathBuf,
    store: Arc<IndexStore>,
    registry: Arc<LanguageRegistry>,
    modules: Mutex<Vec<RegisteredModule>>,
    trigger_tx: crossbeam_channel::Sender<IndexTrigger>,
    trigger_rx: crossbeam_channel::Receiver<IndexTrigger>,
    progress_emitter: ProgressEmitter,
    progress_receiver: Mutex<Option<ProgressReceiver>>,
    generation: AtomicU64,
}

impl IndexEngine {
    /// Open the store under `index_dir` scope and build an engine rooted at
    /// `root`. Registration happens next via [`Self::register`], then [`start`].
    pub fn new(root: PathBuf, registry: Arc<LanguageRegistry>) -> Result<Self> {
        let store = Arc::new(IndexStore::open(&root)?);
        store.touch_last_opened().ok();
        let (trigger_tx, trigger_rx) = crossbeam_channel::unbounded();
        let (progress_emitter, progress_receiver) = progress_channel();
        Ok(Self {
            root,
            store,
            registry,
            modules: Mutex::new(Vec::new()),
            trigger_tx,
            trigger_rx,
            progress_emitter,
            progress_receiver: Mutex::new(Some(progress_receiver)),
            generation: AtomicU64::new(0),
        })
    }

    /// Register a module and return its typed snapshot handle. Must be called
    /// before [`start`]. On a per-module version bump the module's store data is
    /// dropped so it rebuilds cleanly.
    pub fn register<M: IndexModule>(&mut self, m: M) -> SnapshotHandle<M::Snapshot> {
        let handle: SnapshotHandle<M::Snapshot> = SnapshotHandle::new();
        let m = Arc::new(m);
        let name = m.name();
        let version = m.version();
        let needs = m.needs();

        // Version-bump gate: drop stale data so this module rebuilds.
        if let Ok(Some(recorded)) = self.store.module_version(name)
            && recorded != version
        {
            let _ = self.store.drop_module(name);
        }

        let publish_handle = handle.clone();
        let publish_m = m.clone();
        let publish_into_handle: PublishFn = Box::new(move |store, generation| {
            let rows: Vec<(Vec<u8>, Vec<u8>)> = store
                .iter_data(publish_m.name())
                .map_or_else(|_| Vec::new(), |it| it.filter_map(|r| r.ok()).collect());
            let mut it = rows.iter().map(|(k, v)| (k.as_slice(), v.as_slice()));
            let snap = publish_m.publish(&mut it)?;
            publish_handle.swap(Arc::new(snap), generation);
            Ok(())
        });

        let state_handle = handle.clone();
        let set_state: SetStateFn = Box::new(move |s| state_handle.set_state(s));

        let is_meta_only = needs == InputNeeds::META;
        let index_meta = if is_meta_only {
            let meta_m = m.clone();
            let f: MetaIndexFn = Box::new(move |meta| {
                let input = FileInput {
                    meta,
                    text: None,
                    syntax: None,
                };
                meta_m.index(&input)
            });
            Some(f)
        } else {
            None
        };

        let erased = if is_meta_only {
            None
        } else {
            Some(Arc::new(Mutex::new(Some(ErasedModule::new(m)))))
        };

        self.modules.lock().unwrap().push(RegisteredModule {
            name,
            version,
            needs,
            publish_into_handle,
            index_meta,
            set_state,
            erased,
        });

        handle
    }

    /// Enqueue a trigger. Cheap and non-blocking; the run loop coalesces bursts.
    pub fn request(&self, trigger: IndexTrigger) {
        let _ = self.trigger_tx.send(trigger);
    }

    /// Take the progress receiver (single consumer, typically faber-app). Returns
    /// a fresh channel's receiver only once; subsequent calls get a dummy.
    pub fn progress(&self) -> ProgressReceiver {
        self.progress_receiver
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| progress_channel().1)
    }

    /// Borrow the underlying store (e.g. for symbol queries from faber-app).
    pub fn store(&self) -> &IndexStore {
        &self.store
    }

    /// Clone the store `Arc` for off-thread queries (e.g. symbol picker).
    pub fn store_arc(&self) -> Arc<IndexStore> {
        self.store.clone()
    }

    /// Spawn the run loop on its own thread. Consumes an `Arc<Self>` so the loop
    /// keeps the engine alive.
    pub fn start(self: Arc<Self>) {
        std::thread::spawn(move || {
            self.run_loop();
        });
    }

    /// Blocking run loop: wait for a trigger, coalesce the burst, execute one run.
    fn run_loop(&self) {
        while let Ok(first) = self.trigger_rx.recv() {
            let scope = self.coalesce(first);
            if let Err(e) = self.run_once(scope) {
                log::warn!("index run failed: {e:#}");
            }
        }
    }

    /// Fold the first trigger plus any already-queued ones into one scope.
    fn coalesce(&self, first: IndexTrigger) -> ScanScope {
        let mut scope = ScanScope::from_trigger(first);
        while let Ok(next) = self.trigger_rx.try_recv() {
            scope = scope.merge(ScanScope::from_trigger(next));
        }
        scope
    }

    /// Execute one full run for `scope`: scan, inline META modules, then run the
    /// content pipeline, publishing each module as its writes land.
    fn run_once(&self, scope: ScanScope) -> Result<()> {
        self.progress_emitter.begin();

        let modules = self.modules.lock().unwrap();
        let content_names: Vec<&'static str> = modules
            .iter()
            .filter(|m| m.needs != InputNeeds::META)
            .map(|m| m.name)
            .collect();

        // Scan.
        self.progress_emitter.report(Phase::Scanning, 0, 0);
        let scan: ScanResult = scan_tree(
            &self.root,
            &scope,
            &self.store,
            &content_names,
            &self.registry,
        )?;

        // META-only modules: index inline from all_meta and publish.
        self.run_meta_modules(&modules, &scan)?;

        // Content modules: pipeline the dirty set.
        let files_indexed = if !scan.dirty.is_empty() && !content_names.is_empty() {
            self.run_content_modules(&modules, scan.dirty)?
        } else {
            0
        };

        // Record module versions so warm starts can trust them.
        for m in modules.iter() {
            let _ = self.store.set_module_version(m.name, m.version);
        }

        drop(modules);
        let _ = self.store.sync();
        self.progress_emitter.end(files_indexed);
        Ok(())
    }

    /// Index every META-only module inline and publish its snapshot.
    fn run_meta_modules(&self, modules: &[RegisteredModule], scan: &ScanResult) -> Result<()> {
        for m in modules.iter() {
            let Some(index_meta) = &m.index_meta else {
                continue;
            };
            (m.set_state)(ModuleState::Building {
                done: 0,
                total: scan.all_meta.len(),
            });

            let mut batch: Vec<crate::store::FileBatchEntry> =
                Vec::with_capacity(scan.all_meta.len());
            for meta in &scan.all_meta {
                let kvs = index_meta(meta)?;
                let stamp = Stamp {
                    mtime: meta.mtime,
                    size: meta.size,
                    hash: None,
                };
                batch.push((meta.rel_path.to_vec(), stamp, kvs));
            }
            // Reconcile deletions: drop stamps for files no longer present.
            self.prune_missing(m.name, &scan.all_meta)?;
            self.store.write_batch(m.name, &batch, true)?;

            let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
            (m.publish_into_handle)(&self.store, generation)?;
        }
        Ok(())
    }

    /// Run the content pipeline over `dirty`, writing results per module and
    /// publishing each once drained. Returns the number of files indexed.
    fn run_content_modules(
        &self,
        modules: &[RegisteredModule],
        dirty: Vec<(FileMeta, crate::scanner::DirtyReason)>,
    ) -> Result<usize> {
        // Assemble the erased module set for the pool.
        let mut erased_vec: Vec<ErasedModule> = Vec::new();
        for m in modules.iter() {
            if let Some(shared) = &m.erased
                && let Some(e) = shared.lock().unwrap().take()
            {
                erased_vec.push(e);
            }
        }
        if erased_vec.is_empty() {
            return Ok(0);
        }
        let content_names: Vec<&'static str> = erased_vec.iter().map(|e| e.name).collect();
        let modules_arc = Arc::new(erased_vec);
        let pipeline = Pipeline::new(modules_arc.clone(), self.registry.clone(), &self.root);

        for m in modules.iter() {
            if content_names.contains(&m.name) {
                (m.set_state)(ModuleState::Building {
                    done: 0,
                    total: dirty.len(),
                });
            }
        }

        let total = dirty.len();
        for (meta, _reason) in dirty {
            pipeline.submit(WorkItem { meta })?;
        }

        // Drain: buffer per-module batches, flush in chunks.
        let mut per_module: HashMap<&'static str, Vec<crate::store::FileBatchEntry>> =
            HashMap::new();
        let mut done = 0usize;
        for _ in 0..total {
            let Ok(result) = pipeline.results().recv() else {
                break;
            };
            done += 1;
            self.collect_result(&content_names, result, &mut per_module);
            self.progress_emitter.report(
                Phase::Indexing {
                    module: content_names.first().copied().unwrap_or("content"),
                },
                done,
                total,
            );
        }

        // Flush and publish each content module.
        self.progress_emitter
            .report(Phase::Publishing, 0, content_names.len());
        for name in &content_names {
            if let Some(batch) = per_module.remove(name) {
                self.store.write_batch(name, &batch, true)?;
            }
            let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
            if let Some(m) = modules.iter().find(|m| m.name == *name) {
                (m.publish_into_handle)(&self.store, generation)?;
            }
        }

        pipeline.shutdown();
        // Return erased modules for the next run.
        self.return_erased(modules, modules_arc);
        Ok(done)
    }

    /// Fold one `WorkResult` into the per-module batch buffers.
    fn collect_result(
        &self,
        content_names: &[&'static str],
        result: WorkResult,
        per_module: &mut HashMap<&'static str, Vec<crate::store::FileBatchEntry>>,
    ) {
        if let Some(err) = &result.error {
            log::warn!(
                "index error for {}: {err}",
                String::from_utf8_lossy(&result.meta.rel_path)
            );
        }
        let stamp = Stamp {
            mtime: result.meta.mtime,
            size: result.meta.size,
            hash: result.hash,
        };
        for name in content_names {
            let kvs = result.module_data.get(name).cloned().unwrap_or_default();
            per_module.entry(name).or_default().push((
                result.meta.rel_path.to_vec(),
                stamp.clone(),
                kvs,
            ));
        }
    }

    /// Move the erased modules back into their per-module slots for reuse next run.
    fn return_erased(&self, modules: &[RegisteredModule], modules_arc: Arc<Vec<ErasedModule>>) {
        // `modules_arc` is uniquely held now (pipeline dropped). Reclaim it.
        let Ok(erased_vec) = Arc::try_unwrap(modules_arc) else {
            return; // a worker lingered; drop them, they rebuild next run.
        };
        for e in erased_vec {
            if let Some(m) = modules.iter().find(|m| m.name == e.name)
                && let Some(shared) = &m.erased
            {
                *shared.lock().unwrap() = Some(e);
            }
        }
    }

    /// Delete store stamps/data for files present last run but absent now.
    fn prune_missing(&self, module_name: &str, present: &[FileMeta]) -> Result<()> {
        use std::collections::BTreeSet;
        let present_set: BTreeSet<&[u8]> = present.iter().map(|m| m.rel_path.as_ref()).collect();
        let stale: Vec<Vec<u8>> = self
            .store
            .iter_stamps(module_name)?
            .filter_map(|r| r.ok())
            .map(|(k, _)| k)
            .filter(|k| !present_set.contains(k.as_slice()))
            .collect();
        for k in stale {
            self.store.delete_file(module_name, &k)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::{FinderQuery, filter};
    use crate::test_util::with_project;
    use std::path::Path;
    use std::time::{Duration, Instant};

    fn write(root: &Path, rel: &str, body: &[u8]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
    }

    #[test]
    fn files_row_roundtrips() {
        let enc = FilesModule::encode_row("src/main.rs", 4, true);
        assert_eq!(
            FilesModule::decode_row(&enc),
            Some(("src/main.rs".to_string(), 4, true))
        );
    }

    #[test]
    fn files_module_publishes_snapshot_from_entries() {
        let m = FilesModule;
        let a = FilesModule::encode_row("a.rs", 0, false);
        let b = FilesModule::encode_row("src/b.rs", 4, false);
        let mut it = [
            (b"a.rs\0".as_slice(), a.as_slice()),
            (b"src/b.rs\0".as_slice(), b.as_slice()),
        ]
        .into_iter();
        let snap = m.publish(&mut it).unwrap();
        assert_eq!(snap.entries.len(), 2);
        assert_eq!(snap.entries[0].rel_path, "a.rs");
        assert_eq!(snap.entries[1].rel_path, "src/b.rs");
        assert_eq!(snap.extensions, vec![("rs".to_string(), 2)]);
    }

    #[test]
    fn engine_indexes_folder_and_publishes_files() {
        with_project(|root| {
            write(root, "src/main.rs", b"fn main() {}");
            write(root, "Cargo.toml", b"[package]");
            write(root, "README.md", b"# hi");

            let registry = Arc::new(LanguageRegistry::with_defaults());
            let mut engine = IndexEngine::new(root.to_path_buf(), registry).unwrap();
            let files = engine.register(FilesModule);
            let engine = Arc::new(engine);
            engine.clone().start();

            engine.request(IndexTrigger::FolderOpened);

            // Wait for the files module to reach Ready.
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if matches!(files.state(), ModuleState::Ready { .. }) {
                    break;
                }
                if Instant::now() > deadline {
                    panic!("files module never became Ready");
                }
                std::thread::sleep(Duration::from_millis(10));
            }

            let snap = files.load().expect("snapshot published");
            assert_eq!(snap.entries.len(), 3);
            let paths: Vec<&str> = snap.entries.iter().map(|e| e.rel_path.as_str()).collect();
            assert!(paths.contains(&"src/main.rs"));
            assert!(paths.contains(&"Cargo.toml"));
            assert!(paths.contains(&"README.md"));

            // The published snapshot is queryable through the finder core.
            let q = FinderQuery {
                text: "main".into(),
                ..Default::default()
            };
            let matches = filter(&snap, &q, &[], 10);
            assert!(
                matches
                    .iter()
                    .any(|m| snap.entries[m.entry_ix as usize].rel_path == "src/main.rs")
            );
        });
    }

    /// A content module (needs TEXT + SYNTAX) that counts syntax-tree nodes per
    /// file and publishes the grand total. Exercises the worker pool, file read,
    /// tree-sitter parse, and per-module publish end-to-end.
    struct NodeCount;
    impl IndexModule for NodeCount {
        type Snapshot = u64;
        fn name(&self) -> &'static str {
            "nodecount"
        }
        fn version(&self) -> u32 {
            1
        }
        fn needs(&self) -> InputNeeds {
            InputNeeds::TEXT | InputNeeds::SYNTAX
        }
        fn accepts(&self, meta: &FileMeta) -> bool {
            meta.language.is_some()
        }
        fn index(&self, input: &FileInput) -> Result<Vec<(KeySuffix, Vec<u8>)>> {
            let n = input
                .syntax
                .map(|t| t.root_node().descendant_count() as u64)
                .unwrap_or(0);
            Ok(vec![(Vec::new(), n.to_le_bytes().to_vec())])
        }
        fn publish(&self, entries: &mut dyn Iterator<Item = (&[u8], &[u8])>) -> Result<u64> {
            let mut total = 0u64;
            for (_k, v) in entries {
                let arr: [u8; 8] = v.try_into()?;
                total += u64::from_le_bytes(arr);
            }
            Ok(total)
        }
    }

    #[test]
    fn engine_runs_content_pipeline_and_publishes() {
        with_project(|root| {
            write(root, "a.rs", b"fn a() { let x = 1; }");
            write(root, "b.rs", b"fn b() {}");
            write(root, "notes.txt", b"no language here"); // not accepted (no lang)

            let registry = Arc::new(LanguageRegistry::with_defaults());
            let mut engine = IndexEngine::new(root.to_path_buf(), registry).unwrap();
            let nodes = engine.register(NodeCount);
            let engine = Arc::new(engine);
            engine.clone().start();
            engine.request(IndexTrigger::FolderOpened);

            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if matches!(nodes.state(), ModuleState::Ready { .. }) {
                    break;
                }
                if Instant::now() > deadline {
                    panic!("nodecount module never became Ready");
                }
                std::thread::sleep(Duration::from_millis(10));
            }

            let total = nodes.load().expect("content snapshot published");
            // Two rust files parsed into non-trivial trees; total node count is
            // well above zero, proving parse + index + publish all ran.
            assert!(*total > 0, "expected parsed nodes, got {total}");
        });
    }
}
