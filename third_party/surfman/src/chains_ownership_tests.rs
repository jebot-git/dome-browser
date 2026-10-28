// SPDX-License-Identifier: MPL-2.0
//! Exercise production swap-chain ownership with ordinary backend errors.
//! The fake handles below never open a display or issue any GPU operation.
use super::*;
use crate::{ContextAttributeFlags, ContextAttributes, GLApi, GLVersion, SurfaceID};
use std::collections::{BTreeSet, VecDeque};
use std::ffi::c_void;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    CreateSurface,
    Bind,
    BindSurface(usize),
    Unbind,
    UnbindSurface(usize),
    CreateTexture,
    DestroyTexture,
    DestroySurface(usize),
    SurfaceInfo,
}

#[derive(Default)]
struct Ledger {
    next: usize,
    live: BTreeSet<usize>,
    dropped_live: Vec<usize>,
    destroyed: Vec<usize>,
    faults: VecDeque<Fault>,
}

struct TestSurface {
    id: usize,
    context: ContextID,
    size: Size2D<i32>,
    destroyed: bool,
    ledger: Arc<Mutex<Ledger>>,
}

impl Drop for TestSurface {
    fn drop(&mut self) {
        if !self.destroyed {
            self.ledger.lock().unwrap().dropped_live.push(self.id);
        }
    }
}

// Match the real backend's producer-local native texture wrapper.
struct TestTexture(TestSurface, std::marker::PhantomData<std::rc::Rc<()>>);
struct TestContext {
    id: ContextID,
    attached: Option<TestSurface>,
}
#[derive(Default)]
struct TestDevice(Arc<Mutex<Ledger>>);

impl TestDevice {
    fn fail(&self, fault: Fault) -> bool {
        let mut ledger = self.0.lock().unwrap();
        if ledger.faults.front() == Some(&fault) {
            ledger.faults.pop_front();
            true
        } else {
            false
        }
    }
    fn faults(&self, faults: &[Fault]) {
        self.0.lock().unwrap().faults.extend(faults);
    }
    fn surface(&self, context: ContextID, size: Size2D<i32>) -> TestSurface {
        let mut ledger = self.0.lock().unwrap();
        ledger.next += 1;
        let id = ledger.next;
        assert!(ledger.live.insert(id));
        TestSurface {
            id,
            context,
            size,
            destroyed: false,
            ledger: self.0.clone(),
        }
    }
    fn assert_retired(&self) {
        let ledger = self.0.lock().unwrap();
        assert!(
            ledger.dropped_live.is_empty(),
            "Dropped live handles: {:?}",
            ledger.dropped_live
        );
        assert!(
            ledger.live.is_empty(),
            "Unretired handles: {:?}",
            ledger.live
        );
    }
}

impl DeviceAPI for TestDevice {
    // Only the type relationships are used; the test never constructs this
    // platform connection, native widget, or a real GL dispatch table.
    type Connection = crate::Connection;
    type Context = TestContext;
    type ContextDescriptor = ();
    type Surface = TestSurface;
    type SurfaceTexture = TestTexture;

