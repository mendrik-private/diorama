use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::document::{BrushPoint, StrokePath};

const MAX_FIT_ZOOM: f64 = 16_384.0;
const COORDINATE_TOOLTIP_DELAY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ZoomFilter {
    Soft,
    #[default]
    Hard,
}

fn normalized_render_scale(scale: f64) -> f64 {
    if scale.is_finite() && scale > 0.0 {
        scale.round().max(1.0)
    } else {
        1.0
    }
}

fn aligned_render_pixel_scale(zoom: f64, render_scale: f64) -> Option<f64> {
    let effective_scale = zoom * normalized_render_scale(render_scale);
    let aligned = effective_scale.round();
    (aligned >= 1.0 && (effective_scale - aligned).abs() <= 1e-6 * effective_scale.abs().max(1.0))
        .then_some(aligned)
}

fn coordinate_tooltip_text((x, y): (u32, u32)) -> String {
    format!("X {x} · Y {y}")
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

fn canvas_image_bounds(
    bounds: gtk::graphene::Rect,
    image_dimensions: (i32, i32),
    zoom: f64,
    filter: ZoomFilter,
    render_scale: f64,
    preview_scale: f32,
) -> gtk::graphene::Rect {
    let image_width = image_dimensions.0.max(1);
    let image_height = image_dimensions.1.max(1);
    let mut image_bounds = if filter == ZoomFilter::Hard {
        aligned_render_pixel_scale(zoom, render_scale).map(|pixel_scale| {
            let render_scale = normalized_render_scale(render_scale);
            let width = f64::from(image_width) * pixel_scale / render_scale;
            let height = f64::from(image_height) * pixel_scale / render_scale;
            let x = ((f64::from(bounds.x()) + (f64::from(bounds.width()) - width) / 2.0)
                * render_scale)
                .round()
                / render_scale;
            let y = ((f64::from(bounds.y()) + (f64::from(bounds.height()) - height) / 2.0)
                * render_scale)
                .round()
                / render_scale;
            gtk::graphene::Rect::new(x as f32, y as f32, width as f32, height as f32)
        })
    } else {
        None
    }
    .unwrap_or_else(|| {
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
    });

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
    image_bounds
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

#[derive(Debug, Clone, Copy)]
pub struct CropOverlay {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub image_width: u32,
    pub image_height: u32,
}

#[derive(Debug, Clone)]
struct MaskFlash {
    texture: gdk::Texture,
    bounds: CropOverlay,
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
        pub(super) lens: RefCell<Option<Lens>>,
        pub marker: Cell<Option<(f32, f32)>>,
        pub crop_overlay: RefCell<Option<CropOverlay>>,
        pub measurement_overlay: Cell<Option<CropOverlay>>,
        pub measurement_cursor: Cell<Option<(f32, f32)>>,
        pub(super) mask_flash: RefCell<Option<MaskFlash>>,
        pub(super) pencil_overlay: RefCell<Option<PencilOverlay>>,
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
            object.update_property(&[gtk::accessible::Property::Label("Image canvas")]);
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
                let image_bounds = canvas_image_bounds(
                    bounds,
                    (texture.width(), texture.height()),
                    self.zoom.get(),
                    self.filter.get(),
                    self.render_scale.get(),
                    self.preview_scale.get(),
                );
                let filter = match self.filter.get() {
                    ZoomFilter::Soft => gtk::gsk::ScalingFilter::Linear,
                    ZoomFilter::Hard => gtk::gsk::ScalingFilter::Nearest,
                };
                let measurement = self.measurement_overlay.get();
                let measurement_cursor = self.measurement_cursor.get();
                if measurement.is_some() || measurement_cursor.is_some() {
                    snapshot.push_blend(gtk::gsk::BlendMode::Difference);
                    snapshot.append_scaled_texture(texture, filter, &image_bounds);
                    snapshot.pop();
                    draw_measurement_layer(
                        snapshot,
                        &object,
                        image_bounds,
                        measurement,
                        measurement_cursor,
                    );
                    snapshot.pop();
                } else {
                    snapshot.append_scaled_texture(texture, filter, &image_bounds);
                }
                image_bounds
            });
            if let Some(lens) = self.lens.borrow().as_ref() {
                draw_lens(snapshot, bounds, lens);
            }
            if let Some(image_bounds) = image_bounds
                && let Some(overlay) = self.pencil_overlay.borrow().as_ref()
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
            if let Some((x, y)) = self.marker.get() {
                draw_marker(snapshot, bounds, x, y);
            }
            if let Some(image_bounds) = image_bounds {
                if let Some(flash) = self.mask_flash.borrow().as_ref() {
                    draw_mask_flash(snapshot, image_bounds, flash);
                }
                if let Some(overlay) = self.crop_overlay.borrow().as_ref() {
                    draw_crop_overlay(snapshot, image_bounds, overlay);
                }
            }
        }
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

    fn draw_lens(snapshot: &gtk::Snapshot, bounds: gtk::graphene::Rect, lens: &Lens) {
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

    fn draw_crop_overlay(
        snapshot: &gtk::Snapshot,
        image_bounds: gtk::graphene::Rect,
        overlay: &CropOverlay,
    ) {
        draw_dashed_crop_border(snapshot, overlay_rect(image_bounds, overlay));
    }

    fn draw_mask_flash(
        snapshot: &gtk::Snapshot,
        image_bounds: gtk::graphene::Rect,
        flash: &MaskFlash,
    ) {
        snapshot.append_scaled_texture(
            &flash.texture,
            gtk::gsk::ScalingFilter::Nearest,
            &overlay_rect(image_bounds, &flash.bounds),
        );
    }

    fn draw_measurement_layer(
        snapshot: &gtk::Snapshot,
        canvas: &super::ImageCanvas,
        image_bounds: gtk::graphene::Rect,
        measurement: Option<CropOverlay>,
        cursor: Option<(f32, f32)>,
    ) {
        let white = gdk::RGBA::WHITE;
        if let Some((normalized_x, normalized_y)) = cursor {
            let x = image_bounds.x() + normalized_x.clamp(0.0, 1.0) * image_bounds.width();
            let y = image_bounds.y() + normalized_y.clamp(0.0, 1.0) * image_bounds.height();
            snapshot.append_color(
                &white,
                &gtk::graphene::Rect::new(x - 0.5, image_bounds.y(), 1.0, image_bounds.height()),
            );
            snapshot.append_color(
                &white,
                &gtk::graphene::Rect::new(image_bounds.x(), y - 0.5, image_bounds.width(), 1.0),
            );
        }
        let Some(measurement) = measurement else {
            return;
        };
        let rect = overlay_rect(image_bounds, &measurement);
        let rounded = gtk::gsk::RoundedRect::from_rect(rect, 0.0);
        snapshot.append_border(&rounded, &[1.0; 4], &[white; 4]);
        let (origin_label, width_label, height_label) = measurement_labels(measurement);
        let [origin_anchor, width_anchor, height_anchor] = measurement_label_anchors(rect);
        append_measurement_label(
            snapshot,
            canvas,
            &origin_label,
            origin_anchor.0,
            origin_anchor.1,
            MeasurementLabelPlacement::InsideTopLeft,
            image_bounds,
        );
        append_measurement_label(
            snapshot,
            canvas,
            &width_label,
            width_anchor.0,
            width_anchor.1,
            MeasurementLabelPlacement::AboveCenter,
            image_bounds,
        );
        append_measurement_label(
            snapshot,
            canvas,
            &height_label,
            height_anchor.0,
            height_anchor.1,
            MeasurementLabelPlacement::RightCenter,
            image_bounds,
        );
    }

    pub(super) fn measurement_label_anchors(rect: gtk::graphene::Rect) -> [(f32, f32); 3] {
        [
            (rect.x(), rect.y()),
            (rect.x() + rect.width() / 2.0, rect.y() - 3.0),
            (
                rect.x() + rect.width() + 4.0,
                rect.y() + rect.height() / 2.0,
            ),
        ]
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum MeasurementLabelPlacement {
        InsideTopLeft,
        AboveCenter,
        RightCenter,
    }

    fn append_measurement_label(
        snapshot: &gtk::Snapshot,
        canvas: &super::ImageCanvas,
        text: &str,
        anchor_x: f32,
        anchor_y: f32,
        placement: MeasurementLabelPlacement,
        image_bounds: gtk::graphene::Rect,
    ) {
        let layout = canvas.create_pango_layout(Some(text));
        layout.set_font_description(Some(&gtk::pango::FontDescription::from_string("Sans 9")));
        let (width, height) = layout.pixel_size();
        let width = width as f32;
        let height = height as f32;
        let (x, y) = measurement_label_position(
            (anchor_x, anchor_y),
            (width, height),
            placement,
            image_bounds,
        );
        snapshot.save();
        snapshot.translate(&gtk::graphene::Point::new(x, y));
        snapshot.append_layout(&layout, &gdk::RGBA::WHITE);
        snapshot.restore();
    }

    fn measurement_label_position(
        anchor: (f32, f32),
        label_size: (f32, f32),
        placement: MeasurementLabelPlacement,
        image_bounds: gtk::graphene::Rect,
    ) -> (f32, f32) {
        let (width, height) = label_size;
        let (x, y) = match placement {
            MeasurementLabelPlacement::InsideTopLeft => (anchor.0 + 4.0, anchor.1 + 4.0),
            MeasurementLabelPlacement::AboveCenter => (
                anchor.0 - width / 2.0,
                if anchor.1 - height >= image_bounds.y() {
                    anchor.1 - height
                } else {
                    anchor.1 + 6.0
                },
            ),
            MeasurementLabelPlacement::RightCenter => {
                let x = if anchor.0 + width <= image_bounds.x() + image_bounds.width() {
                    anchor.0
                } else {
                    anchor.0 - width - 8.0
                };
                (x, anchor.1 - height / 2.0)
            }
        };
        (
            x.clamp(
                image_bounds.x(),
                (image_bounds.x() + image_bounds.width() - width).max(image_bounds.x()),
            ),
            y.clamp(
                image_bounds.y(),
                (image_bounds.y() + image_bounds.height() - height).max(image_bounds.y()),
            ),
        )
    }

    pub(super) fn measurement_labels(measurement: CropOverlay) -> (String, String, String) {
        (
            format!("X {} · Y {}", measurement.x, measurement.y),
            format!("W {} px", measurement.width),
            format!("H {} px", measurement.height),
        )
    }

    pub(super) fn draw_dashed_crop_border(snapshot: &gtk::Snapshot, rect: gtk::graphene::Rect) {
        let black = gdk::RGBA::BLACK;
        let white = gdk::RGBA::WHITE;
        const DASH: f32 = 8.0;
        const GAP: f32 = 4.0;
        const THICKNESS: f32 = 1.0;

        for (horizontal, x, y, length) in [
            (true, rect.x(), rect.y(), rect.width()),
            (
                true,
                rect.x(),
                rect.y() + rect.height() - THICKNESS,
                rect.width(),
            ),
            (false, rect.x(), rect.y(), rect.height()),
            (
                false,
                rect.x() + rect.width() - THICKNESS,
                rect.y(),
                rect.height(),
            ),
        ] {
            let mut offset = 0.0;
            let mut is_black = true;
            while offset < length {
                let dash = (length - offset).min(DASH);
                let color = if is_black { &black } else { &white };
                let segment = if horizontal {
                    gtk::graphene::Rect::new(x + offset, y, dash, THICKNESS)
                } else {
                    gtk::graphene::Rect::new(x, y + offset, THICKNESS, dash)
                };
                snapshot.append_color(color, &segment);
                offset += DASH + GAP;
                is_black = !is_black;
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
        canvas_image_bounds(
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
        self.imp().zoom.set(zoom.clamp(0.01, MAX_FIT_ZOOM));
        self.queue_resize();
        self.queue_draw();
    }

    pub fn set_render_scale(&self, scale: f64) {
        self.imp().render_scale.set(normalized_render_scale(scale));
        self.queue_resize();
        self.queue_draw();
    }

    pub fn zoom_in(&self) {
        self.set_zoom(self.zoom() * 1.25);
    }

    pub fn zoom_out(&self) {
        self.set_zoom(self.zoom() / 1.25);
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
        self.imp().crop_overlay.replace(overlay);
        self.queue_draw();
    }

    pub fn set_measurement_overlay(&self, overlay: Option<CropOverlay>) {
        self.imp().measurement_overlay.set(overlay);
        self.queue_draw();
    }

    pub fn set_measurement_cursor(&self, cursor: Option<(f32, f32)>) {
        self.imp().measurement_cursor.set(cursor);
        self.queue_draw();
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

    pub fn set_mask_flash(&self, texture: Option<&gdk::Texture>, bounds: CropOverlay) {
        self.imp()
            .mask_flash
            .replace(texture.map(|texture| MaskFlash {
                texture: texture.clone(),
                bounds,
            }));
        self.queue_draw();
    }

    pub fn clear_mask_flash(&self) {
        self.imp().mask_flash.replace(None);
        self.queue_draw();
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
    use crate::document::{BrushPoint, StrokePath};

    #[test]
    fn hard_zoom_is_the_default_filter() {
        assert_eq!(ZoomFilter::default(), ZoomFilter::Hard);
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
    fn drag_rectangle_border_is_one_pixel_black_and_white() {
        gtk::init().expect("GTK display initialization");
        let snapshot = gtk::Snapshot::new();
        imp::draw_dashed_crop_border(&snapshot, gtk::graphene::Rect::new(2.0, 3.0, 32.0, 24.0));
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
            match segment.color() {
                color if color == gdk::RGBA::BLACK => black_segments += 1,
                color if color == gdk::RGBA::WHITE => white_segments += 1,
                color => panic!("unexpected drag rectangle color: {color:?}"),
            }
        }

        assert!(black_segments > 0);
        assert!(white_segments > 0);
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
        assert_eq!(normalized_render_scale(1.666_667), 2.0);
        assert_eq!(normalized_render_scale(f64::NAN), 1.0);
        assert_eq!(normalized_render_scale(0.0), 1.0);

        let bounds = gtk::graphene::Rect::new(0.0, 0.0, 4.0, 4.0);
        let soft = canvas_image_bounds(bounds, (4, 2), 1.0, ZoomFilter::Soft, 2.0, 1.0);
        assert_eq!(
            (soft.x(), soft.y(), soft.width(), soft.height()),
            (0.0, 1.0, 4.0, 2.0)
        );
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
    fn measurement_labels_report_source_pixel_position_and_dimensions() {
        let measurement = CropOverlay {
            x: 10,
            y: 20,
            width: 31,
            height: 17,
            image_width: 100,
            image_height: 80,
        };

        assert_eq!(
            imp::measurement_labels(measurement),
            (
                "X 10 · Y 20".to_owned(),
                "W 31 px".to_owned(),
                "H 17 px".to_owned()
            )
        );
    }

    #[test]
    fn coordinate_tooltip_reports_zero_based_source_pixel_position() {
        assert_eq!(coordinate_tooltip_text((0, 0)), "X 0 · Y 0");
        assert_eq!(coordinate_tooltip_text((123, 45)), "X 123 · Y 45");
    }

    #[test]
    fn measurement_origin_label_stays_at_the_rectangle_top_left() {
        let narrow = gtk::graphene::Rect::new(30.0, 40.0, 50.0, 60.0);
        let wide = gtk::graphene::Rect::new(30.0, 40.0, 200.0, 60.0);
        let [narrow_origin, narrow_width, _] = imp::measurement_label_anchors(narrow);
        let [wide_origin, wide_width, _] = imp::measurement_label_anchors(wide);

        assert_eq!(narrow_origin, (30.0, 40.0));
        assert_eq!(wide_origin, narrow_origin);
        assert_ne!(wide_width, narrow_width);
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
