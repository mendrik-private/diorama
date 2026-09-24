use crate::field::{Field, Spatial, distance2};
use crate::{Cancellation, Result};
use image::RgbaImage;
use std::collections::HashMap;

pub type Sample = [f64; 8];
pub type Model = [f64; 10];

pub fn detect(image: &RgbaImage, cancel: &dyn Cancellation) -> Result<Vec<Sample>> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let mut lum = Field::new(w, h);
    let mut alpha = Field::new(w, h);
    for (i, p) in image.pixels().enumerate() {
        let a = p[3] as f64 / 255.;
        alpha.data[i] = a;
        lum.data[i] =
            (p[0] as f64 * 0.2126 + p[1] as f64 * 0.7152 + p[2] as f64 * 0.0722) / 255. * a + 1.
                - a;
    }
    let mut records = Vec::new();
    let mut fine = None;
    // Preserve the existing scales and add one narrow-ink pass.  At 0.5px,
    // the derivative kernel remains a finite 5-tap filter while retaining a
    // one-pixel asymmetric outline that the 0.65px minimum smears away.
    for sigma in [0.5f64, 0.65, 1., 1.5, 2.2] {
        let [l, gx, gy, hxx, hyy, hxy] = lum.gaussian_derivatives(sigma, cancel)?;
        if fine.is_none() {
            fine = Some(l.clone());
        }
        for (dx, dy) in [(1f64, 0f64), (0., 1.), (1., 1.), (1., -1.)] {
            for y in 5..h.saturating_sub(5) {
                cancel.check()?;
                for x in 5..w.saturating_sub(5) {
                    let i = y * w + x;
                    let (px, py) = (x as f64, y as f64);
                    if alpha.data[i] <= 0.15 || l.data[i] >= 0.52 {
                        continue;
                    }
                    let angle = 0.5 * (2. * hxy.data[i]).atan2(hxx.data[i] - hyy.data[i]);
                    let (ny, nx) = angle.sin_cos();
                    if (nx * dx + ny * dy).abs() / dx.hypot(dy)
                        < (std::f64::consts::PI / 8.).cos() - 1e-7
                    {
                        continue;
                    }
                    if l.data[i] > l.sample(px - dx, py - dy)
                        || l.data[i] >= l.sample(px + dx, py + dy)
                    {
                        continue;
                    }
                    let disc = (hxx.data[i] - hyy.data[i]).hypot(2. * hxy.data[i]);
                    let ln = 0.5 * (hxx.data[i] + hyy.data[i] + disc);
                    let lt = 0.5 * (hxx.data[i] + hyy.data[i] - disc);
                    if ln <= 0.0025 / sigma.powi(2) || ln <= 1.7 * lt.abs() {
                        continue;
                    }
                    let offset = -(gx.data[i] * nx + gy.data[i] * ny) / ln;
                    if offset.abs() >= 0.9 {
                        continue;
                    }
                    let (px, py) = (px + offset * nx, py + offset * ny);
                    let center = l.sample(px, py);
                    let shoulder = (2.5 * sigma).max(1.6);
                    let left = l.sample(px - shoulder * nx, py - shoulder * ny);
                    let right = l.sample(px + shoulder * nx, py + shoulder * ny);
                    let contrast = left.min(right) - center;
                    let strongside = left.max(right) - center;
                    let ratio = contrast / left.min(right).max(0.025);
                    let score = contrast * (0.5 + 0.5 * ratio) * (1. - center);
                    if contrast > 0.012 && strongside > 0.055 && ratio > 0.075 {
                        if records.len() >= 500_000 {
                            return Err(crate::Error::GameAssetMemoryLimit {
                                limit_bytes: super::DEFAULT_MEMORY_LIMIT,
                            });
                        }
                        records.push([px, py, nx, ny, score, contrast, center, sigma]);
                    }
                }
            }
        }
    }
    records.sort_by(|a, b| b[4].total_cmp(&a[4]));
    let mut bins: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
    let mut out: Vec<Sample> = Vec::new();
    for p in records {
        cancel.check()?;
        let (bx, by) = ((p[0] / 0.8).floor() as i32, (p[1] / 0.8).floor() as i32);
        let mut duplicate = false;
        for y in by - 1..=by + 1 {
            for x in bx - 1..=bx + 1 {
                if let Some(ids) = bins.get(&(x, y)) {
                    duplicate |= ids.iter().any(|&i| {
                        let q = out[i];
                        distance2([p[0], p[1]], [q[0], q[1]]) < 0.8f64.powi(2)
                            && (p[2] * q[2] + p[3] * q[3]).abs() > 0.8
                    });
                }
            }
        }
        if !duplicate {
            bins.entry((bx, by)).or_default().push(out.len());
            out.push(p);
        }
    }
    match fine {
        Some(fine) => merge_scale_duplicates(out, &fine, cancel),
        None => Ok(out),
    }
}

