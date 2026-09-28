//! Line-art overlay reduction.
//!
//! An application-supplied line-art raster (grayscale, source-aligned, white
//! is no ink) is reduced with bicubic (Catmull-Rom) resampling, sharpened with
//! an unsharp mask of radius 1 and multiplied over the Lanczos foreground fill
//! in 8-bit sRGB, like GIMP's Multiply layer mode. The line art never changes
//! alpha: the result keeps the fill's alpha exactly.
use crate::{
    Cancellation, DEFAULT_MEMORY_LIMIT, Error, GameAssetAa, Result, color, lanczos,
    silhouette::Silhouette,
};
use image::{GrayImage, RgbaImage, imageops::FilterType};
use std::sync::{Arc, Mutex};

/// Source-sized working set: the caller's foreground, its linear and isolated
/// copies (2 × 32 B), premultiplied Lanczos input and its vertical pass
/// (2 × 16 B), silhouette support, flood fill and projections, plus the line
/// art and its bicubic intermediate.
const SOURCE_BYTES: u64 = 160;
/// Target-sized working set: the linear fill, silhouette coverage/opacity,
/// the cached fill and line art, the blur and its horizontal pass, the
/// sharpened line art and the output.
const TARGET_BYTES: u64 = 96;
/// Gaussian standard deviation of the unsharp mask, in target pixels.
const SHARPEN_RADIUS: f32 = 1.;
/// Kernel half-width: the Gaussian is truncated at three standard deviations.
const KERNEL_RADIUS: usize = 3;
/// The fill's silhouette edge softness; this is the traced API's default AA.
const FILL_EDGE: GameAssetAa = GameAssetAa::new(50);

/// Unsharp-mask strength in percent, clamped to 0–100; the default is 40%.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Strength(u8);

impl Strength {
    pub const fn new(percent: u8) -> Self {
        Self(if percent > 100 { 100 } else { percent })
    }

    pub const fn percent(self) -> u8 {
        self.0
    }

    /// The unsharp-mask amount: 0.5 at 0%, 1.5 at 40% and 3.0 at 100%.
    pub fn amount(self) -> f32 {
        0.5 + 2.5 * f32::from(self.0) / 100.
    }
}

impl Default for Strength {
    fn default() -> Self {
        Self(40)
    }
}

/// Bicubic line art at one target size and its Gaussian blur. Both are
/// independent of strength, so a strength change only re-sharpens.
struct ReducedLineArt {
    size: (u32, u32),
    line_art: GrayImage,
    blurred: Vec<f32>,
}

/// The finished straight-alpha sRGB fill of one foreground at one size.
struct ReducedFill {
    size: (u32, u32),
    foreground: Arc<RgbaImage>,
    fill: RgbaImage,
}

#[derive(Default)]
struct Cache {
    line_art: Option<Arc<ReducedLineArt>>,
    fill: Option<Arc<ReducedFill>>,
}

/// Reduce an extracted foreground with source-aligned line art multiplied
/// over it. Each cache holds the latest completed target size; heavy work
/// stays outside the lock and a cancelled request never populates it.
pub struct LineArtSession {
    line_art: Arc<GrayImage>,
    cache: Mutex<Cache>,
}

impl LineArtSession {
    /// Check source-only limits, e.g. before an application generates the
    /// line art for `source`.
    pub fn preflight(source: &RgbaImage) -> Result<()> {
        let (w, h) = source.dimensions();
        if w == 0 || h == 0 {
            return Err(Error::InvalidDimensions);
        }
        check_budget((w, h), (1, 1))
    }

    /// `line_art` must have exactly the dimensions of `source`.
    pub fn new(source: &RgbaImage, line_art: Arc<GrayImage>) -> Result<Self> {
        Self::preflight(source)?;
        if line_art.dimensions() != source.dimensions() {
            return Err(Error::InvalidDimensions);
        }
        Ok(Self {
            line_art,
            cache: Mutex::new(Cache::default()),
        })
    }

