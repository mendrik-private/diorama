//! Keep contour identity through digital cleanup, AA, and overlap resolution.
//! Only one contour-local bounding box is live at a time, not one image per ID.
use super::{antialias, cleanup, coverage};
#[cfg(test)]
use crate::CancellationToken;
use crate::{Cancellation, Error, GameAssetAa, Result};
use image::{GrayImage, Luma};
use std::cmp::Ordering;

pub struct Strokes {
    pub core: GrayImage,
    pub coverage: GrayImage,
    pub owners: Vec<Option<usize>>,
    /// Structural source evidence at the winning target sample. It remains
    /// pixel-local because target coalescing may add one nearby parallel
    /// contributor without strengthening a whole contour.
    pub importance: Vec<f64>,
    pub tangents: Vec<Option<[f64; 2]>>,
    pub overlap_max: Vec<f64>,
    pub overlap_residual: Vec<f64>,
    pub footprint: Vec<f64>,
}

fn curve_tangent(curves: &[coverage::Quadratic], point: [f64; 2]) -> Option<[f64; 2]> {
    let mut best = None;
    for curve in curves {
        for step in 0..=12 {
            let t = step as f64 / 12.;
            let mt = 1. - t;
            let position = [
                mt * mt * curve[0][0] + 2. * mt * t * curve[1][0] + t * t * curve[2][0],
                mt * mt * curve[0][1] + 2. * mt * t * curve[1][1] + t * t * curve[2][1],
            ];
            let distance = (position[0] - point[0]).hypot(position[1] - point[1]);
            let derivative = [
                2. * (mt * (curve[1][0] - curve[0][0]) + t * (curve[2][0] - curve[1][0])),
                2. * (mt * (curve[1][1] - curve[0][1]) + t * (curve[2][1] - curve[1][1])),
            ];
            if derivative[0].hypot(derivative[1]) > 1e-9
                && best.is_none_or(|(old, _)| distance < old)
            {
                best = Some((distance, derivative));
            }
        }
    }
    best.map(|(_, tangent)| {
        let length = tangent[0].hypot(tangent[1]);
        [tangent[0] / length, tangent[1] / length]
    })
}