    fn connection(&self) -> Self::Connection {
        unimplemented!()
    }
    fn adapter(&self) -> crate::Adapter {
        unimplemented!()
    }
    fn gl_api(&self) -> GLApi {
        GLApi::GL
    }
    fn create_context_descriptor(&self, _: &ContextAttributes) -> Result<(), Error> {
        Ok(())
    }
    fn create_context(&self, _: &(), _: Option<&TestContext>) -> Result<TestContext, Error> {
        Ok(TestContext {
            id: ContextID(1),
            attached: None,
        })
    }
    fn destroy_context(&self, context: &mut TestContext) -> Result<(), Error> {
        if let Some(mut surface) = context.attached.take() {
            if let Err(error) = self.destroy_surface(context, &mut surface) {
                context.attached = Some(surface);
                return Err(error);
            }
        }
        Ok(())
    }
    fn context_descriptor(&self, _: &TestContext) {}
    fn make_context_current(&self, _: &TestContext) -> Result<(), Error> {
        Ok(())
    }
    fn make_no_context_current(&self) -> Result<(), Error> {
        Ok(())
    }
    fn context_descriptor_attributes(&self, _: &()) -> ContextAttributes {
        ContextAttributes {
            flags: ContextAttributeFlags::empty(),
            version: GLVersion::new(3, 3),
        }
    }
    fn get_proc_address(&self, _: &TestContext, _: &str) -> *const c_void {
        unimplemented!()
    }
    fn bind_surface_to_context(
        &self,
        context: &mut TestContext,
        surface: TestSurface,
    ) -> Result<(), (Error, TestSurface)> {
        if self.fail(Fault::Bind) || self.fail(Fault::BindSurface(surface.id)) {
            return Err((Error::Failed, surface));
        }
        if surface.context != context.id {
            return Err((Error::IncompatibleSurface, surface));
        }
        if context.attached.is_some() {
            return Err((Error::SurfaceAlreadyBound, surface));
        }
        context.attached = Some(surface);
        Ok(())
    }
    fn unbind_surface_from_context(
        &self,
        context: &mut TestContext,
    ) -> Result<Option<TestSurface>, Error> {
        if self.fail(Fault::Unbind)
            || self.fail(Fault::UnbindSurface(
                context.attached.as_ref().map_or(0, |surface| surface.id),
            ))
        {
            return Err(Error::Failed);
        }
        Ok(context.attached.take())
    }
    fn context_id(&self, context: &TestContext) -> ContextID {
        context.id
    }
    fn context_surface_info(&self, context: &TestContext) -> Result<Option<SurfaceInfo>, Error> {
        if self.fail(Fault::SurfaceInfo) {
            return Err(Error::Failed);
        }
        Ok(context
            .attached
            .as_ref()
            .map(|surface| self.surface_info(surface)))
    }
    fn create_surface(
        &self,
        context: &TestContext,
        _: SurfaceAccess,
        kind: SurfaceType<crate::NativeWidget>,
    ) -> Result<TestSurface, Error> {
        if self.fail(Fault::CreateSurface) {
            return Err(Error::Failed);
        }
        match kind {
            SurfaceType::Generic { size } => Ok(self.surface(context.id, size)),
            _ => Err(Error::UnsupportedOnThisPlatform),
        }
    }
    fn create_surface_texture(
        &self,
        _: &mut TestContext,
        surface: TestSurface,
    ) -> Result<TestTexture, (Error, TestSurface)> {
        if self.fail(Fault::CreateTexture) {
            return Err((Error::Failed, surface));
        }
        Ok(TestTexture(surface, std::marker::PhantomData))
    }
    fn destroy_surface(
        &self,
        context: &mut TestContext,
        surface: &mut TestSurface,
    ) -> Result<(), Error> {
        if surface.context != context.id {
            return Err(Error::IncompatibleSurface);
        }
        if self.fail(Fault::DestroySurface(surface.id)) {
            return Err(Error::Failed);
        }
        if !surface.destroyed {
            surface.destroyed = true;
            let mut ledger = self.0.lock().unwrap();
            assert!(ledger.live.remove(&surface.id));
            ledger.destroyed.push(surface.id);
        }
        Ok(())
    }
    fn destroy_surface_texture(
        &self,
        _: &mut TestContext,
        texture: TestTexture,
    ) -> Result<TestSurface, (Error, TestTexture)> {
        if self.fail(Fault::DestroyTexture) {
            return Err((Error::Failed, texture));
        }
        Ok(texture.0)
    }
    fn surface_gl_texture_target(&self) -> u32 {
        glow::TEXTURE_2D
    }
    fn present_bound_surface(&self, _: &mut TestContext) -> Result<(), Error> {
        Ok(())
    }
    fn present_surface(&self, _: &TestContext, _: &mut TestSurface) -> Result<(), Error> {
        Ok(())
    }
    fn resize_bound_surface(&self, _: &mut TestContext, _: Size2D<i32>) -> Result<(), Error> {
        unimplemented!()
    }
    fn resize_surface(
        &self,
        _: &TestContext,
        _: &mut TestSurface,
        _: Size2D<i32>,
    ) -> Result<(), Error> {
        unimplemented!()
    }
    fn surface_info(&self, surface: &TestSurface) -> SurfaceInfo {
        SurfaceInfo {
            id: SurfaceID(surface.id),
            context_id: surface.context,
            size: surface.size,
            framebuffer_object: Some(glow::NativeFramebuffer(
                std::num::NonZeroU32::new(surface.id as u32).unwrap(),
            )),
        }
    }
    fn surface_texture_object(&self, _: &TestTexture) -> Option<glow::Texture> {
        None
    }
}

fn attached() -> (TestDevice, TestContext, SwapChain<TestDevice>) {
    let device = TestDevice::default();
    let mut context = device.create_context(&(), None).unwrap();
    context.attached = Some(device.surface(context.id, Size2D::new(16, 16)));
    let chain = SwapChain::create_attached(&device, &mut context, SurfaceAccess::GPUOnly).unwrap();
    (device, context, chain)
}

fn finish(device: &TestDevice, context: &mut TestContext, chain: &SwapChain<TestDevice>) {
    // A failed operation must keep every handle available for subsequent
    // successful retirement. Clearing faults does not create replacement handles.
    device.0.lock().unwrap().faults.clear();
    chain.destroy(device, context).unwrap();
    device.destroy_context(context).unwrap();
    device.assert_retired();
}

#[test]
fn healthy_resize_and_texture_transfer_retire_every_handle() {
    let (device, mut context, chain) = attached();
    chain
        .resize(&device, &mut context, Size2D::new(32, 24))
        .unwrap();
    assert_eq!(chain.size(), Size2D::new(32, 24));
    let mut texture = Some(chain.take_surface_texture(&device, &mut context).unwrap());
    chain
        .recycle_surface_texture(&device, &mut context, &mut texture)
        .unwrap();
    assert!(texture.is_none());
    chain
        .swap_buffers(&device, &mut context, PreserveBuffer::No)
        .unwrap();
    finish(&device, &mut context, &chain);
}

#[test]
fn creation_failure_preserves_the_attached_back_buffer() {
    let (device, mut context, chain) = attached();
    device.faults(&[Fault::CreateSurface]);
    assert!(
        chain
            .resize(&device, &mut context, Size2D::new(32, 24))
            .is_err()
    );
    assert_eq!(chain.size(), Size2D::new(16, 16));
    assert_eq!(context.attached.as_ref().unwrap().id, 1);
    finish(&device, &mut context, &chain);
}

#[test]
fn resize_retains_old_buffer_when_retirement_fails() {
    let (device, mut context, chain) = attached();
    device.faults(&[Fault::DestroySurface(1)]);
    assert!(
        chain
            .resize(&device, &mut context, Size2D::new(32, 24))
            .is_err()
    );
    let actual_size = context.attached.as_ref().unwrap().size;
    let recorded_size = chain.size();
    finish(&device, &mut context, &chain);
    assert_eq!(recorded_size, actual_size);
}

