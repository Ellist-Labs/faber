use tree_sitter::{Language as TsLanguage, Parser, Query};
use tree_sitter_md;

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

/// Syntax token categories for highlight mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SyntaxToken {
    Keyword = 0,
    Function,
    Type,
    String,
    Number,
    Comment,
    Constant,
    Operator,
    Punctuation,
    Variable,
    Property,
    Attribute,
    Namespace,
    Tag,
    Label,
}

/// Maps a tree-sitter capture name to a `SyntaxToken`.
pub fn capture_name_to_token(name: &str) -> Option<SyntaxToken> {
    Some(match name {
        // markdown-specific captures (tree-sitter-md block grammar)
        "text.title" => SyntaxToken::Keyword,
        "text.literal" => SyntaxToken::String,
        "text.uri" => SyntaxToken::Constant,
        "text.reference" => SyntaxToken::Label,
        "string.escape" => SyntaxToken::String,
        "keyword" | "keyword.control" | "keyword.operator" | "keyword.special" => {
            SyntaxToken::Keyword
        }
        "function" | "function.method" | "function.builtin" | "function.macro" => {
            SyntaxToken::Function
        }
        "type" | "type.builtin" | "constructor" => SyntaxToken::Type,
        "string" | "string.special" | "character" | "escape" => SyntaxToken::String,
        "number" | "float" => SyntaxToken::Number,
        "comment" | "comment.documentation" => SyntaxToken::Comment,
        "constant" | "constant.builtin" | "constant.macro" => SyntaxToken::Constant,
        "operator" => SyntaxToken::Operator,
        "punctuation" | "punctuation.bracket" | "punctuation.delimiter" => {
            SyntaxToken::Punctuation
        }
        "variable" | "variable.parameter" | "variable.builtin" => SyntaxToken::Variable,
        "property" | "field" => SyntaxToken::Property,
        "attribute" => SyntaxToken::Attribute,
        "namespace" | "module" => SyntaxToken::Namespace,
        "tag" | "tag.builtin" => SyntaxToken::Tag,
        "label" => SyntaxToken::Label,
        _ => return None,
    })
}

/// A supported language: its id, file extensions, and how to build a parser.
pub struct Language {
    pub id: LanguageId,
    /// Lowercase file extensions without the leading dot (e.g. `["rs"]`).
    pub extensions: Vec<String>,
    /// Returns the tree-sitter grammar for this language.
    grammar: fn() -> TsLanguage,
    /// Returns the highlights query source for this language (optional).
    pub highlights_query: Option<fn() -> &'static str>,
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
            highlights_query: None,
        }
    }

    /// Attach a highlights query source to this language definition.
    pub fn with_highlights(mut self, query_fn: fn() -> &'static str) -> Self {
        self.highlights_query = Some(query_fn);
        self
    }

    /// Build a tree-sitter parser configured for this language.
    pub fn make_parser(&self) -> Parser {
        let mut p = Parser::new();
        p.set_language(&(self.grammar)()).expect("failed to set grammar");
        p
    }

    /// Build a `tree_sitter::Query` + capture-index→`SyntaxToken` mapping.
    /// Returns `None` if no highlights query is configured or the query fails to compile.
    pub fn make_highlight_query(&self) -> Option<(Query, Vec<Option<SyntaxToken>>)> {
        let q_src = (self.highlights_query?)(  );
        let ts_lang: TsLanguage = (self.grammar)();
        let query = Query::new(&ts_lang, q_src).ok()?;
        let cap_tokens: Vec<Option<SyntaxToken>> =
            query.capture_names().iter().map(|n| capture_name_to_token(n)).collect();
        Some((query, cap_tokens))
    }
}

/// Built-in Markdown language definition (block grammar).
pub fn markdown() -> Language {
    Language::new("markdown", ["md", "markdown"], || tree_sitter_md::LANGUAGE.into())
        .with_highlights(|| tree_sitter_md::HIGHLIGHT_QUERY_BLOCK)
}

/// Built-in Rust language definition.
pub fn rust() -> Language {
    Language::new("rust", ["rs"], || tree_sitter_rust::LANGUAGE.into())
        .with_highlights(|| tree_sitter_rust::HIGHLIGHTS_QUERY)
}
