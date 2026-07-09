use std::ops::Range;
use std::sync::Arc;

use anyhow::Result;
use faber_lang::{LanguageRegistry, OutlineCache, OutlineItem};
use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

use crate::module::{FileInput, FileMeta, IndexModule, InputNeeds, KeySuffix};

// Key separator in LMDB: `{rel_path}\0{suffix}`.
const KEY_SEP: u8 = 0;

/// A fuzzy-matched project symbol from the LMDB index.
#[derive(Debug, Clone)]
pub struct SymbolMatch {
    /// UTF-8 relative path from the project root.
    pub rel_path: String,
    pub name: String,
    pub source_line: usize,
    pub byte_range: Range<usize>,
    /// Nucleo score (higher = better match); 0 for empty-query listings.
    pub score: u32,
    /// Char positions in `name` where the query matched (for highlighting).
    pub positions: Vec<u32>,
}

/// Scan the symbol LMDB index and return up to `limit` fuzzy matches for
/// `query`. Empty query returns the first `limit` symbols in path order.
///
/// Pure function of `store` + `query` — no global state, table-testable.
pub fn project_symbols(
    store: &crate::store::IndexStore,
    query: &str,
    limit: usize,
) -> Result<Vec<SymbolMatch>> {
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = if query.is_empty() {
        None
    } else {
        Some(Pattern::parse(
            query,
            CaseMatching::Ignore,
            Normalization::Smart,
        ))
    };

    let mut scored: Vec<SymbolMatch> = Vec::new();

    for item in store.iter_data("symbols")? {
        let (key, value) = item?;

        // Key format: `{rel_path}\0outline` — extract rel_path bytes.
        let Some(sep) = key.iter().rposition(|&b| b == KEY_SEP) else {
            continue;
        };
        let rel_bytes = &key[..sep];
        let rel_path = String::from_utf8_lossy(rel_bytes).into_owned();

        let items: Vec<OutlineItem> = match bincode::deserialize(&value) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let mut buf = Vec::new();
        for item in items {
            if scored.len() >= limit * 8 {
                // Rough early cap to bound memory; a final sort+truncate follows.
                break;
            }
            if let Some(pat) = &pattern {
                let Some(score) = pat.score(Utf32Str::new(&item.name, &mut buf), &mut matcher)
                else {
                    continue;
                };
                scored.push(SymbolMatch {
                    rel_path: rel_path.clone(),
                    name: item.name,
                    source_line: item.source_line,
                    byte_range: item.byte_range,
                    score,
                    positions: Vec::new(),
                });
            } else {
                // No query: collect in store order (bounded by limit).
                if scored.len() < limit {
                    scored.push(SymbolMatch {
                        rel_path: rel_path.clone(),
                        name: item.name,
                        source_line: item.source_line,
                        byte_range: item.byte_range,
                        score: 0,
                        positions: Vec::new(),
                    });
                }
            }
        }
    }

    if let Some(pat) = pattern.as_ref() {
        scored.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.name.cmp(&b.name)));
        scored.truncate(limit);

        // Pass 2: compute match positions for survivors.
        let mut pos_buf = Vec::new();
        let mut char_buf = Vec::new();
        for sym in &mut scored {
            pos_buf.clear();
            if pat
                .indices(
                    Utf32Str::new(&sym.name, &mut char_buf),
                    &mut matcher,
                    &mut pos_buf,
                )
                .is_some()
            {
                sym.positions = pos_buf.clone();
            }
        }
    }

    Ok(scored)
}

pub struct SymbolsModule {
    registry: Arc<LanguageRegistry>,
}

impl SymbolsModule {
    pub fn new(registry: Arc<LanguageRegistry>) -> Self {
        Self { registry }
    }
}

pub struct SymbolsSnapshot {
    pub generation: u64,
}

impl IndexModule for SymbolsModule {
    type Snapshot = SymbolsSnapshot;

