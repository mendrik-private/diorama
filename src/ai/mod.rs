use std::path::{Path, PathBuf};

use hf_hub::{
    Cache, Repo, RepoType,
    api::sync::{Api, ApiBuilder},
};
use image::RgbaImage;
use rlx_sam2::{Sam2, Sam2Config, Sam2ImageFeatures};

use crate::error::{AppError, Result};
use crate::tools::crop::CropBounds;

const SAM2_REPOSITORY: &str = "Kijai/sam2-safetensors";
const SAM2_TINY_MODEL: &str = "sam2_hiera_tiny.safetensors";
const SAM2_REVISION: &str = "f885607d88bb3f9145efa49c3e3c50a9e5bf13eb";
const SAM2_INPUT_SIZE: f32 = 1024.0;

pub struct SelectedObject {
    pub image: RgbaImage,
    pub flash: RgbaImage,
    pub bounds: CropBounds,
    pub image_dimensions: (u32, u32),
}

struct MaskedObject {
    image: RgbaImage,
    flash: RgbaImage,
    bounds: CropBounds,
}

pub struct ObjectDetector {
    model: Sam2,
    cached_image: Option<CachedImage>,
}

struct CachedImage {
    revision: (u64, u64),
    dimensions: (u32, u32),
    features: Sam2ImageFeatures,
}

#[derive(Clone, Copy)]
struct MaskSampler<'a> {
    mask: &'a [f32],
    mask_width: usize,
    mask_height: usize,
    image_width: u32,
    image_height: u32,
}

impl<'a> MaskSampler<'a> {
    fn new(
        masks: &'a [f32],
        mask_index: usize,
        mask_dimensions: (usize, usize),
        image_dimensions: (u32, u32),
    ) -> Option<Self> {
        let (mask_width, mask_height) = mask_dimensions;
        let (image_width, image_height) = image_dimensions;
        if mask_width == 0 || mask_height == 0 || image_width == 0 || image_height == 0 {
            return None;
        }
        let mask_area = mask_height.checked_mul(mask_width)?;
        let offset = mask_index.checked_mul(mask_area)?;
        let mask = masks.get(offset..offset.checked_add(mask_area)?)?;
        Some(Self {
            mask,
            mask_width,
            mask_height,
            image_width,
            image_height,
        })
    }

    fn logit(self, x: u32, y: u32) -> Option<f32> {
        if x >= self.image_width || y >= self.image_height {
            return None;
        }
        let mask_x = ((x as f32 + 0.5) * self.mask_width as f32 / self.image_width as f32 - 0.5)
            .clamp(0.0, (self.mask_width - 1) as f32);
        let mask_y = ((y as f32 + 0.5) * self.mask_height as f32 / self.image_height as f32 - 0.5)
            .clamp(0.0, (self.mask_height - 1) as f32);
        let left = mask_x.floor() as usize;
        let top = mask_y.floor() as usize;
        let right = (left + 1).min(self.mask_width - 1);
        let bottom = (top + 1).min(self.mask_height - 1);
        let horizontal = mask_x - left as f32;
        let vertical = mask_y - top as f32;
        let top_value = self.mask[top * self.mask_width + left] * (1.0 - horizontal)
            + self.mask[top * self.mask_width + right] * horizontal;
        let bottom_value = self.mask[bottom * self.mask_width + left] * (1.0 - horizontal)
            + self.mask[bottom * self.mask_width + right] * horizontal;
        Some(top_value * (1.0 - vertical) + bottom_value * vertical)
    }
}

pub fn load_sam2() -> Result<ObjectDetector> {
    let path = match cached_sam2_path() {
        Some(path) => path,
        None => sam2_api()?
            .repo(sam2_repository())
            .get(SAM2_TINY_MODEL)
            .map_err(|error| AppError::AiInference(error.to_string()))?,
    };
    let model = Sam2::from_safetensors(
        path.to_str()
            .ok_or_else(|| AppError::AiInference("SAM 2 model path is not UTF-8".to_owned()))?,
        Sam2Config::hiera_tiny(),
    )
    .map_err(|error| AppError::AiInference(error.to_string()))?;
    Ok(ObjectDetector {
        model,
        cached_image: None,
    })
}

pub fn sam2_is_cached() -> bool {
    cached_sam2_path().is_some()
}

fn cached_sam2_path() -> Option<PathBuf> {
    Cache::new(sam2_cache_dir())
        .repo(sam2_repository())
        .get(SAM2_TINY_MODEL)
        .or_else(legacy_sam2_path)
}

fn legacy_sam2_path() -> Option<PathBuf> {
    let path = sam2_snapshot_path_in(Cache::default().path());
    path.is_file().then_some(path)
}

