/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use super::*;
use layout_api::BoxAreaType;
use webxr_api::{Handedness, InputSource, TargetRayMode, ViewerPose};

use crate::dom::bindings::codegen::UnionTypes::HTMLCanvasElementOrOffscreenCanvas;
use crate::dom::htmlcanvaselement::HTMLCanvasElement;
use crate::dom::inputevent::HitTestResult;
use crate::dom::node::{Node, NodeTraits};

#[derive(Clone, Copy)]
pub(crate) enum InlinePointerAction {
    Down { primary: bool },
    Move,
    Select,
    End,
    Cancel,
}

/// A sample from the native hit test, never from a script-dispatched event.
pub(crate) struct ScreenPointerSample {
    canvas: DomRoot<HTMLCanvasElement>,
    position: [f32; 2],
}

impl ScreenPointerSample {
    pub(crate) fn targets(&self, target: &EventTarget) -> bool {
        self.canvas.upcast::<EventTarget>() == target
    }

    pub(crate) fn from_hit(hit: &HitTestResult) -> Option<Self> {
        let canvas = DomRoot::from_ref(hit.node.downcast::<HTMLCanvasElement>()?);
        let node = canvas.upcast::<Node>();
        let window = node.owner_window();
        // Hit testing already inverts the fragment's CSS transforms. Use the
        // same untransformed box, excluding border/padding from the ray area.
        let content = window.box_area_query(node, BoxAreaType::Content, true)?;
        let border = window.box_area_query(node, BoxAreaType::Border, true)?;
        let width = content.size.width.to_f32_px();
        let height = content.size.height.to_f32_px();
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        let x = (hit.point_in_node.x - (content.origin.x - border.origin.x).to_f32_px()) / width;
        let y = (hit.point_in_node.y - (content.origin.y - border.origin.y).to_f32_px()) / height;
        if ![x, y]
            .iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
        {
            return None;
        }
        Some(Self {
            canvas,
            position: [x, y],
        })
    }
}

#[derive(Clone, MallocSizeOf)]
pub(super) struct InlinePointer {
    id: InputId,
    position: [f32; 2],
    pressed: bool,
    started: bool,
    primary: bool,
}

impl XRSession {
    fn inline_output_canvas(&self) -> Option<DomRoot<HTMLCanvasElement>> {
        if self.is_immersive() {
            return None;
        }
        match self
            .active_render_state
            .get()
            .GetBaseLayer()?
            .context()
            .canvas()?
        {
            HTMLCanvasElementOrOffscreenCanvas::HTMLCanvasElement(canvas) => Some(canvas),
            HTMLCanvasElementOrOffscreenCanvas::OffscreenCanvas(_) => None,
        }
    }

    fn accepts_inline_pointer(&self, cx: &mut JSContext) -> bool {
        !self.is_immersive()
            && !self.is_ended()
            && self.input_poses_visible()
            && !self
                .global()
                .as_window()
                .Navigator(cx)
                .Xr(cx)
                .has_active_immersive_session()
    }

    pub(crate) fn has_inline_pointer(&self, pointer: i32) -> bool {
        self.inline_pointers.borrow().contains_key(&pointer)
    }

    fn inline_pointer_frame(&self) -> Frame {
        let pose = self.last_inline_pose.borrow().clone();
        let inputs = self.inline_pointer_frames(pose.as_ref());
        Frame {
            pose,
            inputs,
            events: vec![],
            sub_images: vec![],
            hit_test_results: vec![],
            predicted_display_time: 0.0,
            predicted_display_instant: Some(CrossProcessInstant::now()),
        }
    }

    pub(super) fn inline_pointer_frames(&self, pose: Option<&ViewerPose>) -> Vec<InputFrame> {
        let Some(pose) = pose else {
            return vec![];
        };
        let state = self.active_render_state.get();
        let Some(layer) = state.GetBaseLayer() else {
            return vec![];
        };
        let size = layer.size();
        if size.width == 0 || size.height == 0 {
            return vec![];
        }
        let Some(fov) = state.GetInlineVerticalFieldOfView() else {
            return vec![];
        };
        self.inline_pointers
            .borrow()
            .values()
            .filter_map(|pointer| {
                let ray = util::inline_pointer_ray(
                    pointer.position,
                    *fov as f32,
                    size.width as f32 / size.height as f32,
                )?;
                Some(InputFrame {
                    id: pointer.id,
                    target_ray_origin: Some(ray.then(&pose.transform)),
                    grip_origin: None,
                    pressed: pointer.pressed,
                    hand: None,
                    squeezed: false,
                    button_values: vec![],
                    button_pressed: vec![],
                    button_touched: vec![],
                    axis_values: vec![],
                    input_changed: true,
                })
            })
            .collect()
    }

    fn inline_pointer_dom_frame(&self, cx: &mut JSContext) -> DomRoot<XRFrame> {
        XRFrame::new(
            cx,
            self.global().as_window(),
            self,
            self.inline_pointer_frame(),
        )
    }

