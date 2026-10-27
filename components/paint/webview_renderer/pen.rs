/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::VecDeque;

use rustc_hash::FxHashSet;
use webrender_api::units::WorldPoint;

use super::*;

const MAX_PENS: usize = 64;
const MAX_QUEUED_SAMPLES: usize = 128;

struct Queued {
    event: InputEventAndId,
    external: bool,
}

struct Stream {
    pointer_id: i32,
    buttons: u16,
    owner: PaintHitTestResult,
    queue: VecDeque<Queued>,
    in_flight: bool,
    blocked: bool,
}

struct Pending {
    native_id: i32,
    queued: Queued,
    route: PaintHitTestResult,
    actual: Option<PaintHitTestResult>,
    previous_buttons: u16,
    retried: bool,
}

#[derive(Default)]
pub(super) struct PenInput {
    streams: FxHashMap<i32, Stream>,
    pending: FxHashMap<InputEventId, Pending>,
    blocked: FxHashSet<i32>,
}

impl WebViewRenderer {
    pub(super) fn publish_pointer_state(&self, state: PointerState) {
        let _ = self.embedder_to_constellation_sender.send(
            EmbedderToConstellationMessage::UpdatePointerState(self.id, state),
        );
    }

    pub(super) fn on_pen_event(
        &mut self,
        api: &RenderApi,
        event: PenEvent,
        id: InputEventId,
    ) -> bool {
        self.enqueue_pen(api, event, id, true)
    }

    fn enqueue_pen(
        &mut self,
        api: &RenderApi,
        event: PenEvent,
        id: InputEventId,
        external: bool,
    ) -> bool {
        let event = event.normalized();
        if self.pen_input.blocked.contains(&event.id) {
            if event.event_type == PenEventType::Leave || event.buttons == 0 {
                self.pen_input.blocked.remove(&event.id);
            } else {
                return false;
            }
        }
        if !self.pen_input.streams.contains_key(&event.id) {
            if event.event_type != PenEventType::Sample
                || self.pen_input.streams.len()
                    + self
                        .pen_input
                        .blocked
                        .iter()
                        .filter(|id| !self.pen_input.streams.contains_key(id))
                        .count()
                    >= MAX_PENS
            {
                return false;
            }
            let point = event
                .point
                .as_device_point(self.device_pixels_per_page_pixel());
            let Some(owner) = self.hit_test(api, point).into_iter().next() else {
                return false;
            };
            let Some(pointer_id) = self.pointer_ids.allocate() else {
                return false;
            };
            self.pen_input.streams.insert(
                event.id,
                Stream {
                    pointer_id,
                    buttons: 0,
                    owner,
                    queue: VecDeque::new(),
                    in_flight: false,
                    blocked: false,
                },
            );
        }
        let stream = self
            .pen_input
            .streams
            .get_mut(&event.id)
            .expect("registered pen stream");
        if stream.blocked && event.event_type == PenEventType::Sample {
            if event.buttons != 0 {
                return false;
            }
            stream.blocked = false;
        }
        if stream.queue.len() >= MAX_QUEUED_SAMPLES {
            let discarded: Vec<_> = stream
                .queue
                .drain(..)
                .filter(|q| q.external)
                .map(|q| InputEventOutcome {
                    id: q.event.id,
                    result: InputEventResult::DispatchFailed,
                })
                .collect();
            let _ = self.embedder_to_constellation_sender.send(
                EmbedderToConstellationMessage::DiscardedInputEvents(self.id, discarded),
            );
            stream.blocked = true;
            self.pen_input.blocked.insert(event.id);
            let mut cancel = event;
            cancel.event_type = PenEventType::Cancel;
            stream.queue.push_back(Queued {
                event: InputEvent::Pen(cancel.normalized()).into(),
                external: false,
            });
            return false;
        }
        stream.queue.push_back(Queued {
            event: InputEventAndId {
                event: InputEvent::Pen(event),
                id,
            },
            external,
        });
        if stream.in_flight {
            return true;
        }
        let sent = self.pump_pen(api, event.id);
        if !sent {
            self.discard_pen_stream(event.id);
        }
        sent
    }

    fn discard_pen_stream(&mut self, native_id: i32) -> Vec<InputEventOutcome> {
        let Some(mut stream) = self.pen_input.streams.remove(&native_id) else {
            return Vec::new();
        };
        self.pointer_ids.release(stream.pointer_id);
        self.publish_pointer_state(PointerState {
            pointer_id: stream.pointer_id,
            active: false,
            buttons: 0,
            pipeline_id: stream.owner.pipeline_id,
        });
        stream
            .queue
            .drain(..)
            .filter(|q| q.external)
            .map(|q| q.event.id)
            .map(|id| InputEventOutcome {
                id,
                result: InputEventResult::DispatchFailed,
            })
            .collect()
    }

