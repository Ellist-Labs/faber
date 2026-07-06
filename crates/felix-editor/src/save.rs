use ropey::Rope;
use std::{io, path::Path};

pub fn save(rope: &Rope, path: &Path) -> io::Result<()> {
    let content = rope.to_string();
    std::fs::write(path, content)
}
