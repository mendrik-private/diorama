//! Local FLUX.2 [klein] 4B line-art generation for Game Asset contours.
//!
//! The model runs in a Python worker on the GPU, offline, from weights the
//! user installed with `build-aux/setup-line-art.py`. Results are cached on
//! disk by input and model parameters, restored to the exact source size, and
//! rejected when they are solid or do not line up with the source.
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

use image::{GrayImage, Rgba, RgbaImage, imageops::FilterType};
use sha2::{Digest, Sha256};

use crate::{
    document::CancellationToken,
    error::{AppError, Result},
    tools::worker_process::{self, Launch, RuntimeConfiguration},
};

const WORKER: &str = include_str!("line_art_worker.py");
const MODEL_REVISION: &str = "e7b7dc27f91deacad38e78976d1f2b499d76a294";
const MODEL_DIRECTORY: &str = "diorama/flux2-klein-4b";
/// Written by setup only after a complete download of `MODEL_REVISION`.
const REVISION_MARKER: &str = ".diorama-revision";
const RUNTIME_CONFIG: &str = "line-art-runtime.conf";
/// Accepted results kept on disk; older ones are evicted least recently used.
const MAX_CACHED_RESULTS: usize = 64;
const SETUP: &str = "python3 build-aux/setup-line-art.py";
const PROMPT: &str = "convert this to line-art, remove thin lines";
const SEED: u64 = 0;
const STEPS: u32 = 4;
const GUIDANCE: &str = "1.0";
/// FLUX.2 works on 16-pixel latent patches and is tuned for about 1 Mpx.
const MAX_SIDE: u32 = 1024;
const MULTIPLE: u32 = 16;
/// The FLUX.2 image processor rejects sides under 64 pixels and aspect
/// ratios over 8:1.
const MIN_SIDE: u32 = 64;
const MAX_ASPECT: u32 = 8;
/// A first run also encodes the prompt; either stage can take minutes.
const TIMEOUT: Duration = Duration::from_secs(20 * 60);

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

struct Runtime {
    python: PathBuf,
    model: PathBuf,
    device: Option<String>,
    launch: Launch,
    host_library_path: Option<String>,
    timeout: Duration,
}

/// Generate a grayscale line-art sketch with the exact source dimensions.
///
/// Input is white-composited and fitted to the pipeline (at most 1024 and at
/// least 64 pixels on a side, multiples of 16, at most 8:1); the result is
/// restored with Lanczos.
pub fn sketch(image: &RgbaImage, cancellation: &CancellationToken) -> Result<GrayImage> {
    cancellation.check()?;
    if image.width() == 0 || image.height() == 0 {
        return Err(AppError::InvalidDimensions);
    }
    let cache = worker_process::cache_home()
        .ok_or_else(|| {
            AppError::SketchGeneration(
                "HOME is unavailable; cannot locate the line-art cache".into(),
            )
        })?
        .join("diorama/line-art");
    sketch_in(image, cancellation, &cache, Runtime::from_environment)
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
        let model = match std::env::var_os("DIORAMA_LINE_ART_MODEL") {
            Some(model) => PathBuf::from(model),
            None => worker_process::app_or_host_install(
                &worker_process::cache_home().ok_or_else(|| {
                    AppError::SketchGeneration(
                        "HOME is unavailable; set DIORAMA_LINE_ART_MODEL".into(),
                    )
                })?,
                worker_process::host_cache_home().as_deref(),
                Path::new(MODEL_DIRECTORY),
                |model| model.join("model_index.json").is_file(),
            ),
        };
        Ok(Self {
            python,
            model,
            device: std::env::var("DIORAMA_LINE_ART_DEVICE")
                .ok()
                .filter(|device| !device.is_empty()),
            launch,
            host_library_path: configuration.library_path,
            timeout: TIMEOUT,
        })
    }
}

