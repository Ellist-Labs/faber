use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// One indexed file, path relative to the project root.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Relative path, '/'-separated.
    pub rel_path: String,
    /// Byte offset where the file name starts within `rel_path`.
    pub name_off: u32,
    /// True when the entry only shows up because gitignored/hidden files were included.
    pub is_ignored: bool,
}

impl FileEntry {
    pub fn name(&self) -> &str {
        &self.rel_path[self.name_off as usize..]
    }

    /// Lowercased extension (after the last dot of the name), if any.
    pub fn extension(&self) -> Option<String> {
        extension_of(self.name())
    }
}

/// Immutable result of one project walk. Cheap to share via `Arc`.
#[derive(Debug, Clone, Default)]
pub struct FileIndexSnapshot {
    pub root: PathBuf,
    /// Sorted by `rel_path`.
    pub entries: Vec<FileEntry>,
    /// Unique lowercased extensions with file counts, most frequent first.
    pub extensions: Vec<(String, u32)>,
    /// True when the walk stopped at the entry cap.
    pub truncated: bool,
}

/// Upper bound on indexed entries; keeps memory bounded on pathological repos.
pub const INDEX_LIMIT: usize = 200_000;

pub fn scan(root: &Path, include_ignored: bool) -> FileIndexSnapshot {
    scan_with_limit(root, include_ignored, INDEX_LIMIT)
}

pub fn scan_with_limit(root: &Path, include_ignored: bool, limit: usize) -> FileIndexSnapshot {
    // The full walk can't tell which entries a normal walk would have skipped,
    // so collect the non-ignored set first and diff against it.
    let normal_set: Option<HashSet<String>> = include_ignored
        .then(|| walk(root, false, limit).into_iter().map(|(p, _)| p).collect());

    let raw = walk(root, include_ignored, limit);
    let truncated = raw.len() >= limit;

    let mut ext_counts: HashMap<String, u32> = HashMap::new();
    let mut entries: Vec<FileEntry> = raw
        .into_iter()
        .map(|(rel_path, name_off)| {
            if let Some(ext) = extension_of(&rel_path[name_off as usize..]) {
                *ext_counts.entry(ext).or_insert(0) += 1;
            }
            let is_ignored =
                normal_set.as_ref().is_some_and(|set| !set.contains(rel_path.as_str()));
            FileEntry { rel_path, name_off, is_ignored }
        })
        .collect();
    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    let mut extensions: Vec<(String, u32)> = ext_counts.into_iter().collect();
    extensions.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    FileIndexSnapshot { root: root.to_owned(), entries, extensions, truncated }
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
        let Ok(rel) = entry.path().strip_prefix(root) else { continue };
        let rel_path = rel.to_string_lossy().into_owned();
        let name_off = rel_path.rfind('/').map(|i| i + 1).unwrap_or(0) as u32;
        out.push((rel_path, name_off));
        if out.len() >= limit {
            break;
        }
    }
    out
}

// ── filtering ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct FinderQuery {
    pub text: String,
    pub case_sensitive: bool,
    /// Match against the file name only (not the whole path).
    pub whole_word: bool,
    /// Treat `text` as a regex over the relative path.
    pub regex: bool,
    /// Lowercased extension lock (e.g. "rs").
    pub mask: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FinderMatch {
    pub entry_ix: u32,
    pub score: u32,
    pub from_history: bool,
    /// Char indices into `rel_path` of matched characters (fuzzy/regex highlight).
    pub positions: Vec<u32>,
}

/// Score boost that pins history entries above ordinary matches, recent first.
const HISTORY_BOOST: u32 = 1 << 24;

