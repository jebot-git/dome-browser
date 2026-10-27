/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */
use super::*;
use embedder_traits::{PenEvent, PenEventType};

#[derive(JSTraceable, MallocSizeOf)]
pub(super) struct PenPointer {
    pub pointer_id: i32,
    primary: bool,
    pub(super) buttons: u16,
    pub(super) owned: bool,
    hover: Option<Dom<Element>>,
    actual: Option<Dom<Element>>,
    compat_hover: Option<Dom<Element>>,
    down: Option<Dom<Element>>,
    suppress_mouse: bool,
    click_allowed: bool,
    auxiliary: FxHashMap<u16, PenButtonPress>,
    #[no_trace]
    down_point: Point2D<f32, CSSPixel>,
    #[no_trace]
    client: Point2D<f32, CSSPixel>,
    #[no_trace]
    page: Point2D<f32, CSSPixel>,
    #[no_trace]
    sample: PenEvent,
}
#[derive(JSTraceable, MallocSizeOf)]
struct PenButtonPress {
    target: Dom<Element>,
    #[no_trace]
    point: Point2D<f32, CSSPixel>,
    click_allowed: bool,
}
// Keep click sequences per tool without retaining detached DOM subtrees.
// Native tool IDs are local to this process; this bounded map is per Document.
#[derive(Default, JSTraceable, MallocSizeOf)]
pub(super) struct PenClickState {
    info: ClickCountingInfo,
    target: String,
}
struct Snapshot {
    id: i32,
    primary: bool,
    buttons: u16,
    owned: bool,
    hover: Option<DomRoot<Element>>,
    actual: Option<DomRoot<Element>>,
    compat_hover: Option<DomRoot<Element>>,
    down: Option<DomRoot<Element>>,
    suppress_mouse: bool,
    click_allowed: bool,
    auxiliary: Vec<(u16, DomRoot<Element>, bool)>,
    down_point: Point2D<f32, CSSPixel>,
    client: Point2D<f32, CSSPixel>,
    page: Point2D<f32, CSSPixel>,
    sample: PenEvent,
}
impl DocumentEventHandler {
    fn pen_snapshot(&self, id: i32) -> Option<Snapshot> {
        self.pen_pointers.borrow().get(&id).map(|p| Snapshot {
            id: p.pointer_id,
            primary: p.primary,
            buttons: p.buttons,
            owned: p.owned,
            hover: p.hover.as_ref().map(|e| DomRoot::from_ref(&**e)),
            actual: p.actual.as_ref().map(|e| DomRoot::from_ref(&**e)),
            compat_hover: p.compat_hover.as_ref().map(|e| DomRoot::from_ref(&**e)),
            down: p.down.as_ref().map(|e| DomRoot::from_ref(&**e)),
            suppress_mouse: p.suppress_mouse,
            click_allowed: p.click_allowed,
            auxiliary: p
                .auxiliary
                .iter()
                .map(|(button, press)| {
                    (
                        *button,
                        DomRoot::from_ref(&*press.target),
                        press.click_allowed,
                    )
                })
                .collect(),
            down_point: p.down_point,
            client: p.client,
            page: p.page,
            sample: p.sample,
        })
    }
    pub(crate) fn update_pointer_state(
        &self,
        cx: &mut JSContext,
        state: embedder_traits::PointerState,
    ) {
        if state.active {
            self.pointer_states
                .borrow_mut()
                .insert(state.pointer_id, state);
            return;
        }
        self.pointer_states.borrow_mut().remove(&state.pointer_id);
        let native = self
            .pen_pointers
            .borrow()
            .iter()
            .find(|(_, p)| p.pointer_id == state.pointer_id)
            .map(|(id, _)| *id);
        if let Some(native) = native
            && let Some(p) = self.pen_snapshot(native)
        {
            let mut sample = p.sample;
            sample.event_type = PenEventType::Cancel;
            let input = ConstellationInputEvent {
                pointer_context: None,
                hit_test_result: None,
                pressed_mouse_buttons: self.mouse_button_state.get(),
                active_keyboard_modifiers: self.active_keyboard_modifiers.get(),
                event: InputEvent::Pen(sample).into(),
            };
            self.retire_pen(cx, sample, &input);
        }
    }

