//! Game Asset reduction: direction-merged contours, source-width opacity,
//! tight antialiasing, and median color with a one-pixel retained-ink halo.
use crate::{
    document::CancellationToken,
    error::{AppError, Result},
};
use image::RgbaImage;
use std::sync::{Arc, Mutex};
mod antialias;
mod cleanup;
mod color;
mod contours;
mod coverage;
mod detect;
mod field;
mod ink;
mod median;
mod opacity;
mod paint;
mod raster;
mod smoothing;
mod source;
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
    fn resize(
        &self,
        image: &RgbaImage,
        w: u32,
        h: u32,
        cancel: &CancellationToken,
    ) -> Result<RgbaImage> {
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
        let (raw, distances) = coverage::render_digital(&curves, w as usize, h as usize, cancel)?;
        let core = cleanup::thin(&raw, &distances, cancel)?;
        let aa = antialias::coverage_map(&core, &distances);
        let ink = paint::ink_colors(
            &self.linear,
            &retained,
            &smoothed,
            &owners,
            &aa,
            scale,
            cancel,
        )?;
        let core_image = image::GrayImage::from_fn(w, h, |x, y| {
            image::Luma([u8::from(core.data[y as usize * w as usize + x as usize]) * 255])
        });
        let strength = opacity::calculate(&self.widths, &core_image, &ink.owners);
        let paint = opacity::apply(&aa, &ink.owners, &strength);
        let retained_mask = self.contours.retained_ink_mask(&self.mask, scale, cancel)?;
        cancel.check()?;
        let base = median::project(&self.linear, &retained_mask, &paint, cancel)?;
        let result = paint::composite(&base, &ink.colors, &paint);
        cancel.check()?;
        Ok(result)
    }
}
#[derive(Default)]
struct Cache {
    prepared: Option<Arc<Prepared>>,
    target: Option<((u32, u32), Arc<RgbaImage>)>,
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
    pub fn resize(&self, w: u32, h: u32, cancel: &CancellationToken) -> Result<RgbaImage> {
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
            if let Some((dimensions, result)) = &cache.target
                && *dimensions == (w, h)
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
        let result = prepared.resize(&self.source, w, h, cancel)?;
        cancel.check()?;
        self.cache.lock().expect("Game Asset cache poisoned").target =
            Some(((w, h), Arc::new(result.clone())));
        Ok(result)
    }
}
