//! Behavior-level integration tests for the editing engine (Document + History + save).
//! Drive the stable public API, not internals — these tests survive refactors.

use faber_editor::{Transaction, buffer::Document, edit_history::History, save::save};
use tempfile::NamedTempFile;

fn plain(s: &str) -> Document {
    Document::from_str(s, None)
}

// ── Document basic mutations ────────────────────────────────────────────────

#[test]
fn insert_then_delete_roundtrip() {
    let mut doc = plain("hello");
    doc.insert(5, " world");
    assert_eq!(doc.rope.to_string(), "hello world");
    doc.delete(5..11);
    assert_eq!(doc.rope.to_string(), "hello");
}

#[test]
fn dirty_flag_tracks_edits() {
    let mut doc = plain("initial");
    assert!(!doc.is_dirty());
    doc.insert(7, " text");
    assert!(doc.is_dirty());
    doc.mark_saved();
    assert!(!doc.is_dirty());
    doc.delete(0..7);
    assert!(doc.is_dirty());
}

#[test]
fn restoring_original_content_clears_dirty() {
    // dirty is derived from rope != saved_rope, not a flag —
    // undoing all changes to the original text makes it clean.
    let mut doc = plain("abc");
    doc.insert(3, "d");
    assert!(doc.is_dirty());
    doc.delete(3..4);
    assert_eq!(doc.rope.to_string(), "abc");
    assert!(
        !doc.is_dirty(),
        "rope matches saved_rope — should not be dirty"
    );
}

// ── History (undo / redo) ───────────────────────────────────────────────────

#[test]
fn undo_single_insert() {
    let mut doc = plain("hi");
    let mut h = History::new();
    let inv = doc.insert(2, " there");
    h.push_insert(inv);
    h.commit();
    h.undo(&mut doc);
    assert_eq!(doc.rope.to_string(), "hi");
}

#[test]
fn redo_after_undo() {
    let mut doc = plain("x");
    let mut h = History::new();
    let inv = doc.insert(1, "yz");
    h.push_insert(inv);
    h.commit();
    h.undo(&mut doc);
    assert_eq!(doc.rope.to_string(), "x");
    h.redo(&mut doc);
    assert_eq!(doc.rope.to_string(), "xyz");
}

#[test]
fn redo_cleared_when_edit_follows_undo() {
    let mut doc = plain("a");
    let mut h = History::new();
    let inv = doc.insert(1, "b");
    h.push_insert(inv);
    h.commit();
    h.undo(&mut doc);
    // New edit after undo must clear the redo stack.
    let inv2 = doc.insert(1, "c");
    h.push_change(inv2);
    assert!(h.redo(&mut doc).is_none(), "redo stack should be empty");
    assert_eq!(doc.rope.to_string(), "ac");
}

#[test]
fn coalesced_inserts_undo_as_one_unit() {
    let mut doc = plain("");
    let mut h = History::new();
    for (i, ch) in "hello".chars().enumerate() {
        let inv = doc.insert(i, &ch.to_string());
        h.push_insert(inv);
    }
    h.commit();
    assert_eq!(doc.rope.to_string(), "hello");
    h.undo(&mut doc);
    assert_eq!(
        doc.rope.to_string(),
        "",
        "all coalesced inserts undone as one"
    );
}

#[test]
fn commit_separates_undo_groups() {
    let mut doc = plain("");
    let mut h = History::new();
    let inv1 = doc.insert(0, "a");
    h.push_insert(inv1);
    h.commit();
    let inv2 = doc.insert(1, "b");
    h.push_insert(inv2);
    h.commit();
    assert_eq!(doc.rope.to_string(), "ab");
    h.undo(&mut doc);
    assert_eq!(doc.rope.to_string(), "a");
    h.undo(&mut doc);
    assert_eq!(doc.rope.to_string(), "");
    // Redo restores both groups.
    h.redo(&mut doc);
    h.redo(&mut doc);
    assert_eq!(doc.rope.to_string(), "ab");
}

#[test]
fn undo_with_empty_stack_is_noop() {
    let mut doc = plain("foo");
    let mut h = History::new();
    assert!(h.undo(&mut doc).is_none());
    assert_eq!(doc.rope.to_string(), "foo");
}

#[test]
fn undo_delete_then_redo() {
    let mut doc = plain("hello world");
    let mut h = History::new();
    let inv = doc.delete(5..11);
    h.push_change(inv);
    assert_eq!(doc.rope.to_string(), "hello");
    h.undo(&mut doc);
    assert_eq!(doc.rope.to_string(), "hello world");
    h.redo(&mut doc);
    assert_eq!(doc.rope.to_string(), "hello");
}

// ── Transaction API ─────────────────────────────────────────────────────────

#[test]
fn transaction_replace_range() {
    let mut doc = plain("foo bar baz");
    let tx = Transaction::replace(&doc.rope, 4..7, "world".to_string());
    doc.apply(tx);
    assert_eq!(doc.rope.to_string(), "foo world baz");
}

#[test]
fn transaction_insert_via_apply() {
    let mut doc = plain("ab");
    let tx = Transaction::insert(&doc.rope, 1, "X");
    doc.apply(tx);
    assert_eq!(doc.rope.to_string(), "aXb");
}

// ── Round-trip through save ─────────────────────────────────────────────────

#[test]
fn edit_then_save_then_reload() {
    let mut doc = plain("initial content");
    doc.insert(7, " changed");
    let tmp = NamedTempFile::new().unwrap();
    save(&doc.rope, tmp.path()).unwrap();
    let reloaded = std::fs::read_to_string(tmp.path()).unwrap();
    assert_eq!(reloaded, doc.rope.to_string());
}

#[test]
fn save_then_mark_saved_clears_dirty() {
    let mut doc = plain("text");
    doc.insert(4, "!");
    assert!(doc.is_dirty());
    let tmp = NamedTempFile::new().unwrap();
    save(&doc.rope, tmp.path()).unwrap();
    doc.mark_saved();
    assert!(!doc.is_dirty());
}
