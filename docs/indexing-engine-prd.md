# Indexing engine PRD

Status: draft, revised after four-lens design review
Related: [indexing-engine-tdd.md](indexing-engine-tdd.md)

## Problem

Faber recomputes project knowledge from scratch on every session and has no shared pattern for project-wide computations:

- The file finder rescans the whole tree on folder open, keeps the result only in memory, and rescans whenever the snapshot is older than 5 seconds (`file_index.rs`, `workspace.rs:534`).
- Symbols exist only for open documents (`outline.rs` runs per edit). There's no project-wide symbol data, so features like "go to symbol in project" can't exist.
- Nothing reacts to external changes: a git checkout or an edit from another tool leaves stale data until the 5-second timer fires.
- The user gets zero feedback while background work runs.
- Each new project-wide feature would invent its own scanning, caching, and threading. That doesn't scale as modules grow.

## Goals

- One engine owns all project-wide indexing. New modules plug in through a registry without touching the engine or existing modules.
- Indexing runs fully in the background. The app starts and stays usable while indexing runs; the engine only starts after the first frame paints.
- Indexing is lazy. Only changed files get reindexed. A warm start with no changes does no visible work.
- Results persist locally. A returning session serves features from the persisted index at first paint, then verifies in the background.
- Each module becomes ready independently, as early as possible. The file list is ready right after the tree walk; symbol data follows when its own work completes. No feature waits on the slowest module.
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

- As a user, I open a large folder and can browse, edit, and save immediately; the file finder works seconds later, while symbol indexing continues behind me.
- As a user, I reopen a project I used yesterday and the file finder answers instantly from the persisted index, with no progress UI flashing.
- As a user, I switch git branches in a terminal and faber picks up the changes without me doing anything.
- As a user, I create and save a new file during a long cold index and it shows up in the finder right away, not minutes later.
- As a user, I see a small progress bar and a label like "Indexing symbols… 1,204 / 8,930" in the bottom bar while a cold index runs.
- As a user, I can trigger a full reindex from settings when something looks stale.
- As a developer, I add a new index module by implementing one trait and registering it, without touching the engine or other modules.

## Functional requirements

- FR1: an `IndexEngine` with a registry of versioned modules. Bumping a module's version rebuilds only that module.
- FR2: all scanning, hashing, parsing, and writing happens on engine-owned background threads. The UI thread only receives throttled status updates and published snapshots.
- FR3: modules declare what they need (metadata, text, syntax). Metadata-only modules index straight from the tree walk without reading file contents. Content staleness uses mtime + size first, content hash second; unchanged files are never reindexed.
- FR4: persisted store per project, survives restarts, self-heals on corruption or version mismatch by rebuilding the affected module in the background, with the reason logged.
- FR5: on startup with a valid persisted index, modules publish from the store before any scanning, then a background verification scan runs.
- FR6: a trigger abstraction any part of the app can call (folder open, file save, manual, watcher batch). Triggers arriving mid-run coalesce by scope into at most one follow-up run; saves during a run join the in-flight run instead of waiting behind it.
- FR7: a filesystem watcher with debounce, batching, and save-echo suppression feeds the trigger abstraction, so external changes reindex automatically.
- FR8: runs are serial and run to completion. No pause, no cancel.
- FR9: v1 ships two modules: file index (replaces `file_index.rs` as the finder's source) and symbol index (project-wide tree-sitter outlines; open buffers win at query time).
- FR10: a generic bottom status bar component hosting an indexing status item; the item appears only for runs still active after ~800 ms and hides when idle.
- FR11: a "Reindex project" action in Settings > General, enabled only when a folder is open. It re-verifies every file by re-hashing.
- FR12: per-module readiness state; consumers query it and show an "indexing…" hint instead of erroring or blocking. Prioritized order: open files first, then recently used files, then the rest.

## UX

Bottom status bar:

- Sticky strip (~26 px) at the very bottom of the window, below the pane group and existing bottom panel, in the style of Zed and VS Code status bars.
- Generic left and right item slots so future items (cursor position, language, git branch) reuse it.
- Indexing item (right side): thin determinate progress bar (~70 px) plus rotating label reflecting the current step: "Scanning files…", "Indexing symbols… N / M". Indeterminate while counting.
- Timing rules (prevents both flicker and silent stalls): item appears only if the run is still going ~800 ms after it starts; once shown it stays at least ~1 s; labels dwell at least 500 ms; counts update in place at most 10 times per second.
- Warm starts and small deltas finish inside the 800 ms window, so they show nothing at all.
- Idle: the item disappears; the bar stays (it's a permanent surface).

Degraded features during a cold build:

- File finder: fully usable seconds after folder open (file list publishes right after the tree walk). Before that, it opens, renders history rows (which live outside the index and always work), and shows an "indexing…" hint row.
- Future symbol search: gated on the symbols module only, with the same hint pattern.
- Saving, editing, tree browsing, and project search never degrade.

## Success metrics

- Window is interactive before any indexing work starts, on every project size.
- Warm start with no changes: zero files reindexed, no progress UI, finder ready at first paint from the persisted index.
- Cold open of a 200K-file project: finder ready in walk time (seconds), not content time (minutes).
- Branch switch touching 500 files: only those files reindex; UI stays at 60 fps.
- A file saved during a cold run appears in the finder within seconds.
- Adding a third module requires no edits to the engine or the two v1 modules.

## Open questions

- Max file size to hash and index (proposed: skip files over 1 MB and binaries).
- GC window for index caches of projects not opened recently (proposed: delete after 30 days).
- Should project search consume the file module's list in v1 to skip its own walk, or stay untouched until v2?
- Should the existing 180 px "Terminal" bottom panel and the new status bar coexist visually, or should the panel dock above the bar permanently?
- Later: a destructive "Rebuild index" action (drop the store) next to the verify-style "Reindex project"?
