//! Target-scale moving least squares on unordered, orientation-compatible patches.
//! No paths; quadratic shape survives while source-scale ripples are attenuated.
use crate::tools::scale::game_asset::{
    detect::{Model, curve_point, solve},
    field::Spatial,
};
use crate::{document::CancellationToken, error::Result};

pub fn smooth(original: &[Model], scale: f64, cancel: &CancellationToken) -> Result<Vec<Model>> {
    let strength = 6.4;
    if scale >= 1. {
        return Ok(original.to_vec());
    }
    let centers: Vec<_> = original.iter().map(|m| curve_point(m, 0.)).collect();
    let tree = Spatial::new(centers.clone(), 5.);
    // The selected 6.4-pixel sigma shrinks continuously to zero at source scale.
    let sigma = strength * (1. - scale * scale).sqrt();
    let radius = (2.5 * sigma / scale).max(2.);
    let mut result = original.to_vec();
    for (id, m) in original.iter().enumerate() {
        cancel.check()?;
        let center = centers[id];
        let n = [m[2], m[3]];
        let t = [-n[1], n[0]];
        let mut observations = Vec::new();
        for j in tree.radius(center, radius) {
            let q = &original[j];
            let d = [centers[j][0] - m[0], centers[j][1] - m[1]];
            let u = d[0] * t[0] + d[1] * t[1];
            let v = d[0] * n[0] + d[1] * n[1];
            let align = q[2] * n[0] + q[3] * n[1];
            let predicted = m[4] + m[5] * u + m[6] * u * u;
            // Narrow source-space cross-contour gate: increasing tangent support
            // must not average adjacent parallel strokes or crossing directions.
            if align.abs() < 0.85 || (v - predicted).abs() > 1.25 {
                continue;
            }
            let weight = (-0.5 * (u * scale / sigma.max(0.01)).powi(2)
                - 0.5 * ((v - predicted) / 0.65).powi(2))
            .exp()
                * (q[9] / m[9].max(0.015)).clamp(0.25, 2.);
            let shifted = curve_point(q, 0.);
            let shifted_v = ((shifted[0] - m[0]) * n[0] + (shifted[1] - m[1]) * n[1]) * scale;
            observations.push((u * scale, shifted_v, weight));
        }
        if observations.len() < 5 {
            continue;
        }
        let mut coef = [m[4] * scale, m[5], m[6] / scale];
        for iteration in 0..3 {
            let mut a = [[0.; 3]; 3];
            let mut b = [0.; 3];
            for &(u, v, weight) in &observations {
                let residual = (coef[0] + coef[1] * u + coef[2] * u * u - v).abs();
                let w = weight
                    * if iteration == 0 {
                        1.
                    } else {
                        (0.12 / residual.max(1e-9)).min(1.)
                    };
                let row = [1., u, u * u];
                for r in 0..3 {
                    b[r] += w * row[r] * v;
                    for c in 0..3 {
                        a[r][c] += w * row[r] * row[c];
                    }
                }
            }
            for (i, ridge) in [1e-6, 0.005, 0.025].into_iter().enumerate() {
                a[i][i] += ridge;
            }
            coef = solve(a, b);
        }
        // Keep position changes local. Sharp orientation changes are already
        // excluded above; no global scale or silhouette shrink is introduced.
        coef[0] = coef[0].clamp(m[4] * scale - 0.5, m[4] * scale + 0.5);
        let mut fitted = *m;
        fitted[4] = coef[0] / scale;
        fitted[5] = coef[1];
        fitted[6] = coef[2] * scale;
        result[id] = fitted;
    }
    Ok(result)
}
