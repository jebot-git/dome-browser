// SPDX-License-Identifier: MPL-2.0
//! Preserve a session's !Send device when its controller is forcibly dropped.
use crate::session::SessionActivity;
use crate::{DeviceAPI, deferred_cleanup};
use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
};

pub(crate) struct SessionDevice<D: DeviceAPI> {
    device: Option<D>,
    active: Arc<SessionActivity>,
    closing: bool,
    complete: bool,
}
impl<D: DeviceAPI> SessionDevice<D> {
    pub fn new(device: D, active: Arc<SessionActivity>) -> Self {
        Self {
            device: Some(device),
            active,
            closing: false,
            complete: false,
        }
    }
    pub fn quit(&mut self) {
        if self.closing {
            return;
        }
        self.closing = true;
        self.active.begin_closing();
        self.device.as_mut().unwrap().quit();
    }
    pub fn poll_cleanup(&mut self) -> bool {
        if self.complete {
            return true;
        }
        if self.device.is_none() {
            return self.active.is_closed();
        }
        self.quit();
        if !self.device.as_mut().unwrap().poll_cleanup() {
            return false;
        }
        self.complete = true;
        self.active.finish_closing();
        true
    }
    pub fn retire_or_defer(&mut self) {
        if self.device.is_none() || self.poll_cleanup() {
            return;
        }
        self.defer_pending_cleanup();
    }
    pub fn defer_pending_cleanup(&mut self) {
        let Some(mut device) = self.device.take() else {
            return;
        };
        debug_assert!(self.closing);

        let active = self.active.clone();
        deferred_cleanup::defer(move || {
            if !device.poll_cleanup() {
                return false;
            }
            active.finish_closing();
            true
        });
    }
}
impl<D: DeviceAPI> Deref for SessionDevice<D> {
    type Target = D;
    fn deref(&self) -> &D {
        self.device.as_ref().expect("retired XR session device")
    }
}
impl<D: DeviceAPI> DerefMut for SessionDevice<D> {
    fn deref_mut(&mut self) -> &mut D {
        self.device.as_mut().expect("retired XR session device")
    }
}
impl<D: DeviceAPI> Drop for SessionDevice<D> {
    fn drop(&mut self) {
        self.retire_or_defer();
    }
}
