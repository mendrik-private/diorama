//! Local, cancellable LaMa inference for a selection's reusable clean background.
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Mutex, TryLockError},
    thread,
    time::{Duration, Instant},
};

use image::{GrayImage, Luma, RgbaImage};

use crate::document::CancellationToken;
use crate::error::{AppError, Result};
use crate::tools::{crop::CropBounds, python_runtime, selection};

const WORKER: &str = include_str!("lama_worker.py");
static INFERENCE: Mutex<()> = Mutex::new(());

struct Runtime {
    python: PathBuf,
    model: PathBuf,
    device: String,
    timeout: Duration,
    launch: Launch,
    temporary_root: PathBuf,
    host_library_path: Option<String>,
    bundled: bool,
}

impl Runtime {
    fn from_environment() -> Result<Self> {
        let launch = launch_mode_from(Path::new("/.flatpak-info"));
        let cache = cache_directory()?;
        let configuration = runtime_configuration(&launch);
        let explicit_python = std::env::var_os("DIORAMA_LAMA_PYTHON").map(PathBuf::from);
        let (python, bundled) = python_selection(
            explicit_python,
            configuration.python,
            python_runtime::bundled_runtime_available(),
        );
        Ok(Self {
            python,
            model: std::env::var_os("DIORAMA_LAMA_MODEL")
                .map(PathBuf::from)
                .unwrap_or_else(|| default_model_path(&cache)),
            device: std::env::var("DIORAMA_LAMA_DEVICE").unwrap_or_else(|_| "cpu".into()),
            timeout: Duration::from_secs(120),
            launch: if bundled { Launch::Direct } else { launch },
            temporary_root: cache.join("diorama/inpainting"),
            host_library_path: configuration.library_path,
            bundled,
        })
    }
}

fn python_selection(
    explicit: Option<PathBuf>,
    configured: Option<PathBuf>,
    bundled_available: bool,
) -> (PathBuf, bool) {
    if let Some(python) = explicit {
        return (python, false);
    }
    if bundled_available {
        return ("python3".into(), true);
    }
    (configured.unwrap_or_else(|| "python3".into()), false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Launch {
    Direct,
    FlatpakHost { launcher: PathBuf },
}

fn launch_mode_from(flatpak_info: &Path) -> Launch {
    if flatpak_info.is_file() {
        Launch::FlatpakHost {
            launcher: PathBuf::from("flatpak-spawn"),
        }
    } else {
        Launch::Direct
    }
}

fn cache_directory() -> Result<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .ok_or_else(|| AppError::Inpainting("HOME is unavailable; configure local LaMa".into()))
}

/// Prefer an app-managed model when it exists, otherwise reuse the model set
/// up by the host command. The latter avoids a second 205 MB download in a
/// Flatpak whose cache directory is intentionally app-scoped.
fn default_model_path(cache: &Path) -> PathBuf {
    let host_cache = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".cache"));
    default_model_path_with_host_cache(cache, host_cache.as_deref())
}

fn default_model_path_with_host_cache(cache: &Path, host_cache: Option<&Path>) -> PathBuf {
    let app_model = cache.join("diorama/big-lama.pt");
    if app_model.is_file() {
        return app_model;
    }
    host_cache
        .map(|cache| cache.join("diorama/big-lama.pt"))
        .filter(|model| model.is_file())
        .unwrap_or(app_model)
}

#[derive(Default)]
struct RuntimeConfiguration {
    python: Option<PathBuf>,
    library_path: Option<String>,
}

fn runtime_configuration(launch: &Launch) -> RuntimeConfiguration {
    let config = std::env::var_os("DIORAMA_LAMA_RUNTIME_CONFIG")
        .map(PathBuf::from)
        .or_else(|| default_runtime_config_path(launch));
    config
        .and_then(|config| fs::read_to_string(config).ok())
        .map(|contents| runtime_configuration_from_contents(&contents))
        .unwrap_or_default()
}

fn runtime_configuration_from_contents(contents: &str) -> RuntimeConfiguration {
    let mut configuration = RuntimeConfiguration::default();
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "python" => configuration.python = Some(PathBuf::from(value)),
            "library_path" => configuration.library_path = Some(value.to_owned()),
            _ => {}
        }
    }
    configuration
}

