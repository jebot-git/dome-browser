// SPDX-License-Identifier: MPL-2.0
//! Keep native context ownership together through initialization and retirement.
use std::cell::RefCell;

use surfman::chains::SwapChain;
use surfman::{Context, Device, Error};

pub(super) struct NativeContextState {
    pub device: RefCell<Device>,
    pub context: RefCell<Context>,
    chains: RefCell<Vec<SwapChain<Device>>>,
    native_only: bool,
}

impl NativeContextState {
    pub(super) fn retire_with(
        &mut self,
        destroy: impl FnOnce(&Device, &mut Context) -> Result<(), Error>,
    ) -> bool {
        let device = self.device.get_mut();
        let context = self.context.get_mut();
        if !self.native_only {
            let chains = self.chains.get_mut();
            if !chains.is_empty() {
                if !device.context_is_lost(context) && device.make_context_current(context).is_err()
                {
                    return false;
                }
                chains.retain(|chain| chain.destroy(device, context).is_err());
                if !chains.is_empty() {
                    return false;
                }
            }
            // Do not rebind or repeat GL retirement once native destruction starts.
            self.native_only = true;
        }
        destroy(device, context).is_ok()
    }

    fn retire(&mut self) -> bool {
        self.retire_with(|device, context| device.destroy_context(context))
    }
}

pub(super) struct NativeContextOwner {
    state: Option<NativeContextState>,
}
impl NativeContextOwner {
    pub fn new(device: Device, context: Context) -> Self {
        Self {
            state: Some(NativeContextState {
                device: RefCell::new(device),
                context: RefCell::new(context),
                chains: RefCell::new(Vec::new()),
                native_only: false,
            }),
        }
    }
    pub fn state(&self) -> &NativeContextState {
        self.state.as_ref().expect("live native context owner")
    }
    pub fn track_chain(&self, chain: &SwapChain<Device>) {
        self.state().chains.borrow_mut().push(chain.clone());
    }
    #[cfg(test)]
    pub fn state_mut(&mut self) -> &mut NativeContextState {
        self.state.as_mut().expect("live native context owner")
    }
}
impl Drop for NativeContextOwner {
    fn drop(&mut self) {
        let Some(mut state) = self.state.take() else {
            return;
        };
        if !state.retire() {
            servo_base::deferred_cleanup::defer(move || state.retire());
        }
    }
}
