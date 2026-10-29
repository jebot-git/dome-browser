/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::Cell;

use dom_struct::dom_struct;
use js::context::JSContext;
use script_bindings::reflector::{Reflector, reflect_dom_object};
use servo_base::generic_channel::GenericSender;
use webxr_api::{
    Handedness, InputId, MockButton, MockButtonType, MockDeviceMsg, MockInputMsg, SelectEvent,
    SelectKind, TargetRayMode,
};

use crate::conversions::Convert;
use crate::dom::bindings::codegen::Bindings::FakeXRDeviceBinding::FakeXRRigidTransformInit;
use crate::dom::bindings::codegen::Bindings::FakeXRInputControllerBinding::{
    FakeXRButtonStateInit, FakeXRButtonType, FakeXRInputControllerMethods,
};
use crate::dom::bindings::codegen::Bindings::XRInputSourceBinding::{
    XRHandedness, XRTargetRayMode,
};
use crate::dom::bindings::error::{Error, Fallible};
use crate::dom::bindings::root::DomRoot;
use crate::dom::bindings::str::DOMString;
use crate::dom::fakexrdevice::get_origin;
use crate::dom::globalscope::GlobalScope;

#[dom_struct]
pub(crate) struct FakeXRInputController {
    reflector: Reflector,
    #[no_trace]
    sender: GenericSender<MockDeviceMsg>,
    #[no_trace]
    id: InputId,
    supported_buttons: Cell<u8>,
}

impl FakeXRInputController {
    pub(crate) fn new_inherited(
        sender: GenericSender<MockDeviceMsg>,
        id: InputId,
        supported_buttons: &[MockButton],
    ) -> FakeXRInputController {
        FakeXRInputController {
            reflector: Reflector::new(),
            sender,
            id,
            supported_buttons: Cell::new(supported_buttons.iter().fold(0, |mask, button| {
                mask | button_type_mask(button.button_type.clone())
            })),
        }
    }

    pub(crate) fn new(
        cx: &mut JSContext,
        global: &GlobalScope,
        sender: GenericSender<MockDeviceMsg>,
        id: InputId,
        supported_buttons: &[MockButton],
    ) -> DomRoot<FakeXRInputController> {
        reflect_dom_object(
            cx,
            Box::new(FakeXRInputController::new_inherited(sender, id, supported_buttons)),
            global,
        )
    }

    fn send_message(&self, msg: MockInputMsg) {
        let _ = self
            .sender
            .send(MockDeviceMsg::MessageInputSource(self.id, msg));
    }
}

impl FakeXRInputControllerMethods<crate::DomTypeHolder> for FakeXRInputController {
    fn SetPrimaryAction(&self, primary: bool) {
        self.send_message(MockInputMsg::SetPrimaryAction(primary));
    }
    /// <https://immersive-web.github.io/webxr-test-api/#dom-fakexrinputcontroller-setpointerorigin>
    fn SetPointerOrigin(&self, origin: &FakeXRRigidTransformInit, emulated: bool) -> Fallible<()> {
        self.send_message(MockInputMsg::SetPointerOrigin(Some(get_origin(origin)?), emulated));
        Ok(())
    }

    /// <https://immersive-web.github.io/webxr-test-api/#dom-fakexrinputcontroller-setgriporigin>
    fn SetGripOrigin(&self, origin: &FakeXRRigidTransformInit, emulated: bool) -> Fallible<()> {
        self.send_message(MockInputMsg::SetGripOrigin(Some(get_origin(origin)?), emulated));
        Ok(())
    }

    /// <https://immersive-web.github.io/webxr-test-api/#dom-fakexrinputcontroller-cleargriporigin>
    fn ClearGripOrigin(&self) {
        self.send_message(MockInputMsg::SetGripOrigin(None, false))
    }

    /// <https://immersive-web.github.io/webxr-test-api/#dom-fakexrinputcontroller-disconnect>
    fn Disconnect(&self) {
        self.send_message(MockInputMsg::Disconnect)
    }

    /// <https://immersive-web.github.io/webxr-test-api/#dom-fakexrinputcontroller-reconnect>
    fn Reconnect(&self) {
        self.send_message(MockInputMsg::Reconnect)
    }

    /// <https://immersive-web.github.io/webxr-test-api/#dom-fakexrinputcontroller-startselection>
    fn StartSelection(&self) {
        self.send_message(MockInputMsg::TriggerSelect(
            SelectKind::Select,
            SelectEvent::Start,
        ))
    }

    /// <https://immersive-web.github.io/webxr-test-api/#dom-fakexrinputcontroller-endselection>
    fn EndSelection(&self) {
        self.send_message(MockInputMsg::TriggerSelect(
            SelectKind::Select,
            SelectEvent::End,
        ))
    }

