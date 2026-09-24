use super::{
    cleanup,
    detect::{Model, controls, curve_point},
    field::Spatial,
    raster::{Mask, Zingl},
};
use crate::{Cancellation, Result};

/// The widest enclosed background sliver, in 8-connected steps from ink, that
/// is treated as a gap between overlapping local fits of one stroke.
const MAX_SLIVER_DEPTH: usize = 1;
/// The longest terminal skeleton branch, in source pixels, that is treated as
/// detector fuzz on a stroke rather than as a drawn line end.
const MAX_SPUR_PIXELS: usize = 4;

/// Rasterize the local fits, then reduce them to a one-pixel source skeleton
/// whose loops and branches are drawn features rather than fitting artifacts.
pub fn skeleton(models: &[Model], w: usize, h: usize, cancel: &dyn Cancellation) -> Result<Mask> {
    let (raw, distance) = rasterize(models, w, h, cancel)?;
    let thinned = cleanup::thin(&raw, &distance, cancel)?;
    let pruned = cleanup::prune_spurs(&thinned, MAX_SPUR_PIXELS, cancel)?;
    // Pruning can leave a junction's corner pixel; one more topology-safe pass
    // removes it without touching any remaining branch.
    cleanup::thin(&pruned, &distance, cancel)
}

pub fn rasterize(
    models: &[Model],
    w: usize,
    h: usize,
    cancel: &dyn Cancellation,
) -> Result<(Mask, Vec<f64>)> {
    let mut raster = Zingl::new(w, h);
    let mut points = Vec::new();
    for m in models {
        cancel.check()?;
        raster
            .quadratic(controls(m, 1.))
            .map_err(crate::Error::Scaling)?;
        let count = (((m[8] - m[7]) / 0.1).ceil() as usize + 1).max(2);
        for i in 0..count {
            let u = m[7] + (m[8] - m[7]) * i as f64 / (count - 1) as f64;
            points.push(curve_point(m, u));
        }
    }
    // Adjacent local fits of one wide stroke overlap with a small offset and
    // enclose slivers. Thinning preserves every hole, so each would become a
    // bubble; filling them first leaves one centerline through the stroke.
    let image = cleanup::fill_slivers(&raster.image, MAX_SLIVER_DEPTH, cancel)?;
    let tree = Spatial::new(points, 2.);
    let mut distance = vec![0.; w * h];
    for (i, &on) in image.data.iter().enumerate() {
        if i % 4096 == 0 {
            cancel.check()?;
        }
        if on && let Some((d, _)) = tree.nearest([(i % w) as f64, (i / w) as f64]) {
            distance[i] = d;
        }
    }
    Ok((image, distance))
}
