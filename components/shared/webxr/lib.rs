/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! This crate defines the Rust API for WebXR. It is implemented by the `webxr` crate.

mod device;
mod error;
mod events;
mod frame;
mod geometry;
mod hand;
mod hittest;
mod input;
mod layer;
mod mock;
mod registry;
mod session;
mod space;
pub mod util;
mod view;

pub use device::{DeviceAPI, DiscoveryAPI};
pub use error::Error;
pub use events::{Event, EventBuffer, Visibility};
pub use frame::{Frame, FrameUpdateEvent, ViewerPose};
pub use geometry::{LayerGeometry, MAX_FRAMEBUFFER_DIMENSION, ViewportScale, scaled_viewport};
pub use hand::{Finger, FingerJoint, Hand, HandSpace, Joint, JointFrame};
pub use hittest::{
    EntityType, EntityTypes, HitTestId, HitTestResult, HitTestSource, HitTestSpace, Ray, Triangle,
};
pub use input::{
    GamepadLayout, Handedness, InputFrame, InputId, InputSource, SelectEvent, SelectKind,
    TargetRayMode,
};
pub use layer::{
    ContextId, GLContexts, GLTypes, LayerAttributes, LayerGrandManager, LayerGrandManagerAPI,
    LayerId, LayerInfo, LayerInit, LayerLayout, LayerManager, LayerManagerAPI, LayerManagerFactory,
    SubImage, SubImages,
};
pub use mock::{
    MockButton, MockButtonType, MockDeviceInit, MockDeviceMsg, MockDiscoveryAPI, MockInputInit,
    MockInputMsg, MockRegion, MockViewInit, MockViewsInit, MockWorld, mock_gamepad_layout,
    mock_gamepad_state,
};
pub use registry::{MainThreadRegistry, Registry};
pub use session::{
    EnvironmentBlendMode, MainThreadSession, Quitter, Session, SessionBuilder, SessionId,
    SessionInit, SessionMode, SessionThread,
};
pub use space::{ApiSpace, BaseSpace, Space};
pub use view::{
    CUBE_BACK, CUBE_BOTTOM, CUBE_LEFT, CUBE_RIGHT, CUBE_TOP, Capture, CubeBack, CubeBottom,
    CubeLeft, CubeRight, CubeTop, Display, Floor, Input, LEFT_EYE, LeftEye, Native, RIGHT_EYE,
    RightEye, SomeEye, VIEWER, View, ViewDescription, ViewEye, Viewer, Viewport, Viewports, Views,
};
