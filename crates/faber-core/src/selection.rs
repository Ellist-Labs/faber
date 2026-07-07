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
    ranges: Vec<Selection>,
    primary: usize,
}

impl SelectionSet {
    pub fn single(sel: Selection) -> Self {
        Self { ranges: vec![sel], primary: 0 }
    }

    /// Read-only view of the ranges (sorted, non-overlapping after `normalize`).
    pub fn ranges(&self) -> &[Selection] {
        &self.ranges
    }

    /// Index of the primary selection within `ranges`.
    pub fn primary_index(&self) -> usize {
        self.primary
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

    /// Append a selection and re-establish the sorted/non-overlapping invariant.
    pub fn push(&mut self, sel: Selection) -> &mut Self {
        self.ranges.push(sel);
        self.normalize();
        self
    }

    /// Sort ranges by start offset and merge overlapping ones, keeping the
    /// primary index pointing at the (possibly merged) range it belonged to.
    pub fn normalize(&mut self) {
        if self.ranges.len() <= 1 {
            return;
        }

        let primary_sel = self.ranges[self.primary];

        let mut sorted = self.ranges.clone();
        sorted.sort_by_key(|s| (s.start(), s.end()));

        let mut merged: Vec<Selection> = Vec::with_capacity(sorted.len());
        let mut primary_merged = 0usize;
        for sel in sorted {
            match merged.last_mut() {
                Some(last) if sel.start() <= last.end() => {
                    // Overlap (or touch) — extend the previous range's end.
                    if sel.end() > last.end() {
                        last.anchor = last.start();
                        last.head = sel.end();
                        last.goal_col = sel.goal_col;
                    }
                }
                _ => merged.push(sel),
            }
            if sel == primary_sel {
                primary_merged = merged.len() - 1;
            }
        }

        self.primary = primary_merged.min(merged.len().saturating_sub(1));
        self.ranges = merged;
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
        assert_eq!(ss.primary_index(), 0);
        assert!(ss.primary().is_empty());
    }

    #[test]
    fn normalize_sorts_and_merges() {
        let mut set = SelectionSet::single(Selection { anchor: 10, head: 10, goal_col: 0 });
        set.push(Selection { anchor: 2, head: 5, goal_col: 0 });
        set.push(Selection { anchor: 3, head: 7, goal_col: 0 }); // overlaps with previous
        // After normalize: should be sorted and [2..7] merged, then [10]
        assert_eq!(set.ranges().len(), 2);
        assert_eq!(set.ranges()[0].start(), 2);
        assert_eq!(set.ranges()[0].end(), 7);
    }

    #[test]
    fn normalize_preserves_single() {
        let set = SelectionSet::single(Selection::default());
        assert_eq!(set.ranges().len(), 1);
    }
}
