// SPDX-License-Identifier: GPL-3.0-or-later
//! Map an opaque runtime epoch to the local monotonic clock, never to wall time.
use servo_base::cross_process_instant::CrossProcessInstant;

#[derive(Default)]
pub(super) struct Clock {
    anchor: Option<(i64, u64)>,
}
impl Clock {
    pub fn observe(&mut self, runtime: i64, monotonic: Option<i64>) {
        let received =
            (CrossProcessInstant::now() - CrossProcessInstant::epoch()).whole_nanoseconds();
        self.observe_at(runtime, monotonic, u64::try_from(received).unwrap_or(0));
    }
    fn observe_at(&mut self, runtime: i64, monotonic: Option<i64>, received: u64) {
        if let Some(instant) = monotonic
            .and_then(|value| u64::try_from(value).ok())
            .filter(|value| *value > 0)
        {
            self.anchor = Some((runtime, instant));
        } else if self.anchor.is_none() {
            // Legacy hosts have no clock conversion. Anchor their elapsed time
            // to the first received frame as an estimate on the correct domain.
            self.anchor = Some((runtime, received));
        }
    }
    fn nanos(&self, runtime: i64, monotonic: Option<i64>) -> Option<u64> {
        let mapped = if let Some(monotonic) = monotonic {
            u64::try_from(monotonic).ok().filter(|value| *value > 0)?
        } else {
            let (origin, local) = self.anchor?;
            local.checked_add_signed(runtime.checked_sub(origin)?)?
        };
        (mapped <= i64::MAX as u64).then_some(mapped)
    }
    pub fn instant(&self, runtime: i64, monotonic: Option<i64>) -> Option<CrossProcessInstant> {
        #[cfg(all(unix, not(any(target_os = "macos", target_os = "ios"))))]
        {
            CrossProcessInstant::from_monotonic_nanos(self.nanos(runtime, monotonic)?)
        }
        #[cfg(not(all(unix, not(any(target_os = "macos", target_os = "ios")))))]
        {
            let _ = (runtime, monotonic);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_epoch_never_becomes_the_document_clock() {
        let mut clock = Clock::default();
        clock.observe_at(700_000_000_000_000, Some(30_000_000_000), 29_999_000_000);
        assert_eq!(clock.nanos(700_000_002_000_000, None), Some(30_002_000_000));
        assert_eq!(
            clock.nanos(700_000_000_000_000, Some(30_000_000_000)),
            Some(30_000_000_000)
        );
    }
    #[test]
    fn legacy_clock_estimate_preserves_elapsed_and_historical_event_time() {
        let mut clock = Clock::default();
        clock.observe_at(1_000_000_000, None, 50_000_000_000);
        clock.observe_at(1_010_000_000, None, 50_025_000_000);
        assert_eq!(clock.nanos(1_020_000_000, None), Some(50_020_000_000));
        assert_eq!(clock.nanos(990_000_000, None), Some(49_990_000_000));
    }
    #[test]
    fn precise_conversion_replaces_estimate_and_survives_temporary_conversion_loss() {
        let mut clock = Clock::default();
        clock.observe_at(100, None, 1000);
        clock.observe_at(110, Some(900), 1010);
        clock.observe_at(120, None, 1020);
        assert_eq!(clock.nanos(120, None), Some(910));
        assert_eq!(clock.nanos(105, Some(895)), Some(895));
    }
    #[test]
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "ios"))))]
    fn converted_instants_share_the_actual_cross_process_clock_domain() {
        let now = CrossProcessInstant::now();
        let nanos =
            i64::try_from((now - CrossProcessInstant::epoch()).whole_nanoseconds()).unwrap();
        assert_eq!(
            Clock::default().instant(700_000_000_000_000, Some(nanos)),
            Some(now)
        );
        assert!(CrossProcessInstant::from_monotonic_nanos(u64::MAX).is_none());
    }
    #[test]
    fn invalid_or_unrepresentable_clocks_do_not_escape_as_timestamps() {
        let mut clock = Clock::default();
        clock.observe_at(100, None, 1);
        assert_eq!(clock.nanos(1, None), None);
        assert_eq!(clock.nanos(100, Some(-1)), None);
        assert_eq!(clock.nanos(100, Some(0)), None);
        clock.observe_at(1, Some(i64::MAX), 1);
        assert_eq!(clock.nanos(2, None), None);
    }
}