#[test]
fn resize_retains_all_recycled_surfaces_after_one_cleanup_error() {
    let (device, mut context, chain) = attached();
    for _ in 0..3 {
        chain
            .lock()
            .recycled_surfaces
            .push(device.surface(context.id, Size2D::new(16, 16)));
    }
    device.faults(&[Fault::DestroySurface(2)]);
    assert!(
        chain
            .resize(&device, &mut context, Size2D::new(32, 24))
            .is_err()
    );
    finish(&device, &mut context, &chain);
}

#[test]
fn resize_unbind_failure_retains_the_new_allocation() {
    let (device, mut context, chain) = attached();
    device.faults(&[Fault::Unbind]);
    assert!(
        chain
            .resize(&device, &mut context, Size2D::new(32, 24))
            .is_err()
    );
    assert_eq!(context.attached.as_ref().unwrap().id, 1);
    finish(&device, &mut context, &chain);
}

#[test]
fn swap_unbind_failure_retains_the_new_allocation() {
    let (device, mut context, chain) = attached();
    device.faults(&[Fault::Unbind]);
    assert!(
        chain
            .swap_buffers(&device, &mut context, PreserveBuffer::No)
            .is_err()
    );
    assert_eq!(context.attached.as_ref().unwrap().id, 1);
    finish(&device, &mut context, &chain);
}

#[test]
fn swap_bind_and_cleanup_failures_preserve_both_buffers() {
    let (device, mut context, chain) = attached();
    device.faults(&[Fault::Bind, Fault::DestroySurface(2)]);
    assert!(
        chain
            .swap_buffers(&device, &mut context, PreserveBuffer::No)
            .is_err()
    );
    finish(&device, &mut context, &chain);
}

#[test]
fn texture_release_failure_retains_the_surface_texture() {
    let (device, mut context, chain) = attached();
    let mut texture = Some(chain.take_surface_texture(&device, &mut context).unwrap());
    device.faults(&[Fault::DestroyTexture]);
    assert!(
        chain
            .recycle_surface_texture(&device, &mut context, &mut texture)
            .is_err()
    );
    assert_eq!(texture.as_ref().unwrap().0.id, 1);
    chain
        .recycle_surface_texture(&device, &mut context, &mut texture)
        .unwrap();
    assert!(texture.is_none());
    finish(&device, &mut context, &chain);
}

#[test]
fn shared_chain_remains_send_with_a_thread_local_texture_type() {
    fn is_send<T: Send>() {}
    is_send::<SwapChain<TestDevice>>();
}

#[test]
fn texture_creation_and_rollback_failures_keep_the_original_surface() {
    let (device, mut context, chain) = attached();
    device.faults(&[Fault::CreateTexture, Fault::Bind, Fault::DestroySurface(1)]);
    assert!(chain.take_surface_texture(&device, &mut context).is_err());
    finish(&device, &mut context, &chain);
}

#[test]
fn attached_creation_returns_the_surface_info_error() {
    let device = TestDevice::default();
    let mut context = device.create_context(&(), None).unwrap();
    device.faults(&[Fault::SurfaceInfo]);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        SwapChain::create_attached(&device, &mut context, SurfaceAccess::GPUOnly)
    }));
    assert!(
        matches!(result, Ok(Err(_))),
        "Creation must return an error without unwinding"
    );
    device.assert_retired();
}

#[test]
fn swap_retry_restores_rendering_after_unbind_failure() {
    let (device, mut context, chain) = attached();
    device.faults(&[Fault::Unbind]);
    assert!(
        chain
            .swap_buffers(&device, &mut context, PreserveBuffer::No)
            .is_err()
    );
    chain
        .swap_buffers(&device, &mut context, PreserveBuffer::No)
        .unwrap();
    assert!(chain.is_attached());
    assert_eq!(context.attached.as_ref().unwrap().id, 2);
    assert_eq!(chain.lock().pending_surface.as_ref().unwrap().id, 1);
    finish(&device, &mut context, &chain);
}

#[test]
fn repeated_bind_failures_keep_the_original_attachment_role_for_retry() {
    let (device, mut context, chain) = attached();
    device.faults(&[Fault::Bind, Fault::Bind]);
    assert!(
        chain
            .swap_buffers(&device, &mut context, PreserveBuffer::No)
            .is_err()
    );
    assert!(chain.is_attached());
    assert!(context.attached.is_none());
    chain
        .swap_buffers(&device, &mut context, PreserveBuffer::No)
        .unwrap();
    assert!(chain.is_attached());
    assert_eq!(context.attached.as_ref().unwrap().id, 2);
    assert_eq!(device.0.lock().unwrap().next, 2);
    finish(&device, &mut context, &chain);
}

#[test]
fn resize_retry_recovers_after_failed_replacement_and_rollback() {
    let (device, mut context, chain) = attached();
    device.faults(&[Fault::Bind, Fault::Bind]);
    assert!(
        chain
            .resize(&device, &mut context, Size2D::new(32, 24))
            .is_err()
    );
    assert_eq!(chain.size(), Size2D::new(16, 16));
    assert!(chain.is_attached());
    chain
        .resize(&device, &mut context, Size2D::new(32, 24))
        .unwrap();
    assert_eq!(context.attached.as_ref().unwrap().size, chain.size());
    assert_eq!(chain.size(), Size2D::new(32, 24));
    finish(&device, &mut context, &chain);
}

