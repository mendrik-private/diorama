//! Local FLUX.2 [klein] 9B generation of the Game Asset layers.
//!
//! A Python worker runs the Q4_K_M GGUF transformer on the GPU, offline, from
//! files the user installed with `build-aux/setup-line-art.py`. Prompts are
//! encoded once, on the CPU, in a separate worker process, and their
//! embeddings are cached next to the model. For a target
//! size it generates line art and a de-inked fill, one after the other on one
//! model load, at the target scaled up to at least 512 pixels per side and
//! rounded up to multiples of 16. Both are center-cropped to the scaled
//! target and, if it was scaled up, reduced to the target. Results are cached on disk by everything that
//! determines them, and rejected when the line art is solid or does not line
//! up with the reference. The worker streams progress, which drives a live
//! runtime estimate.
mod estimate;
mod progress;
mod resident;

use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime},
};

use image::{GrayImage, ImageBuffer, Pixel, RgbImage, Rgba, RgbaImage, imageops::FilterType};
use sha2::{Digest, Sha256};

use crate::{
    document::CancellationToken,
    error::{AppError, Result},
    tools::worker_process::{self, InferenceGate, Launch, RuntimeConfiguration, StdoutLines},
};
use estimate::Calibration;
pub use progress::Progress;
use progress::{Tracker, WorkerEvent};
use resident::{Job, RESIDENCY, Residency};

const WORKER: &str = include_str!("line_art_worker.py");
const MODEL_DIRECTORY: &str = "diorama/flux2-klein-9b";
/// Written by setup only after a complete download of `RECIPE.revision`.
const REVISION_MARKER: &str = ".diorama-revision";
const GGUF_DIRECTORY: &str = "diorama/flux2-klein-9b-gguf";
const GGUF_FILE: &str = "flux-2-klein-9b-Q4_K_M.gguf";
/// `unsloth/FLUX.2-klein-9B-GGUF` revision
/// `fde8634245fe6b749a221c25b34672b5b8fbd079`.
const GGUF_SIZE: u64 = 5_909_829_920;
/// Written by setup next to the GGUF once its SHA-256 matched
/// `RECIPE.gguf_sha256`, so a launch does not hash 5.9 GB.
const GGUF_MARKER: &str = "flux-2-klein-9b-Q4_K_M.gguf.diorama-sha256";
const RUNTIME_CONFIG: &str = "line-art-runtime.conf";
/// Accepted pairs kept on disk; older ones are evicted least recently used.
const MAX_CACHED_PAIRS: usize = 64;
const LINE_ART_SUFFIX: &str = "-line-art.png";
const FILL_SUFFIX: &str = "-fill.png";
const SETUP: &str = "python3 build-aux/setup-line-art.py";
/// FLUX.2 works on 16-pixel latent patches.
const MULTIPLE: u32 = 16;
/// Smaller targets are generated scaled up to this shorter side, where FLUX
/// draws clean lines, and reduced afterwards.
const MIN_GENERATION_SIDE: u32 = 512;
/// The FLUX.2 image processor rejects references with an aspect ratio over
/// 8:1.
const MAX_ASPECT: u32 = 8;
/// FLUX.2 is tuned for about 1 Mpx; larger references are reduced by the
/// pipeline itself.
const MAX_AREA: u32 = 1024 * 1024;
/// A first run also encodes the prompts; either stage can take minutes.
const TIMEOUT: Duration = Duration::from_secs(20 * 60);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Between worker events, the estimate is re-reported at this interval.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// How long to wait for the rest of the worker's output after it exited.
const OUTPUT_GRACE: Duration = Duration::from_secs(1);

/// Pixels darker than this are line ink.
const INK: u8 = 128;
/// Below this, line art is too sparse to measure alignment. A flat source
/// legitimately yields (almost) no lines, so sparse output is accepted.
const MIN_MEASURABLE_INK: f64 = 0.001;
const MAX_INK_FRACTION: f64 = 0.6;
/// Squared Sobel magnitude (per RGB channel, 3x3 kernel) of a source edge.
const EDGE_MAGNITUDE_SQUARED: i32 = 120 * 120;
/// When displaced line art still misses almost no edge, the source is too
/// busy for the alignment test to tell anything.
const MIN_DISPLACED_MISS: f64 = 0.02;
/// Aligned line art misses source edges far less often than the same line
/// art displaced by about 2% of the image. See `check_line_art`.
const MAX_MISS_RATIO: f64 = 0.35;

/// Everything besides the reference that determines the generated pair.
#[derive(Debug, Clone, Copy)]
struct Recipe {
    /// `black-forest-labs/FLUX.2-klein-9B` revision.
    revision: &'static str,
    gguf_sha256: &'static str,
    line_art_prompt: &'static str,
    line_art_seed: u64,
    fill_prompt: &'static str,
    fill_seed: u64,
    /// The text encoder's hidden layers FLUX.2 [klein] stacks, and the
    /// prompt length it encodes.
    text_encoder_layers: &'static str,
    max_sequence_length: u32,
    steps: u32,
    guidance: &'static str,
}

const RECIPE: Recipe = Recipe {
    revision: "92196c8e11f7b6cf2b7493e037d8c5345c559216",
    gguf_sha256: "5489463ed96056b0bb5472abb5d1bba7055e48d574e37877acb43b407465e26f",
    line_art_prompt: "convert this to line-art, remove thin lines",
    line_art_seed: 0,
    fill_prompt: "remove the black outlines, change nothing else",
    fill_seed: 0,
    text_encoder_layers: "9,18,27",
    max_sequence_length: 512,
    steps: 1,
    guidance: "1.0",
};

/// Target-sized, aligned Game Asset layers: grayscale line art (white is no
/// ink) and an opaque fill without ink contours.
#[derive(Debug, Clone, PartialEq)]
pub struct LineArtPair {
    pub line_art: GrayImage,
    pub fill: RgbImage,
}

struct Runtime {
    python: PathBuf,
    model: PathBuf,
    gguf: PathBuf,
    device: Option<String>,
    launch: Launch,
    host_library_path: Option<String>,
    timeout: Duration,
    cancel_grace: Duration,
    idle_timeout: Duration,
    inference: std::sync::Arc<InferenceGate>,
}

/// Generate the line art and fill for `target` (at most the source size).
///
/// The white-composited source is resized with Lanczos to the generation
/// size of [`plan`]; both generated images are center-cropped to its crop
/// size and, when that is larger than `target`, reduced to `target`: the
/// line art with bicubic, the fill with Lanczos. `progress` is called on this
/// thread while the worker runs, never for a cache hit.
pub fn generate(
    source: &RgbaImage,
    target: (u32, u32),
    cancellation: &CancellationToken,
    progress: &dyn Fn(Progress),
) -> Result<LineArtPair> {
    cancellation.check()?;
    generate_in(
        source,
        target,
        cancellation,
        &cache_directory()?,
        Runtime::from_environment,
        &RESIDENCY,
        progress,
    )
}

/// Keep the generation worker loaded (`true`) while Game Asset work is
/// expected, or unload it (`false`). Returns at once: starting and stopping
/// happen in the background, and a worker that cannot start only logs,
/// since the error surfaces when generating.
pub fn set_warm(warm: bool) {
    #[cfg(not(test))]
    let start = || Ok((Runtime::from_environment()?, cache_directory()?));
    // Tests never load the real model in the background; one that wants a
    // resident worker installs a fake one.
    #[cfg(test)]
    let start = fake_worker::runtime;
    RESIDENCY.set_warm(warm, start);
}

/// Keep the real worker loaded between jobs (`true`), or stop it; for the
/// opt-in tests with the real model, since [`set_warm`] never starts it in
/// tests.
#[cfg(test)]
pub(crate) fn keep_real_worker_warm(warm: bool) {
    RESIDENCY.set_keep_warm(warm);
    if !warm {
        RESIDENCY.reconcile(|| unreachable!("stopping resolves no runtime"));
    }
}

/// A fake worker installation for tests of the resident worker's
/// lifecycle.
#[cfg(test)]
pub(crate) mod fake_worker {
    use super::*;
    use std::sync::Mutex;

    static ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);

    /// Install `script` as the interpreter, with a verified fake model, in
    /// `root`, and make [`set_warm`] start it.
    pub(crate) fn install(root: &Path, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        let python = root.join("python");
        fs::write(&python, script).unwrap();
        fs::set_permissions(&python, fs::Permissions::from_mode(0o755)).unwrap();
        let model = root.join("model");
        fs::create_dir_all(&model).unwrap();
        fs::write(model.join("model_index.json"), "{}").unwrap();
        fs::write(model.join(REVISION_MARKER), RECIPE.revision).unwrap();
        let gguf = root.join(GGUF_FILE);
        File::create(&gguf).unwrap().set_len(GGUF_SIZE).unwrap();
        fs::write(gguf.with_file_name(GGUF_MARKER), RECIPE.gguf_sha256).unwrap();
        *ROOT.lock().unwrap() = Some(root.to_owned());
    }

    pub(super) fn runtime() -> Result<(Runtime, PathBuf)> {
        let root = ROOT
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| AppError::SketchGeneration("no fake worker installed".into()))?;
        Ok((
            Runtime {
                python: root.join("python"),
                model: root.join("model"),
                gguf: root.join(GGUF_FILE),
                device: None,
                launch: Launch::Direct,
                host_library_path: None,
                timeout: TIMEOUT,
                cancel_grace: resident::CANCEL_GRACE,
                idle_timeout: resident::IDLE_TIMEOUT,
                inference: std::sync::Arc::default(),
            },
            root.join("cache"),
        ))
    }
}

fn cache_directory() -> Result<PathBuf> {
    Ok(worker_process::cache_home()
        .ok_or_else(|| {
            AppError::SketchGeneration(
                "HOME is unavailable; cannot locate the line-art cache".into(),
            )
        })?
        .join("diorama/line-art"))
}

impl Runtime {
    fn from_environment() -> Result<Self> {
        let launch = Launch::detect();
        let config = launch.runtime_config_path(RUNTIME_CONFIG);
        let configuration = RuntimeConfiguration::read(config.as_deref());
        let python = std::env::var_os("DIORAMA_LINE_ART_PYTHON")
            .map(PathBuf::from)
            .or(configuration.python)
            .ok_or_else(|| {
                AppError::SketchGeneration(format!(
                    "Local line art is not set up: {} records no Python runtime. Run {SETUP} or set DIORAMA_LINE_ART_PYTHON",
                    config.map_or_else(
                        || RUNTIME_CONFIG.to_owned(),
                        |path| path.display().to_string()
                    ),
                ))
            })?;
        let install =
            |variable: &str, relative: &Path, present: fn(&Path) -> bool| -> Result<PathBuf> {
                if let Some(path) = std::env::var_os(variable) {
                    return Ok(PathBuf::from(path));
                }
                let cache = worker_process::cache_home().ok_or_else(|| {
                    AppError::SketchGeneration(format!("HOME is unavailable; set {variable}"))
                })?;
                Ok(worker_process::app_or_host_install(
                    &cache,
                    worker_process::host_cache_home().as_deref(),
                    relative,
                    present,
                ))
            };
        let model = install(
            "DIORAMA_LINE_ART_MODEL",
            Path::new(MODEL_DIRECTORY),
            |model| model.join("model_index.json").is_file(),
        )?;
        let gguf = install(
            "DIORAMA_LINE_ART_GGUF",
            &Path::new(GGUF_DIRECTORY).join(GGUF_FILE),
            Path::is_file,
        )?;
        Ok(Self {
            python,
            model,
            gguf,
            device: std::env::var("DIORAMA_LINE_ART_DEVICE")
                .ok()
                .filter(|device| !device.is_empty()),
            launch,
            host_library_path: configuration.library_path,
            timeout: TIMEOUT,
            cancel_grace: resident::CANCEL_GRACE,
            idle_timeout: resident::IDLE_TIMEOUT,
            inference: worker_process::shared_inference_gate(),
        })
    }
}

