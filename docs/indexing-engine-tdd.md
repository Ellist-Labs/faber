# Indexing engine TDD

Status: draft
Related: [indexing-engine-prd.md](indexing-engine-prd.md)

## Context

Faber is a 6-crate workspace where logic crates never import gpui and `faber-app` is a thin GPUI shell (CLAUDE.md rule, compiler-enforced). Today's index-like work is scattered:

- `faber-editor/src/file_index.rs`: async full-tree walk (ignore crate, 200K cap), in-memory only, stale-after-5s rescan, kicked from `workspace.rs:534`.
- `faber-editor/src/outline.rs`: per-document tree-sitter symbols, recomputed on every edit, never project-wide.
- `faber-editor/src/project_search.rs`: on-demand walk per query (stays as is in v1).
- No filesystem watcher. No persistence beyond TOML settings/state.

Design sources: Zed's worktree scanner and (removed) `semantic_index` crate, IntelliJ's `FileBasedIndexExtension` model, rust-analyzer's revision-based laziness. Key ideas adopted: registry of versioned pure modules, scanning vs indexing phase split, mtime-then-hash staleness, single writer with MVCC readers, snapshot publication, LSP-shaped progress events.

## Architecture

New GPUI-free crate `faber-index`, depending on `faber-core` and `faber-lang` only. `faber-app` wires it to GPUI executors and UI.

```
crates/faber-index/src/
  lib.rs        IndexEngine facade, registry
  module.rs     IndexModule trait, FileInput, ModuleState
  scanner.rs    tree walk + stamp merge-join → dirty set
  store.rs      heed env, per-module DBs, stamps, meta
  pipeline.rs   staged channels: read → hash → parse → fan-out → write
  watcher.rs    notify wrapper: debounce, coalesce, overflow fallback
  trigger.rs    IndexTrigger queue with coalescing
  progress.rs   ProgressEvent types
  modules/
    files.rs    file index module (v1)
    symbols.rs  symbol index module (v1)
```

### Module trait (registry pattern)

```rust
pub trait IndexModule: Send + Sync {
    fn name(&self) -> &'static str;            // stable; names the LMDB sub-DBs
    fn version(&self) -> u32;                  // bump ⇒ rebuild this module only
    fn accepts(&self, meta: &FileMeta) -> bool;        // extension/size filter
    fn index(&self, input: &FileInput) -> anyhow::Result<Vec<(Vec<u8>, Vec<u8>)>>;
    fn publish(&self, txn: &ReadTxn, db: ModuleDb) -> anyhow::Result<Arc<dyn Any + Send + Sync>>;
}
```

Rules copied from IntelliJ, enforced by the shape of the API:

- `index` is a pure function of `FileInput`. No filesystem reads, no globals. This is what makes modules independently rebuildable and testable.
- `FileInput { rel_path, text, language, syntax: Option<&Tree> }`: the engine parses once with tree-sitter and shares the tree, so N modules don't parse N times.
- Keys are path-prefixed (`path bytes + \0 + suffix`, Zed's `db_key_for_path` trick) so deleting a file's entries is one range delete and iteration follows tree order.
- `publish` builds the in-memory snapshot consumers read (e.g. `Arc<FileIndexSnapshot>` for the finder). Called after each committed run; stored in an `ArcSwap` per module.

### Storage (LMDB via heed)

One `heed::Env` per project at `~/.cache/faber/index/<blake3(root_abs_path)>/`, map size 1 GiB. Named databases:

- `meta`: engine schema version, per-module recorded version.
- `stamps:{module}`: rel_path → `Stamp { mtime, size, blake3 }`. Per module, so version bumps invalidate independently.
- `data:{module}`: the module's key/value entries.

Consistency: for each file, stamp and data writes share one write transaction. A crash mid-run loses at most uncommitted files; the next scan's merge-join reindexes them. No clean-shutdown flag needed, LMDB is copy-on-write. On `meta` version mismatch or open failure: drop the affected DBs (or the whole env dir) and rebuild in the background, never surface an error dialog.

Single writer task owns all write transactions. Readers open MVCC read transactions and never block the writer.

### Engine run loop

One run at a time, serial, runs to completion (PRD FR7). Triggers arriving mid-run coalesce into at most one queued follow-up.

```
trigger → scan phase → index phase → publish phase
```

- Scan: walk the tree (ignore crate, same rules as today) collecting `FileMeta`; merge-join the sorted walk against each module's stamp DB (Zed's sorted merge-join): entries missing on disk → deletion set; present in both → dirty if mtime+size differ *and* blake3 differs (hash computed lazily, only when mtime/size changed; branch switches touch many mtimes but few contents); missing in DB → new. Scoped triggers (single file save) stat only the affected paths and skip the walk.
- Index: staged pipeline on background threads, bounded channels for backpressure:
  `read+hash (num_cpus workers, bounded 512) → parse once → fan out to accepting modules → single writer (batched txns, ~100 files per commit)`.
