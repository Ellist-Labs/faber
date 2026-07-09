//! Synchronous project walk producing a `FileIndexSnapshot`.
//!
//! The finder data layer (`FileEntry`, `FileIndexSnapshot`, `FinderQuery`,
//! `FinderMatch`, `filter`) lives in `faber-index` and is re-exported here so
//! existing consumers keep a single import surface. This module owns only the
//! `ignore::WalkBuilder`-based scan; the async engine replaces it in a later wave.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use ignore::WalkBuilder;

pub use faber_index::files::{
    FileEntry, FileIndexSnapshot, FinderMatch, FinderQuery, INDEX_LIMIT, filter,
};

pub fn scan(root: &Path, include_ignored: bool) -> FileIndexSnapshot {
    scan_with_limit(root, include_ignored, INDEX_LIMIT)
}

pub fn scan_with_limit(root: &Path, include_ignored: bool, limit: usize) -> FileIndexSnapshot {
    // The full walk can't tell which entries a normal walk would have skipped,
    // so collect the non-ignored set first and diff against it.
    let normal_set: Option<HashSet<String>> = include_ignored.then(|| {
        walk(root, false, limit)
            .into_iter()
            .map(|(p, _)| p)
            .collect()
    });

    let raw = walk(root, include_ignored, limit);
    let truncated = raw.len() >= limit;

    let mut ext_counts: HashMap<String, u32> = HashMap::new();
    let mut entries: Vec<FileEntry> = raw
        .into_iter()
        .map(|(rel_path, name_off)| {
            if let Some(ext) = extension_of(&rel_path[name_off as usize..]) {
                *ext_counts.entry(ext).or_insert(0) += 1;
            }
            let is_ignored = normal_set
                .as_ref()
                .is_some_and(|set| !set.contains(rel_path.as_str()));
            FileEntry {
                rel_path,
                name_off,
                is_ignored,
            }
        })
        .collect();
    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    let mut extensions: Vec<(String, u32)> = ext_counts.into_iter().collect();
    extensions.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    FileIndexSnapshot {
        root: root.to_owned(),
        entries,
        extensions,
        truncated,
    }
}

/// Walk the tree, returning `(rel_path, name_off)` pairs, capped at `limit`.
fn walk(root: &Path, include_ignored: bool, limit: usize) -> Vec<(String, u32)> {
    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(!include_ignored)
        .git_ignore(!include_ignored)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|e| e.file_name() != ".git");

    let mut out = Vec::new();
    for entry in walker.build().filter_map(|e| e.ok()) {
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let rel_path = rel.to_string_lossy().into_owned();
        let name_off = rel_path.rfind('/').map(|i| i + 1).unwrap_or(0) as u32;
        out.push((rel_path, name_off));
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn extension_of(name: &str) -> Option<String> {
    match name.rfind('.') {
        // Dot must not be first or last char of the name (".gitignore" has no ext).
        Some(i) if i > 0 && i + 1 < name.len() => Some(name[i + 1..].to_lowercase()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn touch(dir: &TempDir, rel: &str) {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"x").unwrap();
    }

    #[test]
    fn indexes_files_with_rel_paths_and_name_offsets() {
        let dir = TempDir::new().unwrap();
        touch(&dir, "src/main.rs");
        touch(&dir, "Cargo.toml");

        let snap = scan(dir.path(), false);
        assert_eq!(snap.entries.len(), 2);
        assert_eq!(snap.entries[0].rel_path, "Cargo.toml");
        assert_eq!(snap.entries[0].name(), "Cargo.toml");
        assert_eq!(snap.entries[1].rel_path, "src/main.rs");
        assert_eq!(snap.entries[1].name(), "main.rs");
    }

    #[test]
    fn respects_gitignore_and_flags_ignored_in_full_scan() {
        let dir = TempDir::new().unwrap();
        touch(&dir, "src/lib.rs");
        touch(&dir, "target/out.bin");
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();

        let normal = scan(dir.path(), false);
        let paths: Vec<_> = normal.entries.iter().map(|e| e.rel_path.as_str()).collect();
        assert!(paths.contains(&"src/lib.rs"));
        assert!(!paths.iter().any(|p| p.starts_with("target/")));
        assert!(normal.entries.iter().all(|e| !e.is_ignored));

        let full = scan(dir.path(), true);
        let target = full
            .entries
            .iter()
            .find(|e| e.rel_path == "target/out.bin")
            .unwrap();
        assert!(target.is_ignored);
        let lib = full
            .entries
            .iter()
            .find(|e| e.rel_path == "src/lib.rs")
            .unwrap();
        assert!(!lib.is_ignored);
        // Dotfiles show up in the full scan, flagged as ignored.
        let gi = full
            .entries
            .iter()
            .find(|e| e.rel_path == ".gitignore")
            .unwrap();
        assert!(gi.is_ignored);
    }

    #[test]
    fn never_indexes_git_dir() {
        let dir = TempDir::new().unwrap();
        touch(&dir, ".git/config");
        touch(&dir, "a.rs");

        let full = scan(dir.path(), true);
        assert!(
            full.entries
                .iter()
                .all(|e| !e.rel_path.starts_with(".git/"))
        );
    }

    #[test]
    fn counts_extensions_most_frequent_first() {
        let dir = TempDir::new().unwrap();
        touch(&dir, "a.rs");
        touch(&dir, "b.rs");
        touch(&dir, "c.toml");
        touch(&dir, "README");
        touch(&dir, ".gitignore");

        let snap = scan(dir.path(), false);
        assert_eq!(snap.extensions, vec![("rs".into(), 2), ("toml".into(), 1)]);
    }

    #[test]
    fn truncates_at_limit() {
        let dir = TempDir::new().unwrap();
        for i in 0..10 {
            touch(&dir, &format!("f{i}.txt"));
        }
        let snap = scan_with_limit(dir.path(), false, 5);
        assert_eq!(snap.entries.len(), 5);
        assert!(snap.truncated);
    }

    #[test]
    fn extension_edge_cases() {
        assert_eq!(extension_of("main.rs"), Some("rs".into()));
        assert_eq!(extension_of("a.test.TS"), Some("ts".into()));
        assert_eq!(extension_of(".gitignore"), None);
        assert_eq!(extension_of("Makefile"), None);
        assert_eq!(extension_of("trailing."), None);
    }
}
