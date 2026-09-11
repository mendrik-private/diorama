use image::{Rgba, RgbaImage};

use crate::document::CancellationToken;
use crate::error::{AppError, Result};

pub fn center_offset(source: u32, target: u32) -> i64 {
    (i64::from(target) - i64::from(source)) / 2
}

/// Estimate the border color in premultiplied RGBA, ignoring hidden RGB in transparent pixels.
pub fn background(image: &RgbaImage) -> [u8; 4] {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return [0; 4];
    }
    let mut channels: [Vec<u8>; 4] = std::array::from_fn(|_| Vec::new());
    let mut sample = |x, y| {
        let color = image.get_pixel(x, y).0;
        for channel in 0..3 {
            channels[channel].push((u16::from(color[channel]) * u16::from(color[3]) / 255) as u8);
        }
        channels[3].push(color[3]);
    };
    for x in 0..width {
        sample(x, 0);
        if height > 1 {
            sample(x, height - 1);
        }
    }
    for y in 1..height.saturating_sub(1) {
        sample(0, y);
        if width > 1 {
            sample(width - 1, y);
        }
    }
    let medians: [u8; 4] = std::array::from_fn(|i| {
        let middle = channels[i].len() / 2;
        *channels[i].select_nth_unstable(middle).1
    });
    if medians[3] == 0 {
        return [0; 4];
    }
    let mut color = medians;
    for value in &mut color[..3] {
        *value = (u16::from(*value) * 255 / u16::from(medians[3])).min(255) as u8;
    }
    color
}

pub fn resize(
    image: &RgbaImage,
    width: u32,
    height: u32,
    background: [u8; 4],
    cancellation: &CancellationToken,
) -> Result<RgbaImage> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(AppError::InvalidDimensions)?;
    if width == 0 || height == 0 || bytes > crate::image::DecodeLimits::default().max_decoded_bytes
    {
        return Err(AppError::InvalidDimensions);
    }
    cancellation.check()?;
    let mut output = RgbaImage::from_pixel(width, height, Rgba(background));
    // Replace rather than blend: preserve semitransparent source pixels exactly.
    image::imageops::replace(
        &mut output,
        image,
        center_offset(image.width(), width),
        center_offset(image.height(), height),
    );
    cancellation.check()?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_padding_preserves_source_pixels_and_alpha() {
        let source = RgbaImage::from_pixel(2, 4, Rgba([80, 40, 20, 128]));
        let output = resize(&source, 5, 4, [0; 4], &CancellationToken::default()).unwrap();
        assert_eq!(output.get_pixel(0, 0).0, [0; 4]);
        assert_eq!(output.get_pixel(1, 0), source.get_pixel(0, 0));
        assert_eq!(output.get_pixel(2, 3), source.get_pixel(1, 3));
        assert_eq!(output.get_pixel(4, 3).0, [0; 4]);
    }

    #[test]
    fn detects_transparent_and_solid_borders() {
        for color in [[255, 255, 255, 255], [20, 40, 60, 255], [90, 30, 60, 0]] {
            let mut image = RgbaImage::from_pixel(8, 6, Rgba(color));
            image.put_pixel(3, 3, Rgba([255, 0, 0, 255]));
            assert_eq!(
                background(&image),
                if color[3] == 0 { [0; 4] } else { color }
            );
        }
    }
}
