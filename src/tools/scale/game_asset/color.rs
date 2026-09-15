//! Straight linear-light RGBA at the analysis and composition boundaries.
use image::{Rgba, RgbaImage};
use std::sync::LazyLock;

// Exact values of the existing conversion at every possible RGBA8 input.
static SRGB8_TO_LINEAR: LazyLock<[f64; 256]> =
    LazyLock::new(|| std::array::from_fn(|v| decode(v as f64 / 255.)));

#[derive(Clone)]
pub struct LinearImage {
    pub w: usize,
    pub h: usize,
    pub pixels: Vec<[f64; 4]>,
}

pub fn decode(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
pub fn encode(v: f64) -> f64 {
    let v = v.clamp(0., 1.);
    if v <= 0.0031308 {
        12.92 * v
    } else {
        1.055 * v.powf(1. / 2.4) - 0.055
    }
}
pub fn rgba(p: [f64; 4]) -> Rgba<u8> {
    let a = (p[3].clamp(0., 1.) * 255.).round() as u8;
    if a == 0 {
        return Rgba([0; 4]);
    }
    Rgba([
        (encode(p[0]) * 255.).round() as u8,
        (encode(p[1]) * 255.).round() as u8,
        (encode(p[2]) * 255.).round() as u8,
        a,
    ])
}
impl LinearImage {
    pub fn from_rgba(source: &RgbaImage) -> Self {
        let decoded = &*SRGB8_TO_LINEAR;
        Self {
            w: source.width() as usize,
            h: source.height() as usize,
            pixels: source
                .pixels()
                .map(|p| {
                    [
                        decoded[p[0] as usize],
                        decoded[p[1] as usize],
                        decoded[p[2] as usize],
                        p[3] as f64 / 255.,
                    ]
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_lookup_matches_transfer_function_exactly() {
        let image = RgbaImage::from_fn(256, 1, |x, _| {
            Rgba([x as u8, (255 - x) as u8, ((x * 17) % 256) as u8, x as u8])
        });
        let linear = LinearImage::from_rgba(&image);
        for (p, actual) in image.pixels().zip(linear.pixels) {
            for c in 0..3 {
                assert_eq!(actual[c].to_bits(), decode(p[c] as f64 / 255.).to_bits());
            }
            assert_eq!(actual[3], p[3] as f64 / 255.);
        }
    }
}
