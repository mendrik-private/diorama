use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use image::{DynamicImage, RgbaImage};

use super::{Annotation, AnnotationEdit, AnnotationId, History, Operation, fold_annotations};
use crate::error::{AppError, Result};
use crate::tools;

type RenderCache = Arc<Mutex<Vec<(usize, Arc<RgbaImage>)>>>;

#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub mime_type: Option<String>,
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<Vec<u8>>,
    pub icc: Option<Vec<u8>>,
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
}

#[derive(Debug)]
pub struct Document {
    source: ImageSource,
    history: History<Operation>,
    saved_operations: Arc<[Operation]>,
    cache: RenderCache,
    next_annotation_id: u64,
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
            saved_operations: self.saved_operations.clone(),
            // Render candidates can diverge from the live operation stack. Sharing a cache
            // keyed only by operation prefix would allow a cancelled candidate to poison it.
            cache: Arc::new(Mutex::new(cache)),
            next_annotation_id: self.next_annotation_id,
        }
    }
}

impl Document {
    pub fn new(source: ImageSource) -> Self {
        Self {
            source,
            history: History::default(),
            saved_operations: Vec::new().into(),
            cache: Arc::new(Mutex::new(Vec::new())),
            next_annotation_id: 1,
        }
    }

    pub fn source(&self) -> &ImageSource {
        &self.source
    }

    pub(crate) fn set_path(&mut self, path: Option<PathBuf>) {
        self.source.path = path;
    }

    pub fn operations(&self) -> &[Operation] {
        self.history.active()
    }

    pub fn apply(&mut self, operation: Operation) {
        let previous_len = self.operations().len();
        self.history.push(operation);
        self.truncate_cache(previous_len);
    }

    pub fn allocate_annotation_id(&mut self) -> AnnotationId {
        let id = AnnotationId(self.next_annotation_id);
        self.next_annotation_id = self.next_annotation_id.saturating_add(1);
        id
    }

    #[must_use]
    pub fn annotations(&self) -> Vec<Annotation> {
        fold_annotations(self.source.pixels.dimensions(), self.operations())
    }

    pub fn amend_annotation(&mut self, annotation: Annotation) -> bool {
        let same_annotation = matches!(
            self.operations().last(),
            Some(Operation::Annotate(AnnotationEdit::Set(previous))) if previous.id == annotation.id
        );
        same_annotation
            && self
                .history
                .replace_last(Operation::Annotate(AnnotationEdit::Set(annotation)))
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
        self.operations() != self.saved_operations.as_ref()
    }

    pub(crate) fn mark_saved_at(&mut self, operations: Arc<[Operation]>) {
        self.saved_operations = operations;
    }

    pub(crate) fn adopt_render_cache(&mut self, rendered: &Self) -> bool {
        if self.operations() != rendered.operations() {
            return false;
        }
        self.cache = rendered.cache.clone();
        true
    }

    pub fn render(&self, cancellation: &CancellationToken) -> Result<RenderedImage> {
        self.render_with_exclusion(cancellation, None)
    }

    pub fn render_excluding(
        &self,
        id: AnnotationId,
        cancellation: &CancellationToken,
    ) -> Result<RenderedImage> {
        self.render_with_exclusion(cancellation, Some(id))
    }

    pub fn render_measurement_drag_base(
        &self,
        excluded: Option<AnnotationId>,
        cancellation: &CancellationToken,
    ) -> Result<RenderedImage> {
        self.render_with_options(cancellation, excluded, false)
    }

    fn render_with_exclusion(
        &self,
        cancellation: &CancellationToken,
        excluded: Option<AnnotationId>,
    ) -> Result<RenderedImage> {
        self.render_with_options(cancellation, excluded, true)
    }

