// DiagnosticStore: (Source, Url)-keyed entries, generation counter,
// DiagnosticProvider trait for non-LSP sources.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use faber_core::anchor::Anchor;

// ── Severity ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}

// ── DiagnosticTag ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticTag {
    Unnecessary,
    Deprecated,
}

// ── Source ───────────────────────────────────────────────────────────────────

pub type Source = Arc<str>;

// ── DiagnosticRange ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DiagnosticRange {
    /// 0-based LSP line number — used by the editor to filter squiggles per rendered line.
    pub lsp_line: u32,
    pub start: Anchor,
    pub end: Anchor,
}

// ── DiagnosticEntry ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DiagnosticEntry {
    pub range: DiagnosticRange,
    pub severity: Severity,
    pub tags: Vec<DiagnosticTag>,
    pub message: String,
    pub source: Source,
    pub code: Option<String>,
}

// ── StoreKey ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StoreKey {
    pub source: Source,
    pub uri: url::Url,
}

// ── DiagnosticStore ──────────────────────────────────────────────────────────

struct StoreInner {
    entries: HashMap<StoreKey, Vec<DiagnosticEntry>>,
}

pub struct DiagnosticStore {
    inner: Arc<RwLock<StoreInner>>,
    generation: Arc<AtomicU64>,
}

impl DiagnosticStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(StoreInner {
                entries: HashMap::new(),
            })),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Replace all diagnostics for (source, uri). An empty vec clears the entry.
    pub fn publish(&self, source: Source, uri: url::Url, entries: Vec<DiagnosticEntry>) {
        let key = StoreKey { source, uri };
        {
            let mut inner = self.inner.write().expect("lock poisoned");
            if entries.is_empty() {
                inner.entries.remove(&key);
            } else {
                inner.entries.insert(key, entries);
            }
        }
        // Bump generation after releasing the write lock.
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// All diagnostics for a URI, across all sources, sorted by severity then start offset.
    pub fn get_for_uri(&self, uri: &url::Url) -> Vec<DiagnosticEntry> {
        let inner = self.inner.read().expect("lock poisoned");
        let mut result: Vec<DiagnosticEntry> = inner
            .entries
            .iter()
            .filter(|(key, _)| &key.uri == uri)
            .flat_map(|(_, entries)| entries.iter().cloned())
            .collect();
        result.sort_by(|a, b| {
            a.severity
                .cmp(&b.severity)
                .then_with(|| a.range.start.offset.cmp(&b.range.start.offset))
        });
        result
    }

    /// Generation counter — bumped after every publish. Poll this at 50ms to detect changes.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Total count across all sources and URIs.
    pub fn total_count(&self) -> usize {
        let inner = self.inner.read().expect("lock poisoned");
        inner.entries.values().map(|v| v.len()).sum()
    }

    /// Count by severity across all sources and URIs.
    pub fn count_by_severity(&self, severity: Severity) -> usize {
        let inner = self.inner.read().expect("lock poisoned");
        inner
            .entries
            .values()
            .flat_map(|v| v.iter())
            .filter(|e| e.severity == severity)
            .count()
    }
}

impl Default for DiagnosticStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── DiagnosticProvider trait ─────────────────────────────────────────────────

pub trait DiagnosticProvider: Send + Sync {
    fn source_name(&self) -> Source;
    fn supported_languages(&self) -> &[faber_lang::LanguageId];
}

// ── Conversion helpers (pub, for use by manager.rs) ──────────────────────────

/// Convert an lsp_types::DiagnosticSeverity to our Severity.
pub fn severity_from_lsp(s: Option<lsp_types::DiagnosticSeverity>) -> Severity {
    match s {
        Some(lsp_types::DiagnosticSeverity::ERROR) => Severity::Error,
        Some(lsp_types::DiagnosticSeverity::WARNING) => Severity::Warning,
        Some(lsp_types::DiagnosticSeverity::INFORMATION) => Severity::Information,
        Some(lsp_types::DiagnosticSeverity::HINT) => Severity::Hint,
        // Unknown variant or None defaults to Error (fail-safe for visibility).
        _ => Severity::Error,
    }
}

