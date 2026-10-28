/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// A disconnected native reply is context loss. Each caller supplies the WebGL
// operation's lost-context return value; never manufacture a resource handle.
macro_rules! webgl_reply {
    ($context:expr, $receiver:expr, $fallback:expr) => {
        match $context.receive_reply(&$receiver) {
            Some(value) => value,
            None => return $fallback,
        }
    };
}

macro_rules! webgl_uniform {
    ($triple:expr, $command:expr, $retval:expr) => {
        match $crate::dom::webgl::webglrenderingcontext::uniform_get($triple, $command) {
            Some(value) => value,
            None => return $retval.set(js::jsval::NullValue()),
        }
    };
}

pub(crate) mod extensions;
pub(crate) mod validations;
pub(crate) mod vertexarrayobject;
pub(crate) mod webgl2renderingcontext;
pub(crate) mod webglactiveinfo;
pub(crate) mod webglbuffer;
pub(crate) mod webglcontextevent;
pub(crate) mod webglframebuffer;
pub(crate) mod webglobject;
pub(crate) mod webglprogram;
pub(crate) mod webglquery;
pub(crate) mod webglrenderbuffer;
pub(crate) mod webglrenderingcontext;
pub(crate) mod webglsampler;
pub(crate) mod webglshader;
pub(crate) mod webglshaderprecisionformat;
pub(crate) mod webglsync;
pub(crate) mod webgltexture;
pub(crate) mod webgltransformfeedback;
pub(crate) mod webgluniformlocation;
pub(crate) mod webglvertexarrayobject;
pub(crate) mod webglvertexarrayobjectoes;
