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
    let idx = char_idx.saturating_sub(1).min(chars.len().saturating_sub(1));
    chars[..idx].iter().chain(chars[char_idx..].iter()).copied().collect()
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