    /// <https://immersive-web.github.io/webxr-test-api/#dom-fakexrinputcontroller-simulateselect>
    fn SimulateSelect(&self) {
        self.send_message(MockInputMsg::TriggerSelect(
            SelectKind::Select,
            SelectEvent::Select,
        ))
    }

    /// <https://immersive-web.github.io/webxr-test-api/#dom-fakexrinputcontroller-sethandedness>
    fn SetHandedness(&self, handedness: XRHandedness) {
        let h = match handedness {
            XRHandedness::None => Handedness::None,
            XRHandedness::Left => Handedness::Left,
            XRHandedness::Right => Handedness::Right,
        };
        self.send_message(MockInputMsg::SetHandedness(h));
    }

    /// <https://immersive-web.github.io/webxr-test-api/#dom-fakexrinputcontroller-settargetraymode>
    fn SetTargetRayMode(&self, target_ray_mode: XRTargetRayMode) {
        let t = match target_ray_mode {
            XRTargetRayMode::Gaze => TargetRayMode::Gaze,
            XRTargetRayMode::Tracked_pointer => TargetRayMode::TrackedPointer,
            XRTargetRayMode::Screen => TargetRayMode::Screen,
            XRTargetRayMode::Transient_pointer => TargetRayMode::TransientPointer,
        };
        self.send_message(MockInputMsg::SetTargetRayMode(t));
    }

    /// <https://immersive-web.github.io/webxr-test-api/#dom-fakexrinputcontroller-setprofiles>
    fn SetProfiles(&self, profiles: Vec<DOMString>) {
        let t = profiles.into_iter().map(String::from).collect();
        self.send_message(MockInputMsg::SetProfiles(t));
    }

    /// <https://immersive-web.github.io/webxr-test-api/#dom-fakexrinputcontroller-setsupportedbuttons>
    fn SetSupportedButtons(&self, supported_buttons: Vec<FakeXRButtonStateInit>) {
        let supported = init_to_mock_buttons(&supported_buttons);
        self.supported_buttons.set(supported.iter().fold(0, |mask, button| {
            mask | button_type_mask(button.button_type.clone())
        }));
        self.send_message(MockInputMsg::SetSupportedButtons(supported));
    }

    /// <https://immersive-web.github.io/webxr-test-api/#dom-fakexrinputcontroller-updatebuttonstate>
    fn UpdateButtonState(&self, button_state: &FakeXRButtonStateInit) -> Fallible<()> {
        // https://immersive-web.github.io/webxr-test-api/#validate-a-button-state
        if (button_state.pressed || *button_state.pressedValue > 0.0) && !button_state.touched {
            return Err(Error::Type(c"Pressed button must also be touched".into()));
        }
        if *button_state.pressedValue < 0.0 {
            return Err(Error::Type(c"Pressed value must be non-negative".into()));
        }

        if self.supported_buttons.get() & button_type_mask(button_state.buttonType.convert()) == 0 {
            return Err(Error::NotFound(None));
        }

        let state = init_to_mock_buttons(std::slice::from_ref(button_state));
        if let Some(state) = state.into_iter().next() {
            self.send_message(MockInputMsg::UpdateButtonState(state));
        }

        Ok(())
    }
}

impl Convert<MockButtonType> for FakeXRButtonType {
    fn convert(self) -> MockButtonType {
        match self {
            FakeXRButtonType::Grip => MockButtonType::Grip,
            FakeXRButtonType::Touchpad => MockButtonType::Touchpad,
            FakeXRButtonType::Thumbstick => MockButtonType::Thumbstick,
            FakeXRButtonType::Optional_button => MockButtonType::OptionalButton,
            FakeXRButtonType::Optional_thumbstick => MockButtonType::OptionalThumbstick,
        }
    }
}

/// <https://immersive-web.github.io/webxr-test-api/#parse-supported-buttons>
pub(crate) fn init_to_mock_buttons(buttons: &[FakeXRButtonStateInit]) -> Vec<MockButton> {
    let mut seen = 0;
    let supported: Vec<MockButton> = buttons
        .iter()
        .filter(|button| {
            let mask = button_type_mask(button.buttonType.convert());
            let first = seen & mask == 0;
            seen |= mask;
            first
        })
        .map(|b| MockButton {
            button_type: b.buttonType.convert(),
            pressed: b.pressed,
            touched: b.touched,
            pressed_value: *b.pressedValue,
            x_value: *b.xValue,
            y_value: *b.yValue,
        })
        .collect();
    supported
}

fn button_type_mask(kind: MockButtonType) -> u8 {
    match kind {
        MockButtonType::Grip => 1,
        MockButtonType::Touchpad => 2,
        MockButtonType::Thumbstick => 4,
        MockButtonType::OptionalButton => 8,
        MockButtonType::OptionalThumbstick => 16,
    }
}