/// `cache` holds accepted pairs, the timing calibration and the worker's
/// host-visible scratch directories. The runtime is resolved only on a cache
/// miss.
fn generate_in(
    source: &RgbaImage,
    target: (u32, u32),
    cancellation: &CancellationToken,
    cache: &Path,
    runtime: impl FnOnce() -> Result<Runtime>,
    residency: &Residency,
    progress: &dyn Fn(Progress),
) -> Result<LineArtPair> {
    let plan = plan(target)?;
    let size = plan.generation;
    if source.width() == 0 || source.height() == 0 {
        return Err(AppError::InvalidDimensions);
    }
    let reference = reference(source, size, cancellation)?;
    let key = cache_key(&RECIPE, &reference);
    let generated = if let Some(cached) = read_cached(cache, &key, size) {
        if let Err(error) = check_line_art(&cached.line_art, &reference, cancellation) {
            // Only accepted results belong in the cache; a cancelled check
            // says nothing about the entry.
            if matches!(error, AppError::SketchGeneration(_)) {
                remove_cached(cache, &key);
            }
            return Err(error);
        }
        touch_cached(cache, &key);
        cached
    } else {
        let generated = run_worker(
            &reference,
            cancellation,
            &runtime()?,
            cache,
            residency,
            progress,
        )?;
        check_line_art(&generated.line_art, &reference, cancellation)?;
        cancellation.check()?;
        match store_cached(cache, &key, &generated) {
            Ok(()) => evict_cached(cache, MAX_CACHED_PAIRS),
            Err(error) => {
                tracing::warn!(%error, cache = %cache.display(), "Could not cache line art");
            }
        }
        generated
    };
    let line_art = center_crop(&generated.line_art, plan.crop);
    let fill = center_crop(&generated.fill, plan.crop);
    if plan.crop == target {
        return Ok(LineArtPair { line_art, fill });
    }
    let pair = LineArtPair {
        line_art: image::imageops::resize(&line_art, target.0, target.1, FilterType::CatmullRom),
        fill: image::imageops::resize(&fill, target.0, target.1, FilterType::Lanczos3),
    };
    cancellation.check()?;
    Ok(pair)
}

/// `side` rounded up to a multiple of 16, at least 16.
fn ceil16(side: u32) -> u32 {
    side.max(1).next_multiple_of(MULTIPLE)
}

/// How one target is generated: at `generation`, center-cropped to `crop`,
/// which is the target scaled by the same factor on both axes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Plan {
    generation: (u32, u32),
    crop: (u32, u32),
}

/// The target scaled up so that its shorter side is `MIN_GENERATION_SIDE`
/// (never down), with each side rounded up to a multiple of 16 for
/// generation. If that exceeds `MAX_AREA`, the scale is lowered until it
/// fits, down to the target itself.
fn plan((width, height): (u32, u32)) -> Result<Plan> {
    if width == 0 || height == 0 {
        return Err(AppError::InvalidDimensions);
    }
    let (w, h) = (f64::from(width), f64::from(height));
    let shorter = w.min(h);
    let crop_at = |scale: f64| {
        (
            ((w * scale).round() as u32).max(1),
            ((h * scale).round() as u32).max(1),
        )
    };
    let area = |(w, h): (u32, u32)| u64::from(w) * u64::from(h);
    let fitting = (f64::from(MAX_AREA) / (w * h)).sqrt();
    let mut scale = (f64::from(MIN_GENERATION_SIDE) / shorter)
        .min(fitting)
        .max(1.);
    let (crop, generation) = loop {
        let crop = crop_at(scale);
        let generation = (ceil16(crop.0), ceil16(crop.1));
        if area(generation) <= u64::from(MAX_AREA) {
            break (crop, generation);
        }
        if scale <= 1. {
            return Err(AppError::SketchGeneration(
                "Game Asset supports targets up to 1 megapixel (1024 × 1024)".into(),
            ));
        }
        // Shrink the shorter side by about one pixel per step.
        scale = (scale - 1. / shorter).max(1.);
    };
    if generation.0.max(generation.1) > MAX_ASPECT * generation.0.min(generation.1) {
        return Err(AppError::SketchGeneration(format!(
            "Game Asset supports aspect ratios up to {MAX_ASPECT}:1"
        )));
    }
    Ok(Plan { generation, crop })
}

/// The top-left corner of the `target` window centered in `size`, rounded
/// down.
fn crop_offset(size: (u32, u32), target: (u32, u32)) -> (u32, u32) {
    ((size.0 - target.0) / 2, (size.1 - target.1) / 2)
}

fn center_crop<P: Pixel + 'static>(
    image: &ImageBuffer<P, Vec<P::Subpixel>>,
    target: (u32, u32),
) -> ImageBuffer<P, Vec<P::Subpixel>> {
    let (x, y) = crop_offset(image.dimensions(), target);
    image::imageops::crop_imm(image, x, y, target.0, target.1).to_image()
}

/// The white-composited source reduced (or, for a slightly larger
/// generation size, enlarged) to `size` with Lanczos.
fn reference(
    source: &RgbaImage,
    size: (u32, u32),
    cancellation: &CancellationToken,
) -> Result<RgbaImage> {
    let opaque = white_composite(source, cancellation)?;
    if opaque.dimensions() == size {
        return Ok(opaque);
    }
    let reference = image::imageops::resize(&opaque, size.0, size.1, FilterType::Lanczos3);
    cancellation.check()?;
    Ok(reference)
}

fn white_composite(image: &RgbaImage, cancellation: &CancellationToken) -> Result<RgbaImage> {
    let mut opaque = RgbaImage::new(image.width(), image.height());
    for (source, target) in image.rows().zip(opaque.rows_mut()) {
        cancellation.check()?;
        for (source, target) in source.zip(target) {
            let [red, green, blue, alpha] = source.0;
            let alpha = u16::from(alpha);
            let composite = |component| {
                ((u16::from(component) * alpha + 255 * (255 - alpha) + 127) / 255) as u8
            };
            *target = Rgba([composite(red), composite(green), composite(blue), 255]);
        }
    }
    Ok(opaque)
}

/// Everything that determines the generated pair; the reference's
/// dimensions are the generation size. Variable-length fields are
/// length-prefixed so adjacent fields cannot alias.
fn cache_key(recipe: &Recipe, reference: &RgbaImage) -> String {
    let mut digest = Sha256::new();
    for field in [
        b"diorama-line-art-pair-v3".as_slice(),
        recipe.revision.as_bytes(),
        recipe.gguf_sha256.as_bytes(),
        recipe.line_art_prompt.as_bytes(),
        recipe.fill_prompt.as_bytes(),
        recipe.text_encoder_layers.as_bytes(),
        Sha256::digest(WORKER.as_bytes()).as_slice(),
        recipe.guidance.as_bytes(),
    ] {
        digest.update((field.len() as u64).to_le_bytes());
        digest.update(field);
    }
    digest.update(recipe.line_art_seed.to_le_bytes());
    digest.update(recipe.fill_seed.to_le_bytes());
    digest.update(recipe.max_sequence_length.to_le_bytes());
    digest.update(recipe.steps.to_le_bytes());
    digest.update(reference.width().to_le_bytes());
    digest.update(reference.height().to_le_bytes());
    digest.update(reference.as_raw());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Where the embeddings of `prompt` are cached: in the model directory, named
/// after the model revision, the prompt and the encoding parameters.
fn prompt_embeds_path(model: &Path, recipe: &Recipe, prompt: &str) -> PathBuf {
    let mut digest = Sha256::new();
    for field in [
        b"diorama-prompt-embeds-v2".as_slice(),
        recipe.revision.as_bytes(),
        prompt.as_bytes(),
        recipe.text_encoder_layers.as_bytes(),
        b"bfloat16",
    ] {
        digest.update((field.len() as u64).to_le_bytes());
        digest.update(field);
    }
    digest.update(recipe.max_sequence_length.to_le_bytes());
    let key = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    model.join(format!(".diorama-prompt-{key}.safetensors"))
}

fn cached_paths(cache: &Path, key: &str) -> [PathBuf; 2] {
    [
        cache.join(format!("{key}{LINE_ART_SUFFIX}")),
        cache.join(format!("{key}{FILL_SUFFIX}")),
    ]
}

fn read_cached(cache: &Path, key: &str, size: (u32, u32)) -> Option<LineArtPair> {
    let [line_art, fill] = cached_paths(cache, key);
    let line_art = image::open(line_art).ok()?.into_luma8();
    let fill = image::open(fill).ok()?.into_rgb8();
    (line_art.dimensions() == size && fill.dimensions() == size)
        .then_some(LineArtPair { line_art, fill })
}

/// Readers never see a partial file: each image is written beside its
/// target, then renamed. The line art goes last, and a lone fill is a miss.
fn store_cached(cache: &Path, key: &str, pair: &LineArtPair) -> Result<()> {
    fs::create_dir_all(cache)?;
    let [line_art, fill] = cached_paths(cache, key);
    let store = |path: &Path, save: &dyn Fn(&Path) -> image::ImageResult<()>| -> Result<()> {
        let temporary = tempfile::Builder::new()
            .prefix(".store-")
            .suffix(".png")
            .tempfile_in(cache)?;
        save(temporary.path())?;
        temporary
            .persist(path)
            .map_err(|error| AppError::Io(error.error))?;
        Ok(())
    };
    store(&fill, &|path| {
        pair.fill.save_with_format(path, image::ImageFormat::Png)
    })?;
    store(&line_art, &|path| {
        pair.line_art
            .save_with_format(path, image::ImageFormat::Png)
    })
}

/// Eviction is least recently used, not least recently made.
fn touch_cached(cache: &Path, key: &str) {
    for path in cached_paths(cache, key) {
        if let Err(error) = File::options()
            .write(true)
            .open(&path)
            .and_then(|file| file.set_modified(SystemTime::now()))
        {
            tracing::debug!(%error, path = %path.display(), "Could not touch cached line art");
        }
    }
}

fn remove_cached(cache: &Path, key: &str) {
    for path in cached_paths(cache, key) {
        let _ = fs::remove_file(path);
    }
}

/// The cache key a file belongs to. Bare `<key>.png` files are results of
/// the earlier single-image format and are evicted like any old pair.
fn cached_key(name: &str) -> Option<&str> {
    let key = name.get(..64)?;
    let suffix = &name[64..];
    (key.bytes().all(|byte| byte.is_ascii_hexdigit())
        && [LINE_ART_SUFFIX, FILL_SUFFIX, ".png"].contains(&suffix))
    .then_some(key)
}

/// Keep the `keep` most recently used pairs. Scratch directories, in-progress
/// `.store-` files and the calibration are never touched; failures only log.
fn evict_cached(cache: &Path, keep: usize) {
    let entries = match fs::read_dir(cache) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::debug!(%error, "Could not list the line-art cache");
            return;
        }
    };
    let mut pairs = std::collections::HashMap::<String, (SystemTime, Vec<PathBuf>)>::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(key) = name.to_str().and_then(cached_key) else {
            continue;
        };
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let pair = pairs
            .entry(key.to_owned())
            .or_insert((SystemTime::UNIX_EPOCH, Vec::new()));
        pair.0 = pair.0.max(modified);
        pair.1.push(entry.path());
    }
    if pairs.len() <= keep {
        return;
    }
    let mut pairs = pairs.into_values().collect::<Vec<_>>();
    pairs.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    for path in pairs[keep..].iter().flat_map(|(_, paths)| paths) {
        if let Err(error) = fs::remove_file(path) {
            tracing::debug!(%error, path = %path.display(), "Could not evict cached line art");
        }
    }
}

