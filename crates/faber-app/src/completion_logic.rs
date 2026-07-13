//! Pure helpers for LSP completion — no gpui dependency.
//! Tested in isolation; called from editor_view.rs.

use faber_core::movement::default_word_classifier;
use faber_lsp::completion::ParsedCompletion;
use ropey::Rope;

/// Compute the word fragment from the start of the current word up to `caret`
/// (char offset into `rope`). Returns `(word_start_char_offset, query_string)`.
///
/// Returns `None` when the caret is at the very start of a word (nothing typed yet
/// after a trigger char like `.`) or immediately after a non-word char with no
/// preceding identifier chars on the same logical prefix.
pub fn compute_word_prefix(rope: &Rope, caret: usize) -> Option<(usize, String)> {
    if caret == 0 {
        return None;
    }
    // Walk left from caret to find the word boundary.
    let mut start = caret;
    while start > 0 {
        let c = rope.char(start - 1);
        if !default_word_classifier(c) {
            break;
        }
        start -= 1;
    }
    if start == caret {
        // Caret is immediately after a non-word char — no prefix.
        return None;
    }
    let query: String = rope.slice(start..caret).chars().collect();
    Some((start, query))
}

// ── Fuzzy filtering ───────────────────────────────────────────────────────────

/// A scored match result.
#[derive(Debug, Clone)]
pub struct FuzzyMatch {
    pub item_ix: usize,
    /// Higher is better.
    pub score: i32,
    /// True if the query matches at the start of the match_text (rank higher).
    pub word_start: bool,
    /// Indices of matched characters within the item label, for bold highlighting.
    pub positions: Vec<u32>,
}

/// Filter `items` against `query` using subsequence fuzzy matching (Zed-style):
/// - Each char of `query` must appear in order (not necessarily contiguous) in `match_text`.
/// - Smart-case: case-insensitive unless `query` contains an uppercase letter.
/// - Scoring: prefix/word-start > consecutive > camelCase boundary > elsewhere.
/// - Two-tier sort: word-start matches first, then score desc, then sort_text/label.
pub fn fuzzy_filter(items: &[ParsedCompletion], query: &str) -> Vec<FuzzyMatch> {
    if query.is_empty() {
        // No query: return all items in their original order.
        return items
            .iter()
            .enumerate()
            .map(|(i, _)| FuzzyMatch {
                item_ix: i,
                score: 0,
                word_start: false,
                positions: vec![],
            })
            .collect();
    }

    let case_sensitive = query.chars().any(|c| c.is_uppercase());

    let mut matches: Vec<FuzzyMatch> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let text = item.match_text();
            subsequence_match(text, query, case_sensitive).map(|(score, word_start, positions)| {
                FuzzyMatch {
                    item_ix: i,
                    score,
                    word_start,
                    positions,
                }
            })
        })
        .collect();

    // Three-tier sort (Zed parity): exact match > word-start > score > sort_text/label.
    matches.sort_by(|a, b| {
        let exact_a = items[a.item_ix].match_text() == query;
        let exact_b = items[b.item_ix].match_text() == query;
        exact_b
            .cmp(&exact_a)
            .then_with(|| b.word_start.cmp(&a.word_start))
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| {
                let ta = items[a.item_ix]
                    .sort_text
                    .as_deref()
                    .unwrap_or(&items[a.item_ix].label);
                let tb = items[b.item_ix]
                    .sort_text
                    .as_deref()
                    .unwrap_or(&items[b.item_ix].label);
                ta.cmp(tb)
            })
    });
    matches
}

