//! Runtime estimate of one FLUX.2 [klein] 9B generation job.
//!
//! Measured on an AMD Radeon 8060S (ROCm) with the Q4_K_M transformer, one
//! denoising step, guidance 1, the reference at the output size, per image
//! in a warm resident worker:
//!
//! | output   | 512² | 768² | 1024²  |
//! |----------|------|------|--------|
//! | s/image  | 4.55 | 8.95 | ~17.5  |
//!
//! 512² and 768² are the second jobs at that size (4.5 and 4.55 s per image
//! in two runs at 512²); 1024² is from a run the memory watchdog stopped
//! during its last decode, with that decode estimated. The quadratic
//! `2.054 + 7.784·MP + 6.625·MP²` (MP = megapixels of the output) passes
//! through all three points. A job's first image at a new size also warms
//! up. A worker that is not running yet adds its start-up (Python and
//! library imports) and the model load; a cold 512² job took 17.35 s in all
//! against 17.1 s predicted. Prompts without cached embeddings are encoded
//! before, in a process of its own: start-up, loading the text encoder on
//! the CPU, and each prompt.
//!
//! Machines differ, so the estimate is scaled by a persisted calibration
//! factor: the smoothed ratio of measured to predicted run times.
use std::{fs, path::Path, time::Duration};

/// Seconds per image: `a + b·MP + c·MP²`.
const IMAGE_SECONDS: [f64; 3] = [2.054, 7.784, 6.625];
/// Images per job: line art, then the fill.
pub(super) const IMAGES: u32 = 2;
/// Python, PyTorch and diffusers imports until a worker's first event.
pub(super) const STARTUP_SECONDS: f64 = 4.2;
/// GGUF transformer and VAE load, with the GGUF in the page cache (2.3–2.5
/// s; 6.8 s when it was not).
pub(super) const LOAD_SECONDS: f64 = 2.3;
/// Extra time of a job's first image at a new size: kernel selection for
/// its shapes (0.9–1.9 s).
pub(super) const WARMUP_SECONDS: f64 = 1.5;
/// Loading the truncated text encoder on the CPU, and encoding one prompt.
pub(super) const ENCODER_LOAD_SECONDS: f64 = 5.;
pub(super) const ENCODE_SECONDS_PER_PROMPT: f64 = 1.;

const CALIBRATION_FILE: &str = "timing-calibration";
const CALIBRATION_SCHEMA: &str = "diorama-line-art-timing-v1";
/// Weight of the newest run in the smoothed calibration factor.
const CALIBRATION_WEIGHT: f64 = 0.3;
/// One outlier run (e.g. a busy GPU) must not wreck later estimates.
const CALIBRATION_RANGE: (f64, f64) = (0.25, 4.);

/// Uncalibrated seconds to generate one image at `size`, warm.
pub(super) fn image_seconds((width, height): (u32, u32)) -> f64 {
    let megapixels = f64::from(width) * f64::from(height) / 1e6;
    let [a, b, c] = IMAGE_SECONDS;
    a + b * megapixels + c * megapixels * megapixels
}

/// Seconds the encoding process spends, after its start-up, on `prompts`
/// uncached prompts.
pub(super) fn encode_seconds(prompts: u32) -> f64 {
    if prompts == 0 {
        0.
    } else {
        ENCODER_LOAD_SECONDS + ENCODE_SECONDS_PER_PROMPT * f64::from(prompts)
    }
}

/// Seconds until a resident worker started now is ready for jobs.
pub(super) fn cold_start_seconds() -> f64 {
    STARTUP_SECONDS + LOAD_SECONDS
}

/// Uncalibrated duration of a job generating both images at `size`, of
/// which `prompts_to_encode` prompts have no cached embeddings, with the
/// resident worker ready after `start_up` seconds (0 when it is warm,
/// [`cold_start_seconds`] when it must be started). `warm_up` is whether the
/// worker has not generated at this size yet.
pub(super) fn estimate(
    size: (u32, u32),
    prompts_to_encode: u32,
    start_up: f64,
    warm_up: bool,
) -> Duration {
    // Encoding is a process of its own, with its own start-up.
    let encoding = if prompts_to_encode == 0 {
        0.
    } else {
        STARTUP_SECONDS + encode_seconds(prompts_to_encode)
    };
    Duration::from_secs_f64(
        encoding
            + start_up.max(0.)
            + if warm_up { WARMUP_SECONDS } else { 0. }
            + f64::from(IMAGES) * image_seconds(size),
    )
}

/// Measured ÷ predicted run time on this machine, persisted in the line-art
/// cache directory. A missing or corrupt file is the neutral factor 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Calibration(f64);

impl Default for Calibration {
    fn default() -> Self {
        Self(1.)
    }
}

impl Calibration {
    pub(super) fn factor(self) -> f64 {
        self.0
    }

    pub(super) fn load(cache: &Path) -> Self {
        fs::read_to_string(cache.join(CALIBRATION_FILE))
            .ok()
            .and_then(|contents| {
                let (schema, factor) = contents.trim().split_once(' ')?;
                let factor = factor.parse::<f64>().ok()?;
                (schema == CALIBRATION_SCHEMA
                    && (CALIBRATION_RANGE.0..=CALIBRATION_RANGE.1).contains(&factor))
                .then_some(Self(factor))
            })
            .unwrap_or_default()
    }

