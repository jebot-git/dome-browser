/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum RetirementPhase {
    #[default]
    Resources,
    Native,
    Complete,
}
impl RetirementPhase {
    pub(crate) fn needs_resources(self) -> bool {
        self == Self::Resources
    }
}
pub(crate) trait RetireOwner {
    type Error;
    fn release_resources(&mut self) -> Result<(), Self::Error>;
    fn destroy_native(&mut self) -> Result<(), Self::Error>;
}
/// Retain the phase on failure: native destruction can partially succeed and
/// must be retried without selecting or recreating its former GL resources.
pub(crate) fn retire_in_order<T: RetireOwner>(
    phase: &mut RetirementPhase,
    owner: &mut T,
) -> Result<(), T::Error> {
    if *phase == RetirementPhase::Resources {
        owner.release_resources()?;
        *phase = RetirementPhase::Native;
    }
    if *phase == RetirementPhase::Native {
        owner.destroy_native()?;
        *phase = RetirementPhase::Complete;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Owner {
        fail_resource: bool,
        fail_native: bool,
        resources_gone: bool,
        native_gone: bool,
        calls: Vec<&'static str>,
    }
    impl RetireOwner for Owner {
        type Error = &'static str;
        fn release_resources(&mut self) -> Result<(), Self::Error> {
            assert!(
                !self.native_gone,
                "resources accessed after partial native destruction"
            );
            self.calls.push("resources");
            if std::mem::take(&mut self.fail_resource) {
                return Err("resources unavailable");
            }
            self.resources_gone = true;
            Ok(())
        }
        fn destroy_native(&mut self) -> Result<(), Self::Error> {
            assert!(self.resources_gone);
            self.calls.push("native");
            self.native_gone = true;
            if std::mem::take(&mut self.fail_native) {
                return Err("native cleanup incomplete");
            }
            Ok(())
        }
    }
    #[test]
    fn resource_failure_keeps_phase_and_does_not_destroy_native_owner() {
        let mut phase = RetirementPhase::default();
        let mut owner = Owner {
            fail_resource: true,
            ..Owner::default()
        };
        assert!(retire_in_order(&mut phase, &mut owner).is_err());
        assert!(phase.needs_resources());
        assert!(!owner.native_gone);
        retire_in_order(&mut phase, &mut owner).unwrap();
        assert_eq!(owner.calls, ["resources", "resources", "native"]);
        assert_eq!(phase, RetirementPhase::Complete);
    }
    #[test]
    fn partial_native_destruction_never_rebinds_or_releases_resources_again() {
        let mut phase = RetirementPhase::default();
        let mut owner = Owner {
            fail_native: true,
            ..Owner::default()
        };
        assert!(retire_in_order(&mut phase, &mut owner).is_err());
        assert_eq!(phase, RetirementPhase::Native);
        assert!(owner.native_gone);
        retire_in_order(&mut phase, &mut owner).unwrap();
        assert_eq!(owner.calls, ["resources", "native", "native"]);
        assert_eq!(phase, RetirementPhase::Complete);
    }
    #[test]
    fn completed_retirement_is_idempotent() {
        let mut phase = RetirementPhase::default();
        let mut owner = Owner::default();
        retire_in_order(&mut phase, &mut owner).unwrap();
        retire_in_order(&mut phase, &mut owner).unwrap();
        assert_eq!(owner.calls, ["resources", "native"]);
    }
    #[test]
    fn native_loss_classification_covers_resource_and_cleanup_errors() {
        use surfman::{Error, WindowingApiError};
        let errors: [fn(WindowingApiError) -> Error; 9] = [
            Error::PixelFormatSelectionFailed,
            Error::ContextCreationFailed,
            Error::ContextDestructionFailed,
            Error::MakeCurrentFailed,
            Error::SurfaceCreationFailed,
            Error::SurfaceDestructionFailed,
            Error::SurfaceImportFailed,
            Error::SurfaceTextureCreationFailed,
            Error::PresentFailed,
        ];
        for error in errors {
            assert!(error(WindowingApiError::ContextLost).is_context_lost());
            assert!(!error(WindowingApiError::BadAlloc).is_context_lost());
        }
        assert!(!Error::Failed.is_context_lost());
        assert!(!Error::NoCurrentContext.is_context_lost());
    }
}
