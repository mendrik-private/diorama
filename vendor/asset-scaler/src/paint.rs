use super::{
    color::{LinearImage, rgba},
    coverage::Quadratic,
    detect::{Model, curve_point},
    field::{Spatial, distance2},
};
#[cfg(test)]
use crate::CancellationToken;
use crate::{Cancellation, Result};
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
    cancel: &dyn Cancellation,
) -> Result<Vec<[f64; 3]>> {
    let (ink, visible) = ink_colors_with_visibility(
        source,
        original,
        smoothed,
        model_owners,
        strokes,
        scale,
        cancel,
    )?;
    if let Some((i, _)) = strokes
        .coverage
        .as_raw()
        .iter()
        .zip(&visible)
        .enumerate()
        .find(|&(_, (&coverage, &visible))| coverage != 0 && !visible)
    {
        return Err(crate::Error::Scaling(format!(
            "No visible source ink donor for target pixel {i}"
        )));
    }
    Ok(ink)
}

/// Color source contours from an optionally transparent aligned source.
///
/// Every target contour pixel retains its original geometry owner. `visible`
/// reports whether that owner has a source-alpha-supported color donor. A
/// caller that uses an extracted foreground can suppress unsupported painted
/// pixels instead of falling back to opaque background RGB from another image.
pub fn ink_colors_with_visibility(
    source: &LinearImage,
    original: &[Model],
    smoothed: &[Model],
    model_owners: &[usize],
    strokes: &super::strokes::Strokes,
    scale: [f64; 2],
    cancel: &dyn Cancellation,
) -> Result<(Vec<[f64; 3]>, Vec<bool>)> {
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
    let mut visible = vec![false; coverage.as_raw().len()];
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
        let candidates = spatial.radius(q, 2.);
        let nearest_owner = candidates
            .iter()
            .copied()
            .filter(|&j| Some(owners[j]) == strokes.owners[i])
            .min_by(|&a, &b| {
                distance2(q, spatial.points[a])
                    .total_cmp(&distance2(q, spatial.points[b]))
                    .then(a.cmp(&b))
            });
        if nearest_owner.is_none() {
            return Err(crate::Error::Scaling(format!(
                "No source contour geometry for target pixel {i}"
            )));
        }
        let nearest_visible = candidates
            .into_iter()
            // The rasterizer owns topology and overlap decisions. A closer
            // sample from another colored contour must never repaint its core.
            .filter(|&j| Some(owners[j]) == strokes.owners[i] && colors[j].is_some())
            .min_by(|&a, &b| {
                distance2(q, spatial.points[a])
                    .total_cmp(&distance2(q, spatial.points[b]))
                    .then(a.cmp(&b))
            });
        let Some(j) = nearest_visible else {
            continue;
        };
        ink[i] = colors[j].unwrap();
        visible[i] = true;
    }
    Ok((ink, visible))
}

