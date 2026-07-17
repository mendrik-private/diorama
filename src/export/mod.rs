mod atomic_write;
mod jpeg;
mod png;

pub use atomic_write::atomic_save;
pub use jpeg::JpegOptions;
pub use png::PngOptions;

use std::path::Path;

use crate::document::{CancellationToken, RenderedImage};
use crate::error::Result;

#[derive(Debug, Clone)]
pub enum ExportOptions {
    Png(PngOptions),
    Jpeg(JpegOptions),
}

pub fn export(
    image: &RenderedImage,
    path: &Path,
    options: &ExportOptions,
    cancellation: &CancellationToken,
) -> Result<()> {
    cancellation.check()?;
    atomic_save(path, |writer| match options {
        ExportOptions::Png(options) => png::encode(writer, image, options),
        ExportOptions::Jpeg(options) => jpeg::encode(writer, image, options),
    })
}
