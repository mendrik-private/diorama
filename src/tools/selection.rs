use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use image::{GrayImage, Rgba, RgbaImage};

use crate::error::{AppError, Result};
use crate::tools::{
    crop::CropBounds,
    worker_process::{self, Launch, WaitError},
};

pub fn bounds_between(
    start: (u32, u32),
    end: (u32, u32),
    image_width: u32,
    image_height: u32,
) -> Result<CropBounds> {
    if image_width == 0 || image_height == 0 {
        return Err(AppError::InvalidDimensions);
    }
    let start = (start.0.min(image_width - 1), start.1.min(image_height - 1));
    let end = (end.0.min(image_width - 1), end.1.min(image_height - 1));
    let x = start.0.min(end.0);
    let y = start.1.min(end.1);
    Ok(CropBounds {
        x,
        y,
        width: start.0.max(end.0) - x + 1,
        height: start.1.max(end.1) - y + 1,
    })
}

pub fn crop(image: &RgbaImage, bounds: CropBounds) -> Result<RgbaImage> {
    let right = bounds
        .x
        .checked_add(bounds.width)
        .ok_or(AppError::InvalidDimensions)?;
    let bottom = bounds
        .y
        .checked_add(bounds.height)
        .ok_or(AppError::InvalidDimensions)?;
    if bounds.width == 0 || bounds.height == 0 || right > image.width() || bottom > image.height() {
        return Err(AppError::InvalidDimensions);
    }
    Ok(
        image::imageops::crop_imm(image, bounds.x, bounds.y, bounds.width, bounds.height)
            .to_image(),
    )
}

/// Returns a border colour only when the border is sufficiently consistent to
/// be a useful replacement. A mixed border is deliberately a transparent hole.
pub fn detected_background(image: &RgbaImage) -> Option<[u8; 4]> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    let estimate = crate::tools::canvas_resize::background(image);
    let mut total = 0usize;
    let mut matching = 0usize;
    let mut sample = |x, y| {
        total += 1;
        let pixel = image.get_pixel(x, y).0;
        // Compare premultiplied colour so transparent hidden RGB is irrelevant.
        let distance = (0..4)
            .map(|i| {
                let actual = if i < 3 {
                    u16::from(pixel[i]) * u16::from(pixel[3]) / 255
                } else {
                    u16::from(pixel[i])
                };
                let expected = if i < 3 {
                    u16::from(estimate[i]) * u16::from(estimate[3]) / 255
                } else {
                    u16::from(estimate[i])
                };
                actual.abs_diff(expected)
            })
            .max()
            .unwrap_or(255);
        if distance <= 12 {
            matching += 1;
        }
    };
    for x in 0..width {
        sample(x, 0);
        if height > 1 {
            sample(x, height - 1);
        }
    }
    for y in 1..height.saturating_sub(1) {
        sample(0, y);
        if width > 1 {
            sample(width - 1, y);
        }
    }
    (total > 0 && matching * 100 >= total * 80).then_some(estimate)
}

/// Applies a BiRefNet alpha mask, preserving the source's existing alpha.
#[cfg(test)]
pub fn apply_alpha_mask(image: &mut RgbaImage, mask: &GrayImage) -> Result<()> {
    if image.dimensions() != mask.dimensions() {
        return Err(AppError::InvalidDimensions);
    }
    for (pixel, alpha) in image.pixels_mut().zip(mask.pixels()) {
        pixel[3] = ((u16::from(pixel[3]) * u16::from(alpha[0]) + 127) / 255) as u8;
        if pixel[3] == 0 {
            pixel.0 = [0; 4];
        }
    }
    Ok(())
}

/// Returns BiRefNet's foreground estimate with the original source alpha
/// applied.  The foreground estimate removes the background colour embedded
/// in soft source pixels, which prevents pale source backgrounds from showing
/// as a fringe when the cutout is composited on a dark canvas.
pub fn birefnet_cutout(
    image: &RgbaImage,
    cancellation: &crate::document::CancellationToken,
) -> Result<RgbaImage> {
    cancellation.check()?;
    let runtime = birefnet_runtime()?;
    birefnet_cutout_with_runtime(image, cancellation, &runtime)
}

