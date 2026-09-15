//! Keep contour identity through digital cleanup, AA, and overlap resolution.
//! Only one contour-local bounding box is live at a time, not one image per ID.
use super::{antialias, cleanup, coverage, opacity};
use crate::{
    document::{CancellationToken, GameAssetAa},
    error::{AppError, Result},
};
use image::{GrayImage, Luma};
use std::cmp::Ordering;

pub struct Strokes {
    pub core: GrayImage,
    pub coverage: GrayImage,
    pub owners: Vec<Option<usize>>,
}

pub fn render(
    curves: &[coverage::Quadratic],
    owners: &[usize],
    widths: &[f64],
    w: u32,
    h: u32,
    intensity: GameAssetAa,
    cancel: &CancellationToken,
) -> Result<Strokes> {
    cancel.check()?;
    if w == 0
        || h == 0
        || curves.len() != owners.len()
        || !coverage::valid_curves(curves)
        || owners.iter().any(|&id| id >= widths.len())
    {
        return Err(AppError::Scaling("Invalid target contours".into()));
    }
    let mut result = Strokes {
        core: GrayImage::new(w, h),
        coverage: GrayImage::new(w, h),
        owners: vec![None; w as usize * h as usize],
    };
    let mut groups = vec![Vec::new(); widths.len()];
    for (q, &id) in curves.iter().zip(owners) {
        groups[id].push(*q);
    }
    let mut lengths = vec![0; widths.len()];
    let mut priorities = vec![0.; widths.len()];
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
        let aa = antialias::coverage_map(&core, &sampled.area, intensity, cancel)?;
        lengths[id] = core.data.iter().filter(|&&on| on).count();
        priorities[id] = opacity::strength(lengths[id], widths[id]);
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
                    result
                        .core
                        .put_pixel(tx, ty, Luma([u8::from(core.data[j]) * 255]));
                    result.coverage.put_pixel(tx, ty, Luma([alpha]));
                    result.owners[i] = Some(id);
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
            &[8.],
            48,
            40,
            GameAssetAa::default(),
            &cancel,
        )
        .unwrap();
        let together = render(
            &[main, detail],
            &[0, 1],
            &[8., 8.],
            48,
            40,
            GameAssetAa::default(),
            &cancel,
        )
        .unwrap();
        let reordered = render(
            &[detail, main],
            &[1, 0],
            &[8., 8.],
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
            assert!(together.coverage.as_raw()[i] >= 243);
        }
    }

    #[test]
    fn invalid_contours_empty_images_and_cancellation_are_explicit() {
        let cancel = CancellationToken::default();
        let q = line([0., 0.], [4., 3.]);
        assert!(render(&[q], &[], &[8.], 8, 8, GameAssetAa::default(), &cancel).is_err());
        assert!(render(&[q], &[1], &[8.], 8, 8, GameAssetAa::default(), &cancel).is_err());
        assert!(render(&[q], &[0], &[8.], 0, 8, GameAssetAa::default(), &cancel).is_err());
        let empty = render(&[], &[], &[], 8, 8, GameAssetAa::default(), &cancel).unwrap();
        assert!(empty.coverage.as_raw().iter().all(|&a| a == 0));
        assert!(empty.owners.iter().all(Option::is_none));
        cancel.cancel();
        assert!(matches!(
            render(&[], &[], &[], 8, 8, GameAssetAa::default(), &cancel),
            Err(AppError::Cancelled)
        ));
    }
}
