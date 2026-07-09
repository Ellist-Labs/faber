# Indexing engine PRD

Status: draft
Related: [indexing-engine-tdd.md](indexing-engine-tdd.md)

## Problem

Faber recomputes project knowledge from scratch on every session and has no shared pattern for project-wide computations:

- The file finder rescans the whole tree on folder open, keeps the result only in memory, and rescans whenever the snapshot is older than 5 seconds (`file_index.rs`, `workspace.rs:534`).
- Symbols exist only for open documents (`outline.rs` runs per edit). There's no project-wide symbol data, so features like "go to symbol in project" can't exist.
- Nothing reacts to external changes: a git checkout or an edit from another tool leaves stale data until the 5-second timer fires.
- The user gets zero feedback while background work runs.
- Each new project-wide feature would invent its own scanning, caching, and threading. That doesn't scale as modules grow.

## Goals

- One engine owns all project-wide indexing. New modules plug in through a registry without touching existing ones.
- Indexing runs fully in the background. The app starts and stays usable while indexing runs.
- Indexing is lazy. Only changed files get reindexed. A warm start with no changes does no visible work.
- Results persist locally, so the second session starts from the persisted index instead of a cold scan.
- The engine reindexes automatically on internal triggers (save, folder open) and external filesystem changes.
- A new sticky bottom status bar, generic enough to host future UI items, shows indexing progress with a subtle progress bar and a rotating step label.
- Settings gains a manual "Reindex project" action.
- Features degrade per capability while their index builds, with an inline hint, never a blocking state.

## Non-goals

- Semantic or embedding-based indexing.
- A persistent full-text search index. Project search stays an on-demand walk (VS Code ships ripgrep-on-demand for the same reason: always fresh, zero invalidation).
- Pausing or cancelling a run. Once started, a run completes (new triggers queue behind it).
- Shared or pre-built indexes, cross-project indexes.
- Git status indexing (candidate for a later module).

## User stories

- As a user, I open a large folder and can browse, edit, and save immediately while the index builds behind me.
- As a user, I reopen a project I used yesterday and the file finder answers instantly from the persisted index, with no progress UI flashing.
- As a user, I switch git branches in a terminal and faber picks up the changes without me doing anything.
- As a user, I see a small progress bar and a label like "Indexing symbols… 1,204 / 8,930" in the bottom bar while a cold index runs.
- As a user, I can trigger a full reindex from settings when something looks stale.
- As a developer, I add a new index module by implementing one trait and registering it, without touching the engine or other modules.

## Functional requirements

- FR1: an `IndexEngine` with a registry of versioned modules. Bumping a module's version rebuilds only that module.
- FR2: all scanning, hashing, parsing, and writing happens on background threads. The UI thread only receives status events and published snapshots.
- FR3: staleness detection per file via mtime + size first, content hash second. Unchanged files are never reindexed.
- FR4: persisted store per project, survives restarts, self-heals on corruption or version mismatch by rebuilding the affected module in the background.
- FR5: a trigger abstraction any part of the app can call (folder open, file save, manual, watcher batch). Triggers arriving mid-run coalesce into one follow-up run.
- FR6: a filesystem watcher with debounce and batching feeds the trigger abstraction, so external changes reindex automatically.
- FR7: runs are serial and run to completion. No pause, no cancel.
- FR8: v1 ships two modules: file index (replaces `file_index.rs` as the file finder's source) and symbol index (project-wide tree-sitter outlines).
- FR9: a generic bottom status bar component hosting an indexing status item; the item hides when idle.
- FR10: a "Reindex project" action in Settings > General, enabled only when a folder is open.
- FR11: per-module readiness state; consumers query it and show an "indexing…" hint instead of erroring or blocking.

## UX

Bottom status bar:

- Sticky strip (~26 px) at the very bottom of the window, below the pane group and existing bottom panel, in the style of Zed and VS Code status bars.
- Generic left and right item slots so future items (cursor position, language, git branch) reuse it.
- Indexing item (right side): thin determinate progress bar (~70 px) plus rotating label reflecting the current step: "Scanning files…", "Indexing files… N / M", "Indexing symbols… N / M". Indeterminate while counting.
- Silent warm start: when the dirty set is small (under ~50 files), no progress UI shows at all. IntelliJ uses the same threshold idea to avoid flashing.
- Idle: the item disappears; the bar stays (it's a permanent surface).

Degraded features during a cold build:

- File finder: opens, shows "indexing…" hint row until the file module publishes its first snapshot.
- Future symbol search: same pattern per module.

## Success metrics

- Window is interactive before any indexing work starts, on every project size.
- Warm start with no changes: zero files reindexed, no progress UI, finder ready at first paint.
- Branch switch touching 500 files: only those files reindex; UI stays at 60 fps.
- Cold index of a 10K-file project completes in the background in seconds, not minutes, without input latency regressions.
- Adding a third module requires no edits to the engine or the two v1 modules.

## Open questions

- Index location: `~/.cache/faber/index/<project-hash>/` (proposed) vs alongside `~/.config/faber/`. Cache semantics fit better; safe to delete.
- Manual reindex semantics: verify everything by re-hashing (proposed) vs dropping the store and rebuilding. Could be two actions later.
- Should project search consume the file module's list in v1 to skip its own walk, or stay untouched until v2?
- Max file size to hash and index (proposed: skip files over 1 MB and binaries).
- Should the existing 180 px "Terminal" bottom panel and the new status bar coexist visually, or should the panel dock above the bar permanently?
