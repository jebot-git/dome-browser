// SPDX-License-Identifier: GPL-3.0-or-later
use std::rc::Rc;

/// Install only missing scheduler helpers on Discord's own HTTPS documents.
/// This is a timer fallback, not an implementation of native idle scheduling.
pub fn manager(servo: &servo::Servo) -> Rc<servo::UserContentManager> {
    let manager = Rc::new(servo::UserContentManager::new(servo));
    manager.add_script(Rc::new(servo::UserScript::new(
        include_str!("discord-compatibility.js").into(), None,
    )));
    manager
}
