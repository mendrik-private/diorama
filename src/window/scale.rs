use crate::document::Resampling;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScaleUnit {
    Pixels,
    Percent,
}

pub(super) fn scaled_dimensions(width: u32, height: u32, target_width: u32) -> (u32, u32) {
    let width = width.max(1);
    let height = height.max(1);
    let target_width = target_width.max(1);
    let target_height = ((u64::from(height) * u64::from(target_width) + u64::from(width) / 2)
        / u64::from(width))
    .max(1)
    .min(u64::from(u32::MAX)) as u32;
    (target_width, target_height)
}

pub(super) fn scaled_width_for_height(width: u32, height: u32, target_height: u32) -> u32 {
    let width = width.max(1);
    let height = height.max(1);
    let target_height = target_height.max(1);
    ((u64::from(width) * u64::from(target_height) + u64::from(height) / 2) / u64::from(height))
        .max(1)
        .min(u64::from(u32::MAX)) as u32
}

pub(super) fn dimensions_from_percent(width: u32, height: u32, percent: f64) -> (u32, u32) {
    let factor = percent.max(0.01) / 100.0;
    (
        (f64::from(width.max(1)) * factor)
            .round()
            .clamp(1.0, f64::from(u32::MAX)) as u32,
        (f64::from(height.max(1)) * factor)
            .round()
            .clamp(1.0, f64::from(u32::MAX)) as u32,
    )
}

pub(super) fn scale_unit(index: u32) -> ScaleUnit {
    if index == 1 {
        ScaleUnit::Percent
    } else {
        ScaleUnit::Pixels
    }
}

pub(super) fn resampling_label(resampling: Resampling) -> &'static str {
    match resampling {
        Resampling::Nearest => "Nearest",
        Resampling::Linear => "Linear",
        Resampling::Bicubic => "Bicubic",
        Resampling::SeamCarving => "Seam carving",
    }
}
