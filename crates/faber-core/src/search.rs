use regex::RegexBuilder;
use ropey::Rope;
use std::ops::Range;

/// A search query supporting literal, regex, case, and whole-word modes.
pub struct Query {
    pub text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

impl Query {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            case_sensitive: false,
            whole_word: false,
            regex: false,
        }
    }

    pub fn case_sensitive(mut self, yes: bool) -> Self {
        self.case_sensitive = yes;
        self
    }

    pub fn whole_word(mut self, yes: bool) -> Self {
        self.whole_word = yes;
        self
    }

    pub fn regex(mut self, yes: bool) -> Self {
        self.regex = yes;
        self
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// All non-overlapping matches in `text` as char-offset ranges.
    /// This is the core matcher; `all_matches` delegates here.
    pub fn all_matches_str(&self, text: &str) -> Vec<Range<usize>> {
        if self.is_empty() {
            return Vec::new();
        }

        // Fast path: literal substring with optional case folding, no word boundaries.
        if !self.regex && !self.whole_word {
            let (haystack, needle): (std::borrow::Cow<str>, std::borrow::Cow<str>) =
                if self.case_sensitive {
                    (text.into(), self.text.as_str().into())
                } else {
                    (text.to_lowercase().into(), self.text.to_lowercase().into())
                };
            let mut results = Vec::new();
            let mut byte_start = 0;
            while let Some(rel) = haystack[byte_start..].find(needle.as_ref()) {
                let abs_byte = byte_start + rel;
                let char_start = haystack[..abs_byte].chars().count();
                let char_end = char_start + needle.chars().count();
                results.push(char_start..char_end);
                byte_start = abs_byte + needle.len().max(1);
            }
            return results;
        }

        // Regex / whole-word path.
        let pat = if self.regex {
            self.text.clone()
        } else {
            regex::escape(&self.text)
        };
        let pat = if self.whole_word {
            format!(r"\b{}\b", pat)
        } else {
            pat
        };

        let re = match RegexBuilder::new(&pat)
            .case_insensitive(!self.case_sensitive)
            .build()
        {
            Ok(r) => r,
            Err(_) => return Vec::new(), // invalid user regex → no matches, no panic
        };

        re.find_iter(text)
            .map(|m| {
                let char_start = text[..m.start()].chars().count();
                let char_end = char_start + text[m.start()..m.end()].chars().count();
                char_start..char_end
            })
            .collect()
    }

    /// All non-overlapping matches as char-offset ranges.
    pub fn all_matches(&self, rope: &Rope) -> Vec<Range<usize>> {
        let src = rope.to_string();
        self.all_matches_str(&src)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    // ── existing literal tests ─────────────────────────────────────────────────

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
        let r = Rope::from_str("héllo wörld");
        let q = Query::new("wörld");
        let m = q.all_matches(&r);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].start, 6);
    }

    // ── whole-word tests ────────────────────────────────────────────────────────

    #[test]
    fn whole_word_matches_standalone() {
        let r = Rope::from_str("foo foobar foo");
        let q = Query::new("foo").whole_word(true);
        let m = q.all_matches(&r);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0], 0..3);
        assert_eq!(m[1], 11..14);
    }

    #[test]
    fn whole_word_rejects_substring() {
        let r = Rope::from_str("foobar");
        let q = Query::new("foo").whole_word(true);
        assert!(q.all_matches(&r).is_empty());
    }

    #[test]
    fn whole_word_case_insensitive() {
        let r = Rope::from_str("Foo foo FOO foobar");
        let q = Query::new("foo").whole_word(true).case_sensitive(false);
        let m = q.all_matches(&r);
        assert_eq!(m.len(), 3);
    }

    // ── regex tests ─────────────────────────────────────────────────────────────

    #[test]
    fn regex_basic_match() {
        let r = Rope::from_str("abc 123 def 456");
        let q = Query::new(r"\d+").regex(true);
        let m = q.all_matches(&r);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn regex_case_insensitive() {
        let r = Rope::from_str("Hello HELLO hello");
        let q = Query::new("hello").regex(true).case_sensitive(false);
        assert_eq!(q.all_matches(&r).len(), 3);
    }

    #[test]
    fn regex_case_sensitive() {
        let r = Rope::from_str("Hello HELLO hello");
        let q = Query::new("hello").regex(true).case_sensitive(true);
        let m = q.all_matches(&r);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].start, 12);
    }

    #[test]
    fn invalid_regex_returns_empty() {
        let r = Rope::from_str("hello");
        let q = Query::new("[invalid").regex(true);
        assert!(q.all_matches(&r).is_empty());
    }
}