fn verify_model(model: &Path) -> Result<()> {
    if !model.join("model_index.json").is_file() {
        return Err(AppError::SketchGeneration(format!(
            "The FLUX.2 [klein] 9B pipeline is missing at {}. Run {SETUP} to download it (or set DIORAMA_LINE_ART_MODEL)",
            model.display()
        )));
    }
    let marker = fs::read_to_string(model.join(REVISION_MARKER)).unwrap_or_default();
    if marker.trim() != RECIPE.revision {
        return Err(AppError::SketchGeneration(format!(
            "The FLUX.2 [klein] 9B pipeline at {} is incomplete or not revision {}. Run {SETUP} to finish or update it",
            model.display(),
            RECIPE.revision
        )));
    }
    Ok(())
}

/// The GGUF must have the pinned size and carry setup's SHA-256 marker.
fn verify_gguf(gguf: &Path) -> Result<()> {
    let size = fs::metadata(gguf)
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len());
    if size.is_none() {
        return Err(AppError::SketchGeneration(format!(
            "The FLUX.2 [klein] 9B Q4_K_M transformer is missing at {}. Run {SETUP} to download it (or set DIORAMA_LINE_ART_GGUF)",
            gguf.display()
        )));
    }
    let marker = gguf.with_file_name(GGUF_MARKER);
    let verified = fs::read_to_string(&marker).unwrap_or_default();
    if size != Some(GGUF_SIZE) || verified.trim() != RECIPE.gguf_sha256 {
        return Err(AppError::SketchGeneration(format!(
            "The FLUX.2 [klein] 9B Q4_K_M transformer at {} is incomplete or not the pinned file (SHA-256 {}). Run {SETUP} to verify or replace it",
            gguf.display(),
            RECIPE.gguf_sha256
        )));
    }
    Ok(())
}

fn run_worker(
    reference: &RgbaImage,
    cancellation: &CancellationToken,
    runtime: &Runtime,
    cache: &Path,
    residency: &Residency,
    progress: &dyn Fn(Progress),
) -> Result<LineArtPair> {
    verify_model(&runtime.model)?;
    verify_gguf(&runtime.gguf)?;
    // Another window may be running a model that takes minutes; only
    // cancellation ends the wait.
    let _permit = runtime.inference.permit(cancellation)?;
    fs::create_dir_all(cache)?;
    // The app cache is host-visible for Flatpak workers while private /tmp is
    // not. TempDir removes the input, the encoder's files and the outputs.
    let directory = tempfile::Builder::new().prefix(".run-").tempdir_in(cache)?;
    let input = directory.path().join("reference.png");
    let (line_art, fill) = (
        directory.path().join("line-art.png"),
        directory.path().join("fill.png"),
    );
    let line_art_embeds = prompt_embeds_path(&runtime.model, &RECIPE, RECIPE.line_art_prompt);
    let fill_embeds = prompt_embeds_path(&runtime.model, &RECIPE, RECIPE.fill_prompt);
    reference.save(&input)?;
    cancellation.check()?;

    let size = reference.dimensions();
    let calibration = Calibration::load(cache);
    let started = Instant::now();
    let job = Job {
        reference: &input,
        size,
        line_art: &line_art,
        fill: &fill,
        line_art_embeds: &line_art_embeds,
        fill_embeds: &fill_embeds,
    };
    let tracker = residency.run(runtime, cache, &job, cancellation, progress, |start| {
        let missing = [
            (RECIPE.line_art_prompt, &line_art_embeds),
            (RECIPE.fill_prompt, &fill_embeds),
        ]
        .into_iter()
        .filter(|(_, path)| !path.is_file())
        .collect::<Vec<_>>();
        let mut tracker = Tracker::new(
            size,
            RECIPE.steps,
            missing.len() as u32,
            start,
            calibration,
            started,
        );
        progress(tracker.progress(started));
        // The text encoder's memory must be returned before a worker loads
        // the transformer, so encoding is a one-shot process of its own.
        if !missing.is_empty() {
            let worker = directory.path().join("line_art_worker.py");
            fs::write(&worker, WORKER)?;
            let log = directory.path().join("encode.log");
            run_process(
                &mut encode_command(runtime, &worker, &missing),
                &log,
                runtime,
                cancellation,
                &mut tracker,
                progress,
            )?;
            if let Some((_, path)) = missing.iter().find(|(_, path)| !path.is_file()) {
                return Err(AppError::SketchGeneration(format!(
                    "The prompt encoder wrote no embeddings to {}: {}",
                    path.display(),
                    worker_process::log_tail(&log)
                )));
            }
        }
        Ok(tracker)
    })?;
    let read = |path: &Path| {
        let image = image::open(path).map_err(|error| {
            AppError::SketchGeneration(format!(
                "The line-art worker wrote no readable image: {error}"
            ))
        })?;
        if image.width() != size.0 || image.height() != size.1 {
            return Err(AppError::SketchGeneration(format!(
                "The line-art worker returned {}×{} instead of {}×{}",
                image.width(),
                image.height(),
                size.0,
                size.1
            )));
        }
        Ok(image)
    };
    let pair = LineArtPair {
        line_art: read(&line_art)?.into_luma8(),
        fill: read(&fill)?.into_rgb8(),
    };
    let mut tracker = tracker;
    calibration
        .updated(started.elapsed(), tracker.predicted())
        .store(cache);
    progress(tracker.progress(Instant::now()));
    Ok(pair)
}

/// Run one worker process to completion, feeding its progress events to
/// `tracker` and reporting the estimate while it runs.
fn run_process(
    command: &mut Command,
    log: &Path,
    runtime: &Runtime,
    cancellation: &CancellationToken,
    tracker: &mut Tracker,
    progress: &dyn Fn(Progress),
) -> Result<()> {
    let mut child = worker_process::spawn_streaming(command, log).map_err(|error| {
        AppError::SketchGeneration(format!(
            "Could not start the line-art worker with {}: {error}. Run {SETUP} or set DIORAMA_LINE_ART_PYTHON",
            runtime.python.display()
        ))
    })?;
    // The encoder reads no requests.
    drop(child.stdin.take());
    let lines = StdoutLines::take(&mut child).expect("the worker's stdout is piped");
    let mut reported = Instant::now();
    let status = worker_process::wait_with(
        &mut child,
        cancellation,
        runtime.timeout,
        POLL_INTERVAL,
        || {
            let now = Instant::now();
            let mut observed = false;
            for line in lines.pending() {
                observed |= observe(tracker, &line, now);
            }
            if observed || now.duration_since(reported) >= PROGRESS_INTERVAL {
                reported = now;
                progress(tracker.progress(now));
            }
        },
    )
    .map_err(|error| {
        error.into_app_error(AppError::SketchGeneration(format!(
            "Local line art timed out after {} minutes",
            runtime.timeout.as_secs() / 60
        )))
    })?;
    for line in lines.finish(OUTPUT_GRACE) {
        observe(tracker, &line, Instant::now());
    }
    cancellation.check()?;
    if !status.success() {
        return Err(AppError::SketchGeneration(format!(
            "The local line-art model failed ({status}): {}",
            worker_process::log_tail(log)
        )));
    }
    Ok(())
}

/// Feed one stdout line to the tracker; `false` when it is no event.
fn observe(tracker: &mut Tracker, line: &str, now: Instant) -> bool {
    match WorkerEvent::parse(line) {
        Some(event) => {
            tracker.observe(&event, now);
            true
        }
        None => {
            tracing::debug!(
                line,
                "Ignoring a line-art worker line that is no progress event"
            );
            false
        }
    }
}

/// Encode `prompts` into their embedding files, on the CPU.
fn encode_command(runtime: &Runtime, worker: &Path, prompts: &[(&str, &PathBuf)]) -> Command {
    let mut command = runtime
        .launch
        .command(&runtime.python, runtime.host_library_path.as_deref());
    command
        .arg(worker)
        .arg("--encode")
        .arg("--model")
        .arg(&runtime.model)
        .arg("--text-encoder-layers")
        .arg(RECIPE.text_encoder_layers)
        .arg("--max-sequence-length")
        .arg(RECIPE.max_sequence_length.to_string());
    for (prompt, path) in prompts {
        command.arg("--prompt-embeds").arg(prompt).arg(path);
    }
    command
}

/// Source pixels within a few pixels of a colour or luminance edge.
struct EdgeProximity {
    near: Vec<bool>,
    width: usize,
    height: usize,
    /// Displacement used to estimate how often unrelated line art would
    /// still land near an edge.
    displacement: i64,
}

impl EdgeProximity {
    fn of(opaque: &RgbaImage, cancellation: &CancellationToken) -> Result<Self> {
        let (width, height) = (opaque.width() as usize, opaque.height() as usize);
        let longest = width.max(height) as f64;
        let radius = ((longest / 512.).round() as usize).max(2);
        let displacement = ((longest * 0.02).round() as i64).max(4 * radius as i64);
        let edges = sobel_edges(opaque, cancellation)?;
        let near = dilate(&edges, width, height, radius, cancellation)?;
        Ok(Self {
            near,
            width,
            height,
            displacement,
        })
    }

    /// Fraction of ink pixels, displaced by `(dx, dy)`, that land near an
    /// edge, and how many landed inside the image.
    fn hits(
        &self,
        sketch: &GrayImage,
        (dx, dy): (i64, i64),
        cancellation: &CancellationToken,
    ) -> Result<(u64, u64)> {
        let (mut hits, mut inside) = (0, 0);
        for (y, row) in sketch.rows().enumerate() {
            cancellation.check()?;
            let target_y = y as i64 + dy;
            if !(0..self.height as i64).contains(&target_y) {
                continue;
            }
            for (x, pixel) in row.enumerate() {
                let target_x = x as i64 + dx;
                if pixel[0] >= INK || !(0..self.width as i64).contains(&target_x) {
                    continue;
                }
                inside += 1;
                hits += u64::from(self.near[target_y as usize * self.width + target_x as usize]);
            }
        }
        Ok((hits, inside))
    }
}

#[derive(Debug)]
struct Measurements {
    ink_fraction: f64,
    /// Fraction of ink off every source edge.
    miss: f64,
    /// The same for the line art displaced in eight directions, if any ink
    /// stayed inside the image.
    displaced_miss: Option<f64>,
}

fn measure(
    sketch: &GrayImage,
    opaque: &RgbaImage,
    cancellation: &CancellationToken,
) -> Result<Measurements> {
    let total = u64::from(sketch.width()) * u64::from(sketch.height());
    let ink = sketch.pixels().filter(|pixel| pixel[0] < INK).count() as u64;
    let ink_fraction = ink as f64 / total as f64;
    if !(MIN_MEASURABLE_INK..=MAX_INK_FRACTION).contains(&ink_fraction) {
        return Ok(Measurements {
            ink_fraction,
            miss: 1.,
            displaced_miss: None,
        });
    }
    let proximity = EdgeProximity::of(opaque, cancellation)?;
    let (hits, inside) = proximity.hits(sketch, (0, 0), cancellation)?;
    let miss = 1. - hits as f64 / inside as f64;
    let step = proximity.displacement;
    let (mut displaced_hits, mut displaced_inside) = (0, 0);
    for offset in [
        (step, 0),
        (-step, 0),
        (0, step),
        (0, -step),
        (step, step),
        (-step, -step),
        (step, -step),
        (-step, step),
    ] {
        let (hits, inside) = proximity.hits(sketch, offset, cancellation)?;
        displaced_hits += hits;
        displaced_inside += inside;
    }
    Ok(Measurements {
        ink_fraction,
        miss,
        displaced_miss: (displaced_inside > 0)
            .then(|| 1. - displaced_hits as f64 / displaced_inside as f64),
    })
}

