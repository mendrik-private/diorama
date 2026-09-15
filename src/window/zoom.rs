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
    let render_zoom = zoom * render_scale;
    if render_zoom <= 1.0 {
        return render_zoom / render_scale;
    }
    let render_zoom = render_zoom.round();
    render_zoom / render_scale
}

pub(super) fn stepped_zoom(
    zoom: f64,
    render_scale: f64,
    zoom_in: bool,
    whole_pixels_above_actual: bool,
) -> f64 {
    const SUBPIXEL_STEPS: [f64; 4] = [0.25, 0.5, 0.75, 1.0];
    const STEP_EPSILON: f64 = 1e-9;

    let render_scale = sanitized_render_scale(render_scale);
    let render_zoom = zoom * render_scale;
    let preset = if (SUBPIXEL_STEPS[0] - STEP_EPSILON..=1.0 + STEP_EPSILON).contains(&render_zoom) {
        if zoom_in {
            SUBPIXEL_STEPS
                .into_iter()
                .find(|step| *step > render_zoom + STEP_EPSILON)
        } else {
            SUBPIXEL_STEPS
                .into_iter()
                .rev()
                .find(|step| *step < render_zoom - STEP_EPSILON)
                .or(Some(SUBPIXEL_STEPS[0]))
        }
    } else {
        None
    };
    let next = if let Some(preset) = preset {
        preset
    } else if render_zoom < SUBPIXEL_STEPS[0] {
        if zoom_in {
            SUBPIXEL_STEPS[0]
        } else {
            render_zoom * 0.8
        }
    } else if whole_pixels_above_actual && zoom_in {
        render_zoom.floor() + 1.0
    } else if whole_pixels_above_actual {
        (render_zoom.ceil() - 1.0).max(1.0)
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
