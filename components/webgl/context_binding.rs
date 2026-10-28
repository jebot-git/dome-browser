/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

/// Cache only a binding that actually succeeded. A failed native operation may
/// invalidate our knowledge of the current context, so retry must bind again.
pub(crate) fn bind_context<T: Copy + Eq, E>(
    bound: &mut Option<T>,
    requested: T,
    validate: impl FnOnce() -> Result<(), E>,
    bind: impl FnOnce() -> Result<(), E>,
) -> Result<(), E> {
    // A locally reported loss must invalidate even an unchanged cached binding.
    if let Err(error) = validate() {
        *bound = None;
        return Err(error);
    }
    if *bound == Some(requested) {
        return Ok(());
    }
    *bound = None;
    bind()?;
    *bound = Some(requested);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::bind_context;
    #[test]
    fn cached_binding_does_not_call_the_driver_again() {
        let mut bound = Some(1);
        bind_context::<_, ()>(&mut bound, 1, || Ok(()), || panic!("unexpected bind")).unwrap();
        assert_eq!(bound, Some(1));
    }
    #[test]
    fn successful_switch_commits_requested_context() {
        let mut bound = Some(1);
        let mut actual = 1;
        bind_context::<_, ()>(
            &mut bound,
            2,
            || Ok(()),
            || {
                actual = 2;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(actual, 2);
        assert_eq!(bound, Some(2));
    }
    #[test]
    fn failed_switch_invalidates_cached_binding_and_retries() {
        let mut bound = Some(1);
        let mut attempts = 0;
        assert_eq!(
            bind_context(
                &mut bound,
                2,
                || Ok(()),
                || {
                    attempts += 1;
                    Err("unavailable")
                }
            ),
            Err("unavailable")
        );
        assert_eq!(bound, None);
        bind_context::<_, ()>(
            &mut bound,
            2,
            || Ok(()),
            || {
                attempts += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(attempts, 2);
        assert_eq!(bound, Some(2));
    }
    #[test]
    fn previous_context_is_rebound_after_failed_switch() {
        let mut bound = Some(1);
        let mut actual = 1;
        assert!(
            bind_context(
                &mut bound,
                2,
                || Ok(()),
                || {
                    actual = 2;
                    Err(())
                }
            )
            .is_err()
        );
        assert_eq!(bound, None);
        let mut rebound = false;
        bind_context::<_, ()>(
            &mut bound,
            1,
            || Ok(()),
            || {
                rebound = true;
                actual = 1;
                Ok(())
            },
        )
        .unwrap();
        assert!(rebound);
        assert_eq!(actual, 1);
        assert_eq!(bound, Some(1));
    }
    #[test]
    fn cached_native_loss_is_reported_without_calling_the_driver() {
        let mut bound = Some(1);
        assert_eq!(
            bind_context(
                &mut bound,
                1,
                || Err("lost"),
                || panic!("bind after known loss")
            ),
            Err("lost")
        );
        assert_eq!(bound, None);
    }
    #[test]
    fn every_cached_use_rechecks_local_loss_state() {
        let mut bound = Some(1);
        let mut validations = 0;
        for _ in 0..3 {
            bind_context::<_, ()>(
                &mut bound,
                1,
                || {
                    validations += 1;
                    Ok(())
                },
                || panic!("unexpected bind"),
            )
            .unwrap();
        }
        assert_eq!(validations, 3);
        assert!(
            bind_context(
                &mut bound,
                1,
                || Err(()),
                || panic!("bind after known loss")
            )
            .is_err()
        );
        assert_eq!(bound, None);
    }
}
