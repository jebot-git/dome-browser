// SPDX-License-Identifier: MPL-2.0
//! Opt-in EGL context attributes. Extension tokens and client API both matter:
//! KHR robustness is for desktop GL, EXT robustness is for GLES, and EGL 1.5
//! supports both using the core attributes.
use crate::{ContextAttributeFlags, Error, GLApi, Gl};
use glow::HasContext;

const CONTEXT_FLAGS_KHR: i32 = 0x30fc;
const ROBUST_ACCESS_BIT_KHR: i32 = 0x0004;
const ROBUST_ACCESS_CORE: i32 = 0x31b2;
const RESET_STRATEGY_CORE: i32 = 0x31bd;
const ROBUST_ACCESS_EXT: i32 = 0x30bf;
const RESET_STRATEGY_EXT: i32 = 0x3138;
const LOSE_CONTEXT_ON_RESET: i32 = 0x31bf;
const GL_RESET_NOTIFICATION_STRATEGY: u32 = 0x8256;
const GL_LOSE_CONTEXT_ON_RESET: i32 = 0x8252;
const GL_CONTEXT_ROBUST_ACCESS: u32 = 0x90f3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum CreationApi {
    Core,
    Khr,
    Ext,
}

pub(super) fn creation_api(version: &str, extensions: &str, gl_api: GLApi) -> Option<CreationApi> {
    let token = version.split_ascii_whitespace().next().unwrap_or("");
    let mut parts = token.split('.');
    let major = parts.next().and_then(|s| s.parse::<u32>().ok());
    let minor = parts.next().and_then(|s| s.parse::<u32>().ok());
    let core = matches!((major, minor), (Some(a), Some(b)) if a > 1 || (a == 1 && b >= 5));
    let has = |name| extensions.split_ascii_whitespace().any(|s| s == name);
    if core {
        Some(CreationApi::Core)
    } else if gl_api == GLApi::GL && has("EGL_KHR_create_context") {
        Some(CreationApi::Khr)
    } else if gl_api == GLApi::GLES && has("EGL_EXT_create_context_robustness") {
        Some(CreationApi::Ext)
    } else {
        None
    }
}

pub(super) fn append_attributes(
    attrs: &mut Vec<i32>,
    flags: ContextAttributeFlags,
    api: Option<CreationApi>,
) -> Result<(), Error> {
    let notify = flags.contains(ContextAttributeFlags::RESET_NOTIFICATION);
    let robust = flags.contains(ContextAttributeFlags::ROBUST_ACCESS);
    if !notify && !robust {
        return Ok(());
    }
    // Never silently downgrade a requested strategy.
    let api = api.ok_or(Error::UnsupportedOnThisPlatform)?;
    if robust {
        attrs.extend(match api {
            CreationApi::Core => [ROBUST_ACCESS_CORE, 1],
            CreationApi::Khr => [CONTEXT_FLAGS_KHR, ROBUST_ACCESS_BIT_KHR],
            CreationApi::Ext => [ROBUST_ACCESS_EXT, 1],
        });
    }
    if notify {
        attrs.extend([
            match api {
                CreationApi::Core | CreationApi::Khr => RESET_STRATEGY_CORE,
                CreationApi::Ext => RESET_STRATEGY_EXT,
            },
            LOSE_CONTEXT_ON_RESET,
        ]);
    }
    Ok(())
}

