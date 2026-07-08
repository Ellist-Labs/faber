use std::io;
use std::path::{Path, PathBuf};

/// One entry in the project tree. `children: None` means the directory
/// hasn't been read yet — children are loaded lazily on first expand.
pub struct FileNode {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub expanded: bool,
    children: Option<Vec<FileNode>>,
}

/// A flattened, render-ready row of the expanded tree.
#[derive(Clone)]
pub struct VisibleRow {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
}

pub struct FileTree {
    pub root: PathBuf,
    nodes: Vec<FileNode>,
}

impl FileTree {
    /// Reads only the top level; deeper directories load on expand.
    pub fn new(root: PathBuf) -> io::Result<Self> {
        let nodes = read_dir_sorted(&root)?;
        Ok(Self { root, nodes })
    }

    /// Expand or collapse the directory at `path`. No-op for files or
    /// paths outside the tree.
    pub fn toggle(&mut self, path: &Path) -> io::Result<()> {
        let mut nodes = &mut self.nodes;
        loop {
            let Some(ix) = nodes.iter().position(|n| path.starts_with(&n.path)) else {
                return Ok(());
            };
            if nodes[ix].path == path {
                let node = &mut nodes[ix];
                if node.is_dir {
                    if node.expanded {
                        node.expanded = false;
                    } else {
                        if node.children.is_none() {
                            node.children = Some(read_dir_sorted(&node.path)?);
                        }
                        node.expanded = true;
                    }
                }
                return Ok(());
            }
            match nodes[ix].children {
                Some(ref mut children) => nodes = children,
                None => return Ok(()),
            }
        }
    }

    /// Expand the directory at `dir` if it isn't already, loading children lazily.
    /// No-op for files or paths outside the tree.
    fn ensure_expanded(&mut self, dir: &Path) -> io::Result<()> {
        let mut nodes = &mut self.nodes;
        loop {
            let Some(ix) = nodes.iter().position(|n| dir.starts_with(&n.path)) else {
                return Ok(());
            };
            if nodes[ix].path == dir {
                let node = &mut nodes[ix];
                if node.is_dir && !node.expanded {
                    if node.children.is_none() {
                        node.children = Some(read_dir_sorted(&node.path)?);
                    }
                    node.expanded = true;
                }
                return Ok(());
            }
            match nodes[ix].children {
                Some(ref mut children) => nodes = children,
                None => return Ok(()),
            }
        }
    }

    /// Expand all ancestor directories of `path` so the entry becomes visible
    /// in `visible()`. No-op if `path` is outside this tree's root.
    pub fn reveal(&mut self, path: &Path) -> io::Result<()> {
        if !path.starts_with(&self.root) {
            return Ok(());
        }
        let Ok(rel) = path.strip_prefix(&self.root) else { return Ok(()); };
        let components: Vec<_> = rel.components().collect();
        let mut current = self.root.clone();
        // Expand every ancestor (all components except the last = the target itself).
        for component in components.iter().take(components.len().saturating_sub(1)) {
            current = current.join(component);
            self.ensure_expanded(&current)?;
        }
        Ok(())
    }

    /// Collapse every expanded directory in the tree.
    pub fn collapse_all(&mut self) {
        collapse_nodes(&mut self.nodes);
    }

    /// Expand every directory in the tree, loading children lazily.
    pub fn expand_all(&mut self) -> io::Result<()> {
        expand_nodes(&mut self.nodes)
    }

    /// Reload the root level and every currently-expanded directory,
    /// preserving `expanded` flags for dirs that still exist.
    pub fn refresh(&mut self) -> io::Result<()> {
        self.nodes = refresh_nodes(&self.root, &self.nodes)?;
        Ok(())
    }

    /// Index of `path` in the current visible row list, or `None` if not visible.
    pub fn visible_index_of(&self, path: &Path) -> Option<usize> {
        self.visible().iter().position(|r| r.path == path)
    }

    /// Flatten the expanded tree into render-ready rows (DFS order).
    /// Callers cache the result and rebuild only after `toggle`.
    pub fn visible(&self) -> Vec<VisibleRow> {
        let mut rows = Vec::new();
        collect_visible(&self.nodes, 0, &mut rows);
        rows
    }
}

fn collapse_nodes(nodes: &mut Vec<FileNode>) {
    for n in nodes {
        if n.is_dir && n.expanded {
            n.expanded = false;
        }
        if let Some(children) = &mut n.children {
            collapse_nodes(children);
        }
    }
}

fn expand_nodes(nodes: &mut Vec<FileNode>) -> io::Result<()> {
    for n in nodes {
        if n.is_dir {
            if n.children.is_none() {
                n.children = Some(read_dir_sorted(&n.path)?);
            }
            n.expanded = true;
            if let Some(children) = &mut n.children {
                expand_nodes(children)?;
            }
        }
    }
    Ok(())
}