fn default_runtime_config_path(launch: &Launch) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(match launch {
        // setup-lama.py is normally run on the host, so use the host's config
        // location rather than Flatpak's app-scoped XDG_CONFIG_HOME.
        Launch::FlatpakHost { .. } => home.join(".config/diorama/lama-runtime.conf"),
        Launch::Direct => std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".config"))
            .join("diorama/lama-runtime.conf"),
    })
}

pub fn clean_background(
    image: &RgbaImage,
    bounds: CropBounds,
    mask: &GrayImage,
    cancellation: &CancellationToken,
) -> Result<RgbaImage> {
    clean_background_with_runtime(image, bounds, mask, cancellation, None)
}

fn clean_background_with_runtime(
    image: &RgbaImage,
    bounds: CropBounds,
    mask: &GrayImage,
    cancellation: &CancellationToken,
    supplied_runtime: Option<&Runtime>,
) -> Result<RgbaImage> {
    cancellation.check()?;
    // Validate all bounds before cropping or modifying any pixels.
    if bounds.width == 0
        || bounds.height == 0
        || bounds
            .x
            .checked_add(bounds.width)
            .is_none_or(|right| right > image.width())
        || bounds
            .y
            .checked_add(bounds.height)
            .is_none_or(|bottom| bottom > image.height())
        || mask.dimensions() != (bounds.width, bounds.height)
    {
        return Err(AppError::InvalidDimensions);
    }
    if !mask.pixels().any(|pixel| pixel[0] != 0) {
        return Err(AppError::NoVisibleContent);
    }
    let mut background = image.clone();
    // A transparent sprite has no hidden RGB scene to reconstruct.
    if selection::detected_background(image).is_some_and(|color| color[3] == 0) {
        selection::clear_masked(&mut background, bounds, mask, [0; 4])?;
        return Ok(background);
    }
    let (context, removal) = context_mask(image.dimensions(), bounds, mask);
    let fragment = selection::crop(image, context)?;
    // Do not resolve or unpack a bundled interpreter until a selection really
    // requires model inference.
    let loaded_runtime;
    let runtime = match supplied_runtime {
        Some(runtime) => runtime,
        None => {
            loaded_runtime = Runtime::from_environment()?;
            &loaded_runtime
        }
    };
    let repaired = run_lama(&fragment, &removal, cancellation, runtime)?;
    cancellation.check()?;
    // Model input is dilated to exclude fringe contamination, but replacement
    // is confined to the actual cutout. Holes and surrounding pixels stay exact.
    for (x, y, alpha) in mask.enumerate_pixels() {
        if alpha[0] == 0 {
            continue;
        }
        let source = image.get_pixel(bounds.x + x, bounds.y + y);
        let mut pixel = *repaired.get_pixel(bounds.x + x - context.x, bounds.y + y - context.y);
        pixel[3] = source[3];
        background.put_pixel(bounds.x + x, bounds.y + y, pixel);
    }
    Ok(background)
}

fn context_mask(
    dimensions: (u32, u32),
    bounds: CropBounds,
    mask: &GrayImage,
) -> (CropBounds, GrayImage) {
    let margin = (bounds.width.max(bounds.height) / 2).clamp(64, 256);
    let x = bounds.x.saturating_sub(margin);
    let y = bounds.y.saturating_sub(margin);
    let right = (bounds.x + bounds.width)
        .saturating_add(margin)
        .min(dimensions.0);
    let bottom = (bounds.y + bounds.height)
        .saturating_add(margin)
        .min(dimensions.1);
    let context = CropBounds {
        x,
        y,
        width: right - x,
        height: bottom - y,
    };
    let mut removal = GrayImage::new(context.width, context.height);
    for (mx, my, alpha) in mask.enumerate_pixels() {
        if alpha[0] == 0 {
            continue;
        }
        let cx = bounds.x + mx - x;
        let cy = bounds.y + my - y;
        for yy in cy.saturating_sub(2)..=cy.saturating_add(2).min(context.height - 1) {
            for xx in cx.saturating_sub(2)..=cx.saturating_add(2).min(context.width - 1) {
                removal.put_pixel(xx, yy, Luma([255]));
            }
        }
    }
    (context, removal)
}