    fn name(&self) -> &'static str {
        "symbols"
    }

    fn version(&self) -> u32 {
        1
    }

    fn needs(&self) -> InputNeeds {
        InputNeeds::TEXT | InputNeeds::SYNTAX
    }

    fn accepts(&self, meta: &FileMeta) -> bool {
        let Some(lang_id) = &meta.language else {
            return false;
        };
        let Some(lang) = self.registry.language_by_id(lang_id) else {
            return false;
        };
        lang.build_grammar().outline.is_some()
    }

    fn index(&self, input: &FileInput) -> Result<Vec<(KeySuffix, Vec<u8>)>> {
        let Some(lang_id) = &input.meta.language else {
            return Ok(vec![]);
        };
        let Some(lang) = self.registry.language_by_id(lang_id) else {
            return Ok(vec![]);
        };
        let Some(tree) = input.syntax else {
            return Ok(vec![]);
        };
        let Some(text) = input.text else {
            return Ok(vec![]);
        };

        let grammar = Arc::new(lang.build_grammar());
        let mut cache = OutlineCache::default();
        cache.setup(Some(&grammar));
        let outline = cache.compute(tree, text);

        let encoded = bincode::serialize(&outline.items)?;
        Ok(vec![(b"outline".to_vec(), encoded)])
    }

    fn publish(
        &self,
        _entries: &mut dyn Iterator<Item = (&[u8], &[u8])>,
    ) -> Result<SymbolsSnapshot> {
        Ok(SymbolsSnapshot { generation: 0 })
    }
}

