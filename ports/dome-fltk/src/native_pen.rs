// SPDX-License-Identifier: GPL-3.0-or-later
//! Pen range ownership is independent of finger contacts and mouse buttons.
use std::collections::{HashMap,HashSet};
use servo::{InputEvent,PenEvent,PenEventType,WebView,WheelEvent,WheelDelta,WheelMode};
use crate::ui;
struct Owner {view:WebView,last:PenEvent}
#[derive(Default)]
pub(crate) struct Pens {owners:HashMap<i32,Owner>,blocked:HashSet<i32>}
#[derive(serde::Deserialize)]
struct Sample {x:f64,y:f64,inside:bool,pressure:f32,tangential_pressure:f32,tilt_x:f64,tilt_y:f64,twist:f64,
    #[serde(default)] wheel:f64}
impl Pens {
    fn cancel(&mut self,id:i32,block:bool) {
        if let Some(owner)=self.owners.remove(&id) {
            if block && owner.last.buttons!=0 {self.blocked.insert(id);}
            let mut event=owner.last;event.event_type=PenEventType::Cancel;
            owner.view.notify_input_event(InputEvent::Pen(event.normalized()));
        }
    }
    pub fn cancel_all(&mut self) {let ids:Vec<_>=self.owners.keys().copied().collect();for id in ids {self.cancel(id,true);}}
    pub fn cancel_contacts(&mut self) {let ids:Vec<_>=self.owners.iter().filter(|(_,owner)|owner.last.buttons!=0).map(|(id,_)|*id).collect();for id in ids {self.cancel(id,true);}}
    pub fn reconcile(&mut self,view:&WebView,visible:bool) {
        if !visible || self.owners.values().any(|owner|owner.view.id()!=view.id()) {self.cancel_all();}
    }
    pub fn handle(&mut self,event:&ui::Event,view:&WebView,visible:bool) {
        let id=event.b;
        if event.a==5 {return;}
        if event.a==3 {self.cancel(id,true);return;}
        if event.a==6 {
            if let Some(owner)=self.owners.remove(&id) {
                let mut sample=owner.last;sample.event_type=PenEventType::Leave;
                owner.view.notify_input_event(InputEvent::Pen(sample.normalized()));
            }
            self.blocked.remove(&id);return;
        }
        let buttons=event.d as u16;
        if self.blocked.contains(&id) {
            if buttons==0 {self.blocked.remove(&id);} else {return;}
            if !matches!(event.a,4|5) {return;}
        }
        let Ok(sample)=serde_json::from_str::<Sample>(&event.text()) else {self.cancel(id,true);return;};
        if !sample.x.is_finite() || !sample.y.is_finite() || sample.x.abs()>f32::MAX as f64 || sample.y.abs()>f32::MAX as f64 {
            self.cancel(id,true);return;
        }
        if !visible {self.cancel(id,true);return;}
        if !sample.inside && self.owners.get(&id).is_none_or(|owner|owner.last.buttons==0) {
            if let Some(owner)=self.owners.remove(&id) {
                let mut leave=owner.last;leave.event_type=PenEventType::Leave;
                owner.view.notify_input_event(InputEvent::Pen(leave.normalized()));
            }
            return;
        }
        if !self.owners.contains_key(&id) {
            if !matches!(event.a,0|4|5) || !sample.inside || self.owners.len()>=64 {return;}
            if event.a!=0 && buttons!=0 {self.blocked.insert(id);return;}
        }
        let point=servo::DevicePoint::new(sample.x as f32,sample.y as f32).into();
        let mut pen=PenEvent::new(PenEventType::Sample,id,point);
        pen.primary=event.c!=0;pen.buttons=buttons;pen.pressure=sample.pressure;
        pen.tangential_pressure=sample.tangential_pressure;pen.tilt_x=sample.tilt_x;pen.tilt_y=sample.tilt_y;pen.twist=sample.twist;
        let owner=self.owners.entry(id).or_insert_with(||Owner {view:view.clone(),last:pen});
        owner.last=pen;owner.view.notify_input_event(InputEvent::Pen(pen.normalized()));
        if sample.inside && matches!(event.a,1|4) && sample.wheel.is_finite() && sample.wheel!=0. {
            // Use continuous rotation consistently: discrete clicks describe the
            // same motion and may arrive after earlier fractional rotation.
            // Positive Wayland rotation scrolls down, opposite to Servo's
            // embedder convention. A conventional 15-degree step is one line.
            let y=sample.wheel/15.;
            owner.view.notify_input_event(InputEvent::Wheel(WheelEvent {point,delta:WheelDelta {x:0.,y:-y,z:0.,mode:WheelMode::DeltaLine}}));
        }
    }
}
