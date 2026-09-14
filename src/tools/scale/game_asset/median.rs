//! Alpha-weighted median in the positive Catmull-Rom footprint, with retained
//! source ink excluded only in the one-pixel neighborhood of actual paint.
use super::{color::LinearImage, raster::Mask};
use crate::{document::CancellationToken, error::Result};
use image::GrayImage;
fn kernel(x: f64) -> f64 {
    let x = x.abs();
    if x < 1. {
        1.5 * x * x * x - 2.5 * x * x + 1.
    } else if x < 2. {
        -0.5 * x * x * x + 2.5 * x * x - 4. * x + 2.
    } else {
        0.
    }
}
fn taps(source: usize, target: usize) -> Vec<Vec<(usize, f64)>> {
    let scale = source as f64 / target as f64;
    (0..target)
        .map(|i| {
            let center = (i as f64 + 0.5) * scale - 0.5;
            let lo = (center - 2. * scale).ceil().max(0.) as usize;
            let hi = ((center + 2. * scale).floor() as usize).min(source - 1);
            let mut taps: Vec<_> = (lo..=hi)
                .map(|j| (j, kernel((j as f64 - center) / scale)))
                .collect();
            let sum: f64 = taps.iter().map(|p| p.1).sum();
            for (_, w) in &mut taps {
                *w /= sum;
            }
            taps
        })
        .collect()
}
fn median(samples: &[([f64; 3], f64)], total: f64, scratch: &mut Vec<(f64, f64)>) -> [f64; 3] {
    let threshold = total * 0.5 - total * f64::EPSILON * samples.len() as f64;
    std::array::from_fn(|c| {
        scratch.clear();
        scratch.extend(samples.iter().map(|(rgb, w)| (rgb[c], *w)));
        scratch.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut weight = 0.;
        for &(value, w) in scratch.iter() {
            weight += w;
            if weight >= threshold {
                return value;
            }
        }
        scratch.last().unwrap().0
    })
}
pub fn project(
    source: &LinearImage,
    mask: &Mask,
    coverage: &GrayImage,
    cancel: &CancellationToken,
) -> Result<LinearImage> {
    let (w, h) = (coverage.width() as usize, coverage.height() as usize);
    let xt = taps(source.w, w);
    let yt = taps(source.h, h);
    let mut pixels = Vec::with_capacity(w * h);
    let mut original = Vec::new();
    let mut remaining = Vec::new();
    let mut scratch = Vec::new();
    for (ty, ys) in yt.iter().enumerate() {
        cancel.check()?;
        for (tx, xs) in xt.iter().enumerate() {
            let halo = coverage.get_pixel(tx as u32, ty as u32)[0] < 255
                && (ty.saturating_sub(1)..=(ty + 1).min(h - 1)).any(|y| {
                    (tx.saturating_sub(1)..=(tx + 1).min(w - 1))
                        .any(|x| coverage.get_pixel(x as u32, y as u32)[0] > 0)
                });
            original.clear();
            remaining.clear();
            let mut alpha = 0.;
            let mut original_mass = 0.;
            let mut remaining_mass = 0.;
            for &(y, wy) in ys {
                cancel.check()?;
                for &(x, wx) in xs {
                    let i = y * source.w + x;
                    let p = source.pixels[i];
                    let weight = wx * wy * p[3];
                    alpha += weight;
                    if wx > 0. && wy > 0. && weight > 0. {
                        let sample = ([p[0], p[1], p[2]], weight);
                        original.push(sample);
                        original_mass += weight;
                        if !halo || !mask.data[i] {
                            remaining.push(sample);
                            remaining_mass += weight;
                        }
                    }
                }
            }
            alpha = alpha.clamp(0., 1.);
            if alpha <= 1e-8 || original_mass <= 1e-8 {
                pixels.push([0.; 4]);
                continue;
            }
            let rgb = if remaining_mass > 1e-8 {
                median(&remaining, remaining_mass, &mut scratch)
            } else {
                median(&original, original_mass, &mut scratch)
            };
            pixels.push([rgb[0], rgb[1], rgb[2], alpha]);
        }
    }
    Ok(LinearImage { w, h, pixels })
}
