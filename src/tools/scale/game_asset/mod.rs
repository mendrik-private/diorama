//! Game Asset reduction: direction-merged contours, source-width opacity,
//! tight antialiasing, and biharmonic texture repair with area projection.
use crate::{
    document::{CancellationToken, GameAssetAa},
    error::{AppError, Result},
};
use image::RgbaImage;
use std::sync::{Arc, Mutex};
mod antialias;
#[cfg(test)]
mod benchmarks;
mod biharmonic;
mod cleanup;
mod color;
mod contours;
mod coverage;
mod detect;
mod field;
mod ink;
mod opacity;
mod paint;
mod project;
mod raster;
mod smoothing;
mod source;
mod strokes;
#[cfg(test)]
mod tests;
const MEMORY_BUDGET: u64 = 1024 * 1024 * 1024;
struct Prepared {
    models: Vec<detect::Model>,
    widths: Vec<f64>,
    contours: contours::Contours,
    mask: raster::Mask,
    linear: color::LinearImage,
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
        Ok(Self {
            models,
            widths,
            contours,
            mask,
            linear: color::LinearImage::from_rgba(image),
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
        let retained_mask = self.contours.retained_ink_mask(&self.mask, scale, cancel)?;
        cancel.check()?;
        let repaired = biharmonic::repair(&self.linear, &retained_mask, cancel)?;
        let base = project::area(&repaired, w as usize, h as usize, cancel)?;
        let result = paint::composite(&base, &colors, &paint);
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
        let estimate = u64::from(sw) * u64::from(sh) * 512 + u64::from(w) * u64::from(h) * 256;
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