/// Reject generated line art that is solid or misaligned with the reference
/// it was generated from. Sparse or empty line art is accepted: a flat source
/// has no lines to draw.
///
/// Alignment compares how often ink misses a reference edge with how often
/// the same ink misses when displaced by about 2% of the image (at least
/// 8 pixels). That ratio adapts to how busy the source is. Edges count within
/// 2 pixels below 1024 pixels. Measured with FLUX.2 line art of a painted
/// wizard (ratio; rejected above 0.35):
///
/// | size | aligned | shifted 1% | 2%        | 3%        | mirrored or rotated |
/// |------|---------|------------|-----------|-----------|---------------------|
/// | 160² | 0.000   | 0.00–0.06  | 0.08–0.18 | 0.40–0.59 | 0.71–1.07           |
/// | 256² | 0.006   | 0.08–0.23  | 0.45–0.75 | 0.78–0.99 | 0.93–1.05           |
/// | 512² | 0.036   | 0.43–0.80  | 0.85–1.07 | 0.93–1.00 | 0.97–1.02           |
///
/// Shifts within the 2-pixel edge radius pass at small sizes; misplaced or
/// mirrored drawings are rejected at every size.
fn check_line_art(
    sketch: &GrayImage,
    opaque: &RgbaImage,
    cancellation: &CancellationToken,
) -> Result<()> {
    let measured = measure(sketch, opaque, cancellation)?;
    let percent = measured.ink_fraction * 100.;
    if measured.ink_fraction > MAX_INK_FRACTION {
        return Err(AppError::SketchGeneration(format!(
            "The line-art model returned a nearly solid image ({percent:.0}% ink). Retry, or check the GPU driver and model install"
        )));
    }
    let Some(displaced_miss) = measured
        .displaced_miss
        .filter(|miss| *miss >= MIN_DISPLACED_MISS)
    else {
        tracing::debug!(
            ?measured,
            "Line-art alignment is not measurable for this source"
        );
        return Ok(());
    };
    if measured.miss / displaced_miss > MAX_MISS_RATIO {
        return Err(AppError::SketchGeneration(format!(
            "The line art does not line up with the source ({:.0}% of lines are off source edges, {:.0}% when displaced), so it was discarded. Retry, or report this image",
            measured.miss * 100.,
            displaced_miss * 100.
        )));
    }
    Ok(())
}

/// Pixels whose 3x3 Sobel gradient exceeds the edge threshold in any RGB
/// channel. Borders replicate the nearest pixel.
fn sobel_edges(opaque: &RgbaImage, cancellation: &CancellationToken) -> Result<Vec<bool>> {
    let (width, height) = (opaque.width(), opaque.height());
    let mut edges = vec![false; width as usize * height as usize];
    let at = |x: i64, y: i64, channel: usize| {
        let x = x.clamp(0, i64::from(width) - 1) as u32;
        let y = y.clamp(0, i64::from(height) - 1) as u32;
        i32::from(opaque.get_pixel(x, y)[channel])
    };
    for y in 0..i64::from(height) {
        cancellation.check()?;
        for x in 0..i64::from(width) {
            edges[y as usize * width as usize + x as usize] = (0..3).any(|channel| {
                let p = |dx, dy| at(x + dx, y + dy, channel);
                let gx = p(1, -1) + 2 * p(1, 0) + p(1, 1) - p(-1, -1) - 2 * p(-1, 0) - p(-1, 1);
                let gy = p(-1, 1) + 2 * p(0, 1) + p(1, 1) - p(-1, -1) - 2 * p(0, -1) - p(1, -1);
                gx * gx + gy * gy > EDGE_MAGNITUDE_SQUARED
            });
        }
    }
    Ok(edges)
}

/// Square (Chebyshev) dilation, separably by rows then columns.
fn dilate(
    mask: &[bool],
    width: usize,
    height: usize,
    radius: usize,
    cancellation: &CancellationToken,
) -> Result<Vec<bool>> {
    let mut rows = vec![false; mask.len()];
    for (input, output) in mask.chunks(width).zip(rows.chunks_mut(width)) {
        dilate_run(input, output, radius);
    }
    cancellation.check()?;
    let mut dilated = vec![false; mask.len()];
    let (mut column, mut spread) = (vec![false; height], vec![false; height]);
    for x in 0..width {
        for (y, value) in column.iter_mut().enumerate() {
            *value = rows[y * width + x];
        }
        dilate_run(&column, &mut spread, radius);
        for (y, value) in spread.iter().enumerate() {
            dilated[y * width + x] = *value;
        }
    }
    cancellation.check()?;
    Ok(dilated)
}