/// `cache` holds accepted results and the worker's host-visible scratch
/// directories. The runtime is resolved only on a cache miss.
fn sketch_in(
    image: &RgbaImage,
    cancellation: &CancellationToken,
    cache: &Path,
    runtime: impl FnOnce() -> Result<Runtime>,
) -> Result<GrayImage> {
    let opaque = white_composite(image, cancellation)?;
    let prepared = prepare(&opaque, cancellation)?;
    let cached = cache.join(format!(
        "{}.png",
        cache_key(MODEL_REVISION, PROMPT, &prepared)
    ));
    if let Some(generated) = read_cached(&cached, prepared.dimensions()) {
        let restored = restore(&generated, image.dimensions(), cancellation)?;
        return match check_line_art(&restored, &opaque, cancellation) {
            Ok(()) => {
                // Eviction is least recently used, not least recently made.
                if let Err(error) = File::options()
                    .write(true)
                    .open(&cached)
                    .and_then(|file| file.set_modified(SystemTime::now()))
                {
                    tracing::debug!(%error, path = %cached.display(), "Could not touch cached line art");
                }
                Ok(restored)
            }
            Err(error) => {
                // Only accepted results belong in the cache; a cancelled
                // check says nothing about the entry.
                if matches!(error, AppError::SketchGeneration(_)) {
                    let _ = fs::remove_file(&cached);
                }
                Err(error)
            }
        };
    }
    let generated = generate(&prepared, cancellation, &runtime()?, cache)?;
    let restored = restore(&generated, image.dimensions(), cancellation)?;
    check_line_art(&restored, &opaque, cancellation)?;
    cancellation.check()?;
    match store_cached(&cached, &generated) {
        Ok(()) => evict_cached(cache, MAX_CACHED_RESULTS),
        Err(error) => tracing::warn!(%error, path = %cached.display(), "Could not cache line art"),
    }
    Ok(restored)
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

/// Reduces the longest side to at most `MAX_SIDE` and rounds each side down
/// to a multiple of 16. Sides are then raised only as far as the pipeline
/// requires: at least `MIN_SIDE`, and no more than `MAX_ASPECT`:1. Restore
/// maps any such stretch back to the exact source dimensions.
fn prepared_dimensions((width, height): (u32, u32)) -> (u32, u32) {
    let scale = (f64::from(MAX_SIDE) / f64::from(width.max(height))).min(1.);
    let fit = |side: u32| {
        let scaled = (f64::from(side) * scale).round() as u32;
        (scaled / MULTIPLE * MULTIPLE).max(MIN_SIDE)
    };
    let (width, height) = (fit(width), fit(height));
    let short_for = |long: u32| long.div_ceil(MAX_ASPECT).next_multiple_of(MULTIPLE);
    (width.max(short_for(height)), height.max(short_for(width)))
}

fn prepare(opaque: &RgbaImage, cancellation: &CancellationToken) -> Result<RgbaImage> {
    let (width, height) = prepared_dimensions(opaque.dimensions());
    if (width, height) == opaque.dimensions() {
        return Ok(opaque.clone());
    }
    let prepared = image::imageops::resize(opaque, width, height, FilterType::Lanczos3);
    cancellation.check()?;
    Ok(prepared)
}

fn restore(
    generated: &GrayImage,
    (width, height): (u32, u32),
    cancellation: &CancellationToken,
) -> Result<GrayImage> {
    if generated.dimensions() == (width, height) {
        return Ok(generated.clone());
    }
    let restored = image::imageops::resize(generated, width, height, FilterType::Lanczos3);
    cancellation.check()?;
    Ok(restored)
}

/// Everything that determines the model output. Variable-length fields are
/// length-prefixed so adjacent fields cannot alias.
fn cache_key(revision: &str, prompt: &str, prepared: &RgbaImage) -> String {
    let mut digest = Sha256::new();
    for field in [
        b"diorama-line-art-cache-v1".as_slice(),
        revision.as_bytes(),
        prompt.as_bytes(),
        Sha256::digest(WORKER.as_bytes()).as_slice(),
        GUIDANCE.as_bytes(),
    ] {
        digest.update((field.len() as u64).to_le_bytes());
        digest.update(field);
    }
    digest.update(SEED.to_le_bytes());
    digest.update(STEPS.to_le_bytes());
    digest.update(prepared.width().to_le_bytes());
    digest.update(prepared.height().to_le_bytes());
    digest.update(prepared.as_raw());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn read_cached(path: &Path, dimensions: (u32, u32)) -> Option<GrayImage> {
    let cached = image::open(path).ok()?.into_luma8();
    (cached.dimensions() == dimensions).then_some(cached)
}

/// Readers never see a partial file: write beside the target, then rename.
fn store_cached(path: &Path, generated: &GrayImage) -> Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| AppError::SketchGeneration("Invalid line-art cache path".into()))?;
    fs::create_dir_all(directory)?;
    let temporary = tempfile::Builder::new()
        .prefix(".store-")
        .suffix(".png")
        .tempfile_in(directory)?;
    generated.save_with_format(temporary.path(), image::ImageFormat::Png)?;
    temporary
        .persist(path)
        .map_err(|error| AppError::Io(error.error))?;
    Ok(())
}

