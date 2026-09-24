//! Conservative source support extraction for assets with a real exterior.
//!
//! Ridges are not generally closed paths, so source support is authoritative
//! independently of open contours. Transparent sources use alpha support and
//! opaque flat canvases use matching pixels connected to the image boundary as
//! an extracted transparent exterior.
use super::{color::LinearImage, raster::Mask};
#[cfg(test)]
use crate::CancellationToken;
use crate::{Cancellation, GameAssetAa, Result};
use image::RgbaImage;
use std::collections::VecDeque;

const OPAQUE: u8 = 255;
const BACKGROUND_TOLERANCE: i32 = 3;

pub struct Silhouette {
    pub support: Mask,
}

impl Silhouette {
    /// Return `None` when an opaque image has no confidently flat canvas.
    pub fn detect(source: &RgbaImage, cancel: &dyn Cancellation) -> Result<Option<Self>> {
        Self::detect_with_opaque_background_removal(source, true, cancel)
    }

    /// Extract support from source alpha and, when requested, a flat opaque
    /// canvas.  Foreground-specific callers retain the default behavior of
    /// [`Self::detect`].
    pub fn detect_with_opaque_background_removal(
        source: &RgbaImage,
        remove_opaque_background: bool,
        cancel: &dyn Cancellation,
    ) -> Result<Option<Self>> {
        cancel.check()?;
        let (w, h) = (source.width() as usize, source.height() as usize);
        if source.pixels().any(|p| p[3] != OPAQUE) {
            let mut support = Mask::new(w, h);
            for (i, p) in source.pixels().enumerate() {
                if i.is_multiple_of(4096) {
                    cancel.check()?;
                }
                support.data[i] = p[3] != 0;
            }
            if support.data.iter().all(|&v| v) {
                return Ok(None);
            }
            return Ok(Some(Self { support }));
        }

        if !remove_opaque_background {
            return Ok(None);
        }

        let corners = [
            source.get_pixel(0, 0).0,
            source.get_pixel((w - 1) as u32, 0).0,
            source.get_pixel(0, (h - 1) as u32).0,
            source.get_pixel((w - 1) as u32, (h - 1) as u32).0,
        ];
        let background = median(corners);
        if corners
            .iter()
            .filter(|pixel| close(pixel, &background))
            .count()
            < 3
        {
            return Ok(None);
        }

        let mut exterior = vec![false; w * h];
        let mut queue = VecDeque::new();
        for x in 0..w {
            queue.push_back((x, 0));
            queue.push_back((x, h - 1));
        }
        for y in 1..h.saturating_sub(1) {
            queue.push_back((0, y));
            queue.push_back((w - 1, y));
        }
        let mut visited = 0usize;
        while let Some((x, y)) = queue.pop_front() {
            let i = y * w + x;
            if exterior[i] || !close(&source.get_pixel(x as u32, y as u32).0, &background) {
                continue;
            }
            exterior[i] = true;
            visited += 1;
            if visited.is_multiple_of(4096) {
                cancel.check()?;
            }
            if x > 0 {
                queue.push_back((x - 1, y));
            }
            if x + 1 < w {
                queue.push_back((x + 1, y));
            }
            if y > 0 {
                queue.push_back((x, y - 1));
            }
            if y + 1 < h {
                queue.push_back((x, y + 1));
            }
        }
        Ok(Some(Self {
            support: Mask {
                w,
                h,
                data: exterior.into_iter().map(|v| !v).collect(),
            },
        }))
    }

    pub fn isolated(&self, source: &LinearImage, cancel: &dyn Cancellation) -> Result<LinearImage> {
        let mut result = source.clone();
        for (i, (pixel, &supported)) in result.pixels.iter_mut().zip(&self.support.data).enumerate()
        {
            if i.is_multiple_of(4096) {
                cancel.check()?;
            }
            if !supported {
                *pixel = [0.; 4];
            }
        }
        Ok(result)
    }

    /// Exact fractional coverage of the source support at an actual target
    /// size.  This is deliberately independent of the (open) ink paths.
    pub fn coverage(&self, w: usize, h: usize, cancel: &dyn Cancellation) -> Result<Vec<f64>> {
        self.project_mask(&self.support, w, h, cancel)
    }

    pub fn project_mask(
        &self,
        mask: &Mask,
        w: usize,
        h: usize,
        cancel: &dyn Cancellation,
    ) -> Result<Vec<f64>> {
        assert_eq!((mask.w, mask.h), (self.support.w, self.support.h));
        let sx = mask.w as f64 / w as f64;
        let sy = mask.h as f64 / h as f64;
        let mut vertical = vec![0.; mask.w * h];
        for y in 0..h {
            cancel.check()?;
            let top = y as f64 * sy;
            let bottom = (y + 1) as f64 * sy;
            for yy in top.floor() as usize..(bottom.ceil() as usize).min(mask.h) {
                let weight = (bottom.min((yy + 1) as f64) - top.max(yy as f64)) / sy;
                for x in 0..mask.w {
                    vertical[y * self.support.w + x] +=
                        f64::from(mask.data[yy * mask.w + x]) * weight;
                }
            }
        }
        let mut result = vec![0.; w * h];
        for y in 0..h {
            cancel.check()?;
            for x in 0..w {
                let left = x as f64 * sx;
                let right = (x + 1) as f64 * sx;
                for xx in left.floor() as usize..(right.ceil() as usize).min(mask.w) {
                    let weight = (right.min((xx + 1) as f64) - left.max(xx as f64)) / sx;
                    result[y * w + x] += vertical[y * self.support.w + xx] * weight;
                }
            }
        }
        Ok(result)
    }

