use image::RgbaImage;

use crate::document::{CancellationToken, Resampling};
use crate::error::{AppError, Result};

#[cfg(test)]
pub mod contour_lanczos;
#[cfg(test)]
pub mod game_asset;
mod gpu;
pub mod palette_halving;
mod source_palette;

pub use gpu::GpuScaler;

#[cfg(test)]
mod reference;

#[cfg(test)]
mod benchmark;

#[cfg(test)]
mod halving_experiment;

pub fn resize(
    image: &RgbaImage,
    target_width: u32,
    target_height: u32,
    resampling: Resampling,
    cancellation: &CancellationToken,
) -> Result<RgbaImage> {
    if target_width == 0 || target_height == 0 {
        return Err(AppError::InvalidDimensions);
    }
    if resampling == Resampling::SeamCarving {
        return seam_carve(image, target_width, target_height, cancellation);
    }
    if resampling == Resampling::GameAsset {
        return palette_halving::Session::new(std::sync::Arc::new(image.clone()))
            .resize(target_width, target_height, false, cancellation)
            .map(|result| result.image);
    }
    cancellation.check()?;
    if image.dimensions() == (target_width, target_height) {
        return Ok(image.clone());
    }
    let source = fast_image_resize::images::ImageRef::new(
        image.width(),
        image.height(),
        image.as_raw(),
        fast_image_resize::PixelType::U8x4,
    )
    .map_err(|_| AppError::InvalidDimensions)?;
    let mut destination = fast_image_resize::images::Image::new(
        target_width,
        target_height,
        fast_image_resize::PixelType::U8x4,
    );
    let algorithm = match resampling {
        Resampling::Nearest => fast_image_resize::ResizeAlg::Nearest,
        Resampling::Linear => {
            fast_image_resize::ResizeAlg::Convolution(fast_image_resize::FilterType::Bilinear)
        }
        Resampling::Bicubic => {
            fast_image_resize::ResizeAlg::Convolution(fast_image_resize::FilterType::CatmullRom)
        }
        Resampling::Lanczos => {
            fast_image_resize::ResizeAlg::Convolution(fast_image_resize::FilterType::Lanczos3)
        }
        Resampling::SeamCarving | Resampling::GameAsset => unreachable!(),
    };
    let options = fast_image_resize::ResizeOptions::new().resize_alg(algorithm);
    fast_image_resize::Resizer::new()
        .resize(&source, &mut destination, &options)
        .map_err(|_| AppError::InvalidDimensions)?;
    cancellation.check()?;
    RgbaImage::from_raw(target_width, target_height, destination.into_vec())
        .ok_or(AppError::InvalidDimensions)
}

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
    cancellation.check()?;
    let mut output = carve_width(image.clone(), target_width, cancellation)?;
    if output.height() > target_height {
        cancellation.check()?;
        output = image::imageops::rotate90(&output);
        output = carve_width(output, target_height, cancellation)?;
        output = image::imageops::rotate270(&output);
    }
    cancellation.check()?;
    Ok(output)
}

