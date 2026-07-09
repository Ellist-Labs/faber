# Indexing engine TDD

Status: draft, revised after four-lens design review (architecture/SOLID, scalability/maintainability/testability, performance/UX, cohesion/coupling)
Related: [indexing-engine-prd.md](indexing-engine-prd.md)

## Context

Faber is a 6-crate workspace where logic crates never import gpui and `faber-app` is a thin GPUI shell (CLAUDE.md rule, compiler-enforced). Today's index-like work is scattered:

- `faber-editor/src/file_index.rs`: async full-tree walk (ignore crate, 200K cap), in-memory only, stale-after-5s rescan, kicked from `workspace.rs:534`. Also hosts the finder data layer: `FileIndexSnapshot`, `FileEntry`, `FinderQuery`, `FinderMatch`, `filter()`.
- `faber-editor/src/outline.rs`: per-document tree-sitter symbols (`Outline`, `OutlineItem`, compute), recomputed on every edit, never project-wide. Imports only `faber_lang::Grammar` + `tree_sitter`.
- `faber-editor/src/project_search.rs`: on-demand walk per query (stays as is in v1).
- No filesystem watcher. No persistence beyond TOML settings/state. The `LanguageRegistry` lives in a GPUI global (`main.rs:36`).

Design sources: Zed's worktree scanner and (removed) `semantic_index` crate, IntelliJ's `FileBasedIndexExtension` model, rust-analyzer's revision-based laziness. Adopted ideas: registry of versioned pure modules, scanning vs indexing phase split, mtime-then-hash staleness, single writer with MVCC readers, per-module snapshot publication, throttled LSP-shaped progress.

The main persistence payoff is symbols and every future content-derived module (parse cost is real); the file list rides along for uniformity and warm-start polish. Don't "simplify" the store away on the file finder's account alone.

## Architecture

New GPUI-free crate `faber-index`, depending on `faber-core` and `faber-lang` only. `faber-editor` and `faber-index` are siblings; neither depends on the other. `faber-app` wires everything.

```
crates/faber-index/src/
  lib.rs        public re-exports
  engine.rs     IndexEngine: run loop, registry, snapshot handles, readiness
  module.rs     IndexModule trait, FileMeta, FileInput, InputNeeds, ModuleState
  scanner.rs    tree walk + stamp merge-join → dirty set (holds a store read view)
  store.rs      heed env, stamps, meta, batched writer, self-heal, GC
  pipeline.rs   std::thread worker pool, staged bounded channels
  watcher.rs    notify wrapper: debounce, coalesce, echo suppression, overflow fallback
  trigger.rs    ScanScope lattice + coalescing queue
  progress.rs   ProgressEvent, throttled emitter
  files.rs      files module + finder data layer (snapshot, filter)
  symbols.rs    symbols module + read API
```

### Prerequisite type moves

Two moves must land before the engine, or nothing compiles with the stated dependency set:

- `Outline`, `OutlineItem`, and the tree-sitter compute function move from `faber-editor/src/outline.rs` to `faber-lang` (next to `OutlineConfig`, their only dependency). `faber-editor` re-exports them (it already re-exports faber-lang types); the markdown outline path stays in `faber-editor` producing the moved type.
- The finder data layer (`FileIndexSnapshot`, `FileEntry`, `FinderQuery`, `FinderMatch`, `filter()`) plus the `nucleo-matcher` and `regex` deps move from `faber-editor/src/file_index.rs` into `faber-index/src/files.rs`. `file_index.rs` is then deleted (the scan half is replaced by the engine); `faber-app` imports from `faber_index`.

### Module trait (registry pattern)

