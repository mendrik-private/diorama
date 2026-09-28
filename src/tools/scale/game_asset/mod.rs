//! Application boundary for Game Asset scaling: FLUX line art of the
//! untouched source, reduced and sharpened by the shared scaler, multiplied
//! over the BiRefNet foreground's Lanczos fill.
use crate::{
    document::{CancellationToken, GameAssetOptions},
    error::{AppError, Result},
};
use image::{GrayImage, RgbaImage};
use std::{
    sync::{Arc, Mutex, TryLockError},
    thread,
    time::Duration,
};

pub(crate) type BackgroundRemover =
    dyn Fn(&RgbaImage, &CancellationToken) -> Result<RgbaImage> + Send + Sync;
pub(crate) type LineArtGenerator =
    dyn Fn(&RgbaImage, &CancellationToken) -> Result<GrayImage> + Send + Sync;

const OPAQUE_ALPHA: u8 = 255;

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

/// Convert grayscale line art into the opaque image used by the inspection
/// preview.
fn line_art_to_rgba(line_art: &GrayImage, cancel: &CancellationToken) -> Result<RgbaImage> {
    let mut image = RgbaImage::new(line_art.width(), line_art.height());
    for (i, (source, output)) in line_art.pixels().zip(image.pixels_mut()).enumerate() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        let value = source[0];
        output.0 = [value, value, value, OPAQUE_ALPHA];
    }
    cancel.check()?;
    Ok(image)
}

/// A preview session keeps the untouched source for line-art generation,
/// while the background-removed foreground supplies the fill. The line-art
/// scaler is published only after generation finishes without cancellation;
/// it caches the reduced fill and line art of the latest target size.
pub struct Session {
    source: Arc<RgbaImage>,
    scaler: Mutex<Option<Arc<asset_scaler::LineArtSession>>>,
    preparation_gate: Mutex<()>,
    foreground: Mutex<Option<Arc<RgbaImage>>>,
    remove_background: Arc<BackgroundRemover>,
    generate_line_art: Arc<LineArtGenerator>,
}

impl Session {
    pub fn new(source: Arc<RgbaImage>) -> Self {
        Self::with_workers(
            source,
            Arc::new(crate::tools::selection::birefnet_cutout),
            Arc::new(crate::tools::line_art::sketch),
        )
    }