fn run_lama(
    image: &RgbaImage,
    mask: &GrayImage,
    cancellation: &CancellationToken,
    runtime: &Runtime,
) -> Result<RgbaImage> {
    // Bound concurrent model memory even when several windows prepare cutouts.
    let waiting = Instant::now();
    let _permit = loop {
        cancellation.check()?;
        match INFERENCE.try_lock() {
            Ok(permit) => break permit,
            Err(TryLockError::Poisoned(error)) => break error.into_inner(),
            Err(TryLockError::WouldBlock) => {
                if waiting.elapsed() >= runtime.timeout {
                    return Err(AppError::Inpainting(
                        "Timed out waiting for local LaMa".into(),
                    ));
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
    };
    if !runtime.model.is_file() {
        return Err(AppError::Inpainting(
            "LaMa model is missing. Run python3 build-aux/setup-lama.py or set DIORAMA_LAMA_MODEL"
                .into(),
        ));
    }
    cancellation.check()?;
    let bundled = runtime
        .bundled
        .then(|| python_runtime::materialize(&cache_directory()?, cancellation))
        .transpose()?;
    fs::create_dir_all(&runtime.temporary_root)?;
    // A host-launched worker cannot see Flatpak's private /tmp. App cache is
    // explicitly shared with the host and TempDir removes every input/output
    // artifact after the worker exits.
    let directory = tempfile::Builder::new()
        .prefix("lama-")
        .tempdir_in(&runtime.temporary_root)?;
    let input = directory.path().join("image.png");
    let matte = directory.path().join("mask.png");
    let output = directory.path().join("filled.png");
    let log = directory.path().join("lama.log");
    image.save(&input)?;
    mask.save(&matte)?;
    let log_file = File::create(&log)?;
    cancellation.check()?;
    let mut child = lama_command(runtime, bundled.as_ref(), &input, &matte, &output)
        .stdin(Stdio::null())
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .spawn()
        .map_err(|error| {
            AppError::Inpainting(format!("Could not start local LaMa worker: {error}"))
        })?;
    let started = Instant::now();
    let status = loop {
        if let Err(error) = cancellation.check() {
            kill_and_reap(&mut child);
            return Err(error);
        }
        if started.elapsed() >= runtime.timeout {
            kill_and_reap(&mut child);
            return Err(AppError::Inpainting("Local LaMa timed out".into()));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) => {
                kill_and_reap(&mut child);
                return Err(error.into());
            }
        }
    };
    cancellation.check()?;
    if !status.success() {
        let mut log = File::open(log)?;
        let length = log.metadata()?.len();
        log.seek(SeekFrom::Start(length.saturating_sub(4096)))?;
        let mut tail = Vec::new();
        log.take(4096).read_to_end(&mut tail)?;
        return Err(AppError::Inpainting(format!(
            "Local LaMa failed: {}",
            String::from_utf8_lossy(&tail).trim()
        )));
    }
    let repaired = image::open(output)?.into_rgba8();
    if repaired.dimensions() != image.dimensions() {
        return Err(AppError::InvalidDimensions);
    }
    cancellation.check()?;
    Ok(repaired)
}

fn lama_command(
    runtime: &Runtime,
    bundled: Option<&python_runtime::MaterializedRuntime>,
    input: &Path,
    matte: &Path,
    output: &Path,
) -> Command {
    let mut command = match bundled {
        Some(bundled) => {
            let mut command = Command::new(&bundled.python);
            command
                .env_remove("PYTHONHOME")
                .env_remove("PYTHONPATH")
                .env_remove("PYTHONUSERBASE")
                .env_remove("LD_LIBRARY_PATH")
                .env_remove("LD_PRELOAD")
                .env("PYTHONNOUSERSITE", "1")
                .env("PYTHONSAFEPATH", "1")
                .arg("-I")
                .arg(&bundled.worker);
            command
        }
        None => match &runtime.launch {
            Launch::Direct => Command::new(&runtime.python),
            Launch::FlatpakHost { launcher } => {
                let mut command = Command::new(launcher);
                command.args([
                    "--host",
                    "--watch-bus",
                    "--unset-env=LD_LIBRARY_PATH",
                    "--unset-env=LD_PRELOAD",
                ]);
                if let Some(library_path) = &runtime.host_library_path {
                    // Restore only the host loader path verified by setup-lama.py,
                    // after stripping the sandbox's loader environment.
                    command.arg(format!("--env=LD_LIBRARY_PATH={library_path}"));
                }
                command.arg(&runtime.python);
                command
            }
        },
    };
    if bundled.is_none() {
        command.arg("-c").arg(WORKER);
    }
    command
        .arg("--model")
        .arg(&runtime.model)
        .arg("--image")
        .arg(input)
        .arg("--mask")
        .arg(matte)
        .arg("--output")
        .arg(output)
        .arg("--device")
        .arg(&runtime.device);
    command
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn bounds() -> CropBounds {
        CropBounds {
            x: 3,
            y: 2,
            width: 3,
            height: 3,
        }
    }

    fn mask() -> GrayImage {
        GrayImage::from_fn(3, 3, |x, y| Luma([if x == 1 && y == 1 { 0 } else { 255 }]))
    }

    #[cfg(unix)]
    fn fake_runtime(root: &std::path::Path, body: &str) -> Runtime {
        use std::os::unix::fs::PermissionsExt;
        let python = root.join("python");
        std::fs::write(&python, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
        let model = root.join("model.pt");
        std::fs::write(&model, b"fake-model").unwrap();
        Runtime {
            python,
            model,
            device: "cpu".into(),
            timeout: Duration::from_secs(2),
            launch: Launch::Direct,
            temporary_root: root.join("cache with spaces"),
            host_library_path: None,
            bundled: false,
        }
    }

    #[test]
    fn launch_mode_detects_flatpak_explicitly() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("flatpak-info");
        assert_eq!(launch_mode_from(&marker), Launch::Direct);
        std::fs::write(&marker, "[Instance]\n").unwrap();
        assert_eq!(
            launch_mode_from(&marker),
            Launch::FlatpakHost {
                launcher: PathBuf::from("flatpak-spawn"),
            }
        );
    }

    #[test]
    fn default_model_prefers_the_app_cache_then_an_existing_host_model() {
        let root = tempfile::tempdir().unwrap();
        let app_cache = root.path().join("app-cache");
        let host_cache = root.path().join("host-cache");
        let app_model = app_cache.join("diorama/big-lama.pt");
        let host_model = host_cache.join("diorama/big-lama.pt");
        assert_eq!(
            default_model_path_with_host_cache(&app_cache, Some(&host_cache)),
            app_model
        );
        std::fs::create_dir_all(host_model.parent().unwrap()).unwrap();
        std::fs::write(&host_model, b"host model").unwrap();
        assert_eq!(
            default_model_path_with_host_cache(&app_cache, Some(&host_cache)),
            host_model
        );
        std::fs::create_dir_all(app_model.parent().unwrap()).unwrap();
        std::fs::write(&app_model, b"app model").unwrap();
        assert_eq!(
            default_model_path_with_host_cache(&app_cache, Some(&host_cache)),
            app_model
        );
    }

    #[test]
    fn configured_runtime_uses_recorded_values_without_normalizing_them() {
        let configuration = runtime_configuration_from_contents(
            "# comment\npython= /venv/bin/python \nlibrary_path=/host/lib with spaces:/host/rocm/lib\n",
        );
        assert_eq!(
            configuration.python,
            Some(PathBuf::from("/venv/bin/python"))
        );
        assert_eq!(
            configuration.library_path.as_deref(),
            Some("/host/lib with spaces:/host/rocm/lib")
        );
    }

    #[test]
    fn runtime_selection_prefers_explicit_then_bundle_then_legacy_configuration() {
        let explicit = PathBuf::from("/explicit/python");
        let configured = PathBuf::from("/configured/python");
        assert_eq!(
            python_selection(Some(explicit.clone()), Some(configured.clone()), true),
            (explicit, false)
        );
        assert_eq!(
            python_selection(None, Some(configured.clone()), true),
            (PathBuf::from("python3"), true)
        );
        assert_eq!(
            python_selection(None, Some(configured), false),
            (PathBuf::from("/configured/python"), false)
        );
        assert_eq!(
            python_selection(None, None, false),
            (PathBuf::from("python3"), false)
        );
    }

    #[test]
    fn host_launcher_preserves_lama_arguments_and_spaced_paths() {
        let runtime = Runtime {
            python: PathBuf::from("/host/python with deps"),
            model: PathBuf::from("/host/models/big lama.pt"),
            device: "cpu".into(),
            timeout: Duration::from_secs(1),
            launch: Launch::FlatpakHost {
                launcher: PathBuf::from("/sandbox/bin/flatpak spawn"),
            },
            temporary_root: PathBuf::from("/host/cache/diorama/inpainting"),
            host_library_path: Some("/host/lib with spaces:/host/rocm/lib".into()),
            bundled: false,
        };
        let command = lama_command(
            &runtime,
            None,
            Path::new("/host/cache/input image.png"),
            Path::new("/host/cache/mask image.png"),
            Path::new("/host/cache/output image.png"),
        );
        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            command.get_program(),
            Path::new("/sandbox/bin/flatpak spawn")
        );
        assert_eq!(
            &arguments[..7],
            [
                "--host",
                "--watch-bus",
                "--unset-env=LD_LIBRARY_PATH",
                "--unset-env=LD_PRELOAD",
                "--env=LD_LIBRARY_PATH=/host/lib with spaces:/host/rocm/lib",
                "/host/python with deps",
                "-c",
            ]
        );
        assert_eq!(arguments[7], WORKER);
        assert!(
            !arguments
                .iter()
                .any(|argument| argument.starts_with("--env=LD_PRELOAD="))
        );
        assert_eq!(
            &arguments[8..],
            [
                "--model",
                "/host/models/big lama.pt",
                "--image",
                "/host/cache/input image.png",
                "--mask",
                "/host/cache/mask image.png",
                "--output",
                "/host/cache/output image.png",
                "--device",
                "cpu",
            ]
        );
    }

    #[test]
    fn inference_mask_has_context_and_dilation_including_at_canvas_edges() {
        let bounds = CropBounds {
            x: 0,
            y: 0,
            width: 3,
            height: 3,
        };
        let (context, mask) = context_mask((80, 70), bounds, &mask());
        assert_eq!(
            (context.x, context.y, context.width, context.height),
            (0, 0, 67, 67)
        );
        assert_eq!(mask.get_pixel(4, 4)[0], 255);
        assert_eq!(mask.get_pixel(5, 5)[0], 0);
        assert_eq!(mask.get_pixel(66, 66)[0], 0);
    }

    #[cfg(unix)]
    #[test]
    fn repair_merges_only_foreground_and_preserves_holes_alpha_and_context() {
        let root = tempfile::tempdir().unwrap();
        let runtime = fake_runtime(
            root.path(),
            "while [ $# -gt 0 ]; do if [ \"$1\" = --output ]; then cp \"$(dirname \"$0\")/result.png\" \"$2\"; exit; fi; shift; done\nexit 1",
        );
        let image = RgbaImage::from_fn(10, 8, |x, y| Rgba([x as u8 * 20, y as u8 * 20, 40, 128]));
        RgbaImage::from_pixel(10, 8, Rgba([90, 80, 70, 255]))
            .save(root.path().join("result.png"))
            .unwrap();
        let repaired = clean_background_with_runtime(
            &image,
            bounds(),
            &mask(),
            &CancellationToken::default(),
            Some(&runtime),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_dir(root.path().join("cache with spaces"))
                .unwrap()
                .count(),
            0,
            "temporary host-visible worker files must be removed"
        );
        for (x, y, pixel) in repaired.enumerate_pixels() {
            let selected = (3..6).contains(&x) && (2..5).contains(&y) && (x, y) != (4, 3);
            assert_eq!(
                pixel.0,
                if selected {
                    [90, 80, 70, 128]
                } else {
                    image.get_pixel(x, y).0
                }
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn transparent_sprite_clears_without_starting_the_model() {
        let root = tempfile::tempdir().unwrap();
        let runtime = fake_runtime(root.path(), "exit 99");
        let mut image = RgbaImage::new(10, 8);
        image.put_pixel(3, 2, Rgba([255, 0, 0, 255]));
        let cleaned = clean_background_with_runtime(
            &image,
            bounds(),
            &mask(),
            &CancellationToken::default(),
            Some(&runtime),
        )
        .unwrap();
        assert!(cleaned.pixels().all(|pixel| pixel[3] == 0));
    }

    #[cfg(unix)]
    #[test]
    fn runtime_rejects_failed_exit_and_wrong_output_dimensions() {
        let root = tempfile::tempdir().unwrap();
        let mut runtime = fake_runtime(root.path(), "echo test-lama-failure >&2; exit 7");
        let image = RgbaImage::from_pixel(10, 8, Rgba([90, 80, 70, 255]));
        let error = clean_background_with_runtime(
            &image,
            bounds(),
            &mask(),
            &CancellationToken::default(),
            Some(&runtime),
        )
        .unwrap_err();
        assert!(error.to_string().contains("test-lama-failure"));
        runtime = fake_runtime(
            root.path(),
            "while [ $# -gt 0 ]; do if [ \"$1\" = --output ]; then cp \"$(dirname \"$0\")/result.png\" \"$2\"; exit; fi; shift; done",
        );
        RgbaImage::new(1, 1)
            .save(root.path().join("result.png"))
            .unwrap();
        assert!(matches!(
            clean_background_with_runtime(
                &image,
                bounds(),
                &mask(),
                &CancellationToken::default(),
                Some(&runtime)
            ),
            Err(AppError::InvalidDimensions)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_and_timeout_stop_the_worker() {
        let root = tempfile::tempdir().unwrap();
        let mut runtime = fake_runtime(
            root.path(),
            "touch \"$(dirname \"$0\")/started\"; exec sleep 60",
        );
        let image = RgbaImage::from_pixel(10, 8, Rgba([90, 80, 70, 255]));
        let token = CancellationToken::default();
        let started = root.path().join("started");
        thread::scope(|scope| {
            let worker = scope.spawn(|| {
                clean_background_with_runtime(&image, bounds(), &mask(), &token, Some(&runtime))
            });
            let deadline = Instant::now() + Duration::from_secs(3);
            while !started.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            assert!(started.exists());
            token.cancel();
            assert!(matches!(worker.join().unwrap(), Err(AppError::Cancelled)));
        });
        runtime.timeout = Duration::from_millis(50);
        let error = clean_background_with_runtime(
            &image,
            bounds(),
            &mask(),
            &CancellationToken::default(),
            Some(&runtime),
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_kills_and_reaps_the_flatpak_launcher() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let mut runtime = fake_runtime(root.path(), "exit 99");
        let launcher = root.path().join("flatpak-spawn");
        std::fs::write(&launcher, "#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        runtime.launch = Launch::FlatpakHost { launcher };
        let image = RgbaImage::from_pixel(10, 8, Rgba([90, 80, 70, 255]));
        let token = CancellationToken::default();
        let worker_token = token.clone();
        let worker = thread::spawn(move || {
            clean_background_with_runtime(&image, bounds(), &mask(), &worker_token, Some(&runtime))
        });
        thread::sleep(Duration::from_millis(60));
        token.cancel();
        assert!(matches!(worker.join().unwrap(), Err(AppError::Cancelled)));
    }

    #[test]
    #[ignore = "requires local LaMa model and Python torch/numpy/Pillow"]
    fn local_lama_removes_a_colored_object_and_preserves_unmasked_pixels() {
        let mut image = RgbaImage::from_pixel(128, 96, Rgba([90, 100, 110, 255]));
        let bounds = CropBounds {
            x: 48,
            y: 32,
            width: 24,
            height: 32,
        };
        let mask = GrayImage::from_pixel(bounds.width, bounds.height, Luma([255]));
        for y in 32..64 {
            for x in 48..72 {
                image.put_pixel(x, y, Rgba([240, 10, 10, 255]));
            }
        }
        let start = Instant::now();
        let repaired =
            clean_background(&image, bounds, &mask, &CancellationToken::default()).unwrap();
        eprintln!("Local LaMa elapsed: {:?}", start.elapsed());
        assert_eq!(repaired.dimensions(), image.dimensions());
        assert_eq!(repaired.get_pixel(0, 0), image.get_pixel(0, 0));
        let center = repaired.get_pixel(60, 48);
        assert!(
            center[0] < 160 && center[1] > 50,
            "object remained: {center:?}"
        );
    }
}
