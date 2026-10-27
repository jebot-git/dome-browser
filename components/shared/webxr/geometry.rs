/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use euclid::{Point2D, Rect, Size2D};
use malloc_size_of_derive::MallocSizeOf;
use serde::{Deserialize, Serialize};

use crate::{Error, Viewport, Viewports};

/// A common allocation bound, further limited by the device and GL context.
pub const MAX_FRAMEBUFFER_DIMENSION: i32 = 4096;

/// The actual allocation and eye rectangles of one compositor layer. These
/// dimensions travel with its ID so script and compositors use the same layout.
#[derive(Clone, Debug, Deserialize, Serialize, MallocSizeOf)]
pub struct LayerGeometry {
    pub framebuffer_size: Size2D<i32, Viewport>,
    pub viewports: Viewports,
}

impl LayerGeometry {
    pub fn new(
        native: &Viewports,
        requested_scale: f32,
        limits: Size2D<i32, Viewport>,
    ) -> Result<Self, Error> {
        let invalid = || Error::BackendSpecific("Invalid XR layer geometry".into());
        if native.viewports.is_empty() || requested_scale.is_nan() {
            return Err(invalid());
        }
        let limits: Size2D<i32, Viewport> = Size2D::new(
            limits.width.min(MAX_FRAMEBUFFER_DIMENSION),
            limits.height.min(MAX_FRAMEBUFFER_DIMENSION),
        );
        if limits.width <= 0 || limits.height <= 0 {
            return Err(invalid());
        }
        let mut extent: Size2D<i32, Viewport> = Size2D::new(0, 0);
        let mut minimum_scale = 0.0_f64;
        for (index, rect) in native.viewports.iter().enumerate() {
            if rect.origin.x < 0
                || rect.origin.y < 0
                || rect.size.width <= 0
                || rect.size.height <= 0
            {
                return Err(invalid());
            }
            let right = rect
                .origin
                .x
                .checked_add(rect.size.width)
                .ok_or_else(invalid)?;
            let top = rect
                .origin
                .y
                .checked_add(rect.size.height)
                .ok_or_else(invalid)?;
            if native.viewports[..index]
                .iter()
                .any(|other| rect.intersects(other))
            {
                return Err(invalid());
            }
            extent.width = extent.width.max(right);
            extent.height = extent.height.max(top);
            minimum_scale = minimum_scale
                .max(1.0 / rect.size.width as f64)
                .max(1.0 / rect.size.height as f64);
        }
        let maximum_scale = (limits.width as f64 / extent.width as f64)
            .min(limits.height as f64 / extent.height as f64);
        if maximum_scale < minimum_scale {
            return Err(invalid());
        }
        // WebXR permits UA clamping/rounding. Keep every eye non-empty, and
        // clamp before any allocation or floating-point-to-integer conversion.
        let scale = (requested_scale as f64).clamp(minimum_scale, maximum_scale);
        let size = Size2D::new(
            (extent.width as f64 * scale)
                .ceil()
                .min(limits.width as f64) as i32,
            (extent.height as f64 * scale)
                .ceil()
                .min(limits.height as f64) as i32,
        );
        // Map shared edges with integer arithmetic. Independently rounding eye
        // widths would create gaps/overlap for odd sizes or asymmetric eyes.
        let edge = |value: i32, source: i32, target: i32| -> i32 {
            (i64::from(value) * i64::from(target) / i64::from(source)) as i32
        };
        let mut viewports = Vec::with_capacity(native.viewports.len());
        for rect in &native.viewports {
            let left = edge(rect.origin.x, extent.width, size.width);
            let right = edge(rect.max_x(), extent.width, size.width);
            let bottom = edge(rect.origin.y, extent.height, size.height);
            let top = edge(rect.max_y(), extent.height, size.height);
            if right <= left || top <= bottom {
                return Err(invalid());
            }
            viewports.push(Rect::new(
                Point2D::new(left, bottom),
                Size2D::new(right - left, top - bottom),
            ));
        }
        Ok(Self {
            framebuffer_size: size,
            viewports: Viewports { viewports },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native() -> Viewports {
        Viewports {
            viewports: vec![
                Rect::new(Point2D::zero(), Size2D::new(63, 95)),
                Rect::new(Point2D::new(63, 0), Size2D::new(64, 95)),
            ],
        }
    }
    fn geometry(scale: f32) -> LayerGeometry {
        LayerGeometry::new(&native(), scale, Size2D::new(512, 256)).unwrap()
    }
    #[test]
    fn fractional_and_asymmetric_eyes_share_exact_edges() {
        for scale in [0.5, 0.73, 1.0, 1.5, 2.0] {
            let g = geometry(scale);
            assert_eq!(
                g.viewports.viewports[0].max_x(),
                g.viewports.viewports[1].origin.x
            );
            assert_eq!(g.viewports.viewports[1].max_x(), g.framebuffer_size.width);
            assert!((g.framebuffer_size.width as f32 - 127. * scale).abs() <= 1.);
            assert!((g.framebuffer_size.height as f32 - 95. * scale).abs() <= 1.);
        }
    }
    #[test]
    fn zero_negative_and_tiny_scales_keep_every_eye_nonempty() {
        for scale in [0., -1., f32::NEG_INFINITY, f32::MIN_POSITIVE] {
            let g = geometry(scale);
            assert!(g.framebuffer_size.width > 0 && g.framebuffer_size.height > 0);
            assert!(
                g.viewports
                    .viewports
                    .iter()
                    .all(|v| v.size.width > 0 && v.size.height > 0)
            );
        }
    }
    #[test]
    fn huge_scales_are_clamped_before_conversion() {
        for scale in [100., f32::MAX, f32::INFINITY] {
            let g = geometry(scale);
            assert!(g.framebuffer_size.width <= 512 && g.framebuffer_size.height <= 256);
            assert_eq!(g.framebuffer_size.height, 256);
        }
    }
    #[test]
    fn offsets_and_secondary_views_use_the_full_framebuffer_extent() {
        let native = Viewports {
            viewports: vec![
                Rect::new(Point2D::new(10, 20), Size2D::new(40, 30)),
                Rect::new(Point2D::new(50, 20), Size2D::new(50, 30)),
                Rect::new(Point2D::new(0, 50), Size2D::new(100, 20)),
            ],
        };
        let g = LayerGeometry::new(&native, 0.5, Size2D::new(512, 256)).unwrap();
        assert_eq!(g.framebuffer_size, Size2D::new(50, 35));
        assert_eq!(g.viewports.viewports[0].origin, Point2D::new(5, 10));
        assert_eq!(g.viewports.viewports[2].max_y(), 35);
    }
    #[test]
    fn invalid_layouts_and_limits_fail_without_dimensions() {
        assert!(LayerGeometry::new(&native(), f32::NAN, Size2D::new(512, 256)).is_err());
        assert!(LayerGeometry::new(&native(), 1., Size2D::new(0, 256)).is_err());
        assert!(
            LayerGeometry::new(&Viewports { viewports: vec![] }, 1., Size2D::new(512, 256))
                .is_err()
        );
        let invalid = Viewports {
            viewports: vec![Rect::new(Point2D::new(i32::MAX, 0), Size2D::new(2, 2))],
        };
        assert!(LayerGeometry::new(&invalid, 1., Size2D::new(512, 256)).is_err());
    }
    #[test]
    fn overlapping_views_are_rejected() {
        let mut native = native();
        native.viewports[1].origin.x = 62;
        assert!(LayerGeometry::new(&native, 1., Size2D::new(512, 256)).is_err());
    }
}