// Used when describing an existing native context: report the actual strategy,
// not the attributes once requested. In particular, cloning a descriptor for a
// shared context must preserve its notification strategy.
pub(super) unsafe fn current_flags(gl: &Gl) -> ContextAttributeFlags {
    let version = gl.version();
    let core = if version.is_embedded {
        (version.major, version.minor) >= (3, 2)
    } else {
        (version.major, version.minor) >= (4, 5)
    };
    let extensions = gl.supported_extensions();
    let khr = extensions.contains("GL_KHR_robustness");
    let ext = version.is_embedded && extensions.contains("GL_EXT_robustness");
    let arb = !version.is_embedded && extensions.contains("GL_ARB_robustness");
    let mut flags = ContextAttributeFlags::empty();
    if core || khr || ext || arb {
        flags.set(
            ContextAttributeFlags::RESET_NOTIFICATION,
            gl.get_parameter_i32(GL_RESET_NOTIFICATION_STRATEGY) == GL_LOSE_CONTEXT_ON_RESET,
        );
        let robust = if core || khr || ext {
            gl.get_parameter_i32(GL_CONTEXT_ROBUST_ACCESS) != 0
        } else {
            gl.get_parameter_i32(glow::CONTEXT_FLAGS) & ROBUST_ACCESS_BIT_KHR != 0
        };
        flags.set(ContextAttributeFlags::ROBUST_ACCESS, robust);
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_attributes_are_unchanged_without_support() {
        let mut attrs = vec![0x3098, 3];
        let original = attrs.clone();
        append_attributes(&mut attrs, ContextAttributeFlags::ALPHA, None).unwrap();
        assert_eq!(attrs, original);
    }
    #[test]
    fn requested_unsupported_strategy_is_not_silently_downgraded() {
        for flags in [
            ContextAttributeFlags::RESET_NOTIFICATION,
            ContextAttributeFlags::ROBUST_ACCESS,
            ContextAttributeFlags::RESET_NOTIFICATION | ContextAttributeFlags::ROBUST_ACCESS,
        ] {
            let mut attrs = vec![0x3098, 3];
            let original = attrs.clone();
            assert!(append_attributes(&mut attrs, flags, None).is_err());
            assert_eq!(attrs, original);
        }
    }
    #[test]
    fn core_notification_supports_both_client_apis() {
        for gl_api in [GLApi::GL, GLApi::GLES] {
            let api = creation_api("1.5 Mesa", "", gl_api);
            assert_eq!(api, Some(CreationApi::Core));
            let mut attrs = vec![];
            append_attributes(
                &mut attrs,
                ContextAttributeFlags::RESET_NOTIFICATION | ContextAttributeFlags::ROBUST_ACCESS,
                api,
            )
            .unwrap();
            assert_eq!(
                attrs,
                [
                    ROBUST_ACCESS_CORE,
                    1,
                    RESET_STRATEGY_CORE,
                    LOSE_CONTEXT_ON_RESET
                ]
            );
        }
    }
    #[test]
    fn khr_notification_is_desktop_only() {
        assert_eq!(
            creation_api("1.4", "EGL_KHR_create_context", GLApi::GLES),
            None
        );
        let mut attrs = vec![];
        append_attributes(
            &mut attrs,
            ContextAttributeFlags::RESET_NOTIFICATION | ContextAttributeFlags::ROBUST_ACCESS,
            creation_api("1.4", "EGL_KHR_create_context", GLApi::GL),
        )
        .unwrap();
        assert_eq!(
            attrs,
            [
                CONTEXT_FLAGS_KHR,
                ROBUST_ACCESS_BIT_KHR,
                RESET_STRATEGY_CORE,
                LOSE_CONTEXT_ON_RESET
            ]
        );
    }
    #[test]
    fn ext_notification_is_gles_only() {
        assert_eq!(
            creation_api("1.4", "EGL_EXT_create_context_robustness", GLApi::GL),
            None
        );
        let mut attrs = vec![];
        append_attributes(
            &mut attrs,
            ContextAttributeFlags::RESET_NOTIFICATION | ContextAttributeFlags::ROBUST_ACCESS,
            creation_api("1.4", "EGL_EXT_create_context_robustness", GLApi::GLES),
        )
        .unwrap();
        assert_eq!(
            attrs,
            [
                ROBUST_ACCESS_EXT,
                1,
                RESET_STRATEGY_EXT,
                LOSE_CONTEXT_ON_RESET
            ]
        );
    }
    #[test]
    fn robust_access_alone_does_not_change_notification_strategy() {
        for (api, expected) in [
            (CreationApi::Core, [ROBUST_ACCESS_CORE, 1]),
            (CreationApi::Khr, [CONTEXT_FLAGS_KHR, ROBUST_ACCESS_BIT_KHR]),
            (CreationApi::Ext, [ROBUST_ACCESS_EXT, 1]),
        ] {
            let mut attrs = vec![];
            append_attributes(&mut attrs, ContextAttributeFlags::ROBUST_ACCESS, Some(api)).unwrap();
            assert_eq!(attrs, expected);
        }
    }
    #[test]
    fn notification_support_is_token_and_version_checked() {
        assert_eq!(
            creation_api("1.4", "EGL_KHR_create_context_suffix", GLApi::GL),
            None
        );
        assert_eq!(creation_api("invalid", "", GLApi::GL), None);
        assert_eq!(
            creation_api("1.10 Vendor", "", GLApi::GLES),
            Some(CreationApi::Core)
        );
        assert_eq!(
            creation_api(
                "1.5",
                "EGL_EXT_create_context_robustness EGL_KHR_create_context",
                GLApi::GL
            ),
            Some(CreationApi::Core)
        );
        assert_eq!(
            creation_api(
                "1.4",
                "EGL_EXT_create_context_robustness EGL_KHR_create_context",
                GLApi::GLES
            ),
            Some(CreationApi::Ext)
        );
    }
}
