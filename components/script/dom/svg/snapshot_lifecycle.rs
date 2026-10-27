/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

/// Coalesce mutations until the snapshot already being decoded/rasterized finishes.
#[derive(Clone, Copy, Default)]
pub(crate) struct SnapshotLifecycle {
    in_flight: bool,
    dirty: bool,
}

impl SnapshotLifecycle {
    pub(crate) fn in_flight(self) -> bool {
        self.in_flight
    }

    /// Return true when the caller may replace the current serialized snapshot.
    pub(crate) fn invalidate(&mut self) -> bool {
        self.dirty = true;
        !self.in_flight
    }

    pub(crate) fn begin(&mut self) -> bool {
        if self.in_flight {
            return false;
        }
        self.in_flight = true;
        self.dirty = false;
        true
    }

    /// Return whether a newer snapshot should be built from the latest DOM.
    pub(crate) fn complete(&mut self) -> bool {
        self.in_flight = false;
        std::mem::take(&mut self.dirty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn continuous_mutations_do_not_cancel_the_in_flight_frame() {
        let mut state = SnapshotLifecycle::default();
        assert!(state.begin());
        for _ in 0..1000 {
            assert!(!state.invalidate());
            assert!(!state.begin());
        }
        assert!(state.complete());
        assert!(state.begin());
        assert!(!state.complete());
    }
    #[test]
    fn idle_completion_does_not_schedule_endless_updates() {
        let mut state = SnapshotLifecycle::default();
        assert!(state.begin());
        assert!(!state.complete());
        assert!(!state.in_flight());
        assert!(state.invalidate());
    }
}
