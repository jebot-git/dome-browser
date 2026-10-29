// SPDX-License-Identifier: GPL-3.0-or-later
//! Nominal cadence from the compositor's predicted period, not measured RAF throughput.

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct State {
    rate: Option<f32>,
}

fn from_period(period: Option<i64>) -> Option<f32> {
    period
        .filter(|period| *period > 0)
        .map(|period| {
            let rate = (1_000_000_000.0 / period as f64) as f32;
            let integer = rate.round();
            if (rate - integer).abs() <= rate * 0.000001 { integer } else { rate }
        })
}

impl State {
    pub fn new(period: Option<i64>) -> Self {
        Self {
            rate: from_period(period),
        }
    }

    pub fn value(&self) -> Option<f32> {
        self.rate
    }

    pub fn observe(&mut self, period: Option<i64>) -> Option<f32> {
        let next = from_period(period)?;
        // Integer nanosecond quantization can alternate adjacent periods at a
        // fixed refresh rate. Keep that rounding noise out of DOM events.
        if self
            .rate
            .is_some_and(|old| (old - next).abs() <= old.max(next) * 0.000001)
        {
            return None;
        }
        self.rate = Some(next);
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_rate_is_known_only_from_a_meaningful_period() {
        for period in [None, Some(0), Some(-1)] {
            assert!(State::new(period).value().is_none());
        }
        assert_eq!(State::new(Some(11_111_111)).value(), Some(90.));
        assert!((State::new(Some(16_683_350)).value().unwrap() - 59.94).abs() < 0.0001);
    }

    #[test]
    fn changes_preserve_fractional_rates_and_skip_duplicate_samples() {
        let mut state = State::new(Some(11_111_111));
        assert_eq!(state.observe(Some(8_333_333)), Some(120.));
        assert_eq!(state.observe(Some(8_333_333)), None);
        let fractional = state.observe(Some(16_683_350)).unwrap();
        assert!((fractional - 59.94).abs() < 0.0001);
        assert_eq!(state.value(), Some(fractional));
    }

    #[test]
    fn nanosecond_quantization_does_not_repeat_rate_change_events() {
        let mut state = State::new(Some(11_111_111));
        for period in [11_111_112, 11_111_111, 11_111_110, 11_111_112] {
            assert_eq!(state.observe(Some(period)), None);
        }
        assert_eq!(state.value(), Some(90.));
    }

    #[test]
    fn missing_metadata_does_not_replace_the_last_known_nominal_rate() {
        let mut state = State::new(Some(11_111_111));
        for period in [None, Some(0), Some(-1)] {
            assert_eq!(state.observe(period), None);
            assert_eq!(state.value(), Some(90.));
        }
        let mut unknown = State::default();
        assert_eq!(unknown.observe(Some(11_111_111)), Some(90.));
    }
}
