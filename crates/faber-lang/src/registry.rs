use std::{path::Path, sync::Arc};

use crate::language::{Language, LanguageId};

/// Maps file paths / extensions to `Language` definitions.
///
/// Usage: call `register` once at startup for each supported language.
/// `Document::open` then calls `language_for_path` to pick the grammar.
pub struct LanguageRegistry {
    languages: Vec<Arc<Language>>,
}

impl LanguageRegistry {
    pub fn new() -> Self {
        Self {
            languages: Vec::new(),
        }
    }

    /// Bootstrap registry with all built-in languages.
    /// To add a language: call `register()` after construction, or extend this function.
    /// The WASM extension host will call `register()` per plugin at runtime.
    pub fn with_defaults() -> Self {
        let mut r = Self::new();
        r.register(crate::language::rust());
        r.register(crate::language::markdown());
        r
    }

    /// Primary extension point: add a language definition to the registry.
    /// Built-in languages, and (in future) each WASM plugin, funnel through here.
    pub fn register(&mut self, lang: Language) {
        self.languages.push(Arc::new(lang));
    }

    /// Resolve the language for a file path by matching the lowercase extension.
    pub fn language_for_path(&self, path: &Path) -> Option<Arc<Language>> {
        let ext = path.extension()?.to_string_lossy().to_lowercase();
        self.languages
            .iter()
            .find(|l| l.extensions.iter().any(|e| e == &ext))
            .cloned()
    }

    /// Look up a language by its id.
    pub fn language_by_id(&self, id: &LanguageId) -> Option<Arc<Language>> {
        self.languages.iter().find(|l| &l.id == id).cloned()
    }

    /// Enumerate all registered languages (for pickers and dropdowns).
    pub fn languages(&self) -> &[Arc<Language>] {
        &self.languages
    }
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn resolves_rust_by_extension() {
        let reg = LanguageRegistry::with_defaults();
        let lang = reg.language_for_path(&PathBuf::from("main.rs")).unwrap();
        assert_eq!(lang.id.0, "rust");
    }

    #[test]
    fn unknown_extension_returns_none() {
        let reg = LanguageRegistry::with_defaults();
        assert!(reg.language_for_path(&PathBuf::from("file.xyz")).is_none());
    }

    #[test]
    fn lookup_by_id() {
        let reg = LanguageRegistry::with_defaults();
        let id = LanguageId::new("rust");
        assert!(reg.language_by_id(&id).is_some());
    }

    #[test]
    fn resolves_markdown_by_extension() {
        let reg = LanguageRegistry::with_defaults();
        let lang = reg.language_for_path(&PathBuf::from("README.md")).unwrap();
        assert_eq!(lang.id.0, "markdown");
        let lang2 = reg
            .language_for_path(&PathBuf::from("notes.markdown"))
            .unwrap();
        assert_eq!(lang2.id.0, "markdown");
    }

    #[test]
    fn markdown_highlight_query_compiles() {
        let reg = LanguageRegistry::with_defaults();
        let lang = reg.language_for_path(&PathBuf::from("README.md")).unwrap();
        assert!(lang.make_highlight_query().is_some());
    }
}
