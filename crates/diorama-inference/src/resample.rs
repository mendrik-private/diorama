//! STB-compatible separable image resampling used by native vision models.
//!
//! Colour images are filtered in linear light with premultiplied alpha. Masks
//! are scalar linear values and deliberately avoid colour conversion.

use image::{GrayImage, Rgba, RgbaImage};

const ALPHA_EPSILON: f32 = 8.271_806e-25; // 2^-80, as used by stb_image_resize.

#[derive(Clone, Copy)]
enum Filter {
    CatmullRom,
    Mitchell,
}

fn srgb_to_linear(value: u8) -> f32 {
    let value = f32::from(value) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}
fn linear_to_srgb(value: f32) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let value = if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (value * 255.0).round().clamp(0.0, 255.0) as u8
}
fn kernel(filter: Filter, x: f32) -> f32 {
    let x = x.abs();
    match filter {
        Filter::CatmullRom if x < 1.0 => 1.0 - x * x * (2.5 - 1.5 * x),
        Filter::CatmullRom if x < 2.0 => 2.0 - x * (4.0 + x * (0.5 * x - 2.5)),
        Filter::Mitchell if x < 1.0 => (16.0 + x * x * (21.0 * x - 36.0)) / 18.0,
        Filter::Mitchell if x < 2.0 => (32.0 + x * (-60.0 + x * (36.0 - 7.0 * x))) / 18.0,
        _ => 0.0,
    }
}

fn weights(source: usize, destination: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = destination as f32 / source as f32;
    let filter = if scale > 1.0 {
        Filter::CatmullRom
    } else {
        Filter::Mitchell
    };
    let radius = if scale > 1.0 { 2.0 } else { 2.0 / scale };
    (0..destination)
        .map(|out| {
            let center = (out as f32 + 0.5) / scale - 0.5;
            let first = (center - radius).floor() as isize;
            let last = (center + radius).ceil() as isize;
            let mut row = Vec::new();
            for input in first..=last {
                let distance = if scale > 1.0 {
                    input as f32 - center
                } else {
                    (input as f32 - center) * scale
                };
                let coefficient = kernel(filter, distance) * if scale > 1.0 { 1.0 } else { scale };
                if coefficient != 0.0 {
                    row.push((input.clamp(0, source as isize - 1) as usize, coefficient));
                }
            }
            let sum: f32 = row.iter().map(|(_, weight)| weight).sum();
            for (_, weight) in &mut row {
                *weight /= sum;
            }
            row
        })
        .collect()
}

fn resize_f32(
    values: &[f32],
    channels: usize,
    source_width: usize,
    source_height: usize,
    width: usize,
    height: usize,
) -> Vec<f32> {
    if (source_width, source_height) == (width, height) {
        return values.to_vec();
    }
    let horizontal = weights(source_width, width);
    let vertical = weights(source_height, height);
    let mut middle = vec![0.0; source_height * width * channels];
    for y in 0..source_height {
        for (x, row) in horizontal.iter().enumerate() {
            for channel in 0..channels {
                middle[(y * width + x) * channels + channel] = row
                    .iter()
                    .map(|(input, weight)| {
                        values[(y * source_width + input) * channels + channel] * weight
                    })
                    .sum();
            }
        }
    }
    let mut output = vec![0.0; height * width * channels];
    for (y, row) in vertical.iter().enumerate() {
        for x in 0..width {
            for channel in 0..channels {
                output[(y * width + x) * channels + channel] = row
                    .iter()
                    .map(|(input, weight)| {
                        middle[(input * width + x) * channels + channel] * weight
                    })
                    .sum();
            }
        }
    }
    output
}

pub(crate) fn resize_rgba(input: &RgbaImage, width: u32, height: u32) -> RgbaImage {
    if input.dimensions() == (width, height) {
        return input.clone();
    }
    let mut source = Vec::with_capacity(input.len());
    for pixel in input.pixels() {
        let alpha = f32::from(pixel[3]) / 255.0 + ALPHA_EPSILON;
        source.extend([
            srgb_to_linear(pixel[0]) * alpha,
            srgb_to_linear(pixel[1]) * alpha,
            srgb_to_linear(pixel[2]) * alpha,
            alpha,
        ]);
    }
    let output = resize_f32(
        &source,
        4,
        input.width() as usize,
        input.height() as usize,
        width as usize,
        height as usize,
    );
    let mut image = RgbaImage::new(width, height);
    for (pixel, values) in image.pixels_mut().zip(output.chunks_exact(4)) {
        let alpha = values[3];
        let reciprocal_alpha = if alpha == 0.0 { 0.0 } else { alpha.recip() };
        *pixel = Rgba([
            linear_to_srgb(values[0] * reciprocal_alpha),
            linear_to_srgb(values[1] * reciprocal_alpha),
            linear_to_srgb(values[2] * reciprocal_alpha),
            (alpha * 255.0).round() as u8,
        ]);
    }
    image
}

