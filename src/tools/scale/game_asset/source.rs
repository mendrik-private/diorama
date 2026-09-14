use super::{
    detect::{Model, controls, curve_point},
    field::Spatial,
    raster::{Mask, Zingl},
};
use crate::{document::CancellationToken, error::Result};
pub fn rasterize(
    models: &[Model],
    w: usize,
    h: usize,
    cancel: &CancellationToken,
) -> Result<(Mask, Vec<f64>)> {
    let mut raster = Zingl::new(w, h);
    let mut points = Vec::new();
    for m in models {
        cancel.check()?;
        raster
            .quadratic(controls(m, 1.))
            .map_err(crate::error::AppError::Scaling)?;
        let count = (((m[8] - m[7]) / 0.1).ceil() as usize + 1).max(2);
        for i in 0..count {
            let u = m[7] + (m[8] - m[7]) * i as f64 / (count - 1) as f64;
            points.push(curve_point(m, u));
        }
    }
    let tree = Spatial::new(points, 2.);
    let mut distance = vec![0.; w * h];
    for (i, &on) in raster.image.data.iter().enumerate() {
        if i % 4096 == 0 {
            cancel.check()?;
        }
        if on && let Some((d, _)) = tree.nearest([(i % w) as f64, (i / w) as f64]) {
            distance[i] = d;
        }
    }
    Ok((raster.image, distance))
}