fn birefnet_runtime() -> Result<BiRefNetRuntime> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
        AppError::BackgroundRemoval("HOME is unavailable; configure vision.cpp".into())
    })?;
    Ok(BiRefNetRuntime {
        executable: std::env::var_os("ASSET_SCALER_VISION_CLI")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("vision.cpp/build/bin/vision-cli")),
        model: std::env::var_os("ASSET_SCALER_BIREFNET_MODEL")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("vision.cpp/models/BiRefNet-F16.gguf")),
        backend: std::env::var("ASSET_SCALER_BIREFNET_BACKEND").unwrap_or_else(|_| "gpu".into()),
        timeout: Duration::from_secs(600),
        poll_interval: Duration::from_millis(20),
        launch: Launch::detect(),
        temporary_root: shared_cache_directory()?,
    })
}

#[derive(Debug, Clone)]
struct BiRefNetRuntime {
    executable: PathBuf,
    model: PathBuf,
    backend: String,
    timeout: Duration,
    poll_interval: Duration,
    launch: Launch,
    temporary_root: PathBuf,
}

fn shared_cache_directory() -> Result<PathBuf> {
    let cache_home = worker_process::cache_home().ok_or_else(|| {
        AppError::BackgroundRemoval("HOME is unavailable; configure vision.cpp".into())
    })?;
    let directory = cache_home.join("diorama").join("background-removal");
    fs::create_dir_all(&directory)?;
    Ok(directory)
}

fn runtime_error(message: impl std::fmt::Display, log: &Path) -> AppError {
    AppError::BackgroundRemoval(format!(
        "{message}. BiRefNet log tail: {}",
        worker_process::log_tail(log)
    ))
}

fn birefnet_command(
    runtime: &BiRefNetRuntime,
    input: &Path,
    output: &Path,
    foreground: Option<&Path>,
) -> Command {
    let mut command = runtime.launch.command(&runtime.executable, None);
    command
        .args(["birefnet", "-m"])
        .arg(&runtime.model)
        .args(["-b", runtime.backend.as_str(), "-i"])
        .arg(input)
        .args(["-o"])
        .arg(output);
    if let Some(foreground) = foreground {
        command.args(["--composite"]).arg(foreground);
    }
    command
}

struct BiRefNetOutput {
    mask: GrayImage,
    foreground: Option<RgbaImage>,
}

/// BiRefNet's foreground mask of `image`, at its size.
pub fn birefnet_mask(
    image: &RgbaImage,
    cancellation: &crate::document::CancellationToken,
) -> Result<GrayImage> {
    cancellation.check()?;
    birefnet_mask_with_runtime(image, cancellation, &birefnet_runtime()?)
}

fn birefnet_mask_with_runtime(
    image: &RgbaImage,
    cancellation: &crate::document::CancellationToken,
    runtime: &BiRefNetRuntime,
) -> Result<GrayImage> {
    Ok(birefnet_with_runtime(image, cancellation, runtime, false)?.mask)
}

fn birefnet_cutout_with_runtime(
    image: &RgbaImage,
    cancellation: &crate::document::CancellationToken,
    runtime: &BiRefNetRuntime,
) -> Result<RgbaImage> {
    let output = birefnet_with_runtime(image, cancellation, runtime, true)?;
    let mut cutout = output
        .foreground
        .expect("foreground output is requested when preparing a BiRefNet cutout");
    if cutout.dimensions() != output.mask.dimensions() {
        return Err(AppError::InvalidDimensions);
    }
    apply_source_alpha(&mut cutout, image)?;
    Ok(cutout)
}

fn apply_source_alpha(cutout: &mut RgbaImage, source: &RgbaImage) -> Result<()> {
    if cutout.dimensions() != source.dimensions() {
        return Err(AppError::InvalidDimensions);
    }
    for (pixel, source_pixel) in cutout.pixels_mut().zip(source.pixels()) {
        pixel[3] = ((u16::from(pixel[3]) * u16::from(source_pixel[3]) + 127) / 255) as u8;
        if pixel[3] == 0 {
            pixel.0 = [0; 4];
        }
    }
    Ok(())
}