/// Filter `snap` against `q`. Empty query returns `history` entries (that still
/// exist in the snapshot) in most-recent-first order. Results capped at `limit`.
pub fn filter(
    snap: &FileIndexSnapshot,
    q: &FinderQuery,
    history: &[String],
    limit: usize,
) -> Vec<FinderMatch> {
    let mask_ok = |e: &FileEntry| match &q.mask {
        Some(m) => e.extension().as_deref() == Some(m.as_str()),
        None => true,
    };
    let history_rank: HashMap<&str, u32> = history
        .iter()
        .enumerate()
        .map(|(i, p)| (p.as_str(), i as u32))
        .collect();
    let boost = |rank: Option<&u32>| match rank {
        Some(&r) => HISTORY_BOOST + HISTORY_BOOST / 2 - r.min(HISTORY_BOOST / 2),
        None => 0,
    };

    if q.text.is_empty() {
        // Idle state: history only, most recent first.
        let mut out: Vec<FinderMatch> = Vec::new();
        for rel in history {
            if out.len() >= limit {
                break;
            }
            if let Ok(ix) = snap.entries.binary_search_by(|e| e.rel_path.as_str().cmp(rel)) {
                if mask_ok(&snap.entries[ix]) {
                    out.push(FinderMatch {
                        entry_ix: ix as u32,
                        score: 0,
                        from_history: true,
                        positions: Vec::new(),
                    });
                }
            }
        }
        return out;
    }

    // Compile regex / build matcher once — reused in both passes.
    let compiled_re = if q.regex {
        let Some(re) = build_regex(q) else { return Vec::new() };
        Some(re)
    } else {
        None
    };
    let case = if q.case_sensitive { CaseMatching::Respect } else { CaseMatching::Ignore };
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(&q.text, case, Normalization::Smart);

    // Pass 1: score only (no per-candidate allocation).
    let mut scored: Vec<(u32, u32, bool)> = Vec::new(); // (entry_ix, score, from_history)
    if let Some(ref re) = compiled_re {
        for (ix, e) in snap.entries.iter().enumerate() {
            if !mask_ok(e) {
                continue;
            }
            let Some(first) = re.find(&e.rel_path) else { continue };
            // Later match start and longer path rank lower.
            let score = 100_000u32
                .saturating_sub(first.start() as u32 * 16)
                .saturating_sub(e.rel_path.len() as u32);
            let rank = history_rank.get(e.rel_path.as_str());
            scored.push((ix as u32, score + boost(rank), rank.is_some()));
        }
    } else {
        let mut buf = Vec::new();
        for (ix, e) in snap.entries.iter().enumerate() {
            if !mask_ok(e) {
                continue;
            }
            let haystack = if q.whole_word { e.name() } else { e.rel_path.as_str() };
            let Some(score) = pattern.score(Utf32Str::new(haystack, &mut buf), &mut matcher)
            else {
                continue;
            };
            let rank = history_rank.get(e.rel_path.as_str());
            scored.push((ix as u32, score + boost(rank), rank.is_some()));
        }
    }

    scored.sort_by(|a, b| {
        b.1.cmp(&a.1).then_with(|| snap.entries[a.0 as usize].rel_path.cmp(&snap.entries[b.0 as usize].rel_path))
    });
    scored.truncate(limit);

    // Pass 2: match positions for the survivors only.
    let mut out: Vec<FinderMatch> = Vec::with_capacity(scored.len());
    if let Some(ref re) = compiled_re {
        for (entry_ix, score, from_history) in scored {
            let e = &snap.entries[entry_ix as usize];
            let positions = re
                .find(&e.rel_path)
                .map(|m| {
                    let start = e.rel_path[..m.start()].chars().count() as u32;
                    let len = e.rel_path[m.start()..m.end()].chars().count() as u32;
                    (start..start + len).collect()
                })
                .unwrap_or_default();
            out.push(FinderMatch { entry_ix, score, from_history, positions });
        }
    } else {
        let mut buf = Vec::new();
        for (entry_ix, score, from_history) in scored {
            let e = &snap.entries[entry_ix as usize];
            let haystack = if q.whole_word { e.name() } else { e.rel_path.as_str() };
            let mut positions: Vec<u32> = Vec::new();
            pattern.indices(Utf32Str::new(haystack, &mut buf), &mut matcher, &mut positions);
            positions.sort_unstable();
            positions.dedup();
            if q.whole_word {
                // Positions were relative to the name segment; the name is ASCII-offset
                // by the char count of the directory prefix.
                let prefix_chars = e.rel_path[..e.name_off as usize].chars().count() as u32;
                for p in &mut positions {
                    *p += prefix_chars;
                }
            }
            out.push(FinderMatch { entry_ix, score, from_history, positions });
        }
    }
    out
}

