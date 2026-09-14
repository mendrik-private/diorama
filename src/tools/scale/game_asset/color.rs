//! Straight linear-light RGBA at the analysis and composition boundaries.
use image::{Rgba, RgbaImage};

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
        Self {
            w: source.width() as usize,
            h: source.height() as usize,
            pixels: source
                .pixels()
                .map(|p| {
                    [
                        decode(p[0] as f64 / 255.),
                        decode(p[1] as f64 / 255.),
                        decode(p[2] as f64 / 255.),
                        p[3] as f64 / 255.,
                    ]
                })
                .collect(),
        }
    }
}