fn birefnet_with_runtime(
    image: &RgbaImage,
    cancellation: &crate::document::CancellationToken,
    runtime: &BiRefNetRuntime,
    estimate_foreground: bool,
) -> Result<BiRefNetOutput> {
    cancellation.check()?;
    let executable = &runtime.executable;
    let model = &runtime.model;
    let backend = &runtime.backend;
    if !executable.is_file() || !model.is_file() || !matches!(backend.as_str(), "cpu" | "gpu") {
        return Err(AppError::BackgroundRemoval(
            "BiRefNet runtime is unavailable; set ASSET_SCALER_VISION_CLI and ASSET_SCALER_BIREFNET_MODEL."
                .into(),
        ));
    }
    fs::create_dir_all(&runtime.temporary_root)?;
    let directory = tempfile::Builder::new()
        .prefix("birefnet-")
        .tempdir_in(&runtime.temporary_root)?;
    let input = directory.path().join("selection.png");
    let output = directory.path().join("mask.png");
    let foreground = directory.path().join("foreground.png");
    let log = directory.path().join("birefnet.log");
    image.save(&input)?;
    let mut child = worker_process::spawn_logged(
        &mut birefnet_command(
            runtime,
            &input,
            &output,
            estimate_foreground.then_some(foreground.as_path()),
        ),
        &log,
    )
    .map_err(|error| {
        AppError::BackgroundRemoval(format!("Could not launch vision.cpp: {error}"))
    })?;
    match worker_process::wait(
        &mut child,
        cancellation,
        runtime.timeout,
        runtime.poll_interval,
    ) {
        Ok(status) if status.success() => {}
        Ok(status) => {
            return Err(runtime_error(
                format!("BiRefNet failed with status {status}"),
                &log,
            ));
        }
        Err(WaitError::Cancelled(error)) => return Err(error),
        Err(WaitError::TimedOut) => return Err(runtime_error("BiRefNet timed out", &log)),
        Err(WaitError::Io(error)) => {
            return Err(runtime_error(
                format!("Could not wait for BiRefNet: {error}"),
                &log,
            ));
        }
    }
    let mask = image::open(output)?.into_luma8();
    if mask.dimensions() != image.dimensions() {
        return Err(AppError::InvalidDimensions);
    }
    let foreground = if estimate_foreground {
        let foreground = image::open(foreground)?.into_rgba8();
        if foreground.dimensions() != image.dimensions() {
            return Err(AppError::InvalidDimensions);
        }
        Some(foreground)
    } else {
        None
    };
    cancellation.check()?;
    Ok(BiRefNetOutput { mask, foreground })
}

