use super::raster::Mask;
fn coverage(distance_in_minor_axis: f64, core: bool) -> f64 {
    let value = ((0.65 - distance_in_minor_axis) / 0.30).clamp(0., 1.);
    // Preserve the selected digital staircase, but allow a restrained neighboring
    // pixel at a fractional crossing. Total fringe width is 0.30 minor-axis px.
    if core {
        value.max(0.65)
    } else {
        value.min(0.35)
    }
}

pub fn coverage_map(mask: &Mask, distances: &[f64]) -> image::GrayImage {
    assert_eq!(mask.data.len(), distances.len());
    image::GrayImage::from_fn(mask.w as u32, mask.h as u32, |x, y| {
        let i = y as usize * mask.w + x as usize;
        let core = mask.data[i];
        let adjacent =
            core || (-1..=1).any(|dy| (-1..=1).any(|dx| mask.at(x as isize + dx, y as isize + dy)));
        let aa = if adjacent {
            coverage(distances[i], core)
        } else {
            0.
        };
        image::Luma([(255. * aa).round() as u8])
    })
}
