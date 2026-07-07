use crate::buffer::{Document, Edit};

/// A group of edits to undo/redo together.
struct Group {
    edits: Vec<Edit>,
}

pub struct History {
    undo_stack: Vec<Group>,
    redo_stack: Vec<Group>,
    /// Accumulates consecutive inserts for coalescing.
    pending: Option<Group>,
}

impl History {
    pub fn new() -> Self {
        Self { undo_stack: Vec::new(), redo_stack: Vec::new(), pending: None }
    }

    /// Push a single-char insert for coalescing.
    /// `end_char` is the char offset right after the inserted text in the current doc.
    pub fn push_insert(&mut self, edit: Edit, _end_char: usize) {
        self.redo_stack.clear();
        // Coalesce if the pending group ends exactly where this insert starts.
        if let Some(ref mut group) = self.pending {
            if let Some(last) = group.edits.last() {
                let last_end = last.char_range.start + last.inserted.chars().count();
                if last_end == edit.char_range.start && !edit.inserted.contains('\n') {
                    group.edits.push(edit);
                    return;
                }
            }
        }
        // Start a new pending group.
        self.commit();
        self.pending = Some(Group { edits: vec![edit] });
    }

    /// Push a delete/replace — always commits pending first.
    pub fn push_other(&mut self, edit: Edit) {
        self.commit();
        self.redo_stack.clear();
        self.undo_stack.push(Group { edits: vec![edit] });
    }

    /// Commit the pending coalesced group to the undo stack.
    pub fn commit(&mut self) {
        if let Some(group) = self.pending.take() {
            if !group.edits.is_empty() {
                self.undo_stack.push(group);
            }
        }
    }

    /// Undo the last group. Returns the cursor char position after undoing.
    pub fn undo(&mut self, doc: &mut Document) -> Option<usize> {
        self.commit();
        let group = self.undo_stack.pop()?;
        let mut cursor = 0;
        // Apply inverse edits in reverse order.
        for edit in group.edits.iter().rev() {
            let inv = edit.invert();
            cursor = if inv.inserted.is_empty() {
                let start = inv.char_range.start;
                doc.delete(inv.char_range.clone());
                start
            } else {
                let start = inv.char_range.start;
                doc.insert(start, &inv.inserted.clone());
                start + inv.inserted.chars().count()
            };
        }
        self.redo_stack.push(group);
        Some(cursor)
    }

    /// Redo the last undone group. Returns the cursor char position after redoing.
    pub fn redo(&mut self, doc: &mut Document) -> Option<usize> {
        let group = self.redo_stack.pop()?;
        let mut cursor = 0;
        for edit in &group.edits {
            cursor = if edit.inserted.is_empty() {
                let start = edit.char_range.start;
                doc.delete(edit.char_range.clone());
                start
            } else {
                let start = edit.char_range.start;
                doc.insert(start, &edit.inserted.clone());
                start + edit.inserted.chars().count()
            };
        }
        self.undo_stack.push(group);
        Some(cursor)
    }
}
