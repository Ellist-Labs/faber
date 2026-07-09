use crate::trigger::IndexTrigger;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const DEBOUNCE_MS: u64 = 100;
const ECHO_SUPPRESS_MS: u64 = 2_000;

/// Coalesce a batch of filesystem event paths into a single trigger.
/// Paths in `ignored` are filtered out. Returns None if nothing remains.
pub fn coalesce(events: Vec<PathBuf>, ignored: &HashSet<PathBuf>) -> Option<IndexTrigger> {
    let paths: Vec<PathBuf> = events
        .into_iter()
        .filter(|p| !ignored.contains(p))
        .collect();
    if paths.is_empty() {
        return None;
    }
    Some(IndexTrigger::ExternalChanges(paths))
}

pub struct FsWatcher {
    _watcher: RecommendedWatcher,
    echo_suppress: Arc<Mutex<Vec<(PathBuf, Instant)>>>,
}

impl FsWatcher {
    /// Start watching `root`. Calls `on_trigger` when a coalesced trigger fires.
    pub fn start(
        root: &Path,
        on_trigger: impl Fn(IndexTrigger) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let pending: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
        let overflow: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
        let echo_suppress: Arc<Mutex<Vec<(PathBuf, Instant)>>> = Arc::new(Mutex::new(Vec::new()));

        let pending_clone = pending.clone();
        let overflow_clone = overflow.clone();

        let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();
        let mut watcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })?;
        watcher.watch(root, RecursiveMode::Recursive)?;

        // Receiver thread: collect events into the pending buffer.
        std::thread::spawn(move || {
            for res in rx {
                match res {
                    Ok(event) => {
                        // Skip metadata-only events (.git internals, etc.)
                        let is_data_event = matches!(
                            event.kind,
                            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                        );
                        if !is_data_event {
                            continue;
                        }
                        // Fast-path: drop .git events.
                        let paths: Vec<PathBuf> = event
                            .paths
                            .into_iter()
                            .filter(|p| !p.components().any(|c| c.as_os_str() == ".git"))
                            .collect();
                        if paths.is_empty() {
                            continue;
                        }
                        if let Ok(mut buf) = pending_clone.lock() {
                            buf.extend(paths);
                        }
                    }
                    Err(_) => {
                        // Watcher error or overflow → degrade to full rescan.
                        if let Ok(mut flag) = overflow_clone.lock() {
                            *flag = true;
                        }
                    }
                }
            }
        });

        // Debounce thread: drains pending every DEBOUNCE_MS, coalesces, fires on_trigger.
        let echo_suppress_clone = echo_suppress.clone();
        let pending_drain = pending.clone();
        let overflow_drain = overflow.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(DEBOUNCE_MS));

                // Check overflow first.
                let is_overflow = {
                    let mut flag = overflow_drain.lock().unwrap();
                    let v = *flag;
                    *flag = false;
                    v
                };
                if is_overflow {
                    on_trigger(IndexTrigger::FolderOpened);
                    continue;
                }

                let batch = {
                    let mut buf = pending_drain.lock().unwrap();
                    std::mem::take(&mut *buf)
                };
                if batch.is_empty() {
                    continue;
                }

                // Remove echo-suppressed paths (written in-app within ECHO_SUPPRESS_MS).
                let now = Instant::now();
                let echo_ignored: HashSet<PathBuf> = {
                    let mut echo = echo_suppress_clone.lock().unwrap();
                    // Prune expired entries.
                    echo.retain(|(_, t)| {
                        now.duration_since(*t).as_millis() < ECHO_SUPPRESS_MS as u128
                    });
                    echo.iter().map(|(p, _)| p.clone()).collect()
                };

                if let Some(trigger) = coalesce(batch, &echo_ignored) {
                    on_trigger(trigger);
                }
            }
        });

        Ok(Self {
            _watcher: watcher,
            echo_suppress,
        })
    }

    /// Call when a file is saved in-app so its watcher event is suppressed.
    pub fn suppress_echo(&self, path: PathBuf) {
        if let Ok(mut echo) = self.echo_suppress.lock() {
            echo.push((path, Instant::now()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths_set(ps: &[&str]) -> HashSet<PathBuf> {
        ps.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn coalesce_empty_batch_is_none() {
        assert!(coalesce(vec![], &HashSet::new()).is_none());
    }

    #[test]
    fn coalesce_paths_not_ignored_returns_external_changes() {
        let result = coalesce(
            vec![PathBuf::from("src/a.rs"), PathBuf::from("src/b.rs")],
            &HashSet::new(),
        );
        match result {
            Some(IndexTrigger::ExternalChanges(paths)) => {
                assert_eq!(paths.len(), 2);
            }
            _ => panic!("expected ExternalChanges"),
        }
    }

    #[test]
    fn coalesce_all_ignored_is_none() {
        let ignored = paths_set(&["src/a.rs", "src/b.rs"]);
        let result = coalesce(
            vec![PathBuf::from("src/a.rs"), PathBuf::from("src/b.rs")],
            &ignored,
        );
        assert!(result.is_none());
    }

    #[test]
    fn coalesce_partial_ignored_filters_correctly() {
        let ignored = paths_set(&["src/a.rs"]);
        let result = coalesce(
            vec![PathBuf::from("src/a.rs"), PathBuf::from("src/b.rs")],
            &ignored,
        );
        match result {
            Some(IndexTrigger::ExternalChanges(paths)) => {
                assert_eq!(paths.len(), 1);
                assert_eq!(paths[0], PathBuf::from("src/b.rs"));
            }
            _ => panic!("expected ExternalChanges with one path"),
        }
    }

    #[test]
    fn watcher_fires_on_file_write() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();

        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = fired.clone();

        let _watcher = FsWatcher::start(&root, move |_trigger| {
            fired_clone.store(true, Ordering::SeqCst);
        })
        .expect("watcher start");

        // Write a file to trigger the watcher.
        let file_path = root.join("test.txt");
        std::fs::write(&file_path, b"hello").unwrap();

        // Allow time for the event to propagate (debounce + processing).
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if fired.load(Ordering::SeqCst) {
                break;
            }
            if Instant::now() > deadline {
                panic!("on_trigger never fired after file write");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
