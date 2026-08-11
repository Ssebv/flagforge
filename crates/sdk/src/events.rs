//! Experiment event accumulation.
//!
//! The SDK never sends individual events. Exposures and conversions land in an
//! in-process counter map — one cell per (experiment, variant, kind) — and the
//! flusher ships the counts. A service evaluating a flag a million times an
//! hour sends the same few dozen bytes as one evaluating it twice, which is
//! what lets the server keep pre-aggregated counters instead of an event log.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;

/// Upper bound on distinct counter cells held between flushes.
///
/// Cells are keyed by experiment × variant × kind, so a healthy process needs
/// a handful. Hitting this cap means experiment keys or variants are being
/// generated per request — a bug upstream — and the right failure mode is to
/// drop new cells and say so, not to grow without limit inside someone else's
/// service.
const MAX_CELLS: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EventKind {
    Exposure,
    Conversion,
}

/// One counter cell's identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Cell {
    pub experiment_key: String,
    pub variant: String,
    pub kind: EventKind,
}

/// What `/api/v1/events` expects, borrowed straight out of a drained map.
#[derive(Debug, Serialize)]
pub(crate) struct EventPayload<'a> {
    pub experiment_key: &'a str,
    pub variant: &'a str,
    pub kind: EventKind,
    pub count: u32,
}

/// The counter map, shared by every clone of the client.
#[derive(Debug, Default)]
pub(crate) struct Recorder {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    counts: HashMap<Cell, u64>,
    /// Increments refused by the cap since the last flush.
    dropped: u64,
}

impl Recorder {
    pub fn record(&self, experiment_key: &str, variant: &str, kind: EventKind) {
        let cell =
            Cell { experiment_key: experiment_key.to_owned(), variant: variant.to_owned(), kind };
        let mut state = self.state.lock().expect("recorder mutex poisoned");

        // Existing cells always increment; the cap only refuses *new* ones, so
        // a well-behaved experiment keeps counting while a runaway one is
        // contained.
        if let Some(count) = state.counts.get_mut(&cell) {
            *count += 1;
        } else if state.counts.len() >= MAX_CELLS {
            state.dropped += 1;
        } else {
            state.counts.insert(cell, 1);
        }
    }

    /// Takes everything recorded so far, leaving the map empty.
    pub fn drain(&self) -> (HashMap<Cell, u64>, u64) {
        let mut state = self.state.lock().expect("recorder mutex poisoned");
        (std::mem::take(&mut state.counts), std::mem::take(&mut state.dropped))
    }

    /// Puts a failed flush back so the next one retries it. Counts recorded in
    /// the meantime are merged rather than overwritten.
    pub fn restore(&self, counts: HashMap<Cell, u64>) {
        let mut state = self.state.lock().expect("recorder mutex poisoned");
        for (cell, count) in counts {
            if let Some(existing) = state.counts.get_mut(&cell) {
                *existing += count;
            } else if state.counts.len() >= MAX_CELLS {
                state.dropped += count;
            } else {
                state.counts.insert(cell, count);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(experiment: &str, variant: &str) -> Cell {
        Cell {
            experiment_key: experiment.to_owned(),
            variant: variant.to_owned(),
            kind: EventKind::Exposure,
        }
    }

    #[test]
    fn identical_events_pile_into_one_cell() {
        let recorder = Recorder::default();
        for _ in 0..5 {
            recorder.record("exp", "on", EventKind::Exposure);
        }
        recorder.record("exp", "on", EventKind::Conversion);

        let (counts, dropped) = recorder.drain();
        assert_eq!(counts.len(), 2);
        assert_eq!(counts[&cell("exp", "on")], 5);
        assert_eq!(dropped, 0);
        let (rest, _) = recorder.drain();
        assert!(rest.is_empty(), "drain must leave the map empty");
    }

    #[test]
    fn the_cap_refuses_new_cells_but_never_existing_ones() {
        let recorder = Recorder::default();
        for i in 0..MAX_CELLS {
            recorder.record(&format!("exp-{i}"), "on", EventKind::Exposure);
        }

        // A brand-new cell is refused...
        recorder.record("one-too-many", "on", EventKind::Exposure);
        // ...but a cell that already exists keeps counting.
        recorder.record("exp-0", "on", EventKind::Exposure);

        let (counts, dropped) = recorder.drain();
        assert_eq!(counts.len(), MAX_CELLS);
        assert_eq!(dropped, 1);
        assert_eq!(counts[&cell("exp-0", "on")], 2);
    }

    #[test]
    fn a_restored_flush_merges_with_what_arrived_meanwhile() {
        let recorder = Recorder::default();
        recorder.record("exp", "on", EventKind::Exposure);
        let (drained, _) = recorder.drain();

        // The flush "failed"; traffic kept arriving.
        recorder.record("exp", "on", EventKind::Exposure);
        recorder.restore(drained);

        let (counts, _) = recorder.drain();
        assert_eq!(counts[&cell("exp", "on")], 2, "nothing may be lost or double-counted");
    }
}
