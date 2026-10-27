/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use rustc_hash::FxHashSet;

/// Pointer IDs belong to the top-level browsing context, rather than to a
/// document. The same allocator serves pen proximity and touch contacts.
pub(crate) struct PointerIds {
    next: i32,
    active: FxHashSet<i32>,
}

impl Default for PointerIds {
    fn default() -> Self {
        Self {
            next: 1,
            active: Default::default(),
        }
    }
}

impl PointerIds {
    pub fn allocate(&mut self) -> Option<i32> {
        let start = self.next;
        loop {
            let id = self.next;
            self.next = id.checked_add(1).unwrap_or(1);
            if self.active.insert(id) {
                return Some(id);
            }
            if self.next == start {
                return None;
            }
        }
    }

    pub fn release(&mut self, id: i32) {
        self.active.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_document_and_device_leases_cannot_collide() {
        let mut ids = PointerIds::default();
        let pen = ids.allocate().unwrap();
        let finger = ids.allocate().unwrap();
        let other_frame = ids.allocate().unwrap();
        assert!(pen > 0 && finger > 0 && other_frame > 0);
        assert_ne!(pen, finger);
        assert_ne!(finger, other_frame);
        ids.release(finger);
        assert_ne!(ids.allocate(), Some(pen));
    }

    #[test]
    fn wrapping_preserves_live_ids_and_reserves_mouse_zero() {
        let mut ids = PointerIds::default();
        assert_eq!(ids.allocate(), Some(1));
        ids.next = i32::MAX;
        assert_eq!(ids.allocate(), Some(i32::MAX));
        assert_eq!(ids.allocate(), Some(2));
    }
}
