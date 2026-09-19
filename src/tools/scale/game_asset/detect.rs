use crate::tools::scale::game_asset::field::{Field, Spatial, distance2};
use crate::{document::CancellationToken, error::Result};
use image::RgbaImage;
use std::collections::HashMap;

pub type Sample = [f64; 8];
pub type Model = [f64; 10];

pub fn detect(image: &RgbaImage, cancel: &CancellationToken) -> Result<Vec<Sample>> {
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
    for sigma in [0.65f64, 1., 1.5, 2.2] {
        let [l, gx, gy, hxx, hyy, hxy] = lum.gaussian_derivatives(sigma, cancel)?;
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
                            return Err(crate::error::AppError::GameAssetMemoryLimit {
                                limit_bytes: super::MEMORY_BUDGET,
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
    Ok(out)
}

pub fn fit_models(
    samples: &[Sample],
    threshold: f64,
    cancel: &CancellationToken,
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