    /// This factor after one run that took `measured` against an uncalibrated
    /// `predicted`.
    pub(super) fn updated(self, measured: Duration, predicted: Duration) -> Self {
        if predicted.is_zero() {
            return self;
        }
        let ratio = (measured.as_secs_f64() / predicted.as_secs_f64())
            .clamp(CALIBRATION_RANGE.0, CALIBRATION_RANGE.1);
        Self(self.0 + CALIBRATION_WEIGHT * (ratio - self.0))
    }

    /// Failures only log: the calibration is a convenience.
    pub(super) fn store(self, cache: &Path) {
        let result = (|| -> std::io::Result<()> {
            fs::create_dir_all(cache)?;
            let temporary = tempfile::Builder::new()
                .prefix(".timing-")
                .tempfile_in(cache)?;
            fs::write(
                temporary.path(),
                format!("{CALIBRATION_SCHEMA} {:.4}\n", self.0),
            )?;
            temporary
                .persist(cache.join(CALIBRATION_FILE))
                .map_err(|error| error.error)?;
            Ok(())
        })();
        if let Err(error) = result {
            tracing::debug!(%error, "Could not store the line-art timing calibration");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEASURED: [(u32, f64); 3] = [(512, 4.55), (768, 8.95), (1024, 17.5)];

    #[test]
    fn the_image_fit_matches_the_measurements() {
        for (side, seconds) in MEASURED {
            let fitted = image_seconds((side, side));
            assert!(
                (fitted - seconds).abs() < 0.01,
                "{side}²: fitted {fitted:.3} s, measured {seconds} s"
            );
        }
    }

    #[test]
    fn a_job_adds_warm_up_start_up_and_an_optional_encoding_process() {
        for size in [(512, 512), (512, 2048), (1024, 1024)] {
            let warm = WARMUP_SECONDS + 2. * image_seconds(size);
            assert!((estimate(size, 0, 0., true).as_secs_f64() - warm).abs() < 1e-6);
            let cold = estimate(size, 0, cold_start_seconds(), true).as_secs_f64();
            assert!((cold - warm - STARTUP_SECONDS - LOAD_SECONDS).abs() < 1e-6);
            // A worker already loading only adds the rest of its start-up.
            assert!((estimate(size, 0, 1.5, true).as_secs_f64() - warm - 1.5).abs() < 1e-6);
            assert_eq!(estimate(size, 0, -3., true), estimate(size, 0, 0., true));
            let encoding = STARTUP_SECONDS + ENCODER_LOAD_SECONDS;
            for prompts in [1, 2] {
                assert!(
                    (estimate(size, prompts, 0., true).as_secs_f64()
                        - warm
                        - encoding
                        - f64::from(prompts) * ENCODE_SECONDS_PER_PROMPT)
                        .abs()
                        < 1e-6,
                    "{size:?}"
                );
            }
        }
        // A worker that generated at this size already skips the warm-up.
        assert!(
            (estimate((512, 512), 0, 0., false).as_secs_f64() - 2. * image_seconds((512, 512)))
                .abs()
                < 1e-6
        );
        // 512²: a cold job measured 17.35 s, one at the same size in a
        // warm worker 9.05 s.
        assert!(
            (estimate((512, 512), 0, cold_start_seconds(), true).as_secs_f64() - 17.1).abs() < 0.05
        );
        assert!((estimate((512, 512), 0, 0., false).as_secs_f64() - 9.1).abs() < 0.05);
    }

    #[test]
    fn the_estimate_grows_with_the_area_and_the_prompts_to_encode() {
        let mut previous = Duration::ZERO;
        for side in (16..=1024).step_by(16) {
            let current = estimate((side, side), 0, 0., true);
            assert!(current > previous, "{side}");
            previous = current;
        }
        assert!(estimate((1024, 512), 0, 0., true) > estimate((512, 512), 0, 0., true));
        assert!(estimate((512, 512), 1, 0., true) > estimate((512, 512), 0, 0., true));
        assert!(estimate((512, 512), 2, 0., true) > estimate((512, 512), 1, 0., true));
    }

    #[test]
    fn calibration_persists_smoothly_and_tolerates_corrupt_files() {
        let cache = tempfile::tempdir().unwrap();
        let path = cache.path().join(CALIBRATION_FILE);
        assert_eq!(Calibration::load(cache.path()), Calibration::default());
        let slower =
            Calibration::default().updated(Duration::from_secs(20), Duration::from_secs(10));
        assert!((slower.factor() - 1.3).abs() < 1e-9);
        slower.store(cache.path());
        assert!((Calibration::load(cache.path()).factor() - 1.3).abs() < 1e-9);
        // One extreme run moves the factor by at most 30% of the clamped ratio.
        let extreme = slower.updated(Duration::from_secs(1000), Duration::from_secs(1));
        assert!((extreme.factor() - (1.3 + 0.3 * (4. - 1.3))).abs() < 1e-9);
        assert_eq!(
            slower.updated(Duration::from_secs(1), Duration::ZERO),
            slower
        );
        for corrupt in [
            "",
            "garbage",
            "diorama-line-art-timing-v1 NaN",
            "diorama-line-art-timing-v1 inf",
            "diorama-line-art-timing-v1 -1",
            "diorama-line-art-timing-v1 99",
            "other-schema 1.5",
            "diorama-line-art-timing-v1",
        ] {
            fs::write(&path, corrupt).unwrap();
            assert_eq!(
                Calibration::load(cache.path()),
                Calibration::default(),
                "{corrupt:?}"
            );
        }
        fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(Calibration::load(cache.path()), Calibration::default());
        // An unwritable location only logs.
        Calibration(2.).store(&path.join("not a directory"));
    }
}
