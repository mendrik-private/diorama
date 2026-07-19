use image::RgbaImage;

use crate::error::{AppError, Result};
use crate::tools::crop::CropBounds;

pub fn bounds_between(
    start: (u32, u32),
    end: (u32, u32),
    image_width: u32,
    image_height: u32,
) -> Result<CropBounds> {
    if image_width == 0 || image_height == 0 {
        return Err(AppError::InvalidDimensions);
    }
    let start = (start.0.min(image_width - 1), start.1.min(image_height - 1));
    let end = (end.0.min(image_width - 1), end.1.min(image_height - 1));
    let x = start.0.min(end.0);
    let y = start.1.min(end.1);
    Ok(CropBounds {
        x,
        y,
        width: start.0.max(end.0) - x + 1,
        height: start.1.max(end.1) - y + 1,
    })
}

pub fn crop(image: &RgbaImage, bounds: CropBounds) -> Result<RgbaImage> {
    let right = bounds
        .x
        .checked_add(bounds.width)
        .ok_or(AppError::InvalidDimensions)?;
    let bottom = bounds
        .y
        .checked_add(bounds.height)
        .ok_or(AppError::InvalidDimensions)?;
    if bounds.width == 0 || bounds.height == 0 || right > image.width() || bottom > image.height() {
        return Err(AppError::InvalidDimensions);
    }
    Ok(
        image::imageops::crop_imm(image, bounds.x, bounds.y, bounds.width, bounds.height)
            .to_image(),
    )
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::{bounds_between, crop};

    #[test]
    fn reverse_drag_produces_inclusive_clamped_bounds() {
        let bounds = bounds_between((8, 7), (2, 3), 6, 5).unwrap();
        assert_eq!(
            (bounds.x, bounds.y, bounds.width, bounds.height),
            (2, 3, 4, 2)
        );
    }

    #[test]
    fn crop_preserves_exact_selected_pixels() {
        let image = RgbaImage::from_fn(4, 3, |x, y| Rgba([x as u8, y as u8, 7, 255]));
        let bounds = bounds_between((1, 1), (2, 2), image.width(), image.height()).unwrap();
        let selected = crop(&image, bounds).unwrap();

        assert_eq!(selected.dimensions(), (2, 2));
        assert_eq!(selected.get_pixel(0, 0).0, [1, 1, 7, 255]);
        assert_eq!(selected.get_pixel(1, 1).0, [2, 2, 7, 255]);
    }
}
