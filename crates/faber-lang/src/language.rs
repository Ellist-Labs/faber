use std::{ops::Range, sync::Arc};

use tree_sitter::{Language as TsLanguage, Parser, Query, QueryCursor, StreamingIterator, Tree};

/// Opaque identifier for a language (e.g. `"rust"`, `"python"`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LanguageId(pub String);

impl LanguageId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
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
///
/// Global fallback shared by all languages. Language-specific overrides live in
/// each `Language`'s `token_map`; this function covers the common tokens plus a
/// few widely-used extended captures (e.g. `text.title` from markdown).
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
        "punctuation" | "punctuation.bracket" | "punctuation.delimiter" => SyntaxToken::Punctuation,
        "variable" | "variable.parameter" | "variable.builtin" => SyntaxToken::Variable,
        "property" | "field" => SyntaxToken::Property,
        "attribute" => SyntaxToken::Attribute,
        "namespace" | "module" => SyntaxToken::Namespace,
        "tag" | "tag.builtin" => SyntaxToken::Tag,
        "label" => SyntaxToken::Label,
        _ => return None,
    })
}

/// Language-specific capture-name → token override table.
type TokenMapFn = fn() -> &'static [(&'static str, SyntaxToken)];

/// Compiled highlight query + capture-index mapping. Built once by `Language::build_grammar`.
pub struct HighlightConfig {
    pub query: Query,
    pub cap_tokens: Vec<Option<SyntaxToken>>,
}

/// Compiled outline query + per-capture indices. Built once by `Language::build_grammar`.
pub struct OutlineConfig {
    pub query: Query,
    /// Capture index for the whole-item node (`@item`).
    pub item_ix: u32,
    /// Capture index for the display name node (`@name`).
    pub name_ix: u32,
    /// Capture index for the dimmed keyword prefix node (`@context`), if present.
    pub context_ix: Option<u32>,
}

/// A single item in a document's symbol outline.
///
/// Used for both code files (tree-sitter, via `OutlineCache`) and markdown
/// (pulldown-cmark, via `parse_markdown`). Features like breadcrumbs, the
/// outline overlay, symbol search, and folding all consume this unified type.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OutlineItem {
    /// Nesting depth from the containment stack (0 = top-level).
    pub depth: u32,
    /// Display name (identifier / heading text).
    pub name: String,
    /// Dimmed keyword prefix shown before the name in the overlay
    /// (e.g. `"fn"`, `"struct"`, `"impl"`, `"##"` for markdown H2).
    /// `None` for items where no prefix is appropriate.
    pub context: Option<String>,
    /// Rope line index of the item's first line — used for scroll / cursor nav.
    pub source_line: usize,
    /// Rope line index of the item's last line (inclusive).
    /// For code items this is the closing brace/end of the node body.
    /// For markdown headings this equals `source_line` (section extent is
    /// computed separately by the overlay).
    pub end_line: usize,
    /// Byte range of the whole item node — used for future features (folding,
    /// structural selection). Drive highlight from `source_line`/`end_line`.
    pub byte_range: Range<usize>,
    /// For markdown items only: index into `MarkdownDoc::blocks` for preview
    /// scroll sync. `None` for code items.
    pub block_ix: Option<usize>,
}

/// The full outline for a document — a flat, depth-ordered list of `OutlineItem`s.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Outline {
    pub items: Vec<OutlineItem>,
}

impl Outline {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Computes an `Outline` from a tree-sitter syntax tree using the language's
/// outline query. Mirrors `HighlightCache` in structure and cost profile.
#[derive(Default)]
pub struct OutlineCache {
    grammar: Option<Arc<Grammar>>,
}

impl OutlineCache {
    /// Attach the compiled grammar. Call once after the document is opened or
    /// the language is assigned; cheap thereafter (Arc clone).
    pub fn setup(&mut self, grammar: Option<&Arc<Grammar>>) {
        self.grammar = grammar.cloned();
    }

    /// Run the outline query over the current syntax tree and source.
    /// O(node_count) — called once per edit in `Document::apply`, not per frame.
    /// Returns an empty `Outline` when no outline query is configured.
    pub fn compute(&self, tree: &Tree, source: &str) -> Outline {
        let Some(ref g) = self.grammar else {
            return Outline::default();
        };
        let Some(ref config) = g.outline else {
            return Outline::default();
        };

        let src_bytes = source.as_bytes();
        let root = tree.root_node();

        // Collect raw (byte_range, start_line, end_line, name, context) tuples via query matches.
        // Each match groups @item + @name + optional @context from one pattern.
        type RawItem = (Range<usize>, usize, usize, String, Option<String>);
        let mut raw: Vec<RawItem> = Vec::new();

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&config.query, root, src_bytes);

