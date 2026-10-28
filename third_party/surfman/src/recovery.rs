// SPDX-License-Identifier: MPL-2.0
//! Optional robust-context creation and supported reset observation.
use crate::device::Device;
use crate::{
    ContextAttributeFlags as Flags, ContextAttributes, Error, GraphicsResetStatus,
    WindowingApiError,
};
use std::{
    cell::Cell,
    time::{Duration, Instant},
};

/// Create an unshared context, preferring robust access and reset notifications.
/// Only rejection of optional context attributes allows a compatibility retry.
/// Explicit robustness flags remain mandatory; allocation/device errors propagate.
pub fn create_context_with_optional_robustness<D: Device>(
    device: &D,
    attributes: ContextAttributes,
) -> Result<D::Context, Error> {
    negotiate(
        attributes,
        device.supports_context_loss_recovery(),
        |attributes| {
            let descriptor = device.create_context_descriptor(&attributes)?;
            device.create_context(&descriptor, None)
        },
    )
}

fn negotiate<T>(
    attributes: ContextAttributes,
    supported: bool,
    mut create: impl FnMut(ContextAttributes) -> Result<T, Error>,
) -> Result<T, Error> {
    let optional = Flags::ROBUST_ACCESS | Flags::RESET_NOTIFICATION;
    if !supported || attributes.flags.intersects(optional) {
        return create(attributes);
    }
    for flags in [optional, Flags::ROBUST_ACCESS, Flags::RESET_NOTIFICATION] {
        match create(ContextAttributes {
            flags: attributes.flags | flags,
            ..attributes
        }) {
            Err(
                Error::UnsupportedOnThisPlatform
                | Error::ContextCreationFailed(
                    WindowingApiError::BadAttribute | WindowingApiError::BadMatch,
                ),
            ) => {},
            result => return result,
        }
    }
    create(attributes)
}