    /// Only one sample for a tool is in Script at a time. Capture changes made
    /// by that sample, or by a task before the next one, are observed before a
    /// different document can receive the following native sample.
    fn pump_pen(&mut self, api: &RenderApi, native_id: i32) -> bool {
        let Some(stream) = self.pen_input.streams.get_mut(&native_id) else {
            return false;
        };
        if stream.in_flight {
            return true;
        }
        let Some(mut queued) = stream.queue.pop_front() else {
            return true;
        };
        let InputEvent::Pen(mut sample) = queued.event.event else {
            unreachable!()
        };
        let mut route = stream.owner.clone();
        let previous_buttons = stream.buttons;
        let pointer_id = stream.pointer_id;
        let point = sample
            .point
            .as_device_point(self.device_pixels_per_page_pixel());
        let tester = api.request_hit_tester(self.webrender_document).resolve();
        let (hits, owner_point) = tester.hit_test_with_pipeline(
            WorldPoint::from_untyped(point.to_untyped()),
            Some(route.pipeline_id.into()),
        );
        let actual = hits.items.first().map(|hit| PaintHitTestResult {
            pipeline_id: hit.pipeline.into(),
            point_in_viewport: euclid::Point2D::from_untyped(hit.point_in_viewport.to_untyped()),
            external_scroll_id: ExternalScrollId(hit.tag.0, hit.pipeline),
        });
        if let Some(point) = owner_point {
            route.point_in_viewport = euclid::Point2D::from_untyped(point.to_untyped());
        } else if sample.event_type == PenEventType::Sample {
            // A removed/non-invertible frame has no valid local coordinates.
            // Cancel at the last valid point; do not invent a transform.
            sample.event_type = PenEventType::Cancel;
            sample = sample.normalized();
            queued.event.event = InputEvent::Pen(sample);
        }
        if sample.event_type == PenEventType::Cancel && previous_buttons != 0 {
            self.pen_input.blocked.insert(native_id);
        }
        let pending = Pending {
            native_id,
            queued,
            route,
            actual,
            previous_buttons,
            retried: false,
        };
        self.send_pen(pending, pointer_id)
    }

    fn send_pen(&mut self, pending: Pending, pointer_id: i32) -> bool {
        let id = pending.queued.event.id;
        let InputEvent::Pen(sample) = pending.queued.event.event else {
            unreachable!()
        };
        let context = PointerEventContext {
            pointer_id,
            previous_buttons: pending.previous_buttons,
            hit_in_pipeline: pending
                .actual
                .as_ref()
                .is_some_and(|hit| hit.pipeline_id == pending.route.pipeline_id),
            has_hit: pending.actual.is_some(),
        };
        self.publish_pointer_state(PointerState {
            pointer_id,
            active: true,
            buttons: sample.buttons,
            pipeline_id: pending.route.pipeline_id,
        });
        let event = pending.queued.event.clone();
        let route = pending.route.clone();
        let native_id = pending.native_id;
        self.pen_input.pending.insert(id, pending);
        if let Some(stream) = self.pen_input.streams.get_mut(&native_id) {
            stream.in_flight = true;
        }
        if self.dispatch_input_event_with_context(event, Some(route), Some(context)) {
            return true;
        }
        if let Some(pending) = self.pen_input.pending.remove(&id)
            && let Some(stream) = self.pen_input.streams.get_mut(&native_id)
        {
            stream.in_flight = false;
            stream.queue.push_front(pending.queued);
        }
        false
    }

    pub(super) fn handle_pen_input_result(
        &mut self,
        api: &RenderApi,
        id: InputEventId,
        mut result: InputEventResult,
    ) -> Option<Vec<InputEventOutcome>> {
        let mut pending = self.pen_input.pending.remove(&id)?;
        let native_id = pending.native_id;
        let stream = self.pen_input.streams.get_mut(&native_id)?;
        if result.contains(InputEventResult::PenRouteRejected) && !pending.retried {
            if let Some(actual) = pending
                .actual
                .as_ref()
                .filter(|hit| hit.pipeline_id != pending.route.pipeline_id)
            {
                pending.route = actual.clone();
                pending.retried = true;
                let pointer_id = stream.pointer_id;
                if self.send_pen(pending, pointer_id) {
                    return Some(Vec::new());
                }
                return Some(self.discard_pen_stream(native_id));
            }
        }
        result.remove(InputEventResult::PenRouteRejected);
        let InputEvent::Pen(sample) = pending.queued.event.event else {
            unreachable!()
        };
        stream.in_flight = false;
        stream.buttons = sample.buttons;
        stream.owner = pending.route.clone();
        let mut outcomes = Vec::new();
        if pending.queued.external {
            outcomes.push(InputEventOutcome { id, result });
        }
        if sample.event_type != PenEventType::Sample {
            let mut stream = self
                .pen_input
                .streams
                .remove(&native_id)
                .expect("acknowledged pen stream");
            self.pointer_ids.release(stream.pointer_id);
            self.publish_pointer_state(PointerState {
                pointer_id: stream.pointer_id,
                active: false,
                buttons: 0,
                pipeline_id: stream.owner.pipeline_id,
            });
            // Valid samples received after explicit cancellation start a new
            // pointer lifetime, in their original order.
            while let Some(queued) = stream.queue.pop_front() {
                let InputEvent::Pen(event) = queued.event.event else {
                    unreachable!()
                };
                if !self.enqueue_pen(api, event, queued.event.id, queued.external)
                    && queued.external
                {
                    outcomes.push(InputEventOutcome {
                        id: queued.event.id,
                        result: InputEventResult::DispatchFailed,
                    });
                }
            }
            return Some(outcomes);
        }
        if pending.previous_buttons != 0
            && sample.buttons == 0
            && pending
                .actual
                .as_ref()
                .is_none_or(|hit| hit.pipeline_id != pending.route.pipeline_id)
        {
            // After releasing capture, restore hover in the actual document
            // even if the hardware does not send another motion sample.
            stream.queue.push_front(Queued {
                event: InputEvent::Pen(sample).into(),
                external: false,
            });
        }
        if !self.pump_pen(api, native_id) {
            outcomes.extend(self.discard_pen_stream(native_id));
        }
        Some(outcomes)
    }
}