```rust
bitflags! { pub struct InputNeeds: u8 { const META; const TEXT; const SYNTAX; } }

pub struct FileMeta {
    pub rel_path: Arc<RelPath>,   // raw bytes, sorted bytewise; non-UTF8 safe
    pub size: u64,
    pub mtime: SystemTime,
    pub is_ignored: bool,
    pub language: Option<LanguageId>,   // resolved once by the engine
}

pub struct FileInput<'a> {
    pub meta: &'a FileMeta,
    pub text: Option<&'a str>,           // present iff the module declared TEXT
    pub syntax: Option<&'a tree_sitter::Tree>,  // present iff SYNTAX and language supported
}

pub trait IndexModule: Send + Sync {
    type Snapshot: Send + Sync + 'static;
    fn name(&self) -> &'static str;      // stable; names the LMDB sub-DBs
    fn version(&self) -> u32;            // bump ⇒ rebuild this module only
    fn needs(&self) -> InputNeeds;       // META-only modules skip content entirely
    fn accepts(&self, meta: &FileMeta) -> bool;
    fn index(&self, input: &FileInput) -> anyhow::Result<Vec<(KeySuffix, Vec<u8>)>>;
    fn publish(&self, entries: &mut dyn Iterator<Item = (&[u8], &[u8])>)
        -> anyhow::Result<Self::Snapshot>;
}
```

Rules, enforced by the shape of the API rather than by convention:

