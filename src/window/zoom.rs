use crate::canvas::CropOverlay;
use crate::settings::ZoomMode;

#[derive(Clone, Copy)]
pub(super) enum ZoomAlignment {
    Nearest,
    Contain,
    Cover,
}

pub(super) fn zoom_rect_target(viewport: (i32, i32), selection: CropOverlay) -> Option<f64> {
    if !usable_panel_size(viewport) || selection.width == 0 || selection.height == 0 {
        return None;
    }
    Some(
        (f64::from(viewport.0) / f64::from(selection.width))
            .min(f64::from(viewport.1) / f64::from(selection.height)),
    )
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

pub(super) fn normalized_render_scale(scale: f64) -> f64 {
    if scale.is_finite() && scale > 0.0 {
        scale.round().max(1.0)
    } else {
        1.0
    }
}

pub(super) fn aligned_hard_zoom(zoom: f64, render_scale: f64, alignment: ZoomAlignment) -> f64 {
    if zoom < 1.0 {
        return zoom;
    }
    let render_scale = normalized_render_scale(render_scale);
    let render_zoom = zoom * render_scale;
    let render_zoom = match alignment {
        ZoomAlignment::Nearest => render_zoom.round(),
        ZoomAlignment::Contain => render_zoom.floor(),
        ZoomAlignment::Cover => render_zoom.ceil(),
    }
    .max(render_scale);
    render_zoom / render_scale
}

pub(super) fn stepped_hard_zoom(zoom: f64, render_scale: f64, zoom_in: bool) -> f64 {
    let render_scale = normalized_render_scale(render_scale);
    let render_zoom = zoom * render_scale;
    let next = if zoom >= 1.0 {
        if zoom_in {
            render_zoom.floor() + 1.0
        } else if zoom > 1.0 + 1e-6 {
            (render_zoom.ceil() - 1.0).max(render_scale)
        } else {
            render_zoom * 0.8
        }
    } else {
        render_zoom * if zoom_in { 1.25 } else { 0.8 }
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
