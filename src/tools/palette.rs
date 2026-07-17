use std::cmp::Ordering;
use std::collections::HashMap;

use image::{Rgba, RgbaImage};
use palette::{FromColor, LinSrgb, Oklab, Srgb};

use crate::document::{CancellationToken, ProtectedColor};
use crate::error::{AppError, Result};

#[derive(Debug, Clone, Copy)]
struct Sample {
    rgba: [u8; 4],
    lab: Oklab,
    weight: u32,
}

pub fn reduce_palette(
    image: &RgbaImage,
    color_count: u16,
    dithering: bool,
    preserve_accents: bool,
    protected: &[ProtectedColor],
    cancellation: &CancellationToken,
) -> Result<RgbaImage> {
    if !(2..=256).contains(&color_count) {
        return Err(AppError::InvalidDimensions);
    }
    let histogram = histogram(image);
    let samples: Vec<_> = histogram
        .into_iter()
        .map(|(rgba, weight)| Sample {
            rgba,
            lab: to_lab(rgba),
            weight,
        })
        .collect();
    if samples.is_empty() {
        return Ok(image.clone());
    }

    let requested = usize::from(color_count).min(samples.len().max(1));
    let mut fixed: Vec<[u8; 4]> = protected.iter().map(|color| color.0).collect();
    fixed.sort_unstable();
    fixed.dedup();
    fixed.truncate(requested);

    if preserve_accents && fixed.len() < requested {
        let reserve = (requested / 4).clamp(1, 8);
        let mut accents: Vec<_> = samples
            .iter()
            .filter(|sample| {
                sample.lab.a.hypot(sample.lab.b) > 0.12
                    && sample.weight > 1
                    && u64::from(sample.weight) * 50
                        < u64::from(image.width()) * u64::from(image.height())
            })
            .collect();
        accents.sort_by(|left, right| {
            right
                .lab
                .a
                .hypot(right.lab.b)
                .partial_cmp(&left.lab.a.hypot(left.lab.b))
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.rgba.cmp(&right.rgba))
        });
        for accent in accents.into_iter().take(reserve) {
            if !fixed.contains(&accent.rgba) && fixed.len() < requested {
                fixed.push(accent.rgba);
            }
        }
    }

    let movable_count = requested.saturating_sub(fixed.len());
    let mut centroids = seed_centroids(&samples, movable_count, &fixed);
    for _ in 0..12 {
        cancellation.check()?;
        let mut sums = vec![(0.0_f64, 0.0_f64, 0.0_f64, 0_u64); centroids.len()];
        for sample in &samples {
            let index = nearest_lab(sample.lab, &centroids);
            let weight = u64::from(sample.weight);
            sums[index].0 += f64::from(sample.lab.l) * weight as f64;
            sums[index].1 += f64::from(sample.lab.a) * weight as f64;
            sums[index].2 += f64::from(sample.lab.b) * weight as f64;
            sums[index].3 += weight;
        }
        let mut changed = false;
        for (centroid, sum) in centroids.iter_mut().zip(sums) {
            if sum.3 == 0 {
                continue;
            }
            let next = Oklab::new(
                (sum.0 / sum.3 as f64) as f32,
                (sum.1 / sum.3 as f64) as f32,
                (sum.2 / sum.3 as f64) as f32,
            );
            changed |= distance(*centroid, next) > 0.000_001;
            *centroid = next;
        }
        if !changed {
            break;
        }
    }

    let mut palette = fixed;
    palette.extend(centroids.into_iter().map(from_lab));
    palette.sort_unstable();
    palette.dedup();
    if palette.is_empty() {
        palette.push(samples[0].rgba);
    }

    if dithering {
        dither(image, &palette, protected, cancellation)
    } else {
        map_pixels(image, &palette, protected, cancellation)
    }
}

fn histogram(image: &RgbaImage) -> HashMap<[u8; 4], u32> {
    let pixel_count = u64::from(image.width()) * u64::from(image.height());
    let stride = ((pixel_count / 65_536).max(1)) as usize;
    let mut histogram = HashMap::new();
    for pixel in image.pixels().step_by(stride) {
        let key = [
            pixel.0[0] & 0b1111_1000,
            pixel.0[1] & 0b1111_1000,
            pixel.0[2] & 0b1111_1000,
            pixel.0[3],
        ];
        *histogram.entry(key).or_insert(0) += 1;
    }
    histogram
}

fn seed_centroids(samples: &[Sample], count: usize, fixed: &[[u8; 4]]) -> Vec<Oklab> {
    if count == 0 {
        return Vec::new();
    }
    let mut centroids = Vec::with_capacity(count);
    let first = samples
        .iter()
        .max_by_key(|sample| (sample.weight, std::cmp::Reverse(sample.rgba)))
        .map_or_else(|| Oklab::new(0.0, 0.0, 0.0), |sample| sample.lab);
    centroids.push(first);
    let fixed_labs: Vec<_> = fixed.iter().copied().map(to_lab).collect();

    while centroids.len() < count {
        let next = samples.iter().max_by(|left, right| {
            let left_distance = distance_to_set(left.lab, &centroids, &fixed_labs);
            let right_distance = distance_to_set(right.lab, &centroids, &fixed_labs);
            (left_distance * left.weight as f32)
                .partial_cmp(&(right_distance * right.weight as f32))
                .unwrap_or(Ordering::Equal)
                .then_with(|| right.rgba.cmp(&left.rgba))
        });
        centroids.push(next.map_or(first, |sample| sample.lab));
    }
    centroids
}

fn distance_to_set(color: Oklab, centroids: &[Oklab], fixed: &[Oklab]) -> f32 {
    centroids
        .iter()
        .chain(fixed)
        .map(|other| distance(color, *other))
        .fold(f32::INFINITY, f32::min)
}

