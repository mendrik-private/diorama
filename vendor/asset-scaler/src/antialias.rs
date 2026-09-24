//! Interpolate binary contour selection toward the editor pen's AA profile.
use super::raster::Mask;
#[cfg(test)]
use crate::CancellationToken;
use crate::{Cancellation, GameAssetAa, Result};

pub fn coverage_map(
    mask: &Mask,
    pen_coverage: &[f64],
    intensity: GameAssetAa,
    cancel: &dyn Cancellation,
) -> Result<image::GrayImage> {
    assert_eq!(mask.data.len(), pen_coverage.len());
    cancel.check()?;
    let fraction = f64::from(intensity.percent()) / 100.;
    let mut result = image::GrayImage::new(mask.w as u32, mask.h as u32);
    for y in 0..mask.h {
        cancel.check()?;
        for x in 0..mask.w {
            let i = y * mask.w + x;
            let binary = if mask.data[i] { 1. } else { 0. };
            let coverage = binary + (pen_coverage[i] - binary) * fraction;
            result.put_pixel(
                x as u32,
                y as u32,
                image::Luma([(coverage.clamp(0., 1.) * 255.).round() as u8]),
            );
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intensity_interpolates_binary_and_pen_coverage_monotonically() {
        let cancel = CancellationToken::default();
        let mut mask = Mask::new(2, 1);
        mask.pixel(0, 0);
        let pen = [0.375, 0.375];
        let mut previous_core = 255;
        let mut previous_fringe = 0;
        for percent in 0..=100 {
            let aa = coverage_map(&mask, &pen, GameAssetAa::new(percent), &cancel).unwrap();
            let core = aa.get_pixel(0, 0)[0];
            let fringe = aa.get_pixel(1, 0)[0];
            assert_eq!(
                core,
                (255. * (1. - 0.625 * f64::from(percent) / 100.)).round() as u8
            );
            assert_eq!(
                fringe,
                (255. * 0.375 * f64::from(percent) / 100.).round() as u8
            );
            assert!(core <= previous_core);
            assert!(fringe >= previous_fringe);
            previous_core = core;
            previous_fringe = fringe;
        }
        assert_eq!(
            coverage_map(&mask, &pen, GameAssetAa::new(0), &cancel)
                .unwrap()
                .as_raw(),
            &[255, 0]
        );
        assert_eq!(
            coverage_map(&mask, &pen, GameAssetAa::new(100), &cancel)
                .unwrap()
                .as_raw(),
            &[96, 96]
        );
    }

    #[test]
    fn cancellation_is_observed() {
        let cancel = CancellationToken::default();
        let mask = Mask::new(2, 2);
        cancel.cancel();
        assert!(matches!(
            coverage_map(&mask, &[0.; 4], GameAssetAa::default(), &cancel),
            Err(crate::Error::Cancelled)
        ));
    }
}