#[test]
fn failed_retirement_is_never_presented_or_reused_for_drawing() {
    let (device, mut context, chain) = attached();
    device.faults(&[Fault::DestroySurface(1)]);
    assert!(
        chain
            .resize(&device, &mut context, Size2D::new(32, 24))
            .is_err()
    );
    assert!(SwapChainAPI::take_surface(&chain).is_none());
    assert_eq!(chain.lock().retired_surfaces[0].id, 1);
    chain
        .swap_buffers(&device, &mut context, PreserveBuffer::No)
        .unwrap();
    assert_ne!(context.attached.as_ref().unwrap().id, 1);
    let frame = chain.take_pending_surface().unwrap();
    assert_eq!(frame.id, 2);
    SwapChainAPI::recycle_surface(&chain, frame);
    finish(&device, &mut context, &chain);
}

#[test]
fn borrowed_texture_defers_chain_destruction_until_the_producer_returns_it() {
    let (device, mut context, chain) = attached();
    let mut texture = Some(chain.take_surface_texture(&device, &mut context).unwrap());
    assert!(chain.destroy(&device, &mut context).is_err());
    assert_eq!(texture.as_ref().unwrap().0.id, 1);
    chain
        .recycle_surface_texture(&device, &mut context, &mut texture)
        .unwrap();
    assert!(texture.is_none());
    finish(&device, &mut context, &chain);
}

#[test]
fn failed_texture_rebind_retains_a_sendable_surface_instead_of_the_wrapper() {
    let (device, mut context, chain) = attached();
    let mut texture = Some(chain.take_surface_texture(&device, &mut context).unwrap());
    device.faults(&[Fault::Bind]);
    assert!(
        chain
            .recycle_surface_texture(&device, &mut context, &mut texture)
            .is_err()
    );
    assert!(texture.is_none());
    assert!(chain.is_attached());
    chain
        .swap_buffers(&device, &mut context, PreserveBuffer::No)
        .unwrap();
    assert!(context.attached.is_some());
    finish(&device, &mut context, &chain);
}

#[test]
fn already_retiring_buffers_survive_repeated_retirement_errors() {
    let (device, mut context, chain) = attached();
    device.faults(&[Fault::DestroySurface(1), Fault::DestroySurface(1)]);
    assert!(chain.destroy(&device, &mut context).is_err());
    assert!(chain.destroy(&device, &mut context).is_err());
    assert!(SwapChainAPI::take_surface(&chain).is_none());
    assert!(
        chain
            .swap_buffers(&device, &mut context, PreserveBuffer::No)
            .is_err()
    );
    assert!(
        chain
            .resize(&device, &mut context, Size2D::new(32, 24))
            .is_err()
    );
    assert_eq!(device.0.lock().unwrap().next, 1);
    finish(&device, &mut context, &chain);
    assert_eq!(device.0.lock().unwrap().destroyed, [1]);
}

#[test]
fn attachment_transfer_retries_after_unbind_failure() {
    let (device, mut context, original) = attached();
    let replacement = SwapChain::create_detached(
        &device,
        &mut context,
        SurfaceAccess::GPUOnly,
        Size2D::new(8, 8),
    )
    .unwrap();
    device.faults(&[Fault::Unbind]);
    assert!(
        replacement
            .take_attachment_from(&device, &mut context, &original)
            .is_err()
    );
    assert!(!replacement.is_attached());
    assert!(original.is_attached());
    assert_eq!(context.attached.as_ref().unwrap().id, 1);
    replacement
        .take_attachment_from(&device, &mut context, &original)
        .unwrap();
    assert!(replacement.is_attached());
    assert!(!original.is_attached());
    assert_eq!(context.attached.as_ref().unwrap().id, 2);
    original.destroy(&device, &mut context).unwrap();
    finish(&device, &mut context, &replacement);
}

#[test]
fn attachment_transfer_retries_after_failed_bind_and_rollback() {
    let (device, mut context, original) = attached();
    let replacement = SwapChain::create_detached(
        &device,
        &mut context,
        SurfaceAccess::GPUOnly,
        Size2D::new(8, 8),
    )
    .unwrap();
    device.faults(&[Fault::Bind, Fault::Bind]);
    assert!(
        replacement
            .take_attachment_from(&device, &mut context, &original)
            .is_err()
    );
    assert!(!replacement.is_attached());
    assert!(original.is_attached());
    assert!(context.attached.is_none());
    replacement
        .take_attachment_from(&device, &mut context, &original)
        .unwrap();
    assert_eq!(context.attached.as_ref().unwrap().id, 2);
    original.destroy(&device, &mut context).unwrap();
    finish(&device, &mut context, &replacement);
}

#[test]
fn self_attachment_transfer_returns_an_error_without_locking_twice() {
    let (device, mut context, chain) = attached();
    assert!(
        chain
            .take_attachment_from(&device, &mut context, &chain.clone())
            .is_err()
    );
    finish(&device, &mut context, &chain);
}

#[test]
fn absent_surface_info_is_a_normal_creation_error() {
    let device = TestDevice::default();
    let mut context = device.create_context(&(), None).unwrap();
    assert!(SwapChain::create_attached(&device, &mut context, SurfaceAccess::GPUOnly).is_err());
    device.assert_retired();
}

#[test]
fn invalid_texture_destination_preserves_the_producer_owned_wrapper() {
    let (device, mut context, attached) = attached();
    let detached = SwapChain::create_detached(
        &device,
        &mut context,
        SurfaceAccess::GPUOnly,
        Size2D::new(8, 8),
    )
    .unwrap();
    let mut texture = Some(
        detached
            .take_surface_texture(&device, &mut context)
            .unwrap(),
    );
    assert!(
        attached
            .recycle_surface_texture(&device, &mut context, &mut texture)
            .is_err()
    );
    assert_eq!(texture.as_ref().unwrap().0.id, 2);
    detached
        .recycle_surface_texture(&device, &mut context, &mut texture)
        .unwrap();
    detached.destroy(&device, &mut context).unwrap();
    finish(&device, &mut context, &attached);
}

