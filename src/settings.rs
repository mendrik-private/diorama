use gio::prelude::*;

use crate::APP_ID;
use crate::canvas::{Background, ZoomFilter};
use crate::document::Resampling;
use crate::navigation::SortOrder;

#[derive(Debug, Clone)]
pub struct Settings {
    inner: Option<gio::Settings>,
}

impl Default for Settings {
    fn default() -> Self {
        let schema =
            gio::SettingsSchemaSource::default().and_then(|source| source.lookup(APP_ID, true));
        Self {
            inner: schema.map(|schema| {
                gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None::<&str>)
            }),
        }
    }
}

impl Settings {
    pub fn zoom_filter(&self) -> ZoomFilter {
        match self.string("zoom-filter").as_deref() {
            Some("hard") => ZoomFilter::Hard,
            _ => ZoomFilter::Soft,
        }
    }

    pub fn set_zoom_filter(&self, filter: ZoomFilter) {
        self.set_string(
            "zoom-filter",
            match filter {
                ZoomFilter::Soft => "soft",
                ZoomFilter::Hard => "hard",
            },
        );
    }

    pub fn background(&self) -> Background {
        match self.string("transparency-background").as_deref() {
            Some("white") => Background::White,
            Some("gray") => Background::Gray,
            Some("black") => Background::Black,
            _ => Background::Gray,
        }
    }

    pub fn set_background(&self, background: Background) {
        self.set_string(
            "transparency-background",
            match background {
                Background::Checkerboard => "checkerboard",
                Background::White => "white",
                Background::Gray => "gray",
                Background::Black => "black",
            },
        );
    }

    pub fn last_zoom(&self) -> f64 {
        self.inner
            .as_ref()
            .map_or(1.0, |settings| settings.double("last-zoom"))
            .clamp(0.01, 64.0)
    }

    pub fn set_last_zoom(&self, zoom: f64) {
        if let Some(settings) = &self.inner
            && let Err(error) = settings.set_double("last-zoom", zoom.clamp(0.01, 64.0))
        {
            tracing::warn!(%error, "Could not save last zoom");
        }
    }

    pub fn last_open_folder(&self) -> Option<gio::File> {
        self.string("last-open-folder-uri")
            .filter(|uri| !uri.is_empty())
            .map(|uri| gio::File::for_uri(&uri))
    }

    pub fn set_last_open_folder(&self, folder: &gio::File) {
        self.set_string("last-open-folder-uri", &folder.uri());
    }

    pub fn scale_resampling(&self) -> Resampling {
        match self.string("scale-resampling").as_deref() {
            Some("nearest") => Resampling::Nearest,
            Some("linear") => Resampling::Linear,
            Some("seam-carving") => Resampling::SeamCarving,
            _ => Resampling::Bicubic,
        }
    }

    pub fn set_scale_resampling(&self, resampling: Resampling) {
        self.set_string(
            "scale-resampling",
            match resampling {
                Resampling::Nearest => "nearest",
                Resampling::Linear => "linear",
                Resampling::Bicubic => "bicubic",
                Resampling::SeamCarving => "seam-carving",
            },
        );
    }

    pub fn window_size(&self) -> (i32, i32) {
        (
            self.integer("window-width").unwrap_or(1000).max(360),
            self.integer("window-height").unwrap_or(700).max(300),
        )
    }

    pub fn set_window_size(&self, width: i32, height: i32) {
        self.set_integer("window-width", width);
        self.set_integer("window-height", height);
    }

    pub fn maximized(&self) -> bool {
        self.boolean("window-maximized").unwrap_or(false)
    }

    pub fn set_maximized(&self, maximized: bool) {
        self.set_boolean("window-maximized", maximized);
    }

    pub fn folder_sort(&self) -> SortOrder {
        match self.string("folder-sort").as_deref() {
            Some("modified") => SortOrder::Modified,
            Some("created") => SortOrder::Created,
            Some("size") => SortOrder::Size,
            Some("type") => SortOrder::FileType,
            Some("accessed") => SortOrder::Accessed,
            _ => SortOrder::Name,
        }
    }

    pub fn compare_lens_size(&self) -> f32 {
        self.integer("compare-lens-size")
            .unwrap_or(280)
            .clamp(64, 512) as f32
    }

    pub fn set_compare_lens_size(&self, size: f32) {
        self.set_integer("compare-lens-size", size.round() as i32);
    }

    pub fn compare_lens_magnification(&self) -> f32 {
        self.inner
            .as_ref()
            .map_or(4.0, |settings| {
                settings.double("compare-lens-magnification")
            })
            .clamp(1.0, 16.0) as f32
    }

    pub fn set_compare_lens_magnification(&self, magnification: f32) {
        if let Some(settings) = &self.inner
            && let Err(error) =
                settings.set_double("compare-lens-magnification", f64::from(magnification))
        {
            tracing::warn!(%error, "Could not save compare lens magnification");
        }
    }

    pub fn preserve_metadata(&self) -> bool {
        self.boolean("preserve-metadata").unwrap_or(true)
    }

    pub fn set_preserve_metadata(&self, preserve: bool) {
        self.set_boolean("preserve-metadata", preserve);
    }

    pub fn png_compression(&self) -> u8 {
        self.integer("png-compression").unwrap_or(6).clamp(0, 9) as u8
    }

    pub fn set_png_compression(&self, compression: u8) {
        self.set_integer("png-compression", i32::from(compression.min(9)));
    }

    pub fn jpeg_quality(&self) -> u8 {
        self.integer("jpeg-quality").unwrap_or(92).clamp(1, 100) as u8
    }

    pub fn set_jpeg_quality(&self, quality: u8) {
        self.set_integer("jpeg-quality", i32::from(quality.clamp(1, 100)));
    }

    pub fn jpeg_background(&self) -> [u8; 3] {
        match self.string("jpeg-background").as_deref() {
            Some("gray") => [128, 128, 128],
            Some("black") => [0, 0, 0],
            _ => [255, 255, 255],
        }
    }

    pub fn set_jpeg_background(&self, background: [u8; 3]) {
        let value = if background == [0, 0, 0] {
            "black"
        } else if background == [128, 128, 128] {
            "gray"
        } else {
            "white"
        };
        self.set_string("jpeg-background", value);
    }

    fn string(&self, key: &str) -> Option<String> {
        self.inner
            .as_ref()
            .map(|settings| settings.string(key).to_string())
    }

    fn integer(&self, key: &str) -> Option<i32> {
        self.inner.as_ref().map(|settings| settings.int(key))
    }

    fn boolean(&self, key: &str) -> Option<bool> {
        self.inner.as_ref().map(|settings| settings.boolean(key))
    }

    fn set_string(&self, key: &str, value: &str) {
        if let Some(settings) = &self.inner
            && let Err(error) = settings.set_string(key, value)
        {
            tracing::warn!(%error, key, "Could not save setting");
        }
    }

    fn set_integer(&self, key: &str, value: i32) {
        if let Some(settings) = &self.inner
            && let Err(error) = settings.set_int(key, value)
        {
            tracing::warn!(%error, key, "Could not save setting");
        }
    }

    fn set_boolean(&self, key: &str, value: bool) {
        if let Some(settings) = &self.inner
            && let Err(error) = settings.set_boolean(key, value)
        {
            tracing::warn!(%error, key, "Could not save setting");
        }
    }
}
