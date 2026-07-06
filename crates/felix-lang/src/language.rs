use tree_sitter::{Language as TsLanguage, Parser};

/// Opaque identifier for a language (e.g. `"rust"`, `"python"`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LanguageId(pub String);

impl LanguageId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for LanguageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A supported language: its id, file extensions, and how to build a parser.
pub struct Language {
    pub id: LanguageId,
    /// Lowercase file extensions without the leading dot (e.g. `["rs"]`).
    pub extensions: Vec<String>,
    /// Returns the tree-sitter grammar for this language.
    grammar: fn() -> TsLanguage,
}

impl Language {
    pub fn new(
        id: impl Into<String>,
        extensions: impl IntoIterator<Item = impl Into<String>>,
        grammar: fn() -> TsLanguage,
    ) -> Self {
        Self {
            id: LanguageId::new(id),
            extensions: extensions.into_iter().map(Into::into).collect(),
            grammar,
        }
    }

    /// Build a tree-sitter parser configured for this language.
    pub fn make_parser(&self) -> Parser {
        let mut p = Parser::new();
        p.set_language(&(self.grammar)()).expect("failed to set grammar");
        p
    }
}

/// Built-in Rust language definition.
pub fn rust() -> Language {
    Language::new("rust", ["rs"], || tree_sitter_rust::LANGUAGE.into())
}
