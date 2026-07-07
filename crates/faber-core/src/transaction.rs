//! `ChangeSet` and `Transaction` — the single edit primitive for Faber.
//!
//! A `ChangeSet` covers the *entire* document as a sequence of:
//!   `Retain(n)` — keep n chars unchanged,
//!   `Delete(n)` — remove n chars,
//!   `Insert(s)` — insert string s at the current position.
//!
//! Invariant: sum of Retain + Delete lengths equals `len_before`.
//! Adjacent ops of the same kind are merged eagerly.

use ropey::Rope;

use crate::anchor::Bias;
use crate::selection::SelectionSet;

// ── Operation ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    Retain(usize),
    Delete(usize),
    Insert(String),
}

// ── ChangeSet ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeSet {
    pub(crate) changes: Vec<Operation>,
    /// Char length of the document this ChangeSet was built for.
    pub len_before: usize,
    /// Char length of the document after applying this ChangeSet.
    pub len_after: usize,
}

impl ChangeSet {
    /// Identity change: retain everything.
    pub fn identity(len: usize) -> Self {
        let changes = if len == 0 { vec![] } else { vec![Operation::Retain(len)] };
        Self { changes, len_before: len, len_after: len }
    }

    /// Build from a sorted, non-overlapping list of `(start, end, replacement)` char ranges.
    /// Ranges outside `doc_len` are clamped/skipped.
    pub fn from_changes(
        doc_len: usize,
        edits: impl IntoIterator<Item = (usize, usize, String)>,
    ) -> Self {
        let mut ops: Vec<Operation> = Vec::new();
        let mut cursor = 0usize;
        let mut len_after = 0usize;

        let edits: Vec<_> = edits.into_iter().collect();

        for (start, end, text) in &edits {
            let start = (*start).min(doc_len);
            let end = (*end).min(doc_len);
            if start > cursor {
                push_retain(&mut ops, start - cursor);
                len_after += start - cursor;
            }
            if end > start {
                push_delete(&mut ops, end - start);
            }
            if !text.is_empty() {
                let n = text.chars().count();
                push_insert(&mut ops, text.clone());
                len_after += n;
            }
            cursor = end;
        }

        if cursor < doc_len {
            push_retain(&mut ops, doc_len - cursor);
            len_after += doc_len - cursor;
        }

        Self { changes: ops, len_before: doc_len, len_after }
    }

    // ── application ──────────────────────────────────────────────────────────

    /// Apply this ChangeSet to a Rope in place.
    pub fn apply(&self, rope: &mut Rope) {
        let mut pos = 0usize;
        for op in &self.changes {
            match op {
                Operation::Retain(n) => pos += n,
                Operation::Delete(n) => rope.remove(pos..pos + n),
                Operation::Insert(s) => {
                    rope.insert(pos, s);
                    pos += s.chars().count();
                }
            }
        }
    }

    // ── inversion ────────────────────────────────────────────────────────────

    /// Compute the inverse ChangeSet that, when applied to the post-edit rope,
    /// restores the original. Requires the original rope to recover deleted text.
    pub fn invert(&self, original: &Rope) -> ChangeSet {
        let mut inv_ops: Vec<Operation> = Vec::new();
        let mut pos = 0usize;

        for op in &self.changes {
            match op {
                Operation::Retain(n) => {
                    push_retain(&mut inv_ops, *n);
                    pos += n;
                }
                Operation::Delete(n) => {
                    let text: String = original.slice(pos..pos + n).to_string();
                    push_insert(&mut inv_ops, text);
                    pos += n;
                }
                Operation::Insert(s) => {
                    push_delete(&mut inv_ops, s.chars().count());
                }
            }
        }

        ChangeSet { changes: inv_ops, len_before: self.len_after, len_after: self.len_before }
    }

    // ── position mapping ─────────────────────────────────────────────────────

