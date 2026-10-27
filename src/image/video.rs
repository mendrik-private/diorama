use std::time::{Duration, Instant};

use gio::prelude::*;
use gtk::gdk;
use gtk::prelude::*;

use crate::error::{AppError, Result};

pub(crate) const VIDEO_MIME_TYPE: &str = "video/webm";
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(16);

pub(crate) fn is_video(file: &gio::File) -> bool {
    file.basename()
        .and_then(|name| {
            name.extension()
                .and_then(std::ffi::OsStr::to_str)
                .map(|extension| extension.eq_ignore_ascii_case("webm"))
        })
        .unwrap_or(false)
}

/// Opens a muted, looping, paused stream through GTK's GStreamer backend and
/// waits until its first frame is available.
pub(crate) async fn open_video(
    file: &gio::File,
    cancellable: &gio::Cancellable,
) -> Result<(gtk::MediaFile, gdk::Texture)> {
    let media = gtk::MediaFile::for_file(file);
    media.set_muted(true);
    media.set_loop(true);
    let started = Instant::now();
    loop {
        if cancellable.is_cancelled() {
            media.clear();
            return Err(AppError::Cancelled);
        }
        if let Some(error) = media.error() {
            media.clear();
            return Err(AppError::CorruptImage(error.to_string()));
        }
        if media.is_prepared() && !media.has_video() {
            media.clear();
            return Err(AppError::UnsupportedFormat);
        }
        if let Some(texture) = current_frame(&media) {
            return Ok((media, texture));
        }
        if started.elapsed() > OPEN_TIMEOUT {
            media.clear();
            return Err(AppError::MissingDecoder(VIDEO_MIME_TYPE.to_owned()));
        }
        glib::timeout_future(POLL_INTERVAL).await;
    }
}

/// Returns the frame the stream currently displays at its intrinsic size.
pub(crate) fn current_frame(media: &gtk::MediaFile) -> Option<gdk::Texture> {
    let width = media.intrinsic_width();
    let height = media.intrinsic_height();
    if width <= 0 || height <= 0 {
        return None;
    }
    let snapshot = gtk::Snapshot::new();
    media.snapshot(&snapshot, f64::from(width), f64::from(height));
    let node = snapshot.to_node()?;
    if let Some(texture) = node
        .downcast_ref::<gtk::gsk::TextureNode>()
        .map(gtk::gsk::TextureNode::texture)
        .filter(|texture| texture.width() == width && texture.height() == height)
    {
        return Some(texture);
    }
    // Rotated or composited frames are flattened in software.
    let renderer = gtk::gsk::CairoRenderer::new();
    renderer
        .realize_for_display(&gdk::Display::default()?)
        .ok()?;
    let texture = renderer.render_texture(
        &node,
        Some(&gtk::graphene::Rect::new(
            0.0,
            0.0,
            width as f32,
            height as f32,
        )),
    );
    renderer.unrealize();
    Some(texture)
}

#[cfg(test)]
mod tests {
    use super::is_video;

    #[test]
    fn recognizes_webm_by_extension_case_insensitively() {
        assert!(is_video(&gio::File::for_path("clip.WebM")));
        assert!(!is_video(&gio::File::for_path("clip.webp")));
        assert!(!is_video(&gio::File::for_path("webm")));
    }
}
