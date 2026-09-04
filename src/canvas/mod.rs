use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::document::{Annotation, BrushPoint, Point, Shape, StrokePath};
use crate::i18n::gettext;
use crate::tools::annotation::hit::{HandleKind, handles};

const MAX_FIT_ZOOM: f64 = 16_384.0;
const COORDINATE_TOOLTIP_DELAY: Duration = Duration::from_secs(2);
const MEASUREMENT_CURSOR_BLEND_MODE: gtk::gsk::BlendMode = gtk::gsk::BlendMode::Difference;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ZoomFilter {
    Soft,
    #[default]
    Hard,
}

fn sanitized_render_scale(scale: f64) -> f64 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

fn bounded_fit_zoom(zoom: f64) -> f64 {
    zoom.clamp(f64::EPSILON, MAX_FIT_ZOOM)
}

fn snap_to_device(value: f32, render_scale: f64) -> f32 {
    let render_scale = sanitized_render_scale(render_scale) as f32;
    (value * render_scale).round() / render_scale
}

fn aligned_render_pixel_scale(zoom: f64, render_scale: f64) -> Option<f64> {
    let effective_scale = zoom * sanitized_render_scale(render_scale);
    let aligned = effective_scale.round();
    (aligned >= 1.0 && (effective_scale - aligned).abs() <= 1e-6 * effective_scale.abs().max(1.0))
        .then_some(aligned)
}

fn coordinate_tooltip_text((x, y): (u32, u32)) -> String {
    gettext("X {x} · Y {y}")
        .replace("{x}", &x.to_string())
        .replace("{y}", &y.to_string())
}

fn measured_image_dimension(
    dimension: i32,
    zoom: f64,
    filter: ZoomFilter,
    render_scale: f64,
) -> i32 {
    let scaled = f64::from(dimension) * zoom;
    if filter == ZoomFilter::Hard && aligned_render_pixel_scale(zoom, render_scale).is_some() {
        scaled.ceil() as i32
    } else {
        scaled.round() as i32
    }
}

#[derive(Debug, Clone, Copy)]
struct CanvasLayout {
    logical_bounds: gtk::graphene::Rect,
    device_bounds: Option<gtk::graphene::Rect>,
}

fn canvas_layout(
    bounds: gtk::graphene::Rect,
    image_dimensions: (i32, i32),
    zoom: f64,
    filter: ZoomFilter,
    render_scale: f64,
    preview_scale: f32,
    surface_origin_device: (f64, f64),
) -> CanvasLayout {
    let image_width = image_dimensions.0.max(1);
    let image_height = image_dimensions.1.max(1);
    let render_scale = sanitized_render_scale(render_scale);
    if filter == ZoomFilter::Hard {
        let pixel_scale = zoom * render_scale * f64::from(preview_scale.clamp(0.01, 64.0));
        let width_device = f64::from(image_width) * pixel_scale;
        let height_device = f64::from(image_height) * pixel_scale;
        let desired_x =
            f64::from(bounds.x()) + (f64::from(bounds.width()) - width_device / render_scale) / 2.0;
        let desired_y = f64::from(bounds.y())
            + (f64::from(bounds.height()) - height_device / render_scale) / 2.0;
        let x_device =
            (surface_origin_device.0 + desired_x * render_scale).round() - surface_origin_device.0;
        let y_device =
            (surface_origin_device.1 + desired_y * render_scale).round() - surface_origin_device.1;
        let device_bounds = gtk::graphene::Rect::new(
            x_device as f32,
            y_device as f32,
            width_device as f32,
            height_device as f32,
        );
        return CanvasLayout {
            logical_bounds: gtk::graphene::Rect::new(
                (x_device / render_scale) as f32,
                (y_device / render_scale) as f32,
                (width_device / render_scale) as f32,
                (height_device / render_scale) as f32,
            ),
            device_bounds: Some(device_bounds),
        };
    }

    let mut image_bounds = {
        let image_ratio = image_width as f32 / image_height as f32;
        let bounds_ratio = bounds.width() / bounds.height().max(1.0);
        if image_ratio > bounds_ratio {
            let height = bounds.width() / image_ratio;
            gtk::graphene::Rect::new(
                bounds.x(),
                bounds.y() + (bounds.height() - height) / 2.0,
                bounds.width(),
                height,
            )
        } else {
            let width = bounds.height() * image_ratio;
            gtk::graphene::Rect::new(
                bounds.x() + (bounds.width() - width) / 2.0,
                bounds.y(),
                width,
                bounds.height(),
            )
        }
    };

    let preview_scale = preview_scale.clamp(0.01, 64.0);
    if preview_scale != 1.0 {
        let width = image_bounds.width() * preview_scale;
        let height = image_bounds.height() * preview_scale;
        image_bounds = gtk::graphene::Rect::new(
            image_bounds.x() + (image_bounds.width() - width) / 2.0,
            image_bounds.y() + (image_bounds.height() - height) / 2.0,
            width,
            height,
        );
    }
    CanvasLayout {
        logical_bounds: image_bounds,
        device_bounds: None,
    }
}

#[cfg(test)]
fn canvas_image_bounds(
    bounds: gtk::graphene::Rect,
    image_dimensions: (i32, i32),
    zoom: f64,
    filter: ZoomFilter,
    render_scale: f64,
    preview_scale: f32,
) -> gtk::graphene::Rect {
    canvas_layout(
        bounds,
        image_dimensions,
        zoom,
        filter,
        render_scale,
        preview_scale,
        (0.0, 0.0),
    )
    .logical_bounds
}

