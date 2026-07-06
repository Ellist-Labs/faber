use ropey::Rope;
use std::{fs, io};
use tree_sitter::{InputEdit, Parser, Tree};

pub fn load_rope(path: &str) -> io::Result<(String, Rope)> {
    let source = fs::read_to_string(path)?;
    let rope = Rope::from_str(&source);
    Ok((source, rope))
}

pub fn make_rust_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("failed to load Rust grammar");
    parser
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
