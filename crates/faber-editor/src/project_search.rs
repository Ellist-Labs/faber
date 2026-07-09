use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use faber_core::search::Query;
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;

// ── public types ──────────────────────────────────────────────────────────────

/// Engine-side search query (no UI flags).
#[derive(Debug, Clone)]
pub struct ProjectSearchQuery {
    pub text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub include_ignored: bool,
    /// Comma-separated include glob patterns (empty = match all).
    pub includes: Vec<String>,
    /// Comma-separated exclude glob patterns.
    pub excludes: Vec<String>,
    /// When `Some`, only search these files (open-files-only mode).
    pub scope_paths: Option<Vec<PathBuf>>,
}

impl ProjectSearchQuery {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            case_sensitive: false,
            whole_word: false,
            regex: false,
            include_ignored: false,
            includes: Vec::new(),
            excludes: Vec::new(),
            scope_paths: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Split a comma-separated filter string into trimmed, non-empty patterns.
    pub fn parse_globs(s: &str) -> Vec<String> {
        s.split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(String::from)
            .collect()
    }
}

/// A single matching line within a file.
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// 0-based line number.
    pub line: usize,
    /// 0-based column (char offset within the line).
    pub col: usize,
    /// Char offset of the start of this line within the whole file (for replace).
    pub line_start_char: usize,
    /// The full source line text (for preview display).
    pub preview: String,
    /// Char ranges within `preview` where the match is; may be multiple if a
    /// pattern matches more than once on the same line.
    pub ranges: Vec<Range<usize>>,
}

/// All hits within one file.
#[derive(Debug, Clone)]
pub struct FileSearchResult {
    pub path: PathBuf,
    pub hits: Vec<SearchHit>,
}

// ── internal: match a single file ────────────────────────────────────────────

/// Match `text` against `query`, returning hits localised per line.
fn match_text(query: &Query, path: &Path, text: &str) -> Option<FileSearchResult> {
    let whole_file_matches = query.all_matches_str(text);
    if whole_file_matches.is_empty() {
        return None;
    }

    // Map whole-file char offsets → per-line hits.
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(
            text.char_indices()
                .filter(|(_, c)| *c == '\n')
                .map(|(i, _)| {
                    // char count up to and including the newline byte → next line start char
                    text[..i].chars().count() + 1
                }),
        )
        .collect();

    // Build a map: for each match range, find its line and localise the range.
    let mut line_hits: std::collections::BTreeMap<usize, Vec<Range<usize>>> =
        std::collections::BTreeMap::new();

    for m in &whole_file_matches {
        // Binary search for the line containing m.start.
        let line_idx = line_starts.partition_point(|&ls| ls <= m.start) - 1;
        let line_start_char = line_starts[line_idx];
        let local_start = m.start - line_start_char;
        let local_end = m.end - line_start_char;
        line_hits
            .entry(line_idx)
            .or_default()
            .push(local_start..local_end);
    }

    let lines: Vec<&str> = text.split('\n').collect();

    let hits: Vec<SearchHit> = line_hits
        .into_iter()
        .map(|(line_idx, ranges)| {
            let preview = lines.get(line_idx).unwrap_or(&"").to_string();
            let col = ranges.first().map(|r| r.start).unwrap_or(0);
            let lsc = line_starts.get(line_idx).copied().unwrap_or(0);
            SearchHit {
                line: line_idx,
                col,
                line_start_char: lsc,
                preview,
                ranges,
            }
        })
        .collect();

    if hits.is_empty() {
        None
    } else {
        Some(FileSearchResult {
            path: path.to_owned(),
            hits,
        })
    }
}

fn match_file(query: &Query, path: &Path) -> Option<FileSearchResult> {
    let content = std::fs::read(path).ok()?;
    // Skip binary files — bail on NUL byte.
    if content.contains(&0u8) {
        return None;
    }
    let text = String::from_utf8(content).ok()?;
    match_text(query, path, &text)
}

// ── public API ────────────────────────────────────────────────────────────────

/// Maximum total matches returned before a "limit reached" flag is raised.
pub const MATCH_LIMIT: usize = 10_000;

fn search_one(
    faber_query: &Query,
    path: &Path,
    open_buffers: &HashMap<PathBuf, String>,
) -> Option<FileSearchResult> {
    if let Some(text) = open_buffers.get(path) {
        match_text(faber_query, path, text)
    } else {
        match_file(faber_query, path)
    }
}

