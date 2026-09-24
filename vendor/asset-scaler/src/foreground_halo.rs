//! Repair low-alpha foreground fringe pixels next to a frozen contour core.
//!
//! Extracted foregrounds can retain a little of an opaque source canvas at a
//! transparent boundary.  The usual rim deliberately excludes those weak
//! samples.  This pass changes their displayed RGB only; it never adds paint,
//! changes alpha, or changes contour ownership.

use crate::{
    Cancellation, Error, Result,
    color::{LinearImage, rgba},
};
use image::{GrayImage, RgbaImage};

const MAX_WEAK_ALPHA: f64 = 0.25;
// Source-core fringes can survive two target pixels past the visible core.
// Donors remain frozen original core pixels, so this does not chain cleanup
// through repaired exterior samples.
const CORE_RADIUS: isize = 2;

pub struct Inputs<'a> {
    pub support: &'a [bool],
    pub original_is_opaque: bool,
    pub fill: &'a LinearImage,
    pub baseline: RgbaImage,
    pub core: &'a GrayImage,
    pub coverage: &'a GrayImage,
    pub owners: &'a [Option<usize>],
    pub colors: &'a [[f64; 3]],
}

/// Replace only the RGB of weak, unsupported exterior samples near an
/// original core.  Donors are frozen core pixels, ranked consistently with
/// the rim repair.  The baseline alpha byte remains exact.
pub fn clean(input: Inputs<'_>, cancel: &dyn Cancellation) -> Result<RgbaImage> {
    let Inputs {
        support,
        original_is_opaque,
        fill,
        mut baseline,
        core,
        coverage,
        owners,
        colors,
    } = input;
    let (w, h) = (core.width() as usize, core.height() as usize);
    let len = w.saturating_mul(h);
    if baseline.dimensions() != core.dimensions()
        || coverage.dimensions() != core.dimensions()
        || (fill.w, fill.h) != (w, h)
        || support.len() != len
        || owners.len() != len
        || colors.len() != len
    {
        return Err(Error::Scaling("Invalid foreground halo inputs".into()));
    }
    cancel.check()?;
    if !original_is_opaque {
        return Ok(baseline);
    }
    let geometry = Geometry {
        core,
        coverage,
        owners,
    };
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if i.is_multiple_of(4096) {
                cancel.check()?;
            }
            let alpha = fill.pixels[i][3];
            if support[i]
                || !(0. < alpha && alpha < MAX_WEAK_ALPHA)
                || geometry.core.as_raw()[i] != 0
            {
                continue;
            }
            let Some(donor) = nearest_core(&geometry, w, h, x, y) else {
                continue;
            };
            let bytes = &mut baseline.as_mut()[i * 4..i * 4 + 4];
            if bytes[3] == 0 {
                bytes.fill(0);
            } else {
                let color = rgba([colors[donor][0], colors[donor][1], colors[donor][2], 1.]);
                bytes[..3].copy_from_slice(&color.0[..3]);
            }
        }
    }
    cancel.check()?;
    Ok(baseline)
}

struct Geometry<'a> {
    core: &'a GrayImage,
    coverage: &'a GrayImage,
    owners: &'a [Option<usize>],
}