    /// The line art as it is multiplied at `w`×`h`: bicubic and sharpened
    /// when reduced, the unchanged source line art at the source size.
    pub fn line_art(
        &self,
        w: u32,
        h: u32,
        strength: Strength,
        cancel: &dyn Cancellation,
    ) -> Result<GrayImage> {
        cancel.check()?;
        self.validate_target(w, h)?;
        if (w, h) == self.line_art.dimensions() {
            return Ok((*self.line_art).clone());
        }
        let reduced = self.reduced_line_art(w, h, cancel)?;
        sharpen(
            &reduced.line_art,
            &reduced.blurred,
            strength.amount(),
            cancel,
        )
    }

    /// Resize a source-aligned extracted foreground and multiply the reduced,
    /// sharpened line art over it. The source size returns `foreground`.
    pub fn resize_with_foreground(
        &self,
        foreground: &Arc<RgbaImage>,
        w: u32,
        h: u32,
        strength: Strength,
        cancel: &dyn Cancellation,
    ) -> Result<RgbaImage> {
        cancel.check()?;
        if foreground.dimensions() != self.line_art.dimensions() {
            return Err(Error::InvalidDimensions);
        }
        self.validate_target(w, h)?;
        if (w, h) == self.line_art.dimensions() {
            return Ok((**foreground).clone());
        }
        let fill = self.reduced_fill(foreground, w, h, cancel)?;
        let reduced = self.reduced_line_art(w, h, cancel)?;
        let line_art = sharpen(
            &reduced.line_art,
            &reduced.blurred,
            strength.amount(),
            cancel,
        )?;
        multiply(&fill.fill, &line_art, cancel)
    }

    fn validate_target(&self, w: u32, h: u32) -> Result<()> {
        let (sw, sh) = self.line_art.dimensions();
        if w == 0 || h == 0 || w > sw || h > sh {
            return Err(Error::InvalidDimensions);
        }
        check_budget((sw, sh), (w, h))
    }

    fn reduced_line_art(
        &self,
        w: u32,
        h: u32,
        cancel: &dyn Cancellation,
    ) -> Result<Arc<ReducedLineArt>> {
        if let Some(cached) = self
            .cache
            .lock()
            .expect("line-art cache poisoned")
            .line_art
            .as_ref()
            .filter(|cached| cached.size == (w, h))
        {
            return Ok(cached.clone());
        }
        let line_art = image::imageops::resize(&*self.line_art, w, h, FilterType::CatmullRom);
        cancel.check()?;
        let blurred = gaussian_blur(&line_art, cancel)?;
        let built = Arc::new(ReducedLineArt {
            size: (w, h),
            line_art,
            blurred,
        });
        let mut cache = self.cache.lock().expect("line-art cache poisoned");
        cancel.check()?;
        cache.line_art = Some(built.clone());
        Ok(built)
    }

    fn reduced_fill(
        &self,
        foreground: &Arc<RgbaImage>,
        w: u32,
        h: u32,
        cancel: &dyn Cancellation,
    ) -> Result<Arc<ReducedFill>> {
        if let Some(cached) = self
            .cache
            .lock()
            .expect("line-art cache poisoned")
            .fill
            .as_ref()
            .filter(|cached| cached.size == (w, h) && Arc::ptr_eq(&cached.foreground, foreground))
        {
            return Ok(cached.clone());
        }
        let linear = color::LinearImage::from_rgba(foreground);
        let silhouette = Silhouette::detect(foreground, cancel)?;
        let fill = foreground_fill(&linear, silhouette.as_ref(), w, h, cancel)?;
        let mut encoded = RgbaImage::new(w, h);
        for (i, (pixel, output)) in fill.pixels.iter().zip(encoded.pixels_mut()).enumerate() {
            if i.is_multiple_of(4096) {
                cancel.check()?;
            }
            *output = color::rgba(*pixel);
        }
        let built = Arc::new(ReducedFill {
            size: (w, h),
            foreground: foreground.clone(),
            fill: encoded,
        });
        let mut cache = self.cache.lock().expect("line-art cache poisoned");
        cancel.check()?;
        cache.fill = Some(built.clone());
        Ok(built)
    }
}