/// Try to match all chars of `query` as a subsequence of `text`.
/// Returns `Some((score, word_start, positions))` on success, `None` if no match.
///
/// Scoring:
/// - Base 100 if query chars start at position 0 (prefix).
/// - +20 per consecutive matched character run.
/// - +10 per match at a word boundary (after `_`, `-`, `.`, or uppercase after lowercase).
/// - +5 per exact-case character match (when case-insensitive mode).
fn subsequence_match(
    text: &str,
    query: &str,
    case_sensitive: bool,
) -> Option<(i32, bool, Vec<u32>)> {
    let text_chars: Vec<char> = text.chars().collect();
    let query_chars: Vec<char> = query.chars().collect();
    let n = text_chars.len();
    let m = query_chars.len();
    if m == 0 || m > n {
        return None;
    }

    // Greedy left-to-right subsequence scan.
    let mut positions: Vec<u32> = Vec::with_capacity(m);
    let mut qi = 0;
    for (ti, &tc) in text_chars.iter().enumerate() {
        if qi >= m {
            break;
        }
        let qc = query_chars[qi];
        let matches_char = if case_sensitive {
            tc == qc
        } else {
            tc.to_lowercase().next() == qc.to_lowercase().next()
        };
        if matches_char {
            positions.push(ti as u32);
            qi += 1;
        }
    }
    if qi < m {
        return None; // not all query chars matched
    }

    // Score the matched positions.
    let mut score: i32 = 0;
    let word_start = positions[0] == 0;

    // Prefix bonus.
    if word_start {
        score += 100;
    }

    let mut prev_pos: Option<u32> = None;
    for &pos in &positions {
        let p = pos as usize;
        // Consecutive run bonus.
        if prev_pos.map_or(false, |pp| pp + 1 == pos) {
            score += 20;
        }
        // Word-boundary bonus.
        if p == 0 {
            score += 10;
        } else {
            let prev = text_chars[p - 1];
            let cur = text_chars[p];
            // After separator or camelCase transition.
            let after_sep = matches!(prev, '_' | '-' | '.' | ':' | '/' | '(');
            let camel = prev.is_lowercase() && cur.is_uppercase();
            if after_sep || camel {
                score += 10;
            }
        }
        // Exact-case bonus (only meaningful in case-insensitive mode).
        if !case_sensitive
            && text_chars[p] == query_chars[positions.iter().position(|&x| x == pos).unwrap()]
        {
            score += 5;
        }
        prev_pos = Some(pos);
    }

    Some((score, word_start, positions))
}

// ── Re-request decision ───────────────────────────────────────────────────────

/// True = use client-side local fuzzy re-filter; false = must re-request server.
///
/// Mirrors Zed: local filter is safe when `isIncomplete=false`, the new query
/// is a prefix extension of the initial query, and the completion anchor has
/// not moved (same word start position).
pub fn should_refilter_locally(
    is_incomplete: bool,
    initial_query: &str,
    new_query: &str,
    same_anchor: bool,
) -> bool {
    if is_incomplete {
        return false;
    }
    if !same_anchor {
        return false;
    }
    new_query.starts_with(initial_query)
}

// ── Empty-filter decision ─────────────────────────────────────────────────────

/// What to do when a filter produces zero matches.
#[derive(Debug, PartialEq)]
pub enum EmptyFilterAction {
    /// Re-request the server at the current caret (response was stale / incomplete).
    Rerequest,
    /// Close the menu (complete list had no matches, or query == initial_query).
    Dismiss,
}