- Publish: writer bumps a generation counter, calls each dirty module's `publish`, swaps the `ArcSwap` snapshots, emits `ProgressEvent::End`.

### Triggers

```rust
pub enum IndexTrigger {
    FolderOpened,
    FileSaved(PathBuf),
    ExternalChanges(Vec<PathBuf>),   // from watcher
    Manual,                          // settings button: re-verify everything
}
```

`IndexEngine::request(trigger)` is the only entry point; any app code can call it. This is the abstraction the PRD asks for: features never talk to the scanner or the store directly.

### Watcher

`notify` recommended watcher on the project root, Zed-style handling:

- Callback pushes into a shared pending buffer; a drain task sleeps 100 ms then takes the whole buffer (debounce + batch).
- Coalesce per path (create+modify+delete of one path collapses), filter through the same ignore rules as the scanner.
- Overflow or rescan events → fall back to a full `FolderOpened`-style scan. Watching is an optimization over scanning, never the source of truth.
- Watcher lives in `faber-index`; `faber-app` starts it when a folder opens and drops it on close (drop = cancellation, GPUI convention).

### Threading model (faber-app wiring)

No tokio; GPUI executors only, matching the codebase:

- `cx.background_spawn` runs the engine loop; the pipeline uses `background_executor().scoped()` for worker pools, as Zed's scanner does.
- A foreground `cx.spawn` drains the progress channel and updates `Entity<IndexStatus>`; views observe it with `cx.observe`. The engine never touches UI, the UI never touches the store.
- Tasks are stored on `Workspace` fields so dropping the workspace tears everything down. User-facing cancellation stays impossible; app quit is the only stop.

### Progress contract

LSP-shaped events over an unbounded channel:

```rust
pub enum ProgressEvent {
    Begin { run_id: u64 },
    Report { phase: Phase, done: usize, total: usize },  // Phase: Scanning | Indexing { module } | Publishing
    End { run_id: u64, files_indexed: usize },
}
```