fn dilate_run(input: &[bool], output: &mut [bool], radius: usize) {
    let mut last = None;
    for (index, (&value, output)) in input.iter().zip(output.iter_mut()).enumerate() {
        if value {
            last = Some(index);
        }
        *output = last.is_some_and(|last| index - last <= radius);
    }
    let mut next = None;
    for (index, (&value, output)) in input.iter().zip(output.iter_mut()).enumerate().rev() {
        if value {
            next = Some(index);
        }
        *output |= next.is_some_and(|next| next - index <= radius);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Luma, Rgb, imageops};
    use std::{
        cell::RefCell,
        thread,
        time::{Duration, Instant},
    };

    fn token() -> CancellationToken {
        CancellationToken::default()
    }

    fn no_progress(_: Progress) {}

    /// Generate with a resident worker of its own that is not kept warm, so
    /// every run starts and stops a worker.
    fn generate_in(
        source: &RgbaImage,
        target: (u32, u32),
        cancellation: &CancellationToken,
        cache: &Path,
        runtime: impl FnOnce() -> Result<Runtime>,
        progress: &dyn Fn(Progress),
    ) -> Result<LineArtPair> {
        super::generate_in(
            source,
            target,
            cancellation,
            cache,
            runtime,
            &Residency::default(),
            progress,
        )
    }

    /// A white canvas with an off-centre filled disc and a rotated square,
    /// so displaced or mirrored outlines cannot coincide with the shapes.
    fn shapes(size: u32) -> RgbaImage {
        let s = f64::from(size);
        RgbaImage::from_fn(size, size, |x, y| {
            let (x, y) = (f64::from(x), f64::from(y));
            let disc = (x - 0.34 * s).hypot(y - 0.38 * s) < 0.2 * s;
            let (u, v) = (x - 0.68 * s, y - 0.66 * s);
            let diamond = u.abs() + v.abs() < 0.17 * s;
            if disc {
                Rgba([40, 60, 170, 255])
            } else if diamond {
                Rgba([170, 90, 40, 255])
            } else {
                Rgba([255, 255, 255, 255])
            }
        })
    }

    /// Line art that traces the shapes' outlines two pixels wide.
    fn outlines(source: &RgbaImage) -> GrayImage {
        let edges = sobel_edges(source, &token()).unwrap();
        let (width, height) = (source.width() as usize, source.height() as usize);
        let ink = dilate(&edges, width, height, 1, &token()).unwrap();
        GrayImage::from_fn(source.width(), source.height(), |x, y| {
            Luma([if ink[y as usize * width + x as usize] {
                0
            } else {
                255
            }])
        })
    }

    fn translated(sketch: &GrayImage, dx: i64, dy: i64) -> GrayImage {
        GrayImage::from_fn(sketch.width(), sketch.height(), |x, y| {
            let (sx, sy) = (i64::from(x) - dx, i64::from(y) - dy);
            if (0..i64::from(sketch.width())).contains(&sx)
                && (0..i64::from(sketch.height())).contains(&sy)
            {
                *sketch.get_pixel(sx as u32, sy as u32)
            } else {
                Luma([255])
            }
        })
    }

    #[test]
    fn small_targets_are_generated_at_a_512_pixel_shorter_side_and_rounded_to_16() {
        assert_eq!(ceil16(1), 16);
        assert_eq!(ceil16(16), 16);
        assert_eq!(ceil16(17), 32);
        let planned = |target| {
            let plan = plan(target).unwrap();
            (plan.generation, plan.crop)
        };
        // From 512 on, the target is generated as is, rounded up.
        assert_eq!(planned((512, 512)), ((512, 512), (512, 512)));
        assert_eq!(planned((513, 600)), ((528, 608), (513, 600)));
        assert_eq!(planned((1024, 1024)), ((1024, 1024), (1024, 1024)));
        // Smaller targets are scaled up uniformly; tiny ones work too.
        assert_eq!(planned((128, 128)), ((512, 512), (512, 512)));
        assert_eq!(planned((360, 360)), ((512, 512), (512, 512)));
        assert_eq!(planned((200, 100)), ((1024, 512), (1024, 512)));
        assert_eq!(planned((161, 255)), ((512, 816), (512, 811)));
        assert_eq!(planned((1, 1)), ((512, 512), (512, 512)));
        // When 512 would exceed 1 Mpx, the scale is lowered until it fits.
        for (target, ratio) in [((128, 1024), 8.), ((3, 20), 20. / 3.)] {
            let (generation, crop) = planned(target);
            assert!(u64::from(generation.0) * u64::from(generation.1) <= u64::from(MAX_AREA));
            assert!(crop.0 < 512 && crop.0 > 340, "{target:?}: {crop:?}");
            assert!((f64::from(crop.1) / f64::from(crop.0) - ratio).abs() < 0.02);
            assert_eq!(generation, (ceil16(crop.0), ceil16(crop.1)));
        }
        assert!(matches!(plan((0, 64)), Err(AppError::InvalidDimensions)));
        for (target, message) in [
            ((1024, 112), "aspect ratios up to 8:1"),
            ((10, 90), "aspect ratios up to 8:1"),
            ((1025, 1024), "1 megapixel"),
            ((2048, 520), "1 megapixel"),
        ] {
            let error = plan(target).unwrap_err().to_string();
            assert!(error.contains(message), "{target:?}: {error}");
        }
    }

    #[test]
    fn both_layers_are_center_cropped_at_the_same_offset() {
        assert_eq!(crop_offset((176, 256), (161, 255)), (7, 0));
        assert_eq!(crop_offset((64, 64), (49, 50)), (7, 7));
        assert_eq!(crop_offset((160, 160), (160, 160)), (0, 0));
        let generated = LineArtPair {
            line_art: GrayImage::from_fn(176, 256, |x, y| Luma([(x * 7 + y) as u8])),
            fill: RgbImage::from_fn(176, 256, |x, y| Rgb([x as u8, y as u8, 9])),
        };
        let line_art = center_crop(&generated.line_art, (161, 255));
        let fill = center_crop(&generated.fill, (161, 255));
        assert_eq!(line_art.dimensions(), (161, 255));
        assert_eq!(fill.dimensions(), (161, 255));
        for (x, y) in [(0, 0), (160, 254), (80, 100)] {
            assert_eq!(
                line_art.get_pixel(x, y),
                generated.line_art.get_pixel(x + 7, y)
            );
            assert_eq!(fill.get_pixel(x, y).0, [(x + 7) as u8, y as u8, 9]);
        }
    }

    #[test]
    fn the_reference_is_the_white_composited_source_at_the_generation_size() {
        let first = RgbaImage::from_fn(640, 256, |x, y| {
            if (300..400).contains(&x) && y == 20 {
                Rgba([20, 40, 60, 255])
            } else {
                Rgba([0, 0, 0, 0])
            }
        });
        let mut second = first.clone();
        for pixel in second.pixels_mut() {
            if pixel[3] == 0 {
                *pixel = Rgba([255, 0, 255, 0]);
            }
        }
        let (first, second) = (
            reference(&first, (320, 128), &token()).unwrap(),
            reference(&second, (320, 128), &token()).unwrap(),
        );
        assert_eq!(first.dimensions(), (320, 128));
        assert_eq!(
            first, second,
            "hidden RGB under transparency must not matter"
        );
        assert!(first.pixels().all(|pixel| pixel[3] == 255));
        assert_eq!(first.get_pixel(0, 0).0, [255, 255, 255, 255]);
        let half =
            white_composite(&RgbaImage::from_pixel(1, 1, Rgba([0, 0, 0, 128])), &token()).unwrap();
        assert_eq!(half.get_pixel(0, 0).0, [127, 127, 127, 255]);
        // The generation size may exceed the source by the rounding.
        assert_eq!(
            reference(&shapes(100), (112, 112), &token())
                .unwrap()
                .dimensions(),
            (112, 112)
        );
    }

    #[test]
    fn aligned_outlines_pass_the_sanity_and_alignment_checks() {
        for size in [96, 256, 1024] {
            let source = shapes(size);
            let sketch = outlines(&source);
            check_line_art(&sketch, &source, &token())
                .unwrap_or_else(|error| panic!("{size}px: {error}"));
            // A few hallucinated strokes away from any edge are tolerated.
            let mut noisy = sketch.clone();
            for x in size / 20..size / 5 {
                noisy.put_pixel(x, size - 4, Luma([0]));
            }
            check_line_art(&noisy, &source, &token()).unwrap();
        }
    }

    #[test]
    fn displaced_or_mirrored_outlines_are_rejected() {
        let source = shapes(512);
        let sketch = outlines(&source);
        let shift = (512. * 0.03_f64).round() as i64;
        for (name, misaligned) in [
            ("right", translated(&sketch, shift, 0)),
            ("down", translated(&sketch, 0, shift)),
            ("diagonal", translated(&sketch, -shift, shift)),
            ("mirrored", imageops::flip_horizontal(&sketch)),
            ("flipped", imageops::flip_vertical(&sketch)),
        ] {
            let error = check_line_art(&misaligned, &source, &token()).unwrap_err();
            assert!(
                error.to_string().contains("does not line up"),
                "{name}: {error}"
            );
        }
    }

    #[test]
    fn sparse_output_passes_and_solid_output_is_rejected() {
        let source = shapes(128);
        let flat = RgbaImage::from_pixel(128, 128, Rgba([90, 140, 60, 255]));
        let empty = GrayImage::from_pixel(128, 128, Luma([255]));
        check_line_art(&empty, &source, &token()).unwrap();
        check_line_art(&empty, &flat, &token()).unwrap();
        // A stray speck is too sparse to measure alignment, even off-edge.
        let mut speck = empty.clone();
        speck.put_pixel(2, 125, Luma([0]));
        check_line_art(&speck, &source, &token()).unwrap();
        for solid in [0, 100] {
            let solid = GrayImage::from_pixel(128, 128, Luma([solid]));
            assert!(
                check_line_art(&solid, &source, &token())
                    .unwrap_err()
                    .to_string()
                    .contains("nearly solid")
            );
        }
    }

    #[test]
    fn cache_key_covers_size_prompts_seeds_pixels_and_model_identity() {
        let reference = RgbaImage::from_pixel(64, 80, Rgba([10, 20, 30, 255]));
        let key = cache_key(&RECIPE, &reference);
        assert_eq!(key.len(), 64);
        assert_eq!(key, cache_key(&RECIPE, &reference.clone()));
        let mut pixel = reference.clone();
        pixel.put_pixel(63, 79, Rgba([10, 20, 31, 255]));
        let transposed = RgbaImage::from_pixel(80, 64, Rgba([10, 20, 30, 255]));
        let larger = RgbaImage::from_pixel(64, 96, Rgba([10, 20, 30, 255]));
        let other = |change: fn(&mut Recipe)| {
            let mut recipe = RECIPE;
            change(&mut recipe);
            cache_key(&recipe, &reference)
        };
        let different = [
            cache_key(&RECIPE, &pixel),
            cache_key(&RECIPE, &transposed),
            cache_key(&RECIPE, &larger),
            other(|recipe| recipe.revision = "0000000000000000000000000000000000000000"),
            other(|recipe| recipe.gguf_sha256 = "00"),
            other(|recipe| recipe.line_art_prompt = "convert this to line-art"),
            other(|recipe| recipe.fill_prompt = "remove the contours"),
            other(|recipe| recipe.line_art_seed = 1),
            other(|recipe| recipe.fill_seed = 1),
            other(|recipe| recipe.steps = 8),
            other(|recipe| recipe.guidance = "2.0"),
            other(|recipe| recipe.text_encoder_layers = "9,18"),
            other(|recipe| recipe.max_sequence_length = 256),
            // Moving text between the prompts is a different recipe.
            other(|recipe| {
                recipe.line_art_prompt = "convert this to line-art, remove thin linesremove";
                recipe.fill_prompt = " the ink contours";
            }),
        ];
        for (index, different) in different.iter().enumerate() {
            assert_ne!(*different, key, "{index}");
        }
    }

    #[test]
    fn missing_model_and_wrong_revision_marker_name_the_setup_command() {
        let model = tempfile::tempdir().unwrap();
        let error = verify_model(model.path()).unwrap_err().to_string();
        assert!(
            error.contains("missing") && error.contains(SETUP),
            "{error}"
        );
        fs::write(model.path().join("model_index.json"), "{}").unwrap();
        let error = verify_model(model.path()).unwrap_err().to_string();
        assert!(
            error.contains(RECIPE.revision) && error.contains(SETUP),
            "{error}"
        );
        fs::write(model.path().join(REVISION_MARKER), "0123abc\n").unwrap();
        assert!(verify_model(model.path()).is_err());
        fs::write(
            model.path().join(REVISION_MARKER),
            format!("{}\n", RECIPE.revision),
        )
        .unwrap();
        verify_model(model.path()).unwrap();
    }

    #[test]
    fn the_gguf_needs_the_pinned_size_and_setup_verified_hash() {
        let directory = tempfile::tempdir().unwrap();
        let gguf = directory.path().join(GGUF_FILE);
        let error = verify_gguf(&gguf).unwrap_err().to_string();
        assert!(
            error.contains("missing") && error.contains(SETUP),
            "{error}"
        );
        File::create(&gguf).unwrap().set_len(GGUF_SIZE).unwrap();
        let error = verify_gguf(&gguf).unwrap_err().to_string();
        assert!(
            error.contains(RECIPE.gguf_sha256) && error.contains(SETUP),
            "{error}"
        );
        let marker = directory.path().join(GGUF_MARKER);
        fs::write(&marker, "0b25\n").unwrap();
        assert!(verify_gguf(&gguf).is_err());
        fs::write(&marker, format!("{}\n", RECIPE.gguf_sha256)).unwrap();
        verify_gguf(&gguf).unwrap();
        // A truncated download fails even with a marker.
        File::options()
            .write(true)
            .open(&gguf)
            .unwrap()
            .set_len(GGUF_SIZE - 1)
            .unwrap();
        assert!(verify_gguf(&gguf).is_err());
        assert!(
            verify_gguf(directory.path()).is_err(),
            "a directory is no GGUF"
        );
    }

    #[test]
    fn host_launch_passes_the_fixed_generation_parameters_and_spaced_paths() {
        let runtime = Runtime {
            python: PathBuf::from("/host/line art/venv/bin/python"),
            model: PathBuf::from("/host/cache/diorama/flux2 klein"),
            gguf: PathBuf::from("/host/cache/diorama/gguf/q4 k m.gguf"),
            device: None,
            launch: Launch::FlatpakHost {
                launcher: PathBuf::from("/sandbox/bin/flatpak-spawn"),
            },
            host_library_path: Some("/opt/rocm/lib:/host/lib with spaces".into()),
            timeout: TIMEOUT,
            cancel_grace: resident::CANCEL_GRACE,
            idle_timeout: resident::IDLE_TIMEOUT,
            inference: std::sync::Arc::default(),
        };
        let arguments = |command: &Command| {
            command
                .get_args()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let command = resident::serve_command(&runtime, Path::new("/cache/run/line_art_worker.py"));
        assert_eq!(
            command.get_program(),
            Path::new("/sandbox/bin/flatpak-spawn")
        );
        assert_eq!(
            arguments(&command),
            [
                "--host",
                "--watch-bus",
                "--unset-env=LD_LIBRARY_PATH",
                "--unset-env=LD_PRELOAD",
                "--env=LD_LIBRARY_PATH=/opt/rocm/lib:/host/lib with spaces",
                "/host/line art/venv/bin/python",
                "/cache/run/line_art_worker.py",
                "--serve",
                "--model",
                "/host/cache/diorama/flux2 klein",
                "--gguf",
                "/host/cache/diorama/gguf/q4 k m.gguf",
                "--idle-timeout",
                "600",
            ]
        );
        let runtime = Runtime {
            device: Some("cpu".into()),
            launch: Launch::Direct,
            ..runtime
        };
        let command = resident::serve_command(&runtime, Path::new("/w.py"));
        assert_eq!(command.get_program(), runtime.python.as_os_str());
        assert_eq!(arguments(&command).last().map(String::as_str), Some("cpu"));
        // Encoding passes each prompt with its embedding file, and the
        // encoding parameters of the recipe.
        let embeds = PathBuf::from("/m/.p.safetensors");
        let command = encode_command(
            &runtime,
            Path::new("/w.py"),
            &[(RECIPE.fill_prompt, &embeds)],
        );
        assert_eq!(
            arguments(&command),
            [
                "/w.py",
                "--encode",
                "--model",
                "/host/cache/diorama/flux2 klein",
                "--text-encoder-layers",
                "9,18,27",
                "--max-sequence-length",
                "512",
                "--prompt-embeds",
                "remove the black outlines, change nothing else",
                "/m/.p.safetensors",
            ]
        );
    }

    #[test]
    fn prompt_embeddings_are_cached_in_the_model_directory_per_prompt_and_encoding() {
        let model = Path::new("/models/flux2 klein 9b");
        let path = prompt_embeds_path(model, &RECIPE, RECIPE.line_art_prompt);
        assert_eq!(path.parent(), Some(model));
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with(".diorama-prompt-") && name.ends_with(".safetensors"));
        assert_eq!(
            path,
            prompt_embeds_path(model, &RECIPE, RECIPE.line_art_prompt)
        );
        let other = |change: fn(&mut Recipe)| {
            let mut recipe = RECIPE;
            change(&mut recipe);
            prompt_embeds_path(model, &recipe, RECIPE.line_art_prompt)
        };
        for different in [
            prompt_embeds_path(model, &RECIPE, RECIPE.fill_prompt),
            other(|recipe| recipe.revision = "0000000000000000000000000000000000000000"),
            other(|recipe| recipe.text_encoder_layers = "9,18"),
            other(|recipe| recipe.max_sequence_length = 256),
        ] {
            assert_ne!(different, path);
        }
        // The transformer, seeds and generation settings do not matter.
        for same in [
            other(|recipe| recipe.gguf_sha256 = "00"),
            other(|recipe| recipe.steps = 8),
            other(|recipe| recipe.fill_prompt = "other"),
        ] {
            assert_eq!(same, path);
        }
    }

    #[test]
    fn cancellation_stops_before_runtime_resolution() {
        let cancellation = token();
        cancellation.cancel();
        assert!(matches!(
            generate(&RgbaImage::new(2, 2), (1, 1), &cancellation, &no_progress),
            Err(AppError::Cancelled)
        ));
    }

    #[cfg(unix)]
    mod worker {
        use super::*;

        /// The real worker's start: loading, then ready for jobs.
        const READY: &str = r#"echo '{"event": "stage", "stage": "load"}'
echo '{"event": "ready"}'"#;
        /// One job's events between library noise, as the real worker
        /// writes them.
        const JOB_EVENTS: &str = r#"echo 'Loading pipeline components...: 100%'
for image in line_art fill; do
  echo "{\"event\": \"step\", \"job\": $job, \"image\": \"$image\", \"step\": 1, \"steps\": 1, \"elapsed\": 0.01}"
  echo 'not json {'
  echo "{\"event\": \"stage\", \"stage\": \"decode\", \"job\": $job, \"image\": \"$image\"}"
done"#;
        const COPY_RESULTS: &str = r#"if cp "$dir/line-art.png" "$line_art" && cp "$dir/fill.png" "$fill"; then
  echo "{\"event\": \"done\", \"job\": $job}"
else
  echo "{\"event\": \"error\", \"job\": $job, \"message\": \"no results\"}"
fi"#;

        /// A fake interpreter. It records each launch's mode in `launches`
        /// and, in encode mode, each prompt in `encoded`, writing its
        /// embedding file if `writes_embeddings`. In serve mode it runs
        /// `start`, then `job` for each job line on stdin with `$job`,
        /// `$line_art` and `$fill` set, recording job ids in `jobs` and
        /// cancel requests in `cancels`, and records `stopped` when stdin
        /// closes.
        fn fake_runtime_with(
            root: &Path,
            writes_embeddings: bool,
            start: &str,
            job: &str,
        ) -> Runtime {
            let python = root.join("python");
            let write = u8::from(writes_embeddings);
            worker_process::write_test_executable(
                &python,
                &format!(
                    r#"#!/bin/bash
dir="$(dirname "$0")"
mode=serve
prompts=0
while [ $# -gt 0 ]; do
  case "$1" in
    --encode) mode=encode ;;
    --prompt-embeds)
      echo "$2" >> "$dir/encoded"
      prompts=$((prompts + 1))
      if [ {write} = 1 ]; then echo embeddings > "$3"; fi
      shift 2 ;;
  esac
  shift
done
echo "$mode" >> "$dir/launches"
if [ "$mode" = encode ]; then
  echo "{{\"event\": \"stage\", \"stage\": \"encode\", \"prompts\": $prompts}}"
  exit 0
fi
{start}
while IFS= read -r request; do
  case "$request" in
    *'"cancel"'*) echo "$request" >> "$dir/cancels"; continue ;;
  esac
  job=$(sed -n 's/.*"job": \([0-9]*\),.*/\1/p' <<< "$request")
  line_art=$(sed -n 's/.*"line_art_output": "\([^"]*\)".*/\1/p' <<< "$request")
  fill=$(sed -n 's/.*"fill_output": "\([^"]*\)".*/\1/p' <<< "$request")
  echo "$job" >> "$dir/jobs"
  {job}
done
echo stopped >> "$dir/launches"
"#
                ),
            );
            let model = root.join("model dir");
            fs::create_dir_all(&model).unwrap();
            fs::write(model.join("model_index.json"), "{}").unwrap();
            fs::write(model.join(REVISION_MARKER), RECIPE.revision).unwrap();
            let gguf = root.join("gguf dir").join(GGUF_FILE);
            fs::create_dir_all(gguf.parent().unwrap()).unwrap();
            File::create(&gguf).unwrap().set_len(GGUF_SIZE).unwrap();
            fs::write(gguf.with_file_name(GGUF_MARKER), RECIPE.gguf_sha256).unwrap();
            Runtime {
                python,
                model,
                gguf,
                device: None,
                launch: Launch::Direct,
                host_library_path: None,
                timeout: Duration::from_secs(10),
                cancel_grace: Duration::from_secs(5),
                idle_timeout: Duration::from_secs(60),
                // Each test serializes only its own runs.
                inference: std::sync::Arc::default(),
            }
        }

        /// A fake runtime whose prompts are already encoded.
        fn fake_runtime(root: &Path, start: &str, job: &str) -> Runtime {
            let runtime = fake_runtime_with(root, true, start, job);
            for prompt in [RECIPE.line_art_prompt, RECIPE.fill_prompt] {
                fs::write(
                    prompt_embeds_path(&runtime.model, &RECIPE, prompt),
                    "cached",
                )
                .unwrap();
            }
            runtime
        }

        fn succeeding(root: &Path) -> Runtime {
            fake_runtime(root, READY, &format!("{JOB_EVENTS}\n{COPY_RESULTS}"))
        }

        /// The fake worker's results for `target`: line art tracing
        /// `source` at the generation size and a fill whose pixels encode
        /// their own coordinates.
        fn prepare_results(root: &Path, source: &RgbaImage, target: (u32, u32)) -> LineArtPair {
            let size = plan(target).unwrap().generation;
            let pair = LineArtPair {
                line_art: outlines(&reference(source, size, &token()).unwrap()),
                fill: RgbImage::from_fn(size.0, size.1, |x, y| Rgb([x as u8, y as u8, 200])),
            };
            pair.line_art.save(root.join("line-art.png")).unwrap();
            pair.fill.save(root.join("fill.png")).unwrap();
            pair
        }

        fn key_for(source: &RgbaImage, target: (u32, u32)) -> String {
            let size = plan(target).unwrap().generation;
            cache_key(&RECIPE, &reference(source, size, &token()).unwrap())
        }

        /// Worker processes started so far.
        fn launches(root: &Path) -> usize {
            fs::read_to_string(root.join("launches"))
                .map(|log| log.lines().filter(|line| *line != "stopped").count())
                .unwrap_or(0)
        }

        fn cache_entries(cache: &Path) -> Vec<String> {
            let mut entries = fs::read_dir(cache)
                .map(|entries| {
                    entries
                        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_else(|_| Vec::new());
            entries.sort();
            entries
        }

        fn pairs(cache: &Path) -> usize {
            cache_entries(cache)
                .iter()
                .filter(|name| name.ends_with(LINE_ART_SUFFIX))
                .count()
        }

        #[test]
        fn a_run_generates_both_layers_at_the_rounded_size_and_crops_them_alike() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache with spaces");
            let source = shapes(700);
            let generated = prepare_results(root.path(), &source, (513, 600));
            let reported = RefCell::new(Vec::new());
            let pair = generate_in(
                &source,
                (513, 600),
                &token(),
                &cache,
                || Ok(succeeding(root.path())),
                &|progress| reported.borrow_mut().push(progress),
            )
            .unwrap();
            assert_eq!(pair.line_art.dimensions(), (513, 600));
            assert_eq!(pair.line_art, center_crop(&generated.line_art, (513, 600)));
            // The fill's pixels record where they were cropped from: at
            // (7, 4) of the 528 × 608 generation.
            assert_eq!(pair.fill.get_pixel(0, 0).0, [7, 4, 200]);
            assert_eq!(
                pair.fill.get_pixel(512, 599).0,
                [519_u32 as u8, 603_u32 as u8, 200]
            );
            assert_eq!(launches(root.path()), 1);

            // Progress starts at zero, never decreases and ends complete.
            let reported = reported.into_inner();
            assert!(reported.len() >= 2, "{reported:?}");
            assert_eq!(reported[0].fraction, 0.);
            assert!(reported[0].remaining > Duration::from_secs(5));
            assert!(
                reported
                    .windows(2)
                    .all(|pair| pair[0].fraction <= pair[1].fraction),
                "{reported:?}"
            );
            let last = reported.last().unwrap();
            assert_eq!((last.fraction, last.remaining), (1., Duration::ZERO));

            // One pair (two images) and the timing calibration are kept.
            let entries = cache_entries(&cache);
            assert_eq!(pairs(&cache), 1, "{entries:?}");
            assert_eq!(entries.len(), 3, "{entries:?}");
            assert!(entries.contains(&"timing-calibration".to_owned()));
            assert_ne!(Calibration::load(&cache), Calibration::default());
        }

        #[test]
        fn small_targets_are_cropped_at_the_scaled_size_then_reduced() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let source = shapes(600);
            // 128² is generated at 512², which is also the crop, and reduced:
            // the line art with bicubic, the fill with Lanczos.
            let generated = prepare_results(root.path(), &source, (128, 128));
            assert_eq!(generated.fill.dimensions(), (512, 512));
            let pair = generate_in(
                &source,
                (128, 128),
                &token(),
                &cache,
                || Ok(succeeding(root.path())),
                &no_progress,
            )
            .unwrap();
            let crop =
                |image: &GrayImage| image::imageops::crop_imm(image, 0, 0, 512, 512).to_image();
            let fill = image::imageops::crop_imm(&generated.fill, 0, 0, 512, 512).to_image();
            assert_eq!(
                pair.line_art,
                image::imageops::resize(
                    &crop(&generated.line_art),
                    128,
                    128,
                    FilterType::CatmullRom
                )
            );
            assert_eq!(
                pair.fill,
                image::imageops::resize(&fill, 128, 128, FilterType::Lanczos3)
            );
            // A 1-pixel target works as well, from the same generation size.
            let tiny = generate_in(
                &source,
                (1, 1),
                &token(),
                &cache,
                || Ok(succeeding(root.path())),
                &no_progress,
            )
            .unwrap();
            assert_eq!(tiny.line_art.dimensions(), (1, 1));
            assert_eq!(tiny.fill.dimensions(), (1, 1));
        }

        #[test]
        fn missing_prompt_embeddings_are_encoded_first_in_a_process_of_their_own() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let source = shapes(600);
            prepare_results(root.path(), &source, (512, 512));
            let runtime = || {
                Ok(fake_runtime_with(
                    root.path(),
                    true,
                    READY,
                    &format!("{JOB_EVENTS}\n{COPY_RESULTS}"),
                ))
            };
            let reported = RefCell::new(Vec::new());
            generate_in(
                &source,
                (512, 512),
                &token(),
                &cache,
                runtime,
                &|progress| reported.borrow_mut().push(progress),
            )
            .unwrap();
            let read = |name| fs::read_to_string(root.path().join(name)).unwrap();
            assert_eq!(read("launches"), "encode\nserve\nstopped\n");
            assert_eq!(
                read("encoded"),
                format!("{}\n{}\n", RECIPE.line_art_prompt, RECIPE.fill_prompt)
            );
            let model = root.path().join("model dir");
            for prompt in [RECIPE.line_art_prompt, RECIPE.fill_prompt] {
                assert!(prompt_embeds_path(&model, &RECIPE, prompt).is_file());
            }
            // The first estimate includes the encoding process.
            let reported = reported.into_inner();
            assert_eq!(
                reported[0].remaining,
                estimate::estimate((512, 512), 2, estimate::cold_start_seconds(), true)
            );
            assert_eq!(reported.last().unwrap().fraction, 1.);

            // Cached embeddings are not encoded again; only a missing one is.
            let mut changed = source.clone();
            for (x, y) in (0..40).flat_map(|x| (0..40).map(move |y| (x, y))) {
                changed.put_pixel(x, y, Rgba([200, 30, 30, 255]));
            }
            generate_in(
                &changed,
                (512, 512),
                &token(),
                &cache,
                runtime,
                &no_progress,
            )
            .unwrap();
            assert_eq!(read("launches"), "encode\nserve\nstopped\nserve\nstopped\n");
            fs::remove_file(prompt_embeds_path(&model, &RECIPE, RECIPE.fill_prompt)).unwrap();
            changed.put_pixel(300, 300, Rgba([0, 0, 0, 255]));
            for (x, y) in (100..140).flat_map(|x| (100..140).map(move |y| (x, y))) {
                changed.put_pixel(x, y, Rgba([30, 200, 30, 255]));
            }
            generate_in(
                &changed,
                (512, 512),
                &token(),
                &cache,
                runtime,
                &no_progress,
            )
            .unwrap();
            assert_eq!(
                read("launches"),
                "encode\nserve\nstopped\nserve\nstopped\nencode\nserve\nstopped\n"
            );
            assert!(read("encoded").ends_with(&format!("{}\n", RECIPE.fill_prompt)));
            assert_eq!(read("encoded").lines().count(), 3);
        }

        #[test]
        fn an_encoder_that_writes_no_embeddings_fails_before_generating() {
            let root = tempfile::tempdir().unwrap();
            let error = generate_in(
                &shapes(64),
                (64, 64),
                &token(),
                &root.path().join("cache"),
                || Ok(fake_runtime_with(root.path(), false, READY, "")),
                &no_progress,
            )
            .unwrap_err();
            assert!(error.to_string().contains("wrote no embeddings"), "{error}");
            assert_eq!(
                fs::read_to_string(root.path().join("launches")).unwrap(),
                "encode\n"
            );
        }

        #[test]
        fn a_cache_hit_skips_the_worker_and_reports_no_progress() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let source = shapes(700);
            prepare_results(root.path(), &source, (515, 520));
            let first = generate_in(
                &source,
                (515, 520),
                &token(),
                &cache,
                || Ok(succeeding(root.path())),
                &no_progress,
            )
            .unwrap();
            // Another target with the same generation size reuses the pair.
            for (target, offset) in [((515, 520), 6), ((520, 518), 4)] {
                let hit = generate_in(
                    &source,
                    target,
                    &token(),
                    &cache,
                    || panic!("a cache hit must not resolve or launch the worker"),
                    &|_| panic!("a cache hit reports no progress"),
                )
                .unwrap();
                assert_eq!(hit.fill.dimensions(), target);
                assert_eq!(hit.fill.get_pixel(0, 0)[0], offset);
                if target == (515, 520) {
                    assert_eq!(hit, first);
                }
            }
            assert_eq!(launches(root.path()), 1);

            // Different pixels are a different key and run the worker again.
            let mut changed = source.clone();
            for (x, y) in (0..20).flat_map(|x| (0..20).map(move |y| (x, y))) {
                changed.put_pixel(x, y, Rgba([200, 30, 30, 255]));
            }
            generate_in(
                &changed,
                (515, 520),
                &token(),
                &cache,
                || Ok(succeeding(root.path())),
                &no_progress,
            )
            .unwrap();
            assert_eq!(launches(root.path()), 2);
            assert_eq!(pairs(&cache), 2);
        }

        #[test]
        fn empty_line_art_for_a_flat_source_is_accepted_and_cached() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let flat = RgbaImage::from_pixel(100, 90, Rgba([90, 140, 60, 255]));
            let (w, h) = plan((60, 50)).unwrap().generation;
            GrayImage::from_pixel(w, h, Luma([255]))
                .save(root.path().join("line-art.png"))
                .unwrap();
            RgbImage::from_pixel(w, h, Rgb([90, 140, 60]))
                .save(root.path().join("fill.png"))
                .unwrap();
            let first = generate_in(
                &flat,
                (60, 50),
                &token(),
                &cache,
                || Ok(succeeding(root.path())),
                &no_progress,
            )
            .unwrap();
            assert_eq!(first.line_art.dimensions(), (60, 50));
            assert!(first.line_art.pixels().all(|pixel| pixel[0] == 255));
            let second = generate_in(
                &flat,
                (60, 50),
                &token(),
                &cache,
                || panic!("no launch"),
                &no_progress,
            )
            .unwrap();
            assert_eq!(second, first);
            assert_eq!(launches(root.path()), 1);
        }

        #[test]
        fn cache_eviction_keeps_the_most_recently_used_pairs() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let source = shapes(64);
            prepare_results(root.path(), &source, (64, 64));
            let runtime = || Ok(succeeding(root.path()));
            let hit =
                generate_in(&source, (64, 64), &token(), &cache, runtime, &no_progress).unwrap();
            let key = key_for(&source, (64, 64));
            let hit_files = cached_paths(&cache, &key);
            // Older unrelated pairs, legacy single-image results, a scratch
            // directory and a partial store.
            let epoch = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
            let aged = |path: &Path, age: u64| {
                File::options()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_modified(epoch + Duration::from_secs(age))
                    .unwrap();
            };
            for index in 0..MAX_CACHED_PAIRS as u64 + 3 {
                for suffix in [LINE_ART_SUFFIX, FILL_SUFFIX] {
                    let path = cache.join(format!("{index:064x}{suffix}"));
                    fs::write(&path, b"x").unwrap();
                    aged(&path, index + 10);
                }
            }
            let legacy = cache.join(format!("{:064x}.png", u64::MAX));
            fs::write(&legacy, b"x").unwrap();
            aged(&legacy, 1);
            fs::create_dir(cache.join(".run-keep")).unwrap();
            fs::write(cache.join(".store-keep.png"), b"x").unwrap();
            for path in &hit_files {
                aged(path, 0);
            }
            // A hit marks the oldest pair as the most recently used.
            assert_eq!(
                generate_in(
                    &source,
                    (64, 64),
                    &token(),
                    &cache,
                    || panic!("no launch"),
                    &no_progress
                )
                .unwrap(),
                hit
            );
            // A fresh result is stored, which evicts beyond the cap.
            let mut changed = source.clone();
            changed.put_pixel(0, 0, Rgba([254, 255, 255, 255]));
            generate_in(&changed, (64, 64), &token(), &cache, runtime, &no_progress).unwrap();
            assert_eq!(pairs(&cache), MAX_CACHED_PAIRS);
            assert!(
                hit_files.iter().all(|path| path.is_file()),
                "the touched hit survives"
            );
            // 67 aged pairs, the legacy file, the hit and the fresh pair:
            // the legacy file and the five oldest pairs go, whole.
            assert!(!legacy.exists());
            for evicted in 0..5_u64 {
                for suffix in [LINE_ART_SUFFIX, FILL_SUFFIX] {
                    assert!(!cache.join(format!("{evicted:064x}{suffix}")).exists());
                }
            }
            for suffix in [LINE_ART_SUFFIX, FILL_SUFFIX] {
                assert!(cache.join(format!("{:064x}{suffix}", 5)).is_file());
            }
            assert_eq!(launches(root.path()), 2);
            assert!(cache.join(".run-keep").is_dir() && cache.join(".store-keep.png").is_file());
            assert!(cache.join("timing-calibration").is_file());
            // A missing directory is not an error.
            evict_cached(&root.path().join("missing"), 1);
        }

        #[test]
        fn a_cached_pair_that_fails_the_checks_is_dropped_and_a_lone_fill_is_a_miss() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let source = shapes(64);
            let key = key_for(&source, (64, 64));
            let (w, h) = plan((64, 64)).unwrap().generation;
            let solid = LineArtPair {
                line_art: GrayImage::from_pixel(w, h, Luma([0])),
                fill: RgbImage::new(w, h),
            };
            store_cached(&cache, &key, &solid).unwrap();
            let error = generate_in(
                &source,
                (64, 64),
                &token(),
                &cache,
                || panic!("no launch"),
                &no_progress,
            )
            .unwrap_err();
            assert!(error.to_string().contains("nearly solid"), "{error}");
            assert!(cached_paths(&cache, &key).iter().all(|path| !path.exists()));

            let [_, fill] = cached_paths(&cache, &key);
            RgbImage::new(w, h).save(&fill).unwrap();
            prepare_results(root.path(), &source, (64, 64));
            generate_in(
                &source,
                (64, 64),
                &token(),
                &cache,
                || Ok(succeeding(root.path())),
                &no_progress,
            )
            .unwrap();
            assert_eq!(launches(root.path()), 1);
        }

        #[test]
        fn failed_or_rejected_runs_are_not_cached_and_surface_the_log_tail() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let source = shapes(128);
            let error = generate_in(
                &source,
                (128, 128),
                &token(),
                &cache,
                || {
                    Ok(fake_runtime(
                        root.path(),
                        "echo 'RuntimeError: HIP out of memory' >&2; exit 7",
                        "",
                    ))
                },
                &no_progress,
            )
            .unwrap_err();
            assert!(matches!(error, AppError::SketchGeneration(_)));
            assert!(error.to_string().contains("HIP out of memory"), "{error}");
            assert!(
                !cache.join("timing-calibration").exists(),
                "a failed worker does not calibrate"
            );

            // Misaligned output passes the worker but fails the check.
            let generated = prepare_results(root.path(), &source, (128, 128));
            translated(&generated.line_art, 4, 4)
                .save(root.path().join("line-art.png"))
                .unwrap();
            let error = generate_in(
                &source,
                (128, 128),
                &token(),
                &cache,
                || Ok(succeeding(root.path())),
                &no_progress,
            )
            .unwrap_err();
            assert!(error.to_string().contains("does not line up"), "{error}");

            // A wrong-size layer is rejected as well.
            generated
                .line_art
                .save(root.path().join("line-art.png"))
                .unwrap();
            RgbImage::new(16, 16)
                .save(root.path().join("fill.png"))
                .unwrap();
            let error = generate_in(
                &source,
                (128, 128),
                &token(),
                &cache,
                || Ok(succeeding(root.path())),
                &no_progress,
            )
            .unwrap_err();
            assert!(error.to_string().contains("16×16"), "{error}");
            // A worker that exits without writing its outputs.
            let error = generate_in(
                &source,
                (128, 128),
                &token(),
                &cache,
                || {
                    Ok(fake_runtime(
                        root.path(),
                        READY,
                        &format!(
                            "{JOB_EVENTS}\necho \"{{\\\"event\\\": \\\"done\\\", \\\"job\\\": $job}}\""
                        ),
                    ))
                },
                &no_progress,
            )
            .unwrap_err();
            assert!(error.to_string().contains("no readable image"), "{error}");
            assert_eq!(pairs(&cache), 0);
        }

        #[test]
        fn missing_runtime_model_or_gguf_fails_before_launch() {
            let root = tempfile::tempdir().unwrap();
            let run = |runtime: Runtime| {
                generate_in(
                    &shapes(64),
                    (64, 64),
                    &token(),
                    &root.path().join("cache"),
                    || Ok(runtime),
                    &no_progress,
                )
                .unwrap_err()
                .to_string()
            };
            let runtime = succeeding(root.path());
            fs::remove_file(runtime.model.join(REVISION_MARKER)).unwrap();
            assert!(run(runtime).contains(SETUP));
            let runtime = succeeding(root.path());
            fs::remove_file(runtime.gguf.with_file_name(GGUF_MARKER)).unwrap();
            let error = run(runtime);
            assert!(error.contains("Q4_K_M") && error.contains(SETUP), "{error}");
            assert_eq!(launches(root.path()), 0);

            let mut runtime = succeeding(root.path());
            runtime.python = root.path().join("no such python");
            assert!(run(runtime).contains("Could not start"));
        }

        /// A job that reports its first step, then ignores stdin.
        const STALLING_JOB: &str = r#"echo "{\"event\": \"step\", \"job\": $job, \"image\": \"line_art\", \"step\": 1, \"steps\": 1, \"elapsed\": 0.5}"
