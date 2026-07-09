//! Content-phase worker pool.
//!
//! `num_cpus` worker threads pull `WorkItem`s off a bounded queue, read the file
//! once (if any module needs `TEXT`/`SYNTAX`), parse a tree-sitter tree once (if
//! any module needs `SYNTAX`), run every accepting module's `index`, and push a
//! `WorkResult` back. Each worker owns one parser, reused across files.
//!
//! Modules are held type-erased (`ErasedModule`) so the pool can carry a
//! heterogeneous set without knowing each `Snapshot` type. A per-thread parser is
//! rebuilt when the file's language changes, which is rare in a sorted stream.

use std::{collections::HashMap, sync::Arc, thread::JoinHandle};

use faber_lang::{LanguageId, LanguageRegistry};

use crate::{
    module::{FileInput, FileMeta, IndexModule, InputNeeds, KeySuffix},
    scanner::is_binary,
};

/// In-flight byte budget across all workers (~64 MiB). Bounds peak memory when
/// many large files are queued; a worker acquires its file's size before reading
/// and releases it when the result is sent.
const BYTE_BUDGET: usize = 64 * 1024 * 1024;

/// Work queue depth. Backpressures the engine's submit loop.
const QUEUE_DEPTH: usize = 512;

/// The erased `index` closure: `FileInput` in, `(key_suffix, value)` pairs out.
pub(crate) type IndexFn =
    Box<dyn Fn(&FileInput) -> anyhow::Result<Vec<(KeySuffix, Vec<u8>)>> + Send + Sync>;

/// Type-erased module: closures over `accepts`/`index` plus the metadata the
/// worker needs to decide what inputs to materialize. Lets a `Vec` hold modules
/// with differing `Snapshot` types.
pub(crate) struct ErasedModule {
    pub name: &'static str,
    pub needs: InputNeeds,
    pub accepts: Box<dyn Fn(&FileMeta) -> bool + Send + Sync>,
    pub index: IndexFn,
}

impl ErasedModule {
    /// Erase a concrete `IndexModule` into closures. The module is shared via
    /// `Arc` so both closures can call it without moving it twice.
    pub(crate) fn new<M: IndexModule>(m: Arc<M>) -> Self {
        let name = m.name();
        let needs = m.needs();
        let accepts_m = m.clone();
        let index_m = m;
        Self {
            name,
            needs,
            accepts: Box::new(move |meta| accepts_m.accepts(meta)),
            index: Box::new(move |input| index_m.index(input)),
        }
    }
}

/// One file to (re)index in the content phase.
pub(crate) struct WorkItem {
    pub meta: FileMeta,
}

/// Per-module key/value entries produced for a single file, keyed by module name.
pub(crate) type ModuleData = HashMap<&'static str, Vec<(Vec<u8>, Vec<u8>)>>;

/// The per-file output: each accepting module's key/value entries, plus the first
/// error hit (if any). `error.is_some()` means the engine should record an error
/// stamp rather than data.
pub(crate) struct WorkResult {
    pub meta: FileMeta,
    pub module_data: ModuleData,
    pub error: Option<String>,
    /// blake3 of the file body, when it was read (content modules). Feeds the
    /// stamp so the next warm scan can skip re-hashing.
    pub hash: Option<[u8; 32]>,
}

/// A fixed pool of workers plus the channels to feed and drain them.
pub(crate) struct Pipeline {
    submit_tx: crossbeam_channel::Sender<WorkItem>,
    results_rx: crossbeam_channel::Receiver<WorkResult>,
    workers: Vec<JoinHandle<()>>,
}