pub(crate) fn resize_mask(
    input: &[f32],
    source_width: u32,
    source_height: u32,
    width: u32,
    height: u32,
) -> GrayImage {
    assert_eq!(
        input.len(),
        source_width as usize * source_height as usize,
        "mask dimensions do not match data"
    );
    let output = resize_f32(
        input,
        1,
        source_width as usize,
        source_height as usize,
        width as usize,
        height as usize,
    );
    let mut image = GrayImage::new(width, height);
    for (pixel, value) in image.pixels_mut().zip(output) {
        *pixel = image::Luma([(value.clamp(0.0, 1.0) * 255.0) as u8]);
    }
    image
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_is_bit_exact() {
        let image = RgbaImage::from_pixel(3, 2, Rgba([4, 5, 6, 7]));
        assert_eq!(resize_rgba(&image, 3, 2), image);
    }
    #[test]
    fn transparent_pixels_do_not_leak_colour() {
        let mut image = RgbaImage::new(2, 1);
        image.put_pixel(0, 0, Rgba([255, 0, 0, 0]));
        image.put_pixel(1, 0, Rgba([0, 0, 255, 255]));
        let result = resize_rgba(&image, 7, 1);
        assert!(result.pixels().any(|pixel| pixel[2] > pixel[0]));
    }
    #[test]
    fn mask_identity_truncates_like_worker() {
        assert_eq!(resize_mask(&[0.0, 1.0], 2, 1, 2, 1).as_raw(), &[0, 255]);
    }

    #[test]
    fn matches_stb_alpha_aware_upscale_fixture() {
        // Generated by /tmp/stb_resample_probe.c using stb_image_resize.h.
        let source = RgbaImage::from_raw(
            3,
            2,
            vec![
                255, 0, 0, 0, 0, 255, 0, 128, 0, 0, 255, 255, 200, 100, 50, 64, 20, 40, 60, 255,
                255, 255, 0, 32,
            ],
        )
        .unwrap();
        let expected = [
            140, 216, 27, 0, 0, 255, 0, 65, 0, 195, 191, 181, 0, 0, 255, 255, 255, 0, 64, 6, 42,
            224, 0, 101, 27, 173, 168, 183, 50, 23, 255, 212, 221, 100, 48, 42, 65, 99, 38, 179,
            57, 111, 101, 190, 166, 162, 227, 70, 218, 110, 47, 59, 68, 0, 64, 215, 65, 61, 31,
            193, 255, 255, 0, 4,
        ];
        let actual = resize_rgba(&source, 4, 4);
        for (actual, expected) in actual.as_raw().iter().zip(expected) {
            assert!(actual.abs_diff(expected) <= 1, "{actual} != {expected}");
        }
    }

    #[test]
    #[allow(clippy::excessive_precision)] // Preserve the independently generated STB f32 golden.
    fn matches_stb_linear_mask_fixture() {
        let source = [0.0, 0.1, 0.9, 1.0, 0.4, 0.7];
        let expected = [
            -0.0774528533,
            -0.0338478088,
            0.378551453,
            0.954025984,
            0.20514375,
            0.12436676,
            0.399299622,
            0.892796278,
            0.818782091,
            0.467918396,
            0.444352716,
            0.75984031,
            1.10137868,
            0.626132965,
            0.465100855,
            0.698610604,
        ];
        let actual = resize_f32(&source, 1, 3, 2, 4, 4);
        for (actual, expected) in actual.iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-5, "{actual} != {expected}");
        }
    }

    #[test]
    fn matches_stb_downsample_and_one_axis_fixtures() {
        let source = RgbaImage::from_raw(
            3,
            2,
            vec![
                255, 0, 0, 0, 0, 255, 0, 128, 0, 0, 255, 255, 200, 100, 50, 64, 20, 40, 60, 255,
                255, 255, 0, 32,
            ],
        )
        .unwrap();
        let down = resize_rgba(&source, 2, 1);
        let expected_down = [104, 149, 36, 88, 72, 127, 192, 162];
        for (actual, expected) in down.as_raw().iter().zip(expected_down) {
            assert!(actual.abs_diff(expected) <= 1, "{actual} != {expected}");
        }
        let one_axis = resize_rgba(&source, 3, 5);
        let expected_one_axis = [
            0, 255, 0, 2, 0, 255, 97, 120, 0, 39, 253, 255, 122, 207, 32, 11, 8, 230, 91, 135, 22,
            54, 250, 235, 175, 119, 50, 41, 33, 157, 74, 180, 91, 100, 234, 146, 182, 94, 52, 70,
            41, 70, 61, 225, 187, 188, 144, 57, 183, 90, 52, 80, 43, 0, 58, 240, 255, 255, 0, 29,
        ];
        for (actual, expected) in one_axis.as_raw().iter().zip(expected_one_axis) {
            assert!(actual.abs_diff(expected) <= 1, "{actual} != {expected}");
        }
    }

    #[test]
    fn matches_stb_one_pixel_wide_alpha_fixture() {
        let source =
            RgbaImage::from_raw(1, 3, vec![255, 0, 0, 0, 0, 255, 0, 128, 0, 0, 255, 255]).unwrap();
        let actual = resize_rgba(&source, 4, 5);
        let expected = [
            0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 42, 0, 255, 0, 42,
            0, 255, 0, 42, 0, 255, 0, 42, 0, 255, 0, 128, 0, 255, 0, 128, 0, 255, 0, 128, 0, 255,
            0, 128, 0, 138, 224, 213, 0, 138, 224, 213, 0, 138, 224, 213, 0, 138, 224, 213, 0, 0,
            255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255,
        ];
        for (actual, expected) in actual.as_raw().iter().zip(expected) {
            assert!(actual.abs_diff(expected) <= 1, "{actual} != {expected}");
        }
    }
}