sleep 60"#;
        /// A job that reports its first step, then stops when its cancel
        /// request arrives.
        const CANCELLABLE_JOB: &str = r#"echo "{\"event\": \"step\", \"job\": $job, \"image\": \"line_art\", \"step\": 1, \"steps\": 1, \"elapsed\": 0.5}"
IFS= read -r request
echo "$request" >> "$dir/cancels"
echo "{\"event\": \"cancelled\", \"job\": $job}""#;

        /// Generate in a thread and cancel once the first step arrived.
        fn cancel_after_first_step(
            source: &RgbaImage,
            cache: &Path,
            runtime: Runtime,
            residency: &Residency,
        ) -> Result<LineArtPair> {
            let cancellation = token();
            let stepped = std::sync::atomic::AtomicBool::new(false);
            thread::scope(|scope| {
                let worker = scope.spawn(|| {
                    super::super::generate_in(
                        source,
                        (512, 512),
                        &cancellation,
                        cache,
                        || Ok(runtime),
                        residency,
                        &|progress| {
                            // The first step's estimate replaces the initial one.
                            if progress.fraction > 0. {
                                stepped.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        },
                    )
                });
                let deadline = Instant::now() + Duration::from_secs(5);
                while !stepped.load(std::sync::atomic::Ordering::Relaxed)
                    && Instant::now() < deadline
                {
                    thread::sleep(Duration::from_millis(5));
                }
                assert!(
                    stepped.load(std::sync::atomic::Ordering::Relaxed),
                    "progress streamed"
                );
                cancellation.cancel();
                let begun = Instant::now();
                let result = worker.join().unwrap();
                assert!(
                    begun.elapsed() < Duration::from_secs(2),
                    "cancel returns at once"
                );
                result
            })
        }

        #[test]
        fn cancellation_returns_at_once_and_stops_a_worker_that_is_not_kept_warm() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let residency = Residency::default();
            let runtime = fake_runtime(root.path(), READY, STALLING_JOB);
            let result = cancel_after_first_step(&shapes(600), &cache, runtime, &residency);
            assert!(matches!(result, Err(AppError::Cancelled)));
            assert!(!residency.is_running());
            assert!(
                cache_entries(&cache).is_empty(),
                "{:?}",
                cache_entries(&cache)
            );
        }

        fn warm(residency: &Residency) {
            residency.set_keep_warm(true);
        }

        #[test]
        fn a_warm_worker_runs_later_jobs_without_starting_again() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let residency = Residency::default();
            warm(&residency);
            let source = shapes(600);
            prepare_results(root.path(), &source, (512, 512));
            let run = |source: &RgbaImage| {
                let reported = RefCell::new(Vec::new());
                super::super::generate_in(
                    source,
                    (512, 512),
                    &token(),
                    &cache,
                    || Ok(succeeding(root.path())),
                    &residency,
                    &|progress| reported.borrow_mut().push(progress),
                )
                .unwrap();
                reported.into_inner()
            };
            let cold = run(&source);
            assert_eq!(
                cold[0].remaining,
                estimate::estimate((512, 512), 0, estimate::cold_start_seconds(), true)
            );
            let mut changed = source.clone();
            for (x, y) in (0..40).flat_map(|x| (0..40).map(move |y| (x, y))) {
                changed.put_pixel(x, y, Rgba([200, 30, 30, 255]));
            }
            // The worker is ready: no start-up in the estimate, which the
            // fast first run calibrated.
            let factor = Calibration::load(&cache).factor();
            let warm = run(&changed);
            // Same size as before: no warm-up either.
            let expected = factor * estimate::estimate((512, 512), 0, 0., false).as_secs_f64();
            assert!((warm[0].remaining.as_secs_f64() - expected).abs() < 1e-6);
            let read = |name| fs::read_to_string(root.path().join(name)).unwrap();
            assert_eq!(read("launches"), "serve\n");
            assert_eq!(read("jobs"), "1\n2\n");
            assert!(residency.is_running());
            // No longer kept warm: the worker stops.
            residency.set_keep_warm(false);
            residency.reconcile(|| panic!("stopping resolves no runtime"));
            assert_eq!(read("launches"), "serve\nstopped\n");
            assert!(!residency.is_running());
        }

        #[test]
        fn a_reported_failure_keeps_the_worker_and_a_crash_restarts_it() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let residency = Residency::default();
            warm(&residency);
            let source = shapes(600);
            prepare_results(root.path(), &source, (512, 512));
            // Job 1 fails, job 2 crashes the worker, job 3 succeeds.
            let job = format!(
                r#"case "$job" in
  1) echo "{{\"event\": \"error\", \"job\": $job, \"message\": \"ValueError: bad input\"}}" ;;
  2) echo 'Segmentation fault' >&2; exit 139 ;;
  *) {JOB_EVENTS}
     {COPY_RESULTS} ;;