fn nearest_lab(color: Oklab, centroids: &[Oklab]) -> usize {
    centroids
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            distance(color, **left)
                .partial_cmp(&distance(color, **right))
                .unwrap_or(Ordering::Equal)
        })
        .map_or(0, |(index, _)| index)
}

fn nearest_rgba(color: [u8; 4], palette: &[[u8; 4]]) -> [u8; 4] {
    let lab = to_lab(color);
    palette
        .iter()
        .min_by(|left, right| {
            distance(lab, to_lab(**left))
                .partial_cmp(&distance(lab, to_lab(**right)))
                .unwrap_or(Ordering::Equal)
        })
        .copied()
        .unwrap_or(color)
}

fn map_pixels(
    image: &RgbaImage,
    palette: &[[u8; 4]],
    protected: &[ProtectedColor],
    cancellation: &CancellationToken,
) -> Result<RgbaImage> {
    let mut output = image.clone();
    for (index, pixel) in output.pixels_mut().enumerate() {
        if index % 16_384 == 0 {
            cancellation.check()?;
        }
        if protected.iter().any(|color| color.0 == pixel.0) {
            continue;
        }
        let alpha = pixel.0[3];
        let mut mapped = nearest_rgba(pixel.0, palette);
        mapped[3] = alpha;
        *pixel = Rgba(mapped);
    }
    Ok(output)
}

fn dither(
    image: &RgbaImage,
    palette: &[[u8; 4]],
    protected: &[ProtectedColor],
    cancellation: &CancellationToken,
) -> Result<RgbaImage> {
    let (width, height) = image.dimensions();
    let len = usize::try_from(u64::from(width) * u64::from(height))
        .map_err(|_| AppError::InvalidDimensions)?;
    let mut working: Vec<[f32; 3]> = image
        .pixels()
        .map(|pixel| pixel.0)
        .map(|color| {
            [
                f32::from(color[0]),
                f32::from(color[1]),
                f32::from(color[2]),
            ]
        })
        .collect();
    let mut output = image.clone();
    for y in 0..height {
        cancellation.check()?;
        for x in 0..width {
            let index = usize::try_from(u64::from(y) * u64::from(width) + u64::from(x))
                .map_err(|_| AppError::InvalidDimensions)?;
            let original = image.get_pixel(x, y).0;
            if protected.iter().any(|color| color.0 == original) {
                output.put_pixel(x, y, Rgba(original));
                continue;
            }
            let current = [
                working[index][0].clamp(0.0, 255.0) as u8,
                working[index][1].clamp(0.0, 255.0) as u8,
                working[index][2].clamp(0.0, 255.0) as u8,
                original[3],
            ];
            let mut mapped = nearest_rgba(current, palette);
            mapped[3] = original[3];
            output.put_pixel(x, y, Rgba(mapped));
            let error = [
                working[index][0] - f32::from(mapped[0]),
                working[index][1] - f32::from(mapped[1]),
                working[index][2] - f32::from(mapped[2]),
            ];
            diffuse(&mut working, width, height, x + 1, y, error, 7.0 / 16.0);
            if x > 0 {
                diffuse(&mut working, width, height, x - 1, y + 1, error, 3.0 / 16.0);
            }
            diffuse(&mut working, width, height, x, y + 1, error, 5.0 / 16.0);
            diffuse(&mut working, width, height, x + 1, y + 1, error, 1.0 / 16.0);
        }
    }
    debug_assert_eq!(working.len(), len);
    Ok(output)
}

fn diffuse(
    working: &mut [[f32; 3]],
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    error: [f32; 3],
    amount: f32,
) {
    if x >= width || y >= height {
        return;
    }
    let Ok(index) = usize::try_from(u64::from(y) * u64::from(width) + u64::from(x)) else {
        return;
    };
    for channel in 0..3 {
        working[index][channel] += error[channel] * amount;
    }
}

fn to_lab(rgba: [u8; 4]) -> Oklab {
    Oklab::from_color(
        Srgb::new(
            f32::from(rgba[0]) / 255.0,
            f32::from(rgba[1]) / 255.0,
            f32::from(rgba[2]) / 255.0,
        )
        .into_linear(),
    )
}

fn from_lab(lab: Oklab) -> [u8; 4] {
    let linear: LinSrgb = LinSrgb::from_color(lab);
    let rgb: Srgb = Srgb::from_linear(linear);
    [
        (rgb.red.clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgb.green.clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgb.blue.clamp(0.0, 1.0) * 255.0).round() as u8,
        255,
    ]
}

fn distance(left: Oklab, right: Oklab) -> f32 {
    (left.l - right.l).powi(2) + (left.a - right.a).powi(2) + (left.b - right.b).powi(2)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use image::{Rgba, RgbaImage};

    use super::reduce_palette;
    use crate::document::{CancellationToken, ProtectedColor};

    #[test]
    fn result_is_deterministic_and_preserves_protected_color() {
        let image = RgbaImage::from_fn(20, 20, |x, y| {
            if x == 10 && y == 10 {
                Rgba([255, 0, 255, 255])
            } else {
                Rgba([(x * 10) as u8, (y * 10) as u8, 80, 255])
            }
        });
        let protected = [ProtectedColor([255, 0, 255, 255])];
        let first = reduce_palette(
            &image,
            8,
            false,
            true,
            &protected,
            &CancellationToken::default(),
        )
        .unwrap();
        let second = reduce_palette(
            &image,
            8,
            false,
            true,
            &protected,
            &CancellationToken::default(),
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.get_pixel(10, 10).0, protected[0].0);
        let colors: HashSet<_> = first.pixels().map(|pixel| pixel.0).collect();
        assert!(colors.len() <= 8);
    }
}
