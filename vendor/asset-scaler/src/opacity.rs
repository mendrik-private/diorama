//! Convert local contour evidence and projected normal thickness into paint.
use crate::{Cancellation, Error, Result};
use crate::{contours::Contours, detect::Sample, raster::Mask};
use image::{GrayImage, RgbaImage};

fn measure(source: &RgbaImage, mask: &Mask, p: &Sample) -> Option<f64> {
    let on = |d: f64| {
        let (x, y) = (
            (p[0] + d * p[2] + 0.5).floor() as isize,
            (p[1] + d * p[3] + 0.5).floor() as isize,
        );
        mask.at(x, y) && source.get_pixel(x as u32, y as u32)[3] as f64 / 255. > 0.035
    };
    if !on(0.) {
        return None;
    }
    let extent = |sign: f64| {
        (1..=128)
            .find(|&i| !on(sign * i as f64 * 0.25))
            .unwrap_or(129) as f64
            * 0.25
    };
    Some(extent(-1.) + extent(1.) - 0.25)
}

pub fn widths(
    source: &RgbaImage,
    mask: &Mask,
    samples: &[Sample],
    contours: &Contours,
    cancel: &dyn Cancellation,
) -> Result<Vec<f64>> {
    let n = contours.lengths.len();
    let mut total = vec![0.; n];
    let mut count = vec![0; n];
    for (p, owner) in samples.iter().zip(contours.sample_owners(samples)) {
        cancel.check()?;
        if let (Some(id), Some(width)) = (owner, measure(source, mask, p)) {
            total[id] += width;
            count[id] += 1;
        }
    }
    Ok((0..n)
        .map(|id| {
            if count[id] > 0 {
                total[id] / count[id] as f64
            } else {
                1.
            }
        })
        .collect())
}

/// Aggregate detector prominence by trace. Local target refinement later
/// restores strong shoulders within a mixed trace without boosting its whole
/// length.
pub fn importances(samples: &[Sample], contours: &Contours, widths: &[f64]) -> Vec<f64> {
    let mut values = vec![Vec::new(); contours.lengths.len()];
    for (sample, owner) in samples.iter().zip(contours.sample_owners(samples)) {
        if let Some(owner) = owner {
            values[owner].push(sample[8]);
        }
    }
    values
        .into_iter()
        .zip(widths)
        .map(|(mut values, &width)| {
            if values.is_empty() {
                return 0.35;
            }
            values.sort_by(f64::total_cmp);
            let median = values[(values.len() - 1) / 2];
            let upper = values[(values.len() - 1) * 3 / 4];
            // Broad, consistently supported colour boundaries must not lose
            // to one low-contrast patch on a dark hat or cloak. A narrow
            // hatch gets little width support and still fades with scale.
            (0.30 * median + 0.50 * upper + 0.20 * ((width - 0.75) / 3.).clamp(0., 1.))
                .clamp(0.08, 0.98)
        })
        .collect()
}

/// Convert target-local structural evidence into paint strength. Weak details
/// fade smoothly as their *normal* thickness is reduced; a line kept at full
/// length by anisotropic scaling does not pay for the unrelated axis.
pub fn pixel_strengths_cancellable(
    importance: &[f64],
    tangents: &[Option<[f64; 2]>],
    scale: [f64; 2],
    cancel: &dyn Cancellation,
) -> Result<Vec<f64>> {
    if importance.len() != tangents.len() {
        return Err(Error::Scaling("Invalid contour opacity dimensions".into()));
    }
    let mut strengths = Vec::with_capacity(importance.len());
    for (i, (&importance, tangent)) in importance.iter().zip(tangents).enumerate() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        let tangent = tangent.unwrap_or([1., 0.]);
        let source_length = (tangent[0] / scale[0].max(1e-9))
            .hypot(tangent[1] / scale[1].max(1e-9))
            .max(1e-9);
        let tangent = [
            tangent[0] / scale[0].max(1e-9) / source_length,
            tangent[1] / scale[1].max(1e-9) / source_length,
        ];
        let normal_scale = (scale[0] * scale[1]
            / (tangent[0] * scale[0])
                .hypot(tangent[1] * scale[1])
                .max(1e-9))
        .clamp(0., 1.);
        let loss = (1. - normal_scale) * (1. - importance) * 1.3;
        strengths.push((importance * (1. - loss)).clamp(0., 1.));
    }
    Ok(strengths)
}

pub fn apply_pixels(coverage: &GrayImage, strengths: &[f64]) -> Result<GrayImage> {
    if coverage.as_raw().len() != strengths.len() {
        return Err(Error::Scaling("Invalid contour opacity dimensions".into()));
    }
    Ok(GrayImage::from_fn(
        coverage.width(),
        coverage.height(),
        |x, y| {
            let i = (y * coverage.width() + x) as usize;
            let strength = strengths[i];
            image::Luma([(coverage.as_raw()[i] as f64 * strength).round() as u8])
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn measures_actual_source_width() {
        for width in [1, 3, 7] {
            let source = RgbaImage::from_pixel(20, 20, image::Rgba([0, 0, 0, 255]));
            let mut mask = Mask::new(20, 20);
            for y in 0..20 {
                for x in 10 - width / 2..=10 + width / 2 {
                    mask.data[y * 20 + x] = true;
                }
            }
            let sample = [10., 10., 1., 0., 0., 0., 0., 1., 1.];
            assert_eq!(measure(&source, &mask, &sample), Some(width as f64));
        }
    }

    #[test]
    fn weak_prominence_fades_with_normal_scale_and_strong_ink_survives() {
        let tangents = [Some([1., 0.]), Some([1., 0.])];
        let cancel = crate::CancellationToken::default();
        let large =
            pixel_strengths_cancellable(&[0.5, 0.95], &tangents, [0.75, 0.75], &cancel).unwrap();
        let small =
            pixel_strengths_cancellable(&[0.5, 0.95], &tangents, [0.25, 0.25], &cancel).unwrap();
        assert!(small[0] * 4. < large[0] * 3.);
        assert!(small[1] > 0.85);
        let unchanged_normal =
            pixel_strengths_cancellable(&[0.5], &[Some([1., 0.])], [0.25, 1.], &cancel).unwrap();
        assert_eq!(unchanged_normal, vec![0.5]);
        let reduced_normal =
            pixel_strengths_cancellable(&[0.5], &[Some([1., 0.])], [1., 0.25], &cancel).unwrap();
        assert!(reduced_normal[0] < 0.5);
        let diagonal =
            pixel_strengths_cancellable(&[0.5], &[Some([0.8, 0.6])], [0.5, 0.25], &cancel).unwrap();
        assert!(diagonal[0] > 0.25 && diagonal[0] < 0.5);
    }
}
