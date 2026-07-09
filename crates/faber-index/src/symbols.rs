use std::sync::Arc;

use anyhow::Result;
use faber_lang::{LanguageRegistry, OutlineCache, OutlineItem};

use crate::module::{FileInput, FileMeta, IndexModule, InputNeeds, KeySuffix};

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

    use faber_lang::{LanguageId, LanguageRegistry};

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
}