/// Keep rows at their original stride while carving. Pixels, energy and parent
/// buffers are allocated once per axis; only the final image is packed tightly.
fn carve_width(
    image: RgbaImage,
    target_width: u32,
    cancellation: &CancellationToken,
) -> Result<RgbaImage> {
    if image.width() == target_width {
        return Ok(image);
    }
    let mut width = image.width() as usize;
    let height = image.height() as usize;
    let stride = width;
    let mut pixels = image.into_raw();
    let len = pixels.len() / 4;
    let mut energies = vec![0_u32; len];
    let mut parents = vec![0_i8; len];
    // A seam only depends on costs in the preceding row.
    let mut previous = vec![0_u64; stride];
    let mut current = vec![0_u64; stride];
    let mut seam = vec![0_usize; height];

    for y in 0..height {
        cancellation.check()?;
        for x in 0..width {
            energies[y * stride + x] = pixel_energy(&pixels, stride, width, height, x, y);
        }
    }

    while width > target_width as usize {
        cancellation.check()?;
        for (cost, energy) in previous[..width].iter_mut().zip(&energies[..width]) {
            *cost = u64::from(*energy);
        }
        for y in 1..height {
            cancellation.check()?;
            let row = y * stride;
            let energy_row = &energies[row..row + width];
            let parent_row = &mut parents[row..row + width];
            let right_is_lower = previous[1] < previous[0];
            current[0] = previous[usize::from(right_is_lower)] + u64::from(energy_row[0]);
            parent_row[0] = i8::from(right_is_lower);
            for (((cost, parent), energy), neighbors) in current[1..width - 1]
                .iter_mut()
                .zip(&mut parent_row[1..width - 1])
                .zip(&energy_row[1..width - 1])
                .zip(previous[..width].windows(3))
            {
                // Preserve the original tie order: straight, left, then right.
                let (mut best, mut direction) = (neighbors[1], 0);
                if neighbors[0] < best {
                    best = neighbors[0];
                    direction = -1;
                }
                if neighbors[2] < best {
                    best = neighbors[2];
                    direction = 1;
                }
                *cost = best + u64::from(*energy);
                *parent = direction;
            }
            let last = width - 1;
            let left_is_lower = previous[last - 1] < previous[last];
            current[last] =
                previous[last - usize::from(left_is_lower)] + u64::from(energy_row[last]);
            parent_row[last] = -i8::from(left_is_lower);
            std::mem::swap(&mut previous, &mut current);
        }
        let mut x = previous[..width]
            .iter()
            .enumerate()
            .min_by_key(|(_, cost)| *cost)
            .map_or(0, |(x, _)| x);
        for y in (0..height).rev() {
            seam[y] = x;
            if y > 0 {
                x = x.saturating_add_signed(isize::from(parents[y * stride + x]));
            }
        }

        for (y, &removed_x) in seam.iter().enumerate() {
            cancellation.check()?;
            let row = y * stride;
            pixels.copy_within(
                (row + removed_x + 1) * 4..(row + width) * 4,
                (row + removed_x) * 4,
            );
            energies.copy_within(row + removed_x + 1..row + width, row + removed_x);
        }
        width -= 1;
        if width == target_width as usize {
            break;
        }
        for y in 0..height {
            cancellation.check()?;
            // Only neighbors of the removed seam can have a changed gradient.
            // Include adjacent rows: their seams may shift a vertical neighbor.
            let above = seam[y.saturating_sub(1)];
            let below = seam[(y + 1).min(height - 1)];
            let first = seam[y].min(above).min(below).saturating_sub(1);
            let last = seam[y].max(above).max(below).min(width - 1);
            for x in first..=last {
                energies[y * stride + x] = pixel_energy(&pixels, stride, width, height, x, y);
            }
        }
    }

    for y in 0..height {
        cancellation.check()?;
        pixels.copy_within(y * stride * 4..(y * stride + width) * 4, y * width * 4);
    }
    pixels.truncate(width * height * 4);
    RgbaImage::from_raw(target_width, height as u32, pixels).ok_or(AppError::InvalidDimensions)
}