    pub(super) fn hit_test_for_capture(
        &self,
        input: &ConstellationInputEvent,
        target: &Element,
    ) -> Option<HitTestResult> {
        let client = input.hit_test_result.as_ref()?.point_in_viewport;
        let page = client + self.window.scroll_offset().cast_unit();
        let origin = target
            .upcast::<Node>()
            .border_box()
            .unwrap_or_default()
            .origin;
        Some(HitTestResult {
            node: DomRoot::from_ref(target.upcast()),
            dom_position_for_selection: None,
            cursor: Cursor::Default,
            point_in_node: page - Vector2D::new(origin.x.to_f32_px(), origin.y.to_f32_px()),
            point_in_frame: client,
            point_relative_to_initial_containing_block: page,
        })
    }

    fn leave_pen_document(
        &self,
        cx: &mut JSContext,
        sample: PenEvent,
        input: &ConstellationInputEvent,
    ) {
        let Some(p) = self.pen_snapshot(sample.id) else {
            return;
        };
        #[cfg(feature = "webxr")]
        self.window
            .inline_xr_pointer_event(cx, p.id, InlinePointerAction::Cancel, None);
        self.pen_boundaries(
            cx,
            sample,
            p.id,
            p.primary,
            p.client,
            p.page,
            p.hover.as_deref(),
            None,
            input.active_keyboard_modifiers,
        );
        if p.primary
            && self.mouse_button_state.get().is_empty()
            && p.compat_hover.is_some()
            && self.current_hover_target.get().as_deref() == p.compat_hover.as_deref()
        {
            let leave = MouseLeftViewportEvent {
                focus_moving_to_another_iframe: false,
                is_compatibility_event: true,
            };
            let compat = ConstellationInputEvent {
                pointer_context: None,
                hit_test_result: input.hit_test_result.clone(),
                pressed_mouse_buttons: MouseButtons::empty(),
                active_keyboard_modifiers: input.active_keyboard_modifiers,
                event: InputEvent::MouseLeftViewport(leave).into(),
            };
            self.handle_mouse_left_viewport_event(cx, &compat, &leave);
            self.unset_active_element(cx.no_gc());
        }
        self.pen_clicks
            .safe_borrow_mut(cx.no_gc())
            .remove(&sample.id);
        if let Some(p) = self.pen_pointers.borrow_mut().get_mut(&sample.id) {
            p.hover = None;
            p.actual = None;
            p.compat_hover = None;
            p.down = None;
            p.auxiliary.clear();
            p.click_allowed = false;
            p.owned = !input.pointer_context.is_some_and(|context| context.has_hit);
        }
    }

