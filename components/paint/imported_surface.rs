/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

/// Consumer-local ownership, including outstanding renderer lock callbacks.
/// Never discard this record while its texture is present.
pub(super) struct ImportedSurface<T, C> {
    pub chain: C,
    pub texture: Option<T>,
    pub locks: usize,
}
impl<T, C> ImportedSurface<T, C> {
    pub fn release<S, E>(
        &mut self,
        export: impl FnOnce(T) -> Result<S, (E, T)>,
        recycle: impl FnOnce(&C, S),
    ) -> Result<bool, E> {
        if self.locks != 0 {
            return Ok(false);
        }
        let Some(texture) = self.texture.take() else {
            return Ok(true);
        };
        match export(texture) {
            Ok(surface) => {
                recycle(&self.chain, surface);
                Ok(true)
            },
            Err((error, texture)) => {
                self.texture = Some(texture);
                Err(error)
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_export_retains_exact_handle_and_retry_returns_once() {
        let mut record = ImportedSurface {
            chain: 7,
            texture: Some(23),
            locks: 0,
        };
        assert_eq!(
            record.release(|t| Err::<i32, _>(("unavailable", t)), |_, _| panic!()),
            Err("unavailable")
        );
        assert_eq!(record.texture, Some(23));
        let mut returned = Vec::new();
        assert_eq!(
            record.release(|t| Ok::<_, ((), i32)>(t), |c, s| returned.push((*c, s))),
            Ok(true)
        );
        assert_eq!(
            record.release(|_| panic!(), |_, _: i32| panic!()),
            Ok::<_, ()>(true)
        );
        assert_eq!(returned, [(7, 23)]);
    }
    #[test]
    fn active_and_nested_locks_prevent_export() {
        let mut record = ImportedSurface {
            chain: (),
            texture: Some(23),
            locks: 2,
        };
        for locks in [2, 1] {
            record.locks = locks;
            assert_eq!(
                record.release(|_| panic!(), |_, _: i32| panic!()),
                Ok::<_, ()>(false)
            );
            assert_eq!(record.texture, Some(23));
        }
        record.locks = 0;
        assert_eq!(
            record.release(|t| Ok::<_, ((), i32)>(t), |_, s| assert_eq!(s, 23)),
            Ok(true)
        );
    }
}
