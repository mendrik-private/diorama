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

pub fn ink_colors(
    source: &LinearImage,
    original: &[Model],
    smoothed: &[Model],
    model_owners: &[usize],
    strokes: &super::strokes::Strokes,
    scale: [f64; 2],
    cancel: &CancellationToken,
) -> Result<Vec<[f64; 3]>> {
    let coverage = &strokes.coverage;
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
            .radius(q, 2.)
            .into_iter()
            // The rasterizer owns topology and overlap decisions. A closer
            // sample from another colored contour must never repaint its core.
            .filter(|&j| Some(owners[j]) == strokes.owners[i] && colors[j].is_some())
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
    }
    Ok(ink)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::scale::game_asset::{opacity, strokes::Strokes};

    #[test]
    fn closer_green_donor_cannot_repaint_owned_black_core() {
        let source = LinearImage::from_rgba(&RgbaImage::from_fn(20, 20, |_, y| {
            image::Rgba(if y == 8 {
                [0, 0, 0, 255]
            } else {
                [60, 150, 30, 255]
            })
        }));
        let black = [0., 8., 0., 1., 0., 0., 0., -18., -1., 1.];
        let green = [0., 9., 0., 1., 0., 0., 0., -18., -1., 1.];
        let mut shifted_black = black;
        shifted_black[1] = 8.4;
        let mut strokes = Strokes {
            core: GrayImage::new(20, 20),
            coverage: GrayImage::new(20, 20),
            owners: vec![None; 400],
        };
        for x in 1..19 {
            strokes.core.put_pixel(x, 9, image::Luma([255]));
            strokes.coverage.put_pixel(x, 9, image::Luma([243]));
            strokes.owners[9 * 20 + x as usize] = Some(0);
        }
        let colors = ink_colors(
            &source,
            &[black, green],
            &[shifted_black, green],
            &[0, 1],
            &strokes,
            [1., 1.],
            &CancellationToken::default(),
        )
        .unwrap();
        for x in 1..19 {
            assert_eq!(colors[9 * 20 + x], [0.; 3]);
        }
        let coverage = opacity::apply(&strokes.coverage, &strokes.owners, &[1., 0.6]);
        assert_eq!(coverage.get_pixel(8, 9)[0], 243);
        let composite = composite(&source, &colors, &coverage);
        assert_eq!(
            *composite.get_pixel(8, 9),
            rgba([
                source.pixels[9 * 20 + 8][0] * 12. / 255.,
                source.pixels[9 * 20 + 8][1] * 12. / 255.,
                source.pixels[9 * 20 + 8][2] * 12. / 255.,
                1.
            ])
        );
        // Invisible donors may not fall back to a different contour's color.
        let invisible = LinearImage::from_rgba(&RgbaImage::new(20, 20));
        assert!(
            ink_colors(
                &invisible,
                &[black],
                &[shifted_black],
                &[0],
                &strokes,
                [1., 1.],
                &CancellationToken::default()
            )
            .is_err()
        );
    }
}