        matches.advance();
        while let Some(mat) = matches.get() {
            let item_node = mat
                .captures
                .iter()
                .find(|c| c.index == config.item_ix)
                .map(|c| c.node);
            let name_node = mat
                .captures
                .iter()
                .find(|c| c.index == config.name_ix)
                .map(|c| c.node);
            let ctx_node = config
                .context_ix
                .and_then(|ix| mat.captures.iter().find(|c| c.index == ix).map(|c| c.node));

            if let (Some(item), Some(name)) = (item_node, name_node) {
                let start = item.start_byte();
                let end = item.end_byte();
                let start_line = item.start_position().row;
                let end_line = item.end_position().row;
                let name_text = source
                    .get(name.start_byte()..name.end_byte())
                    .unwrap_or("")
                    .to_string();
                let ctx_text = ctx_node
                    .and_then(|n| source.get(n.start_byte()..n.end_byte()).map(str::to_string));
                raw.push((start..end, start_line, end_line, name_text, ctx_text));
            }

            matches.advance();
        }

        // Sort by (start_byte asc, end_byte desc) so parent ranges come before children
        // when two items share the same start (e.g. mod keyword inside mod_item).
        raw.sort_by(|a, b| a.0.start.cmp(&b.0.start).then(b.0.end.cmp(&a.0.end)));

        // Compute depth via a containment stack of end-byte values.
        // Pop any item whose range ends at or before the current item's start —
        // it is a sibling or ancestor, not a parent.
        let mut stack_ends: Vec<usize> = Vec::new();
        let mut items: Vec<OutlineItem> = Vec::with_capacity(raw.len());

        for (range, source_line, end_line, name, context) in raw {
            while stack_ends.last().is_some_and(|&end| end <= range.start) {
                stack_ends.pop();
            }
            let depth = stack_ends.len() as u32;
            stack_ends.push(range.end);
            items.push(OutlineItem {
                depth,
                name,
                context,
                source_line,
                end_line,
                byte_range: range,
                block_ix: None,
            });
        }

        Outline { items }
    }
}

/// All compiled tree-sitter queries for a language, built once and shared via `Arc`.
/// Adding a new language feature = add one field here + one `make_*` method on `Language`.
pub struct Grammar {
    pub highlight: Option<HighlightConfig>,
    pub outline: Option<OutlineConfig>,
}

/// A supported language: its id, file extensions, and how to build a parser.
pub struct Language {
    pub id: LanguageId,
    /// Human-readable display name (e.g. `"Rust"`, `"Markdown"`).
    pub name: String,
    /// Lowercase file extensions without the leading dot (e.g. `["rs"]`).
    pub extensions: Vec<String>,
    /// Returns the tree-sitter grammar for this language.
    grammar: fn() -> TsLanguage,
    /// Returns the highlights query source for this language (optional).
    pub(crate) highlights_query: Option<fn() -> &'static str>,
    /// Returns the outline query source for this language (optional).
    outline_query: Option<fn() -> &'static str>,
    /// Language-specific capture-name → token overrides, consulted before the
    /// global `capture_name_to_token` fallback. `None` = use fallback only.
    pub(crate) token_map: Option<TokenMapFn>,
}

impl Language {
    pub fn new(
        id: impl Into<String>,
        extensions: impl IntoIterator<Item = impl Into<String>>,
        grammar: fn() -> TsLanguage,
    ) -> Self {
        let id_str: String = id.into();
        let name = {
            let mut c = id_str.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        };
        Self {
            id: LanguageId::new(id_str),
            name,
            extensions: extensions.into_iter().map(Into::into).collect(),
            grammar,
            highlights_query: None,
            outline_query: None,
            token_map: None,
        }
    }

    /// Attach a highlights query source to this language definition.
    pub fn with_highlights(mut self, query_fn: fn() -> &'static str) -> Self {
        self.highlights_query = Some(query_fn);
        self
    }

    /// Attach an outline query source (`.scm`) to this language definition.
    pub fn with_outline(mut self, query_fn: fn() -> &'static str) -> Self {
        self.outline_query = Some(query_fn);
        self
    }

    /// Attach a language-specific capture-name → token override table.
    pub fn with_token_map(mut self, map_fn: TokenMapFn) -> Self {
        self.token_map = Some(map_fn);
        self
    }

    /// Override the display name (default: id with first letter capitalized).
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Compile all tree-sitter queries into a `Grammar` ready for caching.
    /// Call once at document-open time; store via `Arc` and share across caches.
    pub fn build_grammar(&self) -> Grammar {
        Grammar {
            highlight: self.make_highlight_config(),
            outline: self.make_outline_query(),
        }
    }

    fn make_highlight_config(&self) -> Option<HighlightConfig> {
        let (query, cap_tokens) = self.make_highlight_query()?;
        Some(HighlightConfig { query, cap_tokens })
    }

    /// Resolve a capture name to a token: language-specific `token_map` first,
    /// then the global `capture_name_to_token` fallback.
    fn resolve_capture(&self, name: &str) -> Option<SyntaxToken> {
        if let Some(map_fn) = self.token_map
            && let Some((_, tok)) = map_fn().iter().find(|(n, _)| *n == name)
        {
            return Some(*tok);
        }
        capture_name_to_token(name)
    }

