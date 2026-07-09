//! Query-time merge: open-buffer outlines take priority over the LMDB symbol index.

use std::path::Path;

use faber_editor::{buffer::Document, outline::OutlineItem};
use faber_index::{store::IndexStore, symbols_for};

/// Look up outline items for `abs_path`.
///
/// If an open document for that path is provided, returns its live outline.
/// Otherwise queries the persisted LMDB symbol index.
///
/// `open_doc` — the caller resolves which open buffer (if any) corresponds to
/// `abs_path`; pass `None` when the file is not currently open.
#[allow(dead_code)]
pub fn outline_for_path(
    abs_path: &Path,
    open_doc: Option<&Document>,
    store: &IndexStore,
    project_root: &Path,
) -> anyhow::Result<Vec<OutlineItem>> {
    if let Some(doc) = open_doc {
        return Ok(doc.outline.items.clone());
    }

    let rel_bytes = abs_path
        .strip_prefix(project_root)
        .ok()
        .and_then(|p| p.to_str())
        .map(|s| s.replace('\\', "/").into_bytes())
        .unwrap_or_default();

    Ok(symbols_for(store, &rel_bytes)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::SystemTime;

    use faber_index::store::Stamp;
    use faber_lang::OutlineItem as LangOutlineItem;

    fn test_stamp() -> Stamp {
        Stamp {
            mtime: SystemTime::UNIX_EPOCH,
            size: 0,
            hash: None,
        }
    }

    fn encode_items(items: &[LangOutlineItem]) -> Vec<u8> {
        bincode::serialize(items).expect("serialize")
    }

    /// Write a synthetic outline entry using the same key layout as `symbols_for`:
    /// `{rel_path}\0outline`.
    fn write_outline(store: &IndexStore, rel_path_bytes: &[u8], items: &[LangOutlineItem]) {
        let entry = (
            rel_path_bytes.to_vec(),
            test_stamp(),
            vec![(b"outline".to_vec(), encode_items(items))],
        );
        store
            .write_batch("symbols", &[entry], false)
            .expect("write_batch");
    }

    fn make_item(name: &str) -> LangOutlineItem {
        LangOutlineItem {
            depth: 0,
            name: name.to_string(),
            context: None,
            source_line: 0,
            end_line: 0,
            byte_range: 0..1,
            block_ix: None,
        }
    }

    // Helper to redirect HOME so IndexStore::open uses an isolated cache dir.
    fn with_home<R>(home: &std::path::Path, f: impl FnOnce() -> R) -> R {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap();
        let prev = std::env::var_os("HOME");
        // SAFETY: guarded by LOCK; no other thread reads HOME concurrently here.
        unsafe { std::env::set_var("HOME", home) };
        let out = f();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
        out
    }

    /// Store path: items written to LMDB are returned when no open doc is given.
    #[test]
    fn store_path_returns_persisted_items() {
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();

        with_home(home.path(), || {
            let store = IndexStore::open(project.path()).unwrap();

            let items = vec![make_item("greet"), make_item("Config")];
            write_outline(&store, b"src/lib.rs", &items);

            let abs_path = project.path().join("src/lib.rs");
            let result = outline_for_path(&abs_path, None, &store, project.path()).unwrap();

            let names: Vec<&str> = result.iter().map(|i| i.name.as_str()).collect();
            assert!(names.contains(&"greet"), "expected greet; got {names:?}");
            assert!(names.contains(&"Config"), "expected Config; got {names:?}");
        });
    }

    /// Store path: path outside project root yields an empty result (not an error).
    #[test]
    fn path_outside_root_returns_empty() {
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();

        with_home(home.path(), || {
            let store = IndexStore::open(project.path()).unwrap();
            let outside = PathBuf::from("/tmp/unrelated.rs");
            let result = outline_for_path(&outside, None, &store, project.path()).unwrap();
            assert!(result.is_empty(), "outside-root path must return empty vec");
        });
    }

    // Note: the open-doc path (passing Some(&Document)) is covered by integration
    // tests once the "Go to Symbol in Project" UI is wired up. Document construction
    // requires LanguageRegistry + file I/O and is tested in faber-editor directly.
    // The core logic — `return Ok(doc.outline.items.clone())` — is a one-liner whose
    // correctness follows from the Document::outline invariant upheld in faber-editor.
}
