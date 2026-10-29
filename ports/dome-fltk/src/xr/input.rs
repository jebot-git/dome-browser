// SPDX-License-Identifier: GPL-3.0-or-later
//! Translate the host's bounded transition history into WebXR source lifetimes.
use super::*;
use webxr_api::{
    GamepadLayout, Handedness, InputFrame, InputId, SelectEvent, SelectKind, TargetRayMode,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Hand {
    None,
    Left,
    Right,
}

#[derive(Clone, Deserialize)]
pub(super) struct HostInput {
    id: u32,
    handedness: Hand,
    profiles: Vec<String>,
    supports_grip: bool,
    aim: Option<Pose>,
    grip: Option<Pose>,
    select: bool,
    squeeze: bool,
    #[serde(default = "active_by_default")]
    select_active: bool,
    #[serde(default = "active_by_default")]
    squeeze_active: bool,
    buttons: Vec<f32>,
    #[serde(default)]
    touches: Vec<bool>,
    axes: Vec<f32>,
}

fn active_by_default() -> bool {
    true
}

impl HostInput {
    fn valid(&self) -> bool {
        self.id != 0
            && !self.profiles.is_empty()
            && self.profiles.len() <= 4
            && self.profiles.iter().all(|p| {
                !p.is_empty()
                    && p.len() <= 96
                    && p.bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            })
            && self.aim.as_ref().is_none_or(Pose::valid)
            && self.grip.as_ref().is_none_or(Pose::valid)
            && (self.supports_grip || self.grip.is_none())
            && self.buttons.len() <= 8
            && self.axes.len() <= 4
            && (self.touches.is_empty() || self.touches.len() == self.buttons.len())
            && self
                .buttons
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
            && self
                .axes
                .iter()
                .all(|v| v.is_finite() && (-1.0..=1.0).contains(v))
    }
    fn same_source(&self, other: &Self) -> bool {
        self.id == other.id
            && self.handedness == other.handedness
            && self.profiles == other.profiles
            && self.supports_grip == other.supports_grip
            && self.buttons.len() == other.buttons.len()
            && self.axes.len() == other.axes.len()
    }
    fn source(&self) -> InputSource {
        InputSource {
            id: InputId(self.id),
            handedness: match self.handedness {
                Hand::None => Handedness::None,
                Hand::Left => Handedness::Left,
                Hand::Right => Handedness::Right,
            },
            target_ray_mode: TargetRayMode::TrackedPointer,
            supports_grip: self.supports_grip,
            hand_support: None,
            profiles: self.profiles.clone(),
            gamepad: Some(GamepadLayout {
                buttons: self.buttons.len(),
                axes: self.axes.len(),
                xr_standard: self.supports_grip && !self.buttons.is_empty(),
            }),
        }
    }
    fn frame(&self, local: RigidTransform3D<f32, Native, Native>, allowed: bool) -> InputFrame {
        let allowed = allowed && self.aim.is_some();
        let mut button_values = if allowed {
            self.buttons.clone()
        } else {
            vec![0.; self.buttons.len()]
        };
        for (i, active) in [self.select_active, self.squeeze_active]
            .into_iter()
            .enumerate()
        {
            if !active {
                if let Some(value) = button_values.get_mut(i) {
                    *value = 0.;
                }
            }
        }
        let button_pressed: Vec<_> = button_values
            .iter()
            .enumerate()
            .map(|(i, v)| {
                allowed
                    && match i {
                        0 => self.select && self.select_active,
                        1 => self.squeeze && self.squeeze_active,
                        _ => *v >= 0.5,
                    }
            })
            .collect();
        let button_touched: Vec<_> = button_values
            .iter()
            .enumerate()
            .map(|(i, v)| {
                allowed
                    && (self.touches.get(i).copied().unwrap_or(false)
                        || button_pressed[i]
                        || *v > 0.)
            })
            .collect();
        let mut axis_values = if allowed {
            self.axes.clone()
        } else {
            vec![0.; self.axes.len()]
        };
        if !button_touched.get(2).copied().unwrap_or(false) {
            for v in axis_values.iter_mut().take(2) {
                *v = 0.;
            }
        }
        InputFrame {
            id: InputId(self.id),
            target_ray_emulated_position: false,
            grip_emulated_position: false,
            target_ray_origin: self
                .aim
                .as_ref()
                .filter(|_| allowed)
                .map(|p| p.transform::<webxr_api::Input, Native>().then(&local)),
            grip_origin: self
                .grip
                .as_ref()
                .filter(|_| allowed)
                .map(|p| p.transform::<webxr_api::Input, Native>().then(&local)),
            pressed: allowed && self.select && self.select_active,
            squeezed: allowed && self.squeeze && self.squeeze_active,
            hand: None,
            button_values,
            button_pressed,
            button_touched,
            axis_values,
            input_changed: true,
        }
    }
}

#[derive(Clone, Deserialize)]
pub(super) struct Batch {
    version: u32,
    through: u64,
    reset: bool,
    updates: Vec<Update>,
}
#[derive(Clone, Deserialize)]
struct Update {
    sequence: u64,
    frame: HostFrame,
}

pub(super) fn valid_inputs(inputs: &[HostInput]) -> bool {
    inputs.len() <= 2
        && inputs.iter().all(HostInput::valid)
        && (inputs.len() != 2 || inputs[0].id != inputs[1].id)
}

/// A held control cannot start a new gesture after interruption or connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gesture {
    Suppressed,
    Ready,
    Active,
}
impl Gesture {
    fn baseline(down: bool) -> Self {
        if down { Self::Suppressed } else { Self::Ready }
    }
    fn sample(&mut self, down: bool, allowed: bool) -> Option<SelectEvent> {
        if !allowed {
            let event = (*self == Self::Active).then_some(SelectEvent::End);
            *self = Self::Suppressed;
            return event;
        }
        match (*self, down) {
            (Self::Suppressed, false) => {
                *self = Self::Ready;
                None
            },
            (Self::Ready, true) => {
                *self = Self::Active;
                Some(SelectEvent::Start)
            },
            (Self::Active, false) => {
                *self = Self::Ready;
                Some(SelectEvent::Select)
            },
            _ => None,
        }
    }
}
struct Source {
    info: HostInput,
    select: Gesture,
    squeeze: Gesture,
}
impl Source {
    fn new(info: HostInput, allowed: bool) -> Self {
        Self {
            select: if allowed && info.select_active {
                Gesture::baseline(info.select)
            } else {
                Gesture::Suppressed
            },
            squeeze: if allowed && info.squeeze_active {
                Gesture::baseline(info.squeeze)
            } else {
                Gesture::Suppressed
            },
            info,
        }
    }
    fn cancel(&mut self, frame: &Frame, events: &mut Vec<Event>) {
        for (gesture, kind) in [
            (&mut self.select, SelectKind::Select),
            (&mut self.squeeze, SelectKind::Squeeze),
        ] {
            if let Some(event) = gesture.sample(false, false) {
                let mut frame = frame.clone();
                frame.inputs.retain(|i| i.id != InputId(self.info.id));
                frame
                    .inputs
                    .push(self.info.frame(RigidTransform3D::identity(), false));
                events.push(Event::Select(InputId(self.info.id), kind, event, frame));
            }
        }
    }
}

