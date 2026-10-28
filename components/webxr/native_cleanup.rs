// SPDX-License-Identifier: MPL-2.0
/// Native teardown has a one-way transition: once context destruction starts,
/// retries cannot select that context or issue ordinary GL commands again.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum CleanupPhase {
    #[default]
    Resources,
    Native,
    Complete,
}
pub trait CleanupOwner {
    type Error;
    fn release_resources(&mut self) -> Result<(), Self::Error>;
    fn destroy_native(&mut self) -> Result<(), Self::Error>;
}
pub fn advance(phase: &mut CleanupPhase, owner: &mut impl CleanupOwner) -> bool {
    if *phase == CleanupPhase::Resources {
        if owner.release_resources().is_err() {
            return false;
        }
        *phase = CleanupPhase::Native;
    }
    if *phase == CleanupPhase::Native {
        if owner.destroy_native().is_err() {
            return false;
        }
        *phase = CleanupPhase::Complete;
    }
    true
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Owner {
        events: Vec<&'static str>,
        resources_fail: bool,
        native_fail: bool,
    }
    impl CleanupOwner for Owner {
        type Error = ();
        fn release_resources(&mut self) -> Result<(), ()> {
            self.events.push("resources");
            if std::mem::take(&mut self.resources_fail) {
                Err(())
            } else {
                Ok(())
            }
        }
        fn destroy_native(&mut self) -> Result<(), ()> {
            self.events.push("native");
            if std::mem::take(&mut self.native_fail) {
                Err(())
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn resources_retry_before_native_and_native_retry_never_rebinds() {
        let mut owner = Owner {
            events: vec![],
            resources_fail: true,
            native_fail: true,
        };
        let mut phase = CleanupPhase::default();
        assert!(!advance(&mut phase, &mut owner));
        assert_eq!(phase, CleanupPhase::Resources);
        assert!(!advance(&mut phase, &mut owner));
        assert_eq!(phase, CleanupPhase::Native);
        assert!(advance(&mut phase, &mut owner));
        assert!(advance(&mut phase, &mut owner));
        assert_eq!(owner.events, ["resources", "resources", "native", "native"]);
    }
}