    fn with_workers(
        source: Arc<RgbaImage>,
        remove_background: Arc<BackgroundRemover>,
        generate_line_art: Arc<LineArtGenerator>,
    ) -> Self {
        Self {
            source,
            scaler: Mutex::new(None),
            preparation_gate: Mutex::new(()),
            foreground: Mutex::new(None),
            remove_background,
            generate_line_art,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_workers(
        source: Arc<RgbaImage>,
        remove_background: Arc<BackgroundRemover>,
        generate_line_art: Arc<LineArtGenerator>,
    ) -> Self {
        Self::with_workers(source, remove_background, generate_line_art)
    }

    #[cfg(test)]
    fn with_background_remover(
        source: Arc<RgbaImage>,
        remove_background: Arc<BackgroundRemover>,
    ) -> Self {
        Self::with_workers(
            source,
            remove_background,
            Arc::new(model_like_test_line_art),
        )
    }

    fn validate_dimensions(&self, w: u32, h: u32) -> Result<()> {
        validate_dimensions(self.source.dimensions(), w, h)
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

    /// Serialize line-art generation for this immutable source without
    /// holding the cache lock during model work. Waiting callers poll their
    /// cancellation token, then recheck the completed cache once they acquire
    /// the gate.
    fn scaler(&self, cancel: &CancellationToken) -> Result<Arc<asset_scaler::LineArtSession>> {
        if let Some(scaler) = self
            .scaler
            .lock()
            .expect("line-art scaler cache poisoned")
            .clone()
        {
            cancel.check()?;
            return Ok(scaler);
        }
        loop {
            cancel.check()?;
            let gate = match self.preparation_gate.try_lock() {
                Ok(gate) => gate,
                Err(TryLockError::Poisoned(error)) => error.into_inner(),
                Err(TryLockError::WouldBlock) => {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
            };
            if let Some(scaler) = self
                .scaler
                .lock()
                .expect("line-art scaler cache poisoned")
                .clone()
            {
                drop(gate);
                cancel.check()?;
                return Ok(scaler);
            }
            // Check source dimensions and the working-memory guard ahead of
            // the external model process.
            asset_scaler::LineArtSession::preflight(&self.source).map_err(map_error)?;
            cancel.check()?;
            let line_art = Arc::new((self.generate_line_art)(&self.source, cancel)?);
            cancel.check()?;
            let scaler = Arc::new(
                asset_scaler::LineArtSession::new(&self.source, line_art).map_err(map_error)?,
            );
            let mut cache = self.scaler.lock().expect("line-art scaler cache poisoned");
            cancel.check()?;
            let cached = cache.get_or_insert(scaler).clone();
            drop(cache);
            drop(gate);
            return Ok(cached);
        }
    }

    /// Return the line art as the result multiplies it: the generated line
    /// art at the original size, reduced and sharpened at `options`'s
    /// strength otherwise. This never needs the extracted foreground.
    pub fn line_art(
        &self,
        w: u32,
        h: u32,
        options: GameAssetOptions,
        cancel: &CancellationToken,
    ) -> Result<RgbaImage> {
        cancel.check()?;
        self.validate_dimensions(w, h)?;
        let line_art = self
            .scaler(cancel)?
            .line_art(w, h, options.strength(), &|| cancel.check().is_err())
            .map_err(map_error)?;
        line_art_to_rgba(&line_art, cancel)
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
        // Generate the line art from the untouched original before BiRefNet
        // runs; neither cache lock is held during model work.
        let scaler = self.scaler(cancel)?;
        let foreground = self.foreground(cancel)?;
        let resized = scaler
            .resize_with_foreground(&foreground, w, h, options.strength(), &|| {
                cancel.check().is_err()
            })
            .map_err(map_error)?;
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
    #[cfg(test)]
    let generate_line_art: Arc<LineArtGenerator> = Arc::new(model_like_test_line_art);
    #[cfg(not(test))]
    let generate_line_art: Arc<LineArtGenerator> = Arc::new(crate::tools::line_art::sketch);
    Session::with_workers(
        Arc::new(image.clone()),
        remove_background,
        generate_line_art,
    )
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

/// A deterministic stand-in for the line-art model in normal unit tests:
/// near-neutral dark ink becomes black or weak gray, while saturated fill
/// stays white.
#[cfg(test)]
fn model_like_test_line_art(image: &RgbaImage, cancel: &CancellationToken) -> Result<GrayImage> {
    let mut line_art = GrayImage::new(image.width(), image.height());
    for (index, (source, output)) in image.pixels().zip(line_art.pixels_mut()).enumerate() {
        if index.is_multiple_of(4096) {
            cancel.check()?;
        }
        let (minimum, maximum) = source.0[..3]
            .iter()
            .copied()
            .fold((u8::MAX, 0_u8), |(minimum, maximum), value| {
                (minimum.min(value), maximum.max(value))
            });
        output[0] = if maximum <= 80 {
            0
        } else if maximum - minimum <= 32 && maximum <= 130 {
            112
        } else {
            255
        };
    }
    cancel.check()?;
    Ok(line_art)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env, fs,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    fn options(strength: u8) -> GameAssetOptions {
        GameAssetOptions::new(strength)
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

    fn counting_remover(calls: Arc<AtomicUsize>) -> Arc<BackgroundRemover> {
        Arc::new(move |image, cancel| {
            calls.fetch_add(1, Ordering::Relaxed);
            remove_flat_background(image, cancel)
        })
    }

    fn counting_line_art_generator(
        calls: Arc<AtomicUsize>,
        delay: Duration,
    ) -> Arc<LineArtGenerator> {
        Arc::new(move |image, cancel| {
            calls.fetch_add(1, Ordering::Relaxed);
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            model_like_test_line_art(image, cancel)
        })
    }

    fn outlined_fixture() -> Arc<RgbaImage> {
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

    /// The line art the shared scaler multiplies, built directly from the
    /// same generated line art.
    fn expected_scaler(source: &RgbaImage) -> asset_scaler::LineArtSession {
        let line_art = model_like_test_line_art(source, &CancellationToken::default()).unwrap();
        asset_scaler::LineArtSession::new(source, Arc::new(line_art)).unwrap()
    }

    #[test]
    fn line_art_conversion_preserves_intermediate_grayscale_bytes() {
        let line_art = GrayImage::from_raw(4, 1, vec![0, 13, 128, 255]).unwrap();
        let converted = line_art_to_rgba(&line_art, &CancellationToken::default()).unwrap();

        assert_eq!(
            converted.as_raw(),
            &[
                0, 0, 0, 255, 13, 13, 13, 255, 128, 128, 128, 255, 255, 255, 255, 255
            ]
        );
    }

    #[test]
    fn identity_returns_the_original_without_background_or_line_art_work() {
        let source = Arc::new(RgbaImage::from_fn(16, 12, |x, y| {
            image::Rgba([
                x as u8,
                y as u8,
                180,
                if (x + y) % 3 == 0 { 90 } else { 255 },
            ])
        }));
        let removals = Arc::new(AtomicUsize::new(0));
        let generations = Arc::new(AtomicUsize::new(0));
        let session = Session::with_test_workers(
            source.clone(),
            counting_remover(removals.clone()),
            counting_line_art_generator(generations.clone(), Duration::ZERO),
        );
        let output = session
            .resize(16, 12, options(40), &CancellationToken::default())
            .unwrap();
        assert_eq!(output, *source);
        assert_eq!(removals.load(Ordering::Relaxed), 0);
        assert_eq!(generations.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn line_art_is_generated_once_across_targets_strengths_and_concurrent_requests() {
        let source = outlined_fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        let session = Arc::new(Session::with_test_workers(
            source,
            Arc::new(remove_flat_background),
            counting_line_art_generator(calls.clone(), Duration::from_millis(30)),
        ));
        std::thread::scope(|scope| {
            let first = session.clone();
            let second = session.clone();
            let first = scope
                .spawn(move || first.resize(32, 32, options(0), &CancellationToken::default()));
            let second = scope
                .spawn(move || second.resize(32, 32, options(100), &CancellationToken::default()));
            first.join().unwrap().unwrap();
            second.join().unwrap().unwrap();
        });
        session
            .resize(24, 24, options(60), &CancellationToken::default())
            .unwrap();
        session
            .line_art(32, 32, options(40), &CancellationToken::default())
            .unwrap();
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "one immutable source must run the line-art model once despite strength, target, and concurrent requests"
        );
    }

    #[test]
    fn invalid_or_cancelled_line_art_is_not_cached_and_a_retry_succeeds() {
        let source = outlined_fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        let generator: Arc<LineArtGenerator> = {
            let calls = calls.clone();
            Arc::new(
                move |image, cancel| match calls.fetch_add(1, Ordering::Relaxed) {
                    0 => Ok(GrayImage::new(1, 1)),
                    1 => {
                        cancel.cancel();
                        model_like_test_line_art(image, cancel)
                    }
                    _ => model_like_test_line_art(image, cancel),
                },
            )
        };
        let session =
            Session::with_test_workers(source, Arc::new(remove_flat_background), generator);
        let first = CancellationToken::default();
        assert!(matches!(
            session.resize(32, 32, options(40), &first),
            Err(AppError::InvalidDimensions)
        ));
        assert!(
            session
                .scaler
                .lock()
                .expect("line-art scaler cache poisoned")
                .is_none()
        );
        let cancelled = CancellationToken::default();
        assert!(matches!(
            session.resize(32, 32, options(40), &cancelled),
            Err(AppError::Cancelled)
        ));
        assert!(
            session
                .scaler
                .lock()
                .expect("line-art scaler cache poisoned")
                .is_none()
        );
        session
            .resize(32, 32, options(40), &CancellationToken::default())
            .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn injected_line_art_is_multiplied_over_the_foreground_fill() {
        // The subject's RGB is flat, so only the supplied line art can darken
        // the result, and only where it has ink.
        let source = Arc::new(RgbaImage::from_fn(64, 64, |x, y| {
            if (4..60).contains(&x) && (4..60).contains(&y) {
                image::Rgba([190, 95, 55, 255])
            } else {
                image::Rgba([0; 4])
            }
        }));
        let generator: Arc<LineArtGenerator> = Arc::new(|image, cancel| {
            cancel.check()?;
            let mut line_art =
                GrayImage::from_pixel(image.width(), image.height(), image::Luma([255]));
            for y in 30..34 {
                for x in 8..56 {
                    line_art.put_pixel(x, y, image::Luma([0]));
                }
            }
            Ok(line_art)
        });
        let session =
            Session::with_test_workers(source, Arc::new(|image, _| Ok(image.clone())), generator);
        let result = session
            .resize(32, 32, options(40), &CancellationToken::default())
            .unwrap();
        assert_eq!(result.get_pixel(16, 16).0, [0, 0, 0, 255]);
        assert_eq!(result.get_pixel(16, 6).0, [190, 95, 55, 255]);
        assert_eq!(result.get_pixel(0, 0).0, [0; 4]);
    }

    #[test]
    fn resize_and_preview_use_the_shared_line_art_scaler_at_every_strength() {
        let source = outlined_fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        let remover = counting_remover(calls.clone());
        let session = Session::with_background_remover(source.clone(), remover.clone());
        let cancel = CancellationToken::default();
        let expected = expected_scaler(&source);
        let foreground = Arc::new(remove_flat_background(&source, &cancel).unwrap());
        let mut outputs = Vec::new();
        for strength in [0, 100, 40, 0] {
            let options = options(strength);
            let preview = session.resize(32, 27, options, &cancel).unwrap();
            assert_eq!(
                preview,
                expected
                    .resize_with_foreground(&foreground, 32, 27, options.strength(), &|| false)
                    .unwrap()
            );
            assert_eq!(
                resize_with_background_remover(&source, 32, 27, options, &cancel, remover.clone())
                    .unwrap(),
                preview
            );
            assert_eq!(
                session.line_art(32, 27, options, &cancel).unwrap(),
                line_art_to_rgba(
                    &expected
                        .line_art(32, 27, options.strength(), &|| false)
                        .unwrap(),
                    &cancel
                )
                .unwrap()
            );
            outputs.push(preview);
        }
        // One cached foreground plus one per one-shot resize.
        assert_eq!(calls.load(Ordering::Relaxed), 5);
        assert_ne!(outputs[0], outputs[1]);
        assert_ne!(outputs[1], outputs[2]);
        assert_eq!(outputs[0], outputs[3]);
        assert_eq!(outputs[0].get_pixel(0, 0)[3], 0, "the canvas is removed");
    }

    #[test]
    fn line_art_preview_is_raw_at_original_size_and_never_extracts_the_foreground() {
        let source = outlined_fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        let session =
            Session::with_background_remover(source.clone(), counting_remover(calls.clone()));
        let cancel = CancellationToken::default();
        let raw = model_like_test_line_art(&source, &cancel).unwrap();
        for strength in [0, 100] {
            assert_eq!(
                session
                    .line_art(64, 64, options(strength), &cancel)
                    .unwrap(),
                line_art_to_rgba(&raw, &cancel).unwrap()
            );
        }
        let reduced = session.line_art(32, 32, options(100), &cancel).unwrap();
        assert!(reduced.pixels().all(|pixel| {
            let [r, g, b, a] = pixel.0;
            r == g && g == b && a == 255
        }));
        assert!(reduced.pixels().any(|pixel| pixel[0] < 128));
        assert!(reduced.pixels().any(|pixel| pixel.0 == [255; 4]));
        assert_ne!(
            reduced,
            session.line_art(32, 32, options(0), &cancel).unwrap()
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn line_art_preview_validates_dimensions_and_cancellation() {
        let calls = Arc::new(AtomicUsize::new(0));
        let session = Session::with_test_workers(
            outlined_fixture(),
            Arc::new(remove_flat_background),
            counting_line_art_generator(calls.clone(), Duration::ZERO),
        );
        let cancel = CancellationToken::default();
        assert!(matches!(
            session.line_art(0, 32, options(40), &cancel),
            Err(AppError::InvalidDimensions)
        ));
        assert!(matches!(
            session.line_art(65, 32, options(40), &cancel),
            Err(AppError::InvalidDimensions)
        ));
        cancel.cancel();
        assert!(matches!(
            session.line_art(32, 32, options(40), &cancel),
            Err(AppError::Cancelled)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
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
                .foreground
                .lock()
                .expect("foreground cache poisoned")
                .is_none(),
            "a cancelled foreground must not be cached"
        );
        let retry = CancellationToken::default();
        session.resize(8, 8, Default::default(), &retry).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    /// Runs the production line-art and BiRefNet workers through `Session::new`.
    ///
    /// This is deliberately opt-in: normal unit tests inject deterministic
    /// grayscale line art and never require local inference models. Set
    /// `DIORAMA_LINE_ART_SMOKE_INPUT` to a wizard-like PNG and, optionally,
    /// `DIORAMA_LINE_ART_APP_OUT` to a fresh artifact directory before running:
    ///
    /// `cargo test --lib line_art_app_wizard_capture -- --ignored --nocapture`
    #[test]
    #[ignore = "requires local line-art and BiRefNet inference models"]
    fn line_art_app_wizard_capture() -> Result<()> {
        let input = env::var_os("DIORAMA_LINE_ART_SMOKE_INPUT")
            .map(PathBuf::from)
            .expect("set DIORAMA_LINE_ART_SMOKE_INPUT to a wizard PNG");
        let output = env::var_os("DIORAMA_LINE_ART_APP_OUT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp/diorama-line-art-app-v1"));
        fs::create_dir_all(&output)?;

        let source = Arc::new(image::open(&input)?.into_rgba8());
        assert!(
            source.width() >= 256 && source.height() >= 256,
            "the smoke input must support 256px and 128px target captures"
        );
        let cancel = CancellationToken::default();
        let session = Session::new(source.clone());

        // Original-size `line_art` is the generated line art itself. It also
        // ensures this capture uses Session::new's production worker rather
        // than a test-only injected generator.
        session
            .line_art(
                source.width(),
                source.height(),
                GameAssetOptions::default(),
                &cancel,
            )?
            .save(output.join("source-line-art.png"))?;

        for side in [256, 128] {
            for strength in [0, 40, 100] {
                let options = GameAssetOptions::new(strength);
                session
                    .line_art(side, side, options, &cancel)?
                    .save(output.join(format!("line-art-{side}-strength{strength}.png")))?;
                session
                    .resize(side, side, options, &cancel)?
                    .save(output.join(format!("colored-{side}-strength{strength}.png")))?;
            }
        }
        eprintln!("Line-art app capture written to {}", output.display());
        Ok(())
    }
}