/// Farthest normal separation, beyond the sample scale, at which two aligned
/// ridge samples can still describe one ink band.
const SAME_BAND_REACH: f64 = 1.5;
/// A lighter valley of at least this luminance separates two parallel strokes.
const SEPARATING_GAP: f64 = 0.05;

/// Collapse parallel ridge samples that describe one physical ink band.
///
/// A wide or asymmetric outline, such as black ink beside dark shading, pulls
/// the coarse-scale ridge up to 1.5px off the fine-scale ink core. Both
/// survive the isotropic 0.8px suppression, and their rasterized union then
/// leaves slivers that thinning preserves as ladders and small loops. Samples
/// are the same band when they are aligned, displaced mostly along their
/// normal and joined by continuous ink: the lightly smoothed luminance never
/// rises by [`SEPARATING_GAP`] between them. The darkest sample is the ink core
/// and represents the band; distinct nearby strokes keep their lighter gap.
fn merge_scale_duplicates(
    samples: Vec<Sample>,
    fine: &Field,
    cancel: &dyn Cancellation,
) -> Result<Vec<Sample>> {
    let centers: Vec<f64> = samples.iter().map(|p| fine.sample(p[0], p[1])).collect();
    let mut order: Vec<usize> = (0..samples.len()).collect();
    order.sort_by(|&a, &b| {
        centers[a]
            .total_cmp(&centers[b])
            .then(samples[b][4].total_cmp(&samples[a][4]))
            .then(a.cmp(&b))
    });
    let tree = Spatial::new(samples.iter().map(|p| [p[0], p[1]]).collect(), 4.);
    let mut removed = vec![false; samples.len()];
    for (position, &i) in order.iter().enumerate() {
        if position.is_multiple_of(4096) {
            cancel.check()?;
        }
        if removed[i] {
            continue;
        }
        let p = samples[i];
        let reach_limit = SAME_BAND_REACH + 2.2;
        for j in tree.radius([p[0], p[1]], reach_limit) {
            if j == i || removed[j] {
                continue;
            }
            let q = samples[j];
            if (p[2] * q[2] + p[3] * q[3]).abs() <= 0.8 {
                continue;
            }
            let (dx, dy) = (q[0] - p[0], q[1] - p[1]);
            let along = (-dx * p[3] + dy * p[2]).abs();
            let across = (dx * p[2] + dy * p[3]).abs();
            if along > 0.6 || across > SAME_BAND_REACH + p[7].max(q[7]) {
                continue;
            }
            let steps = (across / 0.25).ceil().max(1.) as usize;
            let peak = (1..steps)
                .map(|k| {
                    let t = k as f64 / steps as f64;
                    fine.sample(p[0] + dx * t, p[1] + dy * t)
                })
                .fold(f64::NEG_INFINITY, f64::max);
            if peak < centers[i].max(centers[j]) + SEPARATING_GAP {
                removed[j] = true;
            }
        }
    }
    Ok(samples
        .into_iter()
        .zip(removed)
        .filter_map(|(sample, removed)| (!removed).then_some(sample))
        .collect())
}