    /// Build a tree-sitter parser configured for this language.
    pub fn make_parser(&self) -> Parser {
        let mut p = Parser::new();
        p.set_language(&(self.grammar)())
            .expect("failed to set grammar");
        p
    }

    /// Build a `tree_sitter::Query` + capture-index→`SyntaxToken` mapping.
    /// Returns `None` if no highlights query is configured or the query fails to compile.
    pub fn make_highlight_query(&self) -> Option<(Query, Vec<Option<SyntaxToken>>)> {
        let q_src = (self.highlights_query?)();
        let ts_lang: TsLanguage = (self.grammar)();
        let query = Query::new(&ts_lang, q_src).ok()?;
        let cap_tokens: Vec<Option<SyntaxToken>> = query
            .capture_names()
            .iter()
            .map(|n| self.resolve_capture(n))
            .collect();
        Some((query, cap_tokens))
    }

    /// Build an `OutlineConfig` from the language's outline query source.
    /// Returns `None` if no outline query is configured or the query fails to compile.
    pub fn make_outline_query(&self) -> Option<OutlineConfig> {
        let q_src = (self.outline_query?)();
        let ts_lang: TsLanguage = (self.grammar)();
        let query = Query::new(&ts_lang, q_src).ok()?;
        let names = query.capture_names();
        let index_of =
            |name: &str| -> Option<u32> { names.iter().position(|n| *n == name).map(|i| i as u32) };
        let item_ix = index_of("item")?;
        let name_ix = index_of("name")?;
        let context_ix = index_of("context");
        Some(OutlineConfig {
            query,
            item_ix,
            name_ix,
            context_ix,
        })
    }
}

/// Markdown-specific capture-name → token overrides (tree-sitter-md block grammar).
fn markdown_token_map() -> &'static [(&'static str, SyntaxToken)] {
    &[
        ("text.title", SyntaxToken::Keyword),
        ("text.literal", SyntaxToken::String),
        ("text.uri", SyntaxToken::Constant),
        ("text.reference", SyntaxToken::Label),
        ("string.escape", SyntaxToken::String),
    ]
}

/// Built-in Markdown language definition (block grammar).
pub fn markdown() -> Language {
    Language::new("markdown", ["md", "markdown"], || {
        tree_sitter_md::LANGUAGE.into()
    })
    .with_highlights(|| tree_sitter_md::HIGHLIGHT_QUERY_BLOCK)
    .with_token_map(markdown_token_map)
    .with_name("Markdown")
}

/// Built-in Rust language definition.
pub fn rust() -> Language {
    Language::new("rust", ["rs"], || tree_sitter_rust::LANGUAGE.into())
        .with_highlights(|| tree_sitter_rust::HIGHLIGHTS_QUERY)
        .with_outline(|| include_str!("../queries/rust/outline.scm"))
        .with_name("Rust")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_capture_names_map_to_tokens() {
        assert_eq!(capture_name_to_token("keyword"), Some(SyntaxToken::Keyword));
        assert_eq!(capture_name_to_token("comment"), Some(SyntaxToken::Comment));
        assert_eq!(capture_name_to_token("string"), Some(SyntaxToken::String));
        assert_eq!(capture_name_to_token("unknown_xyz_abc"), None);
    }

    #[test]
    fn text_title_maps_via_global_fallback() {
        // text.title is a markdown capture retained in the global fallback.
        assert_eq!(
            capture_name_to_token("text.title"),
            Some(SyntaxToken::Keyword)
        );
    }

    #[test]
    fn token_map_overrides_global_fallback() {
        fn map() -> &'static [(&'static str, SyntaxToken)] {
            &[("keyword", SyntaxToken::Type)]
        }
        let lang = rust().with_token_map(map);
        // token_map wins over the global fallback for the same capture name.
        assert_eq!(lang.resolve_capture("keyword"), Some(SyntaxToken::Type));
        // Names absent from token_map fall through to the global fallback.
        assert_eq!(lang.resolve_capture("comment"), Some(SyntaxToken::Comment));
    }

    #[test]
    fn rust_outline_query_compiles_and_has_captures() {
        let config = rust()
            .make_outline_query()
            .expect("Rust outline query should compile");
        // All mandatory captures must resolve.
        assert!(config.item_ix < u32::MAX, "item capture must exist");
        assert!(config.name_ix < u32::MAX, "name capture must exist");
        assert!(
            config.context_ix.is_some(),
            "context capture expected for Rust"
        );
    }

    #[test]
    fn grammar_bundles_both_configs() {
        let grammar = rust().build_grammar();
        assert!(grammar.highlight.is_some(), "highlight config expected");
        assert!(grammar.outline.is_some(), "outline config expected");
    }
}
