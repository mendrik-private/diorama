use std::collections::VecDeque;

use image::RgbaImage;

use crate::error::{AppError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CropBounds {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub fn alpha_content_bounds(image: &RgbaImage, threshold: u8) -> Result<CropBounds> {
    let mut min_x = image.width();
    let mut min_y = image.height();
    let mut max_x = 0;
    let mut max_y = 0;
    let mut found = false;

    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel.0[3] > threshold {
            found = true;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }

    if !found {
        return Err(AppError::NoVisibleContent);
    }

    Ok(CropBounds {
        x: min_x,
        y: min_y,
        width: max_x - min_x + 1,
        height: max_y - min_y + 1,
    })
}

pub fn opaque_content_bounds(image: &RgbaImage, tolerance: u8) -> Result<Option<CropBounds>> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Err(AppError::InvalidDimensions);
    }

    let corners = [
        image.get_pixel(0, 0).0,
        image.get_pixel(width - 1, 0).0,
        image.get_pixel(0, height - 1).0,
        image.get_pixel(width - 1, height - 1).0,
    ];
    let background = median_color(&corners);
    let corner_matches = corners
        .iter()
        .filter(|color| color_distance(color, &background) <= u32::from(tolerance).pow(2) * 3)
        .count();
    if corner_matches < 3 {
        return Ok(None);
    }

    let len = usize::try_from(u64::from(width) * u64::from(height))
        .map_err(|_| AppError::InvalidDimensions)?;
    let mut background_mask = vec![false; len];
    let mut queue = VecDeque::new();
    for x in 0..width {
        queue.push_back((x, 0));
        queue.push_back((x, height - 1));
    }
    for y in 0..height {
        queue.push_back((0, y));
        queue.push_back((width - 1, y));
    }
    let threshold = u32::from(tolerance).pow(2) * 3;

    while let Some((x, y)) = queue.pop_front() {
        let index = usize::try_from(u64::from(y) * u64::from(width) + u64::from(x))
            .map_err(|_| AppError::InvalidDimensions)?;
        if background_mask[index]
            || color_distance(&image.get_pixel(x, y).0, &background) > threshold
        {
            continue;
        }
        background_mask[index] = true;
        if x > 0 {
            queue.push_back((x - 1, y));
        }
        if x + 1 < width {
            queue.push_back((x + 1, y));
        }
        if y > 0 {
            queue.push_back((x, y - 1));
        }
        if y + 1 < height {
            queue.push_back((x, y + 1));
        }
    }

    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0;
    let mut max_y = 0;
    let mut found = false;
    for y in 0..height {
        for x in 0..width {
            let index = usize::try_from(u64::from(y) * u64::from(width) + u64::from(x))
                .map_err(|_| AppError::InvalidDimensions)?;
            if !background_mask[index] {
                found = true;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }

    if !found {
        return Err(AppError::NoVisibleContent);
    }
    Ok(Some(CropBounds {
        x: min_x,
        y: min_y,
        width: max_x - min_x + 1,
        height: max_y - min_y + 1,
    }))
}

fn median_color(colors: &[[u8; 4]]) -> [u8; 4] {
    let mut result = [0; 4];
    for channel in 0..4 {
        let mut values: Vec<_> = colors.iter().map(|color| color[channel]).collect();
        values.sort_unstable();
        result[channel] = values[values.len() / 2];
    }
    result
}

fn color_distance(left: &[u8; 4], right: &[u8; 4]) -> u32 {
    left[..3]
        .iter()
        .zip(&right[..3])
        .map(|(a, b)| i32::from(*a) - i32::from(*b))
        .map(|delta| delta.unsigned_abs().pow(2))
        .sum()
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::{alpha_content_bounds, opaque_content_bounds};

    #[test]
    fn finds_alpha_content() {
        let mut image = RgbaImage::new(10, 10);
        for y in 3..7 {
            for x in 2..8 {
                image.put_pixel(x, y, Rgba([1, 2, 3, 255]));
            }
        }
        let bounds = alpha_content_bounds(&image, 1).unwrap();
        assert_eq!(
            (bounds.x, bounds.y, bounds.width, bounds.height),
            (2, 3, 6, 4)
        );
    }

    #[test]
    fn refuses_low_confidence_opaque_background() {
        let image = RgbaImage::from_fn(2, 2, |x, y| {
            Rgba([
                u8::try_from(x * 100).unwrap(),
                u8::try_from(y * 100).unwrap(),
                0,
                255,
            ])
        });
        assert!(opaque_content_bounds(&image, 1).unwrap().is_none());
    }
}
