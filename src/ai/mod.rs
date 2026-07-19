use image::RgbaImage;
use object_detector::{DetectedObject, ObjectDetector};

use crate::document::CancellationToken;
use crate::error::{AppError, Result};
use crate::tools::crop::CropBounds;
use crate::tools::selection;

pub struct SelectedObject {
    pub image: RgbaImage,
    pub flash: RgbaImage,
    pub bounds: CropBounds,
    pub image_dimensions: (u32, u32),
    pub tag: String,
    pub score: f32,
}

struct MaskedCutout {
    image: RgbaImage,
    flash: RgbaImage,
    bounds: CropBounds,
}

pub fn select_object_at(
    detector: &ObjectDetector,
    image: RgbaImage,
    x: u32,
    y: u32,
) -> Result<Option<SelectedObject>> {
    if x >= image.width() || y >= image.height() {
        return Ok(None);
    }
    let image_dimensions = image.dimensions();
    let dynamic = image::DynamicImage::ImageRgba8(image);
    let detections = detector
        .predict(&dynamic)
        .confidence_threshold(0.25)
        .call()
        .map_err(|error| AppError::AiInference(error.to_string()))?;
    let Some(detection) = detection_at(&detections, x, y) else {
        return Ok(None);
    };
    let source = dynamic
        .as_rgba8()
        .expect("the detector input was constructed from RGBA pixels");
    let cutout = masked_cutout(source, detection)?;
    Ok(Some(SelectedObject {
        image: cutout.image,
        flash: cutout.flash,
        bounds: cutout.bounds,
        image_dimensions,
        tag: detection.tag.clone(),
        score: detection.score,
    }))
}

fn detection_at(detections: &[DetectedObject], x: u32, y: u32) -> Option<&DetectedObject> {
    detections
        .iter()
        .filter(|detection| detection_contains(detection, x, y))
        .max_by(|left, right| left.score.total_cmp(&right.score))
}

fn detection_contains(detection: &DetectedObject, x: u32, y: u32) -> bool {
    detection.mask.as_ref().map_or_else(
        || {
            let x = x as f32 + 0.5;
            let y = y as f32 + 0.5;
            x >= detection.bbox.x1
                && x < detection.bbox.x2
                && y >= detection.bbox.y1
                && y < detection.bbox.y2
        },
        |mask| x < mask.width && y < mask.height && mask.get(x, y),
    )
}

fn masked_cutout(image: &RgbaImage, detection: &DetectedObject) -> Result<MaskedCutout> {
    let x = detection.bbox.x1.floor().max(0.0) as u32;
    let y = detection.bbox.y1.floor().max(0.0) as u32;
    let right = detection.bbox.x2.ceil().max(0.0) as u32;
    let bottom = detection.bbox.y2.ceil().max(0.0) as u32;
    let bounds = CropBounds {
        x: x.min(image.width()),
        y: y.min(image.height()),
        width: right
            .min(image.width())
            .saturating_sub(x.min(image.width())),
        height: bottom
            .min(image.height())
            .saturating_sub(y.min(image.height())),
    };
    let mut cutout = selection::crop(image, bounds)?;
    let mut flash = RgbaImage::from_pixel(
        bounds.width,
        bounds.height,
        image::Rgba([53, 132, 228, 150]),
    );
    if let Some(mask) = detection.mask.as_ref() {
        for (local_x, local_y, pixel) in cutout.enumerate_pixels_mut() {
            let source_x = bounds.x + local_x;
            let source_y = bounds.y + local_y;
            if source_x >= mask.width || source_y >= mask.height || !mask.get(source_x, source_y) {
                *pixel = image::Rgba([0, 0, 0, 0]);
                flash.put_pixel(local_x, local_y, image::Rgba([0, 0, 0, 0]));
            }
        }
    }
    Ok(MaskedCutout {
        image: cutout,
        flash,
        bounds,
    })
}

#[derive(Debug, Clone, Copy)]
pub enum PromptKind {
    Foreground,
    Background,
}

#[derive(Debug, Clone, Copy)]
pub struct Prompt {
    pub x: f32,
    pub y: f32,
    pub kind: PromptKind,
}

pub trait SegmentationBackend: Send + Sync {
    fn name(&self) -> &str;
    fn is_installed(&self) -> bool;
    fn segment(
        &self,
        image: &RgbaImage,
        prompts: &[Prompt],
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>>;
}

#[derive(Debug, Default)]
pub struct UnavailableBackend;

impl SegmentationBackend for UnavailableBackend {
    fn name(&self) -> &'static str {
        "Not installed"
    }

    fn is_installed(&self) -> bool {
        false
    }

    fn segment(
        &self,
        _image: &RgbaImage,
        _prompts: &[Prompt],
        _cancellation: &CancellationToken,
    ) -> Result<Vec<u8>> {
        Err(AppError::AiModelUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};
    use object_detector::{DetectedObject, ObjectBBox, ObjectMask};

    use super::{detection_at, masked_cutout};

    fn mask(width: u32, height: u32, points: &[(u32, u32)]) -> ObjectMask {
        let mut data = vec![0; (width as usize * height as usize).div_ceil(8)];
        for &(x, y) in points {
            let bit = (y * width + x) as usize;
            data[bit >> 3] |= 1 << (bit & 7);
        }
        ObjectMask {
            width,
            height,
            data,
        }
    }

    fn detection(score: f32, points: &[(u32, u32)]) -> DetectedObject {
        DetectedObject {
            bbox: ObjectBBox {
                x1: 0.0,
                y1: 0.0,
                x2: 3.0,
                y2: 2.0,
            },
            score,
            class_id: 0,
            tag: format!("score-{score}"),
            mask: Some(mask(3, 2, points)),
        }
    }

    #[test]
    fn point_selects_highest_scoring_containing_mask() {
        let detections = [
            detection(0.95, &[(0, 0)]),
            detection(0.40, &[(1, 1)]),
            detection(0.80, &[(1, 1)]),
        ];

        assert_eq!(detection_at(&detections, 1, 1).unwrap().score, 0.80);
        assert!(detection_at(&detections, 2, 1).is_none());
    }

    #[test]
    fn cutout_preserves_masked_rgba_and_clears_other_pixels() {
        let image = RgbaImage::from_fn(3, 2, |x, y| Rgba([x as u8, y as u8, 9, 200]));
        let detection = detection(0.8, &[(1, 1)]);
        let cutout = masked_cutout(&image, &detection).unwrap();

        assert_eq!(cutout.image.dimensions(), (3, 2));
        assert_eq!(cutout.image.get_pixel(1, 1).0, [1, 1, 9, 200]);
        assert_eq!(cutout.image.get_pixel(0, 0).0, [0, 0, 0, 0]);
        assert_eq!(cutout.flash.get_pixel(1, 1).0, [53, 132, 228, 150]);
        assert_eq!(cutout.flash.get_pixel(0, 0).0, [0, 0, 0, 0]);
        assert_eq!(cutout.bounds.width, 3);
    }
}