fn build_regex(q: &FinderQuery) -> Option<regex::Regex> {
    let pat =
        if q.whole_word { format!(r"\b(?:{})\b", q.text) } else { q.text.clone() };
    regex::RegexBuilder::new(&pat).case_insensitive(!q.case_sensitive).build().ok()
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
        let target = full.entries.iter().find(|e| e.rel_path == "target/out.bin").unwrap();
        assert!(target.is_ignored);
        let lib = full.entries.iter().find(|e| e.rel_path == "src/lib.rs").unwrap();
        assert!(!lib.is_ignored);
        // Dotfiles show up in the full scan, flagged as ignored.
        let gi = full.entries.iter().find(|e| e.rel_path == ".gitignore").unwrap();
        assert!(gi.is_ignored);
    }

    #[test]
    fn never_indexes_git_dir() {
        let dir = TempDir::new().unwrap();
        touch(&dir, ".git/config");
        touch(&dir, "a.rs");

        let full = scan(dir.path(), true);
        assert!(full.entries.iter().all(|e| !e.rel_path.starts_with(".git/")));
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

    fn snap_of(paths: &[&str]) -> FileIndexSnapshot {
        let mut entries: Vec<FileEntry> = paths
            .iter()
            .map(|p| FileEntry {
                rel_path: p.to_string(),
                name_off: p.rfind('/').map(|i| i + 1).unwrap_or(0) as u32,
                is_ignored: false,
            })
            .collect();
        entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        FileIndexSnapshot { root: PathBuf::new(), entries, extensions: Vec::new(), truncated: false }
    }

    fn q(text: &str) -> FinderQuery {
        FinderQuery { text: text.into(), ..Default::default() }
    }

    #[test]
    fn fuzzy_matches_subsequences_and_reports_positions() {
        let snap = snap_of(&["src/workspace.rs", "crates/faber-app/src/main.rs", "README.md"]);
        let m = filter(&snap, &q("wsp"), &[], 100);
        assert_eq!(m.len(), 1);
        assert_eq!(snap.entries[m[0].entry_ix as usize].rel_path, "src/workspace.rs");
        assert!(!m[0].positions.is_empty());
    }

    #[test]
    fn empty_query_returns_history_in_order() {
        let snap = snap_of(&["a.rs", "b.rs", "c.rs"]);
        let history = vec!["b.rs".to_string(), "gone.rs".to_string(), "a.rs".to_string()];
        let m = filter(&snap, &q(""), &history, 100);
        let paths: Vec<_> =
            m.iter().map(|x| snap.entries[x.entry_ix as usize].rel_path.as_str()).collect();
        assert_eq!(paths, ["b.rs", "a.rs"]); // missing files pruned, order kept
        assert!(m.iter().all(|x| x.from_history));
    }

    #[test]
    fn history_entries_rank_above_better_fuzzy_matches() {
        let snap = snap_of(&["exact.rs", "src/deep/loose_ex.rs"]);
        let history = vec!["src/deep/loose_ex.rs".to_string()];
        let m = filter(&snap, &q("ex"), &history, 100);
        assert_eq!(snap.entries[m[0].entry_ix as usize].rel_path, "src/deep/loose_ex.rs");
        assert!(m[0].from_history);
        assert!(!m[1].from_history);
    }

    #[test]
    fn mask_locks_extension() {
        let snap = snap_of(&["a.rs", "a.toml", "b.rs"]);
        let mut query = q("a");
        query.mask = Some("toml".into());
        let m = filter(&snap, &query, &[], 100);
        assert_eq!(m.len(), 1);
        assert_eq!(snap.entries[m[0].entry_ix as usize].rel_path, "a.toml");
    }

    #[test]
    fn case_sensitive_fuzzy() {
        let snap = snap_of(&["README.md", "readme.txt"]);
        let mut query = q("READ");
        query.case_sensitive = true;
        let m = filter(&snap, &query, &[], 100);
        assert_eq!(m.len(), 1);
        assert_eq!(snap.entries[m[0].entry_ix as usize].rel_path, "README.md");
    }

    #[test]
    fn whole_word_matches_name_only_with_shifted_positions() {
        let snap = snap_of(&["src/other.rs", "wid/get.rs", "src/widget.rs"]);
        let mut query = q("widget");
        query.whole_word = true;
        let m = filter(&snap, &query, &[], 100);
        assert_eq!(m.len(), 1);
        let e = &snap.entries[m[0].entry_ix as usize];
        assert_eq!(e.rel_path, "src/widget.rs");
        // Positions are path-relative: "widget" starts at char 4.
        assert_eq!(m[0].positions.first(), Some(&4));
    }

    #[test]
    fn regex_mode_matches_paths() {
        let snap = snap_of(&["src/editor_view.rs", "src/main.rs", "notes_view.md"]);
        let mut query = q(r".*_view\.rs");
        query.regex = true;
        let m = filter(&snap, &query, &[], 100);
        assert_eq!(m.len(), 1);
        assert_eq!(snap.entries[m[0].entry_ix as usize].rel_path, "src/editor_view.rs");
        assert!(!m[0].positions.is_empty());
    }

    #[test]
    fn invalid_regex_returns_no_matches() {
        let snap = snap_of(&["a.rs"]);
        let mut query = q("[");
        query.regex = true;
        assert!(filter(&snap, &query, &[], 100).is_empty());
    }

    #[test]
    fn limit_caps_results() {
        let paths: Vec<String> = (0..50).map(|i| format!("f{i}.rs")).collect();
        let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
        let snap = snap_of(&refs);
        assert_eq!(filter(&snap, &q("f"), &[], 10).len(), 10);
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