pub fn fit_models(
    samples: &[Sample],
    threshold: f64,
    cancel: &dyn Cancellation,
) -> Result<Vec<Model>> {
    let points: Vec<_> = samples.iter().filter(|p| p[4] >= threshold).collect();
    let tree = Spatial::new(points.iter().map(|p| [p[0], p[1]]).collect(), 5.);
    let mut models = Vec::new();
    for p in &points {
        cancel.check()?;
        let (nx, ny) = (p[2], p[3]);
        let mut near = Vec::new();
        for i in tree.radius([p[0], p[1]], 5.) {
            let q = points[i];
            let (dx, dy) = (q[0] - p[0], q[1] - p[1]);
            let (u, v) = (-dx * ny + dy * nx, dx * nx + dy * ny);
            let align = q[2] * nx + q[3] * ny;
            if align.abs() <= 0.76 || v.abs() >= 1.15 + 0.03 * u * u {
                continue;
            }
            let weight =
                (-0.5 * (u / 2.5).powi(2) - 0.5 * v * v).exp() * (q[4] / p[4].max(0.015)).min(2.);
            let slope = -(-q[2] * ny + q[3] * nx) / align;
            near.push((u, v, weight, slope));
        }
        if near.len() < 3 {
            continue;
        }
        let mut robust: Vec<_> = near.iter().map(|q| q.2).collect();
        let mut coef = [0.; 3];
        for _ in 0..2 {
            let mut mat = [[0.; 3]; 3];
            let mut rhs = [0.; 3];
            for (i, &(u, v, w, slope)) in near.iter().enumerate() {
                let a = [1., u, u * u];
                let d = [0., 1., 2. * u];
                for r in 0..3 {
                    rhs[r] += robust[i] * a[r] * v + w * 0.25 * d[r] * slope;
                    for c in 0..3 {
                        mat[r][c] += robust[i] * a[r] * a[c] + w * 0.25 * d[r] * d[c];
                    }
                }
            }
            for (i, r) in [0.001f64, 0.015, 0.1].iter().enumerate() {
                mat[i][i] += r * r;
            }
            coef = solve(mat, rhs);
            for (i, &(u, v, w, _)) in near.iter().enumerate() {
                let residual = (coef[0] + coef[1] * u + coef[2] * u * u - v).abs();
                robust[i] = w * (0.35 / residual.max(1e-9)).min(1.);
            }
        }
        if coef[0].abs() > 0.9 || coef[2].abs() > 0.3 {
            coef = [0.; 3];
        }
        let lo = (-1f64).max(near.iter().map(|q| q.0).fold(f64::INFINITY, f64::min) - 0.5);
        let hi = 1f64.min(near.iter().map(|q| q.0).fold(f64::NEG_INFINITY, f64::max) + 0.5);
        models.push([p[0], p[1], nx, ny, coef[0], coef[1], coef[2], lo, hi, p[4]]);
    }
    Ok(models)
}

/// Positive-definite 3x3 normal system; ridge terms make every pivot positive.
pub(crate) fn solve(mut a: [[f64; 3]; 3], mut b: [f64; 3]) -> [f64; 3] {
    for k in 0..3 {
        for i in k + 1..3 {
            let f = a[i][k] / a[k][k];
            let pivot_row = a[k];
            for (cell, pivot) in a[i][k..].iter_mut().zip(&pivot_row[k..]) {
                *cell -= f * pivot;
            }
            b[i] -= f * b[k];
        }
    }
    let mut x = [0.; 3];
    for i in (0..3).rev() {
        x[i] = (b[i] - (i + 1..3).map(|j| a[i][j] * x[j]).sum::<f64>()) / a[i][i];
    }
    x
}

pub fn curve_point(m: &Model, u: f64) -> [f64; 2] {
    let v = m[4] + m[5] * u + m[6] * u * u;
    [m[0] - u * m[3] + v * m[2], m[1] + u * m[2] + v * m[3]]
}

