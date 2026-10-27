// SPDX-License-Identifier: GPL-3.0-or-later
use std::rc::Rc;

/// Install supplementary page compatibility helpers and optional diagnostics.
/// Idle scheduling is provided by the native engine on every origin.
pub fn manager(servo: &servo::Servo) -> Rc<servo::UserContentManager> {
    let manager = Rc::new(servo::UserContentManager::new(servo));
    if matches!(std::env::var("DOME_BROWSER_ERROR_TRACE").as_deref(), Ok("1")) {
        manager.add_script(Rc::new(servo::UserScript::new(
            include_str!("error-diagnostics.js").into(), None,
        )));
    }
    manager.add_script(Rc::new(servo::UserScript::new(include_str!("compatibility.js").into(), None)));
    manager
}
