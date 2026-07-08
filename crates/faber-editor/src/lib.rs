pub mod buffer;
pub mod edit_history;
pub mod file_index;
pub mod highlight;
pub mod markdown;
pub mod outline;
pub mod project;
pub mod project_search;
pub mod save;

// Re-export from faber-core so faber-app's imports keep working.
pub use faber_core::anchor::{Anchor, Bias};
pub use faber_core::movement as cursor;
pub use faber_core::search;
pub use faber_core::selection::{Selection, SelectionSet};
pub use faber_core::transaction::{ChangeSet, Transaction};

// Re-export faber-lang so consumers can set up the registry.
pub use faber_lang::{Language, LanguageId, LanguageRegistry, SyntaxToken};

use tree_sitter::{InputEdit, Parser, Tree};

/// Build a tree-sitter parser for Rust (convenience wrapper used by benches/tests
/// that don't want to go through the LanguageRegistry).
pub fn make_rust_parser() -> Parser {
    faber_lang::LanguageRegistry::with_defaults()
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
    // Parser::parse returns None when no language is set or parsing is cancelled.
    // Return the invalidated tree in that case so callers never panic on a hot path.
    parser.parse(new_source, Some(&tree)).unwrap_or(tree)
}

pub fn node_count(tree: &Tree) -> usize {
    tree.root_node().descendant_count()
}