pub(super) struct State {
    floor_transform: Option<RigidTransform3D<f32, Native, Floor>>,
    sources: Vec<Source>,
    cursor: u64,
}
impl State {
    pub fn new(frame: &HostFrame) -> Result<Self> {
        Self::validate(frame, 0)?;
        Ok(Self {
            floor_transform: None,
            sources: frame
                .inputs
                .iter()
                .cloned()
                .map(|i| {
                    let allowed = frame.input_allowed() && i.aim.is_some();
                    Source::new(i, allowed)
                })
                .collect(),
            cursor: frame.input_batch.as_ref().unwrap().through,
        })
    }
    pub fn set_floor_transform(&mut self, floor: Option<RigidTransform3D<f32, Native, Floor>>) {
        self.floor_transform = floor;
    }
    fn web_frame(&self, host: &HostFrame, anchor: RigidTransform3D<f32, Native, Native>,
        clip: ClipPlanes, clock: &time::Clock) -> Frame {
        let mut frame = host.web_frame(anchor, clip, clock);
        frame.floor_transform = self.floor_transform;
        frame
    }
    pub fn cursor(&self) -> u64 {
        self.cursor
    }
    pub fn sources(&self) -> Vec<InputSource> {
        self.sources.iter().map(|s| s.info.source()).collect()
    }
    fn validate(frame: &HostFrame, cursor: u64) -> Result<()> {
        let batch = frame
            .input_batch
            .as_ref()
            .ok_or("Missing WebXR input history")?;
        if batch.version != 1
            || batch.through < cursor
            || batch.updates.len() > 64
            || (batch.reset && !batch.updates.is_empty())
        {
            return Err("Invalid WebXR input history".into());
        }
        let mut sequence = cursor;
        for update in &batch.updates {
            if sequence.checked_add(1) != Some(update.sequence)
                || !update.frame.valid()
                || update.frame.input_batch.is_some()
                || update.frame.frame_id > frame.frame_id
            {
                return Err("Discontinuous WebXR input history".into());
            }
            sequence = update.sequence;
        }
        if !batch.reset && sequence != batch.through {
            return Err("Incomplete WebXR input history".into());
        }
        Ok(())
    }
    pub fn cancel(&mut self, frame: &Frame) -> Vec<Event> {
        let mut events = vec![];
        for source in &mut self.sources {
            source.cancel(frame, &mut events);
        }
        events
    }
    pub fn update(
        &mut self,
        host: &HostFrame,
        anchor: RigidTransform3D<f32, Native, Native>,
        clip: ClipPlanes,
        clock: &time::Clock,
        reset_page: bool,
    ) -> Result<Vec<Event>> {
        Self::validate(host, self.cursor)?;
        let batch = host.input_batch.as_ref().unwrap();
        let mut events = vec![];
        if batch.reset || reset_page {
            events.extend(self.cancel(&self.web_frame(host, anchor, clip, clock)));
            self.observe(host, anchor, clip, clock, true, &mut events);
        } else {
            for update in &batch.updates {
                self.observe(&update.frame, anchor, clip, clock, false, &mut events);
            }
            self.observe(host, anchor, clip, clock, false, &mut events);
        }
        self.cursor = batch.through;
        Ok(events)
    }
    fn observe(
        &mut self,
        host: &HostFrame,
        anchor: RigidTransform3D<f32, Native, Native>,
        clip: ClipPlanes,
        clock: &time::Clock,
        reset: bool,
        events: &mut Vec<Event>,
    ) {
        let frame = self.web_frame(host, anchor, clip, clock);
        let mut retained = vec![];
        for mut source in self.sources.drain(..) {
            if !host.inputs.iter().any(|i| i.id == source.info.id) {
                source.cancel(&frame, events);
                events.push(Event::RemoveInput(InputId(source.info.id)));
            } else {
                retained.push(source);
            }
        }
        self.sources = retained;
        for input in &host.inputs {
            let Some(source) = self.sources.iter_mut().find(|s| s.info.id == input.id) else {
                self.sources.push(Source::new(
                    input.clone(),
                    host.input_allowed() && input.aim.is_some(),
                ));
                events.push(Event::AddInput(input.source()));
                continue;
            };
            if !source.info.same_source(input) {
                source.cancel(&frame, events);
                *source = Source::new(input.clone(), host.input_allowed() && input.aim.is_some());
                events.push(Event::UpdateInput(InputId(input.id), input.source()));
                continue;
            }
            if reset {
                let baseline =
                    Source::new(input.clone(), host.input_allowed() && input.aim.is_some());
                source.select = baseline.select;
                source.squeeze = baseline.squeeze;
            } else {
                let allowed = host.input_allowed() && input.aim.is_some();
                for (gesture, down, active, kind) in [
                    (
                        &mut source.select,
                        input.select,
                        input.select_active,
                        SelectKind::Select,
                    ),
                    (
                        &mut source.squeeze,
                        input.squeeze,
                        input.squeeze_active,
                        SelectKind::Squeeze,
                    ),
                ] {
                    if let Some(event) = gesture.sample(down, allowed && active) {
                        events.push(Event::Select(InputId(input.id), kind, event, frame.clone()));
                    }
                }
            }
            source.info = input.clone();
        }
    }
}

