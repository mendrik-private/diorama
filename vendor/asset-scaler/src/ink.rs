use crate::{Cancellation, Result};
use crate::{detect::Sample, field::Spatial, raster::Mask};
use image::RgbaImage;

fn luminance(c: [f64; 3]) -> f64 {
    c[0] * 0.2126 + c[1] * 0.7152 + c[2] * 0.0722
}

/// Hidden RGB, including near-transparent red pixels, is never a color donor.
pub(crate) fn color(source: &RgbaImage, x: f64, y: f64) -> Option<[f64; 3]> {
    let mut sum = [0.; 3];
    let mut weight = 0.;
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    for dy in 0..2 {
        for dx in 0..2 {
            let (px, py) = (ix + dx, iy + dy);
            if px < 0 || py < 0 || px >= source.width() as i32 || py >= source.height() as i32 {
                continue;
            }
            let p = source.get_pixel(px as u32, py as u32).0;
            if p[3] < 128 {
                continue;
            }
            let w =
                (1. - (x - px as f64).abs()) * (1. - (y - py as f64).abs()) * p[3] as f64 / 255.;
            for c in 0..3 {
                sum[c] += w * p[c] as f64 / 255.;
            }
            weight += w;
        }
    }
    (weight > 1e-8).then(|| sum.map(|v| v / weight))
}

fn shoulder(source: &RgbaImage, p: &Sample, sign: f64, center: f64) -> Option<(f64, [f64; 3])> {
    let reach = (4. * p[7]).clamp(4., 10.);
    let mut profile = Vec::new();
    for i in 1..=(reach * 2.) as usize {
        let d = i as f64 * 0.5;
        if let Some(c) = color(source, p[0] + sign * d * p[2], p[1] + sign * d * p[3]) {
            profile.push((d, c));
        } else {
            break;
        } // do not jump across a transparent gap to another part
    }
    let peak = profile
        .iter()
        .map(|(_, c)| luminance(*c))
        .fold(center, f64::max);
    if peak - center < 0.045 {
        return None;
    }
    profile
        .into_iter()
        .find(|(_, c)| luminance(*c) >= center + 0.8 * (peak - center))
}

pub fn ink_mask(
    source: &RgbaImage,
    samples: &[Sample],
    support: &Mask,
    cancel: &dyn Cancellation,
) -> Result<Mask> {
    let (w, h) = (support.w, support.h);
    let supported = Spatial::new(
        support
            .data
            .iter()
            .enumerate()
            .filter(|&(_, v)| *v)
            .map(|(i, _)| [(i % w) as f64, (i / w) as f64])
            .collect(),
        2.,
    );
    let mut mask = support.clone();
    for p in samples {
        cancel.check()?;
        // Source cleanup, not a texture detector, decides which ridges to erase.
        if !supported
            .nearest([p[0], p[1]])
            .is_some_and(|(d, _)| d <= 1.25)
        {
            continue;
        }
        let Some(center) = color(source, p[0], p[1]) else {
            continue;
        };
        let lc = luminance(center);
        let left = shoulder(source, p, -1., lc);
        let right = shoulder(source, p, 1., lc);
        if left.is_none() && right.is_none() {
            continue;
        }
        let reach = left
            .map_or(0., |s| s.0)
            .max(right.map_or(0., |s| s.0))
            .ceil() as i32
            + 1;
        for y in (p[1] as i32 - reach).max(0)..=(p[1] as i32 + reach).min(h as i32 - 1) {
            for x in (p[0] as i32 - reach).max(0)..=(p[0] as i32 + reach).min(w as i32 - 1) {
                let delta = [x as f64 - p[0], y as f64 - p[1]];
                let u = -delta[0] * p[3] + delta[1] * p[2];
                let v = delta[0] * p[2] + delta[1] * p[3];
                if u.abs() > 1.0 {
                    continue;
                }
                let chosen = if v < 0. {
                    left.or(right)
                } else {
                    right.or(left)
                };
                let Some((width, donor)) = chosen else {
                    continue;
                };
                if v.abs() > width {
                    continue;
                }
                let original = source.get_pixel(x as u32, y as u32).0;
                if original[3] < 38 {
                    continue;
                }
                let l = luminance([
                    original[0] as f64 / 255.,
                    original[1] as f64 / 255.,
                    original[2] as f64 / 255.,
                ]);
                // Avoid erasing the bright shoulders and surrounding texture.
                if l >= lc + 0.8 * (luminance(donor) - lc) || luminance(donor) - l < 0.025 {
                    continue;
                }
                mask.data[y as usize * w + x as usize] = true;
            }
        }
    }
    Ok(mask)
}
