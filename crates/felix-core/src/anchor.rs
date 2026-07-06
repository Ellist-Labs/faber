use ropey::Rope;

/// Which side of an inserted character a position sticks to.
///
/// `Left`  → the position stays *before* newly inserted text (useful for
///           the start of a diagnostic range — it doesn't move when text
///           is prepended at that location).
/// `Right` → the position moves *after* newly inserted text (useful for a
///           cursor that keeps up with typing).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Bias {
    Left,
    #[default]
    Right,
}

/// A char-offset position that survives edits when mapped through a
/// `ChangeSet::map_pos(offset, bias)`. Used for long-lived markers
/// (LSP diagnostics, bookmarks, collab cursors) rather than the hot
/// selection path (which stays as raw `usize` offsets mapped eagerly).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub offset: usize,
    pub bias: Bias,
}

impl Anchor {
    pub fn new(offset: usize, bias: Bias) -> Self {
        Self { offset, bias }
    }

    /// Clamp the anchor offset to the current rope length.
    pub fn to_offset(&self, rope: &Rope) -> usize {
        self.offset.min(rope.len_chars())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    #[test]
    fn anchor_clamps_to_rope_len() {
        let rope = Rope::from_str("hello");
        let a = Anchor::new(100, Bias::Left);
        assert_eq!(a.to_offset(&rope), 5);
    }

    #[test]
    fn anchor_within_bounds() {
        let rope = Rope::from_str("hello");
        let a = Anchor::new(3, Bias::Right);
        assert_eq!(a.to_offset(&rope), 3);
    }
}