fn check_budget((sw, sh): (u32, u32), (w, h): (u32, u32)) -> Result<()> {
    let estimate = u64::from(sw)
        .saturating_mul(u64::from(sh))
        .saturating_mul(SOURCE_BYTES)
        .saturating_add(
            u64::from(w)
                .saturating_mul(u64::from(h))
                .saturating_mul(TARGET_BYTES),
        );
    if estimate > DEFAULT_MEMORY_LIMIT {
        return Err(Error::GameAssetMemoryLimit {
            limit_bytes: DEFAULT_MEMORY_LIMIT,
        });
    }
    Ok(())
}

/// Lanczos3 of the silhouette-isolated foreground in premultiplied linear
/// light, with alpha from the silhouette's target coverage and the
/// foreground's intrinsic alpha.
fn foreground_fill(
    linear: &color::LinearImage,
    silhouette: Option<&Silhouette>,
    w: u32,
    h: u32,
    cancel: &dyn Cancellation,
) -> Result<color::LinearImage> {
    let isolated = silhouette
        .map(|silhouette| silhouette.isolated(linear, cancel))
        .transpose()?;
    let fill_source = isolated.as_ref().unwrap_or(linear);
    let base = lanczos::resize(fill_source, w as usize, h as usize, cancel)?;
    match silhouette {
        Some(silhouette) => silhouette.target_alpha(base, fill_source, FILL_EDGE, cancel),
        None => Ok(base),
    }
}

fn gaussian_kernel() -> [f32; 2 * KERNEL_RADIUS + 1] {
    let mut kernel = std::array::from_fn(|i| {
        let offset = i as f32 - KERNEL_RADIUS as f32;
        (-offset * offset / (2. * SHARPEN_RADIUS * SHARPEN_RADIUS)).exp()
    });
    let sum: f32 = kernel.iter().sum();
    for weight in &mut kernel {
        *weight /= sum;
    }
    kernel
}

/// Separable Gaussian blur of the stored 8-bit values with clamp-to-edge.
fn gaussian_blur(image: &GrayImage, cancel: &dyn Cancellation) -> Result<Vec<f32>> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let kernel = gaussian_kernel();
    let pixels = image.as_raw();
    let tap = |position: usize, offset: usize, len: usize| {
        (position + offset)
            .saturating_sub(KERNEL_RADIUS)
            .min(len - 1)
    };
    let mut horizontal = vec![0f32; w * h];
    for y in 0..h {
        cancel.check()?;
        let row = &pixels[y * w..(y + 1) * w];
        for x in 0..w {
            horizontal[y * w + x] = kernel
                .iter()
                .enumerate()
                .map(|(offset, weight)| weight * f32::from(row[tap(x, offset, w)]))
                .sum();
        }
    }
    let mut blurred = vec![0f32; w * h];
    for y in 0..h {
        cancel.check()?;
        for x in 0..w {
            blurred[y * w + x] = kernel
                .iter()
                .enumerate()
                .map(|(offset, weight)| weight * horizontal[tap(y, offset, h) * w + x])
                .sum();
        }
    }
    Ok(blurred)
}

/// GEGL-style unsharp mask with threshold 0:
/// `clamp(x + amount · (x − blur(x)), 0, 255)`, rounded once.
fn sharpen(
    line_art: &GrayImage,
    blurred: &[f32],
    amount: f32,
    cancel: &dyn Cancellation,
) -> Result<GrayImage> {
    let mut pixels = Vec::with_capacity(blurred.len());
    for (i, (&value, &blur)) in line_art.as_raw().iter().zip(blurred).enumerate() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        let value = f32::from(value);
        pixels.push((value + amount * (value - blur)).clamp(0., 255.).round() as u8);
    }
    cancel.check()?;
    GrayImage::from_vec(line_art.width(), line_art.height(), pixels)
        .ok_or_else(|| Error::Scaling("Invalid line-art dimensions".into()))
}

