use ropey::Rope;
use std::ops::Range;

/// A single cursor range with an immovable anchor and a moving head.
/// When anchor == head the selection is collapsed (caret only).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
    /// Desired column preserved across up/down moves.
    pub goal_col: usize,
}

impl Selection {
    pub fn collapsed(pos: usize, rope: &Rope) -> Self {
        Self { anchor: pos, head: pos, goal_col: crate::movement::col_of(rope, pos) }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn range(&self) -> Range<usize> {
        self.start()..self.end()
    }
}

/// A set of selections (one per cursor). The `primary` index is the one the
/// view scrolls to and that single-cursor operations default to.
///
/// Invariants:
/// - `ranges` is never empty.
/// - `primary < ranges.len()`.
/// - Ranges are sorted by start offset and non-overlapping (enforced by `normalize`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionSet {
    pub ranges: Vec<Selection>,
    pub primary: usize,
}

impl SelectionSet {
    pub fn single(sel: Selection) -> Self {
        Self { ranges: vec![sel], primary: 0 }
    }

    pub fn primary(&self) -> &Selection {
        &self.ranges[self.primary]
    }

    pub fn primary_mut(&mut self) -> &mut Selection {
        &mut self.ranges[self.primary]
    }

    /// Replace the primary selection, keeping the set as a single-cursor set.
    pub fn set_primary(&mut self, sel: Selection) {
        self.ranges[self.primary] = sel;
    }

    pub fn is_single(&self) -> bool {
        self.ranges.len() == 1
    }
}

impl Default for SelectionSet {
    fn default() -> Self {
        Self::single(Selection::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    #[test]
    fn collapsed_selection() {
        let rope = Rope::from_str("hello\nworld");
        let s = Selection::collapsed(3, &rope);
        assert!(s.is_empty());
        assert_eq!(s.start(), 3);
        assert_eq!(s.end(), 3);
    }

    #[test]
    fn selection_range() {
        let s = Selection { anchor: 5, head: 2, goal_col: 0 };
        assert_eq!(s.start(), 2);
        assert_eq!(s.end(), 5);
        assert_eq!(s.range(), 2..5);
        assert!(!s.is_empty());
    }

    #[test]
    fn selection_set_default_is_single() {
        let ss = SelectionSet::default();
        assert!(ss.is_single());
        assert_eq!(ss.primary, 0);
        assert!(ss.primary().is_empty());
    }
}
