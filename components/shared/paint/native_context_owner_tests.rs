// SPDX-License-Identifier: MPL-2.0
// Private surfaceless contexts only; no display session or physical reset.
#![expect(unsafe_code)]
use std::cell::Cell;
use std::ffi::c_void;
use std::rc::Rc;

use super::*;
use surfman::chains::{PreserveBuffer, SwapChainAPI};

#[link(name = "EGL")]
unsafe extern "C" {
    fn eglGetCurrentDisplay() -> *mut c_void;
    fn eglGetCurrentContext() -> *mut c_void;
    fn eglQueryContext(
        display: *mut c_void,
        context: *mut c_void,
        attribute: i32,
        value: *mut i32,
    ) -> u32;
    fn eglGetError() -> i32;
}
type MesaConnection = surfman::mesa_surfaceless::connection::Connection;

fn raw_owner() -> (NativeContextOwner, MesaConnection) {
    let connection = MesaConnection::new().unwrap();
    let adapter = if std::env::var_os("DOME_CREATION_SOFTWARE").is_some() {
        connection.create_software_adapter().unwrap()
    } else {
        connection.create_low_power_adapter().unwrap()
    };
    let device = Device::Alternate(connection.create_device(&adapter).unwrap());
    let descriptor = device
        .create_context_descriptor(&ContextAttributes {
            version: GLVersion::new(3, 2),
            flags: ContextAttributeFlags::ALPHA,
        })
        .unwrap();
    let context = device.create_context(&descriptor, None).unwrap();
    let owner = NativeContextOwner::new(device, context);
    owner
        .state()
        .device
        .borrow()
        .make_context_current(&owner.state().context.borrow())
        .unwrap();
    (owner, connection)
}
fn handle(owner: &NativeContextOwner) -> *mut c_void {
    let device = owner.state().device.borrow();
    let context = owner.state().context.borrow();
    let (Device::Alternate(device), Context::Alternate(context)) = (&*device, &*context) else {
        panic!("private offscreen backend required");
    };
    device.native_context(context).egl_context.cast_mut()
}
fn alive(display: *mut c_void, context: *mut c_void) -> bool {
    let mut value = 0;
    // EGL_CONTEXT_CLIENT_TYPE queries ownership without GL commands.
    let result = unsafe { eglQueryContext(display, context, 0x3097, &mut value) != 0 };
    unsafe {
        eglGetError();
    }
    result
}
fn current() -> *mut c_void {
    unsafe { eglGetCurrentContext() }
}
fn display() -> *mut c_void {
    unsafe { eglGetCurrentDisplay() }
}
fn finish(native: NativeContextOwner) -> Rc<SurfmanRenderingContext> {
    let context = SurfmanRenderingContext::finish_initialization(
        native,
        GLApi::GL,
        None,
        |device, context| {
            unsafe {
                glow::Context::try_from_loader_function(|name| {
                    device.get_proc_address(context, name)
                })
            }
            .map_err(Error::GLContextInitializationFailed)
        },
    )
    .unwrap();
    println!(
        "DOME_ROOT_NATIVE_RENDERER={}",
        context.gleam_gl.get_string(gl::RENDERER)
    );
    context
}
fn surface(context: &SurfmanRenderingContext) -> Surface {
    context
        .create_surface(SurfaceType::Generic {
            size: Size2D::new(8, 8),
        })
        .unwrap()
}

#[test]
fn failed_loader_retires_unpublished_native_context() {
    let (owner, _connection) = raw_owner();
    let native = handle(&owner);
    let display = display();
    let result = SurfmanRenderingContext::finish_initialization(owner, GLApi::GL, None, |_, _| {
        Err(Error::GLContextInitializationFailed(
            "fixture loader unavailable".into(),
        ))
    });
    let destroyed = !alive(display, native);
    assert!(result.is_err() && destroyed && !servo_base::deferred_cleanup::pending());
}

#[test]
fn failed_initial_binding_does_not_publish_or_load_gl() {
    let (owner, _connection) = raw_owner();
    let native = handle(&owner);
    let display = display();
    owner
        .state()
        .device
        .borrow()
        .mark_context_lost(&mut owner.state().context.borrow_mut())
        .unwrap();
    let called = Cell::new(false);
    let result = SurfmanRenderingContext::finish_initialization(owner, GLApi::GL, None, |_, _| {
        called.set(true);
        Err(Error::Failed)
    });
    let destroyed = !alive(display, native);
    assert!(
        result.is_err() && !called.get() && destroyed && !servo_base::deferred_cleanup::pending()
    );
}

#[test]
fn producer_owner_waits_for_outstanding_surface_loan() {
    let (owner, _connection) = raw_owner();
    let native = handle(&owner);
    let display = display();
    let context = finish(owner);
    context.bind_surface(surface(&context)).unwrap();
    let chain = context.create_attached_swap_chain().unwrap();
    chain
        .swap_buffers(
            &context.device.borrow(),
            &mut context.context.borrow_mut(),
            PreserveBuffer::No,
        )
        .unwrap();
    let borrowed = chain.take_surface().unwrap();
    drop(context);
    let retained = alive(display, native) && servo_base::deferred_cleanup::pending();
    chain.recycle_surface(borrowed);
    let complete = servo_base::deferred_cleanup::poll();
    let destroyed = !alive(display, native);
    assert!(retained && complete && destroyed);
}

