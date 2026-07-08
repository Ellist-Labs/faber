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
        let outline = syntax.as_ref()
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
        let outline = syntax.as_ref()
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
        self.highlight_cache.lines.get(line_idx).map(|v| v.as_slice()).unwrap_or(&[])
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
        let before = doc.syntax.as_ref().map(|s| node_count(&s.tree)).unwrap_or(0);
        doc.insert(11, " let x = 1;");
        let after = doc.syntax.as_ref().map(|s| node_count(&s.tree)).unwrap_or(0);
        assert!(after >= before, "tree should grow after insert");
    }
}