/// Keep the `keep` most recently used results. Scratch directories and
/// in-progress `.store-` files are never touched; failures only log.
fn evict_cached(cache: &Path, keep: usize) {
    let entries = match fs::read_dir(cache) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::debug!(%error, "Could not list the line-art cache");
            return;
        }
    };
    let mut results = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name();
            let key = name.to_str()?.strip_suffix(".png")?;
            if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect::<Vec<_>>();
    if results.len() <= keep {
        return;
    }
    results.sort_unstable_by(|a, b| b.cmp(a));
    for (_, path) in &results[keep..] {
        if let Err(error) = fs::remove_file(path) {
            tracing::debug!(%error, path = %path.display(), "Could not evict cached line art");
        }
    }
}

fn verify_model(model: &Path) -> Result<()> {
    if !model.join("model_index.json").is_file() {
        return Err(AppError::SketchGeneration(format!(
            "The FLUX.2 [klein] 4B line-art model is missing at {}. Run {SETUP} to download it (or set DIORAMA_LINE_ART_MODEL)",
            model.display()
        )));
    }
    let marker = fs::read_to_string(model.join(REVISION_MARKER)).unwrap_or_default();
    if marker.trim() != MODEL_REVISION {
        return Err(AppError::SketchGeneration(format!(
            "The line-art model at {} is incomplete or not revision {MODEL_REVISION}. Run {SETUP} to finish or update it",
            model.display()
        )));
    }
    Ok(())
}

fn generate(
    prepared: &RgbaImage,
    cancellation: &CancellationToken,
    runtime: &Runtime,
    cache: &Path,
) -> Result<GrayImage> {
    verify_model(&runtime.model)?;
    // Another window may be running a model that takes minutes; only
    // cancellation ends the wait.
    let _permit = worker_process::inference_permit(cancellation)?;
    fs::create_dir_all(cache)?;
    // The app cache is host-visible for Flatpak workers while private /tmp is
    // not. TempDir removes the input, worker, log, and output together.
    let directory = tempfile::Builder::new().prefix(".run-").tempdir_in(cache)?;
    let input = directory.path().join("image.png");
    let worker = directory.path().join("line_art_worker.py");
    let output = directory.path().join("line-art.png");
    let log = directory.path().join("line-art.log");
    prepared.save(&input)?;
    fs::write(&worker, WORKER)?;
    cancellation.check()?;
    let mut child = worker_process::spawn_logged(
        &mut worker_command(runtime, &worker, &input, &output),
        &log,
    )
    .map_err(|error| {
        AppError::SketchGeneration(format!(
            "Could not start the line-art worker with {}: {error}. Run {SETUP} or set DIORAMA_LINE_ART_PYTHON",
            runtime.python.display()
        ))
    })?;
    let status = worker_process::wait(
        &mut child,
        cancellation,
        runtime.timeout,
        Duration::from_millis(50),
    )
    .map_err(|error| {
        error.into_app_error(AppError::SketchGeneration(format!(
            "Local line art timed out after {} minutes",
            runtime.timeout.as_secs() / 60
        )))
    })?;
    cancellation.check()?;
    if !status.success() {
        return Err(AppError::SketchGeneration(format!(
            "The local line-art model failed ({status}): {}",
            worker_process::log_tail(&log)
        )));
    }
    let generated = image::open(&output)
        .map_err(|error| {
            AppError::SketchGeneration(format!(
                "The line-art worker wrote no readable image ({error}): {}",
                worker_process::log_tail(&log)
            ))
        })?
        .into_luma8();
    if generated.dimensions() != prepared.dimensions() {
        return Err(AppError::SketchGeneration(format!(
            "The line-art worker returned {}×{} for a {}×{} input",
            generated.width(),
            generated.height(),
            prepared.width(),
            prepared.height()
        )));
    }
    Ok(generated)
}

