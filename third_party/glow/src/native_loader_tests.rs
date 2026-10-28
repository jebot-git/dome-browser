// SPDX-License-Identifier: MIT OR Apache-2.0 OR Zlib
use super::Context;
use crate::{HasContext, EXTENSIONS, MAX_LABEL_LENGTH, NUM_EXTENSIONS, RENDERER, VERSION};
use std::cell::Cell;
use std::ffi::c_void;
use std::ptr;

#[derive(Clone, Copy)]
enum Metadata {
    Legacy,
    Core,
    Embedded,
    NullVersion,
    InvalidVersion,
    InvalidVersionUtf8,
    VersionVendorNonUtf8,
    NullLegacyExtensions,
    InvalidLegacyExtensions,
    NullIndexedExtension,
    InvalidIndexedExtension,
    NegativeExtensionCount,
    NegativeLabelLength,
    Vanished,
    Changed,
    NullStrings,
    InvalidStrings,
}
thread_local! {
    static METADATA: Cell<Metadata> = const { Cell::new(Metadata::Legacy) };
    static VERSION_READS: Cell<u32> = const { Cell::new(0) };
}
fn configure(metadata: Metadata) {
    METADATA.set(metadata);
    VERSION_READS.set(0);
}
unsafe extern "system" fn get_string(parameter: u32) -> *const u8 {
    let metadata = METADATA.get();
    if matches!(metadata, Metadata::NullStrings) {
        return ptr::null();
    }
    if matches!(metadata, Metadata::InvalidStrings) {
        return b"\xff\0".as_ptr();
    }
    if parameter == VERSION {
        let reads = VERSION_READS.get();
        VERSION_READS.set(reads + 1);
        return match metadata {
            Metadata::NullVersion => ptr::null(),
            Metadata::InvalidVersion => b"not a GL version\0".as_ptr(),
            Metadata::InvalidVersionUtf8 => b"\xff\0".as_ptr(),
            Metadata::VersionVendorNonUtf8 => b"3.3 Vend\xffr\0".as_ptr(),
            Metadata::Vanished if reads > 0 => ptr::null(),
            Metadata::Changed if reads > 0 => b"3.2 Different context\0".as_ptr(),
            Metadata::Legacy
            | Metadata::NullLegacyExtensions
            | Metadata::InvalidLegacyExtensions => b"2.1 Fixture\0".as_ptr(),
            Metadata::Embedded => b"OpenGL ES 3.2 Fixture\0".as_ptr(),
            Metadata::NegativeLabelLength => b"4.6 Fixture\0".as_ptr(),
            _ => b"3.3 Fixture\0".as_ptr(),
        };
    }
    match metadata {
        Metadata::NullLegacyExtensions => ptr::null(),
        Metadata::InvalidLegacyExtensions => b"\xff\0".as_ptr(),
        _ => b"\0".as_ptr(),
    }
}
unsafe extern "system" fn get_integer(parameter: u32, output: *mut i32) {
    *output = match parameter {
        NUM_EXTENSIONS if matches!(METADATA.get(), Metadata::NegativeExtensionCount) => -1,
        NUM_EXTENSIONS => 1,
        MAX_LABEL_LENGTH if matches!(METADATA.get(), Metadata::NegativeLabelLength) => -1,
        MAX_LABEL_LENGTH => 256,
        _ => 0,
    };
}
unsafe extern "system" fn get_indexed_string(_: u32, _: u32) -> *const u8 {
    match METADATA.get() {
        Metadata::NullIndexedExtension | Metadata::NullStrings => ptr::null(),
        Metadata::InvalidIndexedExtension | Metadata::InvalidStrings => b"\xff\0".as_ptr(),
        _ => b"GL_EXT_fixture\0".as_ptr(),
    }
}
fn load(name: &str) -> *const c_void {
    match name {
        "glGetString" => get_string as *const c_void,
        "glGetIntegerv" => get_integer as *const c_void,
        "glGetStringi" => get_indexed_string as *const c_void,
        _ => ptr::null(),
    }
}
fn rejects(metadata: Metadata, message: &str) {
    configure(metadata);
    let error = unsafe { Context::try_from_loader_function(load) }.unwrap_err();
    assert!(
        error.contains(message),
        "unexpected initialization error: {error}"
    );
}
#[test]
fn missing_required_entry_points_are_errors() {
    for missing in ["glGetString", "glGetIntegerv", "glGetStringi"] {
        configure(Metadata::Core);
        let error = unsafe {
            Context::try_from_loader_function(|name| {
                if name == missing {
                    ptr::null()
                } else {
                    load(name)
                }
            })
        }
        .unwrap_err();
        assert!(error.contains(missing));
    }
}
#[test]
fn null_version_is_an_error() {
    rejects(Metadata::NullVersion, "GL_VERSION");
}
#[test]
fn invalid_version_is_an_error() {
    rejects(Metadata::InvalidVersion, "Invalid GL_VERSION");
}
#[test]
fn invalid_version_utf8_is_an_error() {
    rejects(Metadata::InvalidVersionUtf8, "Invalid GL_VERSION");
}
#[test]
fn null_legacy_extension_string_is_an_error() {
    rejects(Metadata::NullLegacyExtensions, "GL_EXTENSIONS");
}
#[test]
fn legacy_extension_text_without_utf8_remains_opaque() {
    configure(Metadata::InvalidLegacyExtensions);
    let context = unsafe { Context::try_from_loader_function(load) }.unwrap();
    assert!(context.supported_extensions().contains("\u{fffd}"));
}
#[test]
fn null_indexed_extension_is_an_error() {
    rejects(Metadata::NullIndexedExtension, "GL_EXTENSIONS");
}
#[test]
fn indexed_extension_text_without_utf8_remains_opaque() {
    configure(Metadata::InvalidIndexedExtension);
    let context = unsafe { Context::try_from_loader_function(load) }.unwrap();
    assert!(context.supported_extensions().contains("\u{fffd}"));
}
#[test]
fn negative_extension_count_is_an_error() {
    rejects(Metadata::NegativeExtensionCount, "GL_NUM_EXTENSIONS");
}
#[test]
fn negative_label_limit_is_an_error() {
    rejects(Metadata::NegativeLabelLength, "GL_MAX_LABEL_LENGTH");
}
#[test]
fn context_disappearing_during_initialization_is_an_error() {
    rejects(Metadata::Vanished, "GL_VERSION");
}
#[test]
fn context_changing_during_initialization_is_an_error() {
    rejects(Metadata::Changed, "changed");
}
#[test]
fn healthy_legacy_metadata_has_no_empty_extension() {
    configure(Metadata::Legacy);
    let context = unsafe { Context::try_from_loader_function(load) }.unwrap();
    assert_eq!((context.version().major, context.version().minor), (2, 1));
    assert!(context.supported_extensions().is_empty());
}
#[test]
fn healthy_core_metadata_is_cached() {
    configure(Metadata::Core);
    let context = unsafe { Context::try_from_loader_function(load) }.unwrap();
    assert_eq!((context.version().major, context.version().minor), (3, 3));
    assert!(context.supported_extensions().contains("GL_EXT_fixture"));
}
#[test]
fn healthy_embedded_metadata_is_cached() {
    configure(Metadata::Embedded);
    let context = unsafe { Context::try_from_loader_function(load) }.unwrap();
    assert!(context.version().is_embedded);
    assert_eq!((context.version().major, context.version().minor), (3, 2));
}
#[test]
fn ordinary_string_queries_tolerate_context_unavailability() {
    configure(Metadata::Core);
    let context = unsafe { Context::try_from_loader_function(load) }.unwrap();
    configure(Metadata::NullStrings);
    unsafe {
        assert_eq!(context.get_parameter_string(RENDERER), "");
        assert_eq!(context.get_parameter_indexed_string(EXTENSIONS, 0), "");
    }
}
#[test]
fn ordinary_string_queries_tolerate_non_utf8_driver_text() {
    configure(Metadata::Core);
    let context = unsafe { Context::try_from_loader_function(load) }.unwrap();
    configure(Metadata::InvalidStrings);
    unsafe {
        assert_eq!(context.get_parameter_string(RENDERER), "\u{fffd}");
        assert_eq!(
            context.get_parameter_indexed_string(EXTENSIONS, 0),
            "\u{fffd}"
        );
    }
}
#[test]
fn existing_constructor_keeps_healthy_context_behavior() {
    configure(Metadata::Core);
    let context = unsafe { Context::from_loader_function(load) };
    assert_eq!(context.version().major, 3);
}
#[test]
fn fallible_cstr_constructor_uses_the_same_checks() {
    configure(Metadata::Core);
    let context =
        unsafe { Context::try_from_loader_function_cstr(|name| load(name.to_str().unwrap())) }
            .unwrap();
    assert_eq!(context.version().major, 3);
}

#[test]
fn implementation_defined_vendor_text_does_not_need_utf8() {
    configure(Metadata::VersionVendorNonUtf8);
    let context = unsafe { Context::try_from_loader_function(load) }.unwrap();
    assert_eq!(context.version().vendor_info, "Vend\u{fffd}r");
    unsafe { context.validate_current_version() }.unwrap();
}
#[test]
fn cached_version_validation_reports_unavailability() {
    configure(Metadata::Core);
    let context = unsafe { Context::try_from_loader_function(load) }.unwrap();
    configure(Metadata::NullStrings);
    assert!(unsafe { context.validate_current_version() }.is_err());
}

#[test]
fn cached_version_validation_rejects_changed_metadata() {
    configure(Metadata::Core);
    let context = unsafe { Context::try_from_loader_function(load) }.unwrap();
    configure(Metadata::Legacy);
    assert!(unsafe { context.validate_current_version() }.is_err());
}