    pub(super) fn retarget_pen_mouse_hit(hit: &mut HitTestResult, target: Option<&Element>) {
        let Some(target) = target else {
            return;
        };
        if !hit
            .node
            .inclusive_ancestors(ShadowIncluding::Yes)
            .any(|node| &*node == target.upcast::<Node>())
        {
            hit.dom_position_for_selection = None;
        }
        let origin = target
            .upcast::<Node>()
            .border_box()
            .unwrap_or_default()
            .origin;
        hit.point_in_node = hit.point_relative_to_initial_containing_block
            - Vector2D::new(origin.x.to_f32_px(), origin.y.to_f32_px());
        hit.node = DomRoot::from_ref(target.upcast());
    }
    pub(super) fn pen_capture_boundary_transition(
        &self,
        cx: &mut JSContext,
        pointer_id: i32,
        old: &Element,
        new: &Element,
    ) -> bool {
        let native = self
            .pen_pointers
            .borrow()
            .iter()
            .find(|(_, p)| p.pointer_id == pointer_id)
            .map(|(native, _)| *native);
        let Some(native) = native else {
            return false;
        };
        let Some(p) = self.pen_snapshot(native) else {
            return false;
        };
        self.pen_boundaries(
            cx,
            p.sample,
            p.id,
            p.primary,
            p.client,
            p.page,
            Some(old),
            Some(new),
            self.active_keyboard_modifiers.get(),
        );
        if let Some(p) = self.pen_pointers.borrow_mut().get_mut(&native) {
            p.hover = Some(Dom::from_ref(new));
        }
        true
    }
    pub(super) fn process_pen_pending_capture(&self, cx: &mut JSContext, pointer_id: i32) -> bool {
        let native = self
            .pen_pointers
            .borrow()
            .iter()
            .find(|(_, p)| p.pointer_id == pointer_id)
            .map(|(native, _)| *native);
        let Some(native) = native else {
            return false;
        };
        let Some(p) = self.pen_snapshot(native) else {
            return false;
        };
        let pending = || {
            self.pending_pointer_capture
                .borrow()
                .get(&pointer_id)
                .map(|el| DomRoot::from_ref(&**el))
                .filter(|el| el.upcast::<Node>().is_connected())
        };
        let current = self
            .pointer_capture_target
            .borrow()
            .get(&pointer_id)
            .map(|el| DomRoot::from_ref(&**el));
        let pending_disconnected = self
            .pending_pointer_capture
            .borrow()
            .get(&pointer_id)
            .is_some_and(|el| !el.upcast::<Node>().is_connected());
        if pending_disconnected {
            self.pending_pointer_capture
                .safe_borrow_mut(cx.no_gc())
                .remove(&pointer_id);
        }
        if current == pending() {
            return true;
        }
        if let Some(current) = current.as_ref() {
            if current.upcast::<Node>().is_connected() {
                self.fire_pointer_capture_event(
                    cx,
                    "lostpointercapture",
                    pointer_id,
                    "pen",
                    p.primary,
                    current,
                );
            } else {
                self.fire_pointer_capture_event_at_target(
                    cx,
                    "lostpointercapture",
                    pointer_id,
                    "pen",
                    p.primary,
                    self.window.Document().upcast(),
                );
            }
        }
        // A lostcapture listener may replace the pending target. Snapshot it
        // before gotcapture so changes in that listener apply to the next event.
        let next = pending();
        let hover = next.as_deref().or(p.actual.as_deref());
        self.pen_boundaries(
            cx,
            p.sample,
            p.id,
            p.primary,
            p.client,
            p.page,
            p.hover.as_deref(),
            hover,
            self.active_keyboard_modifiers.get(),
        );
        if let Some(next) = next.as_ref() {
            self.fire_pointer_capture_event(
                cx,
                "gotpointercapture",
                pointer_id,
                "pen",
                p.primary,
                next,
            );
            self.pointer_capture_target
                .safe_borrow_mut(cx.no_gc())
                .insert(pointer_id, Dom::from_ref(&**next));
        } else {
            self.pointer_capture_target
                .safe_borrow_mut(cx.no_gc())
                .remove(&pointer_id);
        }
        if let Some(pointer) = self.pen_pointers.borrow_mut().get_mut(&native) {
            pointer.hover = hover.map(Dom::from_ref);
        }
        true
    }
    fn record_pen_click(
        &self,
        cx: &mut JSContext,
        native: i32,
        target: &Node,
        button: MouseButton,
        client: Point2D<f32, CSSPixel>,
    ) -> usize {
        let target_id = target.unique_id(self.window.pipeline_id());
        let mut clicks = self.pen_clicks.safe_borrow_mut(cx.no_gc());
        if !clicks.contains_key(&native) && clicks.len() >= 64 {
            let oldest = clicks
                .iter()
                .min_by_key(|(_, click)| click.info.time)
                .map(|(id, _)| *id);
            if let Some(oldest) = oldest {
                clicks.remove(&oldest);
            }
        }
        let state = clicks.entry(native).or_default();
        if state.target != target_id {
            state.info.reset_click_count();
            state.target = target_id;
        }
        state.info.reset_click_count_if_necessary(button, client);
        state.info.increment_click_count(button, client)
    }
    pub(super) fn pen_event_for_pointer(
        &self,
        cx: &mut JSContext,
        name: &str,
        pointer_id: i32,
    ) -> Option<DomRoot<PointerEvent>> {
        let native = self
            .pen_pointers
            .borrow()
            .iter()
            .find(|(_, p)| p.pointer_id == pointer_id)
            .map(|(id, _)| *id)?;
        let p = self.pen_snapshot(native)?;
        let mut sample = p.sample;
        sample.buttons = p.buttons;
        Some(self.pen_dom_event(
            cx,
            name,
            sample,
            p.id,
            p.primary,
            p.client,
            p.page,
            MouseButton::None,
            None,
            self.active_keyboard_modifiers.get(),
        ))
    }
    pub(super) fn capture_hover_target(
        &self,
        pointer_id: i32,
        pointer_type: &str,
    ) -> Option<DomRoot<Element>> {
        if pointer_type == "pen" {
            let native = self
                .pen_pointers
                .borrow()
                .iter()
                .find(|(_, p)| p.pointer_id == pointer_id)
                .map(|(id, _)| *id);
            if let Some(native) = native {
                return self.pen_snapshot(native).and_then(|p| p.actual);
            }
        }
        self.current_hover_target.get()
    }
    #[allow(clippy::too_many_arguments)]
    fn pen_dom_event(
        &self,
        cx: &mut JSContext,
        name: &str,
        sample: PenEvent,
        id: i32,
        primary: bool,
        client: Point2D<f32, CSSPixel>,
        page: Point2D<f32, CSSPixel>,
        button: MouseButton,
        point_in_node: Option<Point2D<f32, CSSPixel>>,
        modifiers: Modifiers,
    ) -> DomRoot<PointerEvent> {
        let sample = sample.normalized();
        let (tx, ty, altitude, azimuth) = sample.orientation();
        let boundaries = matches!(name, "pointerenter" | "pointerleave");
        let event = PointerEvent::new(
            cx,
            &self.window,
            name.into(),
            EventBubbles::from(!boundaries),
            EventCancelable::from(
                !boundaries
                    && !matches!(
                        name,
                        "pointercancel" | "gotpointercapture" | "lostpointercapture"
                    ),
            ),
            Some(&self.window),
            if matches!(name, "click" | "auxclick") {
                let native = self
                    .pen_pointers
                    .borrow()
                    .iter()
                    .find(|(_, p)| p.pointer_id == id)
                    .map(|(native, _)| *native);
                native
                    .and_then(|native| {
                        self.pen_clicks
                            .borrow()
                            .get(&native)
                            .map(|click| click.info.count)
                    })
                    .unwrap_or(1)
                    .max(1) as i32
            } else {
                0
            },
            client.to_i32(),
            client.to_i32(),
            page.to_i32(),
            modifiers,
            button,
            MouseButtons::from_bits_retain(sample.buttons),
            None,
            point_in_node,
            id,
            1,
            1,
            sample.pressure,
            sample.tangential_pressure,
            tx,
            ty,
            sample.twist.round() as i32 % 360,
            altitude,
            azimuth,
            "pen".into(),
            primary,
            vec![],
            vec![],
        );
        event.upcast::<Event>().set_composed(!boundaries);
        event
    }
    #[allow(clippy::too_many_arguments)]
    fn pen_boundaries(
        &self,
        cx: &mut JSContext,
        sample: PenEvent,
        id: i32,
        primary: bool,
        client: Point2D<f32, CSSPixel>,
        page: Point2D<f32, CSSPixel>,
        old: Option<&Element>,
        new: Option<&Element>,
        modifiers: Modifiers,
    ) {
        if old == new {
            return;
        }
        let common = old.zip(new).and_then(|(a, b)| {
            a.upcast::<Node>()
                .common_ancestor_in_flat_tree(cx.no_gc(), b.upcast())
        });
        if let Some(old) = old {
            let e = self.pen_dom_event(
                cx,
                "pointerout",
                sample,
                id,
                primary,
                client,
                page,
                MouseButton::None,
                None,
                modifiers,
            );
            e.upcast::<Event>()
                .set_related_target(new.map(|e| e.upcast()));
            e.upcast::<Event>().fire(cx, old.upcast());
            let targets: Vec<_> = old
                .upcast::<Node>()
                .inclusive_ancestors_in_flat_tree_unrooted(cx.no_gc())
                .take_while(|node| common.as_ref().is_none_or(|common| **common != ***node))
                .map(|node| node.as_rooted())
                .collect();
            for target in targets {
                let e = self.pen_dom_event(
                    cx,
                    "pointerleave",
                    sample,
                    id,
                    primary,
                    client,
                    page,
                    MouseButton::None,
                    None,
                    modifiers,
                );
                e.upcast::<Event>()
                    .set_related_target(new.map(|e| e.upcast()));
                e.upcast::<Event>().fire(cx, target.upcast());
            }
        }
        if let Some(new) = new {
            let e = self.pen_dom_event(
                cx,
                "pointerover",
                sample,
                id,
                primary,
                client,
                page,
                MouseButton::None,
                None,
                modifiers,
            );
            e.upcast::<Event>()
                .set_related_target(old.map(|e| e.upcast()));
            e.upcast::<Event>().fire(cx, new.upcast());
            let mut targets: Vec<_> = new
                .upcast::<Node>()
                .inclusive_ancestors_in_flat_tree_unrooted(cx.no_gc())
                .take_while(|node| common.as_ref().is_none_or(|common| **common != ***node))
                .map(|node| node.as_rooted())
                .collect();
            targets.reverse();
            for target in targets {
                let e = self.pen_dom_event(
                    cx,
                    "pointerenter",
                    sample,
                    id,
                    primary,
                    client,
                    page,
                    MouseButton::None,
                    None,
                    modifiers,
                );
                e.upcast::<Event>()
                    .set_related_target(old.map(|e| e.upcast()));
                e.upcast::<Event>().fire(cx, target.upcast());
            }
        }
    }
    fn retire_pen(&self, cx: &mut JSContext, event: PenEvent, input: &ConstellationInputEvent) {
        let Some(p) = self.pen_snapshot(event.id) else {
            return;
        };
        let was_pressed = p.owned && p.buttons != 0;
        let mut sample = p.sample;
        sample.event_type = PenEventType::Cancel;
        sample = sample.normalized();
        if let Some(pointer) = self.pen_pointers.borrow_mut().get_mut(&event.id) {
            pointer.buttons = 0;
            pointer.sample = sample;
        }
        // Cancellation is also a pointer event: apply requests queued by the
        // preceding handler before choosing its target, then release capture.
        self.process_pending_pointer_capture(cx, p.id, "pen", p.primary);
        let Some(p) = self.pen_snapshot(event.id) else {
            return;
        };
        #[cfg(feature = "webxr")]
        self.window
            .inline_xr_pointer_event(cx, p.id, InlinePointerAction::Cancel, None);
        let capture = self.get_pointer_capture_target(p.id);
        if was_pressed && let Some(target) = capture.as_deref().or(p.hover.as_deref()) {
            self.pen_dom_event(
                cx,
                "pointercancel",
                sample,
                p.id,
                p.primary,
                p.client,
                p.page,
                MouseButton::None,
                None,
                input.active_keyboard_modifiers,
            )
            .upcast::<Event>()
            .fire(cx, target.upcast());
        }
        // A suppressed pointer is no longer over an underlying element. Do
        // not manufacture a hover re-entry while releasing its capture.
        if let Some(pointer) = self.pen_pointers.borrow_mut().get_mut(&event.id) {
            pointer.actual = None;
        }
        self.implicit_release_pointer_capture(cx, p.id, "pen", p.primary);
        self.pen_boundaries(
            cx,
            sample,
            p.id,
            p.primary,
            p.client,
            p.page,
            p.hover.as_deref(),
            None,
            input.active_keyboard_modifiers,
        );
        if p.owned && p.primary && self.mouse_button_state.get().is_empty() {
            if p.compat_hover.is_some()
                && self.current_hover_target.get().as_deref() == p.compat_hover.as_deref()
            {
                let leave = MouseLeftViewportEvent {
                    focus_moving_to_another_iframe: false,
                    is_compatibility_event: true,
                };
                let compat = ConstellationInputEvent {
                    pointer_context: None,
                    event: InputEvent::MouseLeftViewport(leave).into(),
                    hit_test_result: input.hit_test_result.clone(),
                    pressed_mouse_buttons: MouseButtons::empty(),
                    active_keyboard_modifiers: input.active_keyboard_modifiers,
                };
                self.handle_mouse_left_viewport_event(cx, &compat, &leave);
            }
            self.unset_active_element(cx.no_gc());
            self.last_mouse_button_down_point.take();
        }
        self.pen_pointers
            .safe_borrow_mut(cx.no_gc())
            .remove(&event.id);
    }
    pub(super) fn handle_pen_event(
        &self,
        cx: &mut JSContext,
        event: PenEvent,
        input: &ConstellationInputEvent,
    ) -> InputEventResult {
        let sample = event.normalized();
        if sample.event_type != PenEventType::Sample || self.window.Document().Hidden() {
            self.retire_pen(cx, sample, input);
            return InputEventResult::default();
        }
        let actual_hit = self
            .window
            .hit_test_from_input_event(HitTestFlags::IncludeDomPosition, input);
        let Some(client) = input
            .hit_test_result
            .as_ref()
            .map(|hit| hit.point_in_viewport)
        else {
            self.retire_pen(cx, sample, input);
            return InputEventResult::default();
        };
        let page = client + self.window.scroll_offset().cast_unit();
        let in_pipeline = input
            .pointer_context
            .is_none_or(|context| context.hit_in_pipeline);
        let actual_element = actual_hit.as_ref().filter(|_| in_pipeline).and_then(|hit| {
            hit.node
                .inclusive_ancestors(ShadowIncluding::Yes)
                .find_map(DomRoot::downcast::<Element>)
        });
        if !self.pen_pointers.borrow().contains_key(&sample.id) {
            let id = input.pointer_context.map_or_else(
                || self.next_touch_pointer_id.get(),
                |context| context.pointer_id,
            );
            self.next_touch_pointer_id
                .set(id.checked_add(1).unwrap_or(1));
            self.pen_pointers.safe_borrow_mut(cx.no_gc()).insert(
                sample.id,
                PenPointer {
                    pointer_id: id,
                    primary: sample.primary,
                    buttons: input
                        .pointer_context
                        .map_or(0, |context| context.previous_buttons),
                    owned: true,
                    hover: None,
                    actual: None,
                    compat_hover: None,
                    down: None,
                    suppress_mouse: false,
                    click_allowed: false,
                    auxiliary: Default::default(),
                    down_point: client,
                    client,
                    page,
                    sample,
                },
            );
        }
        let mut old = self.pen_snapshot(sample.id).unwrap();
        if let Some(context) = input.pointer_context {
            old.buttons = context.previous_buttons;
        }
        {
            let mut pointers = self.pen_pointers.safe_borrow_mut(cx.no_gc());
            let p = pointers.get_mut(&sample.id).unwrap();
            p.buttons = sample.buttons;
            p.client = client;
            p.page = page;
            p.sample = sample;
            p.actual = actual_element.as_deref().map(Dom::from_ref);
            if in_pipeline {
                p.owned = true;
            }
        }
        self.process_pending_pointer_capture(cx, old.id, "pen", old.primary);
        let capture = self.get_pointer_capture_target(old.id);
        if capture.is_none() && actual_element.is_none() {
            self.leave_pen_document(cx, sample, input);
            return InputEventResult::PenRouteRejected;
        }
        let element = capture.as_ref().or(actual_element.as_ref()).unwrap();
        let target = capture.as_deref().unwrap_or(element);
        let hit = actual_hit
            .or_else(|| self.hit_test_for_capture(input, target))
            .unwrap();
        let hover = self.pen_snapshot(sample.id).unwrap().hover;
        self.pen_boundaries(
            cx,
            sample,
            old.id,
            old.primary,
            client,
            page,
            hover.as_deref(),
            Some(target),
            input.active_keyboard_modifiers,
        );
        let changed = old.buttons ^ sample.buttons;
        let first_down = old.buttons == 0 && sample.buttons != 0;
        let last_up = old.buttons != 0 && sample.buttons == 0;
        let tip_start = old.buttons & 33 == 0 && sample.buttons & 33 != 0;
        let tip_end = old.buttons & 33 != 0 && sample.buttons & 33 == 0;
        let button = match changed.trailing_zeros() {
            0 => MouseButton::Primary,
            1 => MouseButton::Secondary,
            2 => MouseButton::Auxiliary,
            3 => MouseButton::Back,
            4 => MouseButton::Forward,
            5 => MouseButton::Other(5),
            _ => MouseButton::None,
        };
        let name = if first_down {
            "pointerdown"
        } else if last_up {
            "pointerup"
        } else {
            "pointermove"
        };
        let moved_too_far =
            old.down_point.distance_to(client) > 10. * self.window.device_pixel_ratio().get();
        {
            let mut pointers = self.pen_pointers.safe_borrow_mut(cx.no_gc());
            let p = pointers.get_mut(&sample.id).unwrap();
            p.buttons = sample.buttons;
            p.client = client;
            p.page = page;
            p.sample = sample;
            p.hover = Some(Dom::from_ref(target));
            p.actual = actual_element.as_deref().map(Dom::from_ref);
            if first_down {
                p.suppress_mouse = false;
            }
            if tip_start {
                p.down = Some(Dom::from_ref(target));
                p.down_point = client;
                p.click_allowed = sample.buttons & 1 != 0;
            } else if moved_too_far {
                p.click_allowed = false;
            }
            for mask in [2, 4, 8, 16, 32] {
                if changed & mask != 0 && sample.buttons & mask != 0 {
                    p.auxiliary.insert(
                        mask,
                        PenButtonPress {
                            target: Dom::from_ref(target),
                            point: client,
                            click_allowed: true,
                        },
                    );
                } else if let Some(press) = p.auxiliary.get_mut(&mask)
                    && press.point.distance_to(client)
                        > 10. * self.window.device_pixel_ratio().get()
                {
                    press.click_allowed = false;
                }
            }
        }
        // Direct pen contact implicitly captures its starting target. Pointer
        // handlers may replace or release that pending capture.
        if first_down && sample.buttons & 33 != 0 {
            self.set_pending_pointer_capture(old.id, target);
        }
        let pointer = self.pen_dom_event(
            cx,
            name,
            sample,
            old.id,
            old.primary,
            client,
            page,
            button,
            Some(hit.point_in_node),
            input.active_keyboard_modifiers,
        );
        #[cfg(feature = "webxr")]
        let xr_ending = tip_end && self.window.has_inline_xr_pointer(old.id);
        #[cfg(not(feature = "webxr"))]
        let xr_ending = false;
        let deferred = last_up && xr_ending;
        let allowed = if deferred {
            true
        } else {
            let allowed = pointer.upcast::<Event>().fire(cx, target.upcast());
            if last_up {
                self.implicit_release_pointer_capture(cx, old.id, "pen", old.primary);
            }
            allowed
        };
        if first_down && !allowed {
            if let Some(p) = self.pen_pointers.borrow_mut().get_mut(&sample.id) {
                p.suppress_mouse = true;
            }
        }
        #[cfg(feature = "webxr")]
        {
            let screen = ScreenPointerSample::from_hit(&hit).filter(|s| s.targets(target.upcast()));
            if tip_start {
                self.window.inline_xr_pointer_event(
                    cx,
                    old.id,
                    InlinePointerAction::Down {
                        primary: old.primary && sample.buttons & 1 != 0,
                    },
                    screen.as_ref(),
                );
            } else if xr_ending {
                self.window.inline_xr_pointer_event(
                    cx,
                    old.id,
                    InlinePointerAction::Select,
                    screen.as_ref(),
                );
            } else if self.window.has_inline_xr_pointer(old.id) {
                self.window.inline_xr_pointer_event(
                    cx,
                    old.id,
                    InlinePointerAction::Move,
                    screen.as_ref(),
                );
            }
        }
        let current = self.pen_snapshot(sample.id).unwrap();
        let compatibility =
            old.primary && !current.suppress_mouse && self.mouse_button_state.get().is_empty();
        if compatibility {
            // Compatibility editing uses the same click detail as this pen,
            // while preserving the independent physical mouse click sequence.
            let mouse_clicks = self.click_counting_info.borrow().clone();
            let mouse_down = self.last_mouse_button_down_point.get();
            let target_id = element
                .upcast::<Node>()
                .unique_id(self.window.pipeline_id());
            let pen_clicks = self
                .pen_clicks
                .borrow()
                .get(&sample.id)
                .filter(|click| click.target == target_id)
                .map(|click| click.info.clone())
                .unwrap_or_default();
            *self.click_counting_info.safe_borrow_mut(cx.no_gc()) = pen_clicks;
            let make = |event: InputEvent, buttons: u16| ConstellationInputEvent {
                pointer_context: None,
                hit_test_result: input.hit_test_result.clone(),
                pressed_mouse_buttons: MouseButtons::from_bits_retain(buttons & 31),
                active_keyboard_modifiers: input.active_keyboard_modifiers,
                event: event.into(),
            };
            if changed == 0 {
                self.handle_mouse_move_event_with_target(
                    cx,
                    &make(
                        InputEvent::MouseMove(MouseMoveEvent::new_compatibility_for_touch(
                            sample.point,
                        )),
                        sample.buttons,
                    ),
                    capture.as_deref(),
                );
            }
            for (mask, mouse_button) in [
                (1, MouseButton::Primary),
                (2, MouseButton::Secondary),
                (4, MouseButton::Auxiliary),
                (8, MouseButton::Back),
                (16, MouseButton::Forward),
            ] {
                if changed & mask == 0 {
                    continue;
                }
                let action = if sample.buttons & mask != 0 {
                    MouseButtonAction::Down
                } else {
                    MouseButtonAction::Up
                };
                let mut mouse = MouseButtonEvent::new_compatibility_for_touch(
                    action,
                    mouse_button,
                    sample.point,
                );
                mouse.pointer_manages_activation = true;
                self.handle_mouse_button_event_with_target(
                    cx,
                    mouse,
                    &make(InputEvent::MouseButton(mouse), sample.buttons),
                    capture.as_deref(),
                );
            }
            *self.click_counting_info.safe_borrow_mut(cx.no_gc()) = mouse_clicks;
            self.last_mouse_button_down_point.set(mouse_down);
        }
        // click remains independent of preventDefault on pointerdown. It is a
        // PointerEvent carrying this pen's identity and final properties.
        if tip_end
            && old.primary
            && current.click_allowed
            && !moved_too_far
            && let Some(down) = current.down.as_ref()
            && down.upcast::<Node>().is_connected()
            && target.upcast::<Node>().is_connected()
        {
            let click_target = if capture.is_some() {
                Some(DomRoot::from_ref(target.upcast::<Node>()))
            } else {
                down.upcast::<Node>()
                    .common_ancestor_in_flat_tree(cx.no_gc(), target.upcast())
            };
            if let Some(click_target) = click_target {
                let count = self.record_pen_click(
                    cx,
                    sample.id,
                    &click_target,
                    MouseButton::Primary,
                    client,
                );
                let click_element = click_target.downcast::<Element>();
                let previous_click_in_progress =
                    click_element.is_some_and(|element| element.click_in_progress());
                if let Some(element) = click_element {
                    self.most_recently_clicked_element.set(Some(element));
                    element.set_click_in_progress(true);
                }
                let click = self.pen_dom_event(
                    cx,
                    "click",
                    sample,
                    old.id,
                    old.primary,
                    client,
                    page,
                    MouseButton::Primary,
                    Some(hit.point_in_node),
                    input.active_keyboard_modifiers,
                );
                click.upcast::<Event>().fire(cx, click_target.upcast());
                if let Some(element) = click_element {
                    element.set_click_in_progress(previous_click_in_progress);
                }
                if count.is_multiple_of(2) && click_target.is_connected() {
                    let mouse = MouseButtonEvent::new_compatibility_for_touch(
                        MouseButtonAction::Up,
                        MouseButton::Primary,
                        sample.point,
                    );
                    MouseEvent::for_platform_button_event(
                        cx,
                        Atom::from("dblclick"),
                        mouse,
                        MouseButtons::from_bits_retain(sample.buttons & 31),
                        &self.window,
                        &hit,
                        input.active_keyboard_modifiers,
                        2,
                    )
                    .upcast::<Event>()
                    .fire(cx, click_target.upcast());
                }
            }
        }
        // Each non-primary button has its own press target and movement guard;
        // chorded releases do not need to end the pen's active-buttons lifetime.
        for (mask, down, eligible) in &current.auxiliary {
            if !eligible
                || changed & mask == 0
                || sample.buttons & mask != 0
                || !down.upcast::<Node>().is_connected()
                || !target.upcast::<Node>().is_connected()
            {
                continue;
            }
            let aux_target = if capture.is_some() {
                Some(DomRoot::from_ref(target.upcast::<Node>()))
            } else {
                down.upcast::<Node>()
                    .common_ancestor_in_flat_tree(cx.no_gc(), target.upcast())
            };
            if let Some(aux_target) = aux_target {
                let button = match mask {
                    2 => MouseButton::Secondary,
                    4 => MouseButton::Auxiliary,
                    8 => MouseButton::Back,
                    16 => MouseButton::Forward,
                    _ => MouseButton::Other(5),
                };
                self.record_pen_click(cx, sample.id, &aux_target, button, client);
                self.pen_dom_event(
                    cx,
                    "auxclick",
                    sample,
                    old.id,
                    old.primary,
                    client,
                    page,
                    button,
                    Some(hit.point_in_node),
                    input.active_keyboard_modifiers,
                )
                .upcast::<Event>()
                .fire(cx, aux_target.upcast());
            }
        }
        if changed & 2 != 0 && sample.buttons & 2 != 0 {
            let menu = self.pen_dom_event(
                cx,
                "contextmenu",
                sample,
                old.id,
                old.primary,
                client,
                page,
                MouseButton::Secondary,
                Some(hit.point_in_node),
                input.active_keyboard_modifiers,
            );
            if menu.upcast::<Event>().fire(cx, target.upcast()) {
                self.window
                    .Document()
                    .embedder_controls()
                    .show_context_menu(cx.no_gc(), &hit);
            }
        }
        #[cfg(feature = "webxr")]
        if xr_ending {
            self.window
                .inline_xr_pointer_event(cx, old.id, InlinePointerAction::End, None);
        }
        if deferred {
            pointer.upcast::<Event>().fire(cx, target.upcast());
        }
        if last_up && deferred {
            self.implicit_release_pointer_capture(cx, old.id, "pen", old.primary);
        }
        if last_up && compatibility && capture.is_some() {
            let hover = ConstellationInputEvent {
                pointer_context: None,
                hit_test_result: input.hit_test_result.clone(),
                pressed_mouse_buttons: MouseButtons::empty(),
                active_keyboard_modifiers: input.active_keyboard_modifiers,
                event: InputEvent::MouseMove(MouseMoveEvent::new_compatibility_for_touch(
                    sample.point,
                ))
                .into(),
            };
            self.handle_native_mouse_move_event(cx, &hover);
        }
        let compat_hover = self.current_hover_target.get();
        let final_hover = self.get_pointer_capture_target(old.id).or(actual_element);
        if let Some(p) = self.pen_pointers.borrow_mut().get_mut(&sample.id) {
            if let Some(final_hover) = final_hover {
                p.hover = Some(Dom::from_ref(&*final_hover));
            }
            if compatibility {
                p.compat_hover = compat_hover.as_deref().map(Dom::from_ref);
            }
            if tip_end {
                p.click_allowed = false;
                p.down = None;
            }
            p.auxiliary.retain(|button, _| sample.buttons & button != 0);
            if last_up {
                p.suppress_mouse = false;
            }
        }
        InputEventResult::default()
    }
}
