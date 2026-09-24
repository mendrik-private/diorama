//! One opacity per contour, using final painted length and mean source width.
use crate::{Cancellation, Result};
use crate::{contours::Contours, detect::Sample, raster::Mask};
use image::{GrayImage, RgbaImage};

pub fn strength(length: usize, width: f64) -> f64 {
    let length = ((length as f64 - 4.) / 28.).clamp(0., 1.);
    let width = ((width - 1.) / 5.).clamp(0., 1.);
    0.95 + 0.05 * (length * width).sqrt()
}

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
pub fn calculate(widths: &[f64], core: &GrayImage, pixel_owners: &[Option<usize>]) -> Vec<f64> {
    let mut lengths = vec![0; widths.len()];
    for (&value, owner) in core.as_raw().iter().zip(pixel_owners) {
        if value > 0
            && let Some(id) = owner
        {
            lengths[*id] += 1;
        }
    }
    lengths
        .iter()
        .zip(widths)
        .map(|(&l, &w)| strength(l, w))
        .collect()
}

pub fn apply(coverage: &GrayImage, owners: &[Option<usize>], opacities: &[f64]) -> GrayImage {
    GrayImage::from_fn(coverage.width(), coverage.height(), |x, y| {
        let i = (y * coverage.width() + x) as usize;
        let opacity = owners[i].map_or(0., |id| opacities[id]);
        image::Luma([(coverage.as_raw()[i] as f64 * opacity).round() as u8])
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opacity_is_bounded_monotonic_and_needs_both_length_and_width() {
        assert_eq!(strength(4, 1.), 0.95);
        assert_eq!(strength(100, 1.), 0.95);
        assert_eq!(strength(4, 10.), 0.95);
        assert_eq!(strength(32, 6.), 1.);
        assert_eq!(strength(100, 10.), 1.);
        for l in 0..100 {
            for w in 1..10 {
                assert!((0.95..=1.).contains(&strength(l, w as f64)));
                assert!(strength(l + 1, w as f64) >= strength(l, w as f64));
                assert!(strength(l, w as f64 + 1.) >= strength(l, w as f64));
            }
        }
    }
    #[test]
    fn measures_actual_source_width_and_multiplies_aa_without_stacking() {
        for width in [1, 3, 7] {
            let source = RgbaImage::from_pixel(20, 20, image::Rgba([0, 0, 0, 255]));
            let mut mask = Mask::new(20, 20);
            for y in 0..20 {
                for x in 10 - width / 2..=10 + width / 2 {
                    mask.data[y * 20 + x] = true;
                }
            }
            let sample = [10., 10., 1., 0., 0., 0., 0., 1.];
            assert_eq!(measure(&source, &mask, &sample), Some(width as f64));
        }
        let coverage = GrayImage::from_raw(3, 1, vec![255, 128, 0]).unwrap();
        assert_eq!(
            apply(&coverage, &[Some(0), Some(0), None], &[0.95]).as_raw(),
            &[242, 122, 0]
        );
    }
}