/// Decide how to handle an empty filter result.
///
/// Re-request the server whenever the query has advanced (Zed parity — avoids
/// permanent dismissal when a bounded `isIncomplete=false` list has no local match
/// for the 2nd+ character). Only dismiss when the query hasn't changed, meaning the
/// server genuinely returned nothing for this exact query.
pub fn resolve_empty_filter(
    _is_incomplete: bool,
    initial_query: &str,
    current_query: &str,
) -> EmptyFilterAction {
    if current_query != initial_query {
        EmptyFilterAction::Rerequest
    } else {
        EmptyFilterAction::Dismiss
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use faber_lsp::completion::ParsedCompletion;

    fn make_items(labels: &[&str]) -> Vec<ParsedCompletion> {
        labels
            .iter()
            .map(|&l| ParsedCompletion {
                label: l.to_owned(),
                kind: None,
                detail: None,
                sort_text: None,
                filter_text: None,
                insert_text: None,
                text_edit: None,
                additional_text_edits: vec![],
                is_snippet: false,
                documentation: None,
                data: serde_json::Value::Null,
            })
            .collect()
    }

    #[test]
    fn word_prefix_identifier() {
        let rope = Rope::from_str("let foo = bar");
        // caret at end of "bar" (char offset 13)
        let (start, q) = compute_word_prefix(&rope, 13).unwrap();
        assert_eq!(q, "bar");
        assert_eq!(start, 10);
    }

    #[test]
    fn word_prefix_after_dot() {
        let rope = Rope::from_str("foo.");
        // caret right after '.' — no word prefix
        assert!(compute_word_prefix(&rope, 4).is_none());
    }

    #[test]
    fn word_prefix_partial() {
        let rope = Rope::from_str("std::io::Wr");
        let (start, q) = compute_word_prefix(&rope, 11).unwrap();
        assert_eq!(q, "Wr");
        assert_eq!(start, 9);
    }

    #[test]
    fn fuzzy_filter_exact_match_first() {
        // Exact match must outrank word-start and higher-score subsequence hits.
        let items = make_items(&["print", "println", "eprintln"]);
        let matches = fuzzy_filter(&items, "print");
        let labels: Vec<&str> = matches
            .iter()
            .map(|m| items[m.item_ix].label.as_str())
            .collect();
        // "print" (exact) must come before "println" (word-start prefix).
        assert_eq!(labels[0], "print");
    }

    #[test]
    fn fuzzy_filter_word_start_first() {
        let items = make_items(&["println", "eprintln", "print"]);
        let matches = fuzzy_filter(&items, "print");
        // "println" and "print" start with "print" → ranked before "eprintln"
        let labels: Vec<&str> = matches
            .iter()
            .map(|m| items[m.item_ix].label.as_str())
            .collect();
        assert!(
            labels.iter().position(|&l| l == "println").unwrap()
                < labels.iter().position(|&l| l == "eprintln").unwrap()
        );
        assert!(
            labels.iter().position(|&l| l == "print").unwrap()
                < labels.iter().position(|&l| l == "eprintln").unwrap()
        );
    }

    #[test]
    fn fuzzy_filter_subsequence() {
        // "println" matches "pln" as a subsequence
        let items = make_items(&["println", "eprintln", "xfoo"]);
        let matches = fuzzy_filter(&items, "pln");
        let labels: Vec<&str> = matches
            .iter()
            .map(|m| items[m.item_ix].label.as_str())
            .collect();
        assert!(labels.contains(&"println"));
        assert!(labels.contains(&"eprintln"));
        assert!(!labels.contains(&"xfoo"));
    }

    #[test]
    fn fuzzy_filter_smart_case_sensitive() {
        let items = make_items(&["FooBar", "foobar", "Foo"]);
        // Uppercase query → case-sensitive
        let matches = fuzzy_filter(&items, "Foo");
        let labels: Vec<&str> = matches
            .iter()
            .map(|m| items[m.item_ix].label.as_str())
            .collect();
        // "foobar" should not match (case-sensitive)
        assert!(!labels.contains(&"foobar"));
        assert!(labels.contains(&"FooBar"));
        assert!(labels.contains(&"Foo"));
    }

    #[test]
    fn fuzzy_filter_case_insensitive() {
        let items = make_items(&["FooBar", "foobar", "Foo"]);
        // Lowercase query → case-insensitive
        let matches = fuzzy_filter(&items, "foo");
        assert_eq!(matches.len(), 3);
    }

    #[test]
    fn fuzzy_filter_empty_query_returns_all() {
        let items = make_items(&["a", "b", "c"]);
        assert_eq!(fuzzy_filter(&items, "").len(), 3);
    }

    #[test]
    fn fuzzy_filter_positions_non_empty() {
        let items = make_items(&["println"]);
        let matches = fuzzy_filter(&items, "prl");
        assert!(!matches[0].positions.is_empty());
        // 'p' at 0, 'r' at 2 (p-r-i-n-t-l-n: p=0,r=2), 'l' at 5
        assert_eq!(matches[0].positions[0], 0);
    }

    #[test]
    fn fuzzy_filter_no_match_returns_empty() {
        let items = make_items(&["abc"]);
        assert!(fuzzy_filter(&items, "xyz").is_empty());
    }

    #[test]
    fn refilter_locally_when_prefix_extension() {
        assert!(should_refilter_locally(false, "pri", "prin", true));
    }

    #[test]
    fn refilter_remotely_when_incomplete() {
        assert!(!should_refilter_locally(true, "pri", "prin", true));
    }

    #[test]
    fn refilter_remotely_when_anchor_moved() {
        assert!(!should_refilter_locally(false, "pri", "prin", false));
    }

    #[test]
    fn refilter_remotely_when_not_prefix() {
        // "pra" is not a prefix extension of "pri"
        assert!(!should_refilter_locally(false, "pri", "pra", true));
    }

    #[test]
    fn empty_filter_incomplete_advanced_query_rerequests() {
        assert_eq!(
            resolve_empty_filter(true, "p", "pr"),
            EmptyFilterAction::Rerequest
        );
    }

    #[test]
    fn empty_filter_incomplete_same_query_dismisses() {
        assert_eq!(
            resolve_empty_filter(true, "pr", "pr"),
            EmptyFilterAction::Dismiss
        );
    }

    #[test]
    fn empty_filter_complete_rerequests_when_query_advanced() {
        // Even for isIncomplete=false, a stale bounded list that has no local match
        // for the 2nd character should trigger a re-request rather than dismissal.
        assert_eq!(
            resolve_empty_filter(false, "p", "pr"),
            EmptyFilterAction::Rerequest
        );
    }

    #[test]
    fn empty_filter_complete_dismisses_when_query_unchanged() {
        assert_eq!(
            resolve_empty_filter(false, "pr", "pr"),
            EmptyFilterAction::Dismiss
        );
    }
}
