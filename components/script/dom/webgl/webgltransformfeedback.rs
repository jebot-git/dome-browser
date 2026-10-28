/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::Cell;

use dom_struct::dom_struct;
use servo_canvas_traits::webgl::WebGLMsgSender;
use crate::dom::webgl::webglrenderingcontext::send_webgl_cleanup_command;
use js::context::JSContext;
use script_bindings::reflector::reflect_dom_object;
use servo_canvas_traits::webgl::{WebGLCommand, webgl_channel};

use crate::dom::bindings::reflector::DomGlobal;
use crate::dom::bindings::root::DomRoot;
use crate::dom::webgl::webglobject::WebGLObject;
use crate::dom::webgl::webglrenderingcontext::{Operation, WebGLRenderingContext};

#[derive(JSTraceable, MallocSizeOf)]
struct DroppableWebGLTransformFeedback {
    #[no_trace]
    #[ignore_malloc_size_of = "Non-owning channel for the creating native generation"]
    cleanup_sender: std::rc::Weak<WebGLMsgSender>,
    id: u32,
    marked_for_deletion: Cell<bool>,
}

impl DroppableWebGLTransformFeedback {
    fn send_with_fallibility(&self, command: WebGLCommand, fallibility: Operation) {
        send_webgl_cleanup_command(&self.cleanup_sender, command, fallibility);
    }

    fn delete(&self, operation_fallibility: Operation) {
        if self.is_valid() && self.id() != 0 {
            self.marked_for_deletion.set(true);
            self.send_with_fallibility(
                WebGLCommand::DeleteTransformFeedback(self.id()),
                operation_fallibility,
            );
        }
    }

    fn id(&self) -> u32 {
        self.id
    }

    fn is_valid(&self) -> bool {
        !self.marked_for_deletion.get()
    }
}

impl Drop for DroppableWebGLTransformFeedback {
    fn drop(&mut self) {
        self.delete(Operation::Fallible);
    }
}

#[dom_struct]
pub(crate) struct WebGLTransformFeedback {
    webgl_object: WebGLObject,
    has_been_bound: Cell<bool>,
    is_active: Cell<bool>,
    is_paused: Cell<bool>,
    droppable: DroppableWebGLTransformFeedback,
}

impl WebGLTransformFeedback {
    fn new_inherited(context: &WebGLRenderingContext, id: u32) -> Self {
        Self {
            webgl_object: WebGLObject::new_inherited(context),
            has_been_bound: Cell::new(false),
            is_active: Cell::new(false),
            is_paused: Cell::new(false),
            droppable: DroppableWebGLTransformFeedback {
                cleanup_sender: context.cleanup_sender(),
                id,
                marked_for_deletion: Cell::new(false),
            },
        }
    }

    pub(crate) fn new(cx: &mut JSContext, context: &WebGLRenderingContext) -> DomRoot<Self> {
        if context.is_context_lost() {
            return reflect_dom_object(cx, Box::new(Self::new_inherited(context, 1)), &*context.global());
        }
        let (sender, receiver) = webgl_channel().unwrap();
        context.send_command(WebGLCommand::CreateTransformFeedback(sender));
        let id = context.receive_reply(&receiver).unwrap_or_else(|| 1);

        reflect_dom_object(
            cx,
            Box::new(WebGLTransformFeedback::new_inherited(context, id)),
            &*context.global(),
        )
    }
}

impl WebGLTransformFeedback {
    pub(crate) fn bind(&self, context: &WebGLRenderingContext, target: u32) {
        context.send_command(WebGLCommand::BindTransformFeedback(target, self.id()));
        self.has_been_bound.set(true);
    }

    pub(crate) fn begin(&self, context: &WebGLRenderingContext, primitive_mode: u32) {
        if self.has_been_bound.get() && !self.is_active() {
            context.send_command(WebGLCommand::BeginTransformFeedback(primitive_mode));
            self.set_active(true);
        }
    }

    pub(crate) fn end(&self, context: &WebGLRenderingContext) {
        if self.has_been_bound.get() && self.is_active() {
            if self.is_paused() {
                context.send_command(WebGLCommand::ResumeTransformFeedback());
            }
            context.send_command(WebGLCommand::EndTransformFeedback());
            self.set_active(false);
        }
    }

    pub(crate) fn resume(&self, context: &WebGLRenderingContext) {
        if self.is_active() && self.is_paused() {
            context.send_command(WebGLCommand::ResumeTransformFeedback());
            self.set_pause(false);
        }
    }

    pub(crate) fn pause(&self, context: &WebGLRenderingContext) {
        if self.is_active() && !self.is_paused() {
            context.send_command(WebGLCommand::PauseTransformFeedback());
            self.set_pause(true);
        }
    }

    pub(crate) fn id(&self) -> u32 {
        self.droppable.id()
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.droppable.is_valid()
    }

    pub(crate) fn is_active(&self) -> bool {
        self.is_active.get()
    }

    pub(crate) fn is_paused(&self) -> bool {
        self.is_paused.get()
    }

    pub(crate) fn delete(&self, operation_fallibility: Operation) {
        self.droppable.delete(operation_fallibility);
    }

    pub(crate) fn set_active(&self, value: bool) {
        if self.is_valid() && self.has_been_bound.get() {
            self.is_active.set(value);
        }
    }

    pub(crate) fn set_pause(&self, value: bool) {
        if self.is_valid() && self.is_active() {
            self.is_active.set(value);
        }
    }
}