/// Read outline items for a specific file from the store.
/// The key in the data DB is `{rel_path}\0outline` (engine-composed key).
pub fn symbols_for(
    store: &crate::store::IndexStore,
    rel_path: &[u8],
) -> Result<Option<Vec<OutlineItem>>> {
    let mut key = rel_path.to_vec();
    key.push(0);
    key.extend_from_slice(b"outline");

    match store.get_data("symbols", &key)? {
        None => Ok(None),
        Some(bytes) => {
            let items: Vec<OutlineItem> = bincode::deserialize(&bytes)?;
            Ok(Some(items))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::SystemTime;

    use faber_lang::{LanguageId, LanguageRegistry, OutlineItem};

    use crate::module::{FileInput, FileMeta, IndexModule, InputNeeds};

    use super::SymbolsModule;

    fn meta_for(lang: Option<LanguageId>) -> FileMeta {
        FileMeta {
            rel_path: Arc::from(b"src/main.rs".as_slice()),
            size: 0,
            mtime: SystemTime::UNIX_EPOCH,
            is_ignored: false,
            language: lang,
        }
    }

    #[test]
    fn no_syntax_tree_returns_empty() {
        let registry = Arc::new(LanguageRegistry::with_defaults());
        let module = SymbolsModule::new(registry);
        let meta = meta_for(Some(LanguageId::new("rust")));
        let input = FileInput {
            meta: &meta,
            text: Some("fn main() {}"),
            syntax: None,
        };
        let result = module.index(&input).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn accepts_rust_rejects_unknown() {
        let registry = Arc::new(LanguageRegistry::with_defaults());
        let module = SymbolsModule::new(registry);

        let rust_meta = meta_for(Some(LanguageId::new("rust")));
        assert!(module.accepts(&rust_meta), "Rust has an outline query");

        let unknown_meta = meta_for(Some(LanguageId::new("cobol")));
        assert!(
            !module.accepts(&unknown_meta),
            "unknown language not accepted"
        );

        let no_lang_meta = meta_for(None);
        assert!(!module.accepts(&no_lang_meta), "None language not accepted");
    }

    #[test]
    fn index_rust_source_yields_outline_items() {
        let registry = Arc::new(LanguageRegistry::with_defaults());
        let module = SymbolsModule::new(registry.clone());

        let source = r#"
pub fn greet(name: &str) -> String {
    format!("hello {name}")
}

pub struct Config {
    pub debug: bool,
}
"#;

        let lang = registry
            .language_by_id(&LanguageId::new("rust"))
            .expect("rust registered");
        let mut parser = lang.make_parser();
        let tree = parser.parse(source, None).expect("parse succeeded");

        let meta = meta_for(Some(LanguageId::new("rust")));
        let input = FileInput {
            meta: &meta,
            text: Some(source),
            syntax: Some(&tree),
        };

        let result = module.index(&input).unwrap();
        assert_eq!(result.len(), 1, "one entry per file");
        assert_eq!(result[0].0, b"outline");

        let items: Vec<faber_lang::OutlineItem> =
            bincode::deserialize(&result[0].1).expect("deserializes");
        assert!(!items.is_empty(), "expected at least one outline item");

        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert!(
            names.contains(&"greet"),
            "expected fn greet in outline, got {names:?}"
        );
        assert!(
            names.contains(&"Config"),
            "expected struct Config in outline, got {names:?}"
        );
    }

    #[test]
    fn needs_text_and_syntax() {
        let registry = Arc::new(LanguageRegistry::with_defaults());
        let module = SymbolsModule::new(registry);
        assert_eq!(module.needs(), InputNeeds::TEXT | InputNeeds::SYNTAX);
    }

    // ── project_symbols tests ─────────────────────────────────────────────────

    use crate::store::{IndexStore, Stamp};
    use crate::test_util::with_home;

    fn write_outline(store: &IndexStore, rel_path: &[u8], items: &[OutlineItem]) {
        let stamp = Stamp {
            mtime: SystemTime::UNIX_EPOCH,
            size: 0,
            hash: None,
        };
        let encoded = bincode::serialize(items).unwrap();
        store
            .write_batch(
                "symbols",
                &[(
                    rel_path.to_vec(),
                    stamp,
                    vec![(b"outline".to_vec(), encoded)],
                )],
                false,
            )
            .unwrap();
    }

    fn make_item(name: &str, line: usize) -> OutlineItem {
        OutlineItem {
            depth: 0,
            name: name.to_string(),
            context: None,
            source_line: line,
            end_line: line,
            byte_range: 0..1,
            block_ix: None,
        }
    }

    #[test]
    fn project_symbols_empty_query_returns_all() {
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        with_home(home.path(), || {
            let store = IndexStore::open(project.path()).unwrap();
            write_outline(
                &store,
                b"src/lib.rs",
                &[make_item("foo", 1), make_item("bar", 5)],
            );
            write_outline(&store, b"src/main.rs", &[make_item("main", 0)]);

            let results = super::project_symbols(&store, "", 10).unwrap();
            let names: Vec<&str> = results.iter().map(|s| s.name.as_str()).collect();
            assert!(names.contains(&"foo"), "expected foo; got {names:?}");
            assert!(names.contains(&"bar"), "expected bar; got {names:?}");
            assert!(names.contains(&"main"), "expected main; got {names:?}");
        });
    }

    #[test]
    fn project_symbols_fuzzy_filters_by_name() {
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        with_home(home.path(), || {
            let store = IndexStore::open(project.path()).unwrap();
            write_outline(
                &store,
                b"src/lib.rs",
                &[
                    make_item("parse_header", 1),
                    make_item("format_row", 10),
                    make_item("build_index", 20),
                ],
            );

            let results = super::project_symbols(&store, "parse", 10).unwrap();
            assert!(!results.is_empty(), "expected at least one result");
            assert_eq!(results[0].name, "parse_header");
            assert_eq!(results[0].rel_path, "src/lib.rs");
        });
    }

    #[test]
    fn project_symbols_respects_limit() {
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        with_home(home.path(), || {
            let store = IndexStore::open(project.path()).unwrap();
            let items: Vec<OutlineItem> =
                (0..20).map(|i| make_item(&format!("fn_{i}"), i)).collect();
            write_outline(&store, b"src/lib.rs", &items);

            let results = super::project_symbols(&store, "", 5).unwrap();
            assert!(results.len() <= 5, "limit not respected: {}", results.len());
        });
    }
}
