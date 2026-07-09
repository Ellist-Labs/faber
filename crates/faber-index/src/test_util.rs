//! Shared test helpers.
//!
//! The store keys its cache dir off `$HOME`. The engine/scanner tests here never
//! mutate the process `HOME` — they open a store against a unique temp project
//! root, which yields a unique per-project cache subdir under whatever the
//! ambient `HOME` is. Not swapping `HOME` means these tests cannot race the
//! store's own tests (which swap/restore `HOME` under a private lock) into a
//! poisoned lock or a mis-scoped GC.

use std::path::Path;

use tempfile::TempDir;

/// Run `f` with a fresh, unique temporary project root. The store's cache lands
/// under the ambient `HOME`, keyed by a blake3 of this unique root, so runs never
/// collide across tests.
pub(crate) fn with_project<R>(f: impl FnOnce(&Path) -> R) -> R {
    let project = TempDir::new().expect("create temp project root");
    f(project.path())
}