/// Colour fitted path geometry from detector models that belong to the same
/// contour owner.  Fitted curves and local detector models deliberately have
/// different parameterizations, so pairing their `u` values would allow a
/// long spline to sample the wrong piece of source ink.  Instead every spline
/// sample keeps the nearest same-owner source donor.
// The renderer, fitted geometry, and source-owner donor sets are independent
// inputs; a bundle would only obscure their distinct lifetimes at call sites.
#[allow(clippy::too_many_arguments)]
pub fn ink_colors_for_curves(
    source: &LinearImage,
    curves: &[Quadratic],
    curve_owners: &[usize],
    donors: &[Model],
    donor_owners: &[usize],
    trace_donors: &[([f64; 2], usize)],
    strokes: &super::strokes::Strokes,
    scale: [f64; 2],
    cancel: &dyn Cancellation,
) -> Result<(Vec<[f64; 3]>, Vec<bool>)> {
    if curves.len() != curve_owners.len() || donors.len() != donor_owners.len() {
        return Err(crate::Error::Scaling(
            "Invalid fitted contour donors".into(),
        ));
    }
    let mut donor_points = Vec::with_capacity(donors.len() * 8 + trace_donors.len());
    let mut donor_colors = Vec::with_capacity(donors.len() * 8 + trace_donors.len());
    let mut donor_ids = Vec::with_capacity(donors.len() * 8 + trace_donors.len());
    for (model, &owner) in donors.iter().zip(donor_owners) {
        cancel.check()?;
        let count = (((model[8] - model[7]).abs() / 0.2).ceil() as usize).max(1);
        for i in 0..=count {
            if i.is_multiple_of(4096) {
                cancel.check()?;
            }
            let u = model[7] + (model[8] - model[7]) * i as f64 / count as f64;
            let p = curve_point(model, u);
            donor_points.push(p);
            donor_colors.push(source_color(source, p));
            donor_ids.push(owner);
        }
    }
    for (i, &(p, owner)) in trace_donors.iter().enumerate() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        donor_points.push(p);
        donor_colors.push(source_color(source, p));
        donor_ids.push(owner);
    }
    let donor_tree = Spatial::new(donor_points, 2.);
    let mut points = Vec::new();
    let mut colors = Vec::new();
    let mut owners = Vec::new();
    for (&curve, &owner) in curves.iter().zip(curve_owners) {
        cancel.check()?;
        let control_length = (curve[1][0] - curve[0][0]).hypot(curve[1][1] - curve[0][1])
            + (curve[2][0] - curve[1][0]).hypot(curve[2][1] - curve[1][1]);
        // Keep target samples closer than half a target pixel. This retains
        // the existing two-pixel target donor search margin without attaching
        // fitted geometry to an unrelated source contour.
        let count = ((control_length * scale[0].max(scale[1]) / 0.35).ceil() as usize).max(2);
        for i in 0..=count {
            if i.is_multiple_of(4096) {
                cancel.check()?;
            }
            let t = i as f64 / count as f64;
            let mt = 1. - t;
            let p = [
                curve[0][0] * mt * mt + 2. * curve[1][0] * mt * t + curve[2][0] * t * t,
                curve[0][1] * mt * mt + 2. * curve[1][1] * mt * t + curve[2][1] * t * t,
            ];
            let candidates: Vec<_> = donor_tree
                .radius(p, 3.)
                .into_iter()
                .filter(|&id| donor_ids[id] == owner)
                .collect();
            let nearest_geometry = candidates.iter().copied().min_by(|&a, &b| {
                distance2(p, donor_tree.points[a])
                    .total_cmp(&distance2(p, donor_tree.points[b]))
                    .then(a.cmp(&b))
            });
            // A fit can cross a sparsely detected stretch of an otherwise
            // well-supported trace.  The spline itself belongs to `owner`, so
            // sample its original source location rather than borrowing a
            // nearby different contour.  This is also the safe alpha donor
            // for foreground-only compositing.
            let visible = candidates
                .into_iter()
                .filter(|&id| donor_colors[id].is_some())
                .min_by(|&a, &b| {
                    distance2(p, donor_tree.points[a])
                        .total_cmp(&distance2(p, donor_tree.points[b]))
                        .then(a.cmp(&b))
                });
            // Source geometry and source alpha are different facts. A
            // transparent nearest donor is not source ink, but it also does
            // not permit borrowing another contour's colour.
            let color = nearest_geometry
                .and(visible)
                .and_then(|id| donor_colors[id]);
            points.push(std::array::from_fn(|axis| {
                (p[axis] + 0.5) * scale[axis] - 0.5
            }));
            colors.push(color);
            owners.push(owner);
        }
    }
    let spatial = Spatial::new(points, 2.);
    let mut ink = vec![[0.; 3]; strokes.coverage.as_raw().len()];
    let mut visible = vec![false; strokes.coverage.as_raw().len()];
    for (i, alpha) in strokes.coverage.as_raw().iter().enumerate() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        if *alpha == 0 {
            continue;
        }
        let q = [
            (i % strokes.coverage.width() as usize) as f64,
            (i / strokes.coverage.width() as usize) as f64,
        ];
        let candidates = spatial.radius(q, 2.);
        let Some(_) = candidates
            .iter()
            .copied()
            .filter(|&id| Some(owners[id]) == strokes.owners[i])
            .min_by(|&a, &b| {
                distance2(q, spatial.points[a])
                    .total_cmp(&distance2(q, spatial.points[b]))
                    .then(a.cmp(&b))
            })
        else {
            return Err(crate::Error::Scaling(format!(
                "No fitted contour geometry for target pixel {i}"
            )));
        };
        let nearest_visible = candidates
            .into_iter()
            .filter(|&id| Some(owners[id]) == strokes.owners[i] && colors[id].is_some())
            .min_by(|&a, &b| {
                distance2(q, spatial.points[a])
                    .total_cmp(&distance2(q, spatial.points[b]))
                    .then(a.cmp(&b))
            });
        if let Some(j) = nearest_visible {
            let color = colors[j].expect("visible samples have color");
            ink[i] = color;
            visible[i] = true;
        }
    }
    Ok((ink, visible))
}

