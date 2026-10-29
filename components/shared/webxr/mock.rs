/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use euclid::{Point2D, Rect, RigidTransform3D, Transform3D};
use profile_traits::generic_callback::GenericCallback as ProfileGenericCallback;
use serde::{Deserialize, Serialize};
use servo_base::generic_channel::GenericReceiver;

use crate::{
    ApiSpace, DiscoveryAPI, Display, EntityType, Error, Floor, Handedness, Input, InputId,
    InputSource, Native, SelectEvent, SelectKind, TargetRayMode, Triangle, ViewEye, Viewer,
    Viewport, Visibility,
};

/// A trait for discovering mock XR devices
pub trait MockDiscoveryAPI<GL>: 'static {
    fn simulate_device_connection(
        &mut self,
        init: MockDeviceInit,
        receiver: GenericReceiver<MockDeviceMsg>,
    ) -> Result<Box<dyn DiscoveryAPI<GL>>, Error>;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MockDeviceInit {
    pub floor_origin: Option<RigidTransform3D<f32, Floor, Native>>,
    pub supports_inline: bool,
    pub supports_vr: bool,
    pub supports_ar: bool,
    pub viewer_origin: Option<RigidTransform3D<f32, Viewer, Native>>,
    pub views: MockViewsInit,
    pub secondary_views: MockViewsInit,
    pub supported_features: Vec<String>,
    pub world: Option<MockWorld>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MockViewInit<Eye> {
    pub transform: RigidTransform3D<f32, Viewer, Eye>,
    pub projection: Transform3D<f32, Eye, Display>,
    pub viewport: Rect<i32, Viewport>,
    /// field of view values, in radians
    pub fov: Option<(f32, f32, f32, f32)>,
    pub eye: ViewEye,
    pub is_first_person_observer: bool,
}

/// Mock devices accept ordered lists, including more than two primary views
/// and repeated eye labels, just as the WebXR test API does.
pub type MockViewsInit = Vec<MockViewInit<ApiSpace>>;

#[derive(Debug, Serialize, Deserialize)]
pub enum MockDeviceMsg {
    SetViewerOrigin(Option<RigidTransform3D<f32, Viewer, Native>>, bool),
    SetFloorOrigin(Option<RigidTransform3D<f32, Floor, Native>>),
    SetViews(MockViewsInit, Option<MockViewsInit>),
    AddInputSource(MockInputInit),
    MessageInputSource(InputId, MockInputMsg),
    VisibilityChange(Visibility),
    SetWorld(MockWorld),
    ClearWorld,
    Disconnect(ProfileGenericCallback<()>),
    SetBoundsGeometry(Vec<Point2D<f32, Floor>>),
    SimulateResetPose,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MockInputInit {
    pub source: InputSource,
    pub pointer_origin: Option<RigidTransform3D<f32, Input, Native>>,
    pub grip_origin: Option<RigidTransform3D<f32, Input, Native>>,
    pub supported_buttons: Vec<MockButton>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum MockInputMsg {
    SetHandedness(Handedness),
    SetTargetRayMode(TargetRayMode),
    SetProfiles(Vec<String>),
    SetPointerOrigin(Option<RigidTransform3D<f32, Input, Native>>, bool),
    SetGripOrigin(Option<RigidTransform3D<f32, Input, Native>>, bool),
    /// Note: SelectEvent::Select here refers to a complete Select event,
    /// not just the end event, i.e. it refers to
    /// <https://immersive-web.github.io/webxr-test-api/#dom-fakexrinputcontroller-simulateselect>
    TriggerSelect(SelectKind, SelectEvent),
    Disconnect,
    Reconnect,
    SetSupportedButtons(Vec<MockButton>),
    UpdateButtonState(MockButton),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MockRegion {
    pub faces: Vec<Triangle>,
    pub ty: EntityType,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MockWorld {
    pub regions: Vec<MockRegion>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MockButtonType {
    Grip,
    Touchpad,
    Thumbstick,
    OptionalButton,
    OptionalThumbstick,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MockButton {
    pub button_type: MockButtonType,
    pub pressed: bool,
    pub touched: bool,
    pub pressed_value: f32,
    pub x_value: f32,
    pub y_value: f32,
}

/// The primary action is always button zero. Missing reserved controls keep
/// their internal placeholders, while trailing placeholders are omitted.
fn mock_slots(buttons: &[MockButton]) -> Vec<(usize, Option<usize>, &MockButton)> {
    let mut optional_button = 4;
    let mut optional_axis = 4;
    buttons
        .iter()
        .map(|button| {
            let (index, axes) = match button.button_type {
                MockButtonType::Grip => (1, None),
                MockButtonType::Touchpad => (2, Some(0)),
                MockButtonType::Thumbstick => (3, Some(2)),
                MockButtonType::OptionalButton => {
                    let i = optional_button;
                    optional_button += 1;
                    (i, None)
                },
                MockButtonType::OptionalThumbstick => {
                    let i = optional_button;
                    optional_button += 1;
                    let a = optional_axis;
                    optional_axis += 2;
                    (i, Some(a))
                },
            };
            (index, axes, button)
        })
        .collect()
}

pub fn mock_gamepad_layout(buttons: &[MockButton]) -> crate::GamepadLayout {
    let slots = mock_slots(buttons);
    crate::GamepadLayout {
        buttons: slots.iter().map(|(i, _, _)| i + 1).max().unwrap_or(0),
        axes: slots
            .iter()
            .filter_map(|(_, axis, _)| axis.map(|a| a + 2))
            .max()
            .unwrap_or(0),
        xr_standard: true,
    }
}

pub fn mock_gamepad_state(
    buttons: &[MockButton],
    primary: bool,
) -> (Vec<f32>, Vec<bool>, Vec<bool>, Vec<f32>) {
    let layout = mock_gamepad_layout(buttons);
    let mut values = vec![0.; layout.buttons];
    let mut pressed = vec![false; layout.buttons];
    let mut touched = vec![false; layout.buttons];
    let mut axes = vec![0.; layout.axes];
    if !values.is_empty() {
        values[0] = if primary { 1. } else { 0. };
        pressed[0] = primary;
        touched[0] = primary;
    }
    for (i, axis, button) in mock_slots(buttons) {
        values[i] = button.pressed_value;
        pressed[i] = button.pressed;
        touched[i] = button.touched;
        if let Some(axis) = axis {
            if button.button_type != MockButtonType::Touchpad || button.touched {
                axes[axis] = button.x_value;
                axes[axis + 1] = button.y_value;
            }
        }
    }
    (values, pressed, touched, axes)
}
