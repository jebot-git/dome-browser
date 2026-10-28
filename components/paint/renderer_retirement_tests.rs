// SPDX-License-Identifier: MPL-2.0
// Private offscreen contexts only; no physical reset or desktop session.
#![allow(clippy::unwrap_used, clippy::panic)]
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use dpi::PhysicalSize;
use gleam::gl::{self, Gl};
use image::RgbaImage;
use paint_api::rendering_context::RenderingContext;
use surfman::{
    Context, ContextAttributeFlags, ContextAttributes, Device, Error, GLVersion, SurfaceAccess,
    SurfaceType,
};
use webrender_api::units::DeviceIntRect;
use webrender_api::{DocumentId, FramePublishId, FrameReadyParams, RenderNotifier};

struct NativeContext {
    device: Rc<Device>,
    context: RefCell<Context>,
    gl: Rc<dyn Gl>,
    fail_binding: Cell<bool>,
    destroyed: Rc<Cell<bool>>,
}
impl NativeContext {
    #[expect(unsafe_code)]
    fn new(device: Rc<Device>) -> Rc<Self> {
        let descriptor = device
            .create_context_descriptor(&ContextAttributes {
                version: GLVersion::new(3, 2),
                flags: ContextAttributeFlags::ALPHA,
            })
            .unwrap();
        let mut context = device.create_context(&descriptor, None).unwrap();
        let surface = device
            .create_surface(
                &context,
                SurfaceAccess::GPUOnly,
                SurfaceType::Generic {
                    size: euclid::default::Size2D::new(8, 8),
                },
            )
            .unwrap();
        device
            .bind_surface_to_context(&mut context, surface)
            .unwrap();
        device.make_context_current(&context).unwrap();
        let gl = unsafe { gl::GlFns::load_with(|name| device.get_proc_address(&context, name)) };
        Rc::new(Self {
            device,
            context: RefCell::new(context),
            gl,
            fail_binding: Cell::new(false),
            destroyed: Rc::new(Cell::new(false)),
        })
    }
}
impl RenderingContext for NativeContext {
    fn read_to_image(&self, _: DeviceIntRect) -> Option<RgbaImage> {
        None
    }
    fn size(&self) -> PhysicalSize<u32> {
        PhysicalSize::new(8, 8)
    }
    fn resize(&self, _: PhysicalSize<u32>) {
        unreachable!()
    }
    fn present(&self) {
        unreachable!()
    }
    fn make_current(&self) -> Result<(), Error> {
        if self.fail_binding.replace(false) {
            return Err(Error::Failed);
        }
        self.device.make_context_current(&self.context.borrow())
    }
    fn native_context_is_lost(&self) -> bool {
        self.device.context_is_lost(&self.context.borrow())
    }
    fn gleam_gl_api(&self) -> Rc<dyn Gl> {
        self.gl.clone()
    }
    fn glow_gl_api(&self) -> Arc<glow::Context> {
        unreachable!()
    }
}
impl Drop for NativeContext {
    fn drop(&mut self) {
        self.device.destroy_context(self.context.get_mut()).unwrap();
        self.destroyed.set(true);
    }
}
struct Notifier;
impl RenderNotifier for Notifier {
    fn clone(&self) -> Box<dyn RenderNotifier> {
        Box::new(Self)
    }
    fn wake_up(&self, _: bool) {}
    fn new_frame_ready(&self, _: DocumentId, _: FramePublishId, _: &FrameReadyParams) {}
}
fn renderer(context: &NativeContext) -> webrender::Renderer {
    let (mut renderer, sender) = webrender::create_webrender_instance(
        context.gl.clone(),
        Box::new(Notifier),
        webrender::WebRenderOptions {
            enable_dithering: true,
            enable_gpu_markers: true,
            workers: Some(Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .unwrap(),
            )),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    // Populate profiling query arrays too: their destructors normally issue GL.
    renderer.gpu_profiler.enable_timers();
    renderer.gpu_profiler.enable_samplers();
    let api = sender.create_api();
    api.stop_render_backend();
    api.shut_down(true);
    renderer
}

fn cleanup(fail_binding: bool, lost: bool, lose_on_retry: bool) {
    let connection = surfman::mesa_surfaceless::connection::Connection::new().unwrap();
    let adapter = if std::env::var_os("DOME_CREATION_SOFTWARE").is_some() {
        connection.create_software_adapter().unwrap()
    } else {
        connection.create_low_power_adapter().unwrap()
    };
    let device = Rc::new(Device::Alternate(
        connection.create_device(&adapter).unwrap(),
    ));
    let owner = NativeContext::new(device.clone());
    println!(
        "DOME_RENDERER_RETIRE_RENDERER={}",
        owner.gl.get_string(gl::RENDERER)
    );
    let mut renderer = renderer(&owner);
    let late_marker = if lost || lose_on_retry {
        Some(
            renderer
                .gpu_profiler
                .start_marker("pending renderer cleanup"),
        )
    } else {
        None
    };
    let peer = NativeContext::new(device.clone());
    let sentinels = peer.gl.gen_textures(128);
    for &id in &sentinels {
        peer.gl.bind_texture(gl::TEXTURE_2D, id);
    }
    let queries = peer.gl.gen_queries(1024);
    // A query name is not an object until it is bound once.
    for &id in &queries {
        peer.gl.begin_query(gl::SAMPLES_PASSED, id);
        peer.gl.end_query(gl::SAMPLES_PASSED);
    }
    peer.gl
        .push_debug_group_khr(gl::DEBUG_SOURCE_APPLICATION, 1, "peer context group");
    owner.fail_binding.set(fail_binding);
    if lost {
        device
            .mark_context_lost(&mut owner.context.borrow_mut())
            .unwrap();
    }
    let weak = Rc::downgrade(&owner);
    let destroyed = owner.destroyed.clone();
    super::retire_renderer(owner.clone(), renderer);
    if lose_on_retry {
        device
            .mark_context_lost(&mut owner.context.borrow_mut())
            .unwrap();
    }
    drop(owner);
    let retained = weak.upgrade().is_some();
    let destroyed_early = destroyed.get();
    let mut polls = 0;
    while servo_base::deferred_cleanup::pending() && polls < 4 {
        servo_base::deferred_cleanup::poll();
        polls += 1;
    }
    peer.make_current().unwrap();
    let group_before = group_depth(&*peer.gl);
    drop(late_marker);
    let groups_survive = group_depth(&*peer.gl) == group_before;
    peer.gl.pop_debug_group_khr();
    let textures_survive = sentinels
        .iter()
        .all(|&id| peer.gl.is_texture(id) != gl::FALSE);
    let _ = peer.gl.get_error();
    let queries_survive = queries.iter().all(|&id| {
        peer.gl.get_query_object_uiv(id, gl::QUERY_RESULT_AVAILABLE);
        peer.gl.get_error() == gl::NO_ERROR
    });
    peer.gl.delete_textures(&sentinels);
    peer.gl.delete_queries(&queries);
    let complete =
        !servo_base::deferred_cleanup::pending() && weak.upgrade().is_none() && destroyed.get();
    drop(peer);
    assert!(
        textures_survive && queries_survive && groups_survive,
        "shutdown touched a peer's native objects: textures={textures_survive} queries={queries_survive} groups={groups_survive}"
    );
    assert!(complete);
    assert_eq!(retained, fail_binding && !lost);
    assert_eq!(destroyed_early, !fail_binding || lost);
    assert_eq!(polls, usize::from(fail_binding && !lost));
}
#[test]
fn renderer_shutdown_selects_healthy_owner_before_deleting_gl_objects() {
    cleanup(false, false, false);
}
#[test]
fn failed_renderer_binding_keeps_owner_and_does_not_delete_peer_objects() {
    cleanup(true, false, false);
}
#[test]
fn known_lost_renderer_drops_cpu_owners_without_gl_or_peer_deletion() {
    cleanup(false, true, false);
}

#[test]
fn context_lost_during_queued_cleanup_abandons_without_rebinding() {
    cleanup(true, false, true);
}

#[expect(unsafe_code)]
fn group_depth(gl: &dyn Gl) -> i32 {
    let mut value = [0];
    unsafe {
        gl.get_integer_v(glow::DEBUG_GROUP_STACK_DEPTH, &mut value);
    }
    value[0]
}