fn nearest_core(geometry: &Geometry<'_>, w: usize, h: usize, x: usize, y: usize) -> Option<usize> {
    let mut donor = None;
    for dy in -CORE_RADIUS..=CORE_RADIUS {
        for dx in -CORE_RADIUS..=CORE_RADIUS {
            if dx == 0 && dy == 0 {
                continue;
            }
            let xx = x as isize + dx;
            let yy = y as isize + dy;
            if xx < 0 || yy < 0 || xx >= w as isize || yy >= h as isize {
                continue;
            }
            let j = yy as usize * w + xx as usize;
            let Some(owner) = geometry.owners[j] else {
                continue;
            };
            if geometry.core.as_raw()[j] == 0 {
                continue;
            }
            let rank = (
                dx * dx + dy * dy,
                std::cmp::Reverse(geometry.coverage.as_raw()[j]),
                owner,
                j,
            );
            if donor.is_none_or(|(best, _)| rank < best) {
                donor = Some((rank, j));
            }
        }
    }
    donor.map(|(_, index)| index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CancellationToken, color};
    use image::{Luma, Rgba};

    type InputParts = (
        Vec<bool>,
        LinearImage,
        RgbaImage,
        GrayImage,
        GrayImage,
        Vec<Option<usize>>,
        Vec<[f64; 3]>,
    );

    fn inputs(w: u32, h: u32) -> InputParts {
        let len = w as usize * h as usize;
        (
            vec![true; len],
            LinearImage {
                w: w as usize,
                h: h as usize,
                pixels: vec![[0., 0., 0., 1.]; len],
            },
            RgbaImage::from_pixel(w, h, Rgba([12, 34, 56, 255])),
            GrayImage::new(w, h),
            GrayImage::new(w, h),
            vec![None; len],
            vec![[0.; 3]; len],
        )
    }

    #[test]
    fn turns_an_unsupported_red_fringe_green_without_changing_alpha() {
        let (mut support, mut fill, mut baseline, mut core, mut coverage, mut owners, mut colors) =
            inputs(5, 1);
        support[2] = false;
        fill.pixels[2][3] = 0.1;
        baseline.put_pixel(2, 0, Rgba([240, 20, 10, 29]));
        // The donor radius is two pixels.  A second weak unsupported sample
        // three pixels from this core remains untouched.
        support[3] = false;
        fill.pixels[3][3] = 0.1;
        baseline.put_pixel(3, 0, Rgba([210, 30, 20, 29]));
        core.put_pixel(0, 0, Luma([255]));
        coverage.put_pixel(0, 0, Luma([200]));
        owners[0] = Some(4);
        colors[0] = [color::decode(0.), color::decode(0.8), color::decode(0.)];
        let result = clean(
            Inputs {
                support: &support,
                original_is_opaque: true,
                fill: &fill,
                baseline,
                core: &core,
                coverage: &coverage,
                owners: &owners,
                colors: &colors,
            },
            &CancellationToken::default(),
        )
        .unwrap();
        assert_eq!(*result.get_pixel(2, 0), Rgba([0, 204, 0, 29]));
        assert_eq!(*result.get_pixel(3, 0), Rgba([210, 30, 20, 29]));
    }

    #[test]
    fn preserves_supported_highlights_cores_and_unrelated_pixels() {
        let (mut support, mut fill, mut baseline, mut core, mut coverage, mut owners, mut colors) =
            inputs(6, 1);
        support[1] = false;
        fill.pixels[1][3] = 0.1;
        baseline.put_pixel(1, 0, Rgba([230, 20, 10, 31]));
        core.put_pixel(3, 0, Luma([255]));
        coverage.put_pixel(3, 0, Luma([255]));
        owners[3] = Some(2);
        colors[3] = [0., 0.5, 0.];
        // A weak, pale interior highlight is supported, so it is not a fringe.
        fill.pixels[4][3] = 0.1;
        baseline.put_pixel(4, 0, Rgba([250, 240, 220, 26]));
        let before = baseline.clone();
        let result = clean(
            Inputs {
                support: &support,
                original_is_opaque: true,
                fill: &fill,
                baseline,
                core: &core,
                coverage: &coverage,
                owners: &owners,
                colors: &colors,
            },
            &CancellationToken::default(),
        )
        .unwrap();
        assert_eq!(result.get_pixel(1, 0)[3], before.get_pixel(1, 0)[3]);
        assert_eq!(*result.get_pixel(3, 0), *before.get_pixel(3, 0));
        assert_eq!(*result.get_pixel(4, 0), *before.get_pixel(4, 0));
        assert_eq!(*result.get_pixel(5, 0), *before.get_pixel(5, 0));
    }

    #[test]
    fn clears_hidden_rgb_and_skips_nonopaque_originals() {
        let (mut support, mut fill, mut baseline, mut core, mut coverage, mut owners, mut colors) =
            inputs(3, 1);
        support[1] = false;
        // A nonzero linear alpha can round to zero in the baseline RGBA
        // image.  Its hidden RGB must not survive cleanup.
        fill.pixels[1][3] = 0.001;
        baseline.put_pixel(1, 0, Rgba([240, 20, 10, 0]));
        core.put_pixel(0, 0, Luma([255]));
        coverage.put_pixel(0, 0, Luma([255]));
        owners[0] = Some(0);
        colors[0] = [0., 0.8, 0.];
        let opaque = clean(
            Inputs {
                support: &support,
                original_is_opaque: true,
                fill: &fill,
                baseline: baseline.clone(),
                core: &core,
                coverage: &coverage,
                owners: &owners,
                colors: &colors,
            },
            &CancellationToken::default(),
        )
        .unwrap();
        assert_eq!(*opaque.get_pixel(1, 0), Rgba([0; 4]));

        let transparent = clean(
            Inputs {
                support: &support,
                original_is_opaque: false,
                fill: &fill,
                baseline: baseline.clone(),
                core: &core,
                coverage: &coverage,
                owners: &owners,
                colors: &colors,
            },
            &CancellationToken::default(),
        )
        .unwrap();
        assert_eq!(transparent, baseline);
    }

    #[test]
    fn cancellation_and_invalid_inputs_fail() {
        let (support, fill, baseline, core, coverage, owners, colors) = inputs(2, 2);
        let cancel = CancellationToken::default();
        cancel.cancel();
        assert!(matches!(
            clean(
                Inputs {
                    support: &support,
                    original_is_opaque: true,
                    fill: &fill,
                    baseline,
                    core: &core,
                    coverage: &coverage,
                    owners: &owners,
                    colors: &colors
                },
                &cancel
            ),
            Err(Error::Cancelled)
        ));

        let (support, fill, _baseline, core, coverage, owners, colors) = inputs(2, 2);
        assert!(matches!(
            clean(
                Inputs {
                    support: &support,
                    original_is_opaque: true,
                    fill: &fill,
                    baseline: RgbaImage::new(1, 1),
                    core: &core,
                    coverage: &coverage,
                    owners: &owners,
                    colors: &colors,
                },
                &CancellationToken::default(),
            ),
            Err(Error::Scaling(_))
        ));
    }
}