fn sam2_api() -> Result<Api> {
    ApiBuilder::new()
        .with_cache_dir(sam2_cache_dir())
        .with_progress(false)
        .build()
        .map_err(|error| AppError::AiInference(error.to_string()))
}

fn sam2_cache_dir() -> PathBuf {
    sam2_cache_dir_in(&glib::user_data_dir())
}

fn sam2_cache_dir_in(data_dir: &Path) -> PathBuf {
    data_dir.join(crate::APP_ID).join("models")
}

fn sam2_snapshot_path_in(cache_dir: &Path) -> PathBuf {
    cache_dir
        .join(sam2_repository().folder_name())
        .join("snapshots")
        .join(SAM2_REVISION)
        .join(SAM2_TINY_MODEL)
}

fn sam2_repository() -> Repo {
    Repo::with_revision(
        SAM2_REPOSITORY.to_owned(),
        RepoType::Model,
        SAM2_REVISION.to_owned(),
    )
}

pub fn select_object_at(
    detector: &mut Option<ObjectDetector>,
    image: RgbaImage,
    x: u32,
    y: u32,
    revision: (u64, u64),
) -> Result<Option<SelectedObject>> {
    if x >= image.width() || y >= image.height() {
        return Ok(None);
    }
    let image_dimensions = image.dimensions();
    if let Some(mask) = transparent_component_at(&image, x, y) {
        return selected_object_from_mask(&image, &mask, image_dimensions).map(Some);
    }

    if detector.is_none() {
        *detector = Some(load_sam2()?);
    }
    let detector = detector.as_mut().ok_or(AppError::AiModelUnavailable)?;
    let cache_matches = detector
        .cached_image
        .as_ref()
        .is_some_and(|cached| cached.revision == revision && cached.dimensions == image_dimensions);
    if !cache_matches {
        let rgb = image
            .pixels()
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect::<Vec<_>>();
        let features = detector
            .model
            .encode_image(&rgb, image.height() as usize, image.width() as usize)
            .map_err(|error| AppError::AiInference(error.to_string()))?;
        detector.cached_image = Some(CachedImage {
            revision,
            dimensions: image_dimensions,
            features,
        });
    }

    let prompt = sam2_prompt_point(x, y, image.width(), image.height());
    let ObjectDetector {
        model,
        cached_image,
    } = detector;
    let prediction = model
        .predict_image_with_features(
            &cached_image
                .as_ref()
                .ok_or(AppError::AiModelUnavailable)?
                .features,
            Some((&prompt, &[1.0])),
            None,
            None,
            true,
        )
        .map_err(|error| AppError::AiInference(error.to_string()))?;
    let Some(mask_index) = largest_mask_at_point(
        &prediction.masks,
        &prediction.iou_pred,
        (prediction.w_out, prediction.h_out),
        (x, y),
        image.dimensions(),
    ) else {
        return Ok(None);
    };
    let mask = source_mask(
        &prediction.masks,
        mask_index,
        (prediction.w_out, prediction.h_out),
        image.dimensions(),
    );
    if !mask.iter().any(|selected| *selected) {
        return Ok(None);
    }
    selected_object_from_mask(&image, &mask, image_dimensions).map(Some)
}

fn selected_object_from_mask(
    image: &RgbaImage,
    mask: &[bool],
    image_dimensions: (u32, u32),
) -> Result<SelectedObject> {
    let object = masked_object(image, mask)?;
    Ok(SelectedObject {
        image: object.image,
        flash: object.flash,
        bounds: object.bounds,
        image_dimensions,
    })
}

fn transparent_component_at(image: &RgbaImage, x: u32, y: u32) -> Option<Vec<bool>> {
    if image.get_pixel(x, y)[3] == 0 || !image.pixels().any(|pixel| pixel[3] == 0) {
        return None;
    }
    let (width, height) = image.dimensions();
    let mut selected = vec![false; (width * height) as usize];
    selected[(y * width + x) as usize] = true;
    let mut pending = vec![(x, y)];
    while let Some((current_x, current_y)) = pending.pop() {
        for offset_y in -1..=1 {
            for offset_x in -1..=1 {
                if offset_x == 0 && offset_y == 0 {
                    continue;
                }
                let neighbor_x = current_x.checked_add_signed(offset_x);
                let neighbor_y = current_y.checked_add_signed(offset_y);
                if let (Some(neighbor_x), Some(neighbor_y)) = (neighbor_x, neighbor_y)
                    && neighbor_x < width
                    && neighbor_y < height
                {
                    let index = (neighbor_y * width + neighbor_x) as usize;
                    if !selected[index] && image.get_pixel(neighbor_x, neighbor_y)[3] != 0 {
                        selected[index] = true;
                        pending.push((neighbor_x, neighbor_y));
                    }
                }
            }
        }
    }
    Some(selected)
}

