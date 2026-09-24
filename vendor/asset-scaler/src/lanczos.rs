//! One-pass premultiplied-linear Lanczos3 source reduction.
//!
//! The source is sampled directly; retained ink is never subtracted or
//! reconstructed here. Silhouette support, intrinsic opacity, contours, and
//! paint remain the responsibility of the surrounding Game Asset pipeline.
use super::color::LinearImage;
#[cfg(test)]
use crate::CancellationToken;
use crate::{Cancellation, Result};
use image::{ImageBuffer, Rgba, imageops::FilterType};

/// Resample a straight-linear source in premultiplied RGBA, returning guarded
/// straight-linear pixels. The `image` crate's Lanczos3 implementation expects
/// scene-linear premultiplied input for varying-alpha images.
pub(super) fn resize(
    source: &LinearImage,
    w: usize,
    h: usize,
    cancel: &dyn Cancellation,
) -> Result<LinearImage> {
    cancel.check()?;
    let mut samples = Vec::with_capacity(source.pixels.len() * 4);
    for (i, sample) in source.pixels.iter().enumerate() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        let alpha = sample[3] as f32;
        samples.extend([
            sample[0] as f32 * alpha,
            sample[1] as f32 * alpha,
            sample[2] as f32 * alpha,
            alpha,
        ]);
    }
    let premultiplied =
        ImageBuffer::<Rgba<f32>, Vec<f32>>::from_vec(source.w as u32, source.h as u32, samples)
            .expect("LinearImage pixel dimensions match its storage");
    let resized = image::imageops::resize(&premultiplied, w as u32, h as u32, FilterType::Lanczos3);
    cancel.check()?;
    let mut pixels = Vec::with_capacity(w * h);
    for (i, sample) in resized.pixels().enumerate() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        let alpha = f64::from(sample[3]).clamp(0., 1.);
        pixels.push(if alpha <= 1e-8 {
            [0.; 4]
        } else {
            [
                (f64::from(sample[0]) / alpha).clamp(0., 1.),
                (f64::from(sample[1]) / alpha).clamp(0., 1.),
                (f64::from(sample[2]) / alpha).clamp(0., 1.),
                alpha,
            ]
        });
    }
    Ok(LinearImage { w, h, pixels })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;

    #[test]
    fn constant_premultiplied_source_survives_lanczos_resize() {
        let source = LinearImage {
            w: 9,
            h: 7,
            pixels: vec![[0.3, 0.5, 0.2, 0.6]; 63],
        };
        let resized = resize(&source, 5, 4, &CancellationToken::default()).unwrap();
        for pixel in resized.pixels {
            for (actual, expected) in pixel.into_iter().zip([0.3, 0.5, 0.2, 0.6]) {
                assert!((actual - expected).abs() < 2e-6, "{actual} != {expected}");
            }
        }
    }

    #[test]
    fn transparent_hidden_rgb_does_not_leak_after_resample() {
        let source = LinearImage {
            w: 2,
            h: 1,
            pixels: vec![[1., 0., 0., 1.], [0., 1., 1., 0.]],
        };
        let pixel = resize(&source, 1, 1, &CancellationToken::default())
            .unwrap()
            .pixels[0];
        assert!(pixel[0] > 0.99999);
        assert!(pixel[1].abs() < 1e-7 && pixel[2].abs() < 1e-7);
        assert!((pixel[3] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn cancellation_is_checked_before_conversion() {
        let source = LinearImage {
            w: 3,
            h: 3,
            pixels: vec![[0.2; 4]; 9],
        };
        let cancel = CancellationToken::default();
        cancel.cancel();
        assert!(matches!(
            resize(&source, 2, 2, &cancel),
            Err(Error::Cancelled)
        ));
    }
}