// A tiny in-memory GL dispatch table for observing production clear-state
// restoration. No function in this table forwards to a native driver.
mod mock_gl {
    use super::*;
    use std::cell::RefCell;

    #[derive(Clone, Debug, PartialEq)]
    pub(super) struct Snapshot {
        draw: u32,
        read: u32,
        color: [f32; 4],
        depth: f32,
        stencil: i32,
        color_mask: [bool; 4],
        depth_mask: bool,
        front_mask: u32,
        back_mask: u32,
        scissor: bool,
        discard: bool,
    }
    impl Default for Snapshot {
        fn default() -> Self {
            Self {
                draw: 17,
                read: 19,
                color: [0.25, 0.5, 0.75, 0.5],
                depth: 0.25,
                stencil: 123,
                color_mask: [true, false, true, false],
                depth_mask: false,
                front_mask: 0xa5a55a5a,
                back_mask: 0x13579bdf,
                scissor: true,
                discard: false,
            }
        }
    }
    #[derive(Default)]
    struct Driver {
        state: Snapshot,
        cleared: Vec<(u32, Snapshot)>,
        unknown: Vec<u32>,
    }
    thread_local! { static DRIVER: RefCell<Driver> = RefCell::new(Driver::default()); }

    unsafe extern "system" fn get_string(_: u32) -> *const u8 {
        c"3.3 ownership fixture".as_ptr().cast()
    }
    unsafe extern "system" fn get_string_i(_: u32, _: u32) -> *const u8 {
        c"".as_ptr().cast()
    }
    unsafe extern "system" fn get_integer(name: u32, out: *mut i32) {
        let value = DRIVER.with(|driver| {
            let mut d = driver.borrow_mut();
            match name {
                glow::NUM_EXTENSIONS => 0,
                glow::DRAW_FRAMEBUFFER_BINDING => d.state.draw as i32,
                glow::READ_FRAMEBUFFER_BINDING => d.state.read as i32,
                glow::STENCIL_CLEAR_VALUE => d.state.stencil,
                glow::STENCIL_WRITEMASK => d.state.front_mask.min(i32::MAX as u32) as i32,
                glow::STENCIL_BACK_WRITEMASK => d.state.back_mask.min(i32::MAX as u32) as i32,
                _ => {
                    d.unknown.push(name);
                    0
                },
            }
        });
        unsafe {
            out.write(value);
        }
    }
    unsafe extern "system" fn get_double(name: u32, out: *mut f64) {
        let value = DRIVER.with(|driver| {
            let mut d = driver.borrow_mut();
            match name {
                glow::STENCIL_WRITEMASK => d.state.front_mask as f64,
                glow::STENCIL_BACK_WRITEMASK => d.state.back_mask as f64,
                _ => {
                    d.unknown.push(name);
                    0.
                },
            }
        });
        unsafe {
            out.write(value);
        }
    }
    unsafe extern "system" fn get_float(name: u32, out: *mut f32) {
        DRIVER.with(|driver| {
            let mut d = driver.borrow_mut();
            match name {
                glow::COLOR_CLEAR_VALUE => unsafe {
                    std::ptr::copy_nonoverlapping(d.state.color.as_ptr(), out, 4);
                },
                glow::DEPTH_CLEAR_VALUE => unsafe {
                    out.write(d.state.depth);
                },
                _ => {
                    d.unknown.push(name);
                    unsafe {
                        out.write(0.);
                    }
                },
            }
        });
    }
    unsafe extern "system" fn get_boolean(name: u32, out: *mut u8) {
        DRIVER.with(|driver| {
            let mut d = driver.borrow_mut();
            match name {
                glow::COLOR_WRITEMASK => {
                    for (i, bit) in d.state.color_mask.iter().enumerate() {
                        unsafe {
                            out.add(i).write(u8::from(*bit));
                        }
                    }
                },
                glow::DEPTH_WRITEMASK => unsafe {
                    out.write(u8::from(d.state.depth_mask));
                },
                _ => {
                    d.unknown.push(name);
                    unsafe {
                        out.write(0);
                    }
                },
            }
        });
    }
    unsafe extern "system" fn is_enabled(name: u32) -> u8 {
        DRIVER.with(|driver| {
            let mut d = driver.borrow_mut();
            u8::from(match name {
                glow::SCISSOR_TEST => d.state.scissor,
                glow::RASTERIZER_DISCARD => d.state.discard,
                _ => {
                    d.unknown.push(name);
                    false
                },
            })
        })
    }
    fn enable_value(name: u32, enabled: bool) {
        DRIVER.with(|driver| {
            let mut d = driver.borrow_mut();
            match name {
                glow::SCISSOR_TEST => d.state.scissor = enabled,
                glow::RASTERIZER_DISCARD => d.state.discard = enabled,
                _ => d.unknown.push(name),
            }
        });
    }
    unsafe extern "system" fn enable(name: u32) {
        enable_value(name, true);
    }
    unsafe extern "system" fn disable(name: u32) {
        enable_value(name, false);
    }
    unsafe extern "system" fn bind_framebuffer(target: u32, name: u32) {
        DRIVER.with(|driver| {
            let mut d = driver.borrow_mut();
            match target {
                glow::FRAMEBUFFER => {
                    d.state.draw = name;
                    d.state.read = name;
                },
                glow::DRAW_FRAMEBUFFER => d.state.draw = name,
                glow::READ_FRAMEBUFFER => d.state.read = name,
                _ => d.unknown.push(target),
            }
        });
    }
    unsafe extern "system" fn clear_color(r: f32, g: f32, b: f32, a: f32) {
        DRIVER.with(|d| d.borrow_mut().state.color = [r, g, b, a]);
    }
    unsafe extern "system" fn clear_depth(value: f64) {
        DRIVER.with(|d| d.borrow_mut().state.depth = value as f32);
    }
    unsafe extern "system" fn clear_stencil(value: i32) {
        DRIVER.with(|d| d.borrow_mut().state.stencil = value);
    }
    unsafe extern "system" fn depth_mask(value: u8) {
        DRIVER.with(|d| d.borrow_mut().state.depth_mask = value != 0);
    }
    unsafe extern "system" fn color_mask(r: u8, g: u8, b: u8, a: u8) {
        DRIVER.with(|d| d.borrow_mut().state.color_mask = [r != 0, g != 0, b != 0, a != 0]);
    }
    unsafe extern "system" fn stencil_mask(value: u32) {
        DRIVER.with(|d| {
            let mut d = d.borrow_mut();
            d.state.front_mask = value;
            d.state.back_mask = value;
        });
    }
    unsafe extern "system" fn stencil_mask_separate(face: u32, value: u32) {
        DRIVER.with(|d| {
            let mut d = d.borrow_mut();
            match face {
                glow::FRONT => d.state.front_mask = value,
                glow::BACK => d.state.back_mask = value,
                _ => d.unknown.push(face),
            }
        });
    }
    unsafe extern "system" fn clear(mask: u32) {
        DRIVER.with(|d| {
            let mut d = d.borrow_mut();
            let state = d.state.clone();
            d.cleared.push((mask, state));
        });
    }
    pub(super) fn context() -> Gl {
        context_with_exact_query(true)
    }
    pub(super) fn context_with_exact_query(exact: bool) -> Gl {
        DRIVER.with(|d| *d.borrow_mut() = Driver::default());
        unsafe {
            Gl::try_from_loader_function(|name| match name {
                "glGetString" => get_string as *const () as *const c_void,
                "glGetStringi" => get_string_i as *const () as *const c_void,
                "glGetIntegerv" => get_integer as *const () as *const c_void,
                "glGetDoublev" if exact => get_double as *const () as *const c_void,
                "glGetFloatv" => get_float as *const () as *const c_void,
                "glGetBooleanv" => get_boolean as *const () as *const c_void,
                "glIsEnabled" => is_enabled as *const () as *const c_void,
                "glEnable" => enable as *const () as *const c_void,
                "glDisable" => disable as *const () as *const c_void,
                "glBindFramebuffer" => bind_framebuffer as *const () as *const c_void,
                "glClearColor" => clear_color as *const () as *const c_void,
                "glClearDepth" => clear_depth as *const () as *const c_void,
                "glClearStencil" => clear_stencil as *const () as *const c_void,
                "glDepthMask" => depth_mask as *const () as *const c_void,
                "glColorMask" => color_mask as *const () as *const c_void,
                "glStencilMask" => stencil_mask as *const () as *const c_void,
                "glStencilMaskSeparate" => stencil_mask_separate as *const () as *const c_void,
                "glClear" => clear as *const () as *const c_void,
                _ => std::ptr::null(),
            })
            .unwrap()
        }
    }
    pub(super) fn snapshot() -> Snapshot {
        DRIVER.with(|d| d.borrow().state.clone())
    }
    pub(super) fn cleared() -> Vec<u32> {
        DRIVER.with(|d| {
            let d = d.borrow();
            assert!(d.unknown.is_empty(), "Unmodelled queries: {:?}", d.unknown);
            d.cleared.iter().map(|(_, state)| state.draw).collect()
        })
    }

