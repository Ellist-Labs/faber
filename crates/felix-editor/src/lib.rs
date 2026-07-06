pub mod buffer;
pub mod edit_history;
pub mod highlight;
pub mod project;
pub mod save;

// Re-export from felix-core so felix-app's imports keep working.
pub use felix_core::movement as cursor;
pub use felix_core::search;
pub use felix_core::selection::{Selection, SelectionSet};
pub use felix_core::anchor::{Anchor, Bias};

// Re-export felix-lang so consumers can set up the registry.
pub use felix_lang::{Language, LanguageId, LanguageRegistry, SyntaxToken};

use ropey::Rope;
use std::{fs, io};
use tree_sitter::{InputEdit, Parser, Tree};

/// Build a tree-sitter parser for Rust (convenience wrapper used by benches/tests
/// that don't want to go through the LanguageRegistry).
pub fn make_rust_parser() -> Parser {
    felix_lang::LanguageRegistry::with_defaults()
        .language_for_path(std::path::Path::new("_.rs"))
        .expect("Rust language not registered")
        .make_parser()
}

pub fn parse_source(parser: &mut Parser, source: &str) -> Tree {
    parser.parse(source, None).expect("parse failed")
}

pub fn reparse_source(
    parser: &mut Parser,
    old_tree: &Tree,
    edit: &InputEdit,
    new_source: &str,
) -> Tree {
    let mut tree = old_tree.clone();
    tree.edit(edit);
    parser.parse(new_source, Some(&tree)).expect("reparse failed")
}

pub fn node_count(tree: &Tree) -> usize {
    tree.root_node().descendant_count()
}

// Dead legacy helper; kept temporarily so the bench include_str! seed
// continues to compile until its removal in a later step.
pub fn load_rope(path: &str) -> io::Result<(String, Rope)> {
    let source = fs::read_to_string(path)?;
    let rope = Rope::from_str(&source);
    Ok((source, rope))
}