    /// Map a char offset through this ChangeSet.
    ///
    /// `Bias::Left`  — position sticks to the left of any insertion at `pos`
    ///                 (good for diagnostic start-of-range, bookmark anchors).
    /// `Bias::Right` — position moves with text inserted at `pos`
    ///                 (good for cursors that keep up with typing).
    pub fn map_pos(&self, pos: usize, bias: Bias) -> usize {
        let mut old = 0usize;
        let mut new = 0usize;

        for op in &self.changes {
            match op {
                Operation::Retain(n) => {
                    if old + n > pos {
                        return new + (pos - old);
                    }
                    old += n;
                    new += n;
                }
                Operation::Delete(n) => {
                    if old + n > pos {
                        // pos falls inside a deleted range → collapse to delete start
                        return new;
                    }
                    old += n;
                }
                Operation::Insert(s) => {
                    let n = s.chars().count();
                    if bias == Bias::Left {
                        // Left-bias: don't advance through insertions at pos.
                        // If old == pos this insertion is "after" us, so stay.
                        if old == pos {
                            return new;
                        }
                    }
                    new += n;
                }
            }
        }

        // pos >= doc end → map to end of new doc
        new + pos.saturating_sub(old)
    }

    // ── changes iterator ─────────────────────────────────────────────────────