impl Pipeline {
    /// Spawn `num_cpus` workers over the shared, erased module set and registry.
    /// Every worker seeds its thread-local project `root` so `meta_abs_path`
    /// resolves the bytewise rel_paths back to disk.
    pub(crate) fn new(
        modules: Arc<Vec<ErasedModule>>,
        registry: Arc<LanguageRegistry>,
        root: &std::path::Path,
    ) -> Self {
        let n = num_cpus::get().max(1);
        let (submit_tx, submit_rx) = crossbeam_channel::bounded::<WorkItem>(QUEUE_DEPTH);
        let (results_tx, results_rx) = crossbeam_channel::unbounded::<WorkResult>();
        let budget = Arc::new(ByteBudget::new(BYTE_BUDGET));
        let root = root.to_path_buf();

        let mut workers = Vec::with_capacity(n);
        for _ in 0..n {
            let submit_rx = submit_rx.clone();
            let results_tx = results_tx.clone();
            let modules = modules.clone();
            let registry = registry.clone();
            let budget = budget.clone();
            let root = root.clone();
            workers.push(std::thread::spawn(move || {
                set_worker_root(&root);
                let mut worker = Worker::new(modules, registry);
                while let Ok(item) = submit_rx.recv() {
                    let permit = budget.acquire(item.meta.size as usize);
                    let result = worker.process(item);
                    drop(permit);
                    if results_tx.send(result).is_err() {
                        break; // engine dropped the receiver; stop.
                    }
                }
            }));
        }

        Self {
            submit_tx,
            results_rx,
            workers,
        }
    }

    /// Enqueue a file. Blocks if the queue is full (backpressure).
    pub(crate) fn submit(&self, item: WorkItem) -> anyhow::Result<()> {
        self.submit_tx
            .send(item)
            .map_err(|_| anyhow::anyhow!("pipeline closed"))
    }

    /// Drain channel for completed results.
    pub(crate) fn results(&self) -> &crossbeam_channel::Receiver<WorkResult> {
        &self.results_rx
    }

    /// Close the submit side and join every worker. Any results still in flight
    /// remain readable on `results()` until the channel is dropped with `self`.
    pub(crate) fn shutdown(self) {
        drop(self.submit_tx);
        for w in self.workers {
            let _ = w.join();
        }
    }
}

/// A worker's reusable state: the module set, registry, and a cached parser keyed
/// by the language it was last configured for.
struct Worker {
    modules: Arc<Vec<ErasedModule>>,
    registry: Arc<LanguageRegistry>,
    parser: Option<(LanguageId, tree_sitter::Parser)>,
}

impl Worker {
    fn new(modules: Arc<Vec<ErasedModule>>, registry: Arc<LanguageRegistry>) -> Self {
        Self {
            modules,
            registry,
            parser: None,
        }
    }

    /// Read + parse once, then run each accepting module. Errors from a single
    /// module are captured into `WorkResult::error`, not propagated, so one bad
    /// file never stalls the pool.
    fn process(&mut self, item: WorkItem) -> WorkResult {
        let meta = item.meta;

        // Which inputs does *any* accepting module want?
        let mut union = InputNeeds::empty();
        let mut any_accepts = false;
        for m in self.modules.iter() {
            if (m.accepts)(&meta) {
                any_accepts = true;
                union |= m.needs;
            }
        }

        if !any_accepts {
            return WorkResult {
                meta,
                module_data: HashMap::new(),
                error: None,
                hash: None,
            };
        }

        // Materialize file body / tree as needed.
        let wants_text = union.intersects(InputNeeds::TEXT | InputNeeds::SYNTAX);
        let mut text: Option<String> = None;
        let mut hash: Option<[u8; 32]> = None;
        if wants_text {
            match self.read_body(&meta) {
                Ok(Some((body, h))) => {
                    text = Some(body);
                    hash = Some(h);
                }
                Ok(None) => {} // binary: content-skip, run META-only accepts below.
                Err(e) => {
                    return WorkResult {
                        meta,
                        module_data: HashMap::new(),
                        error: Some(e.to_string()),
                        hash: None,
                    };
                }
            }
        }

        let tree = if union.contains(InputNeeds::SYNTAX) {
            text.as_deref().and_then(|body| self.parse(&meta, body))
        } else {
            None
        };

        // Run each accepting module.
        let mut module_data: ModuleData = HashMap::new();
        let mut error = None;
        for m in self.modules.iter() {
            if !(m.accepts)(&meta) {
                continue;
            }
            let input = FileInput {
                meta: &meta,
                text: text.as_deref(),
                syntax: tree.as_ref(),
            };
            match (m.index)(&input) {
                Ok(entries) => {
                    module_data.insert(m.name, entries);
                }
                Err(e) => {
                    error.get_or_insert_with(|| format!("{}: {e}", m.name));
                }
            }
        }

        WorkResult {
            meta,
            module_data,
            error,
            hash,
        }
    }

