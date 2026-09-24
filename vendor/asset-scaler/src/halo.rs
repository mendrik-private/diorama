//! Reviewed, bounded dark-ink halo tone-down for the direct Lanczos base.
//!
//! Non-ink samples estimate nearby fill with positive weights. Only the one-pixel
//! neighborhood of a drawn core is eligible; the core and alpha never change.
//! This is a spatial appearance correction, not temporal stabilization.
use super::{color, raster::Mask};
#[cfg(test)]
use crate::CancellationToken;
use crate::{Cancellation, Result};
use image::{GrayImage, ImageBuffer, Rgba, imageops::FilterType};

const AMOUNT: f64 = 0.30;
const CAP: f64 = 12.0 / 255.0;

fn luma(p: &[f64]) -> f64 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}

pub(super) fn apply(
    source: &color::LinearImage,
    mask: &Mask,
    mut base: color::LinearImage,
    core: &GrayImage,
    cancel: &dyn Cancellation,
) -> Result<color::LinearImage> {
    cancel.check()?;
    assert_eq!((source.w, source.h), (mask.w, mask.h));
    assert_eq!((base.w as u32, base.h as u32), core.dimensions());
    // An absent contour has no eligible neighbors. Avoid an extra source pass.
    let mut has_core = false;
    for (i, &value) in core.as_raw().iter().enumerate() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        has_core |= value != 0;
    }
    if !has_core {
        return Ok(base);
    }
    let mut samples = Vec::with_capacity(source.pixels.len() * 4);
    for (i, p) in source.pixels.iter().enumerate() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        let a = if mask.data[i] { 0.0 } else { p[3] as f32 };
        samples.extend([p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, a]);
    }
    let samples =
        ImageBuffer::<Rgba<f32>, Vec<f32>>::from_vec(source.w as u32, source.h as u32, samples)
            .expect("LinearImage pixel dimensions match its storage");
    // Missing ink is evidence weight, not output transparency. Positive weights
    // avoid the signed-lobe/near-zero-residual amplification of full subtraction.
    let estimate =
        image::imageops::resize(&samples, base.w as u32, base.h as u32, FilterType::Triangle);
    drop(samples);
    cancel.check()?;
    for y in 0..base.h {
        cancel.check()?;
        for x in 0..base.w {
            let i = y * base.w + x;
            if i.is_multiple_of(4096) {
                cancel.check()?;
            }
            let p = base.pixels[i];
            if core.as_raw()[i] != 0 || p[3] <= 1e-8 {
                continue;
            }
            let adjacent = (-1isize..=1).any(|dy| {
                (-1isize..=1).any(|dx| {
                    let xx = x as isize + dx;
                    let yy = y as isize + dy;
                    xx >= 0
                        && yy >= 0
                        && xx < base.w as isize
                        && yy < base.h as isize
                        && core.as_raw()[yy as usize * base.w + xx as usize] != 0
                })
            });
            if !adjacent {
                continue;
            }
            let e = estimate.get_pixel(x as u32, y as u32).0;
            let weight = f64::from(e[3]);
            if weight <= 0.25 {
                continue;
            }
            let confidence = ((weight - 0.25) / 0.5).clamp(0.0, 1.0);
            let confidence = confidence * confidence * (3.0 - 2.0 * confidence);
            let candidate: [f64; 3] =
                std::array::from_fn(|c| (f64::from(e[c]) / weight).clamp(0.0, 1.0));
            // Treat dark ink bleed, not bright ringing or global sharpening.
            let lift = ((luma(&candidate) - luma(&p)) / 0.04).clamp(0.0, 1.0);
            let mix = AMOUNT * confidence * lift;
            let encoded: [f64; 3] = std::array::from_fn(|c| color::encode(p[c]));
            let delta: [f64; 3] = std::array::from_fn(|c| {
                color::encode(p[c] + mix * (candidate[c] - p[c])) - encoded[c]
            });
            let peak = delta.iter().fold(0.0f64, |a, d| a.max(d.abs()));
            let scale = if peak > 0.0 {
                (CAP / peak).min(1.0)
            } else {
                0.0
            };
            // Scale the correction vector together, not channels independently.
            for c in 0..3 {
                base.pixels[i][c] = color::decode((encoded[c] + scale * delta[c]).clamp(0.0, 1.0));
            }
        }
    }
    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::super::lanczos;
    use super::*;
    use crate::Error;

    #[test]
    fn moderate_correction_reduces_known_bleed_with_locality_and_cap() {
        let source = color::LinearImage {
            w: 48,
            h: 48,
            pixels: (0..48 * 48)
                .map(|i| {
                    if (22..24).contains(&(i % 48)) {
                        [0.01, 0.01, 0.01, 1.0]
                    } else {
                        [0.6, 0.4, 0.2, 1.0]
                    }
                })
                .collect(),
        };
        let mask = Mask {
            w: 48,
            h: 48,
            data: (0..48 * 48).map(|i| (22..24).contains(&(i % 48))).collect(),
        };
        let cancel = CancellationToken::default();
        let base = lanczos::resize(&source, 15, 15, &cancel).unwrap();
        let core = GrayImage::from_fn(15, 15, |x, _| image::Luma([if x == 7 { 255 } else { 0 }]));
        let result = apply(&source, &mask, base.clone(), &core, &cancel).unwrap();
        let i = 7 * 15 + 6;
        assert!(base.pixels[i][0] < 0.59, "fixture has ink bleed");
        assert!(
            result.pixels[i][0] > base.pixels[i][0] + 0.001,
            "must reduce halo"
        );
        assert!(result.pixels[i][0] <= 0.6, "must not overshoot known fill");
        for (i, (a, b)) in base.pixels.iter().zip(&result.pixels).enumerate() {
            assert_eq!(a[3], b[3]);
            if i % 15 == 7 || !(6..=8).contains(&(i % 15)) {
                assert_eq!(a, b);
            }
            for c in 0..3 {
                assert!(b[c].is_finite() && (0.0..=1.0).contains(&b[c]));
                assert!(color::rgba(*a)[c].abs_diff(color::rgba(*b)[c]) <= 12);
            }
        }
    }

    #[test]
    fn constants_missing_evidence_no_core_and_transparency_stay_unchanged() {
        let cancel = CancellationToken::default();
        let core = GrayImage::from_fn(4, 4, |x, _| image::Luma([if x == 2 { 255 } else { 0 }]));
        for alpha in [0.0, 0.5, 1.0] {
            let source = color::LinearImage {
                w: 12,
                h: 12,
                pixels: vec![[0.3, 0.3, 0.3, alpha]; 144],
            };
            let base = lanczos::resize(&source, 4, 4, &cancel).unwrap();
            for masked in [false, true] {
                let mask = Mask {
                    w: 12,
                    h: 12,
                    data: vec![masked; 144],
                };
                let result = apply(&source, &mask, base.clone(), &core, &cancel).unwrap();
                for (a, b) in base.pixels.iter().zip(&result.pixels) {
                    assert_eq!(a[3], b[3]);
                    assert_eq!(color::rgba(*a), color::rgba(*b));
                }
                assert_eq!(
                    base.pixels,
                    apply(&source, &mask, base.clone(), &GrayImage::new(4, 4), &cancel)
                        .unwrap()
                        .pixels
                );
            }
        }
        let source = color::LinearImage {
            w: 12,
            h: 12,
            pixels: vec![[1.0, 0.0, 1.0, 0.0]; 144],
        };
        let base = lanczos::resize(&source, 4, 4, &cancel).unwrap();
        assert_eq!(
            base.pixels,
            apply(&source, &Mask::new(12, 12), base.clone(), &core, &cancel)
                .unwrap()
                .pixels
        );
        cancel.cancel();
        assert!(matches!(
            apply(&source, &Mask::new(12, 12), base, &core, &cancel),
            Err(Error::Cancelled)
        ));
    }
}
