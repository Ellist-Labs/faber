use std::{ops::Range, sync::Arc};

use faber_lang::Grammar;
use tree_sitter::{QueryCursor, StreamingIterator, Tree};

/// A single item in a document's symbol outline.
///
/// Used for both code files (tree-sitter, via `OutlineCache`) and markdown
/// (pulldown-cmark, via `parse_markdown`). Features like breadcrumbs, the
/// outline overlay, symbol search, and folding all consume this unified type.
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone, Default)]
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
        let Some(ref g) = self.grammar else { return Outline::default() };
        let Some(ref config) = g.outline else { return Outline::default() };

        let src_bytes = source.as_bytes();
        let root = tree.root_node();

        // Collect raw (byte_range, start_line, end_line, name, context) tuples via query matches.
        // Each match groups @item + @name + optional @context from one pattern.
        let mut raw: Vec<(Range<usize>, usize, usize, String, Option<String>)> = Vec::new();

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&config.query, root, src_bytes);

        matches.advance();
        while let Some(mat) = matches.get() {
            let item_node = mat.captures.iter().find(|c| c.index == config.item_ix).map(|c| c.node);
            let name_node = mat.captures.iter().find(|c| c.index == config.name_ix).map(|c| c.node);
            let ctx_node = config.context_ix.and_then(|ix| {
                mat.captures.iter().find(|c| c.index == ix).map(|c| c.node)
            });

            if let (Some(item), Some(name)) = (item_node, name_node) {
                let start = item.start_byte();
                let end = item.end_byte();
                let start_line = item.start_position().row;
                let end_line = item.end_position().row;
                let name_text = source.get(name.start_byte()..name.end_byte())
                    .unwrap_or("")
                    .to_string();
                let ctx_text = ctx_node.and_then(|n| {
                    source.get(n.start_byte()..n.end_byte()).map(str::to_string)
                });
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
            while stack_ends.last().map_or(false, |&end| end <= range.start) {
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

#[cfg(test)]
mod tests {
    use crate::buffer::Document;
    use faber_lang::LanguageRegistry;
    use std::path::Path;

    fn rust_doc(src: &str) -> Document {
        let reg = LanguageRegistry::with_defaults();
        let lang = reg.language_for_path(Path::new("foo.rs")).unwrap();
        Document::from_str(src, Some(&lang))
    }

    #[test]
    fn basic_fn_depth_zero() {
        let doc = rust_doc("fn hello() {}");
        let items = &doc.outline.items;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "hello");
        assert_eq!(items[0].depth, 0);
        assert_eq!(items[0].context.as_deref(), Some("fn"));
    }

    #[test]
    fn impl_with_methods() {
        let src = "struct Foo; impl Foo { fn a() {} fn b() {} }";
        let doc = rust_doc(src);
        let items = &doc.outline.items;
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"Foo"), "struct expected");
        assert!(names.contains(&"a"), "method a expected");
        assert!(names.contains(&"b"), "method b expected");
        let impl_depth = items.iter().find(|i| i.name == "Foo" && i.context.as_deref() == Some("impl"))
            .map(|i| i.depth).unwrap_or(99);
        let a_depth = items.iter().find(|i| i.name == "a").map(|i| i.depth).unwrap_or(99);
        assert!(a_depth > impl_depth, "method should be deeper than impl");
    }

    #[test]
    fn mod_containing_struct_and_fn() {
        let src = "mod app { struct S; impl S { fn bar() {} } fn free() {} }";
        let doc = rust_doc(src);
        let items = &doc.outline.items;
        let depth_of = |name: &str| items.iter().find(|i| i.name == name).map(|i| i.depth);
        assert_eq!(depth_of("app"), Some(0), "mod at depth 0");
        assert!(depth_of("S").unwrap_or(0) > 0, "struct inside mod");
        assert!(depth_of("bar").unwrap_or(0) > depth_of("S").unwrap_or(0), "fn deeper than struct");
        assert_eq!(depth_of("free"), depth_of("S"), "free fn and struct at same depth");
    }

    #[test]
    fn nested_fns() {
        let src = "fn outer() { fn inner() {} }";
        let doc = rust_doc(src);
        let items = &doc.outline.items;
        let outer = items.iter().find(|i| i.name == "outer").map(|i| i.depth);
        let inner = items.iter().find(|i| i.name == "inner").map(|i| i.depth);
        assert!(inner.unwrap_or(0) > outer.unwrap_or(99), "inner fn deeper than outer");
    }
}
