//! Line-art composition at the target size.
//!
//! The application supplies two target-sized layers: grayscale line art
//! (white is no ink) and an opaque fill without ink contours. Neither layer is
//! resampled here. The line art is sharpened with an unsharp mask of radius 1
//! and multiplies the fill in 8-bit sRGB, like GIMP's Multiply layer mode. Alpha comes from the foreground
//! alone: its Lanczos reduction's silhouette alpha at the target size, with a
//! fixed edge softness.
use crate::{
    Cancellation, DEFAULT_MEMORY_LIMIT, Error, GameAssetAa, Result, color, lanczos,
    silhouette::Silhouette,
};
use image::{GrayImage, RgbImage, RgbaImage};
use std::sync::{Arc, Mutex};

/// Source-sized working set of the alpha: the caller's foreground, its linear
/// and isolated copies (2 × 32 B), premultiplied Lanczos input and its
/// vertical pass (2 × 16 B), silhouette support, flood fill and projections.
const SOURCE_BYTES: u64 = 160;
/// Target-sized working set: the line art, its blur and horizontal pass, the
/// sharpened copy, the fill, the linear Lanczos reduction, silhouette
/// coverage/opacity, the cached alpha and the output.
const TARGET_BYTES: u64 = 96;
/// Gaussian standard deviation of the unsharp mask, in target pixels.
const SHARPEN_RADIUS: f32 = 1.;
/// Kernel half-width: the Gaussian is truncated at three standard deviations.
const KERNEL_RADIUS: usize = 3;
/// The silhouette's edge softness; this is the traced API's default AA.
const ALPHA_EDGE: GameAssetAa = GameAssetAa::new(50);

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

/// Target-sized line art and fill, aligned pixel for pixel. The line art's
/// blur is computed once, so a strength change only re-sharpens.
pub struct LineArtLayers {
    line_art: GrayImage,
    fill: RgbImage,
    blurred: Vec<f32>,
}

impl LineArtLayers {
    /// Both layers must have the same, non-zero dimensions.
    pub fn new(line_art: GrayImage, fill: RgbImage, cancel: &dyn Cancellation) -> Result<Self> {
        let (w, h) = line_art.dimensions();
        if w == 0 || h == 0 || fill.dimensions() != (w, h) {
            return Err(Error::InvalidDimensions);
        }
        let blurred = gaussian_blur(&line_art, cancel)?;
        Ok(Self {
            line_art,
            fill,
            blurred,
        })
    }

    pub fn dimensions(&self) -> (u32, u32) {
        self.line_art.dimensions()
    }

    /// The line art as it is multiplied: sharpened at `strength`.
    pub fn line_art(&self, strength: Strength, cancel: &dyn Cancellation) -> Result<GrayImage> {
        cancel.check()?;
        sharpen(&self.line_art, &self.blurred, strength.amount(), cancel)
    }
}

/// The silhouette alpha of one foreground at one target size.
struct TargetAlpha {
    size: (u32, u32),
    alpha: Vec<u8>,
}

/// Composes target-sized layers over an extracted foreground's silhouette
/// alpha. The alpha of the latest target size is cached; heavy work stays
/// outside the lock and a cancelled request never populates it.
pub struct LineArtComposer {
    foreground: Arc<RgbaImage>,
    alpha: Mutex<Option<Arc<TargetAlpha>>>,
}

impl LineArtComposer {
    /// Check source-only limits, e.g. before an application generates the
    /// layers for `source`.
    pub fn preflight(source: &RgbaImage) -> Result<()> {
        let (w, h) = source.dimensions();
        if w == 0 || h == 0 {
            return Err(Error::InvalidDimensions);
        }
        check_budget((w, h), (1, 1))
    }

    /// `foreground` is the extracted, straight-alpha foreground at the source
    /// size.
    pub fn new(foreground: Arc<RgbaImage>) -> Result<Self> {
        Self::preflight(&foreground)?;
        Ok(Self {
            foreground,
            alpha: Mutex::new(None),
        })
    }

    /// `rgb = round(fill · sharpened line art / 255)` with the foreground's
    /// silhouette alpha at the layers' size, in straight alpha. The layers
    /// must be no larger than the foreground.
    pub fn compose(
        &self,
        layers: &LineArtLayers,
        strength: Strength,
        cancel: &dyn Cancellation,
    ) -> Result<RgbaImage> {
        cancel.check()?;
        let (w, h) = layers.dimensions();
        let (sw, sh) = self.foreground.dimensions();
        if w > sw || h > sh {
            return Err(Error::InvalidDimensions);
        }
        check_budget((sw, sh), (w, h))?;
        let alpha = self.alpha(w, h, cancel)?;
        let line_art = layers.line_art(strength, cancel)?;
        multiply(&layers.fill, &line_art, &alpha.alpha, cancel)
    }

