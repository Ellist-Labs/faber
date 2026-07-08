//! Char-based (UTF-8 safe) string editing helpers shared by the single-line
//! text inputs (search bars, outline overlay, file finder).

pub fn insert_at(s: &str, char_idx: usize, text: &str) -> String {
    let mut chars: Vec<char> = s.chars().collect();
    let idx = char_idx.min(chars.len());
    for (i, c) in text.chars().enumerate() {
        chars.insert(idx + i, c);
    }
    chars.into_iter().collect()
}

pub fn delete_char_before(s: &str, char_idx: usize) -> String {
    if char_idx == 0 {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let idx = char_idx
        .saturating_sub(1)
        .min(chars.len().saturating_sub(1));
    chars[..idx]
        .iter()
        .chain(chars[char_idx..].iter())
        .copied()
        .collect()
}

pub fn split_at_char(s: &str, char_idx: usize) -> (String, String) {
    let chars: Vec<char> = s.chars().collect();
    let idx = char_idx.min(chars.len());
    (chars[..idx].iter().collect(), chars[idx..].iter().collect())
}

pub fn word_start_before(s: &str, cursor: usize) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let mut pos = cursor.min(chars.len());
    while pos > 0 && chars[pos - 1].is_whitespace() {
        pos -= 1;
    }
    while pos > 0 && !chars[pos - 1].is_whitespace() {
        pos -= 1;
    }
    pos
}

pub fn delete_char_range(s: &str, start: usize, end: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let start = start.min(chars.len());
    let end = end.min(chars.len());
    chars[..start].iter().chain(chars[end..].iter()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_at_start() {
        assert_eq!(insert_at("world", 0, "hello "), "hello world");
    }

    #[test]
    fn insert_at_end() {
        assert_eq!(insert_at("hello", 5, " world"), "hello world");
    }

    #[test]
    fn insert_at_middle() {
        assert_eq!(insert_at("helo", 3, "l"), "hello");
    }

    #[test]
    fn insert_at_clamped_to_len() {
        assert_eq!(insert_at("hi", 99, "!"), "hi!");
    }

    #[test]
    fn insert_at_multibyte() {
        assert_eq!(insert_at("caf", 3, "é"), "café");
    }

    #[test]
    fn delete_char_before_normal() {
        assert_eq!(delete_char_before("hello", 5), "hell");
    }

    #[test]
    fn delete_char_before_at_zero_is_noop() {
        assert_eq!(delete_char_before("hello", 0), "hello");
    }

    #[test]
    fn delete_char_before_multibyte() {
        assert_eq!(delete_char_before("café", 4), "caf");
    }

    #[test]
    fn split_at_char_start() {
        assert_eq!(
            split_at_char("hello", 0),
            ("".to_string(), "hello".to_string())
        );
    }

    #[test]
    fn split_at_char_end() {
        assert_eq!(
            split_at_char("hello", 5),
            ("hello".to_string(), "".to_string())
        );
    }

    #[test]
    fn split_at_char_middle() {
        assert_eq!(
            split_at_char("hello", 3),
            ("hel".to_string(), "lo".to_string())
        );
    }

    #[test]
    fn split_at_char_clamped() {
        assert_eq!(split_at_char("hi", 99), ("hi".to_string(), "".to_string()));
    }

    #[test]
    fn split_at_char_multibyte() {
        assert_eq!(
            split_at_char("café", 3),
            ("caf".to_string(), "é".to_string())
        );
    }

    #[test]
    fn word_start_before_mid_word() {
        // "hello world" cursor at end (11) → start of "world" = 6
        assert_eq!(word_start_before("hello world", 11), 6);
    }

    #[test]
    fn word_start_before_at_zero_is_zero() {
        assert_eq!(word_start_before("hello", 0), 0);
    }

    #[test]
    fn word_start_before_skips_trailing_spaces_then_word() {
        // cursor after "hello   " (8) → skips spaces → skips "hello" → 0
        assert_eq!(word_start_before("hello   ", 8), 0);
    }

    #[test]
    fn word_start_before_leading_spaces_stops_at_word_boundary() {
        // "  hello" cursor at 7 → skips "hello" → lands at 2 (before spaces end)
        assert_eq!(word_start_before("  hello", 7), 2);
    }

    #[test]
    fn delete_char_range_basic() {
        assert_eq!(delete_char_range("hello world", 5, 11), "hello");
    }

    #[test]
    fn delete_char_range_empty_range_is_noop() {
        assert_eq!(delete_char_range("hello", 2, 2), "hello");
    }

    #[test]
    fn delete_char_range_clamped_to_len() {
        assert_eq!(delete_char_range("hi", 0, 99), "");
    }

    #[test]
    fn delete_char_range_multibyte() {
        // "café bar" → delete chars 0..4 ("café") → " bar"
        assert_eq!(delete_char_range("café bar", 0, 4), " bar");
    }
}
