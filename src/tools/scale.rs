use image::RgbaImage;

use crate::document::{CancellationToken, Resampling};
use crate::error::{AppError, Result};

pub mod game_asset;
mod gpu;

pub use gpu::GpuScaler;

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
    if let Resampling::GameAsset(aa) = resampling {
        return game_asset::resize(image, target_width, target_height, aa, cancellation);
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
        Resampling::Bicubic => {
            fast_image_resize::ResizeAlg::Convolution(fast_image_resize::FilterType::CatmullRom)
        }
        Resampling::Lanczos => {
            fast_image_resize::ResizeAlg::Convolution(fast_image_resize::FilterType::Lanczos3)
        }
        Resampling::GameAsset(_) => unreachable!(),
    };
    let options = fast_image_resize::ResizeOptions::new().resize_alg(algorithm);
    fast_image_resize::Resizer::new()
        .resize(&source, &mut destination, &options)
        .map_err(|_| AppError::InvalidDimensions)?;
    cancellation.check()?;
    RgbaImage::from_raw(target_width, target_height, destination.into_vec())
        .ok_or(AppError::InvalidDimensions)
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::resize;
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
            Resampling::Bicubic,
            Resampling::GameAsset(Default::default()),
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
        let bicubic = resize(&image, 4, 1, Resampling::Bicubic, &cancellation).unwrap();

        assert_ne!(nearest.get_pixel(1, 0), bicubic.get_pixel(1, 0));
    }
}
