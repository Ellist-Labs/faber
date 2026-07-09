use ropey::Rope;
use std::{
    io,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};
use tree_sitter::{Parser, Tree};

use crate::{
    highlight::{HighlightCache, HighlightSpan},
    outline::{Outline, OutlineCache},
    parse_source,
};
use faber_core::transaction::{ChangeSet, Transaction};
use faber_lang::{Grammar, Language, LanguageRegistry};

/// Syntax layer for a document: the tree-sitter parser and its current tree.
/// Present only when the document has a resolved language; `None` for plain text.
struct SyntaxState {
    parser: Parser,
    tree: Tree,
}

pub struct Document {
    pub rope: Rope,
    /// Snapshot of the rope at last save (or at open). Used to compute `dirty`.
    saved_rope: Rope,
    pub path: PathBuf,
    pub dirty: bool,
    pub version: u32,
    /// The resolved language for this file; None for plain text.
    pub language: Option<Arc<Language>>,
    /// Compiled grammar (highlights + outline queries). Built once at open time
    /// and shared across caches via `Arc`. `None` for plain-text documents.
    pub(crate) grammar: Option<Arc<Grammar>>,
    /// Syntax parsing state; `None` for plain-text documents.
    syntax: Option<SyntaxState>,
    pub(crate) highlight_cache: HighlightCache,
    outline_cache: OutlineCache,
    /// Current symbol outline — updated on every `apply()`. Empty for
    /// plain-text and markdown (markdown outline lives on EditorView).
    pub outline: Arc<Outline>,
}

impl Document {
    /// Open a file using the provided registry.
    pub fn open_with_registry(path: &str, registry: &LanguageRegistry) -> io::Result<Self> {
        let source = std::fs::read_to_string(path)?;
        let rope = Rope::from_str(&source);
        let pb = PathBuf::from(path);
        let language = registry.language_for_path(&pb);
        let grammar = language.as_ref().map(|l| Arc::new(l.build_grammar()));
        let syntax = language.as_ref().map(|lang| {
            let mut parser = lang.make_parser();
            let tree = parse_source(&mut parser, &source);
            SyntaxState { parser, tree }
        });
        let mut highlight_cache = HighlightCache::default();
        highlight_cache.setup(grammar.as_ref());
        let mut outline_cache = OutlineCache::default();
        outline_cache.setup(grammar.as_ref());
        let outline = syntax
            .as_ref()
            .map(|syn| outline_cache.compute(&syn.tree, &source))
            .unwrap_or_default();
        if let Some(ref syn) = syntax {
            highlight_cache.compute(&syn.tree, &source);
        }
        Ok(Self {
            saved_rope: rope.clone(),
            rope,
            path: pb,
            dirty: false,
            version: 1,
            language,
            grammar,
            syntax,
            highlight_cache,
            outline_cache,
            outline: Arc::new(outline),
        })
    }

    /// In-memory document (for tests / benches). `language` selects the syntax
    /// layer; pass `None` for plain text.
    pub fn from_str(source: &str, language: Option<&Arc<Language>>) -> Self {
        let rope = Rope::from_str(source);
        let grammar = language.map(|l| Arc::new(l.build_grammar()));
        let syntax = language.map(|lang| {
            let mut parser = lang.make_parser();
            let tree = parse_source(&mut parser, source);
            SyntaxState { parser, tree }
        });
        let mut highlight_cache = HighlightCache::default();
        highlight_cache.setup(grammar.as_ref());
        let mut outline_cache = OutlineCache::default();
        outline_cache.setup(grammar.as_ref());
        let outline = syntax
            .as_ref()
            .map(|syn| outline_cache.compute(&syn.tree, source))
            .unwrap_or_default();
        if let Some(ref syn) = syntax {
            highlight_cache.compute(&syn.tree, source);
        }
        Self {
            saved_rope: rope.clone(),
            rope,
            path: PathBuf::from("<memory>"),
            dirty: false,
            version: 1,
            language: language.cloned(),
            grammar,
            syntax,
            highlight_cache,
            outline_cache,
            outline: Arc::new(outline),
        }
    }

