//! Connected digital cores and geometric coverage for one source contour.
//! An 8x8 bit mask per touched pixel unions samples across overlapping patches:
//! repeated patches and subdivision never stack ink.
use super::raster::{Mask, Zingl};
use crate::{document::CancellationToken, error::Result};

pub type Quadratic = [[f64; 2]; 3];
const TOLERANCE: f64 = 1. / 2048.;
const GRID: usize = 8;
const RADIUS: f64 = 0.375; // Superpixelator-inspired 0.75-pixel sampling stroke.

pub struct Sampled {
    pub core: Mask,
    pub distances: Vec<f64>,
    pub area: Vec<f64>,
}

fn midpoint(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
}

fn flatten(q: Quadratic, depth: usize, segments: &mut Vec<[[f64; 2]; 2]>) {
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

pub(super) fn valid_curves(curves: &[Quadratic]) -> bool {
    curves
        .iter()
        .flatten()
        .flatten()
        .all(|v| v.is_finite() && v.abs() <= 1e6)
}

pub fn rasterize(
    curves: &[Quadratic],
    w: usize,
    h: usize,
    cancel: &CancellationToken,
) -> Result<Sampled> {
    cancel.check()?;
    if w == 0 || h == 0 || !valid_curves(curves) {
        return Err(crate::error::AppError::Scaling(
            "Invalid contour coordinates".into(),
        ));
    }
    let mut raster = Zingl::new(w, h);
    let mut distances = vec![f64::INFINITY; w * h];
    let mut samples = vec![0_u64; w * h];
    let mut segments = Vec::new();
    for &curve in curves {
        cancel.check()?;
        let mut q = curve;
        // Make raster ties and subdivision independent of patch direction.
        if q[0] > q[2] {
            q.reverse();
        }
        raster
            .quadratic(q)
            .map_err(crate::error::AppError::Scaling)?;
        segments.clear();
        flatten(q, 0, &mut segments);
        for &[a, b] in &segments {
            cancel.check()?;
            let d = [b[0] - a[0], b[1] - a[1]];
            let length2 = d[0] * d[0] + d[1] * d[1];
            let inverse = if length2 > 1e-24 { 1. / length2 } else { 0. };
            let distance2 = |x: f64, y: f64| {
                let e = [x - a[0], y - a[1]];
                let t = ((e[0] * d[0] + e[1] * d[1]) * inverse).clamp(0., 1.);
                let v = [e[0] - t * d[0], e[1] - t * d[1]];
                v[0] * v[0] + v[1] * v[1]
            };
            // radius + half a pixel is < 1. The extra margin also supplies
            // distances for digital pixels selected by the integer rasterizer.
            let lower = |v: f64, n: usize| (v - 1.).ceil().clamp(0., n as f64) as usize;
            let upper = |v: f64, n: usize| ((v + 1.).floor() + 1.).clamp(0., n as f64) as usize;
            for y in lower(a[1].min(b[1]), h)..upper(a[1].max(b[1]), h) {
                cancel.check()?;
                for x in lower(a[0].min(b[0]), w)..upper(a[0].max(b[0]), w) {
                    let i = y * w + x;
                    let center_distance2 = distance2(x as f64, y as f64);
                    distances[i] = distances[i].min(center_distance2);
                    if center_distance2 > (RADIUS + std::f64::consts::FRAC_1_SQRT_2).powi(2) {
                        continue;
                    }
                    let mut bits = samples[i];
                    for sy in 0..GRID {
                        let py = y as f64 - 0.5 + (sy as f64 + 0.5) / GRID as f64;
                        for sx in 0..GRID {
                            let bit = 1_u64 << (sy * GRID + sx);
                            if bits & bit != 0 {
                                continue;
                            }
                            let px = x as f64 - 0.5 + (sx as f64 + 0.5) / GRID as f64;
                            if distance2(px, py) <= RADIUS * RADIUS {
                                bits |= bit;
                            }
                        }
                    }
                    samples[i] = bits;
                }
            }
        }
    }
    Ok(Sampled {
        core: raster.image,
        distances,
        area: samples
            .into_iter()
            .map(|b| b.count_ones() as f64 / (GRID * GRID) as f64)
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn line(a: [f64; 2], b: [f64; 2]) -> Quadratic {
        [a, midpoint(a, b), b]
    }

    #[test]
    fn horizontal_area_matches_exact_pixel_intersections() {
        let cancel = CancellationToken::default();
        for (y, expected) in [(2., [0.75, 0.]), (2.25, [0.625, 0.125])] {
            let r = rasterize(&[line([1., y], [9., y])], 12, 6, &cancel).unwrap();
            for x in 3..8 {
                assert_eq!(r.area[2 * 12 + x], expected[0]);
                assert_eq!(r.area[3 * 12 + x], expected[1]);
            }
        }
    }

    #[test]
    fn duplicate_and_reversed_patches_do_not_change_coverage() {
        let q = [[1.25, 2.4], [5.8, 2.], [9.2, 7.3]];
        let mut reversed = q;
        reversed.reverse();
        let cancel = CancellationToken::default();
        let a = rasterize(&[q], 12, 10, &cancel).unwrap();
        let b = rasterize(&[reversed, q, q], 12, 10, &cancel).unwrap();
        assert_eq!(a.core.data, b.core.data);
        assert_eq!(a.area, b.area);
        assert_eq!(a.distances, b.distances);
        assert_eq!(
            super::super::cleanup::labels(&a.core, true, true).1.len(),
            2
        );
    }

    #[test]
    fn invalid_input_and_cancellation_fail_without_rendering() {
        let cancel = CancellationToken::default();
        for v in [f64::NAN, f64::INFINITY, 1e7] {
            assert!(rasterize(&[line([v, 0.], [1., 1.])], 4, 4, &cancel).is_err());
        }
        assert!(rasterize(&[], 0, 4, &cancel).is_err());
        cancel.cancel();
        assert!(matches!(
            rasterize(&[], 4, 4, &cancel),
            Err(crate::error::AppError::Cancelled)
        ));
    }

    #[test]
    fn sampled_stroke_agrees_with_an_independent_dense_distance_oracle() {
        // A separate projection formula and 64x64 reference grid. The fixed
        // 1/8 coverage tolerance is one production sample-row, not a fit to
        // rendered fixtures. Check different slopes, phases, caps and clipping.
        let cancel = CancellationToken::default();
        for (a, b) in [
            ([0.2, 1.3], [7.6, 5.1]),
            ([-1.2, 3.7], [7.8, 0.4]),
            ([3.25, 1.1], [3.6, 7.9]),
            ([2., 2.], [2., 2.]),
        ] {
            let result = rasterize(&[line(a, b)], 9, 9, &cancel).unwrap();
            for y in 0..9 {
                for x in 0..9 {
                    let mut count = 0;
                    for sy in 0..64 {
                        for sx in 0..64 {
                            let p = [
                                x as f64 - 0.5 + (sx as f64 + 0.5) / 64.,
                                y as f64 - 0.5 + (sy as f64 + 0.5) / 64.,
                            ];
                            let ab = [b[0] - a[0], b[1] - a[1]];
                            let len = ab[0].hypot(ab[1]);
                            let d = if len == 0. {
                                (p[0] - a[0]).hypot(p[1] - a[1])
                            } else {
                                let along = ((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / len;
                                if along <= 0. {
                                    (p[0] - a[0]).hypot(p[1] - a[1])
                                } else if along >= len {
                                    (p[0] - b[0]).hypot(p[1] - b[1])
                                } else {
                                    ((p[0] - a[0]) * ab[1] - (p[1] - a[1]) * ab[0]).abs() / len
                                }
                            };
                            if d <= 0.375 {
                                count += 1;
                            }
                        }
                    }
                    let expected = count as f64 / 4096.;
                    assert!(
                        (result.area[y * 9 + x] - expected).abs() <= 0.125,
                        "({x},{y}) on {a:?}->{b:?}"
                    );
                }
            }
        }
    }
}