    /// Iterate over logical change units. Callers in faber-editor convert these
    /// to tree-sitter `InputEdit`s or LSP `TextEdit`s without pulling tree-sitter
    /// into faber-core.
    pub fn iter_changes(&self) -> ChangesIter<'_> {
        ChangesIter { ops: self.changes.iter(), old_pos: 0, new_pos: 0 }
    }

    // ── composition ──────────────────────────────────────────────────────────

    /// Compose two ChangeSets: `self` applied first, then `other`.
    /// The resulting ChangeSet is equivalent to applying both in sequence.
    ///
    /// Precondition: `self.len_after == other.len_before`.
    ///
    /// Algorithm (sequential OT composition):
    ///   1. b's Inserts always emit first — they're new text in doc1.
    ///   2. a's Deletes emit next — they remove doc0 chars that b never sees.
    ///   3. a's Inserts are consumed char-by-char by b's Retains/Deletes.
    ///   4. a's Retains and b's Retains/Deletes align normally.
    pub fn compose(self, other: ChangeSet) -> ChangeSet {
        debug_assert_eq!(self.len_after, other.len_before);

        let len_before = self.len_before;
        let len_after = other.len_after;
        let mut result: Vec<Operation> = Vec::new();

        let mut a_iter = self.changes.into_iter();
        let mut b_iter = other.changes.into_iter();
        let mut a_cur: Option<Operation> = a_iter.next();
        let mut b_cur: Option<Operation> = b_iter.next();

        loop {
            match (a_cur, b_cur) {
                (None, None) => break,

                (None, Some(b)) => {
                    flush_op(&mut result, b);
                    a_cur = None;
                    b_cur = b_iter.next();
                    continue;
                }
                (Some(a), None) => {
                    flush_op(&mut result, a);
                    a_cur = a_iter.next();
                    b_cur = None;
                    continue;
                }

                // Rule 1: b inserts first (regardless of a).
                (Some(a), Some(Operation::Insert(s))) => {
                    push_insert(&mut result, s);
                    a_cur = Some(a);
                    b_cur = b_iter.next();
                }

                // Rule 2: a deletes (b can't see these doc0 chars; b is non-Insert here).
                (Some(Operation::Delete(n)), Some(b)) => {
                    push_delete(&mut result, n);
                    a_cur = a_iter.next();
                    b_cur = Some(b);
                }

                // Rule 3a: a inserts, b retains those chars → keep insert.
                (Some(Operation::Insert(s)), Some(Operation::Retain(m))) => {
                    let n = s.chars().count();
                    if n <= m {
                        push_insert(&mut result, s);
                        a_cur = a_iter.next();
                        b_cur = if m > n { Some(Operation::Retain(m - n)) } else { b_iter.next() };
                    } else {
                        // b retains only part of a's insert
                        let (kept, rest) = str_split_at_char(s, m);
                        push_insert(&mut result, kept);
                        a_cur = Some(Operation::Insert(rest));
                        b_cur = b_iter.next();
                    }
                }

                // Rule 3b: a inserts, b deletes those chars → cancel.
                (Some(Operation::Insert(s)), Some(Operation::Delete(m))) => {
                    let n = s.chars().count();
                    if n <= m {
                        a_cur = a_iter.next();
                        b_cur = if m > n { Some(Operation::Delete(m - n)) } else { b_iter.next() };
                    } else {
                        let rest = s.chars().skip(m).collect::<String>();
                        a_cur = Some(Operation::Insert(rest));
                        b_cur = b_iter.next();
                    }
                }

                // Rule 4: both retain.
                (Some(Operation::Retain(ra)), Some(Operation::Retain(rb))) => {
                    let n = ra.min(rb);
                    push_retain(&mut result, n);
                    a_cur = if ra > n { Some(Operation::Retain(ra - n)) } else { a_iter.next() };
                    b_cur = if rb > n { Some(Operation::Retain(rb - n)) } else { b_iter.next() };
                }

                // Rule 4: a retains, b deletes.
                (Some(Operation::Retain(ra)), Some(Operation::Delete(db))) => {
                    let n = ra.min(db);
                    push_delete(&mut result, n);
                    a_cur = if ra > n { Some(Operation::Retain(ra - n)) } else { a_iter.next() };
                    b_cur = if db > n { Some(Operation::Delete(db - n)) } else { b_iter.next() };
                }
            }
        }

        ChangeSet { changes: result, len_before, len_after }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn push_retain(ops: &mut Vec<Operation>, n: usize) {
    if n == 0 {
        return;
    }
    if let Some(Operation::Retain(last)) = ops.last_mut() {
        *last += n;
    } else {
        ops.push(Operation::Retain(n));
    }
}

fn push_delete(ops: &mut Vec<Operation>, n: usize) {
    if n == 0 {
        return;
    }
    if let Some(Operation::Delete(last)) = ops.last_mut() {
        *last += n;
    } else {
        ops.push(Operation::Delete(n));
    }
}

fn push_insert(ops: &mut Vec<Operation>, s: String) {
    if s.is_empty() {
        return;
    }
    if let Some(Operation::Insert(last)) = ops.last_mut() {
        last.push_str(&s);
    } else {
        ops.push(Operation::Insert(s));
    }
}

/// Split a String at a char offset; returns (prefix, suffix).
fn str_split_at_char(s: String, n: usize) -> (String, String) {
    let byte_idx = s.char_indices().nth(n).map(|(i, _)| i).unwrap_or(s.len());
    let suffix = s[byte_idx..].to_string();
    let mut prefix = s;
    prefix.truncate(byte_idx);
    (prefix, suffix)
}

fn flush_op(ops: &mut Vec<Operation>, op: Operation) {
    match op {
        Operation::Retain(n) => push_retain(ops, n),
        Operation::Delete(n) => push_delete(ops, n),
        Operation::Insert(s) => push_insert(ops, s),
    }
}

// ── ChangesIter ───────────────────────────────────────────────────────────────

/// A logical change unit: what range was replaced and with what text.
pub struct ChangeItem<'a> {
    /// Char range in the *pre-edit* document that was removed (may be empty for pure inserts).
    pub old_range: std::ops::Range<usize>,
    /// Replacement text (empty for pure deletes).
    pub new_text: &'a str,
    /// Char offset in the *post-edit* document where the replacement starts.
    pub new_start: usize,
}

pub struct ChangesIter<'a> {
    ops: std::slice::Iter<'a, Operation>,
    old_pos: usize,
    new_pos: usize,
}

impl<'a> Iterator for ChangesIter<'a> {
    type Item = ChangeItem<'a>;