fn sam2_prompt_point(x: u32, y: u32, width: u32, height: u32) -> [f32; 2] {
    [
        (x as f32 + 0.5) * SAM2_INPUT_SIZE / width as f32,
        (y as f32 + 0.5) * SAM2_INPUT_SIZE / height as f32,
    ]
}

fn largest_mask_at_point(
    masks: &[f32],
    quality: &[f32],
    mask_dimensions: (usize, usize),
    point: (u32, u32),
    image_dimensions: (u32, u32),
) -> Option<usize> {
    quality
        .iter()
        .enumerate()
        .filter(|(_, score)| score.is_finite())
        .filter_map(|(index, score)| {
            let sampler = MaskSampler::new(masks, index, mask_dimensions, image_dimensions)?;
            (sampler.logit(point.0, point.1)? > 0.0).then(|| {
                let selected_area = sampler.mask.iter().filter(|logit| **logit > 0.0).count();
                (index, selected_area, *score)
            })
        })
        .max_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then_with(|| left.2.total_cmp(&right.2))
        })
        .map(|(index, _, _)| index)
}

fn source_mask(
    masks: &[f32],
    mask_index: usize,
    mask_dimensions: (usize, usize),
    image_dimensions: (u32, u32),
) -> Vec<bool> {
    let Some(sampler) = MaskSampler::new(masks, mask_index, mask_dimensions, image_dimensions)
    else {
        return Vec::new();
    };
    let (image_width, image_height) = image_dimensions;
    (0..image_height)
        .flat_map(|y| {
            (0..image_width).map(move |x| sampler.logit(x, y).is_some_and(|logit| logit > 0.0))
        })
        .collect()
}

fn masked_object(image: &RgbaImage, mask: &[bool]) -> Result<MaskedObject> {
    let (width, height) = image.dimensions();
    let Some((left, top, right, bottom)) = mask_bounds(mask, width, height) else {
        return Err(AppError::InvalidCrop);
    };
    let bounds = CropBounds {
        x: left,
        y: top,
        width: right - left + 1,
        height: bottom - top + 1,
    };
    let mut cutout = crate::tools::selection::crop(image, bounds)?;
    let mut flash = RgbaImage::from_pixel(
        bounds.width,
        bounds.height,
        image::Rgba([53, 132, 228, 150]),
    );
    for local_y in 0..bounds.height {
        for local_x in 0..bounds.width {
            let source_x = bounds.x + local_x;
            let source_y = bounds.y + local_y;
            if !mask[(source_y * width + source_x) as usize] {
                cutout.put_pixel(local_x, local_y, image::Rgba([0, 0, 0, 0]));
                flash.put_pixel(local_x, local_y, image::Rgba([0, 0, 0, 0]));
            }
        }
    }
    Ok(MaskedObject {
        image: cutout,
        flash,
        bounds,
    })
}