/// GIMP Multiply on encoded 8-bit channels: `round(base · line / 255)` for
/// RGB, with the base alpha unchanged.
fn multiply(
    fill: &RgbaImage,
    line_art: &GrayImage,
    cancel: &dyn Cancellation,
) -> Result<RgbaImage> {
    if fill.dimensions() != line_art.dimensions() {
        return Err(Error::InvalidDimensions);
    }
    let mut output = fill.clone();
    for (i, (pixel, line)) in output.pixels_mut().zip(line_art.pixels()).enumerate() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        let line = u16::from(line[0]);
        for channel in &mut pixel.0[..3] {
            *channel = ((u16::from(*channel) * line + 127) / 255) as u8;
        }
    }
    cancel.check()?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CancellationToken;
    use image::{Luma, Rgba};

    /// A red disc-like square on transparency, with one soft alpha row.
    fn foreground() -> Arc<RgbaImage> {
        Arc::new(RgbaImage::from_fn(48, 40, |x, y| {
            if !(8..40).contains(&x) || !(6..34).contains(&y) {
                Rgba([0; 4])
            } else if y == 6 {
                Rgba([200, 90, 60, 128])
            } else {
                Rgba([200, 90, 60 + (x as u8 % 7) * 10, 255])
            }
        }))
    }

    fn flat_line_art(value: u8) -> Arc<GrayImage> {
        Arc::new(GrayImage::from_pixel(48, 40, Luma([value])))
    }

    fn session(line_art: Arc<GrayImage>) -> LineArtSession {
        LineArtSession::new(&foreground(), line_art).unwrap()
    }

    fn expected_fill(foreground: &RgbaImage, w: u32, h: u32) -> RgbaImage {
        let cancel = CancellationToken::default();
        let linear = color::LinearImage::from_rgba(foreground);
        let silhouette = Silhouette::detect(foreground, &cancel).unwrap();
        let fill = foreground_fill(&linear, silhouette.as_ref(), w, h, &cancel).unwrap();
        RgbaImage::from_fn(w, h, |x, y| color::rgba(fill.pixels[(y * w + x) as usize]))
    }

    #[test]
    fn strength_maps_percent_to_unsharp_amount() {
        assert_eq!(Strength::new(0).amount(), 0.5);
        assert_eq!(Strength::new(40).amount(), 1.5);
        assert_eq!(Strength::new(100).amount(), 3.0);
        assert_eq!(Strength::default(), Strength::new(40));
        assert_eq!(Strength::new(250).percent(), 100);
    }

    #[test]
    fn unsharp_mask_overshoots_a_step_edge_by_the_hand_computed_amount() {
        // Gaussian σ = 1 truncated at ±3 has normalized tail sums
        // 0.300478 (±1..3), 0.058438 (±2..3) and 0.004433 (±3). A 64|192
        // step at amount 1.5 moves each side by 1.5 · 128 · tail.
        let step = GrayImage::from_fn(12, 3, |x, _| Luma([if x < 6 { 64 } else { 192 }]));
        let cancel = CancellationToken::default();
        let blurred = gaussian_blur(&step, &cancel).unwrap();
        let sharpened = sharpen(&step, &blurred, 1.5, &cancel).unwrap();
        for y in 0..3 {
            let row: Vec<u8> = (0..12).map(|x| sharpened.get_pixel(x, y)[0]).collect();
            assert_eq!(
                row,
                [64, 64, 64, 63, 53, 6, 250, 203, 193, 192, 192, 192],
                "row {y}"
            );
        }
    }

    #[test]
    fn unsharp_mask_leaves_a_flat_image_unchanged() {
        let cancel = CancellationToken::default();
        for value in [0, 1, 128, 254, 255] {
            let flat = GrayImage::from_pixel(9, 7, Luma([value]));
            let blurred = gaussian_blur(&flat, &cancel).unwrap();
            assert_eq!(sharpen(&flat, &blurred, 3.0, &cancel).unwrap(), flat);
        }
    }

    #[test]
    fn white_line_art_leaves_the_fill_unchanged() {
        let output = session(flat_line_art(255))
            .resize_with_foreground(&foreground(), 24, 20, Strength::new(100), &|| false)
            .unwrap();
        assert_eq!(output, expected_fill(&foreground(), 24, 20));
    }

    #[test]
    fn black_line_art_is_black_with_the_fill_alpha() {
        let fill = expected_fill(&foreground(), 24, 20);
        let output = session(flat_line_art(0))
            .resize_with_foreground(&foreground(), 24, 20, Strength::default(), &|| false)
            .unwrap();
        assert!(fill.pixels().any(|pixel| (1..255).contains(&pixel[3])));
        for (fill, output) in fill.pixels().zip(output.pixels()) {
            assert_eq!(output.0, [0, 0, 0, fill[3]]);
        }
    }

    #[test]
    fn mid_gray_multiplies_each_channel_exactly_and_transparency_stays() {
        let fill = expected_fill(&foreground(), 24, 20);
        let output = session(flat_line_art(128))
            .resize_with_foreground(&foreground(), 24, 20, Strength::new(0), &|| false)
            .unwrap();
        assert!(fill.pixels().any(|pixel| pixel[3] == 0));
        for (fill, output) in fill.pixels().zip(output.pixels()) {
            for channel in 0..3 {
                let expected = (f64::from(fill[channel]) * 128. / 255.).round() as u8;
                assert_eq!(output[channel], expected);
            }
            assert_eq!(output[3], fill[3]);
            if fill[3] == 0 {
                assert_eq!(output.0, [0; 4]);
            }
        }
        assert_eq!(
            multiply(
                &RgbaImage::from_pixel(1, 1, Rgba([200, 255, 1, 77])),
                &GrayImage::from_pixel(1, 1, Luma([128])),
                &|| false,
            )
            .unwrap()
            .get_pixel(0, 0)
            .0,
            [100, 128, 1, 77]
        );
    }

    #[test]
    fn preview_is_raw_at_source_size_and_bicubic_sharpened_when_reduced() {
        let line_art = Arc::new(GrayImage::from_fn(48, 40, |x, y| {
            Luma([if x == 20 || y == 17 { 0 } else { 255 }])
        }));
        let session = session(line_art.clone());
        let cancel = CancellationToken::default();
        assert_eq!(
            session
                .line_art(48, 40, Strength::new(100), &cancel)
                .unwrap(),
            *line_art
        );
        let reduced = image::imageops::resize(&*line_art, 24, 20, FilterType::CatmullRom);
        let blurred = gaussian_blur(&reduced, &cancel).unwrap();
        for strength in [0, 40, 100] {
            let strength = Strength::new(strength);
            assert_eq!(
                session.line_art(24, 20, strength, &cancel).unwrap(),
                sharpen(&reduced, &blurred, strength.amount(), &cancel).unwrap()
            );
        }
    }

    #[test]
    fn identity_returns_the_foreground() {
        let foreground = foreground();
        assert_eq!(
            session(flat_line_art(0))
                .resize_with_foreground(&foreground, 48, 40, Strength::default(), &|| false)
                .unwrap(),
            *foreground
        );
    }

    #[test]
    fn dimensions_are_validated() {
        let source = foreground();
        assert!(matches!(
            LineArtSession::new(&source, Arc::new(GrayImage::new(48, 39))),
            Err(Error::InvalidDimensions)
        ));
        assert!(matches!(
            LineArtSession::new(&RgbaImage::new(0, 0), Arc::new(GrayImage::new(0, 0))),
            Err(Error::InvalidDimensions)
        ));
        let session = session(flat_line_art(255));
        let cancel = CancellationToken::default();
        for (w, h) in [(0, 20), (24, 0), (49, 40), (48, 41)] {
            assert!(matches!(
                session.line_art(w, h, Strength::default(), &cancel),
                Err(Error::InvalidDimensions)
            ));
            assert!(matches!(
                session.resize_with_foreground(&source, w, h, Strength::default(), &cancel),
                Err(Error::InvalidDimensions)
            ));
        }
        assert!(matches!(
            session.resize_with_foreground(
                &Arc::new(RgbaImage::new(24, 20)),
                24,
                20,
                Strength::default(),
                &cancel
            ),
            Err(Error::InvalidDimensions)
        ));
    }

    #[test]
    fn cancellation_at_any_check_never_caches_cancelled_work() {
        let line_art = Arc::new(GrayImage::from_fn(48, 40, |x, _| {
            Luma([if x % 5 == 0 { 20 } else { 250 }])
        }));
        let foreground = foreground();
        let expected = LineArtSession::new(&foreground, line_art.clone())
            .unwrap()
            .resize_with_foreground(&foreground, 24, 20, Strength::default(), &|| false)
            .unwrap();
        let reduced = image::imageops::resize(&*line_art, 24, 20, FilterType::CatmullRom);
        let mut cancelled_runs = 0;
        for allowed_checks in 0.. {
            let session = LineArtSession::new(&foreground, line_art.clone()).unwrap();
            let checks = std::sync::atomic::AtomicUsize::new(0);
            let cancel =
                || checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= allowed_checks;
            match session.resize_with_foreground(&foreground, 24, 20, Strength::default(), &cancel)
            {
                Ok(output) => {
                    assert_eq!(output, expected);
                    break;
                }
                Err(Error::Cancelled) => cancelled_runs += 1,
                Err(error) => panic!("unexpected error: {error}"),
            }
            // Only a stage that completed before cancellation may be cached.
            let cache = session.cache.lock().unwrap();
            if allowed_checks == 0 {
                assert!(cache.fill.is_none() && cache.line_art.is_none());
            }
            if let Some(fill) = &cache.fill {
                assert_eq!(fill.fill, expected_fill(&foreground, 24, 20));
            }
            if let Some(cached) = &cache.line_art {
                assert_eq!(cached.line_art, reduced);
            }
            drop(cache);
            assert_eq!(
                session
                    .resize_with_foreground(&foreground, 24, 20, Strength::default(), &|| false)
                    .unwrap(),
                expected
            );
        }
        assert!(cancelled_runs > 10, "cancellation was checked throughout");
        let session = LineArtSession::new(&foreground, line_art).unwrap();
        let cancelled = CancellationToken::default();
        cancelled.cancel();
        assert!(matches!(
            session.line_art(24, 20, Strength::default(), &cancelled),
            Err(Error::Cancelled)
        ));
        assert!(session.cache.lock().unwrap().line_art.is_none());
    }

    #[test]
    fn strength_change_reuses_the_reduced_fill_and_line_art() {
        let line_art = Arc::new(GrayImage::from_fn(48, 40, |x, y| {
            Luma([if (x + y) % 9 < 2 { 30 } else { 240 }])
        }));
        let session = session(line_art.clone());
        let foreground = foreground();
        let cancel = CancellationToken::default();
        session
            .resize_with_foreground(&foreground, 24, 20, Strength::new(40), &cancel)
            .unwrap();
        let (fill, reduced) = {
            let cache = session.cache.lock().unwrap();
            (cache.fill.clone().unwrap(), cache.line_art.clone().unwrap())
        };
        let strong = session
            .resize_with_foreground(&foreground, 24, 20, Strength::new(100), &cancel)
            .unwrap();
        {
            let cache = session.cache.lock().unwrap();
            assert!(Arc::ptr_eq(cache.fill.as_ref().unwrap(), &fill));
            assert!(Arc::ptr_eq(cache.line_art.as_ref().unwrap(), &reduced));
        }
        let fresh = LineArtSession::new(&foreground, line_art)
            .unwrap()
            .resize_with_foreground(&foreground, 24, 20, Strength::new(100), &cancel)
            .unwrap();
        assert_eq!(strong, fresh);

        // Another foreground or size replaces the single fill entry.
        let other = Arc::new((*foreground).clone());
        session
            .resize_with_foreground(&other, 24, 20, Strength::new(100), &cancel)
            .unwrap();
        assert!(!Arc::ptr_eq(
            session.cache.lock().unwrap().fill.as_ref().unwrap(),
            &fill
        ));
    }
}
