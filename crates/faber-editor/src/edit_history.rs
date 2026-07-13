use faber_core::anchor::Bias;
use faber_core::transaction::{ChangeSet, Transaction};

use crate::buffer::Document;

#[derive(Default)]
pub struct History {
    /// Each entry is the INVERSE of what was applied — apply it to undo.
    undo_stack: Vec<ChangeSet>,
    /// Each entry is the forward change to RE-apply on redo.
    redo_stack: Vec<ChangeSet>,
    /// Accumulates consecutive inserts via compose (coalesced undo unit).
    pending: Option<ChangeSet>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Call after every `Document::insert` with the returned inverse ChangeSet.
    /// Consecutive inserts are coalesced via `ChangeSet::compose`.
    pub fn push_insert(&mut self, inverse: ChangeSet) {
        self.redo_stack.clear();
        if let Some(p) = self.pending.take() {
            // `p` is the accumulated inverse of earlier inserts; `inverse` is the
            // newest. Undo applies newest-first, so compose new ∘ old:
            // compose requires new.len_after == old.len_before.
            if inverse.len_after == p.len_before {
                self.pending = Some(inverse.compose(p));
            } else {
                self.undo_stack.push(p);
                self.pending = Some(inverse);
            }
        } else {
            self.pending = Some(inverse);
        }
    }

    /// Call after delete, replace-all, or any non-insert mutation.
    pub fn push_change(&mut self, inverse: ChangeSet) {
        self.commit();
        self.redo_stack.clear();
        self.undo_stack.push(inverse);
    }

    /// Flush pending coalesced inserts to the undo stack.
    pub fn commit(&mut self) {
        if let Some(p) = self.pending.take() {
            self.undo_stack.push(p);
        }
    }

    /// Undo the last change. Returns a cursor position hint.
    pub fn undo(&mut self, doc: &mut Document) -> Option<usize> {
        self.commit();
        let inverse = self.undo_stack.pop()?;
        debug_assert_eq!(
            inverse.len_before,
            doc.rope.len_chars(),
            "undo stack out of sync with document"
        );
        if inverse.len_before != doc.rope.len_chars() {
            return None;
        }
        // Capture the forward change (for redo) against the current rope
        // before we mutate it.
        let forward = inverse.invert(&doc.rope);
        // Cursor hint: where the end of the doc maps to after the undo.
        let cursor_hint = inverse.map_pos(doc.rope.len_chars(), Bias::Left);
        doc.apply(Transaction::from_changeset(inverse));
        self.redo_stack.push(forward);
        Some(cursor_hint.min(doc.rope.len_chars()))
    }

    /// Redo the last undone change. Returns a cursor position hint.
    pub fn redo(&mut self, doc: &mut Document) -> Option<usize> {
        let forward = self.redo_stack.pop()?;
        let inverse = forward.invert(&doc.rope);
        let cursor_hint = forward.map_pos(0, Bias::Right);
        doc.apply(Transaction::from_changeset(forward));
        self.undo_stack.push(inverse);
        Some(cursor_hint.min(doc.rope.len_chars()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_doc(s: &str) -> Document {
        Document::from_str(s, None)
    }

    #[test]
    fn single_insert_undo() {
        let mut doc = make_doc("hello");
        let mut hist = History::new();
        let inv = doc.insert(5, " world");
        hist.push_insert(inv);
        hist.commit();
        assert_eq!(doc.rope.to_string(), "hello world");
        hist.undo(&mut doc);
        assert_eq!(doc.rope.to_string(), "hello");
    }

    #[test]
    fn undo_redo_roundtrip() {
        let mut doc = make_doc("abc");
        let mut hist = History::new();
        let inv = doc.insert(3, "d");
        hist.push_insert(inv);
        hist.commit();
        hist.undo(&mut doc);
        assert_eq!(doc.rope.to_string(), "abc");
        hist.redo(&mut doc);
        assert_eq!(doc.rope.to_string(), "abcd");
    }

    #[test]
    fn redo_cleared_by_new_edit() {
        let mut doc = make_doc("abc");
        let mut hist = History::new();
        let inv = doc.insert(3, "d");
        hist.push_insert(inv);
        hist.commit();
        hist.undo(&mut doc);
        let inv2 = doc.insert(3, "X");
        hist.push_change(inv2);
        assert!(hist.redo(&mut doc).is_none());
    }

    #[test]
    fn delete_undo() {
        let mut doc = make_doc("hello world");
        let mut hist = History::new();
        let inv = doc.delete(5..11);
        hist.push_change(inv);
        assert_eq!(doc.rope.to_string(), "hello");
        hist.undo(&mut doc);
        assert_eq!(doc.rope.to_string(), "hello world");
    }

    #[test]
    fn coalesced_inserts_undo_together() {
        let mut doc = make_doc("");
        let mut hist = History::new();
        for (i, ch) in "abc".chars().enumerate() {
            let inv = doc.insert(i, &ch.to_string());
            hist.push_insert(inv);
        }
        hist.commit();
        assert_eq!(doc.rope.to_string(), "abc");
        hist.undo(&mut doc);
        assert_eq!(doc.rope.to_string(), "");
    }
}