fn refresh_nodes(dir: &Path, existing: &[FileNode]) -> io::Result<Vec<FileNode>> {
    let fresh = read_dir_sorted(dir)?;
    let mut result = Vec::with_capacity(fresh.len());
    for mut node in fresh {
        if let Some(old) = existing.iter().find(|n| n.path == node.path) {
            // Preserve expanded state and recurse into expanded dirs.
            if node.is_dir && old.expanded {
                node.expanded = true;
                let old_children = old.children.as_deref().unwrap_or(&[]);
                node.children = Some(refresh_nodes(&node.path, old_children)?);
            }
        }
        result.push(node);
    }
    Ok(result)
}

fn read_dir_sorted(dir: &Path) -> io::Result<Vec<FileNode>> {
    let mut nodes: Vec<FileNode> = std::fs::read_dir(dir)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                return None;
            }
            let is_dir = entry.file_type().ok()?.is_dir();
            Some(FileNode { path: entry.path(), name, is_dir, expanded: false, children: None })
        })
        .collect();
    nodes.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(nodes)
}

fn collect_visible(nodes: &[FileNode], depth: usize, rows: &mut Vec<VisibleRow>) {
    for n in nodes {
        rows.push(VisibleRow {
            path: n.path.clone(),
            name: n.name.clone(),
            depth,
            is_dir: n.is_dir,
            expanded: n.expanded,
        });
        if n.expanded && let Some(children) = &n.children {
            collect_visible(children, depth + 1, rows);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static FIXTURE_N: AtomicUsize = AtomicUsize::new(0);

    fn fixture() -> PathBuf {
        let n = FIXTURE_N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir()
            .join(format!("faber_tree_test_{}_{}", std::process::id(), n));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/nested")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("README.md"), "").unwrap();
        fs::write(root.join("a.txt"), "").unwrap();
        fs::write(root.join(".hidden"), "").unwrap();
        fs::write(root.join("src/main.rs"), "").unwrap();
        fs::write(root.join("src/nested/deep.rs"), "").unwrap();
        root
    }

    #[test]
    fn top_level_sorted_dirs_first_hidden_skipped() {
        let root = fixture();
        let tree = FileTree::new(root.clone()).unwrap();
        let names: Vec<String> = tree.visible().into_iter().map(|r| r.name).collect();
        assert_eq!(names, ["docs", "src", "a.txt", "README.md"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn expand_is_lazy_and_toggle_collapses() {
        let root = fixture();
        let mut tree = FileTree::new(root.clone()).unwrap();
        let src = root.join("src");

        tree.toggle(&src).unwrap();
        let rows = tree.visible();
        assert!(rows.iter().any(|r| r.name == "main.rs" && r.depth == 1));
        assert!(rows.iter().any(|r| r.name == "nested" && r.depth == 1));
        // nested dir not read yet — no depth-2 rows
        assert!(!rows.iter().any(|r| r.depth == 2));

        tree.toggle(&root.join("src/nested")).unwrap();
        assert!(tree.visible().iter().any(|r| r.name == "deep.rs" && r.depth == 2));

        tree.toggle(&src).unwrap();
        let rows = tree.visible();
        assert!(!rows.iter().any(|r| r.depth >= 1));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn toggle_file_or_unknown_path_is_noop() {
        let root = fixture();
        let mut tree = FileTree::new(root.clone()).unwrap();
        tree.toggle(&root.join("a.txt")).unwrap();
        tree.toggle(Path::new("/nonexistent/elsewhere")).unwrap();
        assert_eq!(tree.visible().len(), 4);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reveal_expands_ancestors_and_makes_file_visible() {
        let root = fixture();
        let mut tree = FileTree::new(root.clone()).unwrap();
        let deep = root.join("src/nested/deep.rs");

        // Initially deep.rs is not visible (src is collapsed).
        assert!(tree.visible_index_of(&deep).is_none());

        tree.reveal(&deep).unwrap();

        // After reveal, deep.rs is visible.
        assert!(tree.visible_index_of(&deep).is_some());
        // Both intermediate dirs must be expanded.
        let rows = tree.visible();
        assert!(rows.iter().any(|r| r.name == "src" && r.expanded));
        assert!(rows.iter().any(|r| r.name == "nested" && r.expanded));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reveal_outside_root_is_noop() {
        let root = fixture();
        let mut tree = FileTree::new(root.clone()).unwrap();
        tree.reveal(Path::new("/nonexistent/elsewhere")).unwrap();
        assert_eq!(tree.visible().len(), 4);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn visible_index_of_returns_correct_index() {
        let root = fixture();
        let mut tree = FileTree::new(root.clone()).unwrap();
        tree.toggle(&root.join("src")).unwrap();
        let rows = tree.visible();
        let main_ix = rows.iter().position(|r| r.name == "main.rs").unwrap();
        assert_eq!(tree.visible_index_of(&root.join("src/main.rs")), Some(main_ix));
        assert_eq!(tree.visible_index_of(&root.join("src/nested/deep.rs")), None); // not expanded yet
        fs::remove_dir_all(root).unwrap();
    }
}
