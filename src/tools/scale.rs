use image::{Rgba, RgbaImage};

use crate::document::CancellationToken;
use crate::error::{AppError, Result};

pub fn seam_carve(
    image: &RgbaImage,
    target_width: u32,
    target_height: u32,
    cancellation: &CancellationToken,
) -> Result<RgbaImage> {
    if target_width == 0
        || target_height == 0
        || target_width > image.width()
        || target_height > image.height()
    {
        return Err(AppError::InvalidDimensions);
    }
    let mut output = image.clone();
    while output.width() > target_width {
        cancellation.check()?;
        output = remove_vertical_seam(&output)?;
    }
    if output.height() > target_height {
        output = image::imageops::rotate90(&output);
        while output.width() > target_height {
            cancellation.check()?;
            output = remove_vertical_seam(&output)?;
        }
        output = image::imageops::rotate270(&output);
    }
    Ok(output)
}

fn remove_vertical_seam(image: &RgbaImage) -> Result<RgbaImage> {
    let (width, height) = image.dimensions();
    if width <= 1 {
        return Err(AppError::InvalidDimensions);
    }
    let len = usize::try_from(u64::from(width) * u64::from(height))
        .map_err(|_| AppError::InvalidDimensions)?;
    let mut costs = vec![0_u64; len];
    let mut parents = vec![0_i8; len];

    for y in 0..height {
        for x in 0..width {
            let pixel_index = index(width, x, y)?;
            let energy = u64::from(energy(image, x, y));
            if y == 0 {
                costs[pixel_index] = energy;
                continue;
            }
            let mut best = (costs[index(width, x, y - 1)?], 0_i8);
            if x > 0 {
                let candidate = (costs[index(width, x - 1, y - 1)?], -1);
                if candidate.0 < best.0 {
                    best = candidate;
                }
            }
            if x + 1 < width {
                let candidate = (costs[index(width, x + 1, y - 1)?], 1);
                if candidate.0 < best.0 {
                    best = candidate;
                }
            }
            costs[pixel_index] = best.0.saturating_add(energy);
            parents[pixel_index] = best.1;
        }
    }

    let last_y = height - 1;
    let mut seam_x = (0..width)
        .min_by_key(|x| costs[index(width, *x, last_y).unwrap_or(0)])
        .unwrap_or(0);
    let mut seam = vec![0_u32; usize::try_from(height).map_err(|_| AppError::InvalidDimensions)?];
    for y in (0..height).rev() {
        seam[usize::try_from(y).map_err(|_| AppError::InvalidDimensions)?] = seam_x;
        if y > 0 {
            let parent = parents[index(width, seam_x, y)?];
            seam_x = seam_x.saturating_add_signed(i32::from(parent));
        }
    }

    let mut output = RgbaImage::new(width - 1, height);
    for y in 0..height {
        let skip = seam[usize::try_from(y).map_err(|_| AppError::InvalidDimensions)?];
        for x in 0..width - 1 {
            let source_x = if x < skip { x } else { x + 1 };
            output.put_pixel(x, y, *image.get_pixel(source_x, y));
        }
    }
    Ok(output)
}

fn index(width: u32, x: u32, y: u32) -> Result<usize> {
    usize::try_from(u64::from(y) * u64::from(width) + u64::from(x))
        .map_err(|_| AppError::InvalidDimensions)
}

fn energy(image: &RgbaImage, x: u32, y: u32) -> u32 {
    let left = image.get_pixel(x.saturating_sub(1), y);
    let right = image.get_pixel((x + 1).min(image.width() - 1), y);
    let above = image.get_pixel(x, y.saturating_sub(1));
    let below = image.get_pixel(x, (y + 1).min(image.height() - 1));
    gradient(left, right) + gradient(above, below)
}

fn gradient(left: &Rgba<u8>, right: &Rgba<u8>) -> u32 {
    left.0[..3]
        .iter()
        .zip(&right.0[..3])
        .map(|(a, b)| i32::from(*a) - i32::from(*b))
        .map(|value| value.unsigned_abs().pow(2))
        .sum()
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::seam_carve;
    use crate::document::CancellationToken;

    #[test]
    fn shrinks_both_axes() {
        let image = RgbaImage::from_pixel(5, 4, Rgba([1, 2, 3, 255]));
        let output = seam_carve(&image, 3, 2, &CancellationToken::default()).unwrap();
        assert_eq!(output.dimensions(), (3, 2));
    }
}
