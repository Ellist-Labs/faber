/// UTF-16 ↔ char-offset helpers needed by the LSP protocol.
///
/// LSP positions are `(line, utf16_col)` where the column is counted in
/// UTF-16 code units.  Everything here is pure and rope-free so it can be
/// tested without a document.
use ropey::Rope;

/// Convert a rope char offset to a `(line, utf16_col)` LSP position.
pub fn char_to_lsp(rope: &Rope, char_idx: usize) -> (u32, u32) {
    let char_idx = char_idx.min(rope.len_chars());
    let line = rope.char_to_line(char_idx);
    let line_start = rope.line_to_char(line);
    let col_chars = char_idx - line_start;

    // Count UTF-16 code units for the chars before char_idx on this line.
    let utf16_col: usize = rope
        .line(line)
        .chars()
        .take(col_chars)
        .map(|c| c.len_utf16())
        .sum();

    (line as u32, utf16_col as u32)
}

/// Convert a `(line, utf16_col)` LSP position to a rope char offset.
pub fn lsp_to_char(rope: &Rope, line: u32, utf16_col: u32) -> usize {
    let line = (line as usize).min(rope.len_lines().saturating_sub(1));
    let line_start = rope.line_to_char(line);
    let mut remaining_utf16 = utf16_col as usize;
    let mut col_chars = 0;
    for ch in rope.line(line).chars() {
        if remaining_utf16 == 0 {
            break;
        }
        let units = ch.len_utf16();
        if units > remaining_utf16 {
            break;
        }
        remaining_utf16 -= units;
        col_chars += 1;
    }
    line_start + col_chars
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn round_trip(src: &str, char_idx: usize) {
        let rope = Rope::from_str(src);
        let (line, col) = char_to_lsp(&rope, char_idx);
        let back = lsp_to_char(&rope, line, col);
        assert_eq!(
            back, char_idx,
            "round-trip failed for '{src}' at {char_idx}"
        );
    }

    #[test]
    fn ascii_round_trip() {
        round_trip("hello\nworld", 0);
        round_trip("hello\nworld", 5);
        round_trip("hello\nworld", 6);
        round_trip("hello\nworld", 11);
    }

    #[test]
    fn multibyte_bmp() {
        // "é" is 1 char but 1 UTF-16 code unit (U+00E9, fits in BMP)
        round_trip("héllo", 0);
        round_trip("héllo", 1);
        round_trip("héllo", 3);
    }

    #[test]
    fn supplementary_plane_char() {
        // "😀" is 1 char but 2 UTF-16 code units (U+1F600, surrogate pair)
        let rope = Rope::from_str("a\u{1F600}b");
        // char 0='a', char 1='😀', char 2='b'
        let (_, col_a) = char_to_lsp(&rope, 0);
        let (_, col_emoji) = char_to_lsp(&rope, 1);
        let (_, col_b) = char_to_lsp(&rope, 2);
        assert_eq!(col_a, 0);
        assert_eq!(col_emoji, 1); // 'a' = 1 UTF-16 unit
        assert_eq!(col_b, 3); // 'a'(1) + '😀'(2) = 3

        round_trip("a\u{1F600}b", 0);
        round_trip("a\u{1F600}b", 1);
        round_trip("a\u{1F600}b", 2);
    }

    #[test]
    fn empty_rope() {
        let rope = Rope::from_str("");
        assert_eq!(char_to_lsp(&rope, 0), (0, 0));
        assert_eq!(lsp_to_char(&rope, 0, 0), 0);
    }
}