    pub(super) fn assert_clear(index: usize, color: [f32; 4]) {
        DRIVER.with(|d| {
            let d = d.borrow();
            let (mask, state) = &d.cleared[index];
            assert_eq!(
                *mask,
                glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT | glow::STENCIL_BUFFER_BIT
            );
            assert_eq!(state.color, color);
            assert_eq!(state.depth, 1.);
            assert_eq!(state.stencil, 0);
            assert_eq!(state.color_mask, [true; 4]);
            assert!(state.depth_mask);
            assert_eq!(state.front_mask, u32::MAX);
            assert_eq!(state.back_mask, u32::MAX);
            assert!(!state.scissor && !state.discard);
        });
    }
}

#[test]
fn clear_preserves_distinct_framebuffers_masks_and_enable_state() {
    let (device, mut context, chain) = attached();
    let gl = mock_gl::context();
    let before = mock_gl::snapshot();
    chain
        .clear_surface(&device, &mut context, &gl, [0., 1., 0., 1.])
        .unwrap();
    assert_eq!(mock_gl::snapshot(), before);
    assert_eq!(mock_gl::cleared(), [1]);
    mock_gl::assert_clear(0, [0., 1., 0., 1.]);
    finish(&device, &mut context, &chain);
}

#[test]
fn clear_surface_info_error_restores_state_without_clearing() {
    let (device, mut context, chain) = attached();
    let gl = mock_gl::context();
    let before = mock_gl::snapshot();
    device.faults(&[Fault::SurfaceInfo]);
    assert!(
        chain
            .clear_surface(&device, &mut context, &gl, [0.; 4])
            .is_err()
    );
    assert_eq!(mock_gl::snapshot(), before);
    assert!(mock_gl::cleared().is_empty());
    finish(&device, &mut context, &chain);
}