Progress counting uses RAII handles traveling with each file through the pipeline (Zed's `IndexingEntrySet`): drop = done, so counts can't desync. The status item only renders when `files_indexed_estimate > ~50`, keeping warm starts silent.

### UI: status bar

New generic component in `faber-app/src/status_bar.rs`:

- Renders at the bottom of `Workspace::render`, below the existing bottom panel, ~26 px, themed like the titlebar.
- API: left/right slots of `AnyView` items, so future items (cursor position, git branch) plug in without changes.
- `IndexingStatusItem`: observes `Entity<IndexStatus>`; renders a 70 px determinate bar plus a label. All strings via `t!()` with keys in `locales/en.toml` (run `/i18n-guardrails` before finishing).

Settings: new `Reindex project` button in Settings > General (`settings_view.rs`), enabled when a root folder is set, sends `IndexTrigger::Manual`.

### V1 modules

- `files`: replaces `file_index.rs` as the finder's source. Entries carry rel_path, name, extension, ignored flag. `publish` materializes today's `FileIndexSnapshot` shape so `filter()`, nucleo matching, and the finder UI stay untouched. The 5-second staleness hack and `kick_index_scan` are deleted; the finder subscribes to snapshot swaps instead. The "include ignored" variant becomes a flag on entries rather than a second walk.
- `symbols`: runs the existing outline tree-sitter queries (`outline.rs`) per accepted file, stores `Vec<Symbol { name, kind, range }>` per path. V1 exposes a read API (`symbols_for(path)`, `all_symbols()`); a project-wide symbol picker consumes it in a later PR. Open documents keep their live per-edit outline; the index covers the unopened rest (IntelliJ's memory-overlay idea, simplified: open-buffer data wins at query time).

### Feature gating

Per-module state, no global dumb mode:

```rust
pub enum ModuleState { Cold, Building { done: usize, total: usize }, Ready { generation: u64 } }
```

Consumers check state and degrade inline (finder shows an "indexing…" row). Nothing throws, nothing blocks.

## Alternatives considered

- SQLite WAL instead of LMDB: single file, transactional multi-DB commits, FTS5 for free. Chosen against in review: LMDB has zero-copy reads, no checkpointing, and a proven shape in Zed's index on the same stack. Revisit if multi-module transactions or tooling needs grow.
- Flat bincode files per module: simplest, but whole-index rewrites per commit kill incrementality.
- No persistence (rescan each start): today's model; fails the warm-start goal as modules and project sizes grow.
- tokio: rejected, the codebase is GPUI-executor only and gains nothing from a second runtime.
- Global dumb mode (IntelliJ pre-2023): rejected for per-module gating; scanning-in-smart-mode is the industry direction.

## Migration plan (PRs ≤ 700 LOC each)

1. `faber-index` crate: trait, store, scanner, pipeline, `files` module, unit tests. No app wiring.
2. App wiring: engine spawn on folder open, triggers for open/save, finder reads published snapshots, delete `kick_index_scan` + staleness timer.
3. Status bar component + indexing item + i18n keys.
4. Watcher + `ExternalChanges` trigger + settings reindex button.
5. `symbols` module + read API.
6. CLAUDE.md section (indexing architecture rules, "new project-wide computation ⇒ new module, never a bespoke scan") and a `.claude/skills/index-module/SKILL.md` guide covering: trait contract, purity rule, key layout, version bumping, publish snapshots, and the testing pattern.

## Risks

- LMDB map sizing: fixed at open; 1 GiB virtual reservation is fine on 64-bit but needs a resize story if ever exceeded (log + rebuild larger).
- Hashing cost on cold start: bounded by skipping files over 1 MB and binary sniffing (first-KB NUL check), pending PRD open question.
- Watcher platform quirks (FSEvents coalescing, inotify limits): mitigated by the overflow-falls-back-to-scan rule.
- Tree-sitter parse cost in the pipeline: bounded by `accepts` filters; only `symbols` requests parses, and only for supported languages.
- Finder history (`ProjectHistory`) interplay: history stays in `state.toml`; the index stores no per-user data, keeping the cache safe to delete.

## Testing

- Modules are pure: table-driven unit tests on `index()` with string inputs, no filesystem.
- Engine tests against tempdir fixtures: cold build, warm no-op, touch-without-change (mtime bump, same content ⇒ zero reindex), edit, delete, rename, version-bump rebuild, corrupt-env recovery.
- Watcher tests behind a small `ChangeSource` trait so the debounce/coalesce logic tests without real fs events.
- App layer: existing pattern, logic stays out of `faber-app`; the status item renders from a plain `IndexStatus` value.

## Open questions

- Same as PRD, plus: should `publish` snapshots be incremental (apply diff) instead of rebuilt per run? Rebuild is O(entries) and fine at 200K; measure before complicating.
- Store one shared stamp DB with per-module generation markers instead of per-module stamp DBs, if duplicate hashing shows up in profiles (hash is computed once per run regardless; only stamp storage duplicates).