pub fn controls(m: &Model, scale: f64) -> [[f64; 2]; 3] {
    let p0 = curve_point(m, m[7]);
    let p2 = curve_point(m, m[8]);
    let pm = curve_point(m, (m[7] + m[8]) * 0.5);
    let p1 = [
        2. * pm[0] - (p0[0] + p2[0]) * 0.5,
        2. * pm[1] - (p0[1] + p2[1]) * 0.5,
    ];
    [p0, p1, p2].map(|p| [(p[0] + 0.5) * scale - 0.5, (p[1] + 0.5) * scale - 0.5])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CancellationToken, source};

    // A native-resolution crop of the reported tail has at least 12 source
    // pixels around the covered diagonal.  That exceeds the 9px radius of the
    // largest 2.2px Gaussian kernel, so filter reflection cannot create it.
    const TAIL: &[u8] = include_bytes!("../tests/fixtures/red_dragon_tail.png");
    const MISSING_EDGE: [[usize; 2]; 33] = [
        [65, 20],
        [64, 21],
        [63, 22],
        [62, 23],
        [60, 24],
        [59, 25],
        [58, 26],
        [57, 27],
        [56, 28],
        [55, 29],
        [54, 30],
        [53, 31],
        [51, 32],
        [50, 33],
        [49, 34],
        [48, 35],
        [47, 36],
        [46, 37],
        [46, 38],
        [45, 39],
        [44, 40],
        [43, 41],
        [42, 42],
        [41, 43],
        [40, 44],
        [39, 45],
        [39, 46],
        [38, 47],
        [37, 48],
        [36, 49],
        [35, 50],
        [34, 51],
        [34, 52],
    ];

    fn tail_fixture() -> RgbaImage {
        image::load_from_memory_with_format(TAIL, image::ImageFormat::Png)
            .unwrap()
            .into_rgba8()
    }

    fn covered_points(mask: &crate::raster::Mask) -> usize {
        MISSING_EDGE
            .iter()
            .filter(|&&[x, y]| {
                (-1isize..=1)
                    .any(|dy| (-1isize..=1).any(|dx| mask.at(x as isize + dx, y as isize + dy)))
            })
            .count()
    }

    fn source_mask(image: &RgbaImage) -> crate::raster::Mask {
        let cancel = CancellationToken::default();
        let samples = detect(image, &cancel).unwrap();
        let models = fit_models(&samples, 0.012, &cancel).unwrap();
        source::skeleton(
            &models,
            image.width() as usize,
            image.height() as usize,
            &cancel,
        )
        .unwrap()
    }

    fn ridge_offsets(samples: &[Sample], y: std::ops::Range<f64>) -> Vec<f64> {
        let mut xs: Vec<_> = samples
            .iter()
            .filter(|s| y.contains(&s[1]))
            .map(|s| s[0])
            .collect();
        xs.sort_by(f64::total_cmp);
        xs
    }

    #[test]
    fn black_outline_beside_dark_shading_yields_one_ridge_track() {
        // A 5px black outline on white, with dark shading on its inner side:
        // coarse scales see a ridge pulled toward the shading.
        let image = RgbaImage::from_fn(64, 48, |x, _| {
            image::Rgba(match x {
                24..29 => [8, 8, 8, 255],
                29..34 => [70, 45, 25, 255],
                34.. => [170, 110, 60, 255],
                _ => [255, 255, 255, 255],
            })
        });
        let cancel = CancellationToken::default();
        let samples = detect(&image, &cancel).unwrap();
        let xs = ridge_offsets(&samples, 20.0..21.0);
        assert!(!xs.is_empty(), "the outline must be detected");
        assert!(
            xs.iter().all(|&x| (x - 26.).abs() < 1.6),
            "one track inside the black band, got {xs:?}"
        );
        let skeleton = source_mask(&image);
        let row: Vec<_> = (0..64).filter(|&x| skeleton.data[20 * 64 + x]).collect();
        assert_eq!(row.len(), 1, "one centerline, got {row:?}");
    }

    #[test]
    fn parallel_strokes_with_a_light_gap_keep_both_tracks() {
        let image = RgbaImage::from_fn(64, 48, |x, _| {
            image::Rgba(if x == 24 || x == 27 {
                [10, 10, 10, 255]
            } else {
                [240, 240, 240, 255]
            })
        });
        let cancel = CancellationToken::default();
        let xs = ridge_offsets(&detect(&image, &cancel).unwrap(), 20.0..21.0);
        assert!(xs.iter().any(|&x| (x - 24.).abs() < 0.5), "{xs:?}");
        assert!(xs.iter().any(|&x| (x - 27.).abs() < 0.5), "{xs:?}");
    }

    #[test]
    fn narrow_asymmetric_tail_outline_survives_source_extraction() {
        let image = tail_fixture();
        let thinned = source_mask(&image);
        assert!(
            covered_points(&thinned) == MISSING_EDGE.len(),
            "expected dense 1px-tolerant coverage of the 33px asymmetric tail outline; got {}",
            covered_points(&thinned),
        );
    }

    #[test]
    fn narrow_scale_keeps_flat_steps_transparency_ramps_and_isolated_noise_empty() {
        let opaque_flat = RgbaImage::from_pixel(96, 64, image::Rgba([30, 30, 30, 255]));
        let opaque_step = RgbaImage::from_fn(96, 64, |x, _| {
            image::Rgba(if x < 48 {
                [20, 20, 20, 255]
            } else {
                [245, 245, 245, 255]
            })
        });
        let ramp = RgbaImage::from_fn(96, 64, |x, _| {
            let value = 20 + (x * 220 / 95) as u8;
            image::Rgba([value, value, value, 255])
        });
        let transparent_noise = RgbaImage::from_fn(96, 64, |x, y| {
            image::Rgba([(x * 37) as u8, (y * 73) as u8, ((x + y) * 11) as u8, 0])
        });
        let isolated_noise = RgbaImage::from_pixel(96, 64, image::Rgba([250, 250, 250, 255]));
        let mut isolated_noise = isolated_noise;
        isolated_noise.put_pixel(48, 32, image::Rgba([0, 0, 0, 255]));
        for (name, image) in [
            ("opaque flat", opaque_flat),
            ("plain dark/light step", opaque_step),
            ("smooth ramp", ramp),
            ("transparent hidden RGB", transparent_noise),
            ("isolated dark pixel", isolated_noise),
        ] {
            let mask = source_mask(&image);
            assert!(
                mask.data.iter().all(|&on| !on),
                "{name} produced a source contour"
            );
        }
    }
}
