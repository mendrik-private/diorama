use super::{
    color::{LinearImage, rgba},
    detect::{Model, curve_point},
    field::{Spatial, distance2},
};
use crate::{document::CancellationToken, error::Result};
use image::{GrayImage, RgbaImage};
fn source_color(source: &LinearImage, point: [f64; 2]) -> Option<[f64; 3]> {
    let x0 = point[0].floor() as isize;
    let y0 = point[1].floor() as isize;
    let mut sum = [0.; 3];
    let mut total = 0.;
    for y in y0..=y0 + 1 {
        for x in x0..=x0 + 1 {
            if x < 0 || y < 0 || x >= source.w as isize || y >= source.h as isize {
                continue;
            }
            let p = source.pixels[y as usize * source.w + x as usize];
            let weight =
                (1. - (point[0] - x as f64).abs()) * (1. - (point[1] - y as f64).abs()) * p[3];
            total += weight;
            for c in 0..3 {
                sum[c] += p[c] * weight;
            }
        }
    }
    (total > 1e-8).then(|| sum.map(|v| v / total))
}

pub struct Ink {
    pub colors: Vec<[f64; 3]>,
    pub owners: Vec<Option<usize>>,
}

pub fn ink_colors(
    source: &LinearImage,
    original: &[Model],
    smoothed: &[Model],
    model_owners: &[usize],
    coverage: &GrayImage,
    scale: [f64; 2],
    cancel: &CancellationToken,
) -> Result<Ink> {
    let mut points = Vec::new();
    let mut colors = Vec::new();
    let mut owners = Vec::new();
    for ((m, donor), &owner) in smoothed.iter().zip(original).zip(model_owners) {
        cancel.check()?;
        let count = (((m[8] - m[7]) / 0.1).ceil() as usize).max(1);
        for i in 0..=count {
            let u = m[7] + (m[8] - m[7]) * i as f64 / count as f64;
            points.push(std::array::from_fn(|i| {
                (curve_point(m, u)[i] + 0.5) * scale[i] - 0.5
            }));
            colors.push(source_color(source, curve_point(donor, u)));
            owners.push(owner);
        }
    }
    let spatial = Spatial::new(points, 2.);
    let mut ink = vec![[0.; 3]; coverage.as_raw().len()];
    let mut pixel_owners = vec![None; coverage.as_raw().len()];
    for (i, p) in coverage.pixels().enumerate() {
        if i % 4096 == 0 {
            cancel.check()?;
        }
        if p[0] == 0 {
            continue;
        }
        let q = [
            (i % coverage.width() as usize) as f64,
            (i / coverage.width() as usize) as f64,
        ];
        let nearest = spatial
            .radius(q, 1.5)
            .into_iter()
            .filter(|&j| colors[j].is_some())
            .min_by(|&a, &b| {
                distance2(q, spatial.points[a])
                    .total_cmp(&distance2(q, spatial.points[b]))
                    .then(a.cmp(&b))
            });
        let Some(j) = nearest else {
            return Err(crate::error::AppError::Scaling(format!(
                "No visible source ink donor for target pixel {i}"
            )));
        };
        ink[i] = colors[j].unwrap();
        pixel_owners[i] = Some(owners[j]);
    }
    Ok(Ink {
        colors: ink,
        owners: pixel_owners,
    })
}

pub fn composite(base: &LinearImage, ink: &[[f64; 3]], coverage: &GrayImage) -> RgbaImage {
    RgbaImage::from_fn(base.w as u32, base.h as u32, |x, y| {
        let i = y as usize * base.w + x as usize;
        let p = base.pixels[i];
        let a = coverage.as_raw()[i] as f64 / 255.;
        let alpha = a + p[3] * (1. - a);
        if alpha <= 1e-8 {
            return image::Rgba([0; 4]);
        }
        rgba([
            (ink[i][0] * a + p[0] * p[3] * (1. - a)) / alpha,
            (ink[i][1] * a + p[1] * p[3] * (1. - a)) / alpha,
            (ink[i][2] * a + p[2] * p[3] * (1. - a)) / alpha,
            alpha,
        ])
    })
}
