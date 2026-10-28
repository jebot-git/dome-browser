// SPDX-License-Identifier: GPL-3.0-or-later
//! Display failure stays local to a tab while the application event loop runs.
use image::RgbaImage;
use servo::RenderingContext;
use std::{
    cell::Cell,
    time::{Duration, Instant},
};
#[derive(Default)]
pub struct RetryState {
    unavailable: Cell<bool>,
    due: Cell<Option<Instant>>,
    delay: Cell<Duration>,
}
impl RetryState {
    pub fn due(&self, now: Instant) -> bool {
        self.due.get().is_none_or(|due| now >= due)
    }
    pub fn failure(&self, now: Instant) -> bool {
        let first = !self.unavailable.replace(true);
        let delay = if first {
            Duration::from_millis(20)
        } else {
            (self.delay.get() * 2).min(Duration::from_millis(500))
        };
        self.delay.set(delay);
        self.due.set(Some(now + delay));
        first
    }
    pub fn recover(&self) -> bool {
        self.due.set(None);
        self.delay.set(Duration::ZERO);
        self.unavailable.replace(false)
    }
}
pub fn capture(
    context: &dyn RenderingContext,
    paint: impl FnOnce(),
) -> Result<RgbaImage, surfman::Error> {
    if context.native_context_is_lost() {
        return Err(surfman::Error::Failed);
    }
    context.make_current()?;
    if context.native_context_is_lost() {
        return Err(surfman::Error::Failed);
    }
    let size = context.size();
    if size.width == 0
        || size.height == 0
        || size.width > i32::MAX as u32
        || size.height > i32::MAX as u32
    {
        return Err(surfman::Error::Failed);
    }
    paint();
    context
        .read_to_image(servo::DeviceIntRect::from_size(euclid::Size2D::new(
            size.width as i32,
            size.height as i32,
        )))
        .ok_or(surfman::Error::Failed)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_display_backs_off_but_recovery_resets_the_next_attempt() {
        let state = RetryState::default();
        let now = Instant::now();
        assert!(state.due(now));
        assert!(state.failure(now));
        assert!(!state.due(now));
        assert!(state.due(now + Duration::from_millis(20)));
        assert!(!state.failure(now));
        assert!(!state.due(now + Duration::from_millis(20)));
        assert!(state.recover() && state.due(now));
        assert!(!state.recover());
    }
}