fn overlay_rect(image_bounds: gtk::graphene::Rect, overlay: &CropOverlay) -> gtk::graphene::Rect {
    let width = overlay.image_width.max(1) as f32;
    let height = overlay.image_height.max(1) as f32;
    gtk::graphene::Rect::new(
        image_bounds.x() + image_bounds.width() * overlay.x as f32 / width,
        image_bounds.y() + image_bounds.height() * overlay.y as f32 / height,
        image_bounds.width() * overlay.width as f32 / width,
        image_bounds.height() * overlay.height as f32 / height,
    )
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Background {
    #[default]
    Checkerboard,
    Auto,
    White,
    Gray,
    Black,
}

fn normalized_pixel_boundary(boundary: (u32, u32), image_dimensions: (u32, u32)) -> (f32, f32) {
    (
        if image_dimensions.0 == 0 {
            0.0
        } else {
            boundary.0.min(image_dimensions.0) as f32 / image_dimensions.0 as f32
        },
        if image_dimensions.1 == 0 {
            0.0
        } else {
            boundary.1.min(image_dimensions.1) as f32 / image_dimensions.1 as f32
        },
    )
}

fn pixel_boundary_from_normalized(
    normalized: (f32, f32),
    image_dimensions: (u32, u32),
) -> (u32, u32) {
    let width = image_dimensions.0 as f32;
    let height = image_dimensions.1 as f32;
    (
        (normalized.0 * width).round().clamp(0.0, width) as u32,
        (normalized.1 * height).round().clamp(0.0, height) as u32,
    )
}

fn opposite_grayscale_luminance(image: &image::RgbaImage) -> f32 {
    let (weighted_luminance, alpha_sum) = image.pixels().fold((0.0_f64, 0.0_f64), |sum, pixel| {
        let alpha = f64::from(pixel[3]) / 255.0;
        let luminance = (0.2126 * f64::from(pixel[0])
            + 0.7152 * f64::from(pixel[1])
            + 0.0722 * f64::from(pixel[2]))
            / 255.0;
        (sum.0 + luminance * alpha, sum.1 + alpha)
    });
    if alpha_sum <= f64::EPSILON {
        0.5
    } else {
        let luminance = (1.0 - weighted_luminance / alpha_sum).clamp(0.0, 1.0) as f32;
        if luminance <= f32::EPSILON {
            0.0
        } else if luminance >= 1.0 - f32::EPSILON {
            1.0
        } else {
            luminance
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CropOverlay {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub image_width: u32,
    pub image_height: u32,
}

#[derive(Debug, Clone)]
pub(super) struct Lens {
    texture: gdk::Texture,
    normalized_x: f32,
    normalized_y: f32,
    diameter: f32,
    magnification: f32,
    show_cross: bool,
}

#[derive(Debug, Clone)]
struct PencilOverlay {
    points: Vec<BrushPoint>,
    path: StrokePath,
    color: [u8; 4],
    width: f32,
}

#[derive(Debug, Clone)]
pub struct AnnotationOverlay {
    pub texture: gdk::Texture,
    pub bounds: crate::document::Rect,
}

#[derive(Debug)]
struct PreviewLayers<T> {
    committed: Vec<T>,
    active: Option<T>,
}

impl<T> Default for PreviewLayers<T> {
    fn default() -> Self {
        Self {
            committed: Vec::new(),
            active: None,
        }
    }
}

impl<T> PreviewLayers<T> {
    fn set_active(&mut self, preview: Option<T>) {
        self.active = preview;
    }

    fn commit_active(&mut self) {
        if let Some(preview) = self.active.take() {
            self.committed.push(preview);
        }
    }

    fn finish_document_render(&mut self) -> bool {
        let changed = !self.committed.is_empty();
        self.committed.clear();
        changed
    }

    fn clear(&mut self) -> bool {
        let changed = self.active.is_some() || !self.committed.is_empty();
        self.active = None;
        self.committed.clear();
        changed
    }

    fn visible(&self) -> impl Iterator<Item = &T> {
        self.committed.iter().chain(self.active.iter())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectionHandles {
    pub annotation: Annotation,
    pub hot: Option<HandleKind>,
}

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct ImageCanvas {
        pub texture: RefCell<Option<gdk::Texture>>,
        pub zoom: Cell<f64>,
        pub render_scale: Cell<f64>,
        pub filter: Cell<ZoomFilter>,
        pub background: Cell<Background>,
        pub auto_background_luminance: Cell<f32>,
        pub preview_scale: Cell<f32>,
        pub device_origin_phase: Cell<(u64, u64)>,
        pub(super) lens: RefCell<Option<Lens>>,
        pub marker: Cell<Option<(f32, f32)>>,
        pub crop_overlay: RefCell<Option<CropOverlay>>,
        pub crop_dash_phase: Cell<f32>,
        pub crop_animation_running: Cell<bool>,
        pub measurement_cursor: Cell<Option<(f32, f32)>>,
        pub(super) pencil_overlay: RefCell<Option<PencilOverlay>>,
        pub(super) annotation_previews: RefCell<PreviewLayers<AnnotationOverlay>>,
        pub(super) selection: RefCell<Option<SelectionHandles>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ImageCanvas {
        const NAME: &'static str = "DioramaImageCanvas";
        type Type = super::ImageCanvas;
        type ParentType = gtk::Widget;

        fn class_init(class: &mut Self::Class) {
            class.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    impl ObjectImpl for ImageCanvas {
        fn constructed(&self) {
            self.parent_constructed();
            self.zoom.set(1.0);
            self.render_scale.set(1.0);
            self.preview_scale.set(1.0);
            self.auto_background_luminance.set(0.5);
            let object = self.obj();
            object.set_focusable(true);
            object.set_overflow(gtk::Overflow::Hidden);
            object.install_coordinate_tooltip();
            object.update_property(&[gtk::accessible::Property::Label(&gettext("Image canvas"))]);
        }
    }

    impl WidgetImpl for ImageCanvas {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let size = self.texture.borrow().as_ref().map_or(1, |texture| {
                let dimension = if orientation == gtk::Orientation::Horizontal {
                    texture.width()
                } else {
                    texture.height()
                };
                measured_image_dimension(
                    dimension,
                    self.zoom.get(),
                    self.filter.get(),
                    self.render_scale.get(),
                )
            });
            (size.max(1), size.max(1), -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let object = self.obj();
            let bounds = gtk::graphene::Rect::new(
                0.0,
                0.0,
                object.width().max(1) as f32,
                object.height().max(1) as f32,
            );
            draw_background(
                snapshot,
                bounds,
                self.background.get(),
                self.auto_background_luminance.get(),
            );
            let image_bounds = self.texture.borrow().as_ref().map(|texture| {
                let layout = canvas_layout(
                    bounds,
                    (texture.width(), texture.height()),
                    self.zoom.get(),
                    self.filter.get(),
                    self.render_scale.get(),
                    self.preview_scale.get(),
                    object.surface_origin_device(),
                );
                let image_bounds = layout.logical_bounds;
                let filter = match self.filter.get() {
                    ZoomFilter::Soft => gtk::gsk::ScalingFilter::Linear,
                    ZoomFilter::Hard => gtk::gsk::ScalingFilter::Nearest,
                };
                let measurement_cursor = self.measurement_cursor.get();
                if measurement_cursor.is_some() {
                    append_measurement_cursor_xor(
                        snapshot,
                        texture,
                        filter,
                        layout,
                        image_bounds,
                        measurement_cursor,
                        self.render_scale.get(),
                    );
                } else {
                    append_texture(snapshot, texture, filter, layout, self.render_scale.get());
                }
                image_bounds
            });
            let pencil_overlay = self.pencil_overlay.borrow();
            let annotation_previews = self.annotation_previews.borrow();
            if let Some(image_bounds) = image_bounds {
                let render_scale = sanitized_render_scale(self.render_scale.get());
                let image_dimensions = self
                    .texture
                    .borrow()
                    .as_ref()
                    .map_or((1, 1), |texture| (texture.width(), texture.height()));
                for overlay in annotation_previews.visible() {
                    let scale_x = image_bounds.width() / image_dimensions.0.max(1) as f32;
                    let scale_y = image_bounds.height() / image_dimensions.1.max(1) as f32;
                    let preview_bounds = gtk::graphene::Rect::new(
                        image_bounds.x() + overlay.bounds.x * scale_x,
                        image_bounds.y() + overlay.bounds.y * scale_y,
                        overlay.bounds.width * scale_x,
                        overlay.bounds.height * scale_y,
                    );
                    let layout = CanvasLayout {
                        logical_bounds: preview_bounds,
                        device_bounds: (self.filter.get() == ZoomFilter::Hard).then(|| {
                            gtk::graphene::Rect::new(
                                preview_bounds.x() * render_scale as f32,
                                preview_bounds.y() * render_scale as f32,
                                preview_bounds.width() * render_scale as f32,
                                preview_bounds.height() * render_scale as f32,
                            )
                        }),
                    };
                    append_texture(
                        snapshot,
                        &overlay.texture,
                        match self.filter.get() {
                            ZoomFilter::Soft => gtk::gsk::ScalingFilter::Linear,
                            ZoomFilter::Hard => gtk::gsk::ScalingFilter::Nearest,
                        },
                        layout,
                        render_scale,
                    );
                }
            }
            if let Some(image_bounds) = image_bounds
                && let Some(selection) = self.selection.borrow().as_ref()
            {
                draw_annotation_selection(
                    snapshot,
                    image_bounds,
                    self.texture
                        .borrow()
                        .as_ref()
                        .map_or((1, 1), |texture| (texture.width(), texture.height())),
                    selection,
                    self.render_scale.get(),
                );
            }
            if let Some(image_bounds) = image_bounds
                && let Some(overlay) = pencil_overlay.as_ref()
            {
                draw_pencil_overlay(
                    snapshot,
                    image_bounds,
                    self.texture
                        .borrow()
                        .as_ref()
                        .map_or((1, 1), |texture| (texture.width(), texture.height())),
                    overlay,
                );
            }
            if let Some(lens) = self.lens.borrow().as_ref() {
                draw_lens(
                    snapshot,
                    bounds,
                    lens,
                    pencil_overlay.as_ref(),
                    self.background.get(),
                    self.auto_background_luminance.get(),
                );
            }
            if let Some((x, y)) = self.marker.get() {
                draw_marker(snapshot, bounds, x, y);
            }
            if let Some(image_bounds) = image_bounds
                && let Some(overlay) = self.crop_overlay.borrow().as_ref()
            {
                draw_crop_overlay(
                    snapshot,
                    image_bounds,
                    overlay,
                    self.render_scale.get(),
                    self.crop_dash_phase.get(),
                );
            }
        }
    }

    fn append_texture(
        snapshot: &gtk::Snapshot,
        texture: &gdk::Texture,
        filter: gtk::gsk::ScalingFilter,
        layout: CanvasLayout,
        render_scale: f64,
    ) {
        if let Some(device_bounds) = layout.device_bounds {
            let inverse = (1.0 / sanitized_render_scale(render_scale)) as f32;
            snapshot.save();
            snapshot.scale(inverse, inverse);
            snapshot.append_scaled_texture(texture, filter, &device_bounds);
            snapshot.restore();
        } else {
            snapshot.append_scaled_texture(texture, filter, &layout.logical_bounds);
        }
    }

    pub(super) fn append_measurement_cursor_xor(
        snapshot: &gtk::Snapshot,
        texture: &gdk::Texture,
        filter: gtk::gsk::ScalingFilter,
        layout: CanvasLayout,
        image_bounds: gtk::graphene::Rect,
        cursor: Option<(f32, f32)>,
        render_scale: f64,
    ) {
        snapshot.push_blend(MEASUREMENT_CURSOR_BLEND_MODE);
        append_texture(snapshot, texture, filter, layout, render_scale);
        // A blend snapshot has two child slots. The first pop closes the image
        // (bottom) child; the second closes the white crosshair (top) child.
        snapshot.pop();
        draw_measurement_cursor(snapshot, image_bounds, cursor, render_scale);
        snapshot.pop();
    }

    pub(super) fn draw_pencil_overlay(
        snapshot: &gtk::Snapshot,
        image_bounds: gtk::graphene::Rect,
        image_dimensions: (i32, i32),
        overlay: &PencilOverlay,
    ) {
        let Some(first) = overlay.points.first().copied() else {
            return;
        };
        let scale_x = image_bounds.width() / image_dimensions.0.max(1) as f32;
        let scale_y = image_bounds.height() / image_dimensions.1.max(1) as f32;
        let map = |point: BrushPoint| {
            gtk::graphene::Point::new(
                image_bounds.x() + point.x * scale_x,
                image_bounds.y() + point.y * scale_y,
            )
        };
        let color = gdk::RGBA::new(
            f32::from(overlay.color[0]) / 255.0,
            f32::from(overlay.color[1]) / 255.0,
            f32::from(overlay.color[2]) / 255.0,
            f32::from(overlay.color[3]) / 255.0,
        );
        let line_width = overlay.width * scale_x;
        let builder = gtk::gsk::PathBuilder::new();

        if overlay.points.len() == 1 {
            draw_pencil_dot(snapshot, image_bounds, map(first), line_width, &color);
            return;
        }

        match overlay.path {
            StrokePath::Smooth => {
                let first = map(first);
                builder.move_to(first.x(), first.y());
                if overlay.points.len() == 2 {
                    let last = map(overlay.points[1]);
                    builder.line_to(last.x(), last.y());
                } else {
                    let second = map(overlay.points[1]);
                    builder.quad_to(
                        first.x(),
                        first.y(),
                        (first.x() + second.x()) / 2.0,
                        (first.y() + second.y()) / 2.0,
                    );
                    for points in overlay.points.windows(3) {
                        let control = map(points[1]);
                        let next = map(points[2]);
                        builder.quad_to(
                            control.x(),
                            control.y(),
                            (control.x() + next.x()) / 2.0,
                            (control.y() + next.y()) / 2.0,
                        );
                    }
                    let last = map(*overlay.points.last().expect("at least two points"));
                    builder.quad_to(last.x(), last.y(), last.x(), last.y());
                }
            }
            StrokePath::Linear => {
                let first = map(first);
                builder.move_to(first.x(), first.y());
                for point in overlay.points.iter().skip(1).copied().map(map) {
                    builder.line_to(point.x(), point.y());
                }
            }
            StrokePath::Circle => {
                let center = map(first);
                let edge = map(overlay.points[1]);
                let radius = (edge.x() - center.x()).hypot(edge.y() - center.y());
                if radius <= f32::EPSILON {
                    draw_pencil_dot(snapshot, image_bounds, center, line_width, &color);
                    return;
                }
                builder.add_circle(&center, radius);
            }
        }

        let stroke = gtk::gsk::Stroke::builder(line_width)
            .line_cap(gtk::gsk::LineCap::Round)
            .line_join(gtk::gsk::LineJoin::Round)
            .build();
        snapshot.push_clip(&image_bounds);
        snapshot.append_stroke(&builder.to_path(), &stroke, &color);
        snapshot.pop();
    }

    pub(super) fn draw_annotation_selection(
        snapshot: &gtk::Snapshot,
        image_bounds: gtk::graphene::Rect,
        image_dimensions: (i32, i32),
        selection: &SelectionHandles,
        render_scale: f64,
    ) {
        let scale_x = image_bounds.width() / image_dimensions.0.max(1) as f32;
        let scale_y = image_bounds.height() / image_dimensions.1.max(1) as f32;
        let map = |point: Point| {
            gtk::graphene::Point::new(
                snap_to_device(image_bounds.x() + point.x * scale_x, render_scale),
                snap_to_device(image_bounds.y() + point.y * scale_y, render_scale),
            )
        };
        let mut outline = Vec::new();
        match &selection.annotation.shape {
            Shape::Pencil { geometry, .. } => {
                outline = crate::tools::annotation::pencil::outline_points(geometry);
            }
            Shape::Highlight { rect, .. } => {
                outline.extend([
                    Point {
                        x: rect.x,
                        y: rect.y,
                    },
                    Point {
                        x: rect.x + rect.width,
                        y: rect.y,
                    },
                    Point {
                        x: rect.x + rect.width,
                        y: rect.y + rect.height,
                    },
                    Point {
                        x: rect.x,
                        y: rect.y + rect.height,
                    },
                    Point {
                        x: rect.x,
                        y: rect.y,
                    },
                ]);
            }
            Shape::Arrow { start, end, .. } => outline.extend([*start, *end]),
            Shape::Measurement { .. } => {
                // The rendered one-pixel measurement remains the outline. Drawing a GSK
                // selection stroke over it would reintroduce antialiasing and extra width.
            }
            Shape::Text {
                anchor,
                angle,
                font_size,
                bend,
                text,
                ..
            } => {
                outline = crate::tools::annotation::text::baseline(
                    *anchor, *angle, *bend, text, *font_size,
                );
            }
        }
        if outline.len() >= 2 {
            let builder = gtk::gsk::PathBuilder::new();
            let first = map(outline[0]);
            builder.move_to(first.x(), first.y());
            for point in outline.into_iter().skip(1) {
                let point = map(point);
                builder.line_to(point.x(), point.y());
            }
            let path = builder.to_path();
            let hairline = 1.0 / sanitized_render_scale(render_scale) as f32;
            for (width, color) in [
                (hairline * 3.0, gdk::RGBA::BLACK),
                (hairline, gdk::RGBA::WHITE),
            ] {
                snapshot.append_stroke(&path, &gtk::gsk::Stroke::new(width), &color);
            }
        }
        for (kind, point) in handles(&selection.annotation) {
            let point = map(point);
            let outer = gtk::graphene::Rect::new(point.x() - 4.0, point.y() - 4.0, 8.0, 8.0);
            let outer_color = if selection.hot == Some(kind) {
                gdk::RGBA::new(1.0, 0.5, 0.0, 1.0)
            } else {
                gdk::RGBA::BLACK
            };
            snapshot.append_color(&outer_color, &outer);
            snapshot.append_color(
                &gdk::RGBA::WHITE,
                &gtk::graphene::Rect::new(point.x() - 2.5, point.y() - 2.5, 5.0, 5.0),
            );
        }
    }

    fn draw_pencil_dot(
        snapshot: &gtk::Snapshot,
        image_bounds: gtk::graphene::Rect,
        center: gtk::graphene::Point,
        diameter: f32,
        color: &gdk::RGBA,
    ) {
        let builder = gtk::gsk::PathBuilder::new();
        builder.add_circle(&center, diameter / 2.0);
        snapshot.push_clip(&image_bounds);
        snapshot.append_fill(&builder.to_path(), gtk::gsk::FillRule::Winding, color);
        snapshot.pop();
    }

    pub(super) fn draw_lens(
        snapshot: &gtk::Snapshot,
        bounds: gtk::graphene::Rect,
        lens: &Lens,
        pencil_overlay: Option<&PencilOverlay>,
        background: Background,
        auto_background_luminance: f32,
    ) {
        let center_x = lens.normalized_x.clamp(0.0, 1.0) * bounds.width();
        let center_y = lens.normalized_y.clamp(0.0, 1.0) * bounds.height();
        let diameter = lens.diameter.max(32.0);
        let clip = gtk::graphene::Rect::new(
            center_x - diameter / 2.0,
            center_y - diameter / 2.0,
            diameter,
            diameter,
        );
        let rounded = gtk::gsk::RoundedRect::from_rect(clip, diameter / 2.0);
        snapshot.push_rounded_clip(&rounded);
        draw_background(snapshot, bounds, background, auto_background_luminance);
        let magnification = lens.magnification.max(1.0);
        let source_bounds = contain_bounds(bounds, &lens.texture);
        let source_x =
            source_bounds.x() + lens.normalized_x.clamp(0.0, 1.0) * source_bounds.width();
        let source_y =
            source_bounds.y() + lens.normalized_y.clamp(0.0, 1.0) * source_bounds.height();
        let scaled = gtk::graphene::Rect::new(
            center_x - (source_x - source_bounds.x()) * magnification,
            center_y - (source_y - source_bounds.y()) * magnification,
            source_bounds.width() * magnification,
            source_bounds.height() * magnification,
        );
        snapshot.push_blend(gtk::gsk::BlendMode::Difference);
        snapshot.append_scaled_texture(&lens.texture, gtk::gsk::ScalingFilter::Nearest, &scaled);
        snapshot.pop();
        if let Some(overlay) = pencil_overlay {
            draw_pencil_overlay(
                snapshot,
                scaled,
                (lens.texture.width(), lens.texture.height()),
                overlay,
            );
        }
        if lens.show_cross {
            let cross = gdk::RGBA::WHITE;
            snapshot.append_color(
                &cross,
                &gtk::graphene::Rect::new(center_x - 5.0, center_y - 1.0, 10.0, 2.0),
            );
            snapshot.append_color(
                &cross,
                &gtk::graphene::Rect::new(center_x - 1.0, center_y - 5.0, 2.0, 10.0),
            );
        }
        snapshot.pop();
        snapshot.pop();
        let outline = gdk::RGBA::new(1.0, 1.0, 1.0, 0.9);
        snapshot.append_border(&rounded, &[2.0; 4], &[outline; 4]);
    }

    fn contain_bounds(bounds: gtk::graphene::Rect, texture: &gdk::Texture) -> gtk::graphene::Rect {
        let image_ratio = texture.width() as f32 / texture.height().max(1) as f32;
        let bounds_ratio = bounds.width() / bounds.height().max(1.0);
        if image_ratio > bounds_ratio {
            let height = bounds.width() / image_ratio;
            gtk::graphene::Rect::new(
                0.0,
                (bounds.height() - height) / 2.0,
                bounds.width(),
                height,
            )
        } else {
            let width = bounds.height() * image_ratio;
            gtk::graphene::Rect::new((bounds.width() - width) / 2.0, 0.0, width, bounds.height())
        }
    }

    fn draw_marker(
        snapshot: &gtk::Snapshot,
        bounds: gtk::graphene::Rect,
        normalized_x: f32,
        normalized_y: f32,
    ) {
        let center_x = normalized_x.clamp(0.0, 1.0) * bounds.width();
        let center_y = normalized_y.clamp(0.0, 1.0) * bounds.height();
        let rect = gtk::graphene::Rect::new(center_x - 7.0, center_y - 7.0, 14.0, 14.0);
        let rounded = gtk::gsk::RoundedRect::from_rect(rect, 7.0);
        let color = gdk::RGBA::new(1.0, 1.0, 1.0, 0.7);
        snapshot.append_border(&rounded, &[1.5; 4], &[color; 4]);
    }

    pub(super) fn draw_crop_overlay(
        snapshot: &gtk::Snapshot,
        image_bounds: gtk::graphene::Rect,
        overlay: &CropOverlay,
        render_scale: f64,
        dash_phase: f32,
    ) {
        let rect = overlay_rect(image_bounds, overlay);
        draw_dashed_crop_border(snapshot, rect, render_scale, dash_phase);
        draw_crop_handles(snapshot, rect);
    }

    fn draw_measurement_cursor(
        snapshot: &gtk::Snapshot,
        image_bounds: gtk::graphene::Rect,
        cursor: Option<(f32, f32)>,
        render_scale: f64,
    ) {
        let white = gdk::RGBA::WHITE;
        if let Some((normalized_x, normalized_y)) = cursor {
            let thickness = 1.0 / sanitized_render_scale(render_scale) as f32;
            let x = snap_to_device(
                image_bounds.x() + normalized_x.clamp(0.0, 1.0) * image_bounds.width(),
                render_scale,
            );
            let y = snap_to_device(
                image_bounds.y() + normalized_y.clamp(0.0, 1.0) * image_bounds.height(),
                render_scale,
            );
            snapshot.append_color(
                &white,
                &gtk::graphene::Rect::new(
                    x - thickness / 2.0,
                    image_bounds.y(),
                    thickness,
                    image_bounds.height(),
                ),
            );
            snapshot.append_color(
                &white,
                &gtk::graphene::Rect::new(
                    image_bounds.x(),
                    y - thickness / 2.0,
                    image_bounds.width(),
                    thickness,
                ),
            );
        }
    }

    pub(super) fn draw_dashed_crop_border(
        snapshot: &gtk::Snapshot,
        rect: gtk::graphene::Rect,
        render_scale: f64,
        phase: f32,
    ) {
        let black = gdk::RGBA::BLACK;
        let white = gdk::RGBA::WHITE;
        const DASH: f32 = 4.0;
        const CYCLE: f32 = DASH * 2.0;
        let thickness = 1.0 / sanitized_render_scale(render_scale) as f32;
        let rect = gtk::graphene::Rect::new(
            snap_to_device(rect.x(), render_scale),
            snap_to_device(rect.y(), render_scale),
            snap_to_device(rect.width(), render_scale),
            snap_to_device(rect.height(), render_scale),
        );

        for (horizontal, x, y, length) in [
            (true, rect.x(), rect.y(), rect.width()),
            (
                true,
                rect.x(),
                rect.y() + rect.height() - thickness,
                rect.width(),
            ),
            (false, rect.x(), rect.y(), rect.height()),
            (
                false,
                rect.x() + rect.width() - thickness,
                rect.y(),
                rect.height(),
            ),
        ] {
            let phase = phase.rem_euclid(CYCLE);
            let mut segment_index = (phase / DASH).floor() as i32;
            let mut offset = segment_index as f32 * DASH - phase;
            while offset < length {
                let start = offset.max(0.0);
                let end = (offset + DASH).min(length);
                if end > start {
                    let color = if segment_index.rem_euclid(2) == 0 {
                        &black
                    } else {
                        &white
                    };
                    let segment = if horizontal {
                        gtk::graphene::Rect::new(x + start, y, end - start, thickness)
                    } else {
                        gtk::graphene::Rect::new(x, y + start, thickness, end - start)
                    };
                    snapshot.append_color(color, &segment);
                }
                offset += DASH;
                segment_index += 1;
            }
        }
    }

    fn draw_crop_handles(snapshot: &gtk::Snapshot, rect: gtk::graphene::Rect) {
        let left = rect.x();
        let center_x = rect.x() + rect.width() / 2.0;
        let right = rect.x() + rect.width();
        let top = rect.y();
        let center_y = rect.y() + rect.height() / 2.0;
        let bottom = rect.y() + rect.height();
        for point in [
            (left, top),
            (center_x, top),
            (right, top),
            (right, center_y),
            (right, bottom),
            (center_x, bottom),
            (left, bottom),
            (left, center_y),
        ] {
            for (diameter, color) in [(9.0, gdk::RGBA::BLACK), (5.0, gdk::RGBA::WHITE)] {
                let builder = gtk::gsk::PathBuilder::new();
                builder.add_circle(&gtk::graphene::Point::new(point.0, point.1), diameter / 2.0);
                snapshot.append_fill(&builder.to_path(), gtk::gsk::FillRule::Winding, &color);
            }
        }
    }

    fn draw_background(
        snapshot: &gtk::Snapshot,
        bounds: gtk::graphene::Rect,
        mode: Background,
        auto_luminance: f32,
    ) {
        match mode {
            Background::Auto => {
                let grayscale = auto_luminance.clamp(0.0, 1.0);
                snapshot.append_color(
                    &gdk::RGBA::new(grayscale, grayscale, grayscale, 1.0),
                    &bounds,
                );
            }
            Background::White => snapshot.append_color(&gdk::RGBA::WHITE, &bounds),
            Background::Gray => {
                snapshot.append_color(&gdk::RGBA::new(0.32, 0.32, 0.32, 1.0), &bounds);
            }
            Background::Black => snapshot.append_color(&gdk::RGBA::BLACK, &bounds),
            Background::Checkerboard => {
                snapshot.append_color(&gdk::RGBA::new(0.76, 0.76, 0.76, 1.0), &bounds);
                let tile = 12.0;
                let columns = (bounds.width() / tile).ceil() as u32;
                let rows = (bounds.height() / tile).ceil() as u32;
                for y in 0..rows {
                    for x in 0..columns {
                        if (x + y) % 2 == 0 {
                            snapshot.append_color(
                                &gdk::RGBA::new(0.9, 0.9, 0.9, 1.0),
                                &gtk::graphene::Rect::new(
                                    x as f32 * tile,
                                    y as f32 * tile,
                                    tile,
                                    tile,
                                ),
                            );
                        }
                    }
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct ImageCanvas(ObjectSubclass<imp::ImageCanvas>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ImageCanvas {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl ImageCanvas {
    fn install_coordinate_tooltip(&self) {
        let pending = Rc::new(RefCell::new(None::<glib::SourceId>));
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion({
            let canvas = self.downgrade();
            let pending = pending.clone();
            move |_, x, y| {
                if let Some(source) = pending.borrow_mut().take() {
                    source.remove();
                }
                let Some(canvas) = canvas.upgrade() else {
                    return;
                };
                canvas.set_tooltip_text(None);
                if canvas.pixel_at(x, y).is_none() {
                    return;
                }
                let canvas = canvas.downgrade();
                let pending_for_timeout = pending.clone();
                let source = glib::timeout_add_local_once(COORDINATE_TOOLTIP_DELAY, move || {
                    pending_for_timeout.borrow_mut().take();
                    let Some(canvas) = canvas.upgrade() else {
                        return;
                    };
                    let Some(pixel) = canvas.pixel_at(x, y) else {
                        return;
                    };
                    canvas.set_tooltip_text(Some(&coordinate_tooltip_text(pixel)));
                    canvas.trigger_tooltip_query();
                });
                pending.replace(Some(source));
            }
        });
        motion.connect_leave({
            let canvas = self.downgrade();
            move |_| {
                if let Some(source) = pending.borrow_mut().take() {
                    source.remove();
                }
                if let Some(canvas) = canvas.upgrade() {
                    canvas.set_tooltip_text(None);
                }
            }
        });
        self.add_controller(motion);
    }

    fn image_bounds_for_texture(&self, texture: &gdk::Texture) -> gtk::graphene::Rect {
        canvas_layout(
            gtk::graphene::Rect::new(
                0.0,
                0.0,
                self.width().max(1) as f32,
                self.height().max(1) as f32,
            ),
            (texture.width(), texture.height()),
            self.imp().zoom.get(),
            self.imp().filter.get(),
            self.imp().render_scale.get(),
            self.imp().preview_scale.get(),
            self.surface_origin_device(),
        )
        .logical_bounds
    }

    fn surface_origin_device(&self) -> (f64, f64) {
        let render_scale = sanitized_render_scale(self.imp().render_scale.get());
        let Some(native) = self.native() else {
            return (0.0, 0.0);
        };
        let Some(native_widget) = self.root().and_downcast::<gtk::Window>() else {
            return (0.0, 0.0);
        };
        let point = self
            .compute_point(&native_widget, &gtk::graphene::Point::new(0.0, 0.0))
            .unwrap_or_else(|| gtk::graphene::Point::new(0.0, 0.0));
        let (surface_x, surface_y) = native.surface_transform();
        (
            (f64::from(point.x()) + surface_x) * render_scale,
            (f64::from(point.y()) + surface_y) * render_scale,
        )
    }

    pub fn set_texture(&self, texture: Option<&gdk::Texture>) {
        self.imp().texture.replace(texture.cloned());
        self.queue_resize();
        self.queue_draw();
    }

    pub fn texture(&self) -> Option<gdk::Texture> {
        self.imp().texture.borrow().clone()
    }

    pub fn zoom(&self) -> f64 {
        self.imp().zoom.get()
    }

    pub fn set_zoom(&self, zoom: f64) {
        self.imp().zoom.set(zoom.clamp(0.01, 64.0));
        self.queue_resize();
        self.queue_draw();
    }

    pub fn set_fit_zoom(&self, zoom: f64) {
        self.imp().zoom.set(bounded_fit_zoom(zoom));
        self.queue_resize();
        self.queue_draw();
    }

    pub fn set_render_scale(&self, scale: f64) {
        self.imp().render_scale.set(sanitized_render_scale(scale));
        self.queue_resize();
        self.queue_draw();
    }

    pub fn queue_draw_if_device_phase_changed(&self) {
        let render_scale = sanitized_render_scale(self.imp().render_scale.get());
        if render_scale.fract().abs() <= f64::EPSILON {
            return;
        }
        let (x, y) = self.surface_origin_device();
        let phase = (x.rem_euclid(1.0).to_bits(), y.rem_euclid(1.0).to_bits());
        if self.imp().device_origin_phase.replace(phase) != phase {
            self.queue_draw();
        }
    }

    pub fn filter(&self) -> ZoomFilter {
        self.imp().filter.get()
    }

    pub fn set_filter(&self, filter: ZoomFilter) {
        self.imp().filter.set(filter);
        self.queue_resize();
        self.queue_draw();
    }

    pub fn background(&self) -> Background {
        self.imp().background.get()
    }

    pub fn set_background(&self, background: Background) {
        self.imp().background.set(background);
        self.queue_draw();
    }

    pub fn set_auto_background_from_image(&self, image: &image::RgbaImage) {
        self.imp()
            .auto_background_luminance
            .set(opposite_grayscale_luminance(image));
        if self.background() == Background::Auto {
            self.queue_draw();
        }
    }

    pub fn set_lens(
        &self,
        texture: &gdk::Texture,
        normalized_x: f32,
        normalized_y: f32,
        diameter: f32,
        magnification: f32,
        show_cross: bool,
    ) {
        self.imp().lens.replace(Some(Lens {
            texture: texture.clone(),
            normalized_x,
            normalized_y,
            diameter,
            magnification,
            show_cross,
        }));
        self.queue_draw();
    }

    pub fn clear_lens(&self) {
        self.imp().lens.replace(None);
        self.queue_draw();
    }

    pub fn update_lens_texture(&self, texture: &gdk::Texture) {
        if let Some(lens) = self.imp().lens.borrow_mut().as_mut() {
            lens.texture = texture.clone();
            self.queue_draw();
        }
    }

    pub fn set_marker(&self, marker: Option<(f32, f32)>) {
        self.imp().marker.set(marker);
        self.queue_draw();
    }

    pub fn set_crop_overlay(&self, overlay: Option<CropOverlay>) {
        if self.imp().crop_overlay.replace(overlay) == overlay {
            return;
        }
        if overlay.is_some() && !self.imp().crop_animation_running.replace(true) {
            let canvas = self.downgrade();
            self.add_tick_callback(move |_, frame_clock| {
                let Some(canvas) = canvas.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if canvas.imp().crop_overlay.borrow().is_none() {
                    canvas.imp().crop_dash_phase.set(0.0);
                    canvas.imp().crop_animation_running.set(false);
                    return glib::ControlFlow::Break;
                }
                if canvas.settings().is_gtk_enable_animations() {
                    let seconds = frame_clock.frame_time() as f64 / 1_000_000.0;
                    canvas
                        .imp()
                        .crop_dash_phase
                        .set((seconds * 16.0).rem_euclid(8.0) as f32);
                    canvas.queue_draw();
                } else if canvas.imp().crop_dash_phase.replace(0.0) != 0.0 {
                    canvas.queue_draw();
                }
                glib::ControlFlow::Continue
            });
        }
        self.queue_draw();
    }

    pub fn set_measurement_cursor(&self, cursor: Option<(f32, f32)>) {
        if self.imp().measurement_cursor.replace(cursor) != cursor {
            self.queue_draw();
        }
    }

    pub fn set_pencil_overlay(
        &self,
        points: &[BrushPoint],
        path: StrokePath,
        color: [u8; 4],
        width: f32,
    ) {
        self.imp().pencil_overlay.replace(Some(PencilOverlay {
            points: points.to_vec(),
            path,
            color,
            width,
        }));
        self.queue_draw();
    }

    pub fn clear_pencil_overlay(&self) {
        if self.imp().pencil_overlay.borrow_mut().take().is_some() {
            self.queue_draw();
        }
    }

    pub fn set_annotation_preview(&self, overlay: Option<AnnotationOverlay>) {
        self.imp()
            .annotation_previews
            .borrow_mut()
            .set_active(overlay);
        self.queue_draw();
    }

    pub fn commit_annotation_preview(&self) {
        self.imp().annotation_previews.borrow_mut().commit_active();
    }

    pub fn finish_annotation_render(&self) {
        if self
            .imp()
            .annotation_previews
            .borrow_mut()
            .finish_document_render()
        {
            self.queue_draw();
        }
    }

    pub fn clear_annotation_previews(&self) {
        if self.imp().annotation_previews.borrow_mut().clear() {
            self.queue_draw();
        }
    }

    pub fn set_annotation_selection(&self, selection: Option<SelectionHandles>) {
        if self.imp().selection.borrow().as_ref() == selection.as_ref() {
            return;
        }
        self.imp().selection.replace(selection);
        self.queue_draw();
    }

    pub fn image_point_at(&self, x: f64, y: f64) -> Option<Point> {
        let texture = self.imp().texture.borrow();
        let texture = texture.as_ref()?;
        let bounds = self.image_bounds_for_texture(texture);
        if x < f64::from(bounds.x())
            || y < f64::from(bounds.y())
            || x > f64::from(bounds.x() + bounds.width())
            || y > f64::from(bounds.y() + bounds.height())
        {
            return None;
        }
        Some(Point {
            x: ((x as f32 - bounds.x()) / bounds.width() * texture.width() as f32)
                .clamp(0.0, texture.width() as f32),
            y: ((y as f32 - bounds.y()) / bounds.height() * texture.height() as f32)
                .clamp(0.0, texture.height() as f32),
        })
    }

    pub fn widget_point_for_image(&self, point: Point) -> Option<gtk::graphene::Point> {
        let texture = self.texture()?;
        let bounds = self.image_bounds_for_texture(&texture);
        Some(gtk::graphene::Point::new(
            bounds.x() + point.x * bounds.width() / texture.width().max(1) as f32,
            bounds.y() + point.y * bounds.height() / texture.height().max(1) as f32,
        ))
    }

    pub fn image_scale(&self) -> f32 {
        let texture = self.imp().texture.borrow();
        texture.as_ref().map_or(1.0, |texture| {
            self.image_bounds_for_texture(texture).width() / texture.width().max(1) as f32
        })
    }

    pub fn set_preview_scale(&self, scale: f32) {
        self.imp().preview_scale.set(scale.clamp(0.01, 64.0));
        self.queue_draw();
    }

    pub fn crop_display_bounds(&self, overlay: CropOverlay) -> Option<gtk::graphene::Rect> {
        let texture = self.texture()?;
        Some(overlay_rect(
            self.image_bounds_for_texture(&texture),
            &overlay,
        ))
    }

    pub fn pixel_at(&self, x: f64, y: f64) -> Option<(u32, u32)> {
        let texture = self.texture()?;
        let normalized = self.normalized_at(x, y)?;
        Some((
            (normalized.0 * texture.width() as f32) as u32,
            (normalized.1 * texture.height() as f32) as u32,
        ))
    }

    pub fn normalized_at(&self, x: f64, y: f64) -> Option<(f32, f32)> {
        let texture = self.texture()?;
        let bounds = self.image_bounds_for_texture(&texture);
        let x = x as f32;
        let y = y as f32;
        if x < bounds.x()
            || y < bounds.y()
            || x >= bounds.x() + bounds.width()
            || y >= bounds.y() + bounds.height()
        {
            return None;
        }
        Some((
            (x - bounds.x()) / bounds.width(),
            (y - bounds.y()) / bounds.height(),
        ))
    }

    pub fn snapped_normalized_at(&self, x: f64, y: f64) -> Option<(f32, f32)> {
        let texture = self.texture()?;
        let boundary = self.pixel_boundary_at(x, y)?;
        Some(normalized_pixel_boundary(
            boundary,
            (texture.width() as u32, texture.height() as u32),
        ))
    }

    pub fn pixel_boundary_at(&self, x: f64, y: f64) -> Option<(u32, u32)> {
        let texture = self.texture()?;
        let normalized = self.normalized_at(x, y)?;
        Some(pixel_boundary_from_normalized(
            normalized,
            (texture.width() as u32, texture.height() as u32),
        ))
    }

    pub fn clamped_pixel_boundary_at(&self, x: f64, y: f64) -> Option<(u32, u32)> {
        let texture = self.texture()?;
        let bounds = self.image_bounds_for_texture(&texture);
        let normalized = (
            ((x as f32 - bounds.x()) / bounds.width()).clamp(0.0, 1.0),
            ((y as f32 - bounds.y()) / bounds.height()).clamp(0.0, 1.0),
        );
        Some(pixel_boundary_from_normalized(
            normalized,
            (texture.width() as u32, texture.height() as u32),
        ))
    }

    pub fn set_accessible_label(&self, label: &str) {
        self.update_property(&[gtk::accessible::Property::Label(label)]);
    }
}

#[derive(Clone, Copy)]
struct MiniMapViewport {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

fn aspect_fit_bounds(
    bounds: gtk::graphene::Rect,
    image_width: i32,
    image_height: i32,
) -> gtk::graphene::Rect {
    let image_ratio = image_width.max(1) as f32 / image_height.max(1) as f32;
    let bounds_ratio = bounds.width() / bounds.height().max(1.0);
    if image_ratio > bounds_ratio {
        let height = bounds.width() / image_ratio;
        gtk::graphene::Rect::new(
            bounds.x(),
            bounds.y() + (bounds.height() - height) / 2.0,
            bounds.width(),
            height,
        )
    } else {
        let width = bounds.height() * image_ratio;
        gtk::graphene::Rect::new(
            bounds.x() + (bounds.width() - width) / 2.0,
            bounds.y(),
            width,
            bounds.height(),
        )
    }
}

mod minimap_imp {
    use super::*;

    #[derive(Default)]
    pub struct MiniMap {
        pub(super) texture: RefCell<Option<gdk::Texture>>,
        pub(super) viewport: Cell<Option<MiniMapViewport>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MiniMap {
        const NAME: &'static str = "DioramaMiniMap";
        type Type = super::MiniMap;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for MiniMap {}

    impl WidgetImpl for MiniMap {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let object = self.obj();
            let bounds = gtk::graphene::Rect::new(
                0.0,
                0.0,
                object.width().max(1) as f32,
                object.height().max(1) as f32,
            );
            if let Some(texture) = self.texture.borrow().as_ref() {
                let image_bounds = aspect_fit_bounds(bounds, texture.width(), texture.height());
                let image_rounded = gtk::gsk::RoundedRect::from_rect(image_bounds, 0.0);
                if let Some(viewport) = self.viewport.get() {
                    snapshot.push_blend(gtk::gsk::BlendMode::Difference);
                    snapshot.append_scaled_texture(
                        texture,
                        gtk::gsk::ScalingFilter::Linear,
                        &image_bounds,
                    );
                    snapshot.pop();
                    let viewport = gtk::graphene::Rect::new(
                        image_bounds.x() + viewport.x.clamp(0.0, 1.0) * image_bounds.width(),
                        image_bounds.y() + viewport.y.clamp(0.0, 1.0) * image_bounds.height(),
                        viewport.width.clamp(0.0, 1.0) * image_bounds.width(),
                        viewport.height.clamp(0.0, 1.0) * image_bounds.height(),
                    );
                    let viewport = gtk::gsk::RoundedRect::from_rect(viewport, 0.0);
                    snapshot.append_border(&viewport, &[1.0; 4], &[gdk::RGBA::WHITE; 4]);
                    snapshot.pop();
                } else {
                    snapshot.append_scaled_texture(
                        texture,
                        gtk::gsk::ScalingFilter::Linear,
                        &image_bounds,
                    );
                }
                snapshot.append_border(&image_rounded, &[1.0; 4], &[gdk::RGBA::BLACK; 4]);
            }
        }
    }
}

glib::wrapper! {
    pub struct MiniMap(ObjectSubclass<minimap_imp::MiniMap>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for MiniMap {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl MiniMap {
    pub fn set_texture(&self, texture: Option<&gdk::Texture>) {
        self.imp().texture.replace(texture.cloned());
        self.queue_draw();
    }

    pub fn set_viewport(&self, viewport: Option<(f32, f32, f32, f32)>) {
        self.imp()
            .viewport
            .set(viewport.map(|(x, y, width, height)| MiniMapViewport {
                x,
                y,
                width,
                height,
            }));
        self.queue_draw();
    }

    pub fn image_bounds(&self) -> Option<gtk::graphene::Rect> {
        let texture = self.imp().texture.borrow().clone()?;
        let bounds = gtk::graphene::Rect::new(
            0.0,
            0.0,
            self.width().max(1) as f32,
            self.height().max(1) as f32,
        );
        Some(aspect_fit_bounds(bounds, texture.width(), texture.height()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{
        AnnotationId, BrushPoint, PencilGeometry, Rect, Shape, StrokePath, StrokeStyle,
    };

    #[test]
    fn committed_annotation_preview_stays_visible_until_document_render_finishes() {
        let mut previews = PreviewLayers::default();
        previews.set_active(Some("last drag frame"));

        previews.commit_active();

        assert_eq!(
            previews.visible().copied().collect::<Vec<_>>(),
            ["last drag frame"]
        );
        previews.finish_document_render();
        assert!(previews.visible().next().is_none());
    }

    #[test]
    fn finishing_an_older_document_render_does_not_clear_the_active_preview() {
        let mut previews = PreviewLayers::default();
        previews.set_active(Some("committed"));
        previews.commit_active();
        previews.set_active(Some("new drag"));

        previews.finish_document_render();

        assert_eq!(
            previews.visible().copied().collect::<Vec<_>>(),
            ["new drag"]
        );
    }

    #[test]
    fn hard_zoom_is_the_default_filter() {
        assert_eq!(ZoomFilter::default(), ZoomFilter::Hard);
    }

    #[test]
    fn measurement_crosshair_uses_difference_blending() {
        assert_eq!(
            MEASUREMENT_CURSOR_BLEND_MODE,
            gtk::gsk::BlendMode::Difference
        );
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn measurement_crosshair_is_the_top_child_of_a_difference_blend() {
        gtk::init().expect("GTK display initialization");
        let pixels = glib::Bytes::from_owned(vec![0_u8; 4 * 4 * 4]);
        let texture: gdk::Texture =
            gdk::MemoryTexture::new(4, 4, gdk::MemoryFormat::R8g8b8a8, &pixels, 4 * 4).upcast();
        let bounds = gtk::graphene::Rect::new(0.0, 0.0, 4.0, 4.0);
        let snapshot = gtk::Snapshot::new();

        imp::append_measurement_cursor_xor(
            &snapshot,
            &texture,
            gtk::gsk::ScalingFilter::Nearest,
            CanvasLayout {
                logical_bounds: bounds,
                device_bounds: None,
            },
            bounds,
            Some((0.5, 0.5)),
            1.0,
        );

        let blend = snapshot
            .to_node()
            .expect("measurement render node")
            .downcast::<gtk::gsk::BlendNode>()
            .expect("measurement cursor must wrap the image in a blend node");
        assert_eq!(blend.blend_mode(), gtk::gsk::BlendMode::Difference);
        assert!(
            blend
                .top_child()
                .downcast::<gtk::gsk::ContainerNode>()
                .is_ok(),
            "the two crosshair lines must occupy the blend's top child"
        );
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn pencil_overlay_builds_render_nodes_for_every_path_shape() {
        gtk::init().expect("GTK display initialization");
        let point = |x, y| BrushPoint {
            x,
            y,
            pressure: 1.0,
        };
        let cases = [
            (vec![point(4.5, 4.5)], StrokePath::Smooth),
            (
                vec![point(1.5, 1.5), point(4.5, 6.5), point(8.5, 2.5)],
                StrokePath::Smooth,
            ),
            (
                vec![
                    point(1.5, 1.5),
                    point(8.5, 1.5),
                    point(8.5, 8.5),
                    point(1.5, 8.5),
                    point(1.5, 1.5),
                ],
                StrokePath::Linear,
            ),
            (vec![point(5.5, 5.5), point(5.5, 5.5)], StrokePath::Circle),
            (vec![point(5.5, 5.5), point(9.5, 5.5)], StrokePath::Circle),
        ];

        for (points, path) in cases {
            let snapshot = gtk::Snapshot::new();
            imp::draw_pencil_overlay(
                &snapshot,
                gtk::graphene::Rect::new(0.0, 0.0, 10.0, 10.0),
                (10, 10),
                &PencilOverlay {
                    points,
                    path,
                    color: [255, 0, 0, 255],
                    width: 1.0,
                },
            );
            assert!(snapshot.to_node().is_some());
        }
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn lens_magnifies_the_active_pencil_overlay() {
        fn find_stroke(node: &gtk::gsk::RenderNode) -> Option<gtk::gsk::StrokeNode> {
            if let Ok(stroke) = node.clone().downcast::<gtk::gsk::StrokeNode>() {
                return Some(stroke);
            }
            if let Ok(container) = node.clone().downcast::<gtk::gsk::ContainerNode>() {
                return (0..container.n_children())
                    .map(|index| container.child(index))
                    .find_map(|child| find_stroke(&child));
            }
            if let Ok(clip) = node.clone().downcast::<gtk::gsk::ClipNode>() {
                return find_stroke(&clip.child());
            }
            if let Ok(clip) = node.clone().downcast::<gtk::gsk::RoundedClipNode>() {
                return find_stroke(&clip.child());
            }
            if let Ok(blend) = node.clone().downcast::<gtk::gsk::BlendNode>() {
                return find_stroke(&blend.bottom_child())
                    .or_else(|| find_stroke(&blend.top_child()));
            }
            None
        }

        gtk::init().expect("GTK display initialization");
        let pixels = glib::Bytes::from_owned(vec![0_u8; 10 * 10 * 4]);
        let texture: gdk::Texture =
            gdk::MemoryTexture::new(10, 10, gdk::MemoryFormat::R8g8b8a8, &pixels, 10 * 4).upcast();
        let lens = Lens {
            texture,
            normalized_x: 0.5,
            normalized_y: 0.5,
            diameter: 80.0,
            magnification: 4.0,
            show_cross: true,
        };
        let overlay = PencilOverlay {
            points: vec![
                BrushPoint {
                    x: 4.5,
                    y: 4.5,
                    pressure: 1.0,
                },
                BrushPoint {
                    x: 5.5,
                    y: 5.5,
                    pressure: 1.0,
                },
            ],
            path: StrokePath::Linear,
            color: [255, 0, 0, 255],
            width: 1.0,
        };
        let snapshot = gtk::Snapshot::new();
        imp::draw_lens(
            &snapshot,
            gtk::graphene::Rect::new(0.0, 0.0, 100.0, 100.0),
            &lens,
            Some(&overlay),
            Background::Checkerboard,
            0.5,
        );

        let node = snapshot.to_node().expect("lens render node");
        let stroke = find_stroke(&node).expect("magnified pencil stroke inside lens");
        assert_eq!(stroke.stroke().line_width(), 40.0);
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn lens_draws_the_configured_transparency_background() {
        fn contains_color(node: &gtk::gsk::RenderNode, expected: &gdk::RGBA) -> bool {
            if let Ok(color) = node.clone().downcast::<gtk::gsk::ColorNode>() {
                return color.color() == *expected;
            }
            if let Ok(container) = node.clone().downcast::<gtk::gsk::ContainerNode>() {
                return (0..container.n_children())
                    .map(|index| container.child(index))
                    .any(|child| contains_color(&child, expected));
            }
            if let Ok(clip) = node.clone().downcast::<gtk::gsk::RoundedClipNode>() {
                return contains_color(&clip.child(), expected);
            }
            if let Ok(blend) = node.clone().downcast::<gtk::gsk::BlendNode>() {
                return contains_color(&blend.bottom_child(), expected)
                    || contains_color(&blend.top_child(), expected);
            }
            false
        }

        gtk::init().expect("GTK display initialization");
        let pixels = glib::Bytes::from_owned(vec![0_u8; 4]);
        let texture: gdk::Texture =
            gdk::MemoryTexture::new(1, 1, gdk::MemoryFormat::R8g8b8a8, &pixels, 4).upcast();
        let lens = Lens {
            texture,
            normalized_x: 0.5,
            normalized_y: 0.5,
            diameter: 80.0,
            magnification: 4.0,
            show_cross: false,
        };
        let snapshot = gtk::Snapshot::new();

        imp::draw_lens(
            &snapshot,
            gtk::graphene::Rect::new(0.0, 0.0, 100.0, 100.0),
            &lens,
            None,
            Background::Gray,
            0.5,
        );

        let node = snapshot.to_node().expect("lens render node");
        assert!(contains_color(
            &node,
            &gdk::RGBA::new(0.32, 0.32, 0.32, 1.0)
        ));
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn drag_rectangle_border_is_one_pixel_black_and_white() {
        gtk::init().expect("GTK display initialization");
        let snapshot = gtk::Snapshot::new();
        imp::draw_dashed_crop_border(
            &snapshot,
            gtk::graphene::Rect::new(2.0, 3.0, 32.0, 24.0),
            1.0,
            0.0,
        );
        let border = snapshot
            .to_node()
            .expect("border render node")
            .downcast::<gtk::gsk::ContainerNode>()
            .expect("border segment container");
        let mut black_segments = 0;
        let mut white_segments = 0;

        for index in 0..border.n_children() {
            let segment = border
                .child(index)
                .downcast::<gtk::gsk::ColorNode>()
                .expect("solid border segment");
            let bounds = segment.bounds();
            assert!(
                bounds.width() == 1.0 || bounds.height() == 1.0,
                "each border segment must be exactly one pixel thick: {bounds:?}"
            );
            let dash_length = if bounds.width() == 1.0 {
                bounds.height()
            } else {
                bounds.width()
            };
            assert!(
                dash_length <= 4.0,
                "selection dashes must be at most four pixels long: {bounds:?}"
            );
            match segment.color() {
                color if color == gdk::RGBA::BLACK => black_segments += 1,
                color if color == gdk::RGBA::WHITE => white_segments += 1,
                color => panic!("unexpected drag rectangle color: {color:?}"),
            }
        }

        assert!(black_segments > 0);
        assert!(white_segments > 0);

        let shifted = gtk::Snapshot::new();
        imp::draw_dashed_crop_border(
            &shifted,
            gtk::graphene::Rect::new(2.0, 3.0, 32.0, 24.0),
            1.0,
            2.0,
        );
        let shifted = shifted
            .to_node()
            .expect("shifted border render node")
            .downcast::<gtk::gsk::ContainerNode>()
            .expect("shifted border segment container");
        let first = shifted
            .child(0)
            .downcast::<gtk::gsk::ColorNode>()
            .expect("first shifted segment");
        assert_eq!(first.bounds().width(), 2.0);
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn region_selection_draws_eight_resize_handles() {
        fn count_fills(node: &gtk::gsk::RenderNode) -> u32 {
            if node.clone().downcast::<gtk::gsk::FillNode>().is_ok() {
                return 1;
            }
            node.clone()
                .downcast::<gtk::gsk::ContainerNode>()
                .map_or(0, |container| {
                    (0..container.n_children())
                        .map(|index| count_fills(&container.child(index)))
                        .sum()
                })
        }

        gtk::init().expect("GTK display initialization");
        let snapshot = gtk::Snapshot::new();
        imp::draw_crop_overlay(
            &snapshot,
            gtk::graphene::Rect::new(0.0, 0.0, 100.0, 80.0),
            &CropOverlay {
                x: 10,
                y: 10,
                width: 50,
                height: 40,
                image_width: 100,
                image_height: 80,
            },
            1.0,
            0.0,
        );

        assert_eq!(
            count_fills(&snapshot.to_node().expect("selection render node")),
            16,
            "eight handles must each have an outer and inner dot"
        );
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn selected_pencil_rectangle_draws_eight_resize_handles() {
        fn count_colors(node: &gtk::gsk::RenderNode) -> u32 {
            if node.clone().downcast::<gtk::gsk::ColorNode>().is_ok() {
                return 1;
            }
            node.clone()
                .downcast::<gtk::gsk::ContainerNode>()
                .map_or(0, |container| {
                    (0..container.n_children())
                        .map(|index| count_colors(&container.child(index)))
                        .sum()
                })
        }

        gtk::init().expect("GTK display initialization");
        let snapshot = gtk::Snapshot::new();
        imp::draw_annotation_selection(
            &snapshot,
            gtk::graphene::Rect::new(0.0, 0.0, 100.0, 80.0),
            (100, 80),
            &SelectionHandles {
                annotation: Annotation {
                    id: AnnotationId(1),
                    shape: Shape::Pencil {
                        geometry: PencilGeometry::Rectangle(Rect {
                            x: 10.0,
                            y: 10.0,
                            width: 50.0,
                            height: 40.0,
                        }),
                        style: StrokeStyle {
                            color: [255, 0, 0, 255],
                            width: 3.0,
                        },
                        anti_aliasing: true,
                    },
                },
                hot: None,
            },
            1.0,
        );

        assert_eq!(
            count_colors(&snapshot.to_node().expect("pencil selection render node")),
            16,
            "eight handles must each have an outer and inner node"
        );
    }

    #[test]
    fn hard_zoom_measurement_reserves_the_full_render_aligned_extent() {
        assert_eq!(measured_image_dimension(3, 1.0, ZoomFilter::Hard, 2.0), 3);
        assert_eq!(measured_image_dimension(3, 1.5, ZoomFilter::Hard, 2.0), 5);
        assert_eq!(measured_image_dimension(3, 1.25, ZoomFilter::Hard, 1.0), 4);
    }

    #[test]
    fn one_hundred_percent_maps_every_source_pixel_to_equal_render_blocks() {
        let render_scale = 2.0;
        let bounds = gtk::graphene::Rect::new(0.0, 0.0, 101.0, 3.0);
        let image_bounds =
            canvas_image_bounds(bounds, (101, 3), 1.0, ZoomFilter::Hard, render_scale, 1.0);

        assert!((f64::from(image_bounds.x()) * render_scale).fract().abs() < 1e-6);
        assert!((f64::from(image_bounds.y()) * render_scale).fract().abs() < 1e-6);
        assert_eq!(image_bounds.width(), 101.0);
        assert_eq!(image_bounds.height(), 3.0);
        assert!(
            (f64::from(image_bounds.width()) * render_scale - 202.0).abs() < 1e-5,
            "each source pixel must occupy two GTK render-buffer pixels"
        );
        assert!(
            (f64::from(image_bounds.height()) * render_scale - 6.0).abs() < 1e-5,
            "each source pixel must occupy two GTK render-buffer pixels"
        );
    }

    #[test]
    fn fractional_or_soft_zoom_keeps_the_requested_contained_bounds() {
        assert_eq!(aligned_render_pixel_scale(1.25, 1.0), None);
        assert_eq!(aligned_render_pixel_scale(1.0, 2.0), Some(2.0));
        assert_eq!(aligned_render_pixel_scale(0.8, 2.0), None);
        assert_eq!(aligned_render_pixel_scale(0.8, 1.25), Some(1.0));
        assert_eq!(aligned_render_pixel_scale(1.5, 2.0), Some(3.0));
        assert_eq!(sanitized_render_scale(1.666_667), 1.666_667);
        assert_eq!(sanitized_render_scale(f64::NAN), 1.0);
        assert_eq!(sanitized_render_scale(0.0), 1.0);

        let bounds = gtk::graphene::Rect::new(0.0, 0.0, 4.0, 4.0);
        let soft = canvas_image_bounds(bounds, (4, 2), 1.0, ZoomFilter::Soft, 2.0, 1.0);
        assert_eq!(
            (soft.x(), soft.y(), soft.width(), soft.height()),
            (0.0, 1.0, 4.0, 2.0)
        );
    }

    #[test]
    fn fit_zoom_can_go_below_the_manual_one_percent_floor() {
        assert_eq!(bounded_fit_zoom(0.005), 0.005);
    }

    #[test]
    fn device_rect_origin_is_integral_in_surface_space() {
        let bounds = gtk::graphene::Rect::new(0.0, 0.0, 101.0, 79.0);
        for render_scale in [1.0, 1.25, 1.5, 2.0] {
            for origin in [(0.0, 0.0), (0.2, 0.7), (12.375, 5.125)] {
                for pixel_scale in [1.0, 2.0, 3.0, 8.0] {
                    let layout = canvas_layout(
                        bounds,
                        (17, 11),
                        pixel_scale / render_scale,
                        ZoomFilter::Hard,
                        render_scale,
                        1.0,
                        origin,
                    );
                    let logical = layout.logical_bounds;
                    let x = origin.0 + f64::from(logical.x()) * render_scale;
                    let y = origin.1 + f64::from(logical.y()) * render_scale;
                    assert!((x - x.round()).abs() < 1e-4);
                    assert!((y - y.round()).abs() < 1e-4);
                }
            }
        }
    }

    #[test]
    fn auto_background_uses_opposite_grayscale_luminance() {
        let black = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]));
        let white = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 255, 255, 255]));
        let transparent = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));

        assert_eq!(opposite_grayscale_luminance(&black), 1.0);
        assert_eq!(opposite_grayscale_luminance(&white), 0.0);
        assert_eq!(opposite_grayscale_luminance(&transparent), 0.5);
    }

    #[test]
    fn minimap_bounds_preserve_wide_image_aspect_ratio() {
        let bounds = gtk::graphene::Rect::new(0.0, 0.0, 160.0, 120.0);
        let fitted = aspect_fit_bounds(bounds, 1600, 900);

        assert_eq!(fitted.x(), 0.0);
        assert_eq!(fitted.y(), 15.0);
        assert_eq!(fitted.width(), 160.0);
        assert_eq!(fitted.height(), 90.0);
    }

    #[test]
    fn minimap_bounds_preserve_tall_image_aspect_ratio() {
        let bounds = gtk::graphene::Rect::new(0.0, 0.0, 160.0, 120.0);
        let fitted = aspect_fit_bounds(bounds, 800, 1200);

        assert_eq!(fitted.x(), 40.0);
        assert_eq!(fitted.y(), 0.0);
        assert_eq!(fitted.width(), 80.0);
        assert_eq!(fitted.height(), 120.0);
    }

    #[test]
    fn coordinate_tooltip_reports_zero_based_source_pixel_position() {
        assert_eq!(coordinate_tooltip_text((0, 0)), "X 0 · Y 0");
        assert_eq!(coordinate_tooltip_text((123, 45)), "X 123 · Y 45");
    }

    #[test]
    fn normalized_pixel_boundary_uses_the_source_grid_phase() {
        assert_eq!(normalized_pixel_boundary((0, 0), (4, 8)), (0.0, 0.0));
        assert_eq!(normalized_pixel_boundary((2, 3), (4, 8)), (0.5, 0.375));
        assert_eq!(normalized_pixel_boundary((4, 8), (4, 8)), (1.0, 1.0));
    }

    #[test]
    fn normalized_pixel_boundary_clamps_to_valid_grid_bounds() {
        assert_eq!(normalized_pixel_boundary((9, 9), (1, 1)), (1.0, 1.0));
        assert_eq!(normalized_pixel_boundary((9, 9), (0, 0)), (0.0, 0.0));
    }

    #[test]
    fn normalized_position_snaps_to_the_nearest_grid_intersection() {
        assert_eq!(
            pixel_boundary_from_normalized((0.124, 0.124), (4, 4)),
            (0, 0)
        );
        assert_eq!(
            pixel_boundary_from_normalized((0.126, 0.126), (4, 4)),
            (1, 1)
        );
        assert_eq!(pixel_boundary_from_normalized((1.0, 1.0), (4, 4)), (4, 4));
    }
}
