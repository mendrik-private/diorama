//! Exact fractional area integration in premultiplied linear light.
use super::color::LinearImage;
use crate::{document::CancellationToken, error::Result};

pub fn area(
    source: &LinearImage,
    w: usize,
    h: usize,
    cancel: &CancellationToken,
) -> Result<LinearImage> {
    let sy = source.h as f64 / h as f64;
    let sx = source.w as f64 / w as f64;
    let mut vertical = vec![[0.; 4]; source.w * h];
    for y in 0..h {
        cancel.check()?;
        let top = y as f64 * sy;
        let bottom = (y + 1) as f64 * sy;
        for yy in top.floor() as usize..(bottom.ceil() as usize).min(source.h) {
            cancel.check()?;
            let weight = (bottom.min((yy + 1) as f64) - top.max(yy as f64)) / sy;
            for x in 0..source.w {
                let p = source.pixels[yy * source.w + x];
                let sum = &mut vertical[y * source.w + x];
                for c in 0..3 {
                    sum[c] += p[c] * p[3] * weight;
                }
                sum[3] += p[3] * weight;
            }
        }
    }
    let mut pixels = vec![[0.; 4]; w * h];
    for y in 0..h {
        cancel.check()?;
        for x in 0..w {
            let left = x as f64 * sx;
            let right = (x + 1) as f64 * sx;
            let sum = &mut pixels[y * w + x];
            for xx in left.floor() as usize..(right.ceil() as usize).min(source.w) {
                let weight = (right.min((xx + 1) as f64) - left.max(xx as f64)) / sx;
                let p = vertical[y * source.w + xx];
                for c in 0..4 {
                    sum[c] += p[c] * weight;
                }
            }
            if sum[3] > 0. {
                for c in 0..3 {
                    sum[c] /= sum[3];
                }
            }
        }
    }
    Ok(LinearImage { w, h, pixels })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_rectangles_conserve_color_and_alpha_without_hidden_rgb() {
        let source = LinearImage {
            w: 3,
            h: 2,
            pixels: vec![
                [1., 0., 0., 1.],
                [0., 1., 0., 0.5],
                [0., 0., 1., 0.],
                [1., 0., 0., 1.],
                [0., 1., 0., 0.5],
                [1., 1., 1., 0.],
            ],
        };
        let result = area(&source, 2, 1, &CancellationToken::default()).unwrap();
        for (actual, expected) in result
            .pixels
            .iter()
            .zip([[0.8, 0.2, 0., 5. / 6.], [0., 1., 0., 1. / 6.]])
        {
            for c in 0..4 {
                assert!((actual[c] - expected[c]).abs() < 1e-12);
            }
        }
    }
}
