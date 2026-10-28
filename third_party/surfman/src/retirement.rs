// SPDX-License-Identifier: MPL-2.0
//! Retryable destruction without dropping an unreleased resource.
pub(crate) fn retire_pending<T, E>(
    pending: &mut Vec<T>,
    mut destroy: impl FnMut(&mut T) -> Result<(), E>,
) -> Result<(), E> {
    let mut first_error = None;
    for mut resource in std::mem::take(pending) {
        if let Err(error) = destroy(&mut resource) {
            pending.push(resource);
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    struct Resource {
        id: u8,
        released: bool,
        drops: Rc<Cell<u32>>,
    }
    impl Drop for Resource {
        fn drop(&mut self) {
            assert!(self.released, "unreleased resource dropped");
            self.drops.set(self.drops.get() + 1);
        }
    }
    #[test]
    fn failed_resources_survive_and_successful_resources_are_not_retried() {
        let drops = Rc::new(Cell::new(0));
        let mut pending = (0..4)
            .map(|id| Resource {
                id,
                released: false,
                drops: drops.clone(),
            })
            .collect();
        let mut attempts = vec![];
        assert_eq!(
            retire_pending(&mut pending, |r| {
                attempts.push(r.id);
                if r.id % 2 == 0 {
                    r.released = true;
                    Ok(())
                } else {
                    Err(r.id)
                }
            }),
            Err(1)
        );
        assert_eq!(attempts, [0, 1, 2, 3]);
        assert_eq!(drops.get(), 2);
        assert_eq!(pending.iter().map(|r| r.id).collect::<Vec<_>>(), [1, 3]);
        attempts.clear();
        retire_pending::<_, u8>(&mut pending, |r| {
            attempts.push(r.id);
            r.released = true;
            Ok(())
        })
        .unwrap();
        assert_eq!(attempts, [1, 3]);
        assert!(pending.is_empty());
        assert_eq!(drops.get(), 4);
    }
    #[test]
    fn repeated_failures_retain_ownership() {
        let drops = Rc::new(Cell::new(0));
        let mut pending = vec![Resource {
            id: 7,
            released: false,
            drops: drops.clone(),
        }];
        for _ in 0..3 {
            assert_eq!(retire_pending(&mut pending, |_| Err(7)), Err(7));
            assert_eq!(pending.len(), 1);
            assert_eq!(drops.get(), 0);
        }
        retire_pending::<_, u8>(&mut pending, |r| {
            r.released = true;
            Ok(())
        })
        .unwrap();
        assert_eq!(drops.get(), 1);
    }
    #[test]
    fn empty_retirement_does_not_call_the_backend() {
        let mut pending: Vec<u8> = vec![];
        retire_pending::<_, u8>(&mut pending, |_| panic!("unexpected destruction")).unwrap();
    }
}
