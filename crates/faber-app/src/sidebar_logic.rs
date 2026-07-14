use std::collections::HashMap;
use std::path::PathBuf;

use faber_lsp::diagnostics::Severity;

/// Given `(file_path, worst_severity)` pairs, produce a map covering every
/// path (file and ancestor directory) keyed to the worst severity among all
/// files under that path.  Used once per render to tint sidebar rows.
///
/// "Worst" follows `Severity`'s `Ord`: `Error < Warning < Information < Hint`,
/// so `min()` gives the highest-priority severity.
pub fn rollup_worst_severity(files: &[(PathBuf, Severity)]) -> HashMap<PathBuf, Severity> {
    let mut map: HashMap<PathBuf, Severity> = HashMap::new();

    for (path, sev) in files {
        let update = |e: &mut Severity| {
            if *sev < *e {
                *e = *sev;
            }
        };
        map.entry(path.clone()).and_modify(update).or_insert(*sev);
        for ancestor in path.ancestors().skip(1) {
            map.entry(ancestor.to_path_buf())
                .and_modify(update)
                .or_insert(*sev);
        }
    }

    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn empty_input_returns_empty_map() {
        assert!(rollup_worst_severity(&[]).is_empty());
    }

    #[test]
    fn single_file_maps_file_and_ancestors() {
        let map = rollup_worst_severity(&[(p("/src/main.rs"), Severity::Error)]);
        assert_eq!(map[&p("/src/main.rs")], Severity::Error);
        assert_eq!(map[&p("/src")], Severity::Error);
        assert_eq!(map[&p("/")], Severity::Error);
    }

    #[test]
    fn warning_only_stays_warning() {
        let map = rollup_worst_severity(&[(p("/src/lib.rs"), Severity::Warning)]);
        assert_eq!(map[&p("/src/lib.rs")], Severity::Warning);
        assert_eq!(map[&p("/src")], Severity::Warning);
    }

    #[test]
    fn siblings_under_dir_take_worst() {
        let files = [
            (p("/src/a.rs"), Severity::Warning),
            (p("/src/b.rs"), Severity::Error),
        ];
        let map = rollup_worst_severity(&files);
        assert_eq!(map[&p("/src/a.rs")], Severity::Warning);
        assert_eq!(map[&p("/src/b.rs")], Severity::Error);
        // dir takes the worst of its children
        assert_eq!(map[&p("/src")], Severity::Error);
    }

    #[test]
    fn nested_dirs_propagate_to_root() {
        let files = [
            (p("/root/a/b/c.rs"), Severity::Error),
            (p("/root/a/d.rs"), Severity::Warning),
        ];
        let map = rollup_worst_severity(&files);
        assert_eq!(map[&p("/root/a/b")], Severity::Error);
        assert_eq!(map[&p("/root/a")], Severity::Error);
        assert_eq!(map[&p("/root")], Severity::Error);
    }

    #[test]
    fn info_and_hint_not_surfaced_above_warning() {
        let files = [
            (p("/src/a.rs"), Severity::Information),
            (p("/src/b.rs"), Severity::Warning),
        ];
        let map = rollup_worst_severity(&files);
        assert_eq!(map[&p("/src")], Severity::Warning);
    }
}
