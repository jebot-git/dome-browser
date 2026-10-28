// SPDX-License-Identifier: MPL-2.0
//! Capability-gated graphics-reset queries. A procedure address alone does not
//! establish support, and a context without reset notifications returns `None`.
use crate::{Error, Gl};
use glow::HasContext;
use std::ffi::c_void;

/// Result of a supported GL graphics-reset query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphicsResetStatus {
    /// No reset was observed since the preceding query, or a reset completed.
    NoError,
    /// The driver attributed the reset to this context.
    Guilty,
    /// The reset originated elsewhere.
    Innocent,
    /// The driver could not attribute the reset.
    Unknown,
}

fn query_name(
    major: u32,
    minor: u32,
    embedded: bool,
    notified: bool,
    has: impl Fn(&str) -> bool,
) -> Option<&'static str> {
    if !notified {
        return None;
    }
    if (major, minor) >= if embedded { (3, 2) } else { (4, 5) } {
        Some("glGetGraphicsResetStatus")
    } else if has("GL_KHR_robustness") {
        Some(if embedded {
            "glGetGraphicsResetStatusKHR"
        } else {
            "glGetGraphicsResetStatus"
        })
    } else if !embedded && has("GL_ARB_robustness") {
        Some("glGetGraphicsResetStatusARB")
    } else if embedded && has("GL_EXT_robustness") {
        Some("glGetGraphicsResetStatusEXT")
    } else {
        None
    }
}

fn decode(status: u32) -> Result<GraphicsResetStatus, Error> {
    match status {
        0 => Ok(GraphicsResetStatus::NoError),
        0x8253 => Ok(GraphicsResetStatus::Guilty),
        0x8254 => Ok(GraphicsResetStatus::Innocent),
        0x8255 => Ok(GraphicsResetStatus::Unknown),
        _ => Err(Error::Failed),
    }
}

pub(crate) struct ResetQuery(unsafe extern "system" fn() -> u32);
impl ResetQuery {
    pub(crate) unsafe fn poll(&self) -> Result<GraphicsResetStatus, Error> {
        decode((self.0)())
    }
}

pub(crate) fn load(
    gl: &Gl,
    notified: bool,
    mut get_proc: impl FnMut(&str) -> *const c_void,
) -> Result<Option<ResetQuery>, Error> {
    let version = gl.version();
    let Some(name) = query_name(
        version.major,
        version.minor,
        version.is_embedded,
        notified,
        |name| gl.supported_extensions().contains(name),
    ) else {
        return Ok(None);
    };
    let address = get_proc(name);
    if address.is_null() {
        return Err(Error::GLFunctionNotFound);
    }
    // The GL API/extension, notification strategy and entry-point spelling were
    // checked before looking up the native function with this fixed signature.
    Ok(Some(ResetQuery(unsafe {
        std::mem::transmute::<*const c_void, unsafe extern "system" fn() -> u32>(address)
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn name(
        version: (u32, u32),
        embedded: bool,
        notified: bool,
        extensions: &[&str],
    ) -> Option<&'static str> {
        query_name(version.0, version.1, embedded, notified, |value| {
            extensions.contains(&value)
        })
    }
    #[test]
    fn no_notifications_does_not_claim_reset_detection() {
        assert_eq!(
            name(
                (4, 6),
                false,
                false,
                &["GL_KHR_robustness", "GL_ARB_robustness"]
            ),
            None
        );
        assert_eq!(name((3, 2), true, false, &["GL_EXT_robustness"]), None);
    }
    #[test]
    fn core_versions_use_the_core_entry_point() {
        assert_eq!(
            name((4, 5), false, true, &[]),
            Some("glGetGraphicsResetStatus")
        );
        assert_eq!(
            name((3, 2), true, true, &[]),
            Some("glGetGraphicsResetStatus")
        );
        assert_eq!(name((4, 4), false, true, &[]), None);
        assert_eq!(name((3, 1), true, true, &[]), None);
    }
    #[test]
    fn khr_spelling_depends_on_the_client_api() {
        assert_eq!(
            name((3, 3), false, true, &["GL_KHR_robustness"]),
            Some("glGetGraphicsResetStatus")
        );
        assert_eq!(
            name((3, 1), true, true, &["GL_KHR_robustness"]),
            Some("glGetGraphicsResetStatusKHR")
        );
    }
    #[test]
    fn arb_is_desktop_only() {
        assert_eq!(
            name((2, 1), false, true, &["GL_ARB_robustness"]),
            Some("glGetGraphicsResetStatusARB")
        );
        assert_eq!(name((3, 1), true, true, &["GL_ARB_robustness"]), None);
    }
    #[test]
    fn ext_is_embedded_only() {
        assert_eq!(
            name((2, 0), true, true, &["GL_EXT_robustness"]),
            Some("glGetGraphicsResetStatusEXT")
        );
        assert_eq!(name((3, 3), false, true, &["GL_EXT_robustness"]), None);
    }
    #[test]
    fn reset_statuses_are_decoded_without_treating_unknown_values_as_healthy() {
        assert_eq!(decode(0).unwrap(), GraphicsResetStatus::NoError);
        assert_eq!(decode(0x8253).unwrap(), GraphicsResetStatus::Guilty);
        assert_eq!(decode(0x8254).unwrap(), GraphicsResetStatus::Innocent);
        assert_eq!(decode(0x8255).unwrap(), GraphicsResetStatus::Unknown);
        assert!(decode(0xdead).is_err());
    }
}