#[test]
fn returned_surface_keeps_context_alive_until_idle_retry() {
    let (owner, _connection) = raw_owner();
    let native = handle(&owner);
    let display = display();
    let context = finish(owner);
    let weak = Rc::downgrade(&context);
    let failed = Rc::new(Cell::new(false));
    let once = failed.clone();
    let result = context.retire_surface_with(surface(&context), move |owner, surface| {
        if !once.replace(true) {
            return Err(Error::Failed);
        }
        owner.release_surface(surface)
    });
    drop(context);
    let retained = result.is_err() && weak.upgrade().is_some() && alive(display, native);
    let complete = servo_base::deferred_cleanup::poll();
    let destroyed = weak.upgrade().is_none() && !alive(display, native);
    assert!(retained && complete && destroyed);
}

#[test]
fn native_retry_does_not_rebind_after_resource_retirement() {
    let (mut owner, _connection) = raw_owner();
    let native = handle(&owner);
    let display = display();
    {
        let state = owner.state();
        let device = state.device.borrow();
        let mut context = state.context.borrow_mut();
        let surface = device
            .create_surface(
                &context,
                SurfaceAccess::GPUOnly,
                SurfaceType::Generic {
                    size: Size2D::new(8, 8),
                },
            )
            .unwrap();
        device
            .bind_surface_to_context(&mut context, surface)
            .unwrap();
        let chain =
            SwapChain::<Device>::create_attached(&device, &mut context, SurfaceAccess::GPUOnly)
                .unwrap();
        owner.track_chain(&chain);
    }
    let failure_retained =
        !owner.state_mut().retire_with(|_, _| Err(Error::Failed)) && alive(display, native);
    let (peer, _peer_connection) = raw_owner();
    let peer_handle = handle(&peer);
    let selected_peer = current() == peer_handle;
    let mut native_called_without_rebind = false;
    let complete = owner.state_mut().retire_with(|device, context| {
        native_called_without_rebind = current() == peer_handle;
        device.destroy_context(context)
    });
    drop(owner);
    let destroyed = !alive(display, native);
    let peer_preserved = current() == peer_handle;
    drop(peer);
    assert!(
        failure_retained
            && selected_peer
            && complete
            && destroyed
            && native_called_without_rebind
            && peer_preserved
    );
}

fn software_context() -> (
    SoftwareRenderingContext,
    MesaConnection,
    *mut c_void,
    *mut c_void,
) {
    let (owner, connection) = raw_owner();
    let native = handle(&owner);
    let display = display();
    let context = finish(owner);
    context.bind_surface(surface(&context)).unwrap();
    let chain = context.create_attached_swap_chain().unwrap();
    (
        SoftwareRenderingContext {
            size: Cell::new(PhysicalSize::new(8, 8)),
            surfman_rendering_info: context,
            swap_chain: chain,
        },
        connection,
        display,
        native,
    )
}
#[test]
fn resize_reports_committed_dimensions_and_ignores_hidden_window_size() {
    let (context, _connection, display, native) = software_context();
    context.resize(PhysicalSize::new(0, 8));
    let hidden_ignored = context.size() == PhysicalSize::new(8, 8);
    context.resize(PhysicalSize::new(u32::MAX, 8));
    let out_of_range_ignored = context.size() == PhysicalSize::new(8, 8);
    context.resize(PhysicalSize::new(16, 12));
    let committed = context.size() == PhysicalSize::new(16, 12);
    drop(context);
    assert!(hidden_ignored && out_of_range_ignored && committed && !alive(display, native));
}
#[test]
fn rejected_resize_keeps_native_committed_dimensions() {
    let (context, _connection, display, native) = software_context();
    context
        .surfman_rendering_info
        .device
        .borrow()
        .mark_context_lost(&mut context.surfman_rendering_info.context.borrow_mut())
        .unwrap();
    context.resize(PhysicalSize::new(16, 12));
    let retained = context.size() == PhysicalSize::new(8, 8);
    drop(context);
    assert!(retained && !alive(display, native));
}

#[test]
fn closing_context_preserves_current_peers_native_framebuffer() {
    let (context, _connection, display, native) = software_context();
    let (peer_owner, _peer_connection) = raw_owner();
    let peer = finish(peer_owner);
    peer.bind_surface(surface(&peer)).unwrap();
    peer.make_current().unwrap();
    peer.prepare_for_rendering();
    let framebuffer = peer
        .device
        .borrow()
        .context_surface_info(&peer.context.borrow())
        .unwrap()
        .unwrap()
        .framebuffer_object
        .unwrap();
    let existed = unsafe { peer.glow_gl.is_framebuffer(framebuffer) };
    drop(context);
    let retired = !alive(display, native);
    peer.make_current().unwrap();
    let survived = unsafe { peer.glow_gl.is_framebuffer(framebuffer) };
    drop(peer);
    assert!(existed && retired && survived && !servo_base::deferred_cleanup::pending());
}