    /// Empty in-memory document with no path yet (File > New).
    pub fn empty_untitled() -> Self {
        Self {
            saved_rope: Rope::new(),
            rope: Rope::new(),
            path: PathBuf::new(),
            dirty: false,
            version: 1,
            language: None,
            grammar: None,
            syntax: None,
            highlight_cache: HighlightCache::default(),
            outline_cache: OutlineCache::default(),
            outline: Arc::new(Outline::default()),
        }
    }

    /// An untitled document has never been saved and has no path.
    pub fn is_untitled(&self) -> bool {
        self.path.as_os_str().is_empty()
    }

    /// The document's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the document has unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Assign a real path (first save of an untitled doc) and re-resolve language.
    pub fn assign_path(&mut self, path: PathBuf, registry: &LanguageRegistry) {
        self.path = path;
        self.language = registry.language_for_path(&self.path);
        self.grammar = self.language.as_ref().map(|l| Arc::new(l.build_grammar()));
        let src = self.rope.to_string();
        self.syntax = self.language.as_ref().map(|lang| {
            let mut parser = lang.make_parser();
            let tree = parse_source(&mut parser, &src);
            SyntaxState { parser, tree }
        });
        self.highlight_cache = HighlightCache::default();
        self.highlight_cache.setup(self.grammar.as_ref());
        self.outline_cache = OutlineCache::default();
        self.outline_cache.setup(self.grammar.as_ref());
        if let Some(ref syn) = self.syntax {
            self.highlight_cache.compute(&syn.tree, &src);
            self.outline = Arc::new(self.outline_cache.compute(&syn.tree, &src));
        } else {
            self.outline = Arc::new(Outline::default());
        }
    }