    fn next(&mut self) -> Option<ChangeItem<'a>> {
        loop {
            match self.ops.next()? {
                Operation::Retain(n) => {
                    self.old_pos += n;
                    self.new_pos += n;
                }
                Operation::Delete(n) => {
                    let old_start = self.old_pos;
                    self.old_pos += n;
                    return Some(ChangeItem {
                        old_range: old_start..self.old_pos,
                        new_text: "",
                        new_start: self.new_pos,
                    });
                }
                Operation::Insert(s) => {
                    let old_pos = self.old_pos;
                    let new_start = self.new_pos;
                    self.new_pos += s.chars().count();
                    return Some(ChangeItem {
                        old_range: old_pos..old_pos,
                        new_text: s,
                        new_start,
                    });
                }
            }
        }
    }
}

// ── Transaction ───────────────────────────────────────────────────────────────

/// An edit together with the resulting selection state.
/// This is the currency passed between the editing engine and the view.
#[derive(Clone, Debug)]
pub struct Transaction {
    pub changes: ChangeSet,
    /// The selection state to apply after the changes (if provided).
    pub selection: Option<SelectionSet>,
}

impl Transaction {
    pub fn from_changeset(changes: ChangeSet) -> Self {
        Self { changes, selection: None }
    }

    pub fn with_selection(mut self, sel: SelectionSet) -> Self {
        self.selection = Some(sel);
        self
    }

    /// A single-range insert at `pos`.
    pub fn insert(rope: &Rope, pos: usize, text: impl Into<String>) -> Self {
        let text = text.into();
        let doc_len = rope.len_chars();
        let changes =
            ChangeSet::from_changes(doc_len, std::iter::once((pos, pos, text)));
        Self::from_changeset(changes)
    }

    /// A single-range delete of `range` (char offsets, exclusive end).
    pub fn delete(rope: &Rope, range: std::ops::Range<usize>) -> Self {
        let doc_len = rope.len_chars();
        let changes =
            ChangeSet::from_changes(doc_len, std::iter::once((range.start, range.end, String::new())));
        Self::from_changeset(changes)
    }