    /// Read the file body and its hash. Returns `Ok(None)` for binaries (NUL in
    /// the first 8 KiB) or non-UTF8 content — either way, content is skipped.
    fn read_body(&self, meta: &FileMeta) -> anyhow::Result<Option<(String, [u8; 32])>> {
        // rel_path is root-relative; the engine only ever queues files under the
        // project root, but the worker doesn't know the root — so it re-reads via
        // the absolute path stashed by the scanner is not available here. Instead
        // the engine resolves absolute paths before submit; see `abs_path`.
        let abs = meta_abs_path(meta);
        let bytes = std::fs::read(&abs)?;
        if is_binary(&bytes) {
            return Ok(None);
        }
        let hash = *blake3::hash(&bytes).as_bytes();
        match String::from_utf8(bytes) {
            Ok(s) => Ok(Some((s, hash))),
            Err(_) => Ok(None), // invalid UTF-8: treat as content-skip.
        }
    }

    /// Parse `body` into a tree, (re)configuring the cached parser if the
    /// language changed. Returns `None` when the language is unsupported.
    fn parse(&mut self, meta: &FileMeta, body: &str) -> Option<tree_sitter::Tree> {
        let lang_id = meta.language.as_ref()?;
        let need_new = self
            .parser
            .as_ref()
            .map(|(id, _)| id != lang_id)
            .unwrap_or(true);
        if need_new {
            let lang = self.registry.language_by_id(lang_id)?;
            self.parser = Some((lang_id.clone(), lang.make_parser()));
        }
        let (_, parser) = self.parser.as_mut()?;
        parser.parse(body, None)
    }
}

/// Resolve a `FileMeta`'s bytewise rel_path back to an absolute path under the
/// worker's project root. The root is a per-thread value seeded by
/// [`set_worker_root`] so `FileMeta` stays root-agnostic.
fn meta_abs_path(meta: &FileMeta) -> std::path::PathBuf {
    ROOT.with(|r| {
        let root = r.borrow();
        // SAFETY: rel_path bytes came from `OsStr::as_encoded_bytes` in the
        // scanner, so this reconstruction round-trips on the same platform.
        let rel: std::ffi::OsString =
            unsafe { std::ffi::OsString::from_encoded_bytes_unchecked(meta.rel_path.to_vec()) };
        root.join(rel)
    })
}

thread_local! {
    /// Project root, seeded per worker thread by [`set_worker_root`] so
    /// `meta_abs_path` can rebuild absolute paths without widening `FileMeta`.
    static ROOT: std::cell::RefCell<std::path::PathBuf> = const { std::cell::RefCell::new(std::path::PathBuf::new()) };
}

/// A counting semaphore over a byte budget. `acquire` blocks (spins with a short
/// park) until the request fits, then returns a permit that restores the budget
/// on drop. Oversized single files are allowed through once the budget is free.
struct ByteBudget {
    inner: std::sync::Mutex<usize>,
    cvar: std::sync::Condvar,
    cap: usize,
}

impl ByteBudget {
    fn new(cap: usize) -> Self {
        Self {
            inner: std::sync::Mutex::new(cap),
            cvar: std::sync::Condvar::new(),
            cap,
        }
    }