pub fn composite(base: &LinearImage, ink: &[[f64; 3]], coverage: &GrayImage) -> RgbaImage {
    RgbaImage::from_fn(base.w as u32, base.h as u32, |x, y| {
        let i = y as usize * base.w + x as usize;
        composite_pixel(base.pixels[i], ink[i], coverage.as_raw()[i])
    })
}

pub(crate) fn composite_pixel(base: [f64; 4], ink: [f64; 3], coverage: u8) -> image::Rgba<u8> {
    let a = f64::from(coverage) / 255.;
    let alpha = a + base[3] * (1. - a);
    if alpha <= 1e-8 {
        return image::Rgba([0; 4]);
    }
    rgba([
        (ink[0] * a + base[0] * base[3] * (1. - a)) / alpha,
        (ink[1] * a + base[1] * base[3] * (1. - a)) / alpha,
        (ink[2] * a + base[2] * base[3] * (1. - a)) / alpha,
        alpha,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{opacity, strokes::Strokes};

    fn owner_pixel(w: u32, h: u32, x: u32, y: u32, owner: usize) -> Strokes {
        let mut core = GrayImage::new(w, h);
        let mut coverage = GrayImage::new(w, h);
        core.put_pixel(x, y, image::Luma([255]));
        coverage.put_pixel(x, y, image::Luma([255]));
        let mut owners = vec![None; (w * h) as usize];
        owners[(y * w + x) as usize] = Some(owner);
        Strokes {
            core,
            coverage,
            owners,
        }
    }

    fn point_model(x: f64, y: f64) -> Model {
        [x, y, 0., 1., 0., 0., 0., 0., 0., 1.]
    }

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
        let coverage = opacity::apply(&strokes.coverage, &strokes.owners, &[1., 0.95]);
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

    #[test]
    fn transparent_foreground_donor_is_reported_not_replaced_by_background() {
        let model = [10., 10., 0., 1., 0., 0., 0., -8., 8., 1.];
        let mut strokes = Strokes {
            core: GrayImage::new(20, 20),
            coverage: GrayImage::new(20, 20),
            owners: vec![None; 400],
        };
        strokes.core.put_pixel(10, 10, image::Luma([255]));
        strokes.coverage.put_pixel(10, 10, image::Luma([255]));
        strokes.owners[210] = Some(0);
        let cancel = CancellationToken::default();
        let white = LinearImage::from_rgba(&RgbaImage::from_pixel(
            20,
            20,
            image::Rgba([245, 240, 230, 255]),
        ));
        let (colors, visible) = ink_colors_with_visibility(
            &white,
            &[model],
            &[model],
            &[0],
            &strokes,
            [1., 1.],
            &cancel,
        )
        .unwrap();
        assert!(visible[210], "opaque pale detail remains a valid donor");
        assert!(colors[210][0] > 0.8);

        let transparent = LinearImage::from_rgba(&RgbaImage::new(20, 20));
        let (_, visible) = ink_colors_with_visibility(
            &transparent,
            &[model],
            &[model],
            &[0],
            &strokes,
            [1., 1.],
            &cancel,
        )
        .unwrap();
        assert!(!visible[210], "unsupported foreground ink is suppressible");
    }

    #[test]
    fn fitted_curve_uses_original_same_owner_donors_over_shifted_green_geometry() {
        let source = LinearImage::from_rgba(&RgbaImage::from_fn(20, 20, |_, y| {
            image::Rgba(if y == 8 {
                [0, 0, 0, 255]
            } else {
                [60, 150, 30, 255]
            })
        }));
        let black = [10., 8., 0., 1., 0., 0., 0., -8., 8., 1.];
        let green = [10., 9., 0., 1., 0., 0., 0., -8., 8., 1.];
        let curve = [[2., 9.], [10., 9.], [18., 9.]];
        let strokes = owner_pixel(20, 20, 10, 9, 0);
        let (colors, visible) = ink_colors_for_curves(
            &source,
            &[curve],
            &[0],
            &[black, green],
            &[0, 1],
            &[],
            &strokes,
            [1., 1.],
            &CancellationToken::default(),
        )
        .unwrap();
        assert!(visible[190]);
        assert_eq!(colors[190], [0.; 3]);
    }

    #[test]
    fn fitted_curve_can_color_a_sparse_stretch_from_its_original_trace() {
        let mut image = RgbaImage::new(20, 20);
        image.put_pixel(10, 10, image::Rgba([0, 0, 0, 255]));
        let source = LinearImage::from_rgba(&image);
        let curve = [[8., 10.], [10., 10.], [12., 10.]];
        let strokes = owner_pixel(20, 20, 10, 10, 0);
        let (colors, visible) = ink_colors_for_curves(
            &source,
            &[curve],
            &[0],
            &[],
            &[],
            &[([10., 10.], 0)],
            &strokes,
            [1., 1.],
            &CancellationToken::default(),
        )
        .unwrap();
        assert!(visible[210]);
        assert_eq!(colors[210], [0.; 3]);
    }

    #[test]
    fn transparent_nearest_curve_sample_does_not_hide_nearby_visible_same_owner_ink() {
        let mut image = RgbaImage::new(30, 30);
        image.put_pixel(10, 10, image::Rgba([0, 0, 0, 255]));
        let source = LinearImage::from_rgba(&image);
        let curve = [[10., 10.], [15., 10.], [20., 10.]];
        let strokes = owner_pixel(4, 4, 1, 1, 0);
        let (colors, visible) = ink_colors_for_curves(
            &source,
            &[curve],
            &[0],
            &[point_model(10., 10.), point_model(13.333_333, 10.)],
            &[0, 0],
            &[],
            &strokes,
            [0.1, 0.1],
            &CancellationToken::default(),
        )
        .unwrap();
        assert!(visible[5]);
        assert_eq!(colors[5], [0.; 3]);
    }

    #[test]
    fn fitted_curve_never_borrows_a_different_owner_donor() {
        let source = LinearImage::from_rgba(&RgbaImage::from_pixel(
            20,
            20,
            image::Rgba([60, 150, 30, 255]),
        ));
        let curve = [[8., 10.], [10., 10.], [12., 10.]];
        let strokes = owner_pixel(20, 20, 10, 10, 0);
        let (colors, visible) = ink_colors_for_curves(
            &source,
            &[curve],
            &[0],
            &[],
            &[],
            &[([10., 10.], 1)],
            &strokes,
            [1., 1.],
            &CancellationToken::default(),
        )
        .unwrap();
        assert!(!visible[210]);
        assert_eq!(colors[210], [0.; 3]);
    }
}
