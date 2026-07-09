// UTF-8 ↔ UTF-16 position conversion for LSP protocol compliance.

// LSP default when positionEncoding capability is not advertised by the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PositionEncoding {
    Utf8,
    #[default]
    Utf16,
}

/// Convert a char-offset in the rope to an LSP Position.
pub fn to_lsp_position(
    rope: &ropey::Rope,
    char_offset: usize,
    encoding: PositionEncoding,
) -> lsp_types::Position {
    let line = rope.char_to_line(char_offset);
    let line_start = rope.line_to_char(line);
    let units: usize = rope
        .slice(line_start..char_offset)
        .chars()
        .map(|ch| match encoding {
            PositionEncoding::Utf16 => ch.len_utf16(),
            PositionEncoding::Utf8 => ch.len_utf8(),
        })
        .sum();
    lsp_types::Position {
        line: line as u32,
        character: units as u32,
    }
}

/// Convert an LSP Position to a char-offset in the rope.
/// Returns None if the position is out of bounds.
pub fn from_lsp_position(
    rope: &ropey::Rope,
    pos: lsp_types::Position,
    encoding: PositionEncoding,
) -> Option<usize> {
    if pos.line as usize >= rope.len_lines() {
        return None;
    }
    let line_start_char = rope.line_to_char(pos.line as usize);
    let line_slice = rope.line(pos.line as usize);
    let target = pos.character as usize;
    let mut units = 0usize;
    let mut chars_consumed = 0usize;
    for ch in line_slice.chars() {
        if units == target {
            return Some(line_start_char + chars_consumed);
        }
        let ch_units = match encoding {
            PositionEncoding::Utf16 => ch.len_utf16(),
            PositionEncoding::Utf8 => ch.len_utf8(),
        };
        units += ch_units;
        chars_consumed += 1;
        if units > target {
            // target landed inside a multi-unit char — out of bounds
            return None;
        }
    }
    // units == target means position is at end of line (before newline or at EOF)
    if units == target {
        Some(line_start_char + chars_consumed)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn pos(line: u32, character: u32) -> lsp_types::Position {
        lsp_types::Position { line, character }
    }

    // 1. ASCII
    #[test]
    fn ascii_utf16() {
        let rope = Rope::from_str("hello\nworld");
        // "hello\nworld": line 1 starts at char 6 ('w').
        // char offset 7 = 'o' → line 1, character 1
        // char offset 8 = 'r' → line 1, character 2
        assert_eq!(
            to_lsp_position(&rope, 7, PositionEncoding::Utf16),
            pos(1, 1)
        );
        assert_eq!(
            to_lsp_position(&rope, 8, PositionEncoding::Utf16),
            pos(1, 2)
        );
    }

    // 2. CJK — 你 is U+4F60, len_utf16 == 1
    #[test]
    fn cjk_utf16() {
        let rope = Rope::from_str("你好\nworld");
        // char offset 1 = '好'
        assert_eq!(
            to_lsp_position(&rope, 1, PositionEncoding::Utf16),
            pos(0, 1)
        );
    }

    // 3. Supplementary plane — 𐐀 is U+10400, len_utf16 == 2, len_utf8 == 4
    #[test]
    fn supplementary_utf16() {
        let rope = Rope::from_str("𐐀b");
        // char offset 1 = 'b', preceded by one char that costs 2 UTF-16 units
        assert_eq!(
            to_lsp_position(&rope, 1, PositionEncoding::Utf16),
            pos(0, 2)
        );
    }

    #[test]
    fn supplementary_utf8() {
        let rope = Rope::from_str("𐐀b");
        // char offset 1 = 'b', preceded by one char that costs 4 UTF-8 bytes
        assert_eq!(to_lsp_position(&rope, 1, PositionEncoding::Utf8), pos(0, 4));
    }

    // 4. Round-trip
    #[test]
    fn round_trip_utf16() {
        let rope = Rope::from_str("hello\n𐐀b\nworld");
        for offset in [0, 3, 5, 6, 7, 8, 9, 14] {
            let lsp = to_lsp_position(&rope, offset, PositionEncoding::Utf16);
            assert_eq!(
                from_lsp_position(&rope, lsp, PositionEncoding::Utf16),
                Some(offset),
                "round-trip failed at offset {offset}"
            );
        }
    }

    #[test]
    fn round_trip_utf8() {
        let rope = Rope::from_str("hello\n𐐀b\nworld");
        for offset in [0, 3, 5, 6, 7, 8, 9, 14] {
            let lsp = to_lsp_position(&rope, offset, PositionEncoding::Utf8);
            assert_eq!(
                from_lsp_position(&rope, lsp, PositionEncoding::Utf8),
                Some(offset),
                "round-trip failed at offset {offset}"
            );
        }
    }

    // 5. Out-of-bounds
    #[test]
    fn out_of_bounds_character() {
        let rope = Rope::from_str("hi\nbye");
        // line 0 has 2 chars = 2 UTF-16 units; character 99 is beyond the line
        assert_eq!(
            from_lsp_position(&rope, pos(0, 99), PositionEncoding::Utf16),
            None
        );
    }

    #[test]
    fn out_of_bounds_line() {
        let rope = Rope::from_str("hi\nbye");
        assert_eq!(
            from_lsp_position(&rope, pos(99, 0), PositionEncoding::Utf16),
            None
        );
    }

    #[test]
    fn out_of_bounds_mid_surrogate_pair() {
        let rope = Rope::from_str("𐐀b");
        // character=1 lands inside the 2-unit surrogate pair of 𐐀
        assert_eq!(
            from_lsp_position(&rope, pos(0, 1), PositionEncoding::Utf16),
            None
        );
    }
}