    fn alpha(&self, w: u32, h: u32, cancel: &dyn Cancellation) -> Result<Arc<TargetAlpha>> {
        if let Some(cached) = self
            .alpha
            .lock()
            .expect("line-art alpha cache poisoned")
            .as_ref()
            .filter(|cached| cached.size == (w, h))
        {
            return Ok(cached.clone());
        }
        let linear = color::LinearImage::from_rgba(&self.foreground);
        let silhouette = Silhouette::detect(&self.foreground, cancel)?;
        let fill = foreground_fill(&linear, silhouette.as_ref(), w, h, cancel)?;
        let mut alpha = Vec::with_capacity(fill.pixels.len());
        for (i, pixel) in fill.pixels.iter().enumerate() {
            if i.is_multiple_of(4096) {
                cancel.check()?;
            }
            alpha.push(color::rgba(*pixel)[3]);
        }
        let built = Arc::new(TargetAlpha {
            size: (w, h),
            alpha,
        });
        let mut cache = self.alpha.lock().expect("line-art alpha cache poisoned");
        cancel.check()?;
        *cache = Some(built.clone());
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
        Some(silhouette) => silhouette.target_alpha(base, fill_source, ALPHA_EDGE, cancel),
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

/// GIMP Multiply on encoded 8-bit channels, `round(fill · line / 255)` for
/// RGB, with `alpha` attached as straight alpha.
fn multiply(
    fill: &RgbImage,
    line_art: &GrayImage,
    alpha: &[u8],
    cancel: &dyn Cancellation,
) -> Result<RgbaImage> {
    let (w, h) = fill.dimensions();
    if line_art.dimensions() != (w, h) || alpha.len() != w as usize * h as usize {
        return Err(Error::InvalidDimensions);
    }
    let mut output = RgbaImage::new(w, h);
    for (i, (((output, fill), line), &alpha)) in output
        .pixels_mut()
        .zip(fill.pixels())
        .zip(line_art.pixels())
        .zip(alpha)
        .enumerate()
    {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        let line = u16::from(line[0]);
        let product = |channel: u8| ((u16::from(channel) * line + 127) / 255) as u8;
        output.0 = [product(fill[0]), product(fill[1]), product(fill[2]), alpha];
    }
    cancel.check()?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CancellationToken;
    use image::{Luma, Rgb, Rgba};

    /// A red square on transparency with one soft alpha row.
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

    /// A fill whose RGB differs from the foreground's, so the result's RGB
    /// can only come from the fill and its alpha only from the foreground.
    fn fill(w: u32, h: u32) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| {
            Rgb([(x * 9) as u8, (y * 11 + 3) as u8, 255 - (x + y) as u8])
        })
    }

    fn layers(line_art: GrayImage) -> LineArtLayers {
        let (w, h) = line_art.dimensions();
        LineArtLayers::new(line_art, fill(w, h), &|| false).unwrap()
    }

    fn flat(value: u8) -> LineArtLayers {
        layers(GrayImage::from_pixel(24, 20, Luma([value])))
    }

    fn composer() -> LineArtComposer {
        LineArtComposer::new(foreground()).unwrap()
    }

    fn expected_alpha(w: u32, h: u32) -> Vec<u8> {
        let cancel = CancellationToken::default();
        let foreground = foreground();
        let linear = color::LinearImage::from_rgba(&foreground);
        let silhouette = Silhouette::detect(&foreground, &cancel).unwrap();
        let fill = foreground_fill(&linear, silhouette.as_ref(), w, h, &cancel).unwrap();
        fill.pixels
            .iter()
            .map(|pixel| color::rgba(*pixel)[3])
            .collect()
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
        let sharpened = layers(step).line_art(Strength::new(40), &|| false).unwrap();
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
        for value in [0, 1, 128, 254, 255] {
            let flat = GrayImage::from_pixel(9, 7, Luma([value]));
            assert_eq!(
                layers(flat.clone())
                    .line_art(Strength::new(100), &|| false)
                    .unwrap(),
                flat
            );
        }
    }

    #[test]
    fn white_line_art_leaves_the_fill_rgb_with_the_foreground_alpha() {
        let output = composer()
            .compose(&flat(255), Strength::new(100), &|| false)
            .unwrap();
        let alpha = expected_alpha(24, 20);
        assert!(alpha.iter().any(|alpha| (1..255).contains(alpha)));
        assert!(alpha.contains(&0) && alpha.contains(&255));
        for ((output, fill), alpha) in output.pixels().zip(fill(24, 20).pixels()).zip(alpha) {
            assert_eq!(output.0, [fill[0], fill[1], fill[2], alpha]);
        }
    }

    #[test]
    fn black_line_art_is_black_with_the_foreground_alpha() {
        let output = composer()
            .compose(&flat(0), Strength::default(), &|| false)
            .unwrap();
        for (output, alpha) in output.pixels().zip(expected_alpha(24, 20)) {
            assert_eq!(output.0, [0, 0, 0, alpha]);
        }
    }

