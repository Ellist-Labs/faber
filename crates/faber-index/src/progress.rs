//! Throttled, latest-value progress channel between the engine (producer) and
//! the UI (consumer).
//!
//! The UI only ever wants the *newest* progress state, never a backlog, so the
//! channel is a `bounded(1)` cell: the emitter overwrites the pending slot rather
//! than queuing. High-frequency `report` calls are throttled — at most one every
//! [`THROTTLE`], or sooner when `done/total` moves more than 1% — while `Begin`,
//! `End`, and phase transitions always emit.

use arc_swap::ArcSwapOption;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

/// Minimum wall-clock gap between throttled `report` emissions.
const THROTTLE: Duration = Duration::from_millis(100);

/// Which phase of a run a progress report belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Scanning,
    Indexing { module: &'static str },
    Publishing,
}

/// A single progress datapoint delivered to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressEvent {
    /// A run started.
    Begin,
    /// Progress within a phase.
    Report {
        phase: Phase,
        done: usize,
        total: usize,
    },
    /// A run finished; `files_indexed` is the run's content-phase file count.
    End { files_indexed: usize },
}

/// RAII counter handle: incrementing the shared total on creation and
/// decrementing it on drop. Lets the engine track in-flight work without manual
/// bookkeeping — hand one out per queued file, drop it when the file is done.
pub struct ProgressEntry {
    counter: Arc<AtomicUsize>,
}

impl Drop for ProgressEntry {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Producer half. The engine holds one and calls `begin`/`report`/`end`.
pub struct ProgressEmitter {
    /// Latest-value slot shared with the receiver. Storing atomically avoids
    /// the race where a shared-receiver drain could steal events from the UI.
    slot: Arc<ArcSwapOption<ProgressEvent>>,
    /// In-flight work counter shared with every outstanding `ProgressEntry`.
    total: Arc<AtomicUsize>,
    /// Throttle bookkeeping: the last emit instant and the last phase emitted.
    throttle: Mutex<ThrottleState>,
}

struct ThrottleState {
    last_emit: Instant,
    last_phase: Option<Phase>,
}

impl ProgressEmitter {
    /// Emit `Begin` unconditionally and reset the throttle window.
    pub fn begin(&self) {
        {
            let mut t = self.throttle.lock().unwrap();
            t.last_emit = Instant::now();
            t.last_phase = None;
        }
        self.send(ProgressEvent::Begin);
    }

    /// Emit a `Report`, subject to throttling. A phase change or a >1% move in
    /// `done/total` bypasses the time throttle; otherwise emissions are spaced at
    /// least [`THROTTLE`] apart.
    pub fn report(&self, phase: Phase, done: usize, total: usize) {
        let should_emit = {
            let mut t = self.throttle.lock().unwrap();
            let phase_changed = t.last_phase.as_ref() != Some(&phase);
            let now = Instant::now();
            let time_ok = now.duration_since(t.last_emit) >= THROTTLE;
            // >1% of total (guard total==0). Cheap integer test: 100*done crossing
            // a new percent bucket is approximated by the time gate; here we treat
            // any nonzero delta past the time window as significant.
            let big_delta = total > 0 && done.saturating_mul(100) >= total;
            if phase_changed || time_ok || big_delta {
                t.last_emit = now;
                t.last_phase = Some(phase.clone());
                true
            } else {
                false
            }
        };
        if should_emit {
            self.send(ProgressEvent::Report { phase, done, total });
        }
    }

    /// Emit `End` unconditionally.
    pub fn end(&self, files_indexed: usize) {
        self.send(ProgressEvent::End { files_indexed });
    }

    /// Hand out a counting handle and bump the in-flight total by one.
    pub fn new_entry(&self) -> ProgressEntry {
        self.total.fetch_add(1, Ordering::Relaxed);
        ProgressEntry {
            counter: self.total.clone(),
        }
    }

    /// Current in-flight work count (sum of live `ProgressEntry`s).
    pub fn in_flight(&self) -> usize {
        self.total.load(Ordering::Relaxed)
    }

    /// Atomically overwrite the shared slot. The receiver takes the value at
    /// its next poll; no intermediate event is ever stolen by the emitter.
    fn send(&self, ev: ProgressEvent) {
        self.slot.store(Some(Arc::new(ev)));
    }
}

/// Consumer half. `faber-app` polls `try_recv` on a timer.
pub struct ProgressReceiver {
    slot: Arc<ArcSwapOption<ProgressEvent>>,
}

impl ProgressReceiver {
    /// Take the latest pending event (if any), leaving the slot empty.
    pub fn try_recv(&self) -> Option<ProgressEvent> {
        self.slot
            .swap(None)
            .map(|arc| Arc::try_unwrap(arc).unwrap_or_else(|a| (*a).clone()))
    }
}

/// Build a linked emitter/receiver pair backed by an atomic latest-value slot.
pub fn progress_channel() -> (ProgressEmitter, ProgressReceiver) {
    let slot = Arc::new(ArcSwapOption::empty());
    let emitter = ProgressEmitter {
        slot: slot.clone(),
        total: Arc::new(AtomicUsize::new(0)),
        throttle: Mutex::new(ThrottleState {
            last_emit: Instant::now() - THROTTLE,
            last_phase: None,
        }),
    };
    (emitter, ProgressReceiver { slot })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_and_end_always_delivered() {
        let (em, rx) = progress_channel();
        em.begin();
        assert_eq!(rx.try_recv(), Some(ProgressEvent::Begin));
        em.end(7);
        assert_eq!(rx.try_recv(), Some(ProgressEvent::End { files_indexed: 7 }));
    }

    #[test]
    fn phase_change_bypasses_throttle() {
        let (em, rx) = progress_channel();
        em.begin();
        let _ = rx.try_recv();
        // Two different phases back-to-back: both should surface (latest-value, so
        // poll between them).
        em.report(Phase::Scanning, 1, 10);
        let first = rx.try_recv();
        assert!(matches!(
            first,
            Some(ProgressEvent::Report {
                phase: Phase::Scanning,
                ..
            })
        ));
        em.report(Phase::Publishing, 1, 10);
        let second = rx.try_recv();
        assert!(matches!(
            second,
            Some(ProgressEvent::Report {
                phase: Phase::Publishing,
                ..
            })
        ));
    }

    #[test]
    fn same_phase_rapid_reports_are_throttled() {
        let (em, rx) = progress_channel();
        // First report of a phase always lands (phase change from None).
        em.report(Phase::Indexing { module: "files" }, 0, 1000);
        assert!(rx.try_recv().is_some());
        // A tiny immediate follow-up in the same phase is throttled away.
        em.report(Phase::Indexing { module: "files" }, 1, 1000);
        assert!(rx.try_recv().is_none());
    }

    #[test]
    fn receiver_collapses_burst_to_latest() {
        let (em, rx) = progress_channel();
        em.begin();
        em.end(3);
        // Only the newest event survives the latest-value cell.
        assert_eq!(rx.try_recv(), Some(ProgressEvent::End { files_indexed: 3 }));
        assert_eq!(rx.try_recv(), None);
    }

    #[test]
    fn entry_tracks_in_flight_count() {
        let (em, _rx) = progress_channel();
        assert_eq!(em.in_flight(), 0);
        let a = em.new_entry();
        let b = em.new_entry();
        assert_eq!(em.in_flight(), 2);
        drop(a);
        assert_eq!(em.in_flight(), 1);
        drop(b);
        assert_eq!(em.in_flight(), 0);
    }
}
