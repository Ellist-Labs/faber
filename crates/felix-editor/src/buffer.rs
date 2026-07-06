use ropey::Rope;
use std::{io, ops::Range, path::PathBuf, sync::Arc};
use tree_sitter::{InputEdit, Parser, Point, Tree};

use crate::{parse_source, reparse_source};
use felix_lang::{Language, LanguageRegistry};

/// A recorded change — enough to apply and invert for undo/redo.
/// Replaced by `Transaction` in Step 5; kept for undo machinery in the interim.
pub struct Edit {
    /// Char range in the document *before* this edit was applied.
    pub char_range: Range<usize>,
    pub removed: String,
    pub inserted: String,
}

impl Edit {
    pub fn invert(&self) -> Edit {
        let new_end = self.char_range.start + self.inserted.chars().count();
        Edit {
            char_range: self.char_range.start..new_end,
            removed: self.inserted.clone(),
            inserted: self.removed.clone(),
        }
    }
}

pub struct Document {
    pub rope: Rope,
    pub path: PathBuf,
    pub dirty: bool,
    /// The resolved language for this file; None for plain text.
    pub language: Option<Arc<Language>>,
    parser: Parser,
    pub tree: Tree,
}

impl Document {
    /// Open a file, resolving its language from the default registry.
    pub fn open(path: &str) -> io::Result<Self> {
        let registry = LanguageRegistry::with_defaults();
        Self::open_with_registry(path, &registry)
    }

    /// Open a file using the provided registry (useful for tests / custom grammars).
    pub fn open_with_registry(path: &str, registry: &LanguageRegistry) -> io::Result<Self> {
        let source = std::fs::read_to_string(path)?;
        let rope = Rope::from_str(&source);
        let pb = PathBuf::from(path);
        let language = registry.language_for_path(&pb);
        let (parser, tree) = if let Some(ref lang) = language {
            let mut p = lang.make_parser();
            let t = parse_source(&mut p, &source);
            (p, t)
        } else {
            // Plain text: use an inert parser and an empty tree.
            let mut p = Parser::new();
            // parse with no language set returns None; fall back gracefully.
            let t = p.parse("", None).unwrap_or_else(|| {
                let mut fallback = LanguageRegistry::with_defaults()
                    .language_for_path(std::path::Path::new("_.rs"))
                    .unwrap()
                    .make_parser();
                parse_source(&mut fallback, "")
            });
            (p, t)
        };
        Ok(Self { rope, path: pb, dirty: false, language, parser, tree })
    }

    /// In-memory document (for tests / benches).
    pub fn from_str(source: &str) -> Self {
        let rope = Rope::from_str(source);
        let registry = LanguageRegistry::with_defaults();
        // Use Rust grammar as the default for in-memory docs (all fixtures are Rust).
        let lang = registry.language_for_path(std::path::Path::new("_.rs")).unwrap();
        let mut parser = lang.make_parser();
        let tree = parse_source(&mut parser, source);
        Self {
            rope,
            path: PathBuf::from("<memory>"),
            dirty: false,
            language: Some(lang),
            parser,
            tree,
        }
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// Insert `text` at char offset `char_idx`. Returns the edit record.
    pub fn insert(&mut self, char_idx: usize, text: &str) -> Edit {
        let start_byte = self.rope.char_to_byte(char_idx);
        let start_line = self.rope.char_to_line(char_idx);
        let start_col = char_idx - self.rope.line_to_char(start_line);

        self.rope.insert(char_idx, text);

        let new_end_byte = start_byte + text.len();
        let newlines = text.chars().filter(|&c| c == '\n').count();
        let new_end_line = start_line + newlines;
        let new_end_col = if newlines == 0 {
            start_col + text.chars().count()
        } else {
            text.rfind('\n').map_or(0, |i| text[i + 1..].chars().count())
        };

        let ie = InputEdit {
            start_byte,
            old_end_byte: start_byte,
            new_end_byte,
            start_position: Point { row: start_line, column: start_col },
            old_end_position: Point { row: start_line, column: start_col },
            new_end_position: Point { row: new_end_line, column: new_end_col },
        };

        // Per-keystroke alloc — acceptable cost until Step 5 (Transaction rewrite).
        let src = self.rope.to_string();
        self.tree = reparse_source(&mut self.parser, &self.tree, &ie, &src);
        self.dirty = true;

        Edit { char_range: char_idx..char_idx, removed: String::new(), inserted: text.to_string() }
    }

    /// Delete `range` (char offsets, exclusive end). Returns the edit record.
    pub fn delete(&mut self, range: Range<usize>) -> Edit {
        debug_assert!(range.start <= range.end && range.end <= self.rope.len_chars());

        let start_byte = self.rope.char_to_byte(range.start);
        let end_byte = self.rope.char_to_byte(range.end);
        let start_line = self.rope.char_to_line(range.start);
        let start_col = range.start - self.rope.line_to_char(start_line);
        let end_line = self.rope.char_to_line(range.end);
        let end_col = range.end - self.rope.line_to_char(end_line);

        let removed: String = self.rope.slice(range.start..range.end).to_string();
        self.rope.remove(range.start..range.end);

        let ie = InputEdit {
            start_byte,
            old_end_byte: end_byte,
            new_end_byte: start_byte,
            start_position: Point { row: start_line, column: start_col },
            old_end_position: Point { row: end_line, column: end_col },
            new_end_position: Point { row: start_line, column: start_col },
        };

        let src = self.rope.to_string();
        self.tree = reparse_source(&mut self.parser, &self.tree, &ie, &src);
        self.dirty = true;

        Edit { char_range: range.start..range.start, removed, inserted: String::new() }
    }
}