fn worker_command(runtime: &Runtime, worker: &Path, input: &Path, output: &Path) -> Command {
    let mut command = runtime
        .launch
        .command(&runtime.python, runtime.host_library_path.as_deref());
    command
        .arg(worker)
        .arg("--model")
        .arg(&runtime.model)
        .arg("--image")
        .arg(input)
        .arg("--output")
        .arg(output)
        .arg("--prompt")
        .arg(PROMPT)
        .arg("--seed")
        .arg(SEED.to_string())
        .arg("--steps")
        .arg(STEPS.to_string())
        .arg("--guidance")
        .arg(GUIDANCE);
    if let Some(device) = &runtime.device {
        command.arg("--device").arg(device);
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

/// Reject restored line art that is solid or misaligned. Sparse or empty
/// line art is accepted: a flat source has no lines to draw.
///
/// Alignment compares how often ink misses a source edge with how often the
/// same ink misses when displaced by about 2% of the image. That ratio
/// adapts to how busy the source is. Calibrated on a 1536² painted sprite
/// with its FLUX.2 line art: aligned 0.003; displaced by 1% 0.53–0.68, by
/// 3% 0.87–1.01; mirrored or rotated 0.97–1.0.
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
    use image::{Luma, imageops};
    use std::{
        thread,
        time::{Duration, Instant},
    };

    fn token() -> CancellationToken {
        CancellationToken::default()
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
    fn prepared_dimensions_reduce_and_meet_the_pipeline_limits() {
        assert_eq!(prepared_dimensions((1536, 1536)), (1024, 1024));
        assert_eq!(prepared_dimensions((4096, 2048)), (1024, 512));
        assert_eq!(prepared_dimensions((3000, 1000)), (1024, 336));
        assert_eq!(prepared_dimensions((640, 480)), (640, 480));
        assert_eq!(prepared_dimensions((100, 70)), (96, 64));
        // Small sprites are raised to the 64-pixel minimum.
        assert_eq!(prepared_dimensions((48, 48)), (64, 64));
        assert_eq!(prepared_dimensions((5, 3)), (64, 64));
        // Extreme strips are widened to 8:1.
        assert_eq!(prepared_dimensions((1030, 20)), (1024, 128));
        assert_eq!(prepared_dimensions((1, 4000)), (128, 1024));
        for dimensions in [(1537, 911), (17, 1025), (999, 999), (4000, 300), (70, 64)] {
            let (width, height) = prepared_dimensions(dimensions);
            assert!(width % 16 == 0 && height % 16 == 0, "{dimensions:?}");
            assert!(width.min(height) >= MIN_SIDE && width.max(height) <= MAX_SIDE);
            assert!(width.max(height) <= MAX_ASPECT * width.min(height));
        }
        // Inputs within the limits are never enlarged.
        for dimensions in [(640, 480), (64, 512), (1024, 128), (96, 80)] {
            let (width, height) = prepared_dimensions(dimensions);
            assert!(
                width <= dimensions.0 && height <= dimensions.1,
                "{dimensions:?}"
            );
        }
    }

    #[test]
    fn preparation_white_composites_before_reducing() {
        let first = RgbaImage::from_fn(2048, 64, |x, y| {
            if (700..900).contains(&x) && y == 20 {
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
        let prepare_image = |image: &RgbaImage| {
            prepare(&white_composite(image, &token()).unwrap(), &token()).unwrap()
        };
        let (first, second) = (prepare_image(&first), prepare_image(&second));
        assert_eq!(first.dimensions(), (1024, 128));
        assert_eq!(
            first, second,
            "hidden RGB under transparency must not matter"
        );
        assert!(first.pixels().all(|pixel| pixel[3] == 255));
        assert_eq!(first.get_pixel(0, 0).0, [255, 255, 255, 255]);
        let half =
            white_composite(&RgbaImage::from_pixel(1, 1, Rgba([0, 0, 0, 128])), &token()).unwrap();
        assert_eq!(half.get_pixel(0, 0).0, [127, 127, 127, 255]);
    }

    #[test]
    fn restore_returns_the_exact_source_dimensions() {
        let generated = GrayImage::from_pixel(1024, 336, Luma([200]));
        let restored = restore(&generated, (3000, 1000), &token()).unwrap();
        assert_eq!(restored.dimensions(), (3000, 1000));
        let same = restore(&generated, (1024, 336), &token()).unwrap();
        assert_eq!(same, generated);
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
    fn cache_key_is_stable_and_covers_pixels_revision_and_prompt() {
        let prepared = RgbaImage::from_pixel(32, 16, Rgba([10, 20, 30, 255]));
        let key = cache_key(MODEL_REVISION, PROMPT, &prepared);
        assert_eq!(key.len(), 64);
        assert_eq!(key, cache_key(MODEL_REVISION, PROMPT, &prepared.clone()));
        let mut pixel = prepared.clone();
        pixel.put_pixel(31, 15, Rgba([10, 20, 31, 255]));
        let transposed = RgbaImage::from_pixel(16, 32, Rgba([10, 20, 30, 255]));
        for different in [
            cache_key(MODEL_REVISION, PROMPT, &pixel),
            cache_key(MODEL_REVISION, PROMPT, &transposed),
            cache_key(
                "0000000000000000000000000000000000000000",
                PROMPT,
                &prepared,
            ),
            cache_key(MODEL_REVISION, "convert this to line-art", &prepared),
        ] {
            assert_ne!(different, key);
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
            error.contains(MODEL_REVISION) && error.contains(SETUP),
            "{error}"
        );
        fs::write(model.path().join(REVISION_MARKER), "0123abc\n").unwrap();
        assert!(verify_model(model.path()).is_err());
        fs::write(
            model.path().join(REVISION_MARKER),
            format!("{MODEL_REVISION}\n"),
        )
        .unwrap();
        verify_model(model.path()).unwrap();
    }

    #[test]
    fn host_launch_passes_the_fixed_generation_parameters_and_spaced_paths() {
        let runtime = Runtime {
            python: PathBuf::from("/host/line art/venv/bin/python"),
            model: PathBuf::from("/host/cache/diorama/flux2 klein"),
            device: None,
            launch: Launch::FlatpakHost {
                launcher: PathBuf::from("/sandbox/bin/flatpak-spawn"),
            },
            host_library_path: Some("/opt/rocm/lib:/host/lib with spaces".into()),
            timeout: TIMEOUT,
        };
        let arguments = |command: &Command| {
            command
                .get_args()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let command = worker_command(
            &runtime,
            Path::new("/cache/run/line_art_worker.py"),
            Path::new("/cache/run/input image.png"),
            Path::new("/cache/run/line art.png"),
        );
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
                "--model",
                "/host/cache/diorama/flux2 klein",
                "--image",
                "/cache/run/input image.png",
                "--output",
                "/cache/run/line art.png",
                "--prompt",
                "convert this to line-art, remove thin lines",
                "--seed",
                "0",
                "--steps",
                "4",
                "--guidance",
                "1.0",
            ]
        );
        let runtime = Runtime {
            device: Some("cpu".into()),
            launch: Launch::Direct,
            ..runtime
        };
        let command = worker_command(
            &runtime,
            Path::new("/w.py"),
            Path::new("/i.png"),
            Path::new("/o.png"),
        );
        assert_eq!(command.get_program(), runtime.python.as_os_str());
        assert_eq!(arguments(&command).last().map(String::as_str), Some("cpu"));
    }

    #[test]
    fn cancellation_stops_before_runtime_resolution() {
        let cancellation = token();
        cancellation.cancel();
        assert!(matches!(
            sketch(&RgbaImage::new(2, 2), &cancellation),
            Err(AppError::Cancelled)
        ));
    }

    #[cfg(unix)]
    mod worker {
        use super::*;

        /// A fake interpreter that records each launch, then runs `body`
        /// with the worker's arguments.
        fn fake_runtime(root: &Path, body: &str) -> Runtime {
            let python = root.join("python");
            worker_process::write_test_executable(
                &python,
                &format!("#!/bin/sh\necho run >> \"$(dirname \"$0\")/launches\"\n{body}\n"),
            );
            let model = root.join("model dir");
            fs::create_dir_all(&model).unwrap();
            fs::write(model.join("model_index.json"), "{}").unwrap();
            fs::write(model.join(REVISION_MARKER), MODEL_REVISION).unwrap();
            Runtime {
                python,
                model,
                device: None,
                launch: Launch::Direct,
                host_library_path: None,
                timeout: Duration::from_secs(10),
            }
        }

        const COPY_RESULT: &str = "while [ $# -gt 0 ]; do if [ \"$1\" = --output ]; then cp \"$(dirname \"$0\")/result.png\" \"$2\"; exit 0; fi; shift; done; exit 1";

        fn launches(root: &Path) -> usize {
            fs::read_to_string(root.join("launches"))
                .map(|log| log.lines().count())
                .unwrap_or(0)
        }

        fn cache_entries(cache: &Path) -> Vec<String> {
            fs::read_dir(cache)
                .map(|entries| {
                    entries
                        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default()
        }

        #[test]
        fn a_cache_hit_skips_the_worker_and_is_restored_and_checked() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache with spaces");
            // 1100 px is reduced to 1024 for inference and restored after.
            let source = shapes(1100);
            let prepared_source = shapes(1024);
            outlines(&prepared_source)
                .save(root.path().join("result.png"))
                .unwrap();
            let runtime = || Ok(fake_runtime(root.path(), COPY_RESULT));
            let first = sketch_in(&source, &token(), &cache, runtime).unwrap();
            assert_eq!(first.dimensions(), (1100, 1100));
            assert_eq!(launches(root.path()), 1);
            let entries = cache_entries(&cache);
            assert_eq!(entries.len(), 1, "only the result remains: {entries:?}");
            assert!(entries[0].ends_with(".png") && entries[0].len() == 68);

            let second = sketch_in(&source, &token(), &cache, || {
                panic!("a cache hit must not resolve or launch the worker")
            })
            .unwrap();
            assert_eq!(second, first);
            assert_eq!(launches(root.path()), 1);

            // Different pixels are a different key and run the worker again.
            let mut changed = source.clone();
            changed.put_pixel(0, 0, Rgba([254, 255, 255, 255]));
            sketch_in(&changed, &token(), &cache, runtime).unwrap();
            assert_eq!(launches(root.path()), 2);
            assert_eq!(cache_entries(&cache).len(), 2);
        }

        #[test]
        fn empty_line_art_for_a_flat_source_is_accepted_and_cached() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let flat = RgbaImage::from_pixel(64, 48, Rgba([90, 140, 60, 255]));
            GrayImage::from_pixel(64, 64, Luma([255]))
                .save(root.path().join("result.png"))
                .unwrap();
            let runtime = || Ok(fake_runtime(root.path(), COPY_RESULT));
            // 64×48 is stretched to the pipeline's 64×64 and restored.
            let first = sketch_in(&flat, &token(), &cache, runtime).unwrap();
            assert_eq!(first.dimensions(), (64, 48));
            assert!(first.pixels().all(|pixel| pixel[0] == 255));
            assert_eq!(cache_entries(&cache).len(), 1);
            let second = sketch_in(&flat, &token(), &cache, || panic!("no launch")).unwrap();
            assert_eq!(second, first);
            assert_eq!(launches(root.path()), 1);
        }

        #[test]
        fn cache_eviction_keeps_the_most_recently_used_results() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let source = shapes(64);
            outlines(&source)
                .save(root.path().join("result.png"))
                .unwrap();
            let runtime = || Ok(fake_runtime(root.path(), COPY_RESULT));
            let hit = sketch_in(&source, &token(), &cache, runtime).unwrap();
            let entry = cache.join(&cache_entries(&cache)[0]);
            // Older unrelated results, a scratch directory, and a partial store.
            let epoch = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
            let aged = |path: &Path, age: u64| {
                File::options()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_modified(epoch + Duration::from_secs(age))
                    .unwrap();
            };
            for index in 0..MAX_CACHED_RESULTS as u64 + 3 {
                let path = cache.join(format!("{index:064x}.png"));
                fs::write(&path, b"x").unwrap();
                aged(&path, index + 10);
            }
            fs::create_dir(cache.join(".run-keep")).unwrap();
            fs::write(cache.join(".store-keep.png"), b"x").unwrap();
            aged(&entry, 0);
            // A hit marks the oldest entry as the most recently used.
            assert_eq!(
                sketch_in(&source, &token(), &cache, || panic!("no launch")).unwrap(),
                hit
            );
            // A fresh result is stored, which evicts beyond the cap.
            let mut changed = source.clone();
            changed.put_pixel(0, 0, Rgba([254, 255, 255, 255]));
            sketch_in(&changed, &token(), &cache, runtime).unwrap();
            let names = cache_entries(&cache);
            let results = names.iter().filter(|name| name.len() == 68).count();
            assert_eq!(results, MAX_CACHED_RESULTS);
            assert!(entry.is_file(), "the touched hit must survive eviction");
            // 67 aged + the hit + the fresh result: the five oldest go.
            for evicted in 0..5_u64 {
                assert!(!cache.join(format!("{evicted:064x}.png")).exists());
            }
            assert!(cache.join(format!("{:064x}.png", 5)).is_file());
            assert_eq!(launches(root.path()), 2);
            assert!(cache.join(".run-keep").is_dir() && cache.join(".store-keep.png").is_file());
            // A missing directory is not an error.
            evict_cached(&root.path().join("missing"), 1);
        }

        #[test]
        fn a_cached_entry_that_fails_the_checks_is_dropped() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let source = shapes(64);
            let prepared = prepare(&white_composite(&source, &token()).unwrap(), &token()).unwrap();
            let entry = cache.join(format!(
                "{}.png",
                cache_key(MODEL_REVISION, PROMPT, &prepared)
            ));
            store_cached(&entry, &GrayImage::from_pixel(64, 64, Luma([0]))).unwrap();
            let error = sketch_in(&source, &token(), &cache, || panic!("no launch")).unwrap_err();
            assert!(error.to_string().contains("nearly solid"), "{error}");
            assert!(!entry.exists());
        }

        #[test]
        fn failed_or_rejected_runs_are_not_cached_and_surface_the_log_tail() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let source = shapes(128);
            let error = sketch_in(&source, &token(), &cache, || {
                Ok(fake_runtime(
                    root.path(),
                    "echo 'RuntimeError: HIP out of memory' >&2; exit 7",
                ))
            })
            .unwrap_err();
            assert!(matches!(error, AppError::SketchGeneration(_)));
            assert!(error.to_string().contains("HIP out of memory"), "{error}");

            // Misaligned output passes the worker but fails the check.
            let shift = 4;
            translated(&outlines(&source), shift, shift)
                .save(root.path().join("result.png"))
                .unwrap();
            let error = sketch_in(&source, &token(), &cache, || {
                Ok(fake_runtime(root.path(), COPY_RESULT))
            })
            .unwrap_err();
            assert!(error.to_string().contains("does not line up"), "{error}");

            // A wrong-size result is rejected as well.
            GrayImage::from_pixel(16, 16, Luma([0]))
                .save(root.path().join("result.png"))
                .unwrap();
            let error = sketch_in(&source, &token(), &cache, || {
                Ok(fake_runtime(root.path(), COPY_RESULT))
            })
            .unwrap_err();
            assert!(error.to_string().contains("16×16"), "{error}");
            assert!(cache_entries(&cache).is_empty());
        }

        #[test]
        fn missing_runtime_or_model_fails_before_launch() {
            let root = tempfile::tempdir().unwrap();
            let mut runtime = fake_runtime(root.path(), "exit 0");
            fs::remove_file(runtime.model.join(REVISION_MARKER)).unwrap();
            let error = sketch_in(&shapes(64), &token(), &root.path().join("cache"), || {
                Ok(runtime)
            })
            .unwrap_err();
            assert!(error.to_string().contains(SETUP), "{error}");
            assert_eq!(launches(root.path()), 0);

            runtime = fake_runtime(root.path(), "exit 0");
            runtime.python = root.path().join("no such python");
            let error = sketch_in(&shapes(64), &token(), &root.path().join("cache"), || {
                Ok(runtime)
            })
            .unwrap_err();
            assert!(error.to_string().contains("Could not start"), "{error}");
        }

        #[test]
        fn cancellation_kills_the_worker_and_leaves_no_files() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let runtime = fake_runtime(
                root.path(),
                "touch \"$(dirname \"$0\")/started\"; exec sleep 60",
            );
            let source = shapes(64);
            let cancellation = token();
            let started = root.path().join("started");
            let begun = Instant::now();
            thread::scope(|scope| {
                let worker =
                    scope.spawn(|| sketch_in(&source, &cancellation, &cache, || Ok(runtime)));
                let deadline = Instant::now() + Duration::from_secs(5);
                while !started.exists() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(5));
                }
                assert!(started.exists());
                cancellation.cancel();
                assert!(matches!(worker.join().unwrap(), Err(AppError::Cancelled)));
            });
            assert!(begun.elapsed() < Duration::from_secs(30));
            assert!(cache_entries(&cache).is_empty());
        }

        #[test]
        fn timeout_kills_the_worker() {
            let root = tempfile::tempdir().unwrap();
            let mut runtime = fake_runtime(root.path(), "exec sleep 60");
            runtime.timeout = Duration::from_millis(100);
            let begun = Instant::now();
            let error = sketch_in(&shapes(64), &token(), &root.path().join("cache"), || {
                Ok(runtime)
            })
            .unwrap_err();
            assert!(error.to_string().contains("timed out"), "{error}");
            assert!(begun.elapsed() < Duration::from_secs(30));
        }
    }

    /// Prints alignment measurements for a real source and its model line
    /// art, and checks that displaced or mirrored copies are rejected:
    ///
    /// `DIORAMA_LINE_ART_CALIBRATION_SOURCE=src.png DIORAMA_LINE_ART_CALIBRATION_SKETCH=art.png cargo test --lib line_art_calibration -- --ignored --nocapture`
    #[test]
    #[ignore = "requires a real source image and its model line art"]
    fn line_art_calibration() {
        let path = |name| std::env::var_os(name).unwrap_or_else(|| panic!("set {name}"));
        let source = image::open(path("DIORAMA_LINE_ART_CALIBRATION_SOURCE"))
            .unwrap()
            .into_rgba8();
        let opaque = white_composite(&source, &token()).unwrap();
        let generated = image::open(path("DIORAMA_LINE_ART_CALIBRATION_SKETCH"))
            .unwrap()
            .into_luma8();
        let sketch = restore(&generated, source.dimensions(), &token()).unwrap();
        let longest = i64::from(source.width().max(source.height()));
        let report = |name: &str, sketch: &GrayImage| {
            let measured = measure(sketch, &opaque, &token()).unwrap();
            let verdict = check_line_art(sketch, &opaque, &token());
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
                assert!(!report(
                    &format!("shift {axis} {percent}%"),
                    &translated(&sketch, dx, dy)
                ));
            }
        }
        assert!(!report("mirror h", &imageops::flip_horizontal(&sketch)));
        assert!(!report("mirror v", &imageops::flip_vertical(&sketch)));
        assert!(!report("rotate 180", &imageops::rotate180(&sketch)));
    }
}
