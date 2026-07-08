use ropey::Rope;

use crate::selection::Selection;

/// Classifies a character as part of a "word" for word-boundary movement.
/// Can be overridden per language in future; default = alphanumeric or `_`.
pub type WordClassifier = fn(char) -> bool;

pub fn default_word_classifier(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Column (0-indexed char offset within the line) of `char_idx`.
pub fn col_of(rope: &Rope, char_idx: usize) -> usize {
    let char_idx = char_idx.min(rope.len_chars());
    let line = rope.char_to_line(char_idx);
    char_idx - rope.line_to_char(line)
}

/// Number of non-newline characters on `line`.
fn line_content_len(rope: &Rope, line: usize) -> usize {
    let s = rope.line(line).to_string();
    s.trim_end_matches(['\n', '\r']).chars().count()
}

fn apply(sel: Selection, new_head: usize, rope: &Rope, extend: bool) -> Selection {
    Selection {
        anchor: if extend { sel.anchor } else { new_head },
        head: new_head,
        goal_col: col_of(rope, new_head),
    }
}

// ── character movement ────────────────────────────────────────────────────────

pub fn move_left(rope: &Rope, sel: Selection, extend: bool) -> Selection {
    if !extend && !sel.is_empty() {
        let pos = sel.start();
        return Selection { anchor: pos, head: pos, goal_col: col_of(rope, pos) };
    }
    let new_head = sel.head.saturating_sub(1);
    apply(sel, new_head, rope, extend)
}

pub fn move_right(rope: &Rope, sel: Selection, extend: bool) -> Selection {
    if !extend && !sel.is_empty() {
        let pos = sel.end();
        return Selection { anchor: pos, head: pos, goal_col: col_of(rope, pos) };
    }
    let new_head = (sel.head + 1).min(rope.len_chars());
    apply(sel, new_head, rope, extend)
}

pub fn move_up(rope: &Rope, sel: Selection, extend: bool) -> Selection {
    let line = rope.char_to_line(sel.head);
    if line == 0 {
        return Selection {
            anchor: if extend { sel.anchor } else { 0 },
            head: 0,
            goal_col: sel.goal_col,
        };
    }
    let target = line - 1;
    let col = sel.goal_col.min(line_content_len(rope, target));
    let new_head = rope.line_to_char(target) + col;
    Selection {
        anchor: if extend { sel.anchor } else { new_head },
        head: new_head,
        goal_col: sel.goal_col,
    }
}

pub fn move_down(rope: &Rope, sel: Selection, extend: bool) -> Selection {
    let line = rope.char_to_line(sel.head);
    let last_line = rope.len_lines().saturating_sub(1);
    if line >= last_line {
        let pos = rope.len_chars();
        return Selection {
            anchor: if extend { sel.anchor } else { pos },
            head: pos,
            goal_col: sel.goal_col,
        };
    }
    let target = line + 1;
    let col = sel.goal_col.min(line_content_len(rope, target));
    let new_head = rope.line_to_char(target) + col;
    Selection {
        anchor: if extend { sel.anchor } else { new_head },
        head: new_head,
        goal_col: sel.goal_col,
    }
}

// ── line movement ─────────────────────────────────────────────────────────────

pub fn move_home(rope: &Rope, sel: Selection, extend: bool) -> Selection {
    let line = rope.char_to_line(sel.head);
    let pos = rope.line_to_char(line);
    Selection { anchor: if extend { sel.anchor } else { pos }, head: pos, goal_col: 0 }
}

pub fn move_end(rope: &Rope, sel: Selection, extend: bool) -> Selection {
    let line = rope.char_to_line(sel.head);
    let content_len = line_content_len(rope, line);
    let pos = rope.line_to_char(line) + content_len;
    Selection {
        anchor: if extend { sel.anchor } else { pos },
        head: pos,
        goal_col: content_len,
    }
}

// ── document movement ─────────────────────────────────────────────────────────

pub fn move_doc_start(sel: Selection, extend: bool) -> Selection {
    Selection { anchor: if extend { sel.anchor } else { 0 }, head: 0, goal_col: 0 }
}

pub fn move_doc_end(rope: &Rope, sel: Selection, extend: bool) -> Selection {
    let pos = rope.len_chars();
    let col = col_of(rope, pos);
    Selection { anchor: if extend { sel.anchor } else { pos }, head: pos, goal_col: col }
}

// ── page movement ─────────────────────────────────────────────────────────────

pub fn move_page_up(rope: &Rope, sel: Selection, extend: bool, page_lines: usize) -> Selection {
    let line = rope.char_to_line(sel.head);
    let target = line.saturating_sub(page_lines);
    let col = sel.goal_col.min(line_content_len(rope, target));
    let new_head = rope.line_to_char(target) + col;
    Selection {
        anchor: if extend { sel.anchor } else { new_head },
        head: new_head,
        goal_col: sel.goal_col,
    }
}

pub fn move_page_down(rope: &Rope, sel: Selection, extend: bool, page_lines: usize) -> Selection {
    let line = rope.char_to_line(sel.head);
    let last_line = rope.len_lines().saturating_sub(1);
    let target = (line + page_lines).min(last_line);
    let col = sel.goal_col.min(line_content_len(rope, target));
    let new_head = rope.line_to_char(target) + col;
    Selection {
        anchor: if extend { sel.anchor } else { new_head },
        head: new_head,
        goal_col: sel.goal_col,
    }
}

// ── word movement ─────────────────────────────────────────────────────────────

pub fn move_word_left(rope: &Rope, sel: Selection, extend: bool) -> Selection {
    let pos = word_boundary_left(rope, sel.head, default_word_classifier);
    apply(sel, pos, rope, extend)
}

pub fn move_word_right(rope: &Rope, sel: Selection, extend: bool) -> Selection {
    let pos = word_boundary_right(rope, sel.head, default_word_classifier);
    apply(sel, pos, rope, extend)
}

pub fn select_all(rope: &Rope) -> Selection {
    let pos = rope.len_chars();
    Selection { anchor: 0, head: pos, goal_col: col_of(rope, pos) }
}

fn word_boundary_left(rope: &Rope, mut pos: usize, is_word: WordClassifier) -> usize {
    if pos == 0 {
        return 0;
    }
    pos -= 1;
    while pos > 0 && rope.char(pos).is_whitespace() {
        pos -= 1;
    }
    if pos == 0 {
        return 0;
    }
    let start_class = is_word(rope.char(pos));
    while pos > 0 && is_word(rope.char(pos - 1)) == start_class {
        pos -= 1;
    }
    pos
}

fn word_boundary_right(rope: &Rope, mut pos: usize, is_word: WordClassifier) -> usize {
    let len = rope.len_chars();
    if pos >= len {
        return len;
    }
    let start_class = is_word(rope.char(pos));
    while pos < len && is_word(rope.char(pos)) == start_class {
        pos += 1;
    }
    while pos < len && rope.char(pos).is_whitespace() && rope.char(pos) != '\n' {
        pos += 1;
    }
    pos
}

// ── click helpers ─────────────────────────────────────────────────────────────

/// Returns a Selection spanning the word at `pos` (double-click word select).
/// Expands left/right from `pos` to the boundaries of the same character class.
pub fn word_at(rope: &Rope, pos: usize) -> Selection {
    let len = rope.len_chars();
    if len == 0 {
        return Selection { anchor: 0, head: 0, goal_col: 0 };
    }
    let pos = pos.min(len.saturating_sub(1));
    let ch = rope.char(pos);
    let is_word = default_word_classifier(ch);
    let mut start = pos;
    while start > 0 && default_word_classifier(rope.char(start - 1)) == is_word {
        start -= 1;
    }
    let mut end = pos + 1;
    while end < len && default_word_classifier(rope.char(end)) == is_word {
        end += 1;
    }
    Selection { anchor: start, head: end, goal_col: col_of(rope, end) }
}

/// Returns a Selection spanning the entire content of the line containing `pos`
/// (triple-click line select).
pub fn line_selection(rope: &Rope, pos: usize) -> Selection {
    let len = rope.len_chars();
    if len == 0 {
        return Selection { anchor: 0, head: 0, goal_col: 0 };
    }
    let line = rope.char_to_line(pos.min(len.saturating_sub(1)));
    let start = rope.line_to_char(line);
    let content = line_content_len(rope, line);
    let end = start + content;
    Selection { anchor: start, head: end, goal_col: content }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn sel(pos: usize) -> Selection {
        Selection { anchor: pos, head: pos, goal_col: 0 }
    }

    // ── left / right ─────────────────────────────────────────────────────────

    #[test]
    fn move_left_at_start_stays() {
        let r = Rope::from_str("abc");
        assert_eq!(move_left(&r, sel(0), false).head, 0);
    }

    #[test]
    fn move_right_at_end_stays() {
        let r = Rope::from_str("abc");
        assert_eq!(move_right(&r, sel(3), false).head, 3);
    }

    #[test]
    fn move_right_across_multibyte() {
        // "a😀b" — emoji is 1 char in ropey
        let r = Rope::from_str("a\u{1F600}b");
        assert_eq!(move_right(&r, sel(1), false).head, 2);
        assert_eq!(move_right(&r, sel(2), false).head, 3);
    }

    #[test]
    fn move_left_collapses_selection() {
        let r = Rope::from_str("hello world");
        let s = Selection { anchor: 2, head: 6, goal_col: 0 };
        let result = move_left(&r, s, false);
        assert_eq!(result.head, 2);
        assert_eq!(result.anchor, 2);
    }

    #[test]
    fn move_right_collapses_selection_to_end() {
        let r = Rope::from_str("hello world");
        let s = Selection { anchor: 2, head: 6, goal_col: 0 };
        let result = move_right(&r, s, false);
        assert_eq!(result.head, 6);
        assert_eq!(result.anchor, 6);
    }

    // ── up / down ────────────────────────────────────────────────────────────

    #[test]
    fn move_up_from_first_line_goes_to_start() {
        let r = Rope::from_str("hello\nworld");
        let result = move_up(&r, sel(3), false);
        assert_eq!(result.head, 0);
    }

    #[test]
    fn move_down_from_last_line_goes_to_end() {
        let r = Rope::from_str("hello\nworld");
        let result = move_down(&r, sel(8), false);
        assert_eq!(result.head, r.len_chars());
    }

    #[test]
    fn move_down_preserves_goal_col() {
        let r = Rope::from_str("hello\nhi");
        let s = Selection { anchor: 4, head: 4, goal_col: 4 };
        let result = move_down(&r, s, false);
        // "hi" only has 2 chars, goal_col=4 clamped to 2
        assert_eq!(result.head, r.line_to_char(1) + 2);
        assert_eq!(result.goal_col, 4); // preserved
    }

    // ── home / end ───────────────────────────────────────────────────────────

    #[test]
    fn move_home_goes_to_line_start() {
        let r = Rope::from_str("hello\nworld");
        assert_eq!(move_home(&r, sel(8), false).head, 6);
    }

    #[test]
    fn move_end_skips_newline() {
        let r = Rope::from_str("hello\nworld");
        // end of first line = position 5 (the \n is excluded from content)
        assert_eq!(move_end(&r, sel(2), false).head, 5);
    }

    // ── doc start / end ──────────────────────────────────────────────────────

    #[test]
    fn move_doc_start_from_middle() {
        let _r = Rope::from_str("abc\ndef");
        assert_eq!(move_doc_start(sel(5), false).head, 0);
    }

    #[test]
    fn move_doc_end_from_start() {
        let r = Rope::from_str("abc\ndef");
        assert_eq!(move_doc_end(&r, sel(0), false).head, 7);
    }

    // ── word movement ────────────────────────────────────────────────────────

    #[test]
    fn word_right_skips_identifier() {
        let r = Rope::from_str("hello world");
        assert_eq!(move_word_right(&r, sel(0), false).head, 6);
    }

    #[test]
    fn word_left_skips_identifier() {
        let r = Rope::from_str("hello world");
        assert_eq!(move_word_left(&r, sel(11), false).head, 6);
    }

    // ── extend ───────────────────────────────────────────────────────────────

    #[test]
    fn extend_right_keeps_anchor() {
        let r = Rope::from_str("hello");
        let s = sel(2);
        let result = move_right(&r, s, true);
        assert_eq!(result.anchor, 2);
        assert_eq!(result.head, 3);
    }

    // ── empty rope edge cases ────────────────────────────────────────────────

    #[test]
    fn movements_on_empty_rope() {
        let r = Rope::from_str("");
        assert_eq!(move_left(&r, sel(0), false).head, 0);
        assert_eq!(move_right(&r, sel(0), false).head, 0);
        assert_eq!(move_up(&r, sel(0), false).head, 0);
        assert_eq!(move_down(&r, sel(0), false).head, 0);
    }
}