pub fn clear_masked(
    image: &mut RgbaImage,
    bounds: CropBounds,
    mask: &GrayImage,
    fill: [u8; 4],
) -> Result<()> {
    if mask.dimensions() != (bounds.width, bounds.height)
        || bounds
            .x
            .checked_add(bounds.width)
            .is_none_or(|right| right > image.width())
        || bounds
            .y
            .checked_add(bounds.height)
            .is_none_or(|bottom| bottom > image.height())
    {
        return Err(AppError::InvalidDimensions);
    }
    for (x, y, alpha) in mask.enumerate_pixels() {
        if alpha[0] != 0 {
            image.put_pixel(bounds.x + x, bounds.y + y, Rgba(fill));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use image::{GrayImage, Luma, Rgba, RgbaImage};

    use super::{bounds_between, crop};
    use crate::tools::crop::CropBounds;

    #[test]
    fn reverse_drag_produces_inclusive_clamped_bounds() {
        let bounds = bounds_between((8, 7), (2, 3), 6, 5).unwrap();
        assert_eq!(
            (bounds.x, bounds.y, bounds.width, bounds.height),
            (2, 3, 4, 2)
        );
    }

    #[test]
    fn crop_preserves_exact_selected_pixels() {
        let image = RgbaImage::from_fn(4, 3, |x, y| Rgba([x as u8, y as u8, 7, 255]));
        let bounds = bounds_between((1, 1), (2, 2), image.width(), image.height()).unwrap();
        let selected = crop(&image, bounds).unwrap();

        assert_eq!(selected.dimensions(), (2, 2));
        assert_eq!(selected.get_pixel(0, 0).0, [1, 1, 7, 255]);
        assert_eq!(selected.get_pixel(1, 1).0, [2, 2, 7, 255]);
    }

    #[test]
    fn mask_preserves_soft_alpha_and_holes() {
        let mut image = RgbaImage::from_pixel(3, 1, Rgba([10, 20, 30, 200]));
        let mask = GrayImage::from_fn(3, 1, |x, _| Luma([[0, 128, 255][x as usize]]));
        super::apply_alpha_mask(&mut image, &mask).unwrap();
        assert_eq!(image.get_pixel(0, 0).0, [0; 4]);
        assert_eq!(image.get_pixel(1, 0).0, [10, 20, 30, 100]);
        assert_eq!(image.get_pixel(2, 0).0, [10, 20, 30, 200]);
    }

    #[test]
    fn detected_background_accepts_uniform_opaque_and_transparent_borders() {
        let opaque = RgbaImage::from_pixel(4, 3, Rgba([20, 40, 60, 255]));
        assert_eq!(super::detected_background(&opaque), Some([20, 40, 60, 255]));

        let transparent =
            RgbaImage::from_fn(4, 3, |x, y| Rgba([x as u8 * 40, y as u8 * 70, 200, 0]));
        assert_eq!(super::detected_background(&transparent), Some([0; 4]));
    }

    #[test]
    fn detected_background_rejects_a_varied_border() {
        let varied = RgbaImage::from_fn(5, 5, |x, y| {
            if (x + y) % 2 == 0 {
                Rgba([200, 10, 10, 255])
            } else {
                Rgba([10, 10, 200, 255])
            }
        });
        assert_eq!(super::detected_background(&varied), None);
    }

    #[test]
    fn clearing_a_mask_keeps_holes_and_soft_edges_do_not_leave_source_pixels() {
        let mut image = RgbaImage::from_pixel(5, 1, Rgba([1, 2, 3, 255]));
        let mask = GrayImage::from_raw(3, 1, vec![255, 0, 128]).unwrap();
        super::clear_masked(
            &mut image,
            CropBounds {
                x: 1,
                y: 0,
                width: 3,
                height: 1,
            },
            &mask,
            [9, 9, 9, 255],
        )
        .unwrap();
        assert_eq!(image.get_pixel(0, 0).0, [1, 2, 3, 255]);
        assert_eq!(image.get_pixel(1, 0).0, [9, 9, 9, 255]);
        assert_eq!(image.get_pixel(2, 0).0, [1, 2, 3, 255]);
        assert_eq!(image.get_pixel(3, 0).0, [9, 9, 9, 255]);
    }

    #[cfg(unix)]
    fn fake_runtime(
        directory: &std::path::Path,
        name: &str,
        script: &str,
        timeout: std::time::Duration,
    ) -> super::BiRefNetRuntime {
        let executable = directory.join(name);
        crate::tools::worker_process::write_test_executable(&executable, script);
        let model = directory.join("model.gguf");
        std::fs::write(&model, b"GGUF").unwrap();
        super::BiRefNetRuntime {
            executable,
            model,
            backend: "cpu".into(),
            timeout,
            poll_interval: std::time::Duration::from_millis(2),
            launch: crate::tools::worker_process::Launch::Direct,
            temporary_root: directory.join("cache with spaces"),
        }
    }

    #[test]
    fn host_launcher_preserves_birefnet_arguments_and_spaced_paths() {
        let runtime = super::BiRefNetRuntime {
            executable: PathBuf::from("/host/bin/vision cli"),
            model: PathBuf::from("/host/models/Bi RefNet.gguf"),
            backend: "gpu".into(),
            timeout: std::time::Duration::from_secs(1),
            poll_interval: std::time::Duration::from_millis(1),
            launch: crate::tools::worker_process::Launch::FlatpakHost {
                launcher: PathBuf::from("/sandbox/bin/flatpak spawn"),
            },
            temporary_root: PathBuf::from("/host/cache/diorama"),
        };
        let command = super::birefnet_command(
            &runtime,
            Path::new("/host/cache/input image.png"),
            Path::new("/host/cache/output mask.png"),
            None,
        );
        assert_eq!(
            command.get_program(),
            Path::new("/sandbox/bin/flatpak spawn")
        );
        assert_eq!(
            command
                .get_args()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            [
                "--host",
                "--watch-bus",
                "--unset-env=LD_LIBRARY_PATH",
                "--unset-env=LD_PRELOAD",
                "/host/bin/vision cli",
                "birefnet",
                "-m",
                "/host/models/Bi RefNet.gguf",
                "-b",
                "gpu",
                "-i",
                "/host/cache/input image.png",
                "-o",
                "/host/cache/output mask.png",
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn birefnet_runtime_returns_a_same_size_mask() {
        let root = tempfile::tempdir().unwrap();
        let runtime = fake_runtime(
            root.path(),
            "success-cli",
            "#!/bin/sh\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    -i) input=\"$2\"; shift 2 ;;\n    -o) output=\"$2\"; shift 2 ;;\n    *) shift ;;\n  esac\ndone\ncp \"$input\" \"$output\"\n",
            std::time::Duration::from_secs(1),
        );
        let source = RgbaImage::from_pixel(2, 2, Rgba([1, 2, 3, 255]));
        let mask = super::birefnet_mask_with_runtime(
            &source,
            &crate::document::CancellationToken::default(),
            &runtime,
        )
        .unwrap();
        assert_eq!(mask.dimensions(), source.dimensions());
        assert!(
            std::fs::read_dir(&runtime.temporary_root)
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn birefnet_cutout_uses_foreground_estimate_and_preserves_source_alpha() {
        let root = tempfile::tempdir().unwrap();
        let mask = GrayImage::from_fn(3, 1, |x, _| Luma([[255, 128, 255][x as usize]]));
        mask.save(root.path().join("mask.png")).unwrap();
        RgbaImage::from_fn(3, 1, |x, _| {
            Rgba([[20, 30, 40, 255], [50, 60, 70, 128], [80, 90, 100, 255]][x as usize])
        })
        .save(root.path().join("foreground.png"))
        .unwrap();
        let runtime = fake_runtime(
            root.path(),
            "foreground-cli",
            "#!/bin/sh\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    -o) output=\"$2\"; shift 2 ;;\n    --composite) foreground=\"$2\"; shift 2 ;;\n    *) shift ;;\n  esac\ndone\ncp \"$(dirname \"$0\")/mask.png\" \"$output\"\ncp \"$(dirname \"$0\")/foreground.png\" \"$foreground\"\n",
            std::time::Duration::from_secs(1),
        );
        let source = RgbaImage::from_fn(3, 1, |x, _| {
            Rgba(
                [
                    [220, 220, 220, 255],
                    [220, 220, 220, 128],
                    [220, 220, 220, 0],
                ][x as usize],
            )
        });

        let cutout = super::birefnet_cutout_with_runtime(
            &source,
            &crate::document::CancellationToken::default(),
            &runtime,
        )
        .unwrap();

        assert_eq!(cutout.get_pixel(0, 0).0, [20, 30, 40, 255]);
        assert_eq!(cutout.get_pixel(1, 0).0, [50, 60, 70, 64]);
        assert_eq!(cutout.get_pixel(2, 0).0, [0; 4]);
        assert!(
            std::fs::read_dir(&runtime.temporary_root)
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn birefnet_cutout_rejects_a_mismatched_foreground_estimate() {
        let root = tempfile::tempdir().unwrap();
        GrayImage::from_pixel(2, 2, Luma([255]))
            .save(root.path().join("mask.png"))
            .unwrap();
        RgbaImage::from_pixel(1, 1, Rgba([20, 30, 40, 255]))
            .save(root.path().join("foreground.png"))
            .unwrap();
        let runtime = fake_runtime(
            root.path(),
            "mismatched-foreground-cli",
            "#!/bin/sh\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    -o) output=\"$2\"; shift 2 ;;\n    --composite) foreground=\"$2\"; shift 2 ;;\n    *) shift ;;\n  esac\ndone\ncp \"$(dirname \"$0\")/mask.png\" \"$output\"\ncp \"$(dirname \"$0\")/foreground.png\" \"$foreground\"\n",
            std::time::Duration::from_secs(1),
        );
        let source = RgbaImage::from_pixel(2, 2, Rgba([220, 220, 220, 255]));

        assert!(matches!(
            super::birefnet_cutout_with_runtime(
                &source,
                &crate::document::CancellationToken::default(),
                &runtime,
            ),
            Err(crate::error::AppError::InvalidDimensions)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn birefnet_runtime_checks_cancellation_before_spawning() {
        let root = tempfile::tempdir().unwrap();
        let runtime = fake_runtime(
            root.path(),
            "never-started-cli",
            "#!/bin/sh\nexit 99\n",
            std::time::Duration::from_secs(1),
        );
        let token = crate::document::CancellationToken::default();
        token.cancel();
        let source = RgbaImage::from_pixel(1, 1, Rgba([1, 2, 3, 255]));
        assert!(matches!(
            super::birefnet_mask_with_runtime(&source, &token, &runtime),
            Err(crate::error::AppError::Cancelled)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn birefnet_runtime_rejects_a_mismatched_mask() {
        let root = tempfile::tempdir().unwrap();
        RgbaImage::from_pixel(1, 1, Rgba([1, 2, 3, 255]))
            .save(root.path().join("mismatch.png"))
            .unwrap();
        let runtime = fake_runtime(
            root.path(),
            "mismatch-cli",
            "#!/bin/sh\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    -o) output=\"$2\"; shift 2 ;;\n    *) shift ;;\n  esac\ndone\ncp \"$(dirname \"$0\")/mismatch.png\" \"$output\"\n",
            std::time::Duration::from_secs(1),
        );
        let source = RgbaImage::from_pixel(2, 2, Rgba([1, 2, 3, 255]));
        assert!(matches!(
            super::birefnet_mask_with_runtime(
                &source,
                &crate::document::CancellationToken::default(),
                &runtime,
            ),
            Err(crate::error::AppError::InvalidDimensions)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn birefnet_runtime_reports_failed_exit_with_a_bounded_log_tail() {
        let root = tempfile::tempdir().unwrap();
        let runtime = fake_runtime(
            root.path(),
            "failing-cli",
            "#!/bin/sh\necho first >&2\necho final-runtime-line >&2\nexit 7\n",
            std::time::Duration::from_secs(1),
        );
        let source = RgbaImage::from_pixel(2, 2, Rgba([1, 2, 3, 255]));
        let error = super::birefnet_mask_with_runtime(
            &source,
            &crate::document::CancellationToken::default(),
            &runtime,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("status"));
        assert!(error.contains("final-runtime-line"));
    }

    #[cfg(unix)]
    #[test]
    fn birefnet_runtime_cancellation_kills_and_reaps_the_running_process() {
        let root = tempfile::tempdir().unwrap();
        let runtime = fake_runtime(
            root.path(),
            "sleeping-cli",
            "#!/bin/sh\nexec sleep 60\n",
            std::time::Duration::from_secs(5),
        );
        let source = RgbaImage::from_pixel(2, 2, Rgba([1, 2, 3, 255]));
        let token = crate::document::CancellationToken::default();
        let worker_token = token.clone();
        let worker = std::thread::spawn(move || {
            super::birefnet_mask_with_runtime(&source, &worker_token, &runtime)
        });
        std::thread::sleep(std::time::Duration::from_millis(60));
        token.cancel();
        assert!(matches!(
            worker.join().unwrap(),
            Err(crate::error::AppError::Cancelled)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn host_launch_cancellation_kills_and_reaps_the_launcher_process() {
        let root = tempfile::tempdir().unwrap();
        let mut runtime = fake_runtime(
            root.path(),
            "vision-cli",
            "#!/bin/sh\nexit 99\n",
            std::time::Duration::from_secs(5),
        );
        let launcher = root.path().join("flatpak-spawn");
        crate::tools::worker_process::write_test_executable(
            &launcher,
            "#!/bin/sh\nexec sleep 60\n",
        );
        runtime.launch = crate::tools::worker_process::Launch::FlatpakHost { launcher };
        let source = RgbaImage::from_pixel(2, 2, Rgba([1, 2, 3, 255]));
        let token = crate::document::CancellationToken::default();
        let worker_token = token.clone();
        let worker = std::thread::spawn(move || {
            super::birefnet_mask_with_runtime(&source, &worker_token, &runtime)
        });
        std::thread::sleep(std::time::Duration::from_millis(60));
        token.cancel();
        assert!(matches!(
            worker.join().unwrap(),
            Err(crate::error::AppError::Cancelled)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn birefnet_runtime_timeout_kills_and_reaps_the_running_process() {
        let root = tempfile::tempdir().unwrap();
        let runtime = fake_runtime(
            root.path(),
            "timeout-cli",
            "#!/bin/sh\nexec sleep 60\n",
            std::time::Duration::from_millis(30),
        );
        let source = RgbaImage::from_pixel(2, 2, Rgba([1, 2, 3, 255]));
        let started = std::time::Instant::now();
        let error = super::birefnet_mask_with_runtime(
            &source,
            &crate::document::CancellationToken::default(),
            &runtime,
        )
        .unwrap_err();
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(error.to_string().contains("timed out"));
    }
}
