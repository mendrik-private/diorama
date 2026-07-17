use std::cell::{Cell, RefCell};

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ZoomFilter {
    Soft,
    #[default]
    Hard,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Background {
    #[default]
    Checkerboard,
    White,
    Gray,
    Black,
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
pub(super) struct Lens {
    texture: gdk::Texture,
    normalized_x: f32,
    normalized_y: f32,
    diameter: f32,
    magnification: f32,
}

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct ImageCanvas {
        pub texture: RefCell<Option<gdk::Texture>>,
        pub zoom: Cell<f64>,
        pub filter: Cell<ZoomFilter>,
        pub background: Cell<Background>,
        pub preview_scale: Cell<f32>,
        pub(super) lens: RefCell<Option<Lens>>,
        pub marker: Cell<Option<(f32, f32)>>,
        pub crop_overlay: RefCell<Option<CropOverlay>>,
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
            self.preview_scale.set(1.0);
            let object = self.obj();
            object.set_focusable(true);
            object.set_overflow(gtk::Overflow::Hidden);
            object.set_tooltip_text(Some("Image canvas"));
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
                (f64::from(dimension) * self.zoom.get()).round() as i32
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
            draw_background(snapshot, bounds, self.background.get());
            if let Some(texture) = self.texture.borrow().as_ref() {
                let image_bounds =
                    scale_bounds(contain_bounds(bounds, texture), self.preview_scale.get());
                let filter = match self.filter.get() {
                    ZoomFilter::Soft => gtk::gsk::ScalingFilter::Linear,
                    ZoomFilter::Hard => gtk::gsk::ScalingFilter::Nearest,
                };
                snapshot.append_scaled_texture(texture, filter, &image_bounds);
            }
            if let Some(lens) = self.lens.borrow().as_ref() {
                draw_lens(snapshot, bounds, lens);
            }
            if let Some((x, y)) = self.marker.get() {
                draw_marker(snapshot, bounds, x, y);
            }
            if let Some(overlay) = self.crop_overlay.borrow().as_ref() {
                draw_crop_overlay(snapshot, bounds, overlay, self.preview_scale.get());
            }
        }
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
        let cross = gdk::RGBA::WHITE;
        snapshot.append_color(
            &cross,
            &gtk::graphene::Rect::new(center_x - 5.0, center_y - 1.0, 10.0, 2.0),
        );
        snapshot.append_color(
            &cross,
            &gtk::graphene::Rect::new(center_x - 1.0, center_y - 5.0, 2.0, 10.0),
        );
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

    fn scale_bounds(bounds: gtk::graphene::Rect, scale: f32) -> gtk::graphene::Rect {
        let scale = scale.clamp(0.01, 64.0);
        let width = bounds.width() * scale;
        let height = bounds.height() * scale;
        gtk::graphene::Rect::new(
            bounds.x() + (bounds.width() - width) / 2.0,
            bounds.y() + (bounds.height() - height) / 2.0,
            width,
            height,
        )
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
        bounds: gtk::graphene::Rect,
        overlay: &CropOverlay,
        preview_scale: f32,
    ) {
        let width = overlay.image_width.max(1) as f32;
        let height = overlay.image_height.max(1) as f32;
        let image_ratio = width / height;
        let bounds_ratio = bounds.width() / bounds.height().max(1.0);
        let image_bounds = if image_ratio > bounds_ratio {
            let fitted_height = bounds.width() / image_ratio;
            gtk::graphene::Rect::new(
                bounds.x(),
                bounds.y() + (bounds.height() - fitted_height) / 2.0,
                bounds.width(),
                fitted_height,
            )
        } else {
            let fitted_width = bounds.height() * image_ratio;
            gtk::graphene::Rect::new(
                bounds.x() + (bounds.width() - fitted_width) / 2.0,
                bounds.y(),
                fitted_width,
                bounds.height(),
            )
        };
        let image_bounds = scale_bounds(image_bounds, preview_scale);
        let rect = gtk::graphene::Rect::new(
            image_bounds.x() + image_bounds.width() * overlay.x as f32 / width,
            image_bounds.y() + image_bounds.height() * overlay.y as f32 / height,
            image_bounds.width() * overlay.width as f32 / width,
            image_bounds.height() * overlay.height as f32 / height,
        );
        draw_dashed_crop_border(snapshot, rect);
    }

    fn draw_dashed_crop_border(snapshot: &gtk::Snapshot, rect: gtk::graphene::Rect) {
        let red = gdk::RGBA::new(0.95, 0.18, 0.18, 1.0);
        let blue = gdk::RGBA::new(0.18, 0.42, 0.96, 1.0);
        const DASH: f32 = 8.0;
        const GAP: f32 = 4.0;
        const THICKNESS: f32 = 2.0;

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
            let mut is_red = true;
            while offset < length {
                let dash = (length - offset).min(DASH);
                let color = if is_red { &red } else { &blue };
                let segment = if horizontal {
                    gtk::graphene::Rect::new(x + offset, y, dash, THICKNESS)
                } else {
                    gtk::graphene::Rect::new(x, y + offset, THICKNESS, dash)
                };
                snapshot.append_color(color, &segment);
                offset += DASH + GAP;
                is_red = !is_red;
            }
        }
    }

    fn draw_background(snapshot: &gtk::Snapshot, bounds: gtk::graphene::Rect, mode: Background) {
        match mode {
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
        self.queue_draw();
    }

    pub fn background(&self) -> Background {
        self.imp().background.get()
    }

    pub fn set_background(&self, background: Background) {
        self.imp().background.set(background);
        self.queue_draw();
    }

    pub fn set_lens(
        &self,
        texture: &gdk::Texture,
        normalized_x: f32,
        normalized_y: f32,
        diameter: f32,
        magnification: f32,
    ) {
        self.imp().lens.replace(Some(Lens {
            texture: texture.clone(),
            normalized_x,
            normalized_y,
            diameter,
            magnification,
        }));
        self.queue_draw();
    }

    pub fn clear_lens(&self) {
        self.imp().lens.replace(None);
        self.queue_draw();
    }

    pub fn set_marker(&self, marker: Option<(f32, f32)>) {
        self.imp().marker.set(marker);
        self.queue_draw();
    }

    pub fn set_crop_overlay(&self, overlay: Option<CropOverlay>) {
        self.imp().crop_overlay.replace(overlay);
        self.queue_draw();
    }

    pub fn set_preview_scale(&self, scale: f32) {
        self.imp().preview_scale.set(scale.clamp(0.01, 64.0));
        self.queue_draw();
    }

    pub fn crop_display_bounds(&self, overlay: CropOverlay) -> Option<gtk::graphene::Rect> {
        let texture = self.texture()?;
        let canvas_width = self.width().max(1) as f32;
        let canvas_height = self.height().max(1) as f32;
        let image_ratio = texture.width() as f32 / texture.height().max(1) as f32;
        let canvas_ratio = canvas_width / canvas_height;
        let image_bounds = if image_ratio > canvas_ratio {
            let height = canvas_width / image_ratio;
            gtk::graphene::Rect::new(0.0, (canvas_height - height) / 2.0, canvas_width, height)
        } else {
            let width = canvas_height * image_ratio;
            gtk::graphene::Rect::new((canvas_width - width) / 2.0, 0.0, width, canvas_height)
        };
        let preview_scale = self.imp().preview_scale.get().clamp(0.01, 64.0);
        let preview_width = image_bounds.width() * preview_scale;
        let preview_height = image_bounds.height() * preview_scale;
        let image_bounds = gtk::graphene::Rect::new(
            image_bounds.x() + (image_bounds.width() - preview_width) / 2.0,
            image_bounds.y() + (image_bounds.height() - preview_height) / 2.0,
            preview_width,
            preview_height,
        );
        Some(gtk::graphene::Rect::new(
            image_bounds.x()
                + image_bounds.width() * overlay.x as f32 / overlay.image_width.max(1) as f32,
            image_bounds.y()
                + image_bounds.height() * overlay.y as f32 / overlay.image_height.max(1) as f32,
            image_bounds.width() * overlay.width as f32 / overlay.image_width.max(1) as f32,
            image_bounds.height() * overlay.height as f32 / overlay.image_height.max(1) as f32,
        ))
    }

    pub fn pixel_at(&self, x: f64, y: f64) -> Option<(u32, u32)> {
        let texture = self.texture()?;
        let width = f64::from(self.width().max(1));
        let height = f64::from(self.height().max(1));
        let image_ratio = f64::from(texture.width()) / f64::from(texture.height().max(1));
        let bounds_ratio = width / height;
        let (left, top, image_width, image_height) = if image_ratio > bounds_ratio {
            let image_height = width / image_ratio;
            (0.0, (height - image_height) / 2.0, width, image_height)
        } else {
            let image_width = height * image_ratio;
            ((width - image_width) / 2.0, 0.0, image_width, height)
        };
        if x < left || y < top || x >= left + image_width || y >= top + image_height {
            return None;
        }
        Some((
            ((x - left) * f64::from(texture.width()) / image_width) as u32,
            ((y - top) * f64::from(texture.height()) / image_height) as u32,
        ))
    }

    pub fn normalized_at(&self, x: f64, y: f64) -> Option<(f32, f32)> {
        let texture = self.texture()?;
        let width = f64::from(self.width().max(1));
        let height = f64::from(self.height().max(1));
        let image_ratio = f64::from(texture.width()) / f64::from(texture.height().max(1));
        let bounds_ratio = width / height;
        let (left, top, image_width, image_height) = if image_ratio > bounds_ratio {
            let image_height = width / image_ratio;
            (0.0, (height - image_height) / 2.0, width, image_height)
        } else {
            let image_width = height * image_ratio;
            ((width - image_width) / 2.0, 0.0, image_width, height)
        };
        if x < left || y < top || x >= left + image_width || y >= top + image_height {
            return None;
        }
        Some((
            (x - left) as f32 / image_width as f32,
            (y - top) as f32 / image_height as f32,
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

    #[test]
    fn hard_zoom_is_the_default_filter() {
        assert_eq!(ZoomFilter::default(), ZoomFilter::Hard);
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
}