    /// A single replace: delete `range` and insert `text`.
    pub fn replace(rope: &Rope, range: std::ops::Range<usize>, text: impl Into<String>) -> Self {
        let doc_len = rope.len_chars();
        let changes =
            ChangeSet::from_changes(doc_len, std::iter::once((range.start, range.end, text.into())));
        Self::from_changeset(changes)
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn apply(src: &str, cs: &ChangeSet) -> String {
        let mut rope = Rope::from_str(src);
        cs.apply(&mut rope);
        rope.to_string()
    }

    // ── basic apply ──────────────────────────────────────────────────────────

    #[test]
    fn insert_at_start() {
        let cs = ChangeSet::from_changes(5, [( 0, 0, "XYZ".into())]);
        assert_eq!(apply("hello", &cs), "XYZhello");
    }

    #[test]
    fn insert_at_end() {
        let cs = ChangeSet::from_changes(5, [(5, 5, "!".into())]);
        assert_eq!(apply("hello", &cs), "hello!");
    }

    #[test]
    fn insert_in_middle() {
        let cs = ChangeSet::from_changes(5, [(2, 2, "XY".into())]);
        assert_eq!(apply("hello", &cs), "heXYllo");
    }

    #[test]
    fn delete_range() {
        let cs = ChangeSet::from_changes(5, [(1, 3, "".into())]);
        assert_eq!(apply("hello", &cs), "hlo");
    }

    #[test]
    fn replace_range() {
        let cs = ChangeSet::from_changes(5, [(1, 3, "EE".into())]);
        assert_eq!(apply("hello", &cs), "hEElo");
    }

    #[test]
    fn identity_is_noop() {
        let cs = ChangeSet::identity(5);
        assert_eq!(apply("hello", &cs), "hello");
    }

    // ── invert ───────────────────────────────────────────────────────────────

    #[test]
    fn invert_insert_restores() {
        let orig = Rope::from_str("hello");
        let cs = ChangeSet::from_changes(5, [(2, 2, "XY".into())]);
        let inv = cs.invert(&orig);
        let result = apply("heXYllo", &inv);
        assert_eq!(result, "hello");
    }

    #[test]
    fn invert_delete_restores() {
        let orig = Rope::from_str("hello");
        let cs = ChangeSet::from_changes(5, [(1, 3, "".into())]);
        let inv = cs.invert(&orig);
        let result = apply("hlo", &inv);
        assert_eq!(result, "hello");
    }

    #[test]
    fn apply_then_invert_is_identity() {
        let src = "fn main() { println!(\"hello\"); }";
        let orig = Rope::from_str(src);
        let cs = ChangeSet::from_changes(src.chars().count(), [(3, 7, "run".into())]);
        let inv = cs.invert(&orig);
        let mut rope = orig.clone();
        cs.apply(&mut rope);
        inv.apply(&mut rope);
        assert_eq!(rope.to_string(), src);
    }

    // ── map_pos ──────────────────────────────────────────────────────────────

    #[test]
    fn map_pos_retain_unchanged() {
        let cs = ChangeSet::identity(10);
        assert_eq!(cs.map_pos(4, Bias::Right), 4);
    }

    #[test]
    fn map_pos_insert_before_right_bias_advances() {
        // insert "AB" at pos 0 → everything shifts right by 2
        let cs = ChangeSet::from_changes(5, [(0, 0, "AB".into())]);
        // pos 0, Right bias → position keeps up with the insert
        assert_eq!(cs.map_pos(0, Bias::Right), 2);
        // pos 2 → 4
        assert_eq!(cs.map_pos(2, Bias::Right), 4);
    }

    #[test]
    fn map_pos_insert_before_left_bias_stays() {
        let cs = ChangeSet::from_changes(5, [(0, 0, "AB".into())]);
        // Left bias: stay before the inserted text
        assert_eq!(cs.map_pos(0, Bias::Left), 0);
    }

    #[test]
    fn map_pos_delete_collapses_range() {
        // delete chars 2..4 from "hello"
        let cs = ChangeSet::from_changes(5, [(2, 4, "".into())]);
        // pos 3 (inside deleted range) → collapse to 2
        assert_eq!(cs.map_pos(3, Bias::Right), 2);
        // pos 4 (after range) → 2
        assert_eq!(cs.map_pos(4, Bias::Right), 2);
        // pos 5 (end) → 3
        assert_eq!(cs.map_pos(5, Bias::Right), 3);
    }

    // ── compose ──────────────────────────────────────────────────────────────

    #[test]
    fn compose_two_inserts() {
        // insert "A" at 0, then insert "B" at 1 → "ABhello"
        let src = "hello";
        let n = src.chars().count();
        let a = ChangeSet::from_changes(n, [(0, 0, "A".into())]);
        let b_len = n + 1; // after A
        let b = ChangeSet::from_changes(b_len, [(1, 1, "B".into())]);
        let composed = a.compose(b);
        assert_eq!(apply(src, &composed), "ABhello");
    }

    // ── transaction helpers ───────────────────────────────────────────────────

    #[test]
    fn transaction_insert() {
        let rope = Rope::from_str("hello");
        let tx = Transaction::insert(&rope, 2, "XY");
        let mut r = rope.clone();
        tx.changes.apply(&mut r);
        assert_eq!(r.to_string(), "heXYllo");
    }

    #[test]
    fn transaction_delete() {
        let rope = Rope::from_str("hello");
        let tx = Transaction::delete(&rope, 1..3);
        let mut r = rope.clone();
        tx.changes.apply(&mut r);
        assert_eq!(r.to_string(), "hlo");
    }

    #[test]
    fn transaction_replace() {
        let rope = Rope::from_str("hello world");
        let tx = Transaction::replace(&rope, 6..11, "rust");
        let mut r = rope.clone();
        tx.changes.apply(&mut r);
        assert_eq!(r.to_string(), "hello rust");
    }
}

// ── proptest ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;
    use ropey::Rope;

