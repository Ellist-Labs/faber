//! File-type and folder icon lookup backed by the Material Icon Theme manifest.
//! Runs per visible row per frame — lookups are alloc-free (stack lowercase buffer
//! + binary search over sorted static tables).

use crate::file_icon_data::{
    DEFAULT_FILE, DEFAULT_FOLDER, DEFAULT_FOLDER_OPEN, FILE_EXTENSIONS, FILE_NAMES, FOLDER_NAMES,
};

const LOWER_BUF: usize = 64;

fn lowered<'b>(name: &str, buf: &'b mut [u8; LOWER_BUF]) -> Option<&'b str> {
    let bytes = name.as_bytes();
    if bytes.len() > LOWER_BUF || !name.is_ascii() {
        return None;
    }
    for (i, b) in bytes.iter().enumerate() {
        buf[i] = b.to_ascii_lowercase();
    }
    std::str::from_utf8(&buf[..bytes.len()]).ok()
}

fn lookup(table: &'static [(&str, &str)], key: &str) -> Option<&'static str> {
    table
        .binary_search_by(|(k, _)| (*k).cmp(key))
        .ok()
        .map(|ix| table[ix].1)
}

pub fn icon_for_file(name: &str) -> &'static str {
    let mut buf = [0u8; LOWER_BUF];
    let Some(lower) = lowered(name, &mut buf) else {
        return DEFAULT_FILE;
    };
    if let Some(icon) = lookup(FILE_NAMES, lower) {
        return icon;
    }
    // Longest-suffix extension match: "a.test.d.ts" tries "test.d.ts", "d.ts", "ts".
    let mut rest = lower;
    while let Some(dot) = rest[1..].find('.') {
        rest = &rest[dot + 2..];
        if let Some(icon) = lookup(FILE_EXTENSIONS, rest) {
            return icon;
        }
    }
    DEFAULT_FILE
}

/// Language brand color for the 7px tab/tree dot (spec §5.2), keyed by file
/// extension. `None` ⇒ caller renders the dot in `text_muted`.
/// Sorted for binary search.
const LANGUAGE_DOT_COLORS: &[(&str, u32)] = &[
    ("c", 0x555555FF),
    ("cpp", 0xF34B7DFF),
    ("css", 0x663399FF),
    ("go", 0x00ADD8FF),
    ("html", 0xE34C26FF),
    ("js", 0xF1E05AFF),
    ("json", 0x8A9A5BFF),
    ("jsx", 0xF1E05AFF),
    ("kt", 0xA97BFFFF),
    ("md", 0x519ABAFF),
    ("py", 0x3572A5FF),
    ("rb", 0x701516FF),
    ("rs", 0xDEA584FF),
    ("swift", 0xF05138FF),
    ("toml", 0x9C4221FF),
    ("ts", 0x3178C6FF),
    ("tsx", 0x3178C6FF),
    ("yaml", 0xCB171EFF),
    ("yml", 0xCB171EFF),
];

pub fn language_dot_color(file_name: &str) -> Option<u32> {
    let mut buf = [0u8; LOWER_BUF];
    let lower = lowered(file_name, &mut buf)?;
    let ext = lower.rsplit('.').next()?;
    LANGUAGE_DOT_COLORS
        .binary_search_by(|(k, _)| (*k).cmp(ext))
        .ok()
        .map(|ix| LANGUAGE_DOT_COLORS[ix].1)
}

#[allow(dead_code)] // tree rows now use language dots; kept for future folder-icon use
pub fn icon_for_folder(name: &str, open: bool) -> &'static str {
    let mut buf = [0u8; LOWER_BUF];
    if let Some(lower) = lowered(name, &mut buf)
        && let Ok(ix) = FOLDER_NAMES.binary_search_by(|(k, _, _)| (*k).cmp(lower))
    {
        let (_, closed, opened) = FOLDER_NAMES[ix];
        return if open { opened } else { closed };
    }
    if open {
        DEFAULT_FOLDER_OPEN
    } else {
        DEFAULT_FOLDER
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_sorted() {
        assert!(FILE_NAMES.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(FILE_EXTENSIONS.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(FOLDER_NAMES.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(LANGUAGE_DOT_COLORS.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn language_dot_lookup() {
        assert_eq!(language_dot_color("main.rs"), Some(0xDEA584FF));
        assert_eq!(language_dot_color("App.swift"), Some(0xF05138FF));
        assert_eq!(language_dot_color("mystery.xyz"), None);
    }

    #[test]
    fn exact_name_beats_extension() {
        assert_eq!(icon_for_file("Dockerfile"), "icons/file/docker.svg");
        assert_eq!(icon_for_file(".gitignore"), "icons/file/git.svg");
    }

    #[test]
    fn extension_lookup() {
        assert_eq!(icon_for_file("main.rs"), "icons/file/rust.svg");
        assert_eq!(icon_for_file("README.md"), "icons/file/readme.svg");
        assert_eq!(icon_for_file("index.d.ts"), "icons/file/typescript-def.svg");
    }

    #[test]
    fn unknown_falls_back() {
        assert_eq!(icon_for_file("mystery.xyz123"), DEFAULT_FILE);
        assert_eq!(icon_for_file("noextension"), DEFAULT_FILE);
    }

    #[test]
    fn folder_variants() {
        assert_eq!(icon_for_folder("src", false), "icons/file/folder-src.svg");
        assert_eq!(
            icon_for_folder("src", true),
            "icons/file/folder-src-open.svg"
        );
        assert_eq!(icon_for_folder("random", false), DEFAULT_FOLDER);
        assert_eq!(icon_for_folder("random", true), DEFAULT_FOLDER_OPEN);
    }
}