esac"#
            );
            let runtime = || Ok(fake_runtime(root.path(), READY, &job));
            let run = |source: &RgbaImage| {
                super::super::generate_in(
                    source,
                    (512, 512),
                    &token(),
                    &cache,
                    runtime,
                    &residency,
                    &no_progress,
                )
            };
            let error = run(&source).unwrap_err().to_string();
            assert!(error.contains("ValueError: bad input"), "{error}");
            assert!(
                residency.is_running(),
                "a reported failure keeps the worker"
            );
            let error = run(&source).unwrap_err().to_string();
            assert!(
                error.contains("stopped") && error.contains("Segmentation fault"),
                "{error}"
            );
            assert!(!residency.is_running());
            // The next job starts a fresh worker, whose first job is job 1
            // again: make it succeed by changing the pixels' job numbering.
            fs::write(root.path().join("jobs"), "").unwrap();
            let succeeding_job = format!("{JOB_EVENTS}\n{COPY_RESULTS}");
            let runtime = || Ok(fake_runtime(root.path(), READY, &succeeding_job));
            super::super::generate_in(
                &source,
                (512, 512),
                &token(),
                &cache,
                runtime,
                &residency,
                &no_progress,
            )
            .unwrap();
            assert_eq!(launches(root.path()), 2);
        }

        #[test]
        fn a_cancelled_job_is_awaited_before_the_next_on_the_same_worker() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let residency = Residency::default();
            warm(&residency);
            let source = shapes(600);
            prepare_results(root.path(), &source, (512, 512));
            let job = format!(
                r#"if [ "$job" = 1 ]; then
  {CANCELLABLE_JOB}
else
  {JOB_EVENTS}
  {COPY_RESULTS}
fi"#
            );
            let result = cancel_after_first_step(
                &source,
                &cache,
                fake_runtime(root.path(), READY, &job),
                &residency,
            );
            assert!(matches!(result, Err(AppError::Cancelled)));
            assert!(residency.is_running(), "a warm worker survives a cancel");
            let mut changed = source.clone();
            changed.put_pixel(0, 0, Rgba([1, 2, 3, 255]));
            for (x, y) in (0..40).flat_map(|x| (0..40).map(move |y| (x, y))) {
                changed.put_pixel(x, y, Rgba([200, 30, 30, 255]));
            }
            super::super::generate_in(
                &changed,
                (512, 512),
                &token(),
                &cache,
                || Ok(fake_runtime(root.path(), READY, &job)),
                &residency,
                &no_progress,
            )
            .unwrap();
            let read = |name| fs::read_to_string(root.path().join(name)).unwrap();
            assert_eq!(read("cancels"), "{\"cancel\": 1}\n");
            assert_eq!(read("jobs"), "1\n2\n");
            assert_eq!(launches(root.path()), 1);
        }

        #[test]
        fn an_unresponsive_cancelled_worker_is_replaced() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let residency = Residency::default();
            warm(&residency);
            let source = shapes(600);
            prepare_results(root.path(), &source, (512, 512));
            let job = format!(
                r#"if [ "$job" = 1 ] && [ ! -e "$dir/stalled" ]; then
  touch "$dir/stalled"
  {STALLING_JOB}
else
  {JOB_EVENTS}
  {COPY_RESULTS}
fi"#
            );
            let mut runtime = fake_runtime(root.path(), READY, &job);
            runtime.cancel_grace = Duration::from_millis(200);
            assert!(matches!(
                cancel_after_first_step(&source, &cache, runtime, &residency),
                Err(AppError::Cancelled)
            ));
            let begun = Instant::now();
            super::super::generate_in(
                &source,
                (512, 512),
                &token(),
                &cache,
                || Ok(fake_runtime(root.path(), READY, &job)),
                &residency,
                &no_progress,
            )
            .unwrap();
            assert!(begun.elapsed() < Duration::from_secs(10));
            assert_eq!(launches(root.path()), 2);
        }

        #[test]
        fn a_worker_that_exited_while_idle_is_restarted_silently() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let residency = Residency::default();
            warm(&residency);
            let source = shapes(600);
            prepare_results(root.path(), &source, (512, 512));
            // Each worker exits after one job, as on its idle timeout.
            let job = format!("{JOB_EVENTS}\n{COPY_RESULTS}\nexit 0");
            let run = |source: &RgbaImage| {
                super::super::generate_in(
                    source,
                    (512, 512),
                    &token(),
                    &cache,
                    || Ok(fake_runtime(root.path(), READY, &job)),
                    &residency,
                    &no_progress,
                )
            };
            run(&source).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while residency.is_running() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            assert!(!residency.is_running(), "the worker exited while idle");
            let mut changed = source.clone();
            for (x, y) in (0..40).flat_map(|x| (0..40).map(move |y| (x, y))) {
                changed.put_pixel(x, y, Rgba([200, 30, 30, 255]));
            }
            run(&changed).unwrap();
            assert_eq!(launches(root.path()), 2);
        }

        #[test]
        fn warming_starts_a_worker_in_the_background_and_cooling_stops_it() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let residency = std::sync::Arc::new(Residency::default());
            let path = root.path().to_owned();
            let started = {
                let cache = cache.clone();
                move || Ok((succeeding(&path), cache))
            };
            residency.set_warm(true, started);
            let read = || fs::read_to_string(root.path().join("launches")).unwrap_or_default();
            let deadline = Instant::now() + Duration::from_secs(5);
            while read() != "serve\n" && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(read(), "serve\n");
            assert!(residency.is_running());
            residency.set_warm(false, || panic!("stopping resolves no runtime"));
            let deadline = Instant::now() + Duration::from_secs(5);
            while read() != "serve\nstopped\n" && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(read(), "serve\nstopped\n");
            // A worker that cannot start only logs.
            let broken = std::sync::Arc::new(Residency::default());
            let model = root.path().join("no model");
            broken.set_warm(true, move || {
                let mut runtime = succeeding(&model);
                runtime.model = model.join("missing");
                Ok((runtime, PathBuf::from("/nonexistent")))
            });
            thread::sleep(Duration::from_millis(200));
            assert!(!broken.is_running());
        }

        #[test]
        fn timeout_kills_the_worker() {
            let root = tempfile::tempdir().unwrap();
            let mut runtime = fake_runtime(root.path(), "exec sleep 60", "");
            runtime.timeout = Duration::from_millis(100);
            let begun = Instant::now();
            let error = generate_in(
                &shapes(64),
                (64, 64),
                &token(),
                &root.path().join("cache"),
                || Ok(runtime),
                &no_progress,
            )
            .unwrap_err();
            assert!(error.to_string().contains("timed out"), "{error}");
            assert!(begun.elapsed() < Duration::from_secs(30));
        }
    }

    /// Prints alignment measurements for a real reference and its model line
    /// art at the same size, and checks that displaced or mirrored copies
    /// are rejected:
    ///
    /// `DIORAMA_LINE_ART_CALIBRATION_SOURCE=reference.png DIORAMA_LINE_ART_CALIBRATION_SKETCH=art.png cargo test --lib line_art_calibration -- --ignored --nocapture`
    #[test]
    #[ignore = "requires a real reference image and its model line art"]
    fn line_art_calibration() {
        let path = |name| std::env::var_os(name).unwrap_or_else(|| panic!("set {name}"));
        let sketch = image::open(path("DIORAMA_LINE_ART_CALIBRATION_SKETCH"))
            .unwrap()
            .into_luma8();
        let source = image::open(path("DIORAMA_LINE_ART_CALIBRATION_SOURCE"))
            .unwrap()
            .into_rgba8();
        let reference = reference(&source, sketch.dimensions(), &token()).unwrap();
        let longest = i64::from(sketch.width().max(sketch.height()));
        let report = |name: &str, sketch: &GrayImage| {
            let measured = measure(sketch, &reference, &token()).unwrap();
            let verdict = check_line_art(sketch, &reference, &token());
            eprintln!(
                "{name:>16}: ink {:.4} miss {:.4} displaced {:.4} ratio {:.3} -> {}",
                measured.ink_fraction,
                measured.miss,
                measured.displaced_miss.unwrap_or(f64::NAN),
                measured.miss / measured.displaced_miss.unwrap_or(f64::NAN),
                if verdict.is_ok() { "pass" } else { "reject" }
            );
            verdict.is_ok()
        };
        assert!(report("aligned", &sketch));
        for percent in [1, 2, 3, 5] {
            let shift = (longest * percent + 50) / 100;
            for (axis, dx, dy) in [("x", shift, 0), ("y", 0, shift), ("xy", shift, shift)] {
                report(
                    &format!("shift {axis} {percent}%"),
                    &translated(&sketch, dx, dy),
                );
            }
        }
        report("mirror h", &imageops::flip_horizontal(&sketch));
        report("mirror v", &imageops::flip_vertical(&sketch));
        report("rotate 180", &imageops::rotate180(&sketch));
    }
}