    /// Clamp two raw indices into a valid (start, end) for a doc of length n.
    fn clamp_range(a: usize, b: usize, n: usize) -> (usize, usize) {
        let s = a.min(n).min(b.min(n));
        let e = a.min(n).max(b.min(n));
        (s, e)
    }

    proptest! {
        /// apply(invert(cs), post_rope) == original doc.
        #[test]
        fn invert_is_identity(
            doc in "[a-z \n]{1,80}",
            ra in 0usize..80, rb in 0usize..80,
            txt in "[a-z]{0,10}",
        ) {
            let n = doc.chars().count();
            if n == 0 { return Ok(()); }
            let (s, e) = clamp_range(ra, rb, n);
            let orig = Rope::from_str(&doc);
            let cs = ChangeSet::from_changes(n, [(s, e, txt)]);
            let inv = cs.invert(&orig);
            let mut rope = orig.clone();
            cs.apply(&mut rope);
            inv.apply(&mut rope);
            prop_assert_eq!(rope.to_string(), doc);
        }

        /// map_pos is monotone: a <= b ⟹ map_pos(a) <= map_pos(b).
        #[test]
        fn map_pos_monotone(
            doc in "[a-z]{1,40}",
            ra in 0usize..40, rb in 0usize..40,
            txt in "[a-z]{0,5}",
            bias_left in proptest::bool::ANY,
        ) {
            let n = doc.chars().count();
            if n == 0 { return Ok(()); }
            let (s, e) = clamp_range(ra, rb, n);
            let cs = ChangeSet::from_changes(n, [(s, e, txt)]);
            let bias = if bias_left { Bias::Left } else { Bias::Right };
            let mapped: Vec<usize> = (0..=n).map(|p| cs.map_pos(p, bias)).collect();
            for w in mapped.windows(2) {
                prop_assert!(w[0] <= w[1], "not monotone: {} > {}", w[0], w[1]);
            }
        }

        /// map_pos result is always in [0, len_after].
        #[test]
        fn map_pos_in_bounds(
            doc in "[a-z]{1,40}",
            ra in 0usize..40, rb in 0usize..40,
            txt in "[a-z]{0,5}",
        ) {
            let n = doc.chars().count();
            if n == 0 { return Ok(()); }
            let (s, e) = clamp_range(ra, rb, n);
            let cs = ChangeSet::from_changes(n, [(s, e, txt)]);
            for p in 0..=n {
                let m = cs.map_pos(p, Bias::Right);
                prop_assert!(m <= cs.len_after);
            }
        }

        /// compose(a, b) applied to src == applying a then b separately.
        #[test]
        fn compose_is_equivalent(
            doc in "[a-z]{1,40}",
            ra1 in 0usize..40, rb1 in 0usize..40, t1 in "[a-z]{0,5}",
            ra2 in 0usize..40, rb2 in 0usize..40, t2 in "[a-z]{0,5}",
        ) {
            let n = doc.chars().count();
            if n == 0 { return Ok(()); }
            let (s1, e1) = clamp_range(ra1, rb1, n);
            let a = ChangeSet::from_changes(n, [(s1, e1, t1)]);
            let n2 = a.len_after;
            let (s2, e2) = clamp_range(ra2, rb2, n2);
            let b = ChangeSet::from_changes(n2, [(s2, e2, t2)]);

            let mut rope_sep = Rope::from_str(&doc);
            a.clone().apply(&mut rope_sep);
            b.clone().apply(&mut rope_sep);

            let composed = a.compose(b);
            let mut rope_comp = Rope::from_str(&doc);
            composed.apply(&mut rope_comp);

            prop_assert_eq!(rope_sep.to_string(), rope_comp.to_string());
        }
    }
}