fn pixel_energy(
    pixels: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    x: usize,
    y: usize,
) -> u32 {
    let left = (y * stride + x.saturating_sub(1)) * 4;
    let right = (y * stride + (x + 1).min(width - 1)) * 4;
    let above = (y.saturating_sub(1) * stride + x) * 4;
    let below = ((y + 1).min(height - 1) * stride + x) * 4;
    let mut energy = 0;
    for channel in 0..3 {
        let horizontal = i32::from(pixels[left + channel]) - i32::from(pixels[right + channel]);
        let vertical = i32::from(pixels[above + channel]) - i32::from(pixels[below + channel]);
        energy += (horizontal * horizontal + vertical * vertical) as u32;
    }
    energy
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::{resize, seam_carve};
    use crate::document::{CancellationToken, Resampling};

    #[test]
    fn lanczos_uses_lanczos3_for_downscaling_and_upscaling() {
        let image = RgbaImage::from_fn(16, 12, |x, y| {
            image::Rgba([(x * 13) as u8, (y * 17) as u8, ((x + y) * 9) as u8, 200])
        });
        let cancellation = CancellationToken::default();
        assert!(!Resampling::Lanczos.downscale_only());
        for (w, h) in [(6, 5), (32, 24)] {
            let input = fast_image_resize::images::ImageRef::new(
                16,
                12,
                image.as_raw(),
                fast_image_resize::PixelType::U8x4,
            )
            .unwrap();
            let mut expected =
                fast_image_resize::images::Image::new(w, h, fast_image_resize::PixelType::U8x4);
            let options = fast_image_resize::ResizeOptions::new().resize_alg(
                fast_image_resize::ResizeAlg::Convolution(fast_image_resize::FilterType::Lanczos3),
            );
            fast_image_resize::Resizer::new()
                .resize(&input, &mut expected, &options)
                .unwrap();
            let actual = resize(&image, w, h, Resampling::Lanczos, &cancellation).unwrap();
            assert_eq!(actual.as_raw().as_slice(), expected.buffer());
        }
        assert_eq!(
            resize(&image, 16, 12, Resampling::Lanczos, &cancellation).unwrap(),
            image
        );
        assert!(resize(&image, 0, 5, Resampling::Lanczos, &cancellation).is_err());
    }

    #[test]
    fn cancelled_resizes_do_not_start_even_when_dimensions_are_unchanged() {
        let image = RgbaImage::new(8, 6);
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        for method in [
            Resampling::Nearest,
            Resampling::Linear,
            Resampling::Bicubic,
            Resampling::SeamCarving,
            Resampling::GameAsset,
            Resampling::Lanczos,
        ] {
            for (width, height) in [(8, 6), (8, 3), (4, 6), (4, 3)] {
                assert!(matches!(
                    resize(&image, width, height, method, &cancellation),
                    Err(crate::error::AppError::Cancelled)
                ));
            }
        }
    }

    #[test]
    fn seam_carving_rejects_empty_or_enlarged_outputs() {
        let image = RgbaImage::new(8, 6);
        for (width, height) in [(0, 6), (8, 0), (9, 6), (8, 7)] {
            assert!(matches!(
                seam_carve(&image, width, height, &CancellationToken::default()),
                Err(crate::error::AppError::InvalidDimensions)
            ));
        }
    }

    #[test]
    fn seam_carving_matches_reference_pixels_and_ties() {
        let mut seed = 17_u32;
        for width in 1..=12 {
            for height in 1..=10 {
                for palette_size in [1, 3, 256] {
                    let image = RgbaImage::from_fn(width, height, |_, _| {
                        let mut channels = [0; 4];
                        for channel in &mut channels {
                            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                            *channel = ((seed >> 16) % palette_size) as u8;
                        }
                        Rgba(channels)
                    });
                    for (target_width, target_height) in [
                        (1, 1),
                        (width, 1),
                        (1, height),
                        (width, height),
                        ((width / 2).max(1), (height / 2).max(1)),
                    ] {
                        let cancellation = CancellationToken::default();
                        let expected = super::reference::seam_carve(
                            &image,
                            target_width,
                            target_height,
                            &cancellation,
                        )
                        .unwrap();
                        let actual =
                            seam_carve(&image, target_width, target_height, &cancellation).unwrap();
                        assert_eq!(
                            actual, expected,
                            "{width}x{height} -> {target_width}x{target_height}, palette {palette_size}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn shrinks_both_axes() {
        let image = RgbaImage::from_pixel(5, 4, Rgba([1, 2, 3, 255]));
        let output = seam_carve(&image, 3, 2, &CancellationToken::default()).unwrap();
        assert_eq!(output.dimensions(), (3, 2));
    }

    #[test]
    fn nearest_resize_preserves_source_pixels() {
        let image = RgbaImage::from_fn(2, 1, |x, _| {
            if x == 0 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 255, 255])
            }
        });

        let output = resize(
            &image,
            4,
            2,
            Resampling::Nearest,
            &CancellationToken::default(),
        )
        .unwrap();

        assert_eq!(output.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(output.get_pixel(3, 1).0, [0, 0, 255, 255]);
    }

    #[test]
    fn resampling_method_changes_interpolated_pixels() {
        let image = RgbaImage::from_fn(2, 1, |x, _| {
            if x == 0 {
                Rgba([0, 0, 0, 255])
            } else {
                Rgba([255, 255, 255, 255])
            }
        });
        let cancellation = CancellationToken::default();

        let nearest = resize(&image, 4, 1, Resampling::Nearest, &cancellation).unwrap();
        let linear = resize(&image, 4, 1, Resampling::Linear, &cancellation).unwrap();

        assert_ne!(nearest.get_pixel(1, 0), linear.get_pixel(1, 0));
    }
}