    /// Syntax-highlight spans for the given display line (empty when none).
    pub fn highlight_spans(&self, line_idx: usize) -> &[HighlightSpan] {
        self.highlight_cache
            .lines
            .get(line_idx)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Find the innermost enclosing bracket pair `() [] {}` around `caret_char`.
    /// Returns `(open_char_offset, close_char_offset)` when found, `None` for plain text
    /// or when the caret is not inside any bracket pair.
    /// Uses the live tree-sitter tree (O(tree depth × children), safe to call per frame).
    pub fn enclosing_brackets(&self, caret_char: usize) -> Option<(usize, usize)> {
        let syn = self.syntax.as_ref()?;
        let root = syn.tree.root_node();
        let caret_byte = self
            .rope
            .char_to_byte(caret_char.min(self.rope.len_chars()));
        let mut node = root.descendant_for_byte_range(caret_byte, caret_byte)?;
        loop {
            let count = node.child_count();
            // Scan all children for an open bracket that precedes the caret and has a
            // matching close bracket after it — handles index_expression where `[` is not
            // the first child, as well as arguments/block/array where it is.
            'outer: for i in 0..count {
                let open_node = match node.child(i as u32) {
                    Some(n) => n,
                    None => continue,
                };
                let close_kind = match open_node.kind() {
                    "(" => ")",
                    "[" => "]",
                    "{" => "}",
                    _ => continue,
                };
                let open_byte = open_node.start_byte();
                if open_byte > caret_byte {
                    break; // open is past caret; remaining children are even further
                }
                for j in (i + 1)..count {
                    let close_node = match node.child(j as u32) {
                        Some(n) => n,
                        None => continue 'outer,
                    };
                    if close_node.kind() == close_kind {
                        let close_byte = close_node.start_byte();
                        if close_byte >= caret_byte {
                            let open_char = self.rope.byte_to_char(open_byte);
                            let close_char = self.rope.byte_to_char(close_byte);
                            return Some((open_char, close_char));
                        }
                        break; // this close is before caret; try next open
                    }
                }
            }
            node = node.parent()?;
        }
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// Single mutation choke-point. Applies `tx` to the rope, reparses any syntax
    /// layer, updates the dirty flag, and returns the inverse ChangeSet for undo.
    pub fn apply(&mut self, tx: Transaction) -> ChangeSet {
        self.version += 1;
        let inverse = tx.changes.invert(&self.rope); // capture before applying
        tx.changes.apply(&mut self.rope);

        if let Some(ref mut syn) = self.syntax {
            let src = self.rope.to_string();
            syn.tree = syn
                .parser
                .parse(&src, Some(&syn.tree))
                .or_else(|| syn.parser.parse(&src, None))
                .expect("parse must succeed");
            self.highlight_cache.compute(&syn.tree, &src);
            self.outline = Arc::new(self.outline_cache.compute(&syn.tree, &src));
        }

        self.dirty = self.rope != self.saved_rope;
        inverse
    }

    /// Insert `text` at char offset `char_idx`. Returns the inverse ChangeSet.
    pub fn insert(&mut self, char_idx: usize, text: &str) -> ChangeSet {
        let tx = Transaction::insert(&self.rope, char_idx, text);
        self.apply(tx)
    }

    /// Delete `range` (char offsets, exclusive end). Returns the inverse ChangeSet.
    pub fn delete(&mut self, range: Range<usize>) -> ChangeSet {
        let tx = Transaction::delete(&self.rope, range);
        self.apply(tx)
    }

    /// Record the current rope as the saved baseline; clears the dirty flag.
    pub fn mark_saved(&mut self) {
        self.saved_rope = self.rope.clone();
        self.dirty = false;
    }

    pub fn lsp_sync_info(&self) -> (u32, std::path::PathBuf, String) {
        (self.version, self.path.clone(), self.rope.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_registry() -> LanguageRegistry {
        LanguageRegistry::with_defaults()
    }

    #[test]
    fn insert_updates_rope_and_dirty() {
        let mut doc = Document::from_str("hello", None);
        assert_eq!(doc.rope.to_string(), "hello");
        assert!(!doc.dirty);
        doc.insert(5, " world");
        assert_eq!(doc.rope.to_string(), "hello world");
        assert!(doc.dirty);
        doc.mark_saved();
        assert!(!doc.dirty);
    }

    #[test]
    fn delete_updates_rope() {
        let mut doc = Document::from_str("hello world", None);
        doc.delete(5..11);
        assert_eq!(doc.rope.to_string(), "hello");
        assert!(doc.dirty);
    }

    #[test]
    fn mark_saved_clears_dirty() {
        let mut doc = Document::from_str("abc", None);
        doc.insert(3, "d");
        assert!(doc.dirty);
        doc.mark_saved();
        assert!(!doc.dirty);
        doc.insert(4, "e");
        assert!(doc.dirty);
    }

    #[test]
    fn tree_node_count_increases_after_insert() {
        use crate::node_count;
        let reg = test_registry();
        let lang = reg.language_for_path(Path::new("foo.rs")).unwrap();
        let mut doc = Document::from_str("fn main() {}", Some(&lang));
        let before = doc
            .syntax
            .as_ref()
            .map(|s| node_count(&s.tree))
            .unwrap_or(0);
        doc.insert(11, " let x = 1;");
        let after = doc
            .syntax
            .as_ref()
            .map(|s| node_count(&s.tree))
            .unwrap_or(0);
        assert!(after >= before, "tree should grow after insert");
    }

    #[test]
    fn enclosing_brackets_finds_innermost() {
        let reg = test_registry();
        let lang = reg.language_for_path(Path::new("foo.rs")).unwrap();
        // Source: `foo(bar[baz])` — caret on 'b' of "baz" (char offset 8)
        // Innermost enclosing pair should be `[` (offset 7) and `]` (offset 11).
        let doc = Document::from_str("foo(bar[baz])", Some(&lang));
        let result = doc.enclosing_brackets(8);
        assert!(result.is_some(), "should find enclosing brackets");
        let (open, close) = result.unwrap();
        let rope = &doc.rope;
        assert_eq!(rope.char(open), '[', "open bracket");
        assert_eq!(rope.char(close), ']', "close bracket");
    }

    #[test]
    fn enclosing_brackets_none_for_plain_text() {
        let doc = Document::from_str("hello world", None);
        assert_eq!(doc.enclosing_brackets(3), None, "no grammar → no result");
    }
}