#[test]
fn clear_a_borrowed_back_buffer_returns_an_error_without_gl_changes() {
    let (device, mut context, chain) = attached();
    let gl = mock_gl::context();
    let before = mock_gl::snapshot();
    let mut texture = Some(chain.take_surface_texture(&device, &mut context).unwrap());
    assert!(
        chain
            .clear_surface(&device, &mut context, &gl, [0.; 4])
            .is_err()
    );
    assert_eq!(mock_gl::snapshot(), before);
    assert!(mock_gl::cleared().is_empty());
    chain
        .recycle_surface_texture(&device, &mut context, &mut texture)
        .unwrap();
    chain
        .clear_surface(&device, &mut context, &gl, [0.; 4])
        .unwrap();
    finish(&device, &mut context, &chain);
}

#[test]
fn detached_clear_restores_the_previous_attachment_and_gl_state() {
    let (device, mut context, original) = attached();
    let detached = SwapChain::create_detached(
        &device,
        &mut context,
        SurfaceAccess::GPUOnly,
        Size2D::new(8, 8),
    )
    .unwrap();
    let gl = mock_gl::context();
    let before = mock_gl::snapshot();
    detached
        .clear_surface(&device, &mut context, &gl, [0.; 4])
        .unwrap();
    assert_eq!(context.attached.as_ref().unwrap().id, 1);
    assert!(!detached.is_attached());
    assert_eq!(mock_gl::snapshot(), before);
    assert_eq!(mock_gl::cleared(), [2]);
    detached.destroy(&device, &mut context).unwrap();
    finish(&device, &mut context, &original);
}

#[test]
fn detached_clear_without_an_existing_attachment_restores_its_surface() {
    let device = TestDevice::default();
    let mut context = device.create_context(&(), None).unwrap();
    let detached = SwapChain::create_detached(
        &device,
        &mut context,
        SurfaceAccess::GPUOnly,
        Size2D::new(8, 8),
    )
    .unwrap();
    let gl = mock_gl::context();
    let before = mock_gl::snapshot();
    detached
        .clear_surface(&device, &mut context, &gl, [0.; 4])
        .unwrap();
    assert!(context.attached.is_none());
    assert!(!detached.is_attached());
    assert_eq!(mock_gl::snapshot(), before);
    assert_eq!(mock_gl::cleared(), [1]);
    finish(&device, &mut context, &detached);
}

#[test]
fn detached_clear_recovers_from_initial_unbind_or_bind_errors() {
    for fault in [Fault::Unbind, Fault::Bind, Fault::SurfaceInfo] {
        let (device, mut context, original) = attached();
        let detached = SwapChain::create_detached(
            &device,
            &mut context,
            SurfaceAccess::GPUOnly,
            Size2D::new(8, 8),
        )
        .unwrap();
        let gl = mock_gl::context();
        let before = mock_gl::snapshot();
        device.faults(&[fault]);
        assert!(
            detached
                .clear_surface(&device, &mut context, &gl, [0.; 4])
                .is_err()
        );
        assert_eq!(context.attached.as_ref().unwrap().id, 1);
        assert_eq!(mock_gl::snapshot(), before);
        detached
            .clear_surface(&device, &mut context, &gl, [0.; 4])
            .unwrap();
        assert_eq!(context.attached.as_ref().unwrap().id, 1);
        assert_eq!(mock_gl::snapshot(), before);
        detached.destroy(&device, &mut context).unwrap();
        finish(&device, &mut context, &original);
    }
}

#[test]
fn detached_clear_retries_repeated_failed_reattachment_without_losing_either_surface() {
    let (device, mut context, original) = attached();
    let detached = SwapChain::create_detached(
        &device,
        &mut context,
        SurfaceAccess::GPUOnly,
        Size2D::new(8, 8),
    )
    .unwrap();
    let gl = mock_gl::context();
    let before = mock_gl::snapshot();
    device.faults(&[
        Fault::BindSurface(1),
        Fault::BindSurface(1),
        Fault::BindSurface(1),
    ]);
    for _ in 0..3 {
        assert!(
            detached
                .clear_surface(&device, &mut context, &gl, [0.; 4])
                .is_err()
        );
        assert_eq!(mock_gl::snapshot(), before);
        assert!(context.attached.is_none());
    }
    assert_eq!(mock_gl::cleared(), [2]);
    detached
        .clear_surface(&device, &mut context, &gl, [0.; 4])
        .unwrap();
    assert_eq!(mock_gl::cleared(), [2, 2]);
    assert_eq!(context.attached.as_ref().unwrap().id, 1);
    detached.destroy(&device, &mut context).unwrap();
    finish(&device, &mut context, &original);
}

#[test]
fn detached_clear_retries_a_failed_temporary_unbind() {
    let (device, mut context, original) = attached();
    let detached = SwapChain::create_detached(
        &device,
        &mut context,
        SurfaceAccess::GPUOnly,
        Size2D::new(8, 8),
    )
    .unwrap();
    let gl = mock_gl::context();
    let before = mock_gl::snapshot();
    device.faults(&[Fault::UnbindSurface(2)]);
    assert!(
        detached
            .clear_surface(&device, &mut context, &gl, [0.; 4])
            .is_err()
    );
    assert_eq!(context.attached.as_ref().unwrap().id, 2);
    assert_eq!(mock_gl::snapshot(), before);
    detached
        .clear_surface(&device, &mut context, &gl, [0.; 4])
        .unwrap();
    assert_eq!(context.attached.as_ref().unwrap().id, 1);
    assert_eq!(mock_gl::snapshot(), before);
    assert_eq!(mock_gl::cleared(), [2, 2]);
    detached.destroy(&device, &mut context).unwrap();
    finish(&device, &mut context, &original);
}