    #[test]
    fn mid_gray_multiplies_each_channel_exactly() {
        let output = composer()
            .compose(&flat(128), Strength::new(0), &|| false)
            .unwrap();
        for ((output, fill), alpha) in output
            .pixels()
            .zip(fill(24, 20).pixels())
            .zip(expected_alpha(24, 20))
        {
            for channel in 0..3 {
                let expected = (f64::from(fill[channel]) * 128. / 255.).round() as u8;
                assert_eq!(output[channel], expected);
            }
            assert_eq!(output[3], alpha);
        }
        assert_eq!(
            multiply(
                &RgbImage::from_pixel(1, 1, Rgb([200, 255, 1])),
                &GrayImage::from_pixel(1, 1, Luma([128])),
                &[77],
                &CancellationToken::default()
            )
            .unwrap()
            .get_pixel(0, 0)
            .0,
            [100, 128, 1, 77]
        );
    }

    #[test]
    fn the_sharpened_line_art_is_multiplied_without_resampling() {
        let line_art = GrayImage::from_fn(24, 20, |x, y| {
            Luma([if x == 9 || y == 13 { 0 } else { 230 }])
        });
        let layers = layers(line_art);
        let composer = composer();
        for strength in [0, 40, 100].map(Strength::new) {
            let sharpened = layers.line_art(strength, &|| false).unwrap();
            let output = composer.compose(&layers, strength, &|| false).unwrap();
            for ((output, fill), line) in output
                .pixels()
                .zip(fill(24, 20).pixels())
                .zip(sharpened.pixels())
            {
                for channel in 0..3 {
                    let expected =
                        (f64::from(fill[channel]) * f64::from(line[0]) / 255.).round() as u8;
                    assert_eq!(output[channel], expected);
                }
            }
        }
        assert_ne!(
            layers.line_art(Strength::new(0), &|| false).unwrap(),
            layers.line_art(Strength::new(100), &|| false).unwrap()
        );
    }

    #[test]
    fn dimensions_are_validated() {
        let cancel = CancellationToken::default();
        assert!(matches!(
            LineArtLayers::new(GrayImage::new(24, 20), RgbImage::new(24, 19), &cancel),
            Err(Error::InvalidDimensions)
        ));
        assert!(matches!(
            LineArtLayers::new(GrayImage::new(0, 0), RgbImage::new(0, 0), &cancel),
            Err(Error::InvalidDimensions)
        ));
        assert!(matches!(
            LineArtComposer::new(Arc::new(RgbaImage::new(0, 0))),
            Err(Error::InvalidDimensions)
        ));
        let composer = composer();
        for (w, h) in [(49, 40), (48, 41)] {
            assert!(matches!(
                composer.compose(&layers(GrayImage::new(w, h)), Strength::default(), &cancel),
                Err(Error::InvalidDimensions)
            ));
        }
        // The foreground's own size is a valid target.
        composer
            .compose(
                &layers(GrayImage::new(48, 40)),
                Strength::default(),
                &cancel,
            )
            .unwrap();
    }

    #[test]
    fn cancellation_at_any_check_never_caches_cancelled_work() {
        let layers = flat(90);
        let expected = composer()
            .compose(&layers, Strength::default(), &|| false)
            .unwrap();
        let mut cancelled_runs = 0;
        for allowed_checks in 0.. {
            let composer = composer();
            let checks = std::sync::atomic::AtomicUsize::new(0);
            let cancel =
                || checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= allowed_checks;
            match composer.compose(&layers, Strength::default(), &cancel) {
                Ok(output) => {
                    assert_eq!(output, expected);
                    break;
                }
                Err(Error::Cancelled) => cancelled_runs += 1,
                Err(error) => panic!("unexpected error: {error}"),
            }
            // Only an alpha that completed before cancellation may be cached.
            if let Some(alpha) = composer.alpha.lock().unwrap().as_ref() {
                assert_eq!(alpha.alpha, expected_alpha(24, 20));
            }
            assert_eq!(
                composer
                    .compose(&layers, Strength::default(), &|| false)
                    .unwrap(),
                expected
            );
        }
        assert!(cancelled_runs > 10, "cancellation was checked throughout");
    }

    #[test]
    fn strength_change_reuses_the_cached_alpha() {
        let composer = composer();
        let layers = layers(GrayImage::from_fn(24, 20, |x, y| {
            Luma([if (x + y) % 9 < 2 { 30 } else { 240 }])
        }));
        let cancel = CancellationToken::default();
        composer
            .compose(&layers, Strength::new(40), &cancel)
            .unwrap();
        let alpha = composer.alpha.lock().unwrap().clone().unwrap();
        let strong = composer
            .compose(&layers, Strength::new(100), &cancel)
            .unwrap();
        assert!(Arc::ptr_eq(
            composer.alpha.lock().unwrap().as_ref().unwrap(),
            &alpha
        ));
        let fresh = LineArtComposer::new(foreground())
            .unwrap()
            .compose(&layers, Strength::new(100), &cancel)
            .unwrap();
        assert_eq!(strong, fresh);

        // Another size replaces the single alpha entry.
        composer
            .compose(
                &LineArtLayers::new(GrayImage::new(12, 10), fill(12, 10), &cancel).unwrap(),
                Strength::new(100),
                &cancel,
            )
            .unwrap();
        assert_eq!(
            composer.alpha.lock().unwrap().as_ref().unwrap().size,
            (12, 10)
        );
    }
}