#[allow(clippy::too_many_arguments)]
pub fn render(
    curves: &[coverage::Quadratic],
    owners: &[usize],
    widths: &[f64],
    importances: &[f64],
    w: u32,
    h: u32,
    intensity: GameAssetAa,
    cancel: &dyn Cancellation,
) -> Result<Strokes> {
    cancel.check()?;
    if w == 0
        || h == 0
        || curves.len() != owners.len()
        || !coverage::valid_curves(curves)
        || widths.len() != importances.len()
        || owners.iter().any(|&id| id >= importances.len())
    {
        return Err(Error::Scaling("Invalid target contours".into()));
    }
    let mut result = Strokes {
        core: GrayImage::new(w, h),
        coverage: GrayImage::new(w, h),
        owners: vec![None; w as usize * h as usize],
        importance: vec![0.; w as usize * h as usize],
        tangents: vec![None; w as usize * h as usize],
        overlap_max: vec![0.; w as usize * h as usize],
        overlap_residual: vec![1.; w as usize * h as usize],
        footprint: vec![0.; w as usize * h as usize],
    };
    let mut groups = vec![Vec::new(); importances.len()];
    for (q, &id) in curves.iter().zip(owners) {
        groups[id].push(*q);
    }
    let mut lengths = vec![0; importances.len()];
    let mut priorities = vec![0.; importances.len()];
    for (id, curves) in groups.iter().enumerate() {
        cancel.check()?;
        if curves.is_empty() {
            continue;
        }
        let mut lower = [f64::INFINITY; 2];
        let mut upper = [f64::NEG_INFINITY; 2];
        for p in curves.iter().flatten() {
            for axis in 0..2 {
                lower[axis] = lower[axis].min(p[axis]);
                upper[axis] = upper[axis].max(p[axis]);
            }
        }
        // A quadratic lies inside its control hull. Two extra pixels contain
        // its rounded core, the sampled stroke and the one-pixel AA neighborhood.
        let origin: [usize; 2] =
            std::array::from_fn(|a| (lower[a].floor() - 2.).clamp(0., [w, h][a] as f64) as usize);
        let end: [usize; 2] =
            std::array::from_fn(|a| (upper[a].ceil() + 3.).clamp(0., [w, h][a] as f64) as usize);
        if end[0] <= origin[0] || end[1] <= origin[1] {
            continue;
        }
        let local: Vec<_> = curves
            .iter()
            .map(|q| q.map(|p| [p[0] - origin[0] as f64, p[1] - origin[1] as f64]))
            .collect();
        let sampled = coverage::rasterize(&local, end[0] - origin[0], end[1] - origin[1], cancel)?;
        // Cleanup may not use another contour as an alternate route. At a
        // genuine intersection, the explicit arbitration below chooses an owner.
        let core = cleanup::thin(&sampled.core, &sampled.distances, cancel)?;
        let aa = antialias::coverage_map(&core, &sampled.pen_coverage, intensity, cancel)?;
        lengths[id] = core.data.iter().filter(|&&on| on).count();
        priorities[id] = importances[id];
        for y in 0..core.h {
            cancel.check()?;
            for x in 0..core.w {
                let j = y * core.w + x;
                let alpha = aa.as_raw()[j];
                if alpha == 0 {
                    continue;
                }
                let tx = (x + origin[0]) as u32;
                let ty = (y + origin[1]) as u32;
                let i = ty as usize * w as usize + tx as usize;
                let old_core = result.core.as_raw()[i] != 0;
                let tangent =
                    curve_tangent(curves, [(x + origin[0]) as f64, (y + origin[1]) as f64]);
                let wins = match result.owners[i] {
                    None => true,
                    Some(old) => {
                        let rank = if core.data[j] && old_core {
                            priorities[id]
                                .total_cmp(&priorities[old])
                                .then(lengths[id].cmp(&lengths[old]))
                        } else {
                            (alpha as f64 * priorities[id])
                                .total_cmp(&(result.coverage.as_raw()[i] as f64 * priorities[old]))
                        };
                        core.data[j].cmp(&old_core).then(rank).then(old.cmp(&id))
                            == Ordering::Greater
                    }
                };
                if wins {
                    let (merged, overlap_max, overlap_residual) = result.owners[i]
                        .filter(|&old| old != id && core.data[j] && old_core)
                        .zip(result.tangents[i])
                        .zip(tangent)
                        .filter(|((_, old), new)| (old[0] * new[0] + old[1] * new[1]).abs() >= 0.9)
                        .map_or((importances[id], importances[id], 1.), |((_, _), _)| {
                            let old_max = result.overlap_max[i];
                            let overlap_max = old_max.max(importances[id]);
                            let overlap_residual = result.overlap_residual[i]
                                * (1. - 0.65 * old_max.min(importances[id]));
                            (
                                1. - (1. - overlap_max) * overlap_residual,
                                overlap_max,
                                overlap_residual,
                            )
                        });
                    result
                        .core
                        .put_pixel(tx, ty, Luma([u8::from(core.data[j]) * 255]));
                    result.coverage.put_pixel(tx, ty, Luma([alpha]));
                    result.owners[i] = Some(id);
                    result.importance[i] = merged;
                    result.tangents[i] = tangent;
                    result.overlap_max[i] = overlap_max;
                    result.overlap_residual[i] = overlap_residual;
                    result.footprint[i] = widths[id] * 0.5 + 1.;
                } else if core.data[j]
                    && old_core
                    && result.owners[i].is_some_and(|old| old != id)
                    && result.tangents[i]
                        .zip(tangent)
                        .is_some_and(|(old, new)| (old[0] * new[0] + old[1] * new[1]).abs() >= 0.9)
                {
                    let old_max = result.overlap_max[i];
                    result.overlap_max[i] = old_max.max(importances[id]);
                    result.overlap_residual[i] *= 1. - 0.65 * old_max.min(importances[id]);
                    result.importance[i] =
                        1. - (1. - result.overlap_max[i]) * result.overlap_residual[i];
                }
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn line(a: [f64; 2], b: [f64; 2]) -> coverage::Quadratic {
        [a, [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5], b]
    }

    #[test]
    fn a_short_neighbor_cannot_delete_or_recolor_a_strong_core() {
        let cancel = CancellationToken::default();
        let main = line([2., 35.], [42., 5.]);
        let detail = line([18., 23.], [24., 20.]);
        let alone = render(
            &[main],
            &[0],
            &[1.],
            &[1.],
            48,
            40,
            GameAssetAa::default(),
            &cancel,
        )
        .unwrap();
        let together = render(
            &[main, detail],
            &[0, 1],
            &[1., 1.],
            &[1., 0.2],
            48,
            40,
            GameAssetAa::default(),
            &cancel,
        )
        .unwrap();
        let reordered = render(
            &[detail, main],
            &[1, 0],
            &[1., 1.],
            &[1., 0.2],
            48,
            40,
            GameAssetAa::default(),
            &cancel,
        )
        .unwrap();
        assert_eq!(together.core, reordered.core);
        assert_eq!(together.coverage, reordered.coverage);
        assert_eq!(together.owners, reordered.owners);
        for (i, &core) in alone.core.as_raw().iter().enumerate() {
            if core == 0 {
                continue;
            }
            assert_eq!(together.core.as_raw()[i], 255);
            assert_eq!(together.owners[i], Some(0));
            assert_eq!(together.coverage.as_raw()[i], alone.coverage.as_raw()[i]);
            assert_ne!(together.coverage.as_raw()[i], 0);
        }
    }

    #[test]
    fn coincident_aligned_owners_merge_strength_independent_of_render_order() {
        let cancel = CancellationToken::default();
        let q = line([2., 5.], [18., 5.]);
        let mut expected = None;
        for importances in [
            [0.2, 0.35, 0.8],
            [0.2, 0.8, 0.35],
            [0.35, 0.2, 0.8],
            [0.35, 0.8, 0.2],
            [0.8, 0.2, 0.35],
            [0.8, 0.35, 0.2],
        ] {
            let rendered = render(
                &[q, q, q],
                &[0, 1, 2],
                &[1., 1., 1.],
                &importances,
                24,
                12,
                GameAssetAa::new(0),
                &cancel,
            )
            .unwrap();
            let values: Vec<_> = rendered
                .core
                .as_raw()
                .iter()
                .zip(&rendered.importance)
                .filter_map(|(&core, &importance)| (core != 0).then_some(importance))
                .collect();
            assert!(values.iter().all(|&importance| importance >= 0.8));
            if let Some(ref expected) = expected {
                assert_eq!(values, *expected);
            } else {
                expected = Some(values);
            }
        }
    }

    #[test]
    fn invalid_contours_empty_images_and_cancellation_are_explicit() {
        let cancel = CancellationToken::default();
        let q = line([0., 0.], [4., 3.]);
        assert!(
            render(
                &[q],
                &[],
                &[1.],
                &[1.],
                8,
                8,
                GameAssetAa::default(),
                &cancel
            )
            .is_err()
        );
        assert!(
            render(
                &[q],
                &[1],
                &[1.],
                &[1.],
                8,
                8,
                GameAssetAa::default(),
                &cancel
            )
            .is_err()
        );
        assert!(
            render(
                &[q],
                &[0],
                &[1.],
                &[1.],
                0,
                8,
                GameAssetAa::default(),
                &cancel
            )
            .is_err()
        );
        let empty = render(&[], &[], &[], &[], 8, 8, GameAssetAa::default(), &cancel).unwrap();
        assert!(empty.coverage.as_raw().iter().all(|&a| a == 0));
        assert!(empty.owners.iter().all(Option::is_none));
        cancel.cancel();
        assert!(matches!(
            render(&[], &[], &[], &[], 8, 8, GameAssetAa::default(), &cancel),
            Err(Error::Cancelled)
        ));
    }
}
