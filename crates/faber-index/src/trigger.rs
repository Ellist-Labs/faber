//! What kicks off an index run (`IndexTrigger`) and the join-semilattice of scan
//! scopes (`ScanScope`) the engine coalesces them into.
//!
//! Multiple triggers can pile up while a run is in flight; the engine folds them
//! into a single `ScanScope` via `merge`, which is associative and commutative so
//! the fold order never matters.

use std::{collections::BTreeSet, path::PathBuf};

/// Above this many distinct paths, a `Paths` scope degrades to a full `Walk`:
/// stat'ing thousands of paths individually costs more than one tree walk.
const PATH_SCOPE_CAP: usize = 1_000;

/// A request to (re)index, from the app or the filesystem watcher.
pub enum IndexTrigger {
    /// A project was opened — do the initial walk.
    FolderOpened,
    /// One file was saved in-editor — restat just that path.
    FileSaved(PathBuf),
    /// The watcher observed external edits — restat those paths.
    ExternalChanges(Vec<PathBuf>),
    /// User asked for a rebuild — re-hash everything (`Verify`).
    Manual,
}

/// The lattice of scan scopes, ordered `Paths ⊑ Walk ⊑ Verify`.
///
/// `merge` is the join (`⊔`): it returns the least scope covering both inputs, so
/// coalescing a batch of triggers never under-scans.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ScanScope {
    /// Restat exactly these paths, no tree walk.
    Paths(BTreeSet<PathBuf>),
    /// Walk the whole tree; hash only files whose mtime/size changed.
    Walk,
    /// Walk the whole tree and re-hash every file (ignore cached hashes).
    Verify,
}

impl ScanScope {
    /// Join two scopes: `Verify` absorbs everything, `Walk` absorbs `Paths`, and
    /// `Paths ⊔ Paths` is the union — capped to `Walk` past [`PATH_SCOPE_CAP`].
    pub(crate) fn merge(self, other: ScanScope) -> ScanScope {
        use ScanScope::*;
        match (self, other) {
            (Verify, _) | (_, Verify) => Verify,
            (Walk, _) | (_, Walk) => Walk,
            (Paths(mut a), Paths(b)) => {
                a.extend(b);
                if a.len() > PATH_SCOPE_CAP {
                    Walk
                } else {
                    Paths(a)
                }
            }
        }
    }

    /// The scope a single trigger maps to before any coalescing.
    pub(crate) fn from_trigger(t: IndexTrigger) -> ScanScope {
        match t {
            IndexTrigger::FolderOpened => ScanScope::Walk,
            IndexTrigger::Manual => ScanScope::Verify,
            IndexTrigger::FileSaved(p) => ScanScope::Paths(BTreeSet::from([p])),
            IndexTrigger::ExternalChanges(paths) => {
                if paths.len() > PATH_SCOPE_CAP {
                    ScanScope::Walk
                } else {
                    ScanScope::Paths(paths.into_iter().collect())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(ps: &[&str]) -> ScanScope {
        ScanScope::Paths(ps.iter().map(PathBuf::from).collect())
    }

    #[test]
    fn verify_absorbs_everything() {
        assert_eq!(ScanScope::Verify.merge(ScanScope::Walk), ScanScope::Verify);
        assert_eq!(ScanScope::Walk.merge(ScanScope::Verify), ScanScope::Verify);
        assert_eq!(ScanScope::Verify.merge(paths(&["a"])), ScanScope::Verify);
        assert_eq!(paths(&["a"]).merge(ScanScope::Verify), ScanScope::Verify);
    }

    #[test]
    fn walk_absorbs_paths_but_not_verify() {
        assert_eq!(ScanScope::Walk.merge(paths(&["a"])), ScanScope::Walk);
        assert_eq!(paths(&["a"]).merge(ScanScope::Walk), ScanScope::Walk);
        assert_eq!(ScanScope::Walk.merge(ScanScope::Walk), ScanScope::Walk);
    }

    #[test]
    fn paths_union_merges() {
        assert_eq!(
            paths(&["a", "b"]).merge(paths(&["b", "c"])),
            paths(&["a", "b", "c"])
        );
    }

    #[test]
    fn paths_union_over_cap_degrades_to_walk() {
        let big: BTreeSet<PathBuf> = (0..=PATH_SCOPE_CAP)
            .map(|i| PathBuf::from(i.to_string()))
            .collect();
        let one = ScanScope::Paths(BTreeSet::from([PathBuf::from("extra")]));
        assert_eq!(ScanScope::Paths(big).merge(one), ScanScope::Walk);
    }

    #[test]
    fn from_trigger_maps_each_variant() {
        assert_eq!(
            ScanScope::from_trigger(IndexTrigger::FolderOpened),
            ScanScope::Walk
        );
        assert_eq!(
            ScanScope::from_trigger(IndexTrigger::Manual),
            ScanScope::Verify
        );
        assert_eq!(
            ScanScope::from_trigger(IndexTrigger::FileSaved(PathBuf::from("x"))),
            paths(&["x"])
        );
        assert_eq!(
            ScanScope::from_trigger(IndexTrigger::ExternalChanges(vec![
                PathBuf::from("a"),
                PathBuf::from("b"),
            ])),
            paths(&["a", "b"])
        );
    }

    #[test]
    fn external_changes_over_cap_is_walk() {
        let many: Vec<PathBuf> = (0..=PATH_SCOPE_CAP)
            .map(|i| PathBuf::from(i.to_string()))
            .collect();
        assert_eq!(
            ScanScope::from_trigger(IndexTrigger::ExternalChanges(many)),
            ScanScope::Walk
        );
    }
}
