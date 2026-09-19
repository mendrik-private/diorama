//! Game Asset reduction: direction-merged contours, source-width opacity,
//! tight antialiasing, and Lanczos3 source fill with bounded halo tone-down.
use crate::{
    document::{CancellationToken, GameAssetAa},
    error::{AppError, Result},
};
use image::RgbaImage;
use std::sync::{Arc, Mutex};
mod antialias;
#[cfg(test)]
mod benchmarks;
mod cleanup;
mod color;
mod contours;
mod coverage;
mod detect;
mod field;
mod halo;
mod ink;
mod lanczos;
mod opacity;
mod paint;
mod raster;
mod silhouette;
mod smoothing;
mod source;
mod strokes;
#[cfg(test)]
mod tests;
const MEMORY_BUDGET: u64 = 1024 * 1024 * 1024;
// The source phase keeps the established 512 B/pixel allowance for analysis,
// contour storage and source resampling. Target data is not concurrent
// with analysis, but can contain two linear images, contour ownership/colors,
// support/opacity projections, the output and cache; 160 B/pixel conservatively
// accounts for that composition peak.  This is phase-aware without weakening
// the explicit 1 GiB source safety limit.
const SOURCE_PHASE_BYTES: u64 = 512;
const TARGET_PHASE_BYTES: u64 = 160;
struct Prepared {
    models: Vec<detect::Model>,
    widths: Vec<f64>,
    contours: contours::Contours,
    mask: raster::Mask,
    linear: color::LinearImage,
    silhouette: Option<silhouette::Silhouette>,
}
struct TargetContours {
    strokes: strokes::Strokes,
    colors: Vec<[f64; 3]>,
}
impl Prepared {
    fn new(image: &RgbaImage, cancel: &CancellationToken) -> Result<Self> {
        cancel.check()?;
        let samples = detect::detect(image, cancel)?;
        cancel.check()?;
        let models = detect::fit_models(&samples, 0.012, cancel)?;
        let (raw, distance) = source::rasterize(
            &models,
            image.width() as usize,
            image.height() as usize,
            cancel,
        )?;
        let thinned = cleanup::thin(&raw, &distance, cancel)?;
        cancel.check()?;
        let contours = contours::Contours::new(&thinned, &models, cancel)?;
        let mask = ink::ink_mask(image, &samples, &thinned, cancel)?;
        cancel.check()?;
        let widths = opacity::widths(image, &mask, &samples, &contours, cancel)?;
        let linear = color::LinearImage::from_rgba(image);
        let silhouette = silhouette::Silhouette::detect(image, cancel)?;
        Ok(Self {
            models,
            widths,
            contours,
            mask,
            linear,
            silhouette,
        })
    }
    fn target_contours(
        &self,
        image: &RgbaImage,
        w: u32,
        h: u32,
        aa: GameAssetAa,
        cancel: &CancellationToken,
    ) -> Result<TargetContours> {
        let scale = [
            w as f64 / image.width() as f64,
            h as f64 / image.height() as f64,
        ];
        let (retained, owners) =
            self.contours
                .retain_with_ids(&self.models, scale, contours::MAX_SHORT_PIXELS);
        let smoothed = smoothing::smooth(&retained, scale[0].min(scale[1]), cancel)?;
        let curves: Vec<_> = smoothed
            .iter()
            .map(|m| {
                detect::controls(m, 1.)
                    .map(|p| [(p[0] + 0.5) * scale[0] - 0.5, (p[1] + 0.5) * scale[1] - 0.5])
            })
            .collect();
        cancel.check()?;
        let strokes = strokes::render(&curves, &owners, &self.widths, w, h, aa, cancel)?;
        let colors = paint::ink_colors(
            &self.linear,
            &retained,
            &smoothed,
            &owners,
            &strokes,
            scale,
            cancel,
        )?;
        Ok(TargetContours { strokes, colors })
    }
    fn resize(
        &self,
        image: &RgbaImage,
        w: u32,
        h: u32,
        aa: GameAssetAa,
        cancel: &CancellationToken,
    ) -> Result<RgbaImage> {
        let TargetContours { strokes, colors } = self.target_contours(image, w, h, aa, cancel)?;
        let strength = opacity::calculate(&self.widths, &strokes.core, &strokes.owners);
        let paint = opacity::apply(&strokes.coverage, &strokes.owners, &strength);
        let scale = [
            w as f64 / image.width() as f64,
            h as f64 / image.height() as f64,
        ];
        let mut retained_mask = self.contours.retained_ink_mask(&self.mask, scale, cancel)?;
        cancel.check()?;
        let isolated = if let Some(silhouette) = &self.silhouette {
            for (i, (masked, &supported)) in retained_mask
                .data
                .iter_mut()
                .zip(&silhouette.support.data)
                .enumerate()
            {
                if i % 4096 == 0 {
                    cancel.check()?;
                }
                *masked &= supported;
            }
            Some(silhouette.isolated(&self.linear, cancel)?)
        } else {
            None
        };
        let fill_source = isolated.as_ref().unwrap_or(&self.linear);
        let base = lanczos::resize(fill_source, w as usize, h as usize, cancel)?;
        let base = halo::apply(fill_source, &retained_mask, base, &strokes.core, cancel)?;
        let fill = if let Some(silhouette) = &self.silhouette {
            let source_coverage = silhouette.coverage(w as usize, h as usize, cancel)?;
            let retained_coverage =
                silhouette.project_mask(&retained_mask, w as usize, h as usize, cancel)?;
            let coverage = silhouette.target_coverage(
                &source_coverage,
                &retained_coverage,
                &strokes.core,
                aa,
                cancel,
            )?;
            let opacity = silhouette.intrinsic_opacity(
                fill_source,
                &source_coverage,
                w as usize,
                h as usize,
                cancel,
            )?;
            let exterior = silhouette.exterior();
            let mut pixels = Vec::with_capacity(base.pixels.len());
            for (i, ((pixel, support), intrinsic)) in
                base.pixels.iter().zip(coverage).zip(opacity).enumerate()
            {
                if i % 4096 == 0 {
                    cancel.check()?;
                }
                let alpha = (intrinsic * support).clamp(0., 1.);
                let out_alpha = alpha + exterior[3] * (1. - alpha);
                pixels.push(if out_alpha <= 1e-8 {
                    [0.; 4]
                } else {
                    [
                        (pixel[0] * alpha + exterior[0] * exterior[3] * (1. - alpha)) / out_alpha,
                        (pixel[1] * alpha + exterior[1] * exterior[3] * (1. - alpha)) / out_alpha,
                        (pixel[2] * alpha + exterior[2] * exterior[3] * (1. - alpha)) / out_alpha,
                        out_alpha,
                    ]
                });
            }
            color::LinearImage {
                w: w as usize,
                h: h as usize,
                pixels,
            }
        } else {
            base
        };
        let result = paint::composite(&fill, &colors, &paint);
        cancel.check()?;
        Ok(result)
    }
}
#[derive(Default)]
struct Cache {
    prepared: Option<Arc<Prepared>>,
    target: Option<((u32, u32, GameAssetAa), Arc<RgbaImage>)>,
}
/// One source analysis and one target result per preview session. Heavy work
/// stays outside the lock so an obsolete preview can be cancelled promptly.
pub struct Session {
    source: Arc<RgbaImage>,
    cache: Mutex<Cache>,
}
impl Session {
    pub fn new(source: Arc<RgbaImage>) -> Self {
        Self {
            source,
            cache: Mutex::new(Cache::default()),
        }
    }
    pub fn resize(
        &self,
        w: u32,
        h: u32,
        aa: GameAssetAa,
        cancel: &CancellationToken,
    ) -> Result<RgbaImage> {
        cancel.check()?;
        let (sw, sh) = self.source.dimensions();
        if w == 0 || h == 0 || sw == 0 || sh == 0 || w > sw || h > sh {
            return Err(AppError::InvalidDimensions);
        }
        if (w, h) == (sw, sh) {
            return Ok((*self.source).clone());
        }
        let estimate = u64::from(sw) * u64::from(sh) * SOURCE_PHASE_BYTES
            + u64::from(w) * u64::from(h) * TARGET_PHASE_BYTES;
        if estimate > MEMORY_BUDGET {
            return Err(AppError::MemoryLimit {
                limit_bytes: MEMORY_BUDGET,
            });
        }
        let prepared = {
            let cache = self.cache.lock().expect("Game Asset cache poisoned");
            if let Some((key, result)) = &cache.target
                && *key == (w, h, aa)
            {
                return Ok((**result).clone());
            }
            cache.prepared.clone()
        };
        let prepared = match prepared {
            Some(p) => p,
            None => {
                let p = Arc::new(Prepared::new(&self.source, cancel)?);
                cancel.check()?;
                self.cache
                    .lock()
                    .expect("Game Asset cache poisoned")
                    .prepared = Some(p.clone());
                p
            }
        };
        let result = prepared.resize(&self.source, w, h, aa, cancel)?;
        cancel.check()?;
        self.cache.lock().expect("Game Asset cache poisoned").target =
            Some(((w, h, aa), Arc::new(result.clone())));
        Ok(result)
    }
}