/// Rate-limit optional reset queries on one context's owner thread. Unsupported
/// contexts are queried once. A positive reset marks native ownership as lost
/// before the caller can issue ordinary rendering commands.
pub struct ResetStatusMonitor {
    enabled: Cell<bool>,
    next: Cell<Option<Instant>>,
}
impl Default for ResetStatusMonitor {
    fn default() -> Self {
        Self::new()
    }
}
impl ResetStatusMonitor {
    /// The idle polling interval also bounds detection for inactive WebGL views.
    pub const INTERVAL: Duration = Duration::from_millis(250);
    /// Start with an immediate capability query.
    pub fn new() -> Self {
        Self {
            enabled: Cell::new(true),
            next: Cell::new(None),
        }
    }
    /// Whether this context still needs supported reset observation.
    pub fn enabled(&self) -> bool {
        self.enabled.get()
    }
    /// Check a current or temporarily selected context without rendering into it.
    pub fn check<D: Device>(&self, device: &D, context: &mut D::Context) -> Result<(), Error> {
        if device.context_is_lost(context) {
            return Err(lost());
        }
        let result = self.poll(Instant::now(), || device.graphics_reset_status(context));
        if result.as_ref().is_err_and(Error::is_context_lost)
            && device.supports_context_loss_recovery()
        {
            let _ = device.mark_context_lost(context);
        }
        result
    }
    fn poll(
        &self,
        now: Instant,
        query: impl FnOnce() -> Result<Option<GraphicsResetStatus>, Error>,
    ) -> Result<(), Error> {
        if !self.enabled.get() || self.next.get().is_some_and(|next| now < next) {
            return Ok(());
        }
        self.next.set(Some(now + Self::INTERVAL));
        match query()? {
            None => {
                self.enabled.set(false);
                Ok(())
            },
            Some(GraphicsResetStatus::NoError) => Ok(()),
            Some(_) => Err(lost()),
        }
    }
}
fn lost() -> Error {
    Error::MakeCurrentFailed(WindowingApiError::ContextLost)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn attributes() -> ContextAttributes {
        ContextAttributes {
            version: crate::GLVersion::new(3, 2),
            flags: Flags::ALPHA,
        }
    }
    #[test]
    fn rejected_optional_attributes_keep_base_format_and_try_compatible_strategies() {
        let base = attributes();
        let mut attempts = vec![];
        let value = negotiate(base, true, |a| {
            assert_eq!(a.version, base.version);
            assert!(a.flags.contains(base.flags));
            attempts.push(a.flags);
            if a.flags == base.flags {
                Ok(1)
            } else {
                Err(Error::ContextCreationFailed(
                    WindowingApiError::BadAttribute,
                ))
            }
        })
        .unwrap();
        assert_eq!(value, 1);
        assert_eq!(attempts.len(), 4);
        assert_eq!(
            attempts[0],
            base.flags | Flags::ROBUST_ACCESS | Flags::RESET_NOTIFICATION
        );
        assert_eq!(attempts[3], base.flags);
    }
    #[test]
    fn robust_access_survives_an_unsupported_notification_strategy() {
        let mut calls = 0;
        let flags = negotiate(attributes(), true, |a| {
            calls += 1;
            if a.flags.contains(Flags::RESET_NOTIFICATION) {
                Err(Error::UnsupportedOnThisPlatform)
            } else {
                Ok(a.flags)
            }
        })
        .unwrap();
        assert!(flags.contains(Flags::ROBUST_ACCESS));
        assert_eq!(calls, 2);
    }
    #[test]
    fn resource_and_loss_errors_never_trigger_compatibility_fallback() {
        for error in [
            Error::ContextCreationFailed(WindowingApiError::BadAlloc),
            lost(),
            Error::DeviceOpenFailed,
            Error::GLContextInitializationFailed("fixture".into()),
        ] {
            let mut failure = Some(error);
            let mut calls = 0;
            assert!(
                negotiate(attributes(), true, |_| -> Result<(), Error> {
                    calls += 1;
                    Err(failure.take().unwrap())
                })
                .is_err()
            );
            assert_eq!(calls, 1);
        }
    }
    #[test]
    fn explicit_strategies_are_never_downgraded() {
        let mut a = attributes();
        a.flags |= Flags::RESET_NOTIFICATION;
        let mut calls = 0;
        assert!(
            negotiate(a, true, |b| -> Result<(), Error> {
                calls += 1;
                assert_eq!(a, b);
                Err(Error::UnsupportedOnThisPlatform)
            })
            .is_err()
        );
        assert_eq!(calls, 1);
    }
    #[test]
    fn unsupported_backend_makes_one_legacy_creation_attempt() {
        let a = attributes();
        let mut calls = 0;
        negotiate(a, false, |b| {
            calls += 1;
            assert_eq!(a, b);
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, 1);
    }
    #[test]
    fn healthy_queries_are_bounded_and_unsupported_queries_stop() {
        let monitor = ResetStatusMonitor::new();
        let now = Instant::now();
        monitor
            .poll(now, || Ok(Some(GraphicsResetStatus::NoError)))
            .unwrap();
        monitor.poll(now, || panic!("early driver query")).unwrap();
        monitor
            .poll(now + ResetStatusMonitor::INTERVAL, || Ok(None))
            .unwrap();
        assert!(!monitor.enabled());
        monitor
            .poll(now + Duration::from_secs(10), || {
                panic!("unsupported driver query")
            })
            .unwrap();
    }
    #[test]
    fn every_reported_reset_stops_rendering() {
        for status in [
            GraphicsResetStatus::Guilty,
            GraphicsResetStatus::Innocent,
            GraphicsResetStatus::Unknown,
        ] {
            assert!(
                ResetStatusMonitor::new()
                    .poll(Instant::now(), || Ok(Some(status)))
                    .unwrap_err()
                    .is_context_lost()
            );
        }
    }
}