#[test]
fn pending_foreign_attachment_is_restored_before_destruction() {
    let (device, mut context, original) = attached();
    let detached = SwapChain::create_detached(
        &device,
        &mut context,
        SurfaceAccess::GPUOnly,
        Size2D::new(8, 8),
    )
    .unwrap();
    let gl = mock_gl::context();
    device.faults(&[Fault::BindSurface(1), Fault::BindSurface(1)]);
    assert!(
        detached
            .clear_surface(&device, &mut context, &gl, [0.; 4])
            .is_err()
    );
    assert!(detached.destroy(&device, &mut context).is_err());
    assert_eq!(device.0.lock().unwrap().live.len(), 2);
    detached.destroy(&device, &mut context).unwrap();
    assert_eq!(context.attached.as_ref().unwrap().id, 1);
    finish(&device, &mut context, &original);
}

#[test]
fn attachment_recovery_does_not_repeat_an_already_completed_clear() {
    let (device, mut context, original) = attached();
    let detached = SwapChain::create_detached(
        &device,
        &mut context,
        SurfaceAccess::GPUOnly,
        Size2D::new(8, 8),
    )
    .unwrap();
    let gl = mock_gl::context();
    let before = mock_gl::snapshot();
    device.faults(&[Fault::BindSurface(1)]);
    assert!(
        detached
            .clear_surface(&device, &mut context, &gl, [0.; 4])
            .is_err()
    );
    assert_eq!(mock_gl::cleared(), [2]);
    detached.recover_attachment(&device, &mut context).unwrap();
    assert_eq!(context.attached.as_ref().unwrap().id, 1);
    assert_eq!(mock_gl::cleared(), [2]);
    assert_eq!(mock_gl::snapshot(), before);
    detached.destroy(&device, &mut context).unwrap();
    finish(&device, &mut context, &original);
}

#[test]
fn missing_exact_mask_query_returns_before_clearing_or_changing_state() {
    let (device, mut context, chain) = attached();
    let gl = mock_gl::context_with_exact_query(false);
    let before = mock_gl::snapshot();
    assert!(
        chain
            .clear_surface(&device, &mut context, &gl, [0.; 4])
            .is_err()
    );
    assert_eq!(mock_gl::snapshot(), before);
    assert!(mock_gl::cleared().is_empty());
    assert_eq!(context.attached.as_ref().unwrap().id, 1);
    finish(&device, &mut context, &chain);
}

#[test]
fn consumer_loan_blocks_retirement_until_returned() {
    let (device, mut context, chain) = attached();
    chain
        .swap_buffers(&device, &mut context, PreserveBuffer::No)
        .unwrap();
    let frame = chain.take_pending_surface().unwrap();
    let waiting = chain.destroy(&device, &mut context).is_err();
    let sealed = SwapChainAPI::take_surface(&chain).is_none();
    SwapChainAPI::recycle_surface(&chain, frame);
    finish(&device, &mut context, &chain);
    assert!(
        waiting,
        "producer retired while a consumer still held its surface"
    );
    assert!(sealed, "retiring chain issued another consumer loan");
}

#[test]
fn recycled_consumer_loan_also_blocks_retirement() {
    let (device, mut context, chain) = attached();
    chain
        .swap_buffers(&device, &mut context, PreserveBuffer::No)
        .unwrap();
    let frame = SwapChainAPI::take_surface(&chain).unwrap();
    SwapChainAPI::recycle_surface(&chain, frame);
    let frame = SwapChainAPI::take_surface(&chain).unwrap();
    let waiting = chain.destroy(&device, &mut context).is_err();
    SwapChainAPI::recycle_surface(&chain, frame);
    finish(&device, &mut context, &chain);
    assert!(waiting);
}

#[test]
fn retirement_waits_for_each_consumer_independently() {
    let (device, mut context, chain) = attached();
    chain
        .swap_buffers(&device, &mut context, PreserveBuffer::No)
        .unwrap();
    let first = chain.take_pending_surface().unwrap();
    chain
        .swap_buffers(&device, &mut context, PreserveBuffer::No)
        .unwrap();
    let second = chain.take_pending_surface().unwrap();
    let waiting_for_two = chain.destroy(&device, &mut context).is_err();
    SwapChainAPI::recycle_surface(&chain, second);
    let waiting_for_one = chain.destroy(&device, &mut context).is_err();
    SwapChainAPI::recycle_surface(&chain, first);
    finish(&device, &mut context, &chain);
    assert!(waiting_for_two && waiting_for_one);
}

#[test]
fn consumer_failed_import_returns_raw_loan() {
    let (device, mut context, chain) = attached();
    chain
        .swap_buffers(&device, &mut context, PreserveBuffer::No)
        .unwrap();
    device.faults(&[Fault::CreateTexture]);
    assert!(SurfaceConsumer::acquire(chain.clone(), &device, &mut context).is_err());
    finish(&device, &mut context, &chain);
}
#[test]
fn consumer_failed_export_retains_texture_and_blocks_retirement() {
    let (device, mut context, chain) = attached();
    chain
        .swap_buffers(&device, &mut context, PreserveBuffer::No)
        .unwrap();
    let mut consumer = SurfaceConsumer::acquire(chain.clone(), &device, &mut context)
        .unwrap()
        .unwrap();
    device.faults(&[Fault::DestroyTexture]);
    assert!(consumer.release(&device, &mut context).is_err());
    assert!(consumer.texture().is_some());
    assert!(chain.destroy(&device, &mut context).is_err());
    consumer.release(&device, &mut context).unwrap();
    assert!(consumer.texture().is_none());
    consumer.release(&device, &mut context).unwrap();
    finish(&device, &mut context, &chain);
}
