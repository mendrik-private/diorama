use crate::canvas::CropOverlay;
use crate::settings::ZoomMode;

pub(super) fn zoom_rect_target(viewport: (f64, f64), selection: CropOverlay) -> Option<f64> {
    if !viewport.0.is_finite()
        || !viewport.1.is_finite()
        || viewport.0 <= 1.0
        || viewport.1 <= 1.0
        || selection.width == 0
        || selection.height == 0
    {
        return None;
    }
    Some((viewport.0 / f64::from(selection.width)).min(viewport.1 / f64::from(selection.height)))
}

pub(super) fn panel_fit_zoom(size: (i32, i32), dimensions: (i32, i32)) -> f64 {
    (f64::from(size.0.max(1)) / f64::from(dimensions.0.max(1)))
        .min(f64::from(size.1.max(1)) / f64::from(dimensions.1.max(1)))
}

pub(super) fn fit_on_load(force_fit: bool, zoom_mode: ZoomMode) -> Option<bool> {
    if force_fit {
        return Some(false);
    }
    match zoom_mode {
        ZoomMode::Fit => Some(false),
        ZoomMode::Fill => Some(true),
        ZoomMode::Manual => None,
    }
}

pub(super) fn sanitized_render_scale(scale: f64) -> f64 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

pub(super) fn device_zoom(logical_zoom: f64, render_scale: f64) -> f64 {
    logical_zoom * sanitized_render_scale(render_scale)
}

pub(super) fn logical_zoom(device_zoom: f64, render_scale: f64) -> f64 {
    device_zoom / sanitized_render_scale(render_scale)
}

pub(super) fn aligned_hard_zoom(zoom: f64, render_scale: f64) -> f64 {
    let render_scale = sanitized_render_scale(render_scale);
    let render_zoom = (zoom * render_scale).round().max(1.0);
    render_zoom / render_scale
}

pub(super) fn stepped_hard_zoom(zoom: f64, render_scale: f64, zoom_in: bool) -> f64 {
    let render_scale = sanitized_render_scale(render_scale);
    let render_zoom = zoom * render_scale;
    let next = if zoom_in {
        render_zoom.floor() + 1.0
    } else {
        (render_zoom.ceil() - 1.0).max(1.0)
    };
    next / render_scale
}

pub(super) fn usable_panel_size(size: (i32, i32)) -> bool {
    size.0 > 1 && size.1 > 1
}

pub(super) fn comparison_zoom(primary_zoom: f64, fit_zooms: (f64, f64)) -> f64 {
    primary_zoom * fit_zooms.1 / fit_zooms.0.max(0.01)
}

pub(super) fn scale_preview_zoom(source_width: u32, target_width: u32, source_zoom: f64) -> f64 {
    source_zoom * f64::from(source_width.max(1)) / f64::from(target_width.max(1))
}

pub(super) fn anchored_adjustment_value(value: f64, content_position: f64, factor: f64) -> f64 {
    let viewport_position = content_position - value;
    content_position * factor - viewport_position
}

pub(super) fn centered_adjustment_value(lower: f64, upper: f64, page_size: f64) -> f64 {
    lower + ((upper - lower - page_size) / 2.0).max(0.0)
}
