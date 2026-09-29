//! Application boundary for Game Asset scaling: FLUX line art and a de-inked
//! fill generated at the target size, cut out by BiRefNet from the generated
//! fill, then sharpened and multiplied by the shared scaler.
use crate::{
    document::{CancellationToken, GameAssetOptions},
    error::{AppError, Result},
    tools::line_art::{GameAssetLayers, Progress},
};
use asset_scaler::LineArtLayers;
use image::{GrayImage, RgbaImage};
use std::{
    sync::{Arc, Mutex, TryLockError},
    thread,
    time::Duration,
};

/// Generates the target-sized layers of the source; see
/// [`crate::tools::line_art::generate`].
pub(crate) type LineArtGenerator = dyn Fn(&RgbaImage, (u32, u32), &CancellationToken, &dyn Fn(Progress)) -> Result<GameAssetLayers>
    + Send
    + Sync;

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

/// The generated layers of one target size.
struct TargetLayers {
    size: (u32, u32),
    layers: Arc<LineArtLayers>,
}

/// A preview session keeps the source and the layers of the latest target
/// size, so a Strength change only re-sharpens and multiplies. The cache is
/// published only by work that finished without cancellation.
pub struct Session {
    source: Arc<RgbaImage>,
    layers: Mutex<Option<Arc<TargetLayers>>>,
    generation_gate: Mutex<()>,
    generate_layers: Arc<LineArtGenerator>,
}

impl Session {
    pub fn new(source: Arc<RgbaImage>) -> Self {
        Self::with_generator(source, Arc::new(crate::tools::line_art::generate))
    }