    fn acquire(self: &Arc<Self>, bytes: usize) -> BudgetPermit {
        let want = bytes.min(self.cap).max(1);
        let mut avail = self.inner.lock().unwrap();
        while *avail < want {
            avail = self.cvar.wait(avail).unwrap();
        }
        *avail -= want;
        BudgetPermit {
            budget: self.clone(),
            bytes: want,
        }
    }
}

struct BudgetPermit {
    budget: Arc<ByteBudget>,
    bytes: usize,
}

impl Drop for BudgetPermit {
    fn drop(&mut self) {
        let mut avail = self.budget.inner.lock().unwrap();
        *avail += self.bytes;
        self.budget.cvar.notify_all();
    }
}

/// Seed this thread's project root. The engine calls it via a wrapper before the
/// worker loop runs so `meta_abs_path` works. Exposed crate-internally for the
/// engine to inject.
pub(crate) fn set_worker_root(root: &std::path::Path) {
    ROOT.with(|r| *r.borrow_mut() = root.to_path_buf());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;
    use tempfile::TempDir;

    /// A module that emits the file's byte length under an empty key suffix.
    struct LenMod;
    impl IndexModule for LenMod {
        type Snapshot = ();
        fn name(&self) -> &'static str {
            "len"
        }
        fn version(&self) -> u32 {
            1
        }
        fn needs(&self) -> InputNeeds {
            InputNeeds::TEXT
        }
        fn accepts(&self, _m: &FileMeta) -> bool {
            true
        }
        fn index(&self, input: &FileInput) -> anyhow::Result<Vec<(KeySuffix, Vec<u8>)>> {
            let len = input.text.map(|t| t.len()).unwrap_or(0) as u64;
            Ok(vec![(Vec::new(), len.to_le_bytes().to_vec())])
        }
        fn publish(&self, _e: &mut dyn Iterator<Item = (&[u8], &[u8])>) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn meta(rel: &[u8], size: u64) -> FileMeta {
        FileMeta {
            rel_path: Arc::from(rel),
            size,
            mtime: SystemTime::UNIX_EPOCH,
            is_ignored: false,
            language: None,
        }
    }

    #[test]
    fn pipeline_indexes_text_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        std::fs::write(dir.path().join("bin"), b"a\0b").unwrap();

        // The worker resolves abs paths from a thread-local root; but the pool
        // spawns its own threads. Seed via a module set + a root the workers read.
        // For the test we set the root on THIS thread and rely on the pool reading
        // it — so instead drive process() directly for determinism.
        set_worker_root(dir.path());
        let modules = Arc::new(vec![ErasedModule::new(Arc::new(LenMod))]);
        let reg = Arc::new(LanguageRegistry::with_defaults());
        let mut worker = Worker::new(modules, reg);

        let r = worker.process(WorkItem {
            meta: meta(b"a.txt", 5),
        });
        assert!(r.error.is_none());
        let entry = &r.module_data["len"][0];
        assert_eq!(entry.1, 5u64.to_le_bytes());
        assert!(r.hash.is_some());

        // Binary file: content-skipped, len sees empty text -> 0, no hash.
        let rb = worker.process(WorkItem {
            meta: meta(b"bin", 3),
        });
        assert!(rb.error.is_none());
        assert_eq!(rb.module_data["len"][0].1, 0u64.to_le_bytes());
        assert!(rb.hash.is_none());
    }

    #[test]
    fn pool_round_trips_via_channels() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("x.txt"), b"1234").unwrap();

        let modules = Arc::new(vec![ErasedModule::new(Arc::new(LenMod))]);
        let reg = Arc::new(LanguageRegistry::with_defaults());
        // Each worker seeds its own thread-local root from the constructor arg.
        let pipeline = Pipeline::new(modules, reg, dir.path());
        pipeline
            .submit(WorkItem {
                meta: meta(b"x.txt", 4),
            })
            .unwrap();
        let r = pipeline.results().recv().unwrap();
        assert_eq!(r.module_data["len"][0].1, 4u64.to_le_bytes());
        pipeline.shutdown();
    }
}
