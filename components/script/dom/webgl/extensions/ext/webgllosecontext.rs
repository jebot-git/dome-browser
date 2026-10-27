/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use super::{WebGLExtension, WebGLExtensionSpec, WebGLExtensions};
use crate::dom::bindings::codegen::Bindings::WEBGLLoseContextBinding::WEBGLLoseContextMethods;
use crate::dom::bindings::reflector::DomGlobal;
use crate::dom::bindings::root::{Dom, DomRoot};
use crate::dom::webgl::webglrenderingcontext::WebGLRenderingContext;
use dom_struct::dom_struct;
use js::context::JSContext;
use script_bindings::reflector::{Reflector, reflect_dom_object};

#[dom_struct]
pub(crate) struct WEBGLLoseContext {
    reflector_: Reflector,
    context: Dom<WebGLRenderingContext>,
}

impl WEBGLLoseContextMethods<crate::DomTypeHolder> for WEBGLLoseContext {
    fn LoseContext(&self, cx: &mut JSContext) {
        self.context.lose_context(cx);
    }
    fn RestoreContext(&self) {
        self.context.restore_context();
    }
}

impl WebGLExtension for WEBGLLoseContext {
    type Extension = Self;
    fn new(cx: &mut JSContext, context: &WebGLRenderingContext) -> DomRoot<Self> {
        reflect_dom_object(
            cx,
            Box::new(Self {
                reflector_: Reflector::new(),
                context: Dom::from_ref(context),
            }),
            &*context.global(),
        )
    }
    fn spec() -> WebGLExtensionSpec {
        WebGLExtensionSpec::All
    }
    fn is_supported(_: &WebGLExtensions) -> bool {
        true
    }
    fn enable(_: &WebGLExtensions) {}
    fn name() -> &'static str {
        "WEBGL_lose_context"
    }
}
