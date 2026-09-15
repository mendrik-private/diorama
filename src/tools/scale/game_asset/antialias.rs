//! Contour-local manual AA, inspired by Inglis et al. (NPAR 2013), section 7.
//! Normalize thin-stroke samples by local apparent thickness, then quantize AA
//! opacity (not image colors). The agreed core floor is enforced LAST.
use super::{cleanup::N8, raster::Mask};
use crate::{
    document::{CancellationToken, GameAssetAa},
    error::Result,
};

// Keep the original selection thresholds, then reduce AA attenuation below.
const CORE_LEVELS: [u8; 3] = [217, 236, 255];
const FRINGE_LEVELS: [u8; 3] = [0, 43, 85];

fn quantize(value: f64, levels: &[u8]) -> u8 {
    let byte = (255. * value.clamp(0., 1.)).round() as u8;
    *levels
        .iter()
        .min_by_key(|&&v| (v.abs_diff(byte), std::cmp::Reverse(v)))
        .unwrap()
}

fn core_coverage(value: f64, intensity: GameAssetAa) -> u8 {
    // Map the strongest quantized loss (38) to 10% at full intensity,
    // rounding toward opacity. Widen before multiplying.
    // At 50% this is exactly the previous [243, 249, 255] palette.
    let loss =
        u32::from(255 - quantize(value, &CORE_LEVELS)) * u32::from(intensity.percent()) * 255
            / 38_000;
    255 - loss as u8
}

// A clean horizontal, vertical or 45-degree run (and its end caps) needs no AA.
// Judge the selected digital contour, not tiny fluctuations in fitted tangents.
fn clean(mask: &Mask, x: isize, y: isize) -> bool {
    let mut ring = 0_u8;
    for (k, &(dy, dx)) in N8.iter().enumerate() {
        if mask.at(x + dx, y + dy) {
            ring |= 1 << k;
        }
    }
    ring.count_ones() <= 1 || (ring.count_ones() == 2 && ring.rotate_left(4) == ring)
}

