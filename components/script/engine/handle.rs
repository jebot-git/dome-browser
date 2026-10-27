/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use js::rust::{JSEngine, JSEngineHandle};

use crate::dom::sharedworker::SharedWorker;

static JS_ENGINE: Mutex<Option<JSEngineHandle>> = Mutex::new(None);

pub(crate) fn current_js_engine_handle() -> JSEngineHandle {
    JS_ENGINE.lock().unwrap().as_ref().unwrap().clone()
}

pub struct JSEngineSetup(JSEngine);

impl Default for JSEngineSetup {
    fn default() -> Self {
        let engine = JSEngine::init().unwrap();
        *JS_ENGINE.lock().unwrap() = Some(engine.handle());
        Self(engine)
    }
}

impl Drop for JSEngineSetup {
    fn drop(&mut self) {
        // Window script threads have finished, but process-wide shared workers
        // can still own JS runtimes. Terminate those before waiting for the
        // engine's outstanding contexts to disappear.
        SharedWorker::shutdown_all();
        *JS_ENGINE.lock().unwrap() = None;

        while !self.0.can_shutdown() {
            thread::sleep(Duration::from_millis(50));
        }
    }
}