pub(super) fn frames(
    host: &HostFrame,
    anchor: RigidTransform3D<f32, Native, Native>,
) -> Vec<InputFrame> {
    host.inputs
        .iter()
        .map(|input| input.frame(anchor.inverse(), host.input_allowed()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(id: u64, down: bool) -> HostFrame {
        serde_json::from_value(json!({"frame_id":id,"time":id*10_000_000,"should_render":true,"visible":true,"focused":true,
            "head_flags":3,"view_flags":3,"head":{"position":[1,1.6,2],"orientation":[0,0,0,1]},
            "views":[{"pose":{"position":[0.97,1.6,2],"orientation":[0,0,0,1]},"fov":{"left":-0.7,"right":0.7,"up":0.7,"down":-0.7}},
                {"pose":{"position":[1.03,1.6,2],"orientation":[0,0,0,1]},"fov":{"left":-0.7,"right":0.7,"up":0.7,"down":-0.7}}],
            "inputs":[{"id":1,"handedness":"left","profiles":["generic-trigger-squeeze-touchpad"],"supports_grip":true,
                "aim":{"position":[1.5,1.4,1],"orientation":[0,0,0,1]},"grip":{"position":[1.4,1.3,1],"orientation":[0,0,0,1]},
                "select":down,"squeeze":false,"buttons":[if down{1}else{0},0,0],"touches":[down,false,true],"axes":[0.5,-0.5]}],
            "input_batch":{"version":1,"through":0,"reset":false,"updates":[]}})).unwrap()
    }
    fn update(
        state: &mut State,
        mut host: HostFrame,
        mut transitions: Vec<HostFrame>,
        reset_page: bool,
    ) -> Vec<Event> {
        let mut sequence = state.cursor;
        let updates = transitions
            .drain(..)
            .map(|mut f| {
                sequence += 1;
                f.input_batch = None;
                Update { sequence, frame: f }
            })
            .collect();
        host.input_batch = Some(Batch {
            version: 1,
            through: sequence,
            reset: false,
            updates,
        });
        state
            .update(
                &host,
                anchor(&host.head),
                ClipPlanes::default(),
                &time::Clock::default(),
                reset_page,
            )
            .unwrap()
    }
    fn selects(events: &[Event]) -> Vec<(SelectKind, SelectEvent, f64)> {
        events
            .iter()
            .filter_map(|e| {
                if let Event::Select(_, kind, event, f) = e {
                    Some((*kind, *event, f.predicted_display_time))
                } else {
                    None
                }
            })
            .collect()
    }
    #[test]
    fn press_and_release_between_frames_retain_event_poses_and_times() {
        let initial = frame(1, false);
        let mut state = State::new(&initial).unwrap();
        let mut pressed = frame(2, true);
        pressed.inputs[0].aim.as_mut().unwrap().position[0] = 3.;
        let events = update(
            &mut state,
            frame(4, false),
            vec![pressed, frame(3, false)],
            false,
        );
        assert_eq!(
            selects(&events),
            [
                (SelectKind::Select, SelectEvent::Start, 20.),
                (SelectKind::Select, SelectEvent::Select, 30.)
            ]
        );
        if let Event::Select(_, _, _, f) = &events[0] {
            assert!((f.inputs[0].target_ray_origin.unwrap().translation.x - 2.).abs() < 1e-5);
        } else {
            panic!()
        }
    }
    #[test]
    fn held_initial_control_and_refocus_require_release_before_new_gesture() {
        let initial = frame(1, true);
        let mut state = State::new(&initial).unwrap();
        assert!(selects(&update(&mut state, frame(2, true), vec![], false)).is_empty());
        assert!(selects(&update(&mut state, frame(3, false), vec![], false)).is_empty());
        assert_eq!(
            selects(&update(&mut state, frame(4, true), vec![], false))[0].1,
            SelectEvent::Start
        );
        let mut blurred = frame(5, true);
        blurred.focused = false;
        assert_eq!(
            selects(&update(&mut state, blurred, vec![], false))[0].1,
            SelectEvent::End
        );
        assert!(selects(&update(&mut state, frame(6, true), vec![], false)).is_empty());
        update(&mut state, frame(7, false), vec![], false);
        assert_eq!(
            selects(&update(&mut state, frame(8, true), vec![], false))[0].1,
            SelectEvent::Start
        );
    }
    #[test]
    fn focus_interruptions_between_reads_discard_pending_gestures() {
        let mut state = State::new(&frame(1, false)).unwrap();
        let events = update(
            &mut state,
            frame(4, true),
            vec![frame(2, true), frame(3, false)],
            true,
        );
        assert!(selects(&events).is_empty());
        assert!(selects(&update(&mut state, frame(5, true), vec![], false)).is_empty());
        update(&mut state, frame(6, false), vec![], false);
        assert_eq!(
            selects(&update(&mut state, frame(7, true), vec![], false))[0].1,
            SelectEvent::Start
        );
    }
    #[test]
    fn disconnect_tracking_loss_profile_change_and_shutdown_only_cancel() {
        for mode in 0..4 {
            let mut state = State::new(&frame(1, false)).unwrap();
            update(&mut state, frame(2, true), vec![], false);
            let mut next = frame(3, true);
            match mode {
                0 => next.inputs.clear(),
                1 => next.inputs[0].aim = None,
                2 => next.inputs[0].profiles = vec!["oculus-touch".into()],
                _ => {},
            }
            let events = if mode == 3 {
                state.cancel(&next.web_frame(
                    anchor(&next.head),
                    ClipPlanes::default(),
                    &time::Clock::default(),
                ))
            } else {
                update(&mut state, next, vec![], false)
            };
            assert_eq!(
                selects(&events).iter().map(|e| e.1).collect::<Vec<_>>(),
                [SelectEvent::End]
            );
            if let Event::Select(_, _, _, f) = &events[0] {
                assert!(!f.inputs[0].pressed);
                assert!(f.inputs[0].button_values.iter().all(|v| *v == 0.));
            }
        }
    }
    #[test]
    fn overflow_reset_cancels_and_rebases_without_replaying_held_button() {
        let mut state = State::new(&frame(1, false)).unwrap();
        update(&mut state, frame(2, true), vec![], false);
        let mut next = frame(3, true);
        next.input_batch = Some(Batch {
            version: 1,
            through: 90,
            reset: true,
            updates: vec![],
        });
        let events = state
            .update(
                &next,
                anchor(&next.head),
                ClipPlanes::default(),
                &time::Clock::default(),
                false,
            )
            .unwrap();
        assert_eq!(
            selects(&events).iter().map(|e| e.1).collect::<Vec<_>>(),
            [SelectEvent::End]
        );
        assert_eq!(state.cursor, 90);
        assert!(selects(&update(&mut state, frame(4, false), vec![], false)).is_empty());
    }
    #[test]
    fn invalid_history_is_rejected_before_any_source_or_gesture_changes() {
        let mut state = State::new(&frame(1, false)).unwrap();
        let mut next = frame(3, true);
        let mut edge = frame(2, true);
        edge.input_batch = None;
        next.input_batch = Some(Batch {
            version: 1,
            through: 2,
            reset: false,
            updates: vec![Update {
                sequence: 2,
                frame: edge,
            }],
        });
        assert!(
            state
                .update(
                    &next,
                    anchor(&next.head),
                    ClipPlanes::default(),
                    &time::Clock::default(),
                    false
                )
                .is_err()
        );
        assert_eq!(state.cursor, 0);
        assert_eq!(state.sources[0].select, Gesture::Ready);
        next.inputs[0].axes[0] = f32::NAN;
        assert!(!next.valid());
    }
    #[test]
    fn action_deactivation_cancels_and_reactivation_cannot_replay_a_held_press() {
        let mut state = State::new(&frame(1, false)).unwrap();
        update(&mut state, frame(2, true), vec![], false);
        let mut inactive = frame(3, true);
        inactive.inputs[0].select_active = false;
        let events = update(&mut state, inactive.clone(), vec![], false);
        assert_eq!(
            selects(&events).iter().map(|e| e.1).collect::<Vec<_>>(),
            [SelectEvent::End]
        );
        let sample = inactive.web_frame(
            anchor(&inactive.head),
            ClipPlanes::default(),
            &time::Clock::default(),
        );
        assert!(!sample.inputs[0].button_pressed[0]);
        assert_eq!(sample.inputs[0].button_values[0], 0.);
        assert!(selects(&update(&mut state, frame(4, true), vec![], false)).is_empty());
        update(&mut state, frame(5, false), vec![], false);
        assert_eq!(
            selects(&update(&mut state, frame(6, true), vec![], false))[0].1,
            SelectEvent::Start
        );
        let mut source = frame(1, false);
        source.inputs[0].select_active = false;
        let mut state = State::new(&source).unwrap();
        assert!(selects(&update(&mut state, frame(2, true), vec![], false)).is_empty());
    }

    #[test]
    fn undefined_untracked_poses_keep_the_session_alive_and_expose_no_tracking_or_controls() {
        let mut host = frame(2, true);
        host.head_flags = 0;
        host.view_flags = 0;
        host.head.orientation = [0.; 4];
        for view in &mut host.views {
            view.pose.orientation = [0.; 4];
        }
        assert!(host.valid());
        let sample = host.web_frame(
            RigidTransform3D::identity(),
            ClipPlanes::default(),
            &time::Clock::default(),
        );
        assert!(sample.pose.is_none());
        assert!(sample.inputs[0].target_ray_origin.is_none());
        assert!(!sample.inputs[0].pressed);
        assert!(sample.inputs[0].button_values.iter().all(|v| *v == 0.));
    }

    #[test]
    fn squeeze_is_independent_and_touch_is_not_a_press() {
        let initial = frame(1, false);
        let mut state = State::new(&initial).unwrap();
        let mut next = frame(2, false);
        next.inputs[0].squeeze = true;
        next.inputs[0].buttons[1] = 1.;
        let events = update(&mut state, next.clone(), vec![], false);
        assert_eq!(
            selects(&events)[0],
            (SelectKind::Squeeze, SelectEvent::Start, 20.)
        );
        let f = next.web_frame(
            anchor(&next.head),
            ClipPlanes::default(),
            &time::Clock::default(),
        );
        assert_eq!(f.inputs[0].button_touched, [false, true, true]);
        assert_eq!(f.inputs[0].button_pressed, [false, true, false]);
        assert_eq!(f.inputs[0].axis_values, [0.5, -0.5]);
        next.focused = false;
        let f = next.web_frame(
            anchor(&next.head),
            ClipPlanes::default(),
            &time::Clock::default(),
        );
        assert!(f.inputs[0].button_touched.iter().all(|v| !*v));
        assert_eq!(f.inputs[0].axis_values, [0., 0.]);
    }
}