    fn fire_inline_pointer_event(
        &self,
        cx: &mut JSContext,
        id: InputId,
        name: Atom,
        frame: &XRFrame,
    ) {
        let Some(source) = self.input_sources.find(id) else {
            return;
        };
        let was_active = frame.is_active();
        frame.set_active(true);
        let event = XRInputSourceEvent::new(
            cx,
            self.global().as_window(),
            name,
            false,
            false,
            frame,
            &source,
        );
        event.upcast::<Event>().fire(cx, self.upcast());
        frame.set_active(was_active);
    }

    pub(crate) fn inline_pointer_event(
        &self,
        cx: &mut JSContext,
        pointer: i32,
        action: InlinePointerAction,
        sample: Option<&ScreenPointerSample>,
    ) {
        if matches!(
            action,
            InlinePointerAction::End | InlinePointerAction::Cancel
        ) {
            self.finish_inline_pointer(cx, pointer);
            return;
        }
        let canvas = self.inline_output_canvas();
        let valid = self.accepts_inline_pointer(cx)
            && canvas
                .as_ref()
                .zip(sample)
                .is_some_and(|(canvas, sample)| **canvas == *sample.canvas);
        if !valid {
            self.finish_inline_pointer(cx, pointer);
            return;
        }
        let sample = sample.unwrap();
        if let InlinePointerAction::Down { primary } = action {
            if self.has_inline_pointer(pointer)
                || self.inline_pointers.borrow().len() >= 32
                || self.last_inline_pose.borrow().is_none()
            {
                return;
            }
            // Inline devices normally have no backend sources. Allocate from
            // the opposite end and still avoid any currently registered ID.
            let mut next = self.next_inline_pointer_id.get();
            while self.input_sources.find(InputId(next)).is_some() {
                let Some(value) = next.checked_sub(1) else {
                    return;
                };
                next = value;
            }
            let Some(following) = next.checked_sub(1) else {
                return;
            };
            self.next_inline_pointer_id.set(following);
            let id = InputId(next);
            self.inline_pointers.borrow_mut().insert(
                pointer,
                InlinePointer {
                    id,
                    position: sample.position,
                    pressed: true,
                    started: false,
                    primary,
                },
            );
            self.inline_pointer_canvas.set(Some(&sample.canvas));
            let info = InputSource {
                id,
                handedness: Handedness::None,
                target_ray_mode: TargetRayMode::Screen,
                supports_grip: false,
                hand_support: None,
                profiles: vec!["generic-touchscreen".into()],
                gamepad: None,
            };
            self.input_sources.add_input_sources(cx, self, &[info]);
            // Source-change handlers may end or hide the session.
            self.validate_inline_pointers(cx);
            if primary && self.has_inline_pointer(pointer) && !self.is_ended() {
                if let Some(input) = self.inline_pointers.borrow_mut().get_mut(&pointer) {
                    input.started = true;
                }
                let frame = self.inline_pointer_dom_frame(cx);
                self.fire_inline_pointer_event(cx, id, atom!("selectstart"), &frame);
            }
            return;
        }
        let input = {
            let mut pointers = self.inline_pointers.borrow_mut();
            let Some(input) = pointers.get_mut(&pointer) else {
                return;
            };
            input.position = sample.position;
            if matches!(action, InlinePointerAction::Select) {
                input.pressed = false;
            }
            input.clone()
        };
        if matches!(action, InlinePointerAction::Select) && input.primary {
            let frame = self.inline_pointer_dom_frame(cx);
            self.inline_action_frames
                .borrow_mut()
                .insert(pointer, Dom::from_ref(&*frame));
            self.fire_inline_pointer_event(cx, input.id, atom!("select"), &frame);
        }
    }

    fn finish_inline_pointer(&self, cx: &mut JSContext, pointer: i32) {
        // Take a frame before removing the source so event-time poses remain
        // queryable. Never retain a map borrow while calling author script.
        if !self.has_inline_pointer(pointer) {
            return;
        }
        let pending = self
            .inline_action_frames
            .borrow_mut()
            .remove(&pointer)
            .map(|frame| frame.as_rooted());
        let frame = pending.unwrap_or_else(|| self.inline_pointer_dom_frame(cx));
        let input = self.inline_pointers.borrow_mut().remove(&pointer);
        let Some(input) = input else {
            return;
        };
        if input.started {
            self.fire_inline_pointer_event(cx, input.id, atom!("selectend"), &frame);
        }
        self.input_sources.remove_input_source(cx, self, input.id);
        if self.inline_pointers.borrow().is_empty() {
            self.inline_pointer_canvas.set(None);
        }
    }

    pub(crate) fn cancel_inline_pointers(&self, cx: &mut JSContext) {
        let ids: Vec<_> = self.inline_pointers.borrow().keys().copied().collect();
        for id in ids {
            self.finish_inline_pointer(cx, id);
        }
    }

    pub(super) fn validate_inline_pointers(&self, cx: &mut JSContext) {
        if self.inline_pointers.borrow().is_empty() {
            return;
        }
        let valid = self.accepts_inline_pointer(cx)
            && self
                .inline_output_canvas()
                .zip(self.inline_pointer_canvas.get())
                .is_some_and(|(current, original)| {
                    current == original
                        && current
                            .upcast::<Node>()
                            .is_connected_with_browsing_context(cx.no_gc())
                });
        if !valid {
            self.cancel_inline_pointers(cx);
        }
    }
}