fn mask_bounds(mask: &[bool], width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for y in 0..height {
        for x in 0..width {
            if !mask[(y * width + x) as usize] {
                continue;
            }
            bounds = Some(match bounds {
                Some((left, top, right, bottom)) => {
                    (left.min(x), top.min(y), right.max(x), bottom.max(y))
                }
                None => (x, y, x, y),
            });
        }
    }
    bounds
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use image::{Rgba, RgbaImage};

    use super::{
        largest_mask_at_point, masked_object, sam2_cache_dir_in, sam2_prompt_point,
        sam2_repository, sam2_snapshot_path_in, source_mask,
    };

    #[test]
    fn sam2_model_location_and_revision_are_stable_across_app_versions() {
        assert_eq!(
            sam2_cache_dir_in(Path::new("/data")),
            Path::new("/data/io.github.mendrik.Diorama/models")
        );
        assert_eq!(
            sam2_repository().revision(),
            "f885607d88bb3f9145efa49c3e3c50a9e5bf13eb"
        );
        assert_eq!(
            sam2_snapshot_path_in(Path::new("/cache")),
            Path::new(
                "/cache/models--Kijai--sam2-safetensors/snapshots/\
                 f885607d88bb3f9145efa49c3e3c50a9e5bf13eb/sam2_hiera_tiny.safetensors"
            )
        );
    }

    #[test]
    fn scales_clicks_to_sam2s_input_grid() {
        assert_eq!(sam2_prompt_point(0, 0, 8, 8), [64.0, 64.0]);
        assert_eq!(sam2_prompt_point(7, 7, 8, 8), [960.0, 960.0]);
    }

    #[test]
    fn object_mask_is_the_largest_candidate_containing_the_clicked_pixel() {
        let masks = [
            -1.0, -1.0, -1.0, -1.0, // high score, but misses the click
            1.0, 1.0, 1.0, 1.0, // lower score, largest candidate containing click
            -1.0, -1.0, -1.0, 1.0, // higher score, smaller candidate containing click
        ];

        assert_eq!(
            largest_mask_at_point(&masks, &[0.99, 0.70, 0.95], (2, 2), (3, 3), (4, 4)),
            Some(1)
        );
        assert_eq!(
            largest_mask_at_point(&masks, &[0.99, f32::NAN, f32::NAN], (2, 2), (3, 3), (4, 4)),
            None
        );
    }

    #[test]
    fn source_mask_scales_sam_logits_to_the_image() {
        let mut logits = vec![-1.0; 2 * 4];
        logits[4 + 3] = 4.0;
        let mask = source_mask(&logits, 1, (2, 2), (3, 3));

        assert!(!mask[0]);
        assert!(mask[4]);
        assert!(mask[8]);
    }

    #[test]
    fn object_cutout_preserves_selected_rgba_and_clears_other_pixels() {
        let image = RgbaImage::from_fn(3, 2, |x, y| Rgba([x as u8, y as u8, 9, 200]));
        let object = masked_object(&image, &[true, false, false, false, false, true]).unwrap();

        assert_eq!(object.image.get_pixel(0, 0).0, [0, 0, 9, 200]);
        assert_eq!(object.image.get_pixel(1, 0).0, [0, 0, 0, 0]);
        assert_eq!(object.image.get_pixel(2, 1).0, [2, 1, 9, 200]);
        assert_eq!(object.flash.get_pixel(0, 0).0, [53, 132, 228, 150]);
        assert_eq!(object.flash.get_pixel(1, 0).0, [0, 0, 0, 0]);
        assert_eq!(
            object.bounds,
            crate::tools::crop::CropBounds {
                x: 0,
                y: 0,
                width: 3,
                height: 2,
            }
        );
    }

    #[test]
    fn transparent_sprite_uses_clicked_component_without_loading_sam2() {
        let mut image = RgbaImage::from_pixel(5, 4, Rgba([0, 0, 0, 0]));
        image.put_pixel(1, 1, Rgba([10, 20, 30, 255]));
        image.put_pixel(2, 2, Rgba([40, 50, 60, 128]));
        image.put_pixel(4, 0, Rgba([70, 80, 90, 255]));
        let mut detector = None;

        let selected = super::select_object_at(&mut detector, image, 1, 1, (1, 1))
            .expect("transparent component selection succeeds")
            .expect("clicked component is selected");

        assert!(detector.is_none(), "the SAM 2 model must stay unloaded");
        assert_eq!(
            selected.bounds,
            crate::tools::crop::CropBounds {
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            }
        );
        assert_eq!(selected.image.get_pixel(0, 0).0, [10, 20, 30, 255]);
        assert_eq!(selected.image.get_pixel(1, 1).0, [40, 50, 60, 128]);
        assert_eq!(selected.image.get_pixel(1, 0).0, [0, 0, 0, 0]);
    }

    #[test]
    #[ignore = "downloads and compiles the 156 MB SAM 2 model"]
    fn sam2_model_loads() {
        super::load_sam2().expect("SAM 2 Tiny model loads");
    }

    #[test]
    #[ignore = "runs CPU inference with the 156 MB SAM 2 model"]
    fn sam2_segments_a_click() {
        let mut sam2 = Some(super::load_sam2().expect("SAM 2 Tiny model loads"));
        let image = RgbaImage::from_fn(64, 64, |x, y| {
            if (16..48).contains(&x) && (16..48).contains(&y) {
                Rgba([210, 35, 25, 255])
            } else {
                Rgba([235, 235, 235, 255])
            }
        });

        let selected = super::select_object_at(&mut sam2, image, 32, 32, (1, 1))
            .expect("SAM 2 produces a valid result for a point prompt")
            .expect("SAM 2 returns a mask containing the click");
        assert!(
            selected.bounds.x <= 32
                && selected.bounds.y <= 32
                && selected.bounds.x + selected.bounds.width > 32
                && selected.bounds.y + selected.bounds.height > 32
        );
        assert_ne!(
            selected
                .image
                .get_pixel(32 - selected.bounds.x, 32 - selected.bounds.y)[3],
            0
        );
    }
}
