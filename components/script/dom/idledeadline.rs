/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::Cell;
use std::time::Instant;

use dom_struct::dom_struct;
use js::context::JSContext;
use script_bindings::num::Finite;
use script_bindings::reflector::{Reflector, reflect_dom_object};

use crate::dom::bindings::codegen::Bindings::IdleDeadlineBinding::IdleDeadlineMethods;
use crate::dom::bindings::root::{Dom, DomRoot};
use crate::dom::window::Window;

#[dom_struct]
pub(crate) struct IdleDeadline {
    reflector_: Reflector,
    window: Dom<Window>,
    deadline: Cell<Instant>,
    did_timeout: bool,
}

impl IdleDeadline {
    pub(crate) fn new(
        cx: &mut JSContext,
        window: &Window,
        deadline: Instant,
        did_timeout: bool,
    ) -> DomRoot<Self> {
        reflect_dom_object(
            cx,
            Box::new(Self {
                reflector_: Reflector::new(),
                window: Dom::from_ref(window),
                deadline: Cell::new(deadline),
                did_timeout,
            }),
            window,
        )
    }
}

impl IdleDeadlineMethods<crate::DomTypeHolder> for IdleDeadline {
    /// https://w3c.github.io/requestidlecallback/#dom-idledeadline-timeremaining
    fn TimeRemaining(&self) -> Finite<f64> {
        if self.did_timeout {
            return Finite::wrap(0.0);
        }
        // Timers and animation callbacks registered during an idle callback
        // shorten its budget. Canceling them must not extend an existing budget.
        let deadline = self.deadline.get().min(self.window.script_thread().idle_deadline());
        self.deadline.set(deadline);
        let milliseconds = deadline.saturating_duration_since(Instant::now()).as_secs_f64() * 1000.0;
        Finite::wrap((milliseconds * 10.0).floor() / 10.0)
    }

    fn DidTimeout(&self) -> bool {
        self.did_timeout
    }
}
