//! Application boundary for the shared Game Asset scaler.
use crate::{
    document::{CancellationToken, GameAssetAa, GameAssetOptions},
    error::{AppError, Result},
};
use image::RgbaImage;
use std::sync::{Arc, Mutex};

pub(crate) type BackgroundRemover =
    dyn Fn(&RgbaImage, &CancellationToken) -> Result<RgbaImage> + Send + Sync;

const OPAQUE_ALPHA: u8 = 255;
const HARD_CUTOUT_ALPHA: u8 = 128;
/// The two stable ends of Diorama's AA control for one target size. Keeping
/// this as one unit prevents a cancelled render from exposing a half-built
/// cache entry.
struct AaEndpoints {
    target: (u32, u32),
    contour_opacity: u8,
    zero: RgbaImage,
    full: RgbaImage,
}

fn validate_dimensions((source_width, source_height): (u32, u32), w: u32, h: u32) -> Result<()> {
    if w == 0
        || h == 0
        || source_width == 0
        || source_height == 0
        || w > source_width
        || h > source_height
    {
        return Err(AppError::InvalidDimensions);
    }
    Ok(())
}

/// AA zero means a hard pixel edge.  BiRefNet returns a soft alpha estimate
/// even for opaque artwork, so use the same half-coverage boundary as the
/// shared scaler's silhouette projection after it has painted the contours.
/// Explicit source alpha remains untouched: it can represent intentional
/// translucency rather than model uncertainty.
fn harden_opaque_cutout_at_zero_aa(
    image: &mut RgbaImage,
    source_is_opaque: bool,
    aa: GameAssetAa,
    cancel: &CancellationToken,
) -> Result<()> {
    if !source_is_opaque || aa.percent() != 0 {
        return Ok(());
    }
    for (i, pixel) in image.pixels_mut().enumerate() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        if pixel[3] < HARD_CUTOUT_ALPHA {
            pixel.0 = [0; 4];
        } else {
            pixel[3] = OPAQUE_ALPHA;
        }
    }
    cancel.check()
}

/// Convert the scaler's binary grayscale contour representation into the
/// opaque black-on-white image used by the inspection preview.
fn contour_mask_to_rgba(mask: &image::GrayImage, cancel: &CancellationToken) -> Result<RgbaImage> {
    let mut image = RgbaImage::new(mask.width(), mask.height());
    for (i, (source, output)) in mask.pixels().zip(image.pixels_mut()).enumerate() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        let value = if source[0] == 0 { 0 } else { OPAQUE_ALPHA };
        output.0 = [value, value, value, OPAQUE_ALPHA];
    }
    cancel.check()?;
    Ok(image)
}

fn srgb_to_linear(encoded: u8) -> f64 {
    let encoded = f64::from(encoded) / 255.;
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(linear: f64) -> u8 {
    let encoded = if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1. / 2.4) - 0.055
    };
    (encoded.clamp(0., 1.) * 255.).round() as u8
}

/// Blend the two rendered AA endpoints in premultiplied linear light. The
/// caller returns the endpoints directly, so their encoded RGB and alpha stay
/// byte-exact at 0% and 100%.
pub(crate) fn blend_aa_endpoints(
    zero: &RgbaImage,
    full: &RgbaImage,
    percent: u8,
    cancel: &CancellationToken,
) -> Result<RgbaImage> {
    debug_assert!(percent > 0 && percent < 100);
    debug_assert_eq!(zero.dimensions(), full.dimensions());
    let t = f64::from(percent) / 100.;
    let mut blended = RgbaImage::new(zero.width(), zero.height());
    for (i, ((zero_pixel, full_pixel), output)) in zero
        .pixels()
        .zip(full.pixels())
        .zip(blended.pixels_mut())
        .enumerate()
    {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        let zero_alpha = f64::from(zero_pixel[3]) / 255.;
        let full_alpha = f64::from(full_pixel[3]) / 255.;
        let alpha = zero_alpha + (full_alpha - zero_alpha) * t;
        let alpha_byte = (alpha * 255.).round() as u8;
        if alpha_byte == 0 {
            output.0 = [0; 4];
            continue;
        }
        for channel in 0..3 {
            let zero_premultiplied = srgb_to_linear(zero_pixel[channel]) * zero_alpha;
            let full_premultiplied = srgb_to_linear(full_pixel[channel]) * full_alpha;
            output[channel] = linear_to_srgb(
                (zero_premultiplied + (full_premultiplied - zero_premultiplied) * t) / alpha,
            );
        }
        output[3] = alpha_byte;
    }
    cancel.check()?;
    Ok(blended)
}

/// A preview session keeps the untouched source for contour analysis and the
/// background-removed foreground for Lanczos fill.  The foreground cache is
/// populated only after a complete, non-cancelled model result.
pub struct Session {
    source: Arc<RgbaImage>,
    source_is_opaque: Mutex<Option<bool>>,
    scaler: asset_scaler::Session,
    foreground: Mutex<Option<Arc<RgbaImage>>>,
    /// One completed pair for the active preview size: two rendered endpoint
    /// images. A completed new size replaces it atomically.
    aa_endpoints: Mutex<Option<Arc<AaEndpoints>>>,
    remove_background: Arc<BackgroundRemover>,
}