/// Filter `candidates` by the include/exclude globs in `query`, returning the
/// subset that should be searched. Paths that don't match includes (when set)
/// or that match excludes are dropped.
///
/// Pure function — no I/O, testable in isolation.
pub fn filter_candidates(
    candidates: &[PathBuf],
    root: &Path,
    query: &ProjectSearchQuery,
) -> Vec<PathBuf> {
    if query.includes.is_empty() && query.excludes.is_empty() {
        return candidates.to_vec();
    }
    let overrides = {
        let mut ob = OverrideBuilder::new(root);
        for pat in &query.excludes {
            let _ = ob.add(&format!("!{pat}"));
        }
        for pat in &query.includes {
            let _ = ob.add(pat);
        }
        ob.build().ok()
    };
    let Some(ov) = overrides else {
        return candidates.to_vec();
    };
    candidates
        .iter()
        .filter(|p| {
            let rel = p.strip_prefix(root).unwrap_or(p);
            !ov.matched(rel, false).is_ignore()
        })
        .cloned()
        .collect()
}

/// Run a project-wide search, calling `on_batch` for each file that has hits.
/// Returns `true` if the match limit was reached.
///
/// `candidates` — when `Some`, search only these paths (index-provided list);
/// when `None`, fall back to a `WalkBuilder` traversal of `root`.
///
/// `open_buffers` — in-memory text for dirty open documents, keyed by absolute
/// path. When a candidate is found here its content is used instead of disk.
///
/// Designed to be called from a background executor task.
pub fn run(
    query: &ProjectSearchQuery,
    root: &Path,
    candidates: Option<&[PathBuf]>,
    open_buffers: &HashMap<PathBuf, String>,
    mut on_batch: impl FnMut(FileSearchResult) -> bool,
) -> bool {
    if query.is_empty() {
        return false;
    }

    let faber_query = Query::new(&query.text)
        .case_sensitive(query.case_sensitive)
        .whole_word(query.whole_word)
        .regex(query.regex);

    let mut total_matches: usize = 0;
    let mut limit_reached = false;

    if let Some(scope) = &query.scope_paths {
        // Open-files-only: iterate the fixed path list.
        for path in scope {
            if let Some(result) = search_one(&faber_query, path, open_buffers) {
                total_matches += result.hits.len();
                let keep_going = on_batch(result);
                if !keep_going || total_matches >= MATCH_LIMIT {
                    limit_reached = total_matches >= MATCH_LIMIT;
                    break;
                }
            }
        }
        return limit_reached;
    }

    if let Some(paths) = candidates {
        // Index-provided list: skip the WalkBuilder traversal.
        for path in paths {
            if let Some(result) = search_one(&faber_query, path, open_buffers) {
                total_matches += result.hits.len();
                let keep_going = on_batch(result);
                if !keep_going || total_matches >= MATCH_LIMIT {
                    limit_reached = total_matches >= MATCH_LIMIT;
                    break;
                }
            }
        }
        return limit_reached;
    }

    // Fallback: WalkBuilder traversal (cold start / index not yet ready).
    let overrides = {
        let mut ob = OverrideBuilder::new(root);
        for pat in &query.excludes {
            let _ = ob.add(&format!("!{pat}"));
        }
        for pat in &query.includes {
            let _ = ob.add(pat);
        }
        ob.build().ok()
    };

    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(!query.include_ignored)
        .git_ignore(!query.include_ignored);
    if let Some(ov) = overrides {
        walker.overrides(ov);
    }

    for entry in walker.build().filter_map(|e| e.ok()) {
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if let Some(result) = search_one(&faber_query, path, open_buffers) {
            total_matches += result.hits.len();
            let keep_going = on_batch(result);
            if !keep_going || total_matches >= MATCH_LIMIT {
                limit_reached = total_matches >= MATCH_LIMIT;
                break;
            }
        }
    }

    limit_reached
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_file(dir: &TempDir, name: &str, content: &str) -> PathBuf {
        let path = dir.path().join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    fn no_bufs() -> HashMap<PathBuf, String> {
        HashMap::new()
    }

    #[test]
    fn finds_matches_across_files() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "a.rs", "fn hello() {}\nfn world() {}");
        write_file(&dir, "b.rs", "let hello = 1;\n");
        write_file(&dir, "c.rs", "no match here\n");

        let q = ProjectSearchQuery::new("hello");
        let mut results = Vec::new();
        run(&q, dir.path(), None, &no_bufs(), |r| {
            results.push(r);
            true
        });
        results.sort_by_key(|r| r.path.clone());
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].hits.len(), 1); // a.rs line 0
        assert_eq!(results[1].hits.len(), 1); // b.rs line 0
    }

    #[test]
    fn case_sensitive_filter() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "a.rs", "Hello hello HELLO");

        let mut q = ProjectSearchQuery::new("hello");
        q.case_sensitive = true;
        let mut results = Vec::new();
        run(&q, dir.path(), None, &no_bufs(), |r| {
            results.push(r);
            true
        });
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].hits[0].ranges.len(), 1);
        assert_eq!(results[0].hits[0].col, 6); // "hello" at char 6
    }

    #[test]
    fn skips_binary_files() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("binary.bin");
        std::fs::write(&path, b"\x00\x01\x02hello\x00").unwrap();

        let q = ProjectSearchQuery::new("hello");
        let mut results = Vec::new();
        run(&q, dir.path(), None, &no_bufs(), |r| {
            results.push(r);
            true
        });
        assert!(results.is_empty());
    }

    #[test]
    fn scope_paths_open_files_only() {
        let dir = TempDir::new().unwrap();
        let p1 = write_file(&dir, "a.rs", "hello world");
        write_file(&dir, "b.rs", "hello there");

        let mut q = ProjectSearchQuery::new("hello");
        q.scope_paths = Some(vec![p1.clone()]);
        let mut results = Vec::new();
        run(&q, dir.path(), None, &no_bufs(), |r| {
            results.push(r);
            true
        });
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, p1);
    }

    #[test]
    fn parse_globs_splits_on_comma() {
        let globs = ProjectSearchQuery::parse_globs("*.rs, *.toml, ");
        assert_eq!(globs, ["*.rs", "*.toml"]);
    }

    #[test]
    fn hit_ranges_are_local_to_line() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "a.rs", "abc\nhello world hello\nxyz");

        let q = ProjectSearchQuery::new("hello");
        let mut results = Vec::new();
        run(&q, dir.path(), None, &no_bufs(), |r| {
            results.push(r);
            true
        });
        assert_eq!(results.len(), 1);
        let hit = &results[0].hits[0];
        assert_eq!(hit.line, 1);
        assert_eq!(hit.col, 0); // first match starts at char 0 of preview
        // Two "hello" matches on the same line.
        assert_eq!(hit.ranges.len(), 2);
    }

    #[test]
    fn candidates_list_skips_unlisted_files() {
        let dir = TempDir::new().unwrap();
        let p1 = write_file(&dir, "a.rs", "hello world");
        write_file(&dir, "b.rs", "hello there");

        let q = ProjectSearchQuery::new("hello");
        let mut results = Vec::new();
        run(
            &q,
            dir.path(),
            Some(std::slice::from_ref(&p1)),
            &no_bufs(),
            |r| {
                results.push(r);
                true
            },
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, p1);
    }

    #[test]
    fn open_buffer_overrides_disk() {
        let dir = TempDir::new().unwrap();
        let p1 = write_file(&dir, "a.rs", "no match on disk");
        let mut bufs = HashMap::new();
        bufs.insert(p1.clone(), "hello in memory".to_string());

        let q = ProjectSearchQuery::new("hello");
        let mut results = Vec::new();
        run(
            &q,
            dir.path(),
            Some(std::slice::from_ref(&p1)),
            &bufs,
            |r| {
                results.push(r);
                true
            },
        );
        assert_eq!(
            results.len(),
            1,
            "buffer text should match despite disk mismatch"
        );
        assert_eq!(results[0].path, p1);
    }

    #[test]
    fn filter_candidates_include_glob() {
        let dir = TempDir::new().unwrap();
        let paths = vec![
            dir.path().join("src/lib.rs"),
            dir.path().join("src/main.rs"),
            dir.path().join("Cargo.toml"),
        ];
        let mut q = ProjectSearchQuery::new("x");
        q.includes = vec!["*.rs".to_string()];
        let filtered = filter_candidates(&paths, dir.path(), &q);
        assert_eq!(filtered.len(), 2);
        assert!(
            filtered
                .iter()
                .all(|p| p.extension().is_some_and(|e| e == "rs"))
        );
    }

    #[test]
    fn filter_candidates_exclude_glob() {
        let dir = TempDir::new().unwrap();
        let paths = vec![dir.path().join("src/lib.rs"), dir.path().join("Cargo.lock")];
        let mut q = ProjectSearchQuery::new("x");
        q.excludes = vec!["*.lock".to_string()];
        let filtered = filter_candidates(&paths, dir.path(), &q);
        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].to_string_lossy().ends_with("lib.rs"));
    }
}