    /// Convert source support to target alpha coverage without eroding the
    /// source silhouette.  The caller paints contours separately.
    pub fn target_coverage(
        &self,
        coverage: &[f64],
        aa: GameAssetAa,
        cancel: &dyn Cancellation,
    ) -> Result<Vec<f64>> {
        cancel.check()?;
        let intensity = f64::from(aa.percent()) / 100.;
        coverage
            .iter()
            .enumerate()
            .map(|(i, &area)| {
                if i.is_multiple_of(4096) {
                    cancel.check()?;
                }
                Ok(if area < 0.5 {
                    0.
                } else {
                    1. - intensity + area * intensity
                })
            })
            .collect()
    }

    /// Source alpha projected independently of repair.  Repairing an outer
    /// ink path against a transparent exterior must not turn an originally
    /// opaque object translucent.
    pub fn intrinsic_opacity(
        &self,
        source: &LinearImage,
        coverage: &[f64],
        w: usize,
        h: usize,
        cancel: &dyn Cancellation,
    ) -> Result<Vec<f64>> {
        let sx = source.w as f64 / w as f64;
        let sy = source.h as f64 / h as f64;
        let mut vertical = vec![0.; source.w * h];
        for y in 0..h {
            cancel.check()?;
            let top = y as f64 * sy;
            let bottom = (y + 1) as f64 * sy;
            for yy in top.floor() as usize..(bottom.ceil() as usize).min(source.h) {
                let weight = (bottom.min((yy + 1) as f64) - top.max(yy as f64)) / sy;
                for x in 0..source.w {
                    vertical[y * source.w + x] += source.pixels[yy * source.w + x][3] * weight;
                }
            }
        }
        let mut result = vec![0.; w * h];
        for y in 0..h {
            cancel.check()?;
            for x in 0..w {
                let left = x as f64 * sx;
                let right = (x + 1) as f64 * sx;
                for xx in left.floor() as usize..(right.ceil() as usize).min(source.w) {
                    let weight = (right.min((xx + 1) as f64) - left.max(xx as f64)) / sx;
                    result[y * w + x] += vertical[y * source.w + xx] * weight;
                }
            }
        }
        Ok(result
            .into_iter()
            .zip(coverage)
            .map(|(alpha, &support)| if support > 0. { alpha / support } else { 0. })
            .collect())
    }
}

fn median(mut pixels: [[u8; 4]; 4]) -> [u8; 4] {
    std::array::from_fn(|channel| {
        pixels.sort_unstable_by_key(|pixel| pixel[channel]);
        pixels[2][channel]
    })
}

fn close(pixel: &[u8; 4], background: &[u8; 4]) -> bool {
    pixel[..3]
        .iter()
        .zip(&background[..3])
        .all(|(&a, &b)| (i32::from(a) - i32::from(b)).abs() <= BACKGROUND_TOLERANCE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn boundary_connected_canvas_keeps_enclosed_matching_detail() {
        let mut image = RgbaImage::from_pixel(9, 9, Rgba([30, 60, 90, 255]));
        for y in 2..7 {
            for x in 2..7 {
                image.put_pixel(x, y, Rgba([200, 30, 20, 255]));
            }
        }
        image.put_pixel(4, 4, Rgba([30, 60, 90, 255]));
        let mask = Silhouette::detect(&image, &CancellationToken::default())
            .unwrap()
            .unwrap();
        assert!(mask.support.at(4, 4));
        assert!(!mask.support.at(0, 0));
    }

    #[test]
    fn boundary_mask_keeps_gaps_islands_and_border_touching_foreground() {
        let mut image = RgbaImage::from_pixel(11, 9, Rgba([30, 60, 90, 255]));
        // A border-touching, disconnected red island remains foreground; an
        // open channel remains true exterior rather than becoming a filled hole.
        for y in 0..5 {
            image.put_pixel(5, y, Rgba([210, 40, 30, 255]));
        }
        image.put_pixel(9, 7, Rgba([210, 40, 30, 255]));
        image.put_pixel(5, 2, Rgba([30, 60, 90, 255]));
        let mask = Silhouette::detect(&image, &CancellationToken::default())
            .unwrap()
            .unwrap();
        assert!(mask.support.at(5, 0));
        assert!(mask.support.at(9, 7));
        assert!(!mask.support.at(5, 2));
    }

    #[test]
    fn varied_opaque_corners_do_not_claim_an_exterior() {
        let image = RgbaImage::from_fn(5, 5, |x, y| {
            Rgba([(x * 31) as u8, (y * 47) as u8, (x * y * 9) as u8, 255])
        });
        assert!(
            Silhouette::detect(&image, &CancellationToken::default())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn antialias_coverage_is_applied_once() {
        let silhouette = Silhouette {
            support: Mask::new(1, 1),
        };
        let result = silhouette
            .target_coverage(&[0.75], GameAssetAa::new(50), &CancellationToken::default())
            .unwrap();
        assert_eq!(result, vec![0.875]);
    }
}