pub fn coverage_map(
    mask: &Mask,
    area: &[f64],
    intensity: GameAssetAa,
    cancel: &CancellationToken,
) -> Result<image::GrayImage> {
    assert_eq!(mask.data.len(), area.len());
    cancel.check()?;
    let mut thickness = vec![0.; area.len()];
    // Contiguous runs, not whole-image sums: neither the opposite side of a
    // loop nor another disconnected piece may normalize this stroke's opacity.
    for y in 0..mask.h {
        cancel.check()?;
        let mut x = 0;
        while x < mask.w {
            if area[y * mask.w + x] == 0. {
                x += 1;
                continue;
            }
            let start = x;
            let mut sum = 0.;
            while x < mask.w && area[y * mask.w + x] > 0. {
                sum += area[y * mask.w + x];
                x += 1;
            }
            thickness[y * mask.w + start..y * mask.w + x].fill(sum);
        }
    }
    for x in 0..mask.w {
        cancel.check()?;
        let mut y = 0;
        while y < mask.h {
            if area[y * mask.w + x] == 0. {
                y += 1;
                continue;
            }
            let start = y;
            let mut sum = 0.;
            while y < mask.h && area[y * mask.w + x] > 0. {
                sum += area[y * mask.w + x];
                y += 1;
            }
            for row in start..y {
                let i = row * mask.w + x;
                thickness[i] = thickness[i].min(sum);
            }
        }
    }
    let mut result = image::GrayImage::new(mask.w as u32, mask.h as u32);
    for y in 0..mask.h {
        cancel.check()?;
        for x in 0..mask.w {
            let i = y * mask.w + x;
            let normalized = if thickness[i] > 0. {
                area[i] / thickness[i]
            } else {
                0.
            };
            let byte = if mask.data[i] {
                if clean(mask, x as isize, y as isize) {
                    255
                } else {
                    core_coverage(normalized, intensity)
                }
            } else {
                let near_bend = N8.iter().any(|&(dy, dx)| {
                    let (nx, ny) = (x as isize + dx, y as isize + dy);
                    mask.at(nx, ny) && !clean(mask, nx, ny)
                });
                if near_bend {
                    // Scale fringe opacity with nearest-byte rounding.
                    ((u16::from(quantize(normalized, &FRINGE_LEVELS))
                        * u16::from(intensity.percent())
                        + 50)
                        / 100) as u8
                } else {
                    0
                }
            };
            result.put_pixel(x as u32, y as u32, image::Luma([byte]));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::scale::game_asset::{cleanup, coverage};

    #[test]
    fn core_aa_never_loses_more_than_five_percent() {
        let cancel = CancellationToken::default();
        for phase in [0., 0.125, 0.25, 0.49, 0.5, 0.875] {
            for end in [[10., 1.49], [10., 7.], [2., 9.], [10., 9.]] {
                let a = [1., 1. + phase];
                let curves = [[a, [(a[0] + end[0]) / 2., (a[1] + end[1]) / 2.], end]];
                let sampled = coverage::rasterize(&curves, 12, 12, &cancel).unwrap();
                let core = cleanup::thin(&sampled.core, &sampled.distances, &cancel).unwrap();
                assert!(core.data.iter().any(|&v| v));
                let aa =
                    coverage_map(&core, &sampled.area, GameAssetAa::default(), &cancel).unwrap();
                for (&on, &value) in core.data.iter().zip(aa.as_raw()) {
                    if on {
                        assert!(
                            value >= 243,
                            "selected core has only {value}/255 AA coverage"
                        );
                    } else {
                        assert!([0, 22, 43].contains(&value));
                    }
                }
            }
        }
        for byte in 0..=255 {
            let expected = match byte {
                0..=226 => 243,
                227..=245 => 249,
                _ => 255,
            };
            assert_eq!(
                core_coverage(f64::from(byte) / 255., GameAssetAa::default()),
                expected
            );
        }
    }

    #[test]
    fn intensity_controls_both_components_with_exact_endpoints_and_monotonicity() {
        let cancel = CancellationToken::default();
        let mut mask = Mask::new(4, 4);
        for (x, y) in [(1, 1), (2, 1), (2, 2)] {
            mask.pixel(x, y);
        }
        // Unit thickness on each occupied row and column.
        let mut area = vec![0.; 16];
        for i in [5, 6, 9, 10] {
            area[i] = 0.5;
        }
        let mut previous_core = 255;
        let mut previous_fringe = 0;
        for percent in 0..=100 {
            let intensity = GameAssetAa::new(percent);
            let aa = coverage_map(&mask, &area, intensity, &cancel).unwrap();
            // Independent floor: ceil(255 * (1 - 0.1 * percent/100)).
            let expected_core = 255 - (255 * u32::from(percent) / 1000) as u8;
            let expected_fringe = (85. * f64::from(percent) / 100.).round() as u8;
            assert_eq!(aa.get_pixel(1, 1)[0], expected_core);
            assert_eq!(aa.get_pixel(1, 2)[0], expected_fringe);
            assert!(expected_core <= previous_core);
            assert!(expected_fringe >= previous_fringe);
            for byte in 0..=255 {
                let value = core_coverage(f64::from(byte) / 255., intensity);
                assert!(value >= expected_core);
                if byte >= 246 || percent == 0 {
                    assert_eq!(value, 255);
                }
            }
            previous_core = expected_core;
            previous_fringe = expected_fringe;
        }
        assert_eq!((previous_core, previous_fringe), (230, 85));
        assert_eq!(GameAssetAa::default().percent(), 50);
        assert_eq!(GameAssetAa::new(255).percent(), 100);
    }

    #[test]
    fn fringe_opacity_is_halved_without_moving_quantization_thresholds() {
        let cancel = CancellationToken::default();
        let mut mask = Mask::new(4, 4);
        for (x, y) in [(1, 1), (2, 1), (2, 2)] {
            mask.pixel(x, y);
        }
        // The fringe at (1,2) has unit row and column thickness, so its
        // normalized coverage is exactly the supplied sample (including ends).
        for byte in 0..=255 {
            let sample = f64::from(byte) / 255.;
            let mut area = vec![0.; 16];
            area[5] = 1. - sample;
            area[9] = sample;
            area[10] = 1. - sample;
            let aa = coverage_map(&mask, &area, GameAssetAa::default(), &cancel).unwrap();
            // Independent old thresholds, followed by half-opacity rounding.
            let expected = match byte {
                0..=21 => 0,
                22..=63 => 22,
                _ => 43,
            };
            assert_eq!(aa.get_pixel(1, 2)[0], expected, "input byte {byte}");
        }
    }

    #[test]
    fn clean_grid_runs_remain_fully_opaque_without_fringe() {
        let cancel = CancellationToken::default();
        for (start, step) in [
            ([2, 2], [1, 0]),
            ([2, 2], [0, 1]),
            ([2, 2], [1, 1]),
            ([2, 7], [1, -1]),
        ] {
            let mut mask = Mask::new(10, 10);
            for k in 0..6 {
                mask.pixel(start[0] + k * step[0], start[1] + k * step[1]);
            }
            let area = mask
                .data
                .iter()
                .map(|&v| if v { 0.5 } else { 0.1 })
                .collect::<Vec<_>>();
            let aa = coverage_map(&mask, &area, GameAssetAa::default(), &cancel).unwrap();
            for (&on, &value) in mask.data.iter().zip(aa.as_raw()) {
                assert_eq!(value, if on { 255 } else { 0 });
            }
        }
    }

    #[test]
    fn normalization_is_local_and_cancellation_is_observed() {
        let cancel = CancellationToken::default();
        let mut mask = Mask::new(12, 6);
        for (x, y) in [(2, 2), (3, 2), (4, 3), (5, 3)] {
            mask.pixel(x, y);
        }
        let mut area = vec![0.; 72];
        for (i, on) in mask.data.iter().enumerate() {
            if *on {
                area[i] = 0.5;
            }
        }
        area[2 * 12 + 4] = 0.25;
        let a = coverage_map(&mask, &area, GameAssetAa::default(), &cancel).unwrap();
        area[2 * 12 + 10] = 1.;
        let b = coverage_map(&mask, &area, GameAssetAa::default(), &cancel).unwrap();
        for y in 0..6 {
            for x in 0..7 {
                assert_eq!(a.get_pixel(x, y), b.get_pixel(x, y));
            }
        }
        cancel.cancel();
        assert!(matches!(
            coverage_map(&mask, &area, GameAssetAa::default(), &cancel),
            Err(crate::error::AppError::Cancelled)
        ));
    }
}