impl Session {
    pub fn new(source: Arc<RgbaImage>) -> Self {
        Self::with_background_remover(source, Arc::new(crate::tools::selection::birefnet_cutout))
    }

    fn with_background_remover(
        source: Arc<RgbaImage>,
        remove_background: Arc<BackgroundRemover>,
    ) -> Self {
        Self {
            scaler: asset_scaler::Session::new(source.clone()),
            source,
            source_is_opaque: Mutex::new(None),
            foreground: Mutex::new(None),
            aa_endpoints: Mutex::new(None),
            remove_background,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_background_remover(
        source: Arc<RgbaImage>,
        remove_background: Arc<BackgroundRemover>,
    ) -> Self {
        Self::with_background_remover(source, remove_background)
    }

    fn validate_dimensions(&self, w: u32, h: u32) -> Result<()> {
        validate_dimensions(self.source.dimensions(), w, h)
    }

    fn source_is_opaque(&self, cancel: &CancellationToken) -> Result<bool> {
        if let Some(source_is_opaque) = *self
            .source_is_opaque
            .lock()
            .expect("source-alpha cache poisoned")
        {
            return Ok(source_is_opaque);
        }

        let mut source_is_opaque = true;
        for (i, pixel) in self.source.pixels().enumerate() {
            if i.is_multiple_of(4096) {
                cancel.check()?;
            }
            if pixel[3] != OPAQUE_ALPHA {
                source_is_opaque = false;
                break;
            }
        }
        cancel.check()?;
        let mut cache = self
            .source_is_opaque
            .lock()
            .expect("source-alpha cache poisoned");
        Ok(*cache.get_or_insert(source_is_opaque))
    }

    fn foreground(&self, cancel: &CancellationToken) -> Result<Arc<RgbaImage>> {
        if let Some(foreground) = self
            .foreground
            .lock()
            .expect("foreground cache poisoned")
            .clone()
        {
            return Ok(foreground);
        }

        let foreground = Arc::new((self.remove_background)(&self.source, cancel)?);
        if foreground.dimensions() != self.source.dimensions() {
            return Err(AppError::InvalidDimensions);
        }
        cancel.check()?;
        let mut cache = self.foreground.lock().expect("foreground cache poisoned");
        Ok(cache.get_or_insert(foreground).clone())
    }

    fn aa_endpoints(
        &self,
        target: (u32, u32),
        options: GameAssetOptions,
        cancel: &CancellationToken,
    ) -> Result<Arc<AaEndpoints>> {
        if let Some(endpoints) = self
            .aa_endpoints
            .lock()
            .expect("AA endpoint cache poisoned")
            .as_ref()
            .filter(|endpoints| {
                endpoints.target == target && endpoints.contour_opacity == options.contour_opacity()
            })
            .cloned()
        {
            cancel.check()?;
            return Ok(endpoints);
        }

        // Trace the untouched original before BiRefNet changes alpha and edge
        // colours. Neither the scaler nor foreground cache lock is held while
        // building either endpoint.
        self.scaler
            .prepare(&|| cancel.check().is_err())
            .map_err(map_error)?;
        let foreground = self.foreground(cancel)?;
        let source_is_opaque = self.source_is_opaque(cancel)?;
        let mut zero = self
            .scaler
            .resize_with_foreground_opacity(
                &foreground,
                target.0,
                target.1,
                GameAssetAa::new(0),
                options.contour_opacity_fraction(),
                &|| cancel.check().is_err(),
            )
            .map_err(map_error)?;
        harden_opaque_cutout_at_zero_aa(&mut zero, source_is_opaque, GameAssetAa::new(0), cancel)?;
        let full = self
            .scaler
            .resize_with_foreground_opacity(
                &foreground,
                target.0,
                target.1,
                GameAssetAa::new(100),
                options.contour_opacity_fraction(),
                &|| cancel.check().is_err(),
            )
            .map_err(map_error)?;
        cancel.check()?;
        let built = Arc::new(AaEndpoints {
            target,
            contour_opacity: options.contour_opacity(),
            zero,
            full,
        });

        let mut cache = self
            .aa_endpoints
            .lock()
            .expect("AA endpoint cache poisoned");
        cancel.check()?;
        if let Some(endpoints) = cache
            .as_ref()
            .filter(|endpoints| {
                endpoints.target == target && endpoints.contour_opacity == options.contour_opacity()
            })
            .cloned()
        {
            cancel.check()?;
            return Ok(endpoints);
        }
        *cache = Some(built.clone());
        Ok(built)
    }

    /// Return a binary contour inspection image for the requested preview
    /// dimensions. Original-size inspection renders the fitted source traces
    /// only; downscaled inspection uses the cached foreground support path.
    pub fn contours(&self, w: u32, h: u32, cancel: &CancellationToken) -> Result<RgbaImage> {
        cancel.check()?;
        self.validate_dimensions(w, h)?;
        let mask = if (w, h) == self.source.dimensions() {
            self.scaler
                .polished_contour_mask(w, h, &|| cancel.check().is_err())
                .map_err(map_error)?
        } else {
            let foreground = self.foreground(cancel)?;
            self.scaler
                .foreground_contour_mask(&foreground, w, h, GameAssetAa::new(0), &|| {
                    cancel.check().is_err()
                })
                .map_err(map_error)?
        };
        contour_mask_to_rgba(&mask, cancel)
    }

    pub fn resize(
        &self,
        w: u32,
        h: u32,
        options: GameAssetOptions,
        cancel: &CancellationToken,
    ) -> Result<RgbaImage> {
        cancel.check()?;
        self.validate_dimensions(w, h)?;
        if (w, h) == self.source.dimensions() {
            return Ok((*self.source).clone());
        }
        let endpoints = self.aa_endpoints((w, h), options, cancel)?;
        let resized = match options.aa().percent() {
            0 => endpoints.zero.clone(),
            100 => endpoints.full.clone(),
            percent => blend_aa_endpoints(&endpoints.zero, &endpoints.full, percent, cancel)?,
        };
        cancel.check()?;
        Ok(resized)
    }
}

pub fn resize(
    image: &RgbaImage,
    w: u32,
    h: u32,
    options: GameAssetOptions,
    cancel: &CancellationToken,
) -> Result<RgbaImage> {
    resize_with_background_remover(
        image,
        w,
        h,
        options,
        cancel,
        Arc::new(crate::tools::selection::birefnet_cutout),
    )
}

fn resize_with_background_remover(
    image: &RgbaImage,
    w: u32,
    h: u32,
    options: GameAssetOptions,
    cancel: &CancellationToken,
    remove_background: Arc<BackgroundRemover>,
) -> Result<RgbaImage> {
    cancel.check()?;
    validate_dimensions(image.dimensions(), w, h)?;
    if image.dimensions() == (w, h) {
        return Ok(image.clone());
    }
    Session::with_background_remover(Arc::new(image.clone()), remove_background)
        .resize(w, h, options, cancel)
}

fn map_error(error: asset_scaler::Error) -> AppError {
    match error {
        asset_scaler::Error::Cancelled => AppError::Cancelled,
        asset_scaler::Error::InvalidDimensions => AppError::InvalidDimensions,
        asset_scaler::Error::GameAssetMemoryLimit { limit_bytes } => {
            AppError::GameAssetMemoryLimit { limit_bytes }
        }
        asset_scaler::Error::Scaling(message) => AppError::Scaling(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn options(aa: u8) -> GameAssetOptions {
        GameAssetOptions::new(GameAssetAa::new(aa), 100)
    }

    fn remove_flat_background(image: &RgbaImage, cancel: &CancellationToken) -> Result<RgbaImage> {
        cancel.check()?;
        let mut foreground = image.clone();
        for pixel in foreground.pixels_mut() {
            if pixel.0 == [240, 230, 220, 255] {
                pixel.0 = [0; 4];
            }
        }
        Ok(foreground)
    }

    fn remove_flat_background_with_soft_subject_edge(
        image: &RgbaImage,
        cancel: &CancellationToken,
    ) -> Result<RgbaImage> {
        cancel.check()?;
        let mut foreground = image.clone();
        for (x, y, pixel) in foreground.enumerate_pixels_mut() {
            if !(16..48).contains(&x) || !(16..48).contains(&y) {
                pixel.0 = [0; 4];
            } else {
                pixel.0 = [
                    220,
                    70,
                    50,
                    if x == 16 || x == 47 || y == 16 || y == 47 {
                        192
                    } else {
                        255
                    },
                ];
            }
        }
        Ok(foreground)
    }

    fn counting_remover(calls: Arc<AtomicUsize>) -> Arc<BackgroundRemover> {
        Arc::new(move |image, cancel| {
            calls.fetch_add(1, Ordering::Relaxed);
            remove_flat_background(image, cancel)
        })
    }

    fn opaque_soft_edge_source() -> Arc<RgbaImage> {
        Arc::new(RgbaImage::from_fn(64, 64, |x, y| {
            if (16..48).contains(&x) && (16..48).contains(&y) {
                image::Rgba([180, 60, 40, 255])
            } else {
                image::Rgba([240, 230, 220, 255])
            }
        }))
    }

    fn contour_fixture() -> Arc<RgbaImage> {
        Arc::new(RgbaImage::from_fn(64, 64, |x, y| {
            if !(16..48).contains(&x) || !(16..48).contains(&y) {
                image::Rgba([240, 230, 220, 255])
            } else if x == 16 || x == 47 || y == 16 || y == 47 || (30..34).contains(&y) {
                image::Rgba([12, 10, 8, 255])
            } else {
                image::Rgba([180, 60, 40, 255])
            }
        }))
    }

    fn assert_binary_contours(image: &RgbaImage, dimensions: (u32, u32)) {
        assert_eq!(image.dimensions(), dimensions);
        assert!(
            image
                .pixels()
                .all(|pixel| { pixel.0 == [0, 0, 0, 255] || pixel.0 == [255, 255, 255, 255] })
        );
        assert!(
            image.pixels().any(|pixel| pixel.0 == [0, 0, 0, 255]),
            "contour preview must render detected contours as black lines"
        );
        assert!(
            image.pixels().any(|pixel| pixel.0 == [255, 255, 255, 255]),
            "contour preview must retain a white background"
        );
    }

    #[test]
    fn premultiplied_blend_clears_hidden_rgb_when_quantized_alpha_is_zero() {
        let zero = RgbaImage::from_pixel(1, 1, image::Rgba([255, 255, 255, 0]));
        let full = RgbaImage::from_pixel(1, 1, image::Rgba([20, 40, 60, 1]));
        let blended = blend_aa_endpoints(&zero, &full, 1, &CancellationToken::default()).unwrap();
        assert_eq!(blended.get_pixel(0, 0).0, [0; 4]);

        let visible = RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]));
        let hidden_white = RgbaImage::from_pixel(1, 1, image::Rgba([255, 255, 255, 0]));
        let blended =
            blend_aa_endpoints(&visible, &hidden_white, 50, &CancellationToken::default()).unwrap();
        assert_eq!(blended.get_pixel(0, 0).0, [255, 0, 0, 128]);
    }

    #[test]
    fn identity_returns_the_original_without_background_or_endpoint_work() {
        let source = Arc::new(RgbaImage::from_fn(16, 12, |x, y| {
            image::Rgba([
                x as u8,
                y as u8,
                180,
                if (x + y) % 3 == 0 { 90 } else { 255 },
            ])
        }));
        let calls = Arc::new(AtomicUsize::new(0));
        let session =
            Session::with_background_remover(source.clone(), counting_remover(calls.clone()));
        let output = session
            .resize(16, 12, options(50), &CancellationToken::default())
            .unwrap();
        assert_eq!(output, *source);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(
            session
                .aa_endpoints
                .lock()
                .expect("AA endpoint cache poisoned")
                .is_none()
        );
    }

    #[test]
    fn contour_inspection_uses_polished_source_mask_at_original_size_and_cached_foreground_when_reduced()
     {
        let source = contour_fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        let session = Session::with_background_remover(source, counting_remover(calls.clone()));
        let cancel = CancellationToken::default();

        let original = session.contours(64, 64, &cancel).unwrap();
        assert_binary_contours(&original, (64, 64));
        let polished_source_mask = session
            .scaler
            .polished_contour_mask(64, 64, &|| cancel.check().is_err())
            .unwrap();
        assert_eq!(
            original,
            contour_mask_to_rgba(&polished_source_mask, &cancel).unwrap()
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(
            session
                .aa_endpoints
                .lock()
                .expect("AA endpoint cache poisoned")
                .is_none(),
            "contour inspection must not render resize endpoints"
        );

        let target = session.contours(32, 32, &cancel).unwrap();
        assert_binary_contours(&target, (32, 32));
        let foreground = session.foreground(&cancel).unwrap();
        let target_mask = session
            .scaler
            .foreground_contour_mask(&foreground, 32, 32, GameAssetAa::new(0), &|| {
                cancel.check().is_err()
            })
            .unwrap();
        assert_eq!(target, contour_mask_to_rgba(&target_mask, &cancel).unwrap());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(session.contours(32, 32, &cancel).unwrap(), target);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "foreground must be cached"
        );
        assert!(
            session
                .aa_endpoints
                .lock()
                .expect("AA endpoint cache poisoned")
                .is_none(),
            "contour inspection must not populate the resize endpoint cache"
        );
    }

    #[test]
    fn contour_inspection_validates_dimensions_and_preserves_foreground_cancellation_semantics() {
        let source = contour_fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        let remover = counting_remover(calls.clone());
        let session = Session::with_background_remover(source.clone(), remover);
        let cancel = CancellationToken::default();
        assert!(matches!(
            session.contours(0, 32, &cancel),
            Err(AppError::InvalidDimensions)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        cancel.cancel();
        assert!(matches!(
            session.contours(32, 32, &cancel),
            Err(AppError::Cancelled)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 0);

        let calls = Arc::new(AtomicUsize::new(0));
        let remover: Arc<BackgroundRemover> = {
            let calls = calls.clone();
            Arc::new(move |image, cancel| {
                if calls.fetch_add(1, Ordering::Relaxed) == 0 {
                    cancel.cancel();
                }
                Ok(image.clone())
            })
        };
        let session = Session::with_background_remover(source, remover);
        let cancelled = CancellationToken::default();
        assert!(matches!(
            session.contours(32, 32, &cancelled),
            Err(AppError::Cancelled)
        ));
        assert!(
            session
                .foreground
                .lock()
                .expect("foreground cache poisoned")
                .is_none(),
            "a cancelled contour foreground must not be cached"
        );
        let retry = CancellationToken::default();
        assert_binary_contours(&session.contours(32, 32, &retry).unwrap(), (32, 32));
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn aa_percent_interpolates_soft_cutout_without_a_zero_to_one_pop() {
        let source = opaque_soft_edge_source();
        let cancel = CancellationToken::default();
        let remover = Arc::new(remove_flat_background_with_soft_subject_edge);
        let session = Session::with_background_remover(source.clone(), remover);
        let zero = session.resize(32, 32, options(0), &cancel).unwrap();
        let full = session.resize(32, 32, options(100), &cancel).unwrap();

        // This is the former direct AA=1 render: it restores the soft model
        // edge at once and therefore supplies a regression reproducer.
        let foreground = remove_flat_background_with_soft_subject_edge(&source, &cancel).unwrap();
        let raw_scaler = asset_scaler::Session::new(source.clone());
        raw_scaler.prepare(&|| false).unwrap();
        let old_one = raw_scaler
            .resize_with_foreground_opacity(
                &foreground,
                32,
                32,
                GameAssetAa::new(1),
                options(1).contour_opacity_fraction(),
                &|| false,
            )
            .unwrap();
        assert!(
            zero.pixels()
                .zip(old_one.pixels())
                .map(|(hard, soft)| hard[3].abs_diff(soft[3]))
                .max()
                .unwrap()
                > 3,
            "the direct AA=1 model edge must reproduce the old pop"
        );

        // The direct calls define the historical 0%/100% recipes. The
        // Session must return those endpoint bytes unchanged before blending.
        let mut raw_zero = raw_scaler
            .resize_with_foreground_opacity(
                &foreground,
                32,
                32,
                GameAssetAa::new(0),
                options(0).contour_opacity_fraction(),
                &|| false,
            )
            .unwrap();
        harden_opaque_cutout_at_zero_aa(&mut raw_zero, true, GameAssetAa::new(0), &cancel).unwrap();
        let raw_full = raw_scaler
            .resize_with_foreground_opacity(
                &foreground,
                32,
                32,
                GameAssetAa::new(100),
                options(100).contour_opacity_fraction(),
                &|| false,
            )
            .unwrap();
        assert_eq!(zero, raw_zero);
        assert_eq!(full, raw_full);

        let mut previous = zero.clone();
        for percent in 1..=100 {
            let current = session.resize(32, 32, options(percent), &cancel).unwrap();
            let mut largest_step = 0u8;
            for ((start, end), (before, after)) in zero
                .pixels()
                .zip(full.pixels())
                .zip(previous.pixels().zip(current.pixels()))
            {
                let expected = (f64::from(start[3])
                    + (f64::from(end[3]) - f64::from(start[3])) * f64::from(percent) / 100.)
                    .round() as u8;
                assert_eq!(after[3], expected, "alpha at {percent}%");
                if end[3] >= start[3] {
                    assert!(after[3] >= before[3], "alpha rose backwards at {percent}%");
                } else {
                    assert!(after[3] <= before[3], "alpha fell backwards at {percent}%");
                }
                largest_step = largest_step.max(before[3].abs_diff(after[3]));
            }
            assert!(
                largest_step <= 3,
                "alpha step at {percent}% was {largest_step}"
            );
            previous = current;
        }
    }

    #[test]
    fn cached_preview_and_one_shot_use_the_same_pipeline_at_every_aa() {
        let source = Arc::new(RgbaImage::from_fn(96, 80, |x, y| {
            image::Rgba(
                if (y as f64 - (0.57 * x as f64 + 10.)).abs() < 3.
                    || (18..78).contains(&x) && (18..68).contains(&y)
                {
                    [8, 10, 4, 255]
                } else {
                    [240, 230, 220, 255]
                },
            )
        }));
        let calls = Arc::new(AtomicUsize::new(0));
        let remover = counting_remover(calls.clone());
        let session = Session::with_background_remover(source.clone(), remover.clone());
        let cancel = CancellationToken::default();
        let mut outputs = Vec::new();
        for percent in [0, 100, 50, 0] {
            let options = options(percent);
            let preview = session.resize(32, 27, options, &cancel).unwrap();
            assert_eq!(session.resize(32, 27, options, &cancel).unwrap(), preview);
            assert_eq!(
                resize_with_background_remover(&source, 32, 27, options, &cancel, remover.clone())
                    .unwrap(),
                preview
            );
            outputs.push(preview);
        }
        assert_eq!(calls.load(Ordering::Relaxed), 5);
        assert_ne!(outputs[0], outputs[1]);
        assert_ne!(outputs[1], outputs[2]);
        assert_eq!(outputs[0], outputs[3]);
        let endpoints_before_aa_change = session
            .aa_endpoints
            .lock()
            .expect("AA endpoint cache poisoned")
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(endpoints_before_aa_change.target, (32, 27));
        session.resize(32, 27, options(80), &cancel).unwrap();
        let endpoints_after_aa_change = session
            .aa_endpoints
            .lock()
            .expect("AA endpoint cache poisoned")
            .as_ref()
            .unwrap()
            .clone();
        assert!(Arc::ptr_eq(
            &endpoints_before_aa_change,
            &endpoints_after_aa_change
        ));
        // Opacity changes contour compositing and therefore rebuilds this bounded
        // endpoint pair, while the foreground estimate stays cached.
        session
            .resize(
                32,
                27,
                GameAssetOptions::new(GameAssetAa::new(80), 40),
                &cancel,
            )
            .unwrap();
        let endpoints_after_opacity_change = session
            .aa_endpoints
            .lock()
            .expect("AA endpoint cache poisoned")
            .as_ref()
            .unwrap()
            .clone();
        assert!(!Arc::ptr_eq(
            &endpoints_after_aa_change,
            &endpoints_after_opacity_change
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 5);
        // A different preview size replaces the bounded pair but reuses the
        // successful foreground estimate.
        session.resize(31, 26, options(50), &cancel).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 5);
        assert_eq!(
            session
                .aa_endpoints
                .lock()
                .expect("AA endpoint cache poisoned")
                .as_ref()
                .unwrap()
                .target,
            (31, 26)
        );
    }

    #[test]
    fn contour_opacity_blends_contours_without_recoloring_fill() {
        let source = Arc::new(RgbaImage::from_fn(64, 64, |x, y| {
            if !(16..48).contains(&x) || !(16..48).contains(&y) {
                image::Rgba([240, 230, 220, 255])
            } else if x == 16 || x == 47 || y == 16 || y == 47 || (30..34).contains(&y) {
                image::Rgba([12, 10, 8, 255])
            } else {
                image::Rgba([180, 60, 40, 255])
            }
        }));
        let session = Session::with_background_remover(
            source,
            Arc::new(remove_flat_background_with_soft_subject_edge),
        );
        let cancel = CancellationToken::default();
        let no_contours = session
            .resize(
                32,
                32,
                GameAssetOptions::new(GameAssetAa::new(50), 0),
                &cancel,
            )
            .unwrap();
        let half_contours = session
            .resize(
                32,
                32,
                GameAssetOptions::new(GameAssetAa::new(50), 50),
                &cancel,
            )
            .unwrap();
        let full_contours = session
            .resize(
                32,
                32,
                GameAssetOptions::new(GameAssetAa::new(50), 100),
                &cancel,
            )
            .unwrap();

        assert!(
            no_contours
                .pixels()
                .zip(half_contours.pixels())
                .any(|(a, b)| a != b)
        );
        assert!(
            half_contours
                .pixels()
                .zip(full_contours.pixels())
                .any(|(a, b)| a != b)
        );
        // This interior subject pixel is away from the border and centerline,
        // so opacity cannot alter its Lanczos fill or opaque alpha.
        assert_eq!(
            no_contours.get_pixel(12, 12),
            half_contours.get_pixel(12, 12)
        );
        assert_eq!(
            no_contours.get_pixel(12, 12),
            full_contours.get_pixel(12, 12)
        );
        assert_eq!(no_contours.get_pixel(12, 12)[3], 255);
        for ((no_contours, half_contours), full_contours) in no_contours
            .pixels()
            .zip(half_contours.pixels())
            .zip(full_contours.pixels())
        {
            // Any fully opaque fill pixel remains opaque. Partially transparent
            // contour destinations may gain alpha through normal compositing.
            if no_contours[3] == 255 {
                assert_eq!(half_contours[3], 255);
                assert_eq!(full_contours[3], 255);
            }
        }
    }

    #[test]
    fn contour_opacity_uses_original_donor_color_and_compositing_alpha() {
        let source = Arc::new(RgbaImage::from_fn(64, 64, |x, y| {
            if !(16..48).contains(&x) || !(16..48).contains(&y) {
                image::Rgba([240, 230, 220, 255])
            } else if x == 16 || x == 47 || y == 16 || y == 47 || (30..34).contains(&y) {
                image::Rgba([12, 10, 8, 255])
            } else {
                image::Rgba([180, 60, 40, 255])
            }
        }));
        let session = Session::with_background_remover(
            source,
            Arc::new(remove_flat_background_with_soft_subject_edge),
        );
        let cancel = CancellationToken::default();
        let no_contours = session
            .resize(
                32,
                32,
                GameAssetOptions::new(GameAssetAa::new(0), 0),
                &cancel,
            )
            .unwrap();
        let full_contours = session
            .resize(
                32,
                32,
                GameAssetOptions::new(GameAssetAa::new(0), 100),
                &cancel,
            )
            .unwrap();

        assert!(
            full_contours
                .pixels()
                .all(|pixel| pixel[3] == 0 || pixel[3] == 255)
        );
        assert!(
            full_contours
                .pixels()
                .filter(|pixel| pixel[3] == 255)
                .all(|pixel| pixel.0 != [0, 0, 0, 255]),
            "full opacity must not turn opaque contour donors black"
        );
        assert!(
            no_contours
                .pixels()
                .zip(full_contours.pixels())
                .any(|(without, with)| without != with),
            "zero opacity leaves the fill while full opacity adds contour paint"
        );
    }

    #[test]
    fn half_contour_opacity_is_premultiplied_linear_midpoint_on_partial_alpha() {
        let source = Arc::new(RgbaImage::from_fn(64, 64, |x, y| {
            if !(12..52).contains(&x) || !(12..52).contains(&y) {
                image::Rgba([0; 4])
            } else if (y as i32 - (x as i32 / 2 + 12)).abs() <= 2 {
                image::Rgba([12, 10, 8, 160])
            } else {
                image::Rgba([180, 60, 40, 160])
            }
        }));
        let session =
            Session::with_background_remover(source, Arc::new(|image, _| Ok(image.clone())));
        let cancel = CancellationToken::default();
        let without = session
            .resize(
                32,
                32,
                GameAssetOptions::new(GameAssetAa::new(100), 0),
                &cancel,
            )
            .unwrap();
        let half = session
            .resize(
                32,
                32,
                GameAssetOptions::new(GameAssetAa::new(100), 50),
                &cancel,
            )
            .unwrap();
        let full = session
            .resize(
                32,
                32,
                GameAssetOptions::new(GameAssetAa::new(100), 100),
                &cancel,
            )
            .unwrap();

        assert!(
            without
                .pixels()
                .zip(full.pixels())
                .any(|(fill, painted)| fill != painted)
        );
        let mut saw_partial_alpha_change = false;
        for ((fill, half), painted) in without.pixels().zip(half.pixels()).zip(full.pixels()) {
            let fill_alpha = f64::from(fill[3]) / 255.;
            let painted_alpha = f64::from(painted[3]) / 255.;
            let expected_alpha = (fill_alpha + painted_alpha) * 0.5;
            assert!(half[3].abs_diff((expected_alpha * 255.).round() as u8) <= 1);
            if fill[3] != painted[3] && (1..255).contains(&fill[3]) {
                saw_partial_alpha_change = true;
            }
            if half[3] == 0 {
                assert_eq!(half.0, [0; 4]);
                continue;
            }
            let actual_alpha = f64::from(half[3]) / 255.;
            for channel in 0..3 {
                let expected = (srgb_to_linear(fill[channel]) * fill_alpha
                    + srgb_to_linear(painted[channel]) * painted_alpha)
                    * 0.5
                    / actual_alpha;
                assert!(
                    half[channel].abs_diff(linear_to_srgb(expected)) <= 2,
                    "channel {channel} was not the opacity midpoint"
                );
            }
        }
        assert!(
            saw_partial_alpha_change,
            "fixture must exercise contour compositing over partial alpha"
        );
    }

    #[test]
    fn shared_scaler_removes_an_opaque_canvas_before_scaling() {
        let source = Arc::new(RgbaImage::from_fn(64, 64, |x, y| {
            if (20..44).contains(&x) && (20..44).contains(&y) {
                image::Rgba([120, 180, 90, 255])
            } else {
                image::Rgba([240, 230, 220, 255])
            }
        }));
        let cancel = CancellationToken::default();
        let remover = Arc::new(remove_flat_background);
        let one_shot =
            resize_with_background_remover(&source, 16, 16, options(20), &cancel, remover.clone())
                .unwrap();
        let cached = Session::with_background_remover(source, remover)
            .resize(16, 16, options(20), &cancel)
            .unwrap();
        assert_eq!(cached, one_shot);
        assert_eq!(one_shot.get_pixel(0, 0)[3], 0);
    }

    #[test]
    fn zero_aa_hardens_a_model_cutout_of_an_opaque_sprite() {
        let source = RgbaImage::from_fn(64, 64, |x, y| {
            if !(16..48).contains(&x) || !(16..48).contains(&y) {
                image::Rgba([240, 230, 220, 255])
            } else if x == 16 || x == 47 || y == 16 || y == 47 || (30..34).contains(&y) {
                image::Rgba([12, 10, 8, 255])
            } else {
                image::Rgba([180, 60, 40, 255])
            }
        });
        let cancel = CancellationToken::default();
        let remove_background = Arc::new(remove_flat_background_with_soft_subject_edge);

        let hard = resize_with_background_remover(
            &source,
            32,
            32,
            options(0),
            &cancel,
            remove_background.clone(),
        )
        .unwrap();
        assert!(hard.pixels().all(|pixel| pixel[3] == 0 || pixel[3] == 255));
        let without_contours = resize_with_background_remover(
            &source,
            32,
            32,
            GameAssetOptions::new(GameAssetAa::new(0), 0),
            &cancel,
            remove_background.clone(),
        )
        .unwrap();
        assert!(
            hard.pixels()
                .zip(without_contours.pixels())
                .any(|(with_contours, fill)| with_contours != fill),
            "full opacity must add source-derived contour paint"
        );

        let softened = resize_with_background_remover(
            &source,
            32,
            32,
            options(50),
            &cancel,
            remove_background,
        )
        .unwrap();
        assert!(softened.pixels().any(|pixel| (1..255).contains(&pixel[3])));
    }

    #[test]
    fn zero_aa_does_not_harden_explicit_source_alpha() {
        let source = Arc::new(RgbaImage::from_fn(64, 64, |x, y| {
            if (16..48).contains(&x) && (16..48).contains(&y) {
                image::Rgba([180, 60, 40, 160])
            } else {
                image::Rgba([0; 4])
            }
        }));
        let session = Session::with_background_remover(
            source.clone(),
            Arc::new(|image, _| Ok(image.clone())),
        );
        let scaled = session
            .resize(32, 32, options(0), &CancellationToken::default())
            .unwrap();
        assert!(scaled.pixels().any(|pixel| (1..255).contains(&pixel[3])));
    }

    #[test]
    fn zero_aa_hardening_honors_cancellation() {
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        assert!(matches!(
            harden_opaque_cutout_at_zero_aa(
                &mut RgbaImage::from_pixel(4096, 1, image::Rgba([30, 40, 50, 160])),
                true,
                GameAssetAa::new(0),
                &cancellation,
            ),
            Err(AppError::Cancelled)
        ));
    }

    #[test]
    fn shared_scaler_keeps_explicit_source_alpha() {
        let source = Arc::new(RgbaImage::from_fn(64, 64, |x, y| {
            if (20..44).contains(&x) && (20..44).contains(&y) {
                image::Rgba([120, 180, 90, 255])
            } else {
                image::Rgba([240, 0, 220, 0])
            }
        }));
        let cancel = CancellationToken::default();
        let remover = Arc::new(remove_flat_background);
        let one_shot =
            resize_with_background_remover(&source, 16, 16, options(20), &cancel, remover.clone())
                .unwrap();
        let cached = Session::with_background_remover(source, remover)
            .resize(16, 16, options(20), &cancel)
            .unwrap();
        assert_eq!(cached, one_shot);
        assert_eq!(one_shot.get_pixel(0, 0)[3], 0);
        assert_eq!(one_shot.get_pixel(8, 8)[3], 255);
    }

    #[test]
    fn shared_scaler_errors_preserve_application_semantics() {
        let image = Arc::new(RgbaImage::new(16, 16));
        let calls = Arc::new(AtomicUsize::new(0));
        let remover = counting_remover(calls.clone());
        let session = Session::with_background_remover(image.clone(), remover.clone());
        let cancel = CancellationToken::default();
        assert!(matches!(
            session.resize(17, 16, Default::default(), &cancel),
            Err(AppError::InvalidDimensions)
        ));
        assert!(matches!(
            resize_with_background_remover(
                &image,
                17,
                16,
                Default::default(),
                &CancellationToken::default(),
                remover.clone()
            ),
            Err(AppError::InvalidDimensions)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        session.resize(8, 8, Default::default(), &cancel).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        cancel.cancel();
        assert!(matches!(
            session.resize(8, 8, Default::default(), &cancel),
            Err(AppError::Cancelled)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(matches!(
            resize_with_background_remover(&image, 16, 16, Default::default(), &cancel, remover),
            Err(AppError::Cancelled)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(matches!(
            map_error(asset_scaler::Error::GameAssetMemoryLimit { limit_bytes: 42 }),
            AppError::GameAssetMemoryLimit { limit_bytes: 42 }
        ));
    }

    #[test]
    fn cancelled_foreground_is_not_cached() {
        let source = Arc::new(RgbaImage::from_pixel(
            16,
            16,
            image::Rgba([120, 180, 90, 255]),
        ));
        let calls = Arc::new(AtomicUsize::new(0));
        let remover: Arc<BackgroundRemover> = {
            let calls = calls.clone();
            Arc::new(move |image, cancel| {
                if calls.fetch_add(1, Ordering::Relaxed) == 0 {
                    cancel.cancel();
                }
                Ok(image.clone())
            })
        };
        let session = Session::with_background_remover(source, remover);
        let cancelled = CancellationToken::default();
        assert!(matches!(
            session.resize(8, 8, Default::default(), &cancelled),
            Err(AppError::Cancelled)
        ));
        assert!(
            session
                .aa_endpoints
                .lock()
                .expect("AA endpoint cache poisoned")
                .is_none(),
            "a cancelled endpoint build must not publish a partial pair"
        );
        let retry = CancellationToken::default();
        session.resize(8, 8, Default::default(), &retry).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }
}