/// Convert lsp_types::DiagnosticTags to our DiagnosticTag vec.
pub fn tags_from_lsp(tags: Option<&[lsp_types::DiagnosticTag]>) -> Vec<DiagnosticTag> {
    let Some(tags) = tags else {
        return Vec::new();
    };
    tags.iter()
        .filter_map(|t| match *t {
            lsp_types::DiagnosticTag::UNNECESSARY => Some(DiagnosticTag::Unnecessary),
            lsp_types::DiagnosticTag::DEPRECATED => Some(DiagnosticTag::Deprecated),
            _ => None,
        })
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use faber_core::anchor::Bias;

    fn make_entry(severity: Severity, start_offset: usize, source: &str) -> DiagnosticEntry {
        DiagnosticEntry {
            range: DiagnosticRange {
                lsp_line: 0,
                start: Anchor::new(start_offset, Bias::Left),
                end: Anchor::new(start_offset + 1, Bias::Right),
            },
            severity,
            tags: Vec::new(),
            message: format!("msg at {start_offset}"),
            source: Arc::from(source),
            code: None,
        }
    }

    fn test_uri() -> url::Url {
        url::Url::parse("file:///test/file.rs").unwrap()
    }

    #[test]
    fn publish_and_retrieve() {
        let store = DiagnosticStore::new();
        let uri = test_uri();
        let source: Source = Arc::from("lsp");

        let entries = vec![
            make_entry(Severity::Warning, 10, "lsp"),
            make_entry(Severity::Error, 5, "lsp"),
        ];
        store.publish(source, uri.clone(), entries);

        let got = store.get_for_uri(&uri);
        assert_eq!(got.len(), 2);
        // Sorted by severity: Error < Warning
        assert_eq!(got[0].severity, Severity::Error);
        assert_eq!(got[1].severity, Severity::Warning);
    }

    #[test]
    fn clear_on_empty_publish() {
        let store = DiagnosticStore::new();
        let uri = test_uri();
        let source: Source = Arc::from("lsp");

        store.publish(
            source.clone(),
            uri.clone(),
            vec![make_entry(Severity::Error, 0, "lsp")],
        );
        assert_eq!(store.get_for_uri(&uri).len(), 1);

        store.publish(source, uri.clone(), vec![]);
        assert!(store.get_for_uri(&uri).is_empty());
    }

    #[test]
    fn multi_source() {
        let store = DiagnosticStore::new();
        let uri = test_uri();

        store.publish(
            Arc::from("lsp"),
            uri.clone(),
            vec![make_entry(Severity::Error, 0, "lsp")],
        );
        store.publish(
            Arc::from("lint"),
            uri.clone(),
            vec![make_entry(Severity::Warning, 5, "lint")],
        );

        let got = store.get_for_uri(&uri);
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn generation_bump() {
        let store = DiagnosticStore::new();
        let gen_before = store.generation();
        store.publish(
            Arc::from("lsp"),
            test_uri(),
            vec![make_entry(Severity::Error, 0, "lsp")],
        );
        assert!(store.generation() > gen_before);
    }

    #[test]
    fn count_helpers() {
        let store = DiagnosticStore::new();
        let uri = test_uri();

        store.publish(
            Arc::from("lsp"),
            uri.clone(),
            vec![
                make_entry(Severity::Error, 0, "lsp"),
                make_entry(Severity::Error, 1, "lsp"),
                make_entry(Severity::Warning, 2, "lsp"),
            ],
        );

        assert_eq!(store.total_count(), 3);
        assert_eq!(store.count_by_severity(Severity::Error), 2);
        assert_eq!(store.count_by_severity(Severity::Warning), 1);
        assert_eq!(store.count_by_severity(Severity::Hint), 0);
    }

    fn make_entry_msg(message: &str) -> DiagnosticEntry {
        DiagnosticEntry {
            range: DiagnosticRange {
                lsp_line: 0,
                start: Anchor::new(0, Bias::Left),
                end: Anchor::new(1, Bias::Right),
            },
            severity: Severity::Error,
            tags: Vec::new(),
            message: message.to_string(),
            source: Arc::from(message),
            code: None,
        }
    }

    #[test]
    fn replace_on_republish() {
        let store = DiagnosticStore::new();
        let source: Source = Arc::from("lsp");
        let uri: url::Url = "file:///foo.rs".parse().unwrap();

        // First publish: 2 entries.
        store.publish(
            Arc::clone(&source),
            uri.clone(),
            vec![
                make_entry_msg("first error"),
                make_entry_msg("second error"),
            ],
        );
        assert_eq!(store.total_count(), 2);

        // Second publish to same (source, uri): should REPLACE, not append.
        store.publish(
            Arc::clone(&source),
            uri.clone(),
            vec![make_entry_msg("only error")],
        );
        assert_eq!(store.total_count(), 1, "republish must replace, not append");

        let entries = store.get_for_uri(&uri);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "only error");
    }

    #[test]
    fn multi_file_isolation() {
        let store = DiagnosticStore::new();
        let source: Source = Arc::from("lsp");
        let uri_a: url::Url = "file:///a.rs".parse().unwrap();
        let uri_b: url::Url = "file:///b.rs".parse().unwrap();

        store.publish(
            Arc::clone(&source),
            uri_a.clone(),
            vec![make_entry_msg("error in a")],
        );
        store.publish(
            Arc::clone(&source),
            uri_b.clone(),
            vec![make_entry_msg("error in b"), make_entry_msg("warning in b")],
        );

        let for_a = store.get_for_uri(&uri_a);
        assert_eq!(for_a.len(), 1);
        assert_eq!(for_a[0].message, "error in a");

        let for_b = store.get_for_uri(&uri_b);
        assert_eq!(for_b.len(), 2);
    }

    #[test]
    fn severity_from_lsp_defaults() {
        // None (missing severity) → Error
        assert_eq!(severity_from_lsp(None), Severity::Error);

        // All explicit variants
        assert_eq!(
            severity_from_lsp(Some(lsp_types::DiagnosticSeverity::ERROR)),
            Severity::Error
        );
        assert_eq!(
            severity_from_lsp(Some(lsp_types::DiagnosticSeverity::WARNING)),
            Severity::Warning
        );
        assert_eq!(
            severity_from_lsp(Some(lsp_types::DiagnosticSeverity::INFORMATION)),
            Severity::Information
        );
        assert_eq!(
            severity_from_lsp(Some(lsp_types::DiagnosticSeverity::HINT)),
            Severity::Hint
        );
    }

    #[test]
    fn tags_from_lsp_unknown_filtered() {
        use lsp_types::DiagnosticTag as LspTag;

        // None → empty
        assert!(tags_from_lsp(None).is_empty());

        // Known tags preserved
        let known = &[LspTag::UNNECESSARY, LspTag::DEPRECATED];
        let result = tags_from_lsp(Some(known));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], DiagnosticTag::Unnecessary);
        assert_eq!(result[1], DiagnosticTag::Deprecated);

        // Empty slice → empty
        assert!(tags_from_lsp(Some(&[])).is_empty());
    }
}
