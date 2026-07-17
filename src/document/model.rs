use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use image::{DynamicImage, RgbaImage};

use super::{History, Operation};
use crate::error::{AppError, Result};
use crate::tools;

type RenderCache = Arc<Mutex<Vec<(usize, Arc<RgbaImage>)>>>;

#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub mime_type: Option<String>,
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<Vec<u8>>,
    pub icc: Option<Vec<u8>>,
    pub key_values: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ImageSource {
    pub pixels: Arc<RgbaImage>,
    pub path: Option<PathBuf>,
    pub metadata: Metadata,
}

#[derive(Debug, Clone)]
pub struct RenderedImage {
    pub pixels: RgbaImage,
    pub metadata: Metadata,
}

#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn check(&self) -> Result<()> {
        if self.0.load(Ordering::Acquire) {
            Err(AppError::Cancelled)
        } else {
            Ok(())
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub struct Document {
    source: ImageSource,
    history: History<Operation>,
    saved_operations: usize,
    cache: RenderCache,
}

impl Clone for Document {
    fn clone(&self) -> Self {
        let cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Self {
            source: self.source.clone(),
            history: self.history.clone(),
            saved_operations: self.saved_operations,
            // Render candidates can diverge from the live operation stack. Sharing a cache
            // keyed only by operation prefix would allow a cancelled candidate to poison it.
            cache: Arc::new(Mutex::new(cache)),
        }
    }
}

impl Document {
    pub fn new(source: ImageSource) -> Self {
        Self {
            source,
            history: History::default(),
            saved_operations: 0,
            cache: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn source(&self) -> &ImageSource {
        &self.source
    }

    pub fn set_metadata(&mut self, metadata: Metadata) {
        self.source.metadata = metadata;
    }

    pub fn operations(&self) -> &[Operation] {
        self.history.active()
    }

    pub fn apply(&mut self, operation: Operation) {
        self.history.push(operation);
        self.truncate_cache(self.operations().len().saturating_sub(1));
    }

    pub fn replace_operation(&mut self, index: usize, operation: Operation) -> bool {
        let replaced = self.history.replace_active(index, operation);
        if replaced {
            self.truncate_cache(index);
        }
        replaced
    }

    pub fn remove_operation(&mut self, index: usize) -> bool {
        let removed = self.history.remove_active(index);
        if removed {
            self.truncate_cache(index);
        }
        removed
    }

    pub fn undo(&mut self) -> bool {
        self.history.undo()
    }

    pub fn redo(&mut self) -> bool {
        self.history.redo()
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    pub fn restore_original(&mut self) {
        self.history.clear();
        self.truncate_cache(0);
    }

    pub fn is_dirty(&self) -> bool {
        self.operations().len() != self.saved_operations
    }

    pub fn mark_saved(&mut self) {
        self.saved_operations = self.operations().len();
    }

    pub fn render(&self, cancellation: &CancellationToken) -> Result<RenderedImage> {
        let (mut pixels, start) = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(prefix, _)| *prefix <= self.operations().len())
            .max_by_key(|(prefix, _)| *prefix)
            .map_or_else(
                || (self.source.pixels.as_ref().clone(), 0),
                |(prefix, image)| (image.as_ref().clone(), *prefix),
            );

        for (index, operation) in self.operations().iter().enumerate().skip(start) {
            cancellation.check()?;
            pixels = apply_operation(pixels, operation, cancellation)?;
            let mut cache = self
                .cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            cache.retain(|(prefix, _)| *prefix != index + 1);
            cache.push((index + 1, Arc::new(pixels.clone())));
            cache.sort_by_key(|(prefix, _)| *prefix);
            while cache.len() > 3 {
                cache.remove(0);
            }
        }

        Ok(RenderedImage {
            pixels,
            metadata: self.source.metadata.clone(),
        })
    }

    fn truncate_cache(&self, prefix: usize) {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(cached_prefix, _)| *cached_prefix <= prefix);
    }
}

fn apply_operation(
    pixels: RgbaImage,
    operation: &Operation,
    cancellation: &CancellationToken,
) -> Result<RgbaImage> {
    let dynamic = DynamicImage::ImageRgba8(pixels);
    let rendered = match operation {
        Operation::Crop {
            x,
            y,
            width,
            height,
        } => {
            let x2 = x.checked_add(*width).ok_or(AppError::InvalidCrop)?;
            let y2 = y.checked_add(*height).ok_or(AppError::InvalidCrop)?;
            if *width == 0 || *height == 0 || x2 > dynamic.width() || y2 > dynamic.height() {
                return Err(AppError::InvalidCrop);
            }
            dynamic.crop_imm(*x, *y, *width, *height)
        }
        Operation::Rotate(rotation) => match rotation {
            super::Rotation::Clockwise90 => dynamic.rotate90(),
            super::Rotation::CounterClockwise90 => dynamic.rotate270(),
            super::Rotation::HalfTurn => dynamic.rotate180(),
        },
        Operation::FlipHorizontal => dynamic.fliph(),
        Operation::FlipVertical => dynamic.flipv(),
        Operation::Scale {
            width,
            height,
            resampling,
        } => {
            return tools::scale::resize(
                &dynamic.into_rgba8(),
                *width,
                *height,
                *resampling,
                cancellation,
            );
        }
        Operation::Palette {
            colors,
            dithering,
            preserve_accents,
            protected,
        } => {
            return tools::palette::reduce_palette(
                &dynamic.into_rgba8(),
                *colors,
                *dithering,
                *preserve_accents,
                protected,
                cancellation,
            );
        }
        Operation::Pencil(stroke) => {
            return tools::pencil::paint_stroke(&dynamic.into_rgba8(), stroke, cancellation);
        }
        Operation::SelectionCutout {
            width,
            height,
            alpha_mask,
            inverted,
        } => {
            let mut rgba = dynamic.into_rgba8();
            if rgba.dimensions() != (*width, *height)
                || u64::from(*width) * u64::from(*height)
                    != u64::try_from(alpha_mask.len()).unwrap_or(u64::MAX)
            {
                return Err(AppError::InvalidDimensions);
            }
            for (index, (pixel, mask)) in rgba.pixels_mut().zip(alpha_mask).enumerate() {
                if index % 4096 == 0 {
                    cancellation.check()?;
                }
                let mask = if *inverted { 255 - mask } else { *mask };
                pixel.0[3] = ((u16::from(pixel.0[3]) * u16::from(mask)) / 255) as u8;
            }
            return Ok(rgba);
        }
    };

    cancellation.check()?;
    Ok(rendered.into_rgba8())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use image::{Rgba, RgbaImage};

    use super::{CancellationToken, Document, ImageSource, Metadata};
    use crate::document::{Operation, Rotation};

    fn document() -> Document {
        let pixels = RgbaImage::from_fn(2, 1, |x, _| {
            if x == 0 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 255, 255])
            }
        });
        Document::new(ImageSource {
            pixels: Arc::new(pixels),
            path: None,
            metadata: Metadata::default(),
        })
    }

    #[test]
    fn operations_are_non_destructive_and_undoable() {
        let mut document = document();
        document.apply(Operation::Rotate(Rotation::Clockwise90));
        let rendered = document.render(&CancellationToken::default()).unwrap();
        assert_eq!(rendered.pixels.dimensions(), (1, 2));
        assert_eq!(document.source().pixels.dimensions(), (2, 1));
        assert!(document.undo());
        assert_eq!(
            document
                .render(&CancellationToken::default())
                .unwrap()
                .pixels
                .dimensions(),
            (2, 1)
        );
    }

    #[test]
    fn cancelled_render_keeps_document_unchanged() {
        let mut document = document();
        document.apply(Operation::FlipHorizontal);
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        assert!(document.render(&cancellation).is_err());
        assert_eq!(document.operations(), &[Operation::FlipHorizontal]);
    }

    #[test]
    fn render_candidates_do_not_share_prefix_caches() {
        let mut document = document();
        let mut cancelled_candidate = document.clone();
        cancelled_candidate.apply(Operation::FlipHorizontal);
        cancelled_candidate
            .render(&CancellationToken::default())
            .expect("candidate should render");

        document.apply(Operation::Rotate(Rotation::Clockwise90));
        let rendered = document
            .render(&CancellationToken::default())
            .expect("live document should not use candidate cache");
        assert_eq!(rendered.pixels.dimensions(), (1, 2));
    }
}
