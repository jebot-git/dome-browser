// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::renderbuffers::allocation_tests::{self as driver, SurfaceFailure};
use crate::{ContextAttributeFlags, GLVersion, WindowingApiError};
use std::cell::Cell;
fn attributes() -> ContextAttributes {
    ContextAttributes {
        version: GLVersion::new(3, 3),
        flags: ContextAttributeFlags::DEPTH | ContextAttributeFlags::STENCIL,
    }
}
#[test]
fn allocation_surface_failures_release_partial_names_and_restore_distinct_bindings() {
    for failure in [
        SurfaceFailure::TextureName,
        SurfaceFailure::TextureStorage,
        SurfaceFailure::FramebufferName,
        SurfaceFailure::FramebufferIncomplete,
        SurfaceFailure::RenderbufferName,
        SurfaceFailure::RenderbufferStorage,
        SurfaceFailure::MissingFramebufferFunction,
    ] {
        let gl = driver::surface_context(false, failure);
        let image_calls = Cell::new(0);
        let result = EGLBackedSurface::new_generic_with_image(
            &gl,
            ContextID(1),
            &attributes(),
            &Size2D::new(8, 8),
            |_| {
                image_calls.set(image_calls.get() + 1);
                Err(Error::SurfaceCreationFailed(WindowingApiError::BadAlloc))
            },
        );
        assert!(result.is_err());
        assert_eq!(
            image_calls.get(),
            0,
            "EGL image acquired before all GL allocations succeeded"
        );
        driver::assert_surface_clean();
    }
}
#[test]
fn allocation_egl_image_failure_releases_all_gl_names() {
    let gl = driver::surface_context(false, SurfaceFailure::None);
    let calls = Cell::new(0);
    let result = EGLBackedSurface::new_generic_with_image(
        &gl,
        ContextID(1),
        &attributes(),
        &Size2D::new(8, 8),
        |_| {
            calls.set(calls.get() + 1);
            Err(Error::SurfaceCreationFailed(WindowingApiError::BadAlloc))
        },
    );
    assert!(result.is_err());
    assert_eq!(calls.get(), 1);
    driver::assert_surface_clean();
}
#[test]
fn allocation_egl_loss_defers_gl_names_without_further_gl_commands() {
    let gl = driver::surface_context(false, SurfaceFailure::None);
    let result = EGLBackedSurface::new_generic_with_image(
        &gl,
        ContextID(1),
        &attributes(),
        &Size2D::new(8, 8),
        |_| {
            driver::mark_native_loss();
            Err(Error::SurfaceCreationFailed(WindowingApiError::ContextLost))
        },
    );
    assert!(matches!(
        result,
        Err(Error::SurfaceCreationFailed(WindowingApiError::ContextLost))
    ));
    driver::assert_loss_deferred();
}
#[test]
fn allocation_legacy_es2_avoids_separate_framebuffer_and_unpack_buffer_queries() {
    let gl = driver::surface_context(true, SurfaceFailure::None);
    let result = EGLBackedSurface::new_generic_with_image(
        &gl,
        ContextID(1),
        &attributes(),
        &Size2D::new(8, 8),
        |_| Err(Error::SurfaceCreationFailed(WindowingApiError::BadAlloc)),
    );
    assert!(result.is_err());
    driver::assert_surface_clean();
}
