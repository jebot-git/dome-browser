// SPDX-License-Identifier: GPL-3.0-or-later
//! Native touch contacts retain their WebView owner until release/cancellation.
use std::collections::HashMap;
use servo::{InputEvent, TouchEvent, TouchEventType, TouchId, TouchPointerType, WebView, WebViewPoint};
use crate::ui;

struct Contact { view: WebView, point: WebViewPoint }
#[derive(Default)]
pub(crate) struct Touches { contacts: HashMap<i32, Contact> }
impl Touches {
    pub fn cancel_all(&mut self) {
        for (id, contact) in self.contacts.drain() {
            contact.view.notify_input_event(InputEvent::Touch(TouchEvent::new(
                TouchEventType::Cancel, TouchId(id), contact.point, TouchPointerType::Touch)));
        }
    }
    pub fn reconcile(&mut self, view: &WebView, available: bool) {
        if !available || self.contacts.values().any(|contact| contact.view.id() != view.id()) {
            self.cancel_all();
        }
    }
    pub fn handle(&mut self, event: &ui::Event, view: &WebView, available: bool) {
        let id=event.b;
        if event.a==3 {
            if let Some(contact)=self.contacts.remove(&id) {
                contact.view.notify_input_event(InputEvent::Touch(TouchEvent::new(
                    TouchEventType::Cancel,TouchId(id),contact.point,TouchPointerType::Touch)));
            }
            return;
        }
        let Ok([x,y])=serde_json::from_str::<[f64;2]>(&event.text()) else {self.cancel_all();return;};
        if !x.is_finite() || !y.is_finite() || x.abs()>f32::MAX as f64 || y.abs()>f32::MAX as f64 {
            self.cancel_all();return;
        }
        let point=servo::DevicePoint::new(x as f32,y as f32).into();
        let phase=match event.a {0=>TouchEventType::Down,1=>TouchEventType::Move,2=>TouchEventType::Up,_=>return};
        if event.a==0 {
            if !available || self.contacts.len()>=32 || self.contacts.contains_key(&id) {self.cancel_all();return;}
            self.contacts.insert(id,Contact {view:view.clone(),point});
        }
        let Some(contact)=self.contacts.get_mut(&id) else {return;};
        contact.point=point;
        let mut touch=TouchEvent::new(phase,TouchId(id),point,TouchPointerType::Touch);
        touch.is_primary=Some(event.c!=0);
        contact.view.notify_input_event(InputEvent::Touch(touch));
        if event.a==2 {self.contacts.remove(&id);}
    }
}
