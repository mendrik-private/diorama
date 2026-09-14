//! Subpixel quadratic stroke coverage. Curves are already detected models: the
//! subdivision here is rendering, never contour detection or path tracing.
//! All coordinates and stroke widths are in FINAL output-pixel units.
use crate::tools::scale::game_asset::raster::Mask;
use crate::{document::CancellationToken, error::Result};

pub type Quadratic = [[f64; 2]; 3];
const TOLERANCE: f64 = 1. / 2048.;

fn midpoint(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
}

fn flatten(q: Quadratic, depth: usize, segments: &mut Vec<[[f64; 2]; 2]>) {
    // Quadratic deviation from its chord is bounded by half the distance
    // from the middle control to the chord midpoint, including cusp/reversal.
    let middle = midpoint(q[0], q[2]);
    let error = (q[1][0] - middle[0]).hypot(q[1][1] - middle[1]) * 0.5;
    if error <= TOLERANCE || depth >= 24 {
        segments.push([q[0], q[2]]);
        return;
    }
    let a = midpoint(q[0], q[1]);
    let b = midpoint(q[1], q[2]);
    let c = midpoint(a, b);
    flatten([q[0], a, c], depth + 1, segments);
    flatten([c, b, q[2]], depth + 1, segments);
}

fn distance_squared(p: [f64; 2], segment: [[f64; 2]; 2]) -> f64 {
    let [a, b] = segment;
    let d = [b[0] - a[0], b[1] - a[1]];
    let e = [p[0] - a[0], p[1] - a[1]];
    let length = d[0] * d[0] + d[1] * d[1];
    let t = if length > 1e-24 {
        ((e[0] * d[0] + e[1] * d[1]) / length).clamp(0., 1.)
    } else {
        0.
    };
    (e[0] - t * d[0]).powi(2) + (e[1] - t * d[1]).powi(2)
}

fn valid_curves(curves: &[Quadratic]) -> bool {
    curves
        .iter()
        .flatten()
        .flatten()
        .all(|v| v.is_finite() && v.abs() <= 1e6)
}

/// Binary, direction-dependent digital stroke. The local half-width is
/// max(|nx|, |ny|)/2, selecting the nearest row/column on a straight interior.
/// Exact boundary ties choose the positive canonical normal side, independent
/// of patch direction. End caps use the same reduced radius. Geometry is never
/// rounded; nearby, nonidentical patches can still need overlap cleanup.
/// Also returns minor-axis distances in a one-pixel neighborhood for tight AA.
pub fn render_digital(
    curves: &[Quadratic],
    w: usize,
    h: usize,
    cancel: &CancellationToken,
) -> Result<(Mask, Vec<f64>)> {
    if w == 0 || h == 0 || !valid_curves(curves) {
        return Err(crate::error::AppError::Scaling(
            "Invalid contour coordinates".into(),
        ));
    }
    let mut mask = Mask::new(w, h);
    let mut distances = vec![f64::INFINITY; w * h];
    let mut segments = Vec::new();
    for &q in curves {
        cancel.check()?;
        segments.clear();
        flatten(q, 0, &mut segments);
        for &segment in &segments {
            cancel.check()?;
            let [a, b] = segment;
            let dx = b[0] - a[0];
            let dy = b[1] - a[1];
            let length = dx.hypot(dy);
            // A point has no direction. Assign its nearest pixel deterministically.
            if length <= 1e-12 {
                let x = (a[0] + 0.5).floor() as isize;
                let y = (a[1] + 0.5).floor() as isize;
                if x >= 0 && y >= 0 && x < w as isize && y < h as isize {
                    let i = y as usize * w + x as usize;
                    mask.data[i] = true;
                    distances[i] = distances[i].min((x as f64 - a[0]).hypot(y as f64 - a[1]));
                }
                continue;
            }
            let radius = 0.5 * dx.abs().max(dy.abs()) / length;
            let mut normal = [-dy / length, dx / length];
            let major = usize::from(normal[1].abs() >= normal[0].abs());
            if normal[major] < 0. {
                normal = normal.map(|v| -v);
            }
            let lower = |v: f64, size: usize| (v - 1.).ceil().clamp(0., size as f64) as usize;
            let upper =
                |v: f64, size: usize| ((v + 1.).floor() + 1.).clamp(0., size as f64) as usize;
            for y in lower(a[1].min(b[1]), h)..upper(a[1].max(b[1]), h) {
                for x in lower(a[0].min(b[0]), w)..upper(a[0].max(b[0]), w) {
                    let p = [x as f64, y as f64];
                    let distance = distance_squared(p, segment).sqrt();
                    let i = y * w + x;
                    distances[i] = distances[i].min(distance / (2. * radius));
                    let tie = (distance - radius).abs() <= 1e-12;
                    let positive = (p[0] - a[0]) * normal[0] + (p[1] - a[1]) * normal[1] >= 0.;
                    if (distance < radius && !tie) || (tie && positive) {
                        mask.data[i] = true;
                    }
                }
            }
        }
    }
    Ok((mask, distances))
}
