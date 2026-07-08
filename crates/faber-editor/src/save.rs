use ropey::Rope;
use std::{io, path::Path};

pub fn save(rope: &Rope, path: &Path) -> io::Result<()> {
    let content = rope.to_string();
    std::fs::write(path, content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn save_then_reload_matches_rope() {
        let rope = Rope::from_str("hello\nworld\n");
        let tmp = NamedTempFile::new().unwrap();
        save(&rope, tmp.path()).unwrap();
        let contents = std::fs::read_to_string(tmp.path()).unwrap();
        assert_eq!(contents, "hello\nworld\n");
    }

    #[test]
    fn save_overwrites_existing_content() {
        let tmp = NamedTempFile::new().unwrap();
        save(&Rope::from_str("first"), tmp.path()).unwrap();
        save(&Rope::from_str("second"), tmp.path()).unwrap();
        let contents = std::fs::read_to_string(tmp.path()).unwrap();
        assert_eq!(contents, "second");
    }

    #[test]
    fn save_empty_rope() {
        let tmp = NamedTempFile::new().unwrap();
        save(&Rope::new(), tmp.path()).unwrap();
        let contents = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(contents.is_empty());
    }

    #[test]
    fn save_to_nonexistent_dir_errors() {
        let rope = Rope::from_str("data");
        let result = save(&rope, Path::new("/nonexistent/dir/file.txt"));
        assert!(result.is_err());
    }
}
