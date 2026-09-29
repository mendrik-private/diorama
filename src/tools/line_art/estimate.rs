//! Runtime estimate of one FLUX.2 [klein] 9B generation.
//!
//! Measured on an AMD Radeon 8060S (ROCm) with the Q4_K_M transformer,
//! 4 steps, guidance 1, the reference at the output size, per image in a warm
//! process (the second image of a run):
//!
//! | output   | 512² | 640² | 768² | 1024² |
//! |----------|------|------|------|-------|
//! | s/image  | 19.6 | 27.3 | 47.4 | 86.6  |
//!
//! 512² and 768² are the means of two runs (19.5/19.7 and 51.4/43.5). The
//! least-squares fit `68.182·MP + 14.031·MP²` (MP = megapixels of the output,
//! no constant term, so it stays positive) has an RMS error of 1.9 s and at
//! most 10.9% relative error (640²). A generation process adds start-up
//! (Python and library imports), the model load, a one-time warm-up in the
//! first image and process exit. Prompts without cached embeddings are
//! encoded before, in a process of its own: start-up, loading the text
//! encoder on the CPU, and each prompt.
//!
//! Machines differ, so the estimate is scaled by a persisted calibration
//! factor: the smoothed ratio of measured to predicted run times.
use std::{fs, path::Path, time::Duration};

/// Seconds per image: `a + b·MP + c·MP²`.
const IMAGE_SECONDS: [f64; 3] = [0., 68.182, 14.031];
/// Images per run: line art, then the fill.
pub(super) const IMAGES: u32 = 2;
/// Python, PyTorch and diffusers imports until a worker's first event.
pub(super) const STARTUP_SECONDS: f64 = 4.2;
/// GGUF transformer and VAE load, with the GGUF in the page cache.
pub(super) const LOAD_SECONDS: f64 = 2.;
/// Extra time of the first image: kernel selection for the new shapes.
pub(super) const WARMUP_SECONDS: f64 = 1.7;
/// From the worker's last event until the process has exited.
pub(super) const EXIT_SECONDS: f64 = 0.2;
/// Loading the truncated text encoder on the CPU, and encoding one prompt.
/// Two prompts took 6.0–10.0 s after start-up, one prompt 7.2 s.
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

/// Uncalibrated duration of a whole worker run generating both images at
/// `size`, of which `prompts_to_encode` prompts have no cached embeddings.
pub(super) fn estimate(size: (u32, u32), prompts_to_encode: u32) -> Duration {
    // Encoding is a process of its own, with its own start-up.
    let encoding = if prompts_to_encode == 0 {
        0.
    } else {
        STARTUP_SECONDS + encode_seconds(prompts_to_encode)
    };
    Duration::from_secs_f64(
        encoding
            + STARTUP_SECONDS
            + LOAD_SECONDS
            + WARMUP_SECONDS
            + f64::from(IMAGES) * image_seconds(size)
            + EXIT_SECONDS,
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

    const MEASURED: [(u32, f64); 4] = [(512, 19.6), (640, 27.3), (768, 47.4), (1024, 86.6)];

    #[test]
    fn the_image_fit_matches_the_measurements() {
        let mut squared_error = 0.;
        for (side, seconds) in MEASURED {
            let fitted = image_seconds((side, side));
            assert!(
                (fitted - seconds).abs() / seconds < 0.11,
                "{side}²: fitted {fitted:.2} s, measured {seconds} s"
            );
            squared_error += (fitted - seconds).powi(2);
        }
        let rms = (squared_error / MEASURED.len() as f64).sqrt();
        assert!(rms < 2., "RMS error {rms:.3} s");
    }

    #[test]
    fn a_run_adds_overhead_two_images_and_an_optional_encoding_process() {
        let overhead = STARTUP_SECONDS + LOAD_SECONDS + WARMUP_SECONDS + EXIT_SECONDS;
        for size in [(512, 512), (512, 2048), (1024, 1024)] {
            let expected = overhead + 2. * image_seconds(size);
            assert!((estimate(size, 0).as_secs_f64() - expected).abs() < 1e-6);
            let encoding = STARTUP_SECONDS + ENCODER_LOAD_SECONDS;
            for prompts in [1, 2] {
                assert!(
                    (estimate(size, prompts).as_secs_f64()
                        - expected
                        - encoding
                        - f64::from(prompts) * ENCODE_SECONDS_PER_PROMPT)
                        .abs()
                        < 1e-6,
                    "{size:?}"
                );
            }
        }
        // 512²: 8.1 s of overhead and 2 × 18.84 s.
        assert!((estimate((512, 512), 0).as_secs_f64() - 45.78).abs() < 0.01);
    }

    #[test]
    fn the_estimate_grows_with_the_area_and_the_prompts_to_encode() {
        let mut previous = Duration::ZERO;
        for side in (16..=1024).step_by(16) {
            let current = estimate((side, side), 0);
            assert!(current > previous, "{side}");
            previous = current;
        }
        assert!(estimate((1024, 512), 0) > estimate((512, 512), 0));
        assert!(estimate((512, 512), 1) > estimate((512, 512), 0));
        assert!(estimate((512, 512), 2) > estimate((512, 512), 1));
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
