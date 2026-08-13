use hf_hub::api::sync::Api;
use image::RgbaImage;
use rlx_sam2::{Sam2, Sam2Config};

use crate::error::{AppError, Result};
use crate::tools::crop::CropBounds;

const SAM2_REPOSITORY: &str = "Kijai/sam2-safetensors";
const SAM2_TINY_MODEL: &str = "sam2_hiera_tiny.safetensors";
const SAM2_INPUT_SIZE: f32 = 1024.0;

pub struct SelectedObject {
    pub flash: RgbaImage,
    pub bounds: CropBounds,
    pub image_dimensions: (u32, u32),
}

struct MaskedFlash {
    flash: RgbaImage,
    bounds: CropBounds,
}

pub fn load_sam2() -> Result<Sam2> {
    let path = Api::new()
        .map_err(|error| AppError::AiInference(error.to_string()))?
        .model(SAM2_REPOSITORY.to_owned())
        .get(SAM2_TINY_MODEL)
        .map_err(|error| AppError::AiInference(error.to_string()))?;
    Sam2::from_safetensors(
        path.to_str()
            .ok_or_else(|| AppError::AiInference("SAM 2 model path is not UTF-8".to_owned()))?,
        Sam2Config::hiera_tiny(),
    )
    .map_err(|error| AppError::AiInference(error.to_string()))
}

pub fn select_object_at(
    detector: &mut Sam2,
    image: RgbaImage,
    x: u32,
    y: u32,
) -> Result<Option<SelectedObject>> {
    if x >= image.width() || y >= image.height() {
        return Ok(None);
    }
    let image_dimensions = image.dimensions();
    let rgb = image
        .pixels()
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect::<Vec<_>>();
    let prompt = sam2_prompt_point(x, y, image.width(), image.height());
    let prediction = detector
        .predict_image(
            &rgb,
            image.height() as usize,
            image.width() as usize,
            Some((&prompt, &[1.0])),
            None,
            None,
            true,
        )
        .map_err(|error| AppError::AiInference(error.to_string()))?;
    let Some(mask_index) = highest_quality_mask(&prediction.iou_pred) else {
        return Ok(None);
    };
    let mask = source_mask(
        &prediction.masks,
        mask_index,
        prediction.h_out,
        prediction.w_out,
        image.width(),
        image.height(),
    );
    if !mask.iter().any(|selected| *selected) {
        return Ok(None);
    }
    let flash = masked_flash(&image, &mask)?;
    Ok(Some(SelectedObject {
        flash: flash.flash,
        bounds: flash.bounds,
        image_dimensions,
    }))
}

fn sam2_prompt_point(x: u32, y: u32, width: u32, height: u32) -> [f32; 2] {
    [
        (x as f32 + 0.5) * SAM2_INPUT_SIZE / width as f32,
        (y as f32 + 0.5) * SAM2_INPUT_SIZE / height as f32,
    ]
}

fn highest_quality_mask(quality: &[f32]) -> Option<usize> {
    quality
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| left.total_cmp(right))
        .map(|(index, _)| index)
}

fn source_mask(
    masks: &[f32],
    mask_index: usize,
    mask_height: usize,
    mask_width: usize,
    image_width: u32,
    image_height: u32,
) -> Vec<bool> {
    let mask_area = mask_height * mask_width;
    let offset = mask_index * mask_area;
    (0..image_height)
        .flat_map(|y| {
            (0..image_width).map(move |x| {
                let source_x = (x as usize * mask_width / image_width as usize).min(mask_width - 1);
                let source_y =
                    (y as usize * mask_height / image_height as usize).min(mask_height - 1);
                masks
                    .get(offset + source_y * mask_width + source_x)
                    .is_some_and(|logit| *logit > 0.0)
            })
        })
        .collect()
}

fn masked_flash(image: &RgbaImage, mask: &[bool]) -> Result<MaskedFlash> {
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
                flash.put_pixel(local_x, local_y, image::Rgba([0, 0, 0, 0]));
            }
        }
    }
    Ok(MaskedFlash { flash, bounds })
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
    use image::{Rgba, RgbaImage};

    use super::{highest_quality_mask, masked_flash, sam2_prompt_point, source_mask};

    #[test]
    fn scales_clicks_to_sam2s_input_grid() {
        assert_eq!(sam2_prompt_point(0, 0, 8, 8), [64.0, 64.0]);
        assert_eq!(sam2_prompt_point(7, 7, 8, 8), [960.0, 960.0]);
    }

    #[test]
    fn highest_quality_mask_selects_the_best_prediction() {
        assert_eq!(highest_quality_mask(&[0.72, 0.95, 0.81]), Some(1));
        assert_eq!(highest_quality_mask(&[]), None);
    }

    #[test]
    fn source_mask_scales_sam_logits_to_the_image() {
        let mut logits = vec![-1.0; 2 * 4];
        logits[4 + 3] = 1.0;
        let mask = source_mask(&logits, 1, 2, 2, 4, 4);

        assert!(!mask[0]);
        assert!(mask[3 * 4 + 3]);
        assert!(mask[2 * 4 + 2]);
    }

    #[test]
    fn flash_matches_mask_bounds() {
        let image = RgbaImage::from_fn(3, 2, |x, y| Rgba([x as u8, y as u8, 9, 200]));
        let flash = masked_flash(&image, &[false, false, false, false, true, false]).unwrap();

        assert_eq!(flash.flash.get_pixel(0, 0).0, [53, 132, 228, 150]);
        assert_eq!(
            flash.bounds,
            crate::tools::crop::CropBounds {
                x: 1,
                y: 1,
                width: 1,
                height: 1,
            }
        );
    }

    #[test]
    #[ignore = "downloads and compiles the 156 MB SAM 2 model"]
    fn sam2_model_loads() {
        super::load_sam2().expect("SAM 2 Tiny model loads");
    }

    #[test]
    #[ignore = "runs CPU inference with the 156 MB SAM 2 model"]
    fn sam2_segments_a_click() {
        let mut sam2 = super::load_sam2().expect("SAM 2 Tiny model loads");
        let image = RgbaImage::from_pixel(8, 8, Rgba([20, 40, 60, 255]));

        super::select_object_at(&mut sam2, image, 4, 4)
            .expect("SAM 2 produces a valid result for a point prompt");
    }
}