    fn with_generator(source: Arc<RgbaImage>, generate_layers: Arc<LineArtGenerator>) -> Self {
        Self {
            source,
            layers: Mutex::new(None),
            generation_gate: Mutex::new(()),
            generate_layers,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_generator(
        source: Arc<RgbaImage>,
        generate_layers: Arc<LineArtGenerator>,
    ) -> Self {
        Self::with_generator(source, generate_layers)
    }

    fn validate_dimensions(&self, w: u32, h: u32) -> Result<()> {
        validate_dimensions(self.source.dimensions(), w, h)
    }

    fn cached_layers(&self, size: (u32, u32)) -> Option<Arc<TargetLayers>> {
        self.layers
            .lock()
            .expect("line-art layer cache poisoned")
            .clone()
            .filter(|cached| cached.size == size)
    }

    /// Serialize generation for this immutable source without holding the
    /// cache lock during model work. Waiting callers poll their cancellation
    /// token, then recheck the completed cache once they acquire the gate.
    fn layers(
        &self,
        size: (u32, u32),
        cancel: &CancellationToken,
        progress: &dyn Fn(Progress),
    ) -> Result<Arc<TargetLayers>> {
        if let Some(layers) = self.cached_layers(size) {
            cancel.check()?;
            return Ok(layers);
        }
        loop {
            cancel.check()?;
            let gate = match self.generation_gate.try_lock() {
                Ok(gate) => gate,
                Err(TryLockError::Poisoned(error)) => error.into_inner(),
                Err(TryLockError::WouldBlock) => {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
            };
            if let Some(layers) = self.cached_layers(size) {
                drop(gate);
                cancel.check()?;
                return Ok(layers);
            }
            // Check the working-memory guard ahead of the model processes.
            LineArtLayers::preflight(size).map_err(map_error)?;
            cancel.check()?;
            let generated = (self.generate_layers)(&self.source, size, cancel, progress)?;
            if generated.line_art.dimensions() != size {
                return Err(AppError::InvalidDimensions);
            }
            let layers = LineArtLayers::new(
                generated.line_art,
                generated.fill,
                generated.alpha,
                &generated.reference,
                &|| cancel.check().is_err(),
            )
            .map_err(map_error)?;
            let layers = Arc::new(TargetLayers {
                size,
                layers: Arc::new(layers),
            });
            let mut cache = self.layers.lock().expect("line-art layer cache poisoned");
            cancel.check()?;
            *cache = Some(layers.clone());
            drop(cache);
            drop(gate);
            return Ok(layers);
        }
    }

    /// Return the line art as the result multiplies it: generated at the
    /// target size and sharpened at `options`'s strength. `progress` reports
    /// a running generation.
    pub fn line_art(
        &self,
        w: u32,
        h: u32,
        options: GameAssetOptions,
        cancel: &CancellationToken,
        progress: &dyn Fn(Progress),
    ) -> Result<RgbaImage> {
        cancel.check()?;
        self.validate_dimensions(w, h)?;
        let line_art = self
            .layers((w, h), cancel, progress)?
            .layers
            .line_art(options.strength(), &|| cancel.check().is_err())
            .map_err(map_error)?;
        line_art_to_rgba(&line_art, cancel)
    }

    /// The Game Asset result at `w`×`h`; the source size returns the
    /// source. `progress` reports a running generation.
    pub fn resize(
        &self,
        w: u32,
        h: u32,
        options: GameAssetOptions,
        cancel: &CancellationToken,
        progress: &dyn Fn(Progress),
    ) -> Result<RgbaImage> {
        cancel.check()?;
        self.validate_dimensions(w, h)?;
        if (w, h) == self.source.dimensions() {
            return Ok((*self.source).clone());
        }
        let resized = self
            .layers((w, h), cancel, progress)?
            .layers
            .compose(options.strength(), &|| cancel.check().is_err())
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
    cancel.check()?;
    validate_dimensions(image.dimensions(), w, h)?;
    if image.dimensions() == (w, h) {
        return Ok(image.clone());
    }
    #[cfg(test)]
    let generate_layers: Arc<LineArtGenerator> = Arc::new(model_like_test_layers);
    #[cfg(not(test))]
    let generate_layers: Arc<LineArtGenerator> = Arc::new(crate::tools::line_art::generate);
    Session::with_generator(Arc::new(image.clone()), generate_layers).resize(
        w,
        h,
        options,
        cancel,
        &|_| {},
    )
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

/// A deterministic stand-in for the model and the cutout in normal unit
/// tests. The fill and the reference are the white-composited source
/// reduced with Lanczos. The line art is drawn at the source size, where
/// near-neutral dark ink becomes black or weak gray and saturated fill stays
/// white, and reduced with Lanczos, so its edges are soft like the model's.
/// The cutout keys out the colour of the source's top-left pixel, capped by
/// the source's alpha.
#[cfg(test)]
pub(crate) fn model_like_test_layers(
    image: &RgbaImage,
    (w, h): (u32, u32),
    cancel: &CancellationToken,
    _progress: &dyn Fn(Progress),
) -> Result<GameAssetLayers> {
    use image::imageops::{FilterType, resize};
    cancel.check()?;
    let key = image.get_pixel(0, 0).0;
    let mut fill = image::RgbImage::new(image.width(), image.height());
    let mut line_art = GrayImage::new(image.width(), image.height());
    let mut alpha = GrayImage::new(image.width(), image.height());
    for (index, (((source, fill), line), alpha)) in image
        .pixels()
        .zip(fill.pixels_mut())
        .zip(line_art.pixels_mut())
        .zip(alpha.pixels_mut())
        .enumerate()
    {
        if index.is_multiple_of(4096) {
            cancel.check()?;
        }
        let opacity = u16::from(source[3]);
        let composite = |channel: u8| {
            ((u16::from(channel) * opacity + 255 * (255 - opacity) + 127) / 255) as u8
        };
        fill.0 = [
            composite(source[0]),
            composite(source[1]),
            composite(source[2]),
        ];
        let (minimum, maximum) = fill
            .0
            .iter()
            .fold((u8::MAX, 0_u8), |(minimum, maximum), &value| {
                (minimum.min(value), maximum.max(value))
            });
        line[0] = if maximum <= 80 {
            0
        } else if maximum - minimum <= 32 && maximum <= 130 {
            112
        } else {
            255
        };
        alpha[0] = if source.0 == key { 0 } else { source[3] };
    }
    cancel.check()?;
    let fill = resize(&fill, w, h, FilterType::Lanczos3);
    Ok(GameAssetLayers {
        line_art: resize(&line_art, w, h, FilterType::Lanczos3),
        reference: fill.clone(),
        fill,
        alpha: resize(&alpha, w, h, FilterType::Lanczos3),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::line_art::Stage;
    use std::{
        env, fs,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
        time::Instant,
    };

    fn options(strength: u8) -> GameAssetOptions {
        GameAssetOptions::new(strength)
    }

    fn no_progress(_: Progress) {}

    fn counting_generator(calls: Arc<AtomicUsize>, delay: Duration) -> Arc<LineArtGenerator> {
        Arc::new(move |image, size, cancel, progress| {
            calls.fetch_add(1, Ordering::Relaxed);
            progress(Progress {
                fraction: 0.5,
                remaining: Duration::from_secs(3),
                stage: Stage::Generating,
            });
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            model_like_test_layers(image, size, cancel, progress)
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

    /// The shared scaler's result for the stand-in model's layers.
    fn expected(source: &RgbaImage, (w, h): (u32, u32), strength: u8) -> (RgbaImage, RgbaImage) {
        let cancel = CancellationToken::default();
        let generated = model_like_test_layers(source, (w, h), &cancel, &no_progress).unwrap();
        let layers = LineArtLayers::new(
            generated.line_art,
            generated.fill,
            generated.alpha,
            &generated.reference,
            &|| false,
        )
        .unwrap();
        let composed = layers
            .compose(options(strength).strength(), &|| false)
            .unwrap();
        let line_art = line_art_to_rgba(
            &layers
                .line_art(options(strength).strength(), &|| false)
                .unwrap(),
            &cancel,
        )
        .unwrap();
        (composed, line_art)
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
    fn identity_returns_the_original_without_generating() {
        let source = Arc::new(RgbaImage::from_fn(16, 12, |x, y| {
            image::Rgba([
                x as u8,
                y as u8,
                180,
                if (x + y) % 3 == 0 { 90 } else { 255 },
            ])
        }));
        let generations = Arc::new(AtomicUsize::new(0));
        let session = Session::with_test_generator(
            source.clone(),
            counting_generator(generations.clone(), Duration::ZERO),
        );
        let output = session
            .resize(16, 12, options(40), &CancellationToken::default(), &|_| {
                panic!("no generation, no progress")
            })
            .unwrap();
        assert_eq!(output, *source);
        assert_eq!(generations.load(Ordering::Relaxed), 0);
        assert_eq!(
            resize(&source, 16, 12, options(40), &CancellationToken::default()).unwrap(),
            *source
        );
    }

    #[test]
    fn layers_are_generated_at_each_target_size_and_reused_across_strengths() {
        let source = outlined_fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        let session = Arc::new(Session::with_test_generator(
            source,
            counting_generator(calls.clone(), Duration::from_millis(30)),
        ));
        std::thread::scope(|scope| {
            let first = session.clone();
            let second = session.clone();
            let cancel = CancellationToken::default();
            let first =
                scope.spawn(move || first.resize(32, 32, options(0), &cancel, &no_progress));
            let cancel = CancellationToken::default();
            let second =
                scope.spawn(move || second.resize(32, 32, options(100), &cancel, &no_progress));
            first.join().unwrap().unwrap();
            second.join().unwrap().unwrap();
        });
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "concurrent requests share one run"
        );
        // A Strength change and the line-art preview reuse the layers: no
        // generation, no cutout and no progress.
        let progressed = AtomicUsize::new(0);
        let count = |_: Progress| {
            progressed.fetch_add(1, Ordering::Relaxed);
        };
        for strength in [60, 0, 100] {
            session
                .resize(
                    32,
                    32,
                    options(strength),
                    &CancellationToken::default(),
                    &count,
                )
                .unwrap();
            session
                .line_art(
                    32,
                    32,
                    options(strength),
                    &CancellationToken::default(),
                    &count,
                )
                .unwrap();
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            progressed.load(Ordering::Relaxed),
            0,
            "a reused pair reports no progress"
        );
        // Another target is generated at its own size, and reports progress.
        session
            .resize(24, 24, options(60), &CancellationToken::default(), &count)
            .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(progressed.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn invalid_or_cancelled_layers_are_not_cached_and_a_retry_succeeds() {
        let source = outlined_fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        let generator: Arc<LineArtGenerator> = {
            let calls = calls.clone();
            Arc::new(move |image, size, cancel, progress| {
                match calls.fetch_add(1, Ordering::Relaxed) {
                    0 => model_like_test_layers(image, (size.0, size.1 - 1), cancel, progress),
                    1 => {
                        let mut pair = model_like_test_layers(image, size, cancel, progress)?;
                        pair.fill = image::RgbImage::new(1, 1);
                        Ok(pair)
                    }
                    2 => {
                        cancel.cancel();
                        model_like_test_layers(image, size, cancel, progress)
                    }
                    _ => model_like_test_layers(image, size, cancel, progress),
                }
            })
        };
        let session = Session::with_test_generator(source, generator);
        let cached = || {
            session
                .layers
                .lock()
                .expect("line-art layer cache poisoned")
                .is_some()
        };
        for _ in 0..2 {
            assert!(matches!(
                session.resize(
                    32,
                    32,
                    options(40),
                    &CancellationToken::default(),
                    &no_progress
                ),
                Err(AppError::InvalidDimensions)
            ));
            assert!(!cached());
        }
        assert!(matches!(
            session.resize(
                32,
                32,
                options(40),
                &CancellationToken::default(),
                &no_progress
            ),
            Err(AppError::Cancelled)
        ));
        assert!(!cached());
        session
            .resize(
                32,
                32,
                options(40),
                &CancellationToken::default(),
                &no_progress,
            )
            .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn the_cleaned_fill_is_multiplied_by_the_line_art_under_the_generated_alpha() {
        // A flat subject but a striped generated fill on a white background:
        // the colour restoration pulls the stripes back to the reference's
        // colour away from the ink, the line art darkens it where it has
        // ink, and alpha is the generated cutout.
        let source = Arc::new(RgbaImage::from_pixel(64, 64, image::Rgba([9, 9, 9, 255])));
        let generator: Arc<LineArtGenerator> = Arc::new(|_, (w, h), cancel, _| {
            cancel.check()?;
            let inside = |x, y| (2..30).contains(&x) && (2..30).contains(&y);
            Ok(GameAssetLayers {
                line_art: GrayImage::from_fn(w, h, |_, y| {
                    image::Luma([if (15..17).contains(&y) { 0 } else { 255 }])
                }),
                fill: image::RgbImage::from_fn(w, h, |x, y| {
                    if inside(x, y) {
                        image::Rgb([x as u8 * 4, 50, 200])
                    } else {
                        image::Rgb([255, 255, 255])
                    }
                }),
                alpha: GrayImage::from_fn(w, h, |x, y| {
                    image::Luma([if inside(x, y) { 255 } else { 0 }])
                }),
                reference: image::RgbImage::from_fn(w, h, |x, y| {
                    if inside(x, y) {
                        image::Rgb([190, 95, 55])
                    } else {
                        image::Rgb([255, 255, 255])
                    }
                }),
            })
        });
        let session = Session::with_test_generator(source, generator);
        let result = session
            .resize(
                32,
                32,
                options(0),
                &CancellationToken::default(),
                &no_progress,
            )
            .unwrap();
        assert_eq!(result.get_pixel(10, 15).0, [0, 0, 0, 255]);
        assert_eq!(result.get_pixel(10, 8).0, [190, 95, 55, 255]);
        assert_eq!(result.get_pixel(0, 0)[3], 0);
        assert_eq!(result.get_pixel(31, 31)[3], 0);
    }

    #[test]
    fn resize_and_preview_use_the_shared_layers_at_every_strength() {
        let source = outlined_fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        let session = Session::with_test_generator(
            source.clone(),
            counting_generator(calls.clone(), Duration::ZERO),
        );
        let cancel = CancellationToken::default();
        let mut outputs = Vec::new();
        for strength in [0, 100, 40, 0] {
            let (composed, line_art) = expected(&source, (32, 27), strength);
            let preview = session
                .resize(32, 27, options(strength), &cancel, &no_progress)
                .unwrap();
            assert_eq!(preview, composed);
            assert_eq!(
                resize(&source, 32, 27, options(strength), &cancel).unwrap(),
                preview
            );
            assert_eq!(
                session
                    .line_art(32, 27, options(strength), &cancel, &no_progress)
                    .unwrap(),
                line_art
            );
            outputs.push(preview);
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_ne!(outputs[0], outputs[1]);
        assert_ne!(outputs[1], outputs[2]);
        assert_eq!(outputs[0], outputs[3]);
        assert_eq!(outputs[0].get_pixel(0, 0)[3], 0, "the canvas is cut out");
    }

    #[test]
    fn line_art_preview_is_the_sharpened_line_art() {
        let source = outlined_fixture();
        let session =
            Session::with_test_generator(source.clone(), Arc::new(model_like_test_layers));
        let cancel = CancellationToken::default();
        // The source size is a target like any other for the preview.
        for size in [(64, 64), (32, 32)] {
            let reduced = session
                .line_art(size.0, size.1, options(100), &cancel, &no_progress)
                .unwrap();
            assert_eq!(reduced, expected(&source, size, 100).1);
            assert!(reduced.pixels().all(|pixel| {
                let [r, g, b, a] = pixel.0;
                r == g && g == b && a == 255
            }));
            assert!(reduced.pixels().any(|pixel| pixel[0] < 128));
            assert!(reduced.pixels().any(|pixel| pixel.0 == [255; 4]));
        }
        // Soft reduced edges respond to the Strength.
        assert_ne!(
            session
                .line_art(32, 32, options(100), &cancel, &no_progress)
                .unwrap(),
            session
                .line_art(32, 32, options(0), &cancel, &no_progress)
                .unwrap()
        );
    }

    #[test]
    fn line_art_preview_validates_dimensions_and_cancellation() {
        let calls = Arc::new(AtomicUsize::new(0));
        let session = Session::with_test_generator(
            outlined_fixture(),
            counting_generator(calls.clone(), Duration::ZERO),
        );
        let cancel = CancellationToken::default();
        assert!(matches!(
            session.line_art(0, 32, options(40), &cancel, &no_progress),
            Err(AppError::InvalidDimensions)
        ));
        assert!(matches!(
            session.line_art(65, 32, options(40), &cancel, &no_progress),
            Err(AppError::InvalidDimensions)
        ));
        cancel.cancel();
        assert!(matches!(
            session.line_art(32, 32, options(40), &cancel, &no_progress),
            Err(AppError::Cancelled)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn shared_scaler_errors_preserve_application_semantics() {
        let image = Arc::new(RgbaImage::new(16, 16));
        let calls = Arc::new(AtomicUsize::new(0));
        let session = Session::with_test_generator(
            image.clone(),
            counting_generator(calls.clone(), Duration::ZERO),
        );
        let cancel = CancellationToken::default();
        assert!(matches!(
            session.resize(17, 16, Default::default(), &cancel, &no_progress),
            Err(AppError::InvalidDimensions)
        ));
        assert!(matches!(
            resize(&image, 17, 16, Default::default(), &cancel),
            Err(AppError::InvalidDimensions)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        session
            .resize(8, 8, Default::default(), &cancel, &no_progress)
            .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        cancel.cancel();
        assert!(matches!(
            session.resize(8, 8, Default::default(), &cancel, &no_progress),
            Err(AppError::Cancelled)
        ));
        assert!(matches!(
            resize(&image, 16, 16, Default::default(), &cancel),
            Err(AppError::Cancelled)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(matches!(
            map_error(asset_scaler::Error::GameAssetMemoryLimit { limit_bytes: 42 }),
            AppError::GameAssetMemoryLimit { limit_bytes: 42 }
        ));
        assert!(
            session
                .resize(
                    8,
                    8,
                    Default::default(),
                    &CancellationToken::default(),
                    &no_progress
                )
                .is_ok()
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    /// Runs the production FLUX and BiRefNet workers through `Session::new`
    /// at 128², 256² and 512² (or the comma-separated sides in
    /// `DIORAMA_LINE_ART_SMOKE_SIDES`) and Strength 0/40/100, and prints each
    /// generation's time, its initial estimate and the progress trace.
    ///
    /// This is deliberately opt-in: normal unit tests use a deterministic
    /// stand-in and never require local inference models. Set
    /// `DIORAMA_LINE_ART_SMOKE_INPUT` to a wizard-like image and, optionally,
    /// `DIORAMA_LINE_ART_APP_OUT` to a fresh artifact directory:
    ///
    /// `cargo test --lib line_art_app_wizard_capture -- --ignored --nocapture`
    /// Renders each image in `DIORAMA_LINE_ART_WARM_INPUTS` (one per line: a
    /// path, optionally followed by a tab and the target's shorter side,
    /// 128 by default, the longer one keeping the source's aspect ratio) at
    /// Strength 40 through `Session::new`, with the resident worker kept
    /// loaded between them, and prints each time against its initial
    /// estimate and the time of its cutout stage. Writes `<stem>.png` (the
    /// result), `<stem>-fill.png`, `<stem>-alpha.png` and `<stem>-line-art.png`
    /// (the sharpened line art) to `DIORAMA_LINE_ART_APP_OUT`:
    ///
    /// `cargo test --lib line_art_app_warm_session -- --ignored --nocapture`
    #[test]
    #[ignore = "requires local line-art and BiRefNet inference models"]
    fn line_art_app_warm_session() -> Result<()> {
        let inputs = env::var("DIORAMA_LINE_ART_WARM_INPUTS")
            .expect("set DIORAMA_LINE_ART_WARM_INPUTS to image paths, one per line");
        let output = env::var_os("DIORAMA_LINE_ART_APP_OUT")
            .map(PathBuf::from)
            .expect("set DIORAMA_LINE_ART_APP_OUT");
        fs::create_dir_all(&output)?;
        crate::tools::line_art::keep_real_worker_warm(true);
        let cancel = CancellationToken::default();
        let result = inputs
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .try_for_each(|line| -> Result<()> {
                let (input, side) = line.split_once('\t').map_or((line, 128), |(input, side)| {
                    (input, side.trim().parse().expect("the side is an integer"))
                });
                let input = PathBuf::from(input);
                let name = input.file_stem().unwrap().to_string_lossy().into_owned();
                let source = Arc::new(image::open(&input)?.into_rgba8());
                let (width, height) = source.dimensions();
                let scale = f64::from(side) / f64::from(width.min(height));
                let size = (
                    ((f64::from(width) * scale).round() as u32).min(width),
                    ((f64::from(height) * scale).round() as u32).min(height),
                );
                let session = Session::new(source.clone());
                let started = Instant::now();
                let estimate = Mutex::new(None);
                let cutting_out = Mutex::new(None);
                let record = |progress: Progress| {
                    estimate.lock().unwrap().get_or_insert(progress.remaining);
                    if progress.stage == Stage::CuttingOut {
                        cutting_out.lock().unwrap().get_or_insert(started.elapsed());
                    }
                };
                let resized = session.resize(size.0, size.1, options(40), &cancel, &record)?;
                let total = started.elapsed().as_secs_f64();
                let cutting_out = cutting_out.into_inner().unwrap();
                eprintln!(
                    "{name} at {}×{}: {total:.2} s, initial estimate {}, cutout stage {}",
                    size.0,
                    size.1,
                    estimate
                        .into_inner()
                        .unwrap()
                        .map_or("none (cache hit)".into(), |estimate| {
                            format!("{:.2} s", estimate.as_secs_f64())
                        }),
                    cutting_out.map_or("none".into(), |at| format!(
                        "from {:.2} s: {:.2} s",
                        at.as_secs_f64(),
                        total - at.as_secs_f64()
                    ))
                );
                resized.save(output.join(format!("{name}.png")))?;
                session
                    .line_art(size.0, size.1, options(40), &cancel, &|_| {})?
                    .save(output.join(format!("{name}-line-art.png")))?;
                let layers = crate::tools::line_art::generate(&source, size, &cancel, &|_| {
                    panic!("the layers are cached")
                })?;
                layers.fill.save(output.join(format!("{name}-fill.png")))?;
                layers
                    .alpha
                    .save(output.join(format!("{name}-alpha.png")))?;
                Ok(())
            });
        crate::tools::line_art::keep_real_worker_warm(false);
        result
    }

    #[test]
    #[ignore = "requires local line-art and BiRefNet inference models"]
    fn line_art_app_wizard_capture() -> Result<()> {
        let input = env::var_os("DIORAMA_LINE_ART_SMOKE_INPUT")
            .map(PathBuf::from)
            .expect("set DIORAMA_LINE_ART_SMOKE_INPUT to a wizard PNG");
        let output = env::var_os("DIORAMA_LINE_ART_APP_OUT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp/diorama-line-art-app-v2"));
        fs::create_dir_all(&output)?;

        let sides = env::var("DIORAMA_LINE_ART_SMOKE_SIDES").map_or_else(
            |_| vec![128, 256, 512],
            |sides| {
                sides
                    .split(',')
                    .map(|side| side.trim().parse().expect("sides are integers"))
                    .collect()
            },
        );
        let source = Arc::new(image::open(&input)?.into_rgba8());
        let largest = sides.iter().copied().max().unwrap_or(0);
        assert!(
            source.width() >= largest && source.height() >= largest,
            "the smoke input must support {largest}px targets"
        );
        let cancel = CancellationToken::default();
        let session = Session::new(source.clone());
        for side in sides {
            let started = Instant::now();
            let trace = Mutex::new(Vec::new());
            let record = |progress: Progress| {
                trace.lock().unwrap().push((started.elapsed(), progress));
            };
            // The first request generates; the others reuse the pair.
            let line_art = session.line_art(side, side, options(40), &cancel, &record)?;
            let generated = started.elapsed();
            let trace = trace.into_inner().unwrap();
            let estimate = trace.first().map(|(_, progress)| progress.remaining);
            eprintln!(
                "{side}²: generated in {:.2} s, initial estimate {}",
                generated.as_secs_f64(),
                estimate.map_or("none (cache hit)".into(), |estimate| format!(
                    "{:.2} s",
                    estimate.as_secs_f64()
                ))
            );
            for (at, progress) in &trace {
                eprintln!(
                    "  {:6.2} s  {:5.1}%  ~{:.1} s left",
                    at.as_secs_f64(),
                    progress.fraction * 100.,
                    progress.remaining.as_secs_f64()
                );
            }
            line_art.save(output.join(format!("line-art-{side}-strength40.png")))?;
            let layers = crate::tools::line_art::generate(&source, (side, side), &cancel, &|_| {
                panic!("the layers are cached")
            })?;
            layers
                .line_art
                .save(output.join(format!("raw-line-art-{side}.png")))?;
            layers.fill.save(output.join(format!("fill-{side}.png")))?;
            layers
                .alpha
                .save(output.join(format!("alpha-{side}.png")))?;
            for strength in [0, 40, 100] {
                let options = options(strength);
                session
                    .line_art(side, side, options, &cancel, &|_| panic!("no regeneration"))?
                    .save(output.join(format!("line-art-{side}-strength{strength}.png")))?;
                session
                    .resize(side, side, options, &cancel, &|_| panic!("no regeneration"))?
                    .save(output.join(format!("colored-{side}-strength{strength}.png")))?;
            }
        }
        eprintln!("Line-art app capture written to {}", output.display());
        Ok(())
    }
}
