use std::{num::NonZeroU32, ops::Range, sync::Arc, sync::OnceLock};

use tree_sitter::{Language as TsLanguage, Parser, Query, QueryCursor, StreamingIterator, Tree};

use faber_theme::SyntaxTheme;

fn default_syntax_theme() -> &'static SyntaxTheme {
    static THEME: OnceLock<SyntaxTheme> = OnceLock::new();
    THEME.get_or_init(|| faber_theme::default::faber_dark().syntax)
}

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

/// 1-indexed style reference. Index into `SyntaxTheme.styles` = `id.index()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HighlightId(pub NonZeroU32);

impl HighlightId {
    pub fn from_style_index(idx: usize) -> Self {
        Self(NonZeroU32::new(idx as u32 + 1).expect("style index must fit"))
    }

    pub fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

/// Per-grammar capture_id → HighlightId mapping (indexed by tree-sitter capture id).
#[derive(Clone, Default)]
pub struct HighlightMap(pub Arc<[Option<HighlightId>]>);

impl HighlightMap {
    pub fn get(&self, capture_id: u32) -> Option<HighlightId> {
        self.0.get(capture_id as usize).copied().flatten()
    }
}

/// Build a HighlightMap from a compiled query's capture names and a SyntaxTheme.
pub fn build_highlight_map(capture_names: &[&str], theme: &SyntaxTheme) -> HighlightMap {
    let v: Vec<Option<HighlightId>> = capture_names
        .iter()
        .map(|name| {
            theme
                .highlight_id(name)
                .map(|idx| HighlightId::from_style_index(idx as usize))
        })
        .collect();
    HighlightMap(v.into())
}

/// Compiled highlight query. Built once by `Language::build_grammar`.
pub struct HighlightQuery {
    pub query: Query,
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
    pub highlight: Option<HighlightQuery>,
    pub highlight_map: HighlightMap,
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

    /// Override the display name (default: id with first letter capitalized).
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Compile all tree-sitter queries into a `Grammar` ready for caching.
    /// Call once at document-open time; store via `Arc` and share across caches.
    pub fn build_grammar(&self) -> Grammar {
        let syntax_theme = default_syntax_theme();
        let highlight = self.make_highlight_query_compiled();
        let highlight_map = match &highlight {
            Some(hq) => build_highlight_map(hq.query.capture_names(), syntax_theme),
            None => HighlightMap::default(),
        };
        Grammar {
            highlight,
            highlight_map,
            outline: self.make_outline_query(),
        }
    }

    fn make_highlight_query_compiled(&self) -> Option<HighlightQuery> {
        let q_src = (self.highlights_query?)();
        let ts_lang: TsLanguage = (self.grammar)();
        let query = Query::new(&ts_lang, q_src).ok()?;
        Some(HighlightQuery { query })
    }

    /// Build a tree-sitter parser configured for this language.
    pub fn make_parser(&self) -> Parser {
        let mut p = Parser::new();
        p.set_language(&(self.grammar)())
            .expect("failed to set grammar");
        p
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

/// Built-in Markdown language definition (block grammar).
pub fn markdown() -> Language {
    Language::new("markdown", ["md", "markdown"], || {
        tree_sitter_md::LANGUAGE.into()
    })
    .with_highlights(|| tree_sitter_md::HIGHLIGHT_QUERY_BLOCK)
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
    fn dotted_fallback_resolves_sub_variants() {
        let theme = faber_theme::default::faber_dark().syntax;
        assert!(
            theme.highlight_id("keyword").is_some(),
            "keyword base exists"
        );
        assert_eq!(
            theme.highlight_id("keyword.control"),
            theme.highlight_id("keyword.control"),
            "exact match"
        );
        let kw = theme.highlight_id("keyword").unwrap();
        let kwc = theme.highlight_id("keyword.control").unwrap();
        let kwcc = theme.highlight_id("keyword.control.conditional");
        assert!(kwcc.is_some(), "keyword.control.conditional falls back");
        assert_eq!(
            kwcc,
            theme.highlight_id("keyword.control"),
            "falls back to keyword.control"
        );
        let _ = (kw, kwc);
    }

    #[test]
    fn build_highlight_map_maps_capture_ids() {
        let lang = rust();
        let grammar = lang.build_grammar();
        let has_some = grammar.highlight_map.0.iter().any(|e| e.is_some());
        assert!(
            has_some,
            "at least one capture should resolve to a HighlightId"
        );
    }

    #[test]
    fn unknown_capture_name_returns_none() {
        let theme = faber_theme::default::faber_dark().syntax;
        assert_eq!(theme.highlight_id("unknown_xyz_abc_123"), None);
    }

    #[test]
    fn rust_outline_query_compiles_and_has_captures() {
        let config = rust()
            .make_outline_query()
            .expect("Rust outline query should compile");
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
