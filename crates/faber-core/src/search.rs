use ropey::Rope;
use std::ops::Range;

/// A literal-substring search query.
pub struct Query {
    pub text: String,
    pub case_sensitive: bool,
}

impl Query {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), case_sensitive: false }
    }

    pub fn case_sensitive(mut self, yes: bool) -> Self {
        self.case_sensitive = yes;
        self
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// All non-overlapping matches, returned as char-offset ranges.
    pub fn all_matches(&self, rope: &Rope) -> Vec<Range<usize>> {
        if self.is_empty() {
            return Vec::new();
        }
        let src = rope.to_string();
        let (haystack, needle) = if self.case_sensitive {
            (src.clone(), self.text.clone())
        } else {
            (src.to_lowercase(), self.text.to_lowercase())
        };

        let mut results = Vec::new();
        let mut byte_start = 0;
        while let Some(rel) = haystack[byte_start..].find(&needle) {
            let abs_byte = byte_start + rel;
            let char_start = rope.byte_to_char(abs_byte);
            let char_end = rope.byte_to_char(abs_byte + needle.len());
            results.push(char_start..char_end);
            byte_start = abs_byte + needle.len().max(1);
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    #[test]
    fn finds_simple_match() {
        let r = Rope::from_str("hello world hello");
        let q = Query::new("hello");
        let m = q.all_matches(&r);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0], 0..5);
        assert_eq!(m[1], 12..17);
    }

    #[test]
    fn case_insensitive_by_default() {
        let r = Rope::from_str("Hello HELLO hello");
        let q = Query::new("hello");
        assert_eq!(q.all_matches(&r).len(), 3);
    }

    #[test]
    fn case_sensitive_mode() {
        let r = Rope::from_str("Hello HELLO hello");
        let q = Query::new("hello").case_sensitive(true);
        let m = q.all_matches(&r);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0], 12..17);
    }

    #[test]
    fn empty_needle_returns_nothing() {
        let r = Rope::from_str("hello");
        assert!(Query::new("").all_matches(&r).is_empty());
    }

    #[test]
    fn no_match_returns_empty() {
        let r = Rope::from_str("hello");
        assert!(Query::new("xyz").all_matches(&r).is_empty());
    }

    #[test]
    fn match_at_eof() {
        let r = Rope::from_str("abcdef");
        let q = Query::new("def");
        let m = q.all_matches(&r);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0], 3..6);
    }

    #[test]
    fn multibyte_chars() {
        // "héllo" — é is 2 UTF-8 bytes but 1 char in ropey
        let r = Rope::from_str("héllo wörld");
        let q = Query::new("wörld");
        let m = q.all_matches(&r);
        assert_eq!(m.len(), 1);
        // "héllo " = 6 chars (h,é,l,l,o,space)
        assert_eq!(m[0].start, 6);
    }
}
