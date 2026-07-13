use faber_core::{anchor::Bias, transaction::ChangeSet};
use ropey::Rope;
use tree_sitter::{InputEdit, Parser, Point, Tree};

/// Derive a single (coalesced) InputEdit from a ChangeSet and the pre-edit rope.
/// Returns None if the ChangeSet has no actual changes.
pub fn input_edit_from_changeset(
    changes: &ChangeSet,
    pre: &Rope,
    post: &Rope,
) -> Option<InputEdit> {
    let mut iter = changes.iter_changes();
    let first = iter.next()?;
    let mut start_char = first.old_range.start;
    let mut old_end_char = first.old_range.end.max(first.old_range.start);

    for item in iter {
        start_char = start_char.min(item.old_range.start);
        old_end_char = old_end_char.max(item.old_range.end);
    }

    let new_end_char = changes.map_pos(old_end_char, Bias::Right);

    let start_byte = pre.char_to_byte(start_char.min(pre.len_chars()));
    let old_end_byte = pre.char_to_byte(old_end_char.min(pre.len_chars()));
    let new_end_byte = post.char_to_byte(new_end_char.min(post.len_chars()));

    Some(InputEdit {
        start_byte,
        old_end_byte,
        new_end_byte,
        start_position: rope_point(pre, start_byte),
        old_end_position: rope_point(pre, old_end_byte),
        new_end_position: rope_point(post, new_end_byte),
    })
}

/// Parse using a Rope chunk callback (zero-copy, no to_string).
/// If parse returns None (cancelled or no language set), the caller should keep the old tree.
pub fn reparse_with_rope(parser: &mut Parser, old_tree: &Tree, rope: &Rope) -> Option<Tree> {
    parser.parse_with_options(
        &mut |byte_offset: usize, _point: Point| -> &[u8] {
            if byte_offset >= rope.len_bytes() {
                return &[];
            }
            let (chunk, chunk_byte_start, _, _) = rope.chunk_at_byte(byte_offset);
            &chunk.as_bytes()[byte_offset - chunk_byte_start..]
        },
        Some(old_tree),
        None,
    )
}

pub fn rope_point(rope: &Rope, byte: usize) -> Point {
    let row = rope.byte_to_line(byte.min(rope.len_bytes()));
    let line_start_byte = rope.line_to_byte(row);
    let column = byte.saturating_sub(line_start_byte);
    Point { row, column }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn make_edit(
        src: &str,
        char_start: usize,
        char_end: usize,
        replacement: &str,
    ) -> (Rope, Rope, ChangeSet) {
        let pre = Rope::from_str(src);
        let changes = ChangeSet::from_changes(
            pre.len_chars(),
            [(char_start, char_end, replacement.to_string())],
        );
        let mut post = pre.clone();
        changes.apply(&mut post);
        (pre, post, changes)
    }

    #[test]
    fn pure_insert_yields_edit() {
        let (pre, post, changes) = make_edit("hello", 2, 2, "XY");
        let edit = input_edit_from_changeset(&changes, &pre, &post).unwrap();
        assert_eq!(edit.start_byte, 2);
        assert_eq!(edit.old_end_byte, 2);
        assert_eq!(edit.new_end_byte, 4);
    }

    #[test]
    fn pure_delete_yields_edit() {
        let (pre, post, changes) = make_edit("hello", 1, 3, "");
        let edit = input_edit_from_changeset(&changes, &pre, &post).unwrap();
        assert_eq!(edit.start_byte, 1);
        assert_eq!(edit.old_end_byte, 3);
        assert_eq!(edit.new_end_byte, 1);
    }

    #[test]
    fn replace_yields_edit() {
        let (pre, post, changes) = make_edit("hello world", 6, 11, "rust");
        let edit = input_edit_from_changeset(&changes, &pre, &post).unwrap();
        assert_eq!(edit.start_byte, 6);
        assert_eq!(edit.old_end_byte, 11);
        assert_eq!(edit.new_end_byte, 10);
    }

    #[test]
    fn multibyte_character_edit() {
        // "héllo" — 'é' is 2 bytes at char index 1. Byte offsets: h=0, é=1..3, l=3, l=4, o=5
        let (pre, post, changes) = make_edit("héllo", 1, 2, "e");
        let edit = input_edit_from_changeset(&changes, &pre, &post).unwrap();
        assert_eq!(edit.start_byte, 1, "start byte");
        assert_eq!(edit.old_end_byte, 3, "old_end byte after 2-byte é");
        assert_eq!(edit.new_end_byte, 2, "new_end byte after 1-byte e");
    }

    #[test]
    fn identity_changeset_returns_none() {
        let pre = Rope::from_str("hello");
        let post = pre.clone();
        let changes = ChangeSet::identity(pre.len_chars());
        let result = input_edit_from_changeset(&changes, &pre, &post);
        assert!(result.is_none(), "identity should produce no edit");
    }

    #[test]
    fn point_column_is_byte_column() {
        // "hello\nworld" — 'w' is at byte 6, row 1, column 0
        let rope = Rope::from_str("hello\nworld");
        let pt = rope_point(&rope, 6);
        assert_eq!(pt.row, 1);
        assert_eq!(pt.column, 0);
    }
}