- `index` is a pure function of `FileInput`. No filesystem reads, no globals. This keeps modules independently rebuildable and table-testable.
- Modules return key *suffixes*; the engine composes the stored key as `path bytes + \0 + suffix` (Zed's layout), so range deletes per file and the merge-join ordering can't be broken by a module.
- `publish` consumes an abstract entry iterator, never heed types. The engine feeds it from a read transaction; tests feed it straight from `index()` output. Storage stays swappable (the SQLite revisit clause survives).
- The engine parses a file once per run, inside the worker handling it, and only when an accepting module declared `SYNTAX` for a supported language. Adding module #3 changes nothing in the engine (open/closed).
- Value encoding is bincode; each module documents its value struct next to its `index()`.

Registration returns a typed handle; `dyn Any` exists only inside the erased registry wrapper, and no downcast ever appears in `faber-app`:

```rust
pub struct SnapshotHandle<S> { /* Arc<ArcSwapOption<S>> + shared ModuleState */ }
impl<S> SnapshotHandle<S> {
    pub fn load(&self) -> Option<Arc<S>>;
    pub fn state(&self) -> ModuleState;
}
```

### Engine facade

```rust
impl IndexEngine {
    pub fn new(root: PathBuf, index_dir: PathBuf, languages: Arc<LanguageRegistry>)
        -> anyhow::Result<Self>;                    // registry injected; engine reads no globals
    pub fn register<M: IndexModule>(&mut self, m: M) -> SnapshotHandle<M::Snapshot>;
    pub fn start(self: Arc<Self>);                  // spawns run loop + workers
    pub fn request(&self, trigger: IndexTrigger);   // the only mutation entry point
    pub fn progress(&self) -> ProgressReceiver;     // throttled, latest-value
}
```

`request` is the anti-corruption boundary: features never talk to the scanner, store, or watcher directly.

### Threading model

`faber-index` owns its threads: one run-loop thread plus a `num_cpus`-sized `std::thread` worker pool with bounded channels. CPU and IO bound batch work gains nothing from GPUI's executor, and gpui is off-limits in this crate; plain threads beat injecting a `Spawner` trait for one consumer (recorded under alternatives).

- Backpressure is bounded twice: channel slots (512) and an in-flight byte budget (~64 MB semaphore) so 512 × 1 MB files can't sit in memory.
- The writer is the only serial stage. Parsing runs in the same worker as read+hash, one parser per worker thread.
- `faber-app` wiring: a foreground `cx.spawn` drains the progress receiver and updates `Entity<IndexStatus>`; views observe it. Engine start is sequenced after the first window frame, so startup stays interactive by construction.
- Teardown: dropping the engine (workspace close, app quit) closes channels; threads exit. User-facing cancellation stays impossible; runs otherwise complete.

### Storage (LMDB via heed)

One `heed::Env` per project at `~/.cache/faber/index/<blake3(root_abs_path)>/`, map size 1 GiB. Named databases:

- `meta`: engine schema version, per-module recorded version, `last_opened`, `last_rebuild_reason`.
- `stamps:{module}`: rel_path → `Stamp`. META-only modules store `{mtime, size}`; content modules store `{mtime, size, blake3}`. Per module, so version bumps invalidate independently.
- `data:{module}`: the module's entries, path-prefixed keys.

Consistency and durability:

- Per file, stamp and data writes share one write transaction.
- Cold runs: env opened with `NO_SYNC`, commits batched by time or count (~100 ms or ~1,000 files), one durable sync at run end. A crash loses at most the uncommitted tail; the next merge-join reindexes it. This avoids ~2,000 fsyncs on a 200K cold build.
- Incremental runs: small batches (~100 files), normal sync.
- Publish runs on a read transaction in a separate task after the writer bumps the generation, so a queued follow-up run never waits on snapshot rebuilds.
- Map-full: abort the transaction, reopen the env with a doubled map, retry the batch.
- Self-heal: version mismatch or open failure drops the affected DBs (or the env dir) and rebuilds in the background. Never an error dialog; always a logged reason.
- Cache GC: on startup, delete index dirs whose `last_opened` is older than 30 days (pure cache; PRD already declares it safe to delete).
- Ordering invariant: the scanner collects and sorts walk output bytewise on rel_path bytes; stored keys are those same bytes, so the LMDB cursor and the walk merge-join in lockstep.

### Run loop and scheduling

One run at a time, serial, runs to completion (PRD FR7). The run has per-phase outputs, not one big barrier:

```
trigger → scan phase → [files publish] → content phase → [per-module publish]
```

- Scan: walk the tree (ignore crate, same rules as today, 200K cap logged when hit) into sorted `FileMeta`s; merge-join against each module's stamps. Dirty test: mtime+size gate first; blake3 computed only when they changed (branch switches touch many mtimes, few contents). Scoped triggers stat only the affected paths and skip the walk.
- META-only modules (files) index straight from scan output: no reads, no hashes, no pipeline. The files snapshot publishes immediately after the scan phase, so a cold open has the finder Ready in walk time (seconds), not read+hash+parse time (minutes). This was the single worst regression risk in the first draft.
- Content phase: dirty set ordered by priority (open documents first, then finder-history paths, then BFS depth), fed through the pipeline. Each content module publishes as soon as its own writes finish.
- Warm start: on engine start, before any scan, each module whose recorded version matches publishes directly from the store (background; budget: under 200 ms at 200K entries), then a `Walk` verification scan queues. Finder answers from the persisted index at first paint.
- Saves during a run: `FileSaved` paths inject into the in-flight run's work queue (the pipeline streams; one more file is cheap), so a newly created file appears in the finder without waiting behind a cold build.
- Zero-work runs skip publish entirely (no snapshot churn on no-op triggers).
- Ignored entries are META-only data for the files module (the `is_ignored` flag replaces today's second walk); they never enter the content phase and the watcher filters them out. `filter()` gains an `include_ignored` flag to replace the old two-snapshot model.
- Failed `index()` calls: stamp with an error marker (retried only when content changes, preventing hot loops), counted as done for progress, logged once.

### Triggers

```rust
pub enum IndexTrigger {
    FolderOpened,
    FileSaved(PathBuf),
    ExternalChanges(Vec<PathBuf>),   // from watcher
    Manual,                          // settings: re-verify everything (re-hash)
}

enum ScanScope { Paths(BTreeSet<PathBuf>), Walk, Verify }
// merge: Verify ⊔ _ = Verify;  Walk ⊔ Paths = Walk;  Paths ⊔ Paths = union
// a Paths set exceeding ~1,000 entries degrades to Walk (cheaper than per-path stats)
```

Triggers arriving mid-run merge into one pending scope through the lattice; at most one follow-up run queues.

### Watcher

`notify` recommended watcher on the project root:

- Callback pushes into a shared pending buffer; a drain step sleeps 100 ms then takes the whole buffer. Coalescing is a pure function `coalesce(Vec<PathEvent>) -> Option<IndexTrigger>` (testable without real fs events or clocks); the sleep lives in a thin outer loop.
- Echo suppression: paths indexed via `FileSaved` in the last ~2 s are dropped from watcher batches, so every in-app save doesn't schedule a second run.
- Events filter through the same ignore rules as the scanner. Overflow or rescan events degrade to `Walk`. Watching is an optimization over scanning, never the source of truth.
- Owned by the engine, started on folder open, dropped on close.

### Progress

Counting and emission are separate concerns:

- Counting: RAII handles travel with each file through the pipeline (Zed's `IndexingEntrySet`); drop decrements an atomic. Counts can't desync.
- Emission: the engine publishes into a latest-value cell plus a change signal, throttled to at most one `Report` per 100 ms or 1% delta. `Begin`/`End` and phase transitions always emit. No unbounded queue, no 200K foreground wakeups.

```rust
pub enum ProgressEvent {
    Begin,
    Report { phase: Phase, done: usize, total: usize },  // Scanning | Indexing { module } | Publishing
    End { files_indexed: usize },
}
```

UI policy is time-based, not count-based (IntelliJ's mechanism): the status item appears only if a run is still active ~800 ms after `Begin`, stays at least ~1 s once shown (no flicker), rotates labels with a minimum 500 ms dwell, and updates counts in place at ≤10 Hz. Warm starts and small deltas therefore show nothing, without a magic file-count threshold.

### UI: status bar

New generic component in `faber-app/src/status_bar.rs`:

- Sticky strip (~26 px) at the bottom of `Workspace::render`, below the existing bottom panel, themed like the titlebar.
- Left/right slots of `AnyView` items so future items (cursor position, git branch) plug in without changes.
- `IndexingStatusItem` observes `Entity<IndexStatus>`, which the foreground drain task keeps updated (per-module `ModuleState` mirror plus current progress). Snapshot swap notification is explicit: publish events reach the drain task, which calls the finder's existing `on_index_updated` and `cx.notify`; `ArcSwap` loads happen at query time (ArcSwap itself has no subscription).
- All strings via `t!()` with keys in `locales/en.toml`; run `/i18n-guardrails` before finishing.

Settings: `Reindex project` in Settings > General dispatches a `ReindexProject` GPUI action handled by `Workspace` (the settings view holds no workspace reference, and a handle global would be a new hidden coupling). Enabled when `ProjectRoot` is set. Manual maps to `Verify` scope.

### V1 modules

- `files` (META-only): entries carry rel_path, name, extension, ignored flag. `publish` materializes the moved `FileIndexSnapshot` (~20-40 MB at 200K entries, two generations briefly alive during a swap; acceptable, noted in the memory budget). The finder, nucleo matching, and `filter()` keep their shapes except the new `include_ignored` flag. `kick_index_scan` and the 5-second staleness hack are deleted.
- `symbols` (TEXT | SYNTAX): runs the moved outline compute per accepted file, stores `Vec<OutlineItem>` per path. Its `Snapshot` is deliberately *not* materialized: it's a thin generation-stamped store handle; `symbols_for(path)` and future fuzzy queries iterate LMDB read transactions. Materializing 200K files of symbols would cost hundreds of MB and negate the zero-copy rationale for LMDB.
- Open-buffer overlay: storing `OutlineItem` makes index data type-identical to live outlines, so the merge is a thin composition in `faber-app` (open documents answer from their live `Outline`; everything else from the index). The engine never learns about editor state.

### Feature gating

Per-module state, no global dumb mode:

```rust
pub enum ModuleState { Cold, Building { done: usize, total: usize }, Ready { generation: u64 } }
```

Consumers check the handle's state and degrade inline. During `Cold`, the finder still renders and opens history rows (they live in `state.toml`, independent of the index). Nothing throws, nothing blocks.

### Observability

`faber-index` takes a `log` dependency and emits: run start/end with trigger, dirty counts, and duration; self-heal rebuild reason; map-full resizes; watcher overflow fallbacks; walk truncation. `meta` records `last_rebuild_reason`, so a version-mismatch rebuild loop is diagnosable instead of reading as "faber is slow". A small `examples/dump.rs` prints any project's stamps and entries for inspection.

## Alternatives considered

- SQLite WAL instead of LMDB: single file, transactional multi-DB commits, FTS5 for free. Chosen against in review: LMDB has zero-copy reads, no checkpointing, and a proven shape in Zed's index on the same stack. Revisit if multi-module transactions or tooling needs grow; the storage-agnostic `publish` iterator keeps the swap contained.
- Injected `Spawner` trait instead of engine-owned std threads: keeps execution on GPUI's pool, but adds an abstraction with one real implementation and makes engine tests depend on an executor. Owned threads are simpler and deterministic under test; revisit if the app ever needs to throttle the engine against other background work.
- Flat bincode files per module: whole-index rewrites per commit kill incrementality.
- No persistence (rescan each start): today's model; fails the warm-start goal as content modules arrive.
- tokio: the codebase is GPUI-executor plus threads; a second runtime buys nothing.
- Global dumb mode (IntelliJ pre-2023): rejected for per-module gating; scanning-in-smart-mode is the industry direction.

## Migration plan (PRs ≤ 700 LOC each)

1. Type moves: outline compute + `Outline`/`OutlineItem` to `faber-lang`; create `faber-index` with the moved finder data layer (`FileIndexSnapshot`, `filter()`, deps); delete `faber-editor/src/file_index.rs` scan half; re-point imports. No engine yet.
2. Store: heed env, stamps, meta, batched writer, self-heal, GC, logging, `examples/dump.rs`, tempdir tests.
3. Engine core: module trait, registry + typed handles, scanner merge-join, pipeline, trigger lattice, files module, progress emitter. Unit + tempdir tests.
4. App wiring: engine spawn after first frame, warm-start publish, triggers for open/save, finder reads the handle, delete `kick_index_scan` + staleness timer.
5. Status bar component + indexing item + i18n keys.
6. Watcher + echo suppression + `ExternalChanges` + settings `ReindexProject` action.
7. Symbols module + read API + open-buffer merge in `faber-app`.
8. CLAUDE.md section (indexing rules: "new project-wide computation ⇒ new module, never a bespoke scan") and `.claude/skills/index-module/SKILL.md`: trait contract, purity rule, needs/accepts, key suffixes, publish snapshots, version bumping, testing pattern.

## Risks

- LMDB map sizing: fixed at open; 1 GiB virtual reservation is fine on 64-bit; recovery path specified above (double and retry).
- RSS accounting: mmap pages are file-backed but count toward RSS when touched; a cold build will read above the 100 MB "typical" target even though pages are evictable. Measure with cache pages separated before judging the target.
- Hashing cost on cold start: bounded by skipping files over 1 MB and binary sniffing (first-KB NUL check), pending PRD open question.
- Watcher platform quirks (FSEvents coalescing, inotify limits): covered by overflow-degrades-to-Walk plus echo suppression.
- Tree-sitter parse cost: bounded by `needs`/`accepts`; only symbols requests parses, only for supported languages.
- Finder history (`ProjectHistory`) stays in `state.toml`; the index stores no per-user data, keeping the cache safe to delete.

## Testing

- Modules are pure: table-driven tests on `index()` with string inputs, and on `publish()` by piping `index()` output through the iterator, no filesystem or env.
- Engine tests on tempdir fixtures with explicit mtimes via the `filetime` crate (no sleeps, no mtime-granularity flakes): cold build, warm no-op, touch-without-change (mtime bump, same content ⇒ zero reindex), edit, delete, rename, version-bump rebuild, corrupt-env recovery, error-marker retry.
- Watcher: `coalesce()` is a pure function tested directly; the debounce sleep stays in an untested two-line loop.
- App layer: the status item renders from a plain `IndexStatus` value; no logic in `faber-app` beyond wiring, per codebase convention.

## Open questions

- Max file size to hash and index (proposed: skip over 1 MB plus binary sniff).
- GC window for stale index dirs (proposed: 30 days).
- Should project search consume the files snapshot in v1 to skip its own walk, or stay untouched until v2?
- Should `publish` snapshots become incremental (apply diff) later? Rebuild is O(entries) and measured fine for the files module at 200K; symbols already avoids materializing. Measure before complicating.