    fn render_with_options(
        &self,
        cancellation: &CancellationToken,
        excluded: Option<AnnotationId>,
        include_measurement_markers: bool,
    ) -> Result<RenderedImage> {
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
            if matches!(operation, Operation::Annotate(_)) {
                continue;
            }
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

        let mut annotations = self.annotations();
        if let Some(excluded) = excluded {
            annotations.retain(|annotation| annotation.id != excluded);
        }
        if include_measurement_markers {
            tools::annotation::composite_annotations(&mut pixels, &annotations, cancellation)?;
        } else {
            tools::annotation::composite_annotation_shapes(
                &mut pixels,
                &annotations,
                cancellation,
            )?;
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
        Operation::Annotate(_) => return Ok(dynamic.into_rgba8()),
    };

    cancellation.check()?;
    Ok(rendered.into_rgba8())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use image::{Rgba, RgbaImage};

    use super::{CancellationToken, Document, ImageSource, Metadata};
    use crate::document::{
        Annotation, AnnotationEdit, AnnotationId, Operation, Rect, Rotation, Shape, StrokeStyle,
    };

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

    fn annotation_document() -> Document {
        Document::new(ImageSource {
            pixels: Arc::new(RgbaImage::from_pixel(64, 64, Rgba([255, 255, 255, 255]))),
            path: None,
            metadata: Metadata::default(),
        })
    }

    fn highlight(id: u64, x: f32) -> Annotation {
        Annotation {
            id: AnnotationId(id),
            shape: Shape::Highlight {
                rect: Rect {
                    x,
                    y: 16.0,
                    width: 20.0,
                    height: 20.0,
                },
                seed: id,
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 2.0,
                },
            },
        }
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
    fn dirty_state_compares_history_content_instead_of_length() {
        let mut document = document();
        document.apply(Operation::Rotate(Rotation::Clockwise90));
        document.mark_saved_at(document.operations().into());
        assert!(!document.is_dirty());

        assert!(document.undo());
        document.apply(Operation::FlipHorizontal);

        assert_eq!(document.operations().len(), 1);
        assert!(document.is_dirty());
    }

    #[test]
    fn returning_to_the_saved_history_clears_dirty_state() {
        let mut document = document();
        document.apply(Operation::Rotate(Rotation::Clockwise90));
        document.mark_saved_at(document.operations().into());

        assert!(document.undo());
        assert!(document.is_dirty());
        assert!(document.redo());
        assert!(!document.is_dirty());
    }

    #[test]
    fn marking_an_older_export_state_keeps_newer_edits_dirty() {
        let mut document = document();
        document.apply(Operation::Rotate(Rotation::Clockwise90));
        let exported_operations = document.operations().into();
        document.apply(Operation::FlipHorizontal);

        document.mark_saved_at(exported_operations);

        assert!(document.is_dirty());
        assert!(document.undo());
        assert!(!document.is_dirty());
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

    #[test]
    fn rendered_cache_is_only_adopted_for_the_same_history() {
        let mut document = document();
        document.apply(Operation::FlipHorizontal);
        let rendered_candidate = document.clone();
        rendered_candidate
            .render(&CancellationToken::default())
            .expect("candidate should render");
        assert!(document.adopt_render_cache(&rendered_candidate));

        document.apply(Operation::Rotate(Rotation::Clockwise90));
        assert!(!document.adopt_render_cache(&rendered_candidate));
    }

    #[test]
    fn annotation_entries_do_not_create_raster_cache_prefixes() {
        let mut document = annotation_document();
        document.apply(Operation::Annotate(AnnotationEdit::Create(highlight(
            1, 8.0,
        ))));
        document.render(&CancellationToken::default()).unwrap();
        assert!(document.cache.lock().unwrap().is_empty());

        document.apply(Operation::Rotate(Rotation::Clockwise90));
        document.render(&CancellationToken::default()).unwrap();
        let cache = document.cache.lock().unwrap();
        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].0, 2);
    }

    #[test]
    fn undoing_set_restores_annotation_geometry_and_dirty_state() {
        let mut document = annotation_document();
        let original = highlight(1, 8.0);
        document.apply(Operation::Annotate(AnnotationEdit::Create(
            original.clone(),
        )));
        document.mark_saved_at(document.operations().into());
        let changed = highlight(1, 24.0);
        document.apply(Operation::Annotate(AnnotationEdit::Set(changed.clone())));
        assert_eq!(document.annotations(), vec![changed]);
        assert!(document.is_dirty());
        assert!(document.undo());
        assert_eq!(document.annotations(), vec![original]);
        assert!(!document.is_dirty());
    }

    #[test]
    fn amendment_coalesces_only_the_same_annotation_at_the_history_tip() {
        let mut document = annotation_document();
        document.apply(Operation::Annotate(AnnotationEdit::Create(highlight(
            1, 8.0,
        ))));
        document.apply(Operation::Annotate(AnnotationEdit::Set(highlight(1, 12.0))));
        assert!(document.amend_annotation(highlight(1, 16.0)));
        assert_eq!(document.operations().len(), 2);
        assert!(!document.amend_annotation(highlight(2, 16.0)));
        assert!(document.undo());
        assert!(!document.amend_annotation(highlight(1, 20.0)));
        assert!(document.can_redo());
    }

    #[test]
    fn render_excluding_omits_only_the_requested_annotation() {
        let mut document = annotation_document();
        document.apply(Operation::Annotate(AnnotationEdit::Create(highlight(
            1, 6.0,
        ))));
        document.apply(Operation::Annotate(AnnotationEdit::Create(highlight(
            2, 34.0,
        ))));
        let all = document
            .render(&CancellationToken::default())
            .unwrap()
            .pixels;
        let without_first = document
            .render_excluding(AnnotationId(1), &CancellationToken::default())
            .unwrap()
            .pixels;
        let source = document.source().pixels.as_ref();
        assert_ne!(all, without_first);
        assert_ne!(without_first, *source);
        assert_eq!(without_first.get_pixel(10, 26), source.get_pixel(10, 26));
    }
}
