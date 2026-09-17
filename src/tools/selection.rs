use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use image::{GrayImage, Rgba, RgbaImage};

use crate::error::{AppError, Result};
use crate::tools::crop::CropBounds;

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

/// Applies a BiRefNet alpha mask exactly as Sprite Studio does.
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

/// Runs the same local vision.cpp BiRefNet entrypoint used by Sprite Studio.
/// This deliberately has no network fallback: a missing runtime is surfaced to
/// the caller, which can leave the rectangular selection usable.
pub fn birefnet_mask(
    image: &RgbaImage,
    cancellation: &crate::document::CancellationToken,
) -> Result<GrayImage> {
    cancellation.check()?;
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
        AppError::BackgroundRemoval("HOME is unavailable; configure vision.cpp".into())
    })?;
    let runtime = BiRefNetRuntime {
        executable: std::env::var_os("SPRITE_STUDIO_VISION_CLI")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("vision.cpp/build/bin/vision-cli")),
        model: std::env::var_os("SPRITE_STUDIO_BIREFNET_MODEL")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("vision.cpp/models/BiRefNet-F16.gguf")),
        backend: std::env::var("SPRITE_STUDIO_BIREFNET_BACKEND").unwrap_or_else(|_| "gpu".into()),
        timeout: Duration::from_secs(600),
        poll_interval: Duration::from_millis(20),
        launch: launch_mode_from(Path::new("/.flatpak-info")),
        temporary_root: shared_cache_directory()?,
    };
    birefnet_mask_with_runtime(image, cancellation, &runtime)
}

#[derive(Debug, Clone)]
struct BiRefNetRuntime {
    executable: PathBuf,
    model: PathBuf,
    backend: String,
    timeout: Duration,
    poll_interval: Duration,
    launch: BiRefNetLaunch,
    temporary_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BiRefNetLaunch {
    Direct,
    FlatpakHost { launcher: PathBuf },
}

fn launch_mode_from(flatpak_info: &Path) -> BiRefNetLaunch {
    if flatpak_info.is_file() {
        BiRefNetLaunch::FlatpakHost {
            launcher: PathBuf::from("flatpak-spawn"),
        }
    } else {
        BiRefNetLaunch::Direct
    }
}

fn shared_cache_directory() -> Result<PathBuf> {
    let cache_home = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".cache"))
        })
        .ok_or_else(|| {
            AppError::BackgroundRemoval("HOME is unavailable; configure vision.cpp".into())
        })?;
    let directory = cache_home.join("diorama").join("background-removal");
    fs::create_dir_all(&directory)?;
    Ok(directory)
}

const LOG_TAIL_BYTES: u64 = 8 * 1024;

fn log_tail(path: &std::path::Path) -> String {
    let Ok(mut log) = File::open(path) else {
        return "<unavailable>".into();
    };
    let start = log
        .metadata()
        .map(|metadata| metadata.len().saturating_sub(LOG_TAIL_BYTES))
        .unwrap_or(0);
    if log.seek(SeekFrom::Start(start)).is_err() {
        return "<unavailable>".into();
    }
    let mut tail = String::new();
    if log.take(LOG_TAIL_BYTES).read_to_string(&mut tail).is_err() {
        return "<unavailable>".into();
    }
    if start > 0 {
        tail.insert_str(0, "…\n");
    }
    tail.trim().to_owned()
}

fn runtime_error(message: impl std::fmt::Display, log: &std::path::Path) -> AppError {
    AppError::BackgroundRemoval(format!("{message}. BiRefNet log tail: {}", log_tail(log)))
}

fn kill_and_reap(child: &mut Child) {
    // `kill` reports InvalidInput when `try_wait` raced with normal exit; the
    // following `wait` still reaps that child (or confirms it was already reaped).
    let _ = child.kill();
    let _ = child.wait();
}

fn birefnet_command(runtime: &BiRefNetRuntime, input: &Path, output: &Path) -> Command {
    let mut command = match &runtime.launch {
        BiRefNetLaunch::Direct => Command::new(&runtime.executable),
        BiRefNetLaunch::FlatpakHost { launcher } => {
            let mut command = Command::new(launcher);
            command
                .args([
                    "--host",
                    "--watch-bus",
                    "--unset-env=LD_LIBRARY_PATH",
                    "--unset-env=LD_PRELOAD",
                ])
                .arg(&runtime.executable);
            command
        }
    };
    command
        .args(["birefnet", "-m"])
        .arg(&runtime.model)
        .args(["-b", runtime.backend.as_str(), "-i"])
        .arg(input)
        .args(["-o"])
        .arg(output);
    command
}

fn birefnet_mask_with_runtime(
    image: &RgbaImage,
    cancellation: &crate::document::CancellationToken,
    runtime: &BiRefNetRuntime,
) -> Result<GrayImage> {
    cancellation.check()?;
    let executable = &runtime.executable;
    let model = &runtime.model;
    let backend = &runtime.backend;
    if !executable.is_file() || !model.is_file() || !matches!(backend.as_str(), "cpu" | "gpu") {
        return Err(AppError::BackgroundRemoval(
            "BiRefNet runtime is unavailable; set SPRITE_STUDIO_VISION_CLI and SPRITE_STUDIO_BIREFNET_MODEL."
                .into(),
        ));
    }
    fs::create_dir_all(&runtime.temporary_root)?;
    let directory = tempfile::Builder::new()
        .prefix("birefnet-")
        .tempdir_in(&runtime.temporary_root)?;
    let input = directory.path().join("selection.png");
    let output = directory.path().join("mask.png");
    let log = directory.path().join("birefnet.log");
    image.save(&input)?;
    let log_file = File::create(&log)?;
    let mut child = birefnet_command(runtime, &input, &output)
        .stdin(Stdio::null())
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .spawn()
        .map_err(|error| {
            AppError::BackgroundRemoval(format!("Could not launch vision.cpp: {error}"))
        })?;
    let started = Instant::now();
    loop {
        if let Err(error) = cancellation.check() {
            kill_and_reap(&mut child);
            return Err(error);
        }
        if started.elapsed() >= runtime.timeout {
            kill_and_reap(&mut child);
            return Err(runtime_error("BiRefNet timed out", &log));
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(status)) => {
                return Err(runtime_error(
                    format!("BiRefNet failed with status {status}"),
                    &log,
                ));
            }
            Ok(None) => thread::sleep(runtime.poll_interval),
            Err(error) => {
                kill_and_reap(&mut child);
                return Err(runtime_error(
                    format!("Could not wait for BiRefNet: {error}"),
                    &log,
                ));
            }
        }
    }
    let mask = image::open(output)?.into_luma8();
    if mask.dimensions() != image.dimensions() {
        return Err(AppError::InvalidDimensions);
    }
    cancellation.check()?;
    Ok(mask)
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

/// Composites a full prepared cutout at a signed origin, clipping only the
/// pixels outside the fixed-size canvas. The source and matte are never
/// cropped, so a foreground moved fully off-canvas can return unchanged.
pub fn paste_cutout_at(
    image: &mut RgbaImage,
    x: i64,
    y: i64,
    source_pixels: &RgbaImage,
    mask: &GrayImage,
) -> Result<()> {
    if mask.dimensions() != source_pixels.dimensions() {
        return Err(AppError::InvalidDimensions);
    }
    for (mask_x, mask_y, alpha) in mask.enumerate_pixels() {
        if alpha[0] == 0 {
            continue;
        }
        let Some(target_x) = x.checked_add(i64::from(mask_x)) else {
            continue;
        };
        let Some(target_y) = y.checked_add(i64::from(mask_y)) else {
            continue;
        };
        if target_x < 0
            || target_y < 0
            || target_x >= i64::from(image.width())
            || target_y >= i64::from(image.height())
        {
            continue;
        }
        let foreground = source_pixels.get_pixel(mask_x, mask_y).0;
        if foreground[3] == 0 {
            continue;
        }
        // Source-over keeps destination pixels visible through soft mask edges.
        let background = image.get_pixel(target_x as u32, target_y as u32).0;
        let a = u16::from(foreground[3]);
        let inverse = 255 - a;
        let out_a = a + u16::from(background[3]) * inverse / 255;
        let mut out = [0; 4];
        for channel in 0..3 {
            let premultiplied = u32::from(foreground[channel]) * u32::from(a)
                + u32::from(background[channel]) * u32::from(background[3]) * u32::from(inverse)
                    / 255;
            out[channel] = if out_a == 0 {
                0
            } else {
                (premultiplied / u32::from(out_a)).min(255) as u8
            };
        }
        out[3] = out_a as u8;
        image.put_pixel(target_x as u32, target_y as u32, Rgba(out));
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
    fn pasting_on_a_clean_base_keeps_holes_and_handles_overlap() {
        let mut image = RgbaImage::from_pixel(5, 1, Rgba([1, 2, 3, 255]));
        image.put_pixel(0, 0, Rgba([200, 0, 0, 255]));
        image.put_pixel(2, 0, Rgba([0, 200, 0, 255]));
        let bounds = CropBounds {
            x: 0,
            y: 0,
            width: 3,
            height: 1,
        };
        let mask = GrayImage::from_fn(3, 1, |x, _| Luma([[255, 0, 255][x as usize]]));
        let cutout = super::crop(&image, bounds).unwrap();
        super::clear_masked(&mut image, bounds, &mask, [9, 9, 9, 255]).unwrap();
        super::paste_cutout_at(&mut image, 1, 0, &cutout, &mask).unwrap();
        assert_eq!(image.get_pixel(0, 0).0, [9, 9, 9, 255]);
        assert_eq!(image.get_pixel(1, 0).0, [200, 0, 0, 255]);
        assert_eq!(
            image.get_pixel(2, 0).0,
            [9, 9, 9, 255],
            "destination hole keeps the cleared canvas"
        );
        assert_eq!(image.get_pixel(3, 0).0, [0, 200, 0, 255]);
    }

    #[test]
    fn signed_paste_composites_soft_cutout_alpha_source_over_destination() {
        let mut image = RgbaImage::from_pixel(2, 1, Rgba([0, 0, 200, 128]));
        image.put_pixel(0, 0, Rgba([200, 0, 0, 128]));
        let source = CropBounds {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
        let cutout = super::crop(&image, source).unwrap();
        let mask = GrayImage::from_pixel(1, 1, Luma([128]));

        super::paste_cutout_at(&mut image, 1, 0, &cutout, &mask).unwrap();

        assert_eq!(image.get_pixel(0, 0).0, [200, 0, 0, 128]);
        assert_eq!(image.get_pixel(1, 0).0, [134, 0, 66, 191]);
    }

    #[test]
    fn signed_paste_accepts_a_transparent_clean_base() {
        let mut image = RgbaImage::from_pixel(2, 1, Rgba([20, 30, 40, 255]));
        image.put_pixel(0, 0, Rgba([200, 0, 0, 255]));
        let source = CropBounds {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
        let cutout = super::crop(&image, source).unwrap();
        let mask = GrayImage::from_pixel(1, 1, Luma([255]));

        super::clear_masked(&mut image, source, &mask, [0; 4]).unwrap();
        super::paste_cutout_at(&mut image, 1, 0, &cutout, &mask).unwrap();

        assert_eq!(image.get_pixel(0, 0).0, [0; 4]);
        assert_eq!(image.get_pixel(1, 0).0, [200, 0, 0, 255]);
    }

    #[test]
    fn signed_paste_rejects_only_a_mismatched_matte() {
        let original = RgbaImage::from_fn(3, 1, |x, _| Rgba([x as u8, 2, 3, 255]));
        let source = CropBounds {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };
        let cutout = super::crop(&original, source).unwrap();
        let mut image = original.clone();
        assert!(matches!(
            super::paste_cutout_at(&mut image, -20, 0, &cutout, &GrayImage::new(1, 1)),
            Err(crate::error::AppError::InvalidDimensions)
        ));
        assert_eq!(image, original);
    }

    #[test]
    fn pasting_a_cutout_clips_at_every_canvas_edge_without_losing_the_source() {
        let cutout = RgbaImage::from_fn(3, 3, |x, y| Rgba([20 + x as u8, 40 + y as u8, 60, 255]));
        let mask = GrayImage::from_fn(3, 3, |x, y| Luma([if (x, y) == (1, 1) { 0 } else { 255 }]));
        let cases = [(-2, 0), (0, -2), (3, 0), (0, 3), (-4, -4)];

        for (x, y) in cases {
            let mut image = RgbaImage::from_pixel(4, 4, Rgba([1, 2, 3, 255]));
            super::paste_cutout_at(&mut image, x, y, &cutout, &mask).unwrap();
            for canvas_y in 0..image.height() {
                for canvas_x in 0..image.width() {
                    let cutout_x = i64::from(canvas_x) - x;
                    let cutout_y = i64::from(canvas_y) - y;
                    let expected = if (0..3).contains(&cutout_x)
                        && (0..3).contains(&cutout_y)
                        && mask.get_pixel(cutout_x as u32, cutout_y as u32)[0] != 0
                    {
                        *cutout.get_pixel(cutout_x as u32, cutout_y as u32)
                    } else {
                        Rgba([1, 2, 3, 255])
                    };
                    assert_eq!(
                        *image.get_pixel(canvas_x, canvas_y),
                        expected,
                        "origin {x},{y}"
                    );
                }
            }
        }
    }

    #[test]
    fn birefnet_log_tail_is_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("runtime.log");
        std::fs::write(&log, vec![b'x'; super::LOG_TAIL_BYTES as usize + 512]).unwrap();
        let tail = super::log_tail(&log);
        assert!(tail.starts_with('…'));
        assert!(tail.len() <= super::LOG_TAIL_BYTES as usize + "…\n".len());
    }

    #[cfg(unix)]
    fn fake_runtime(
        directory: &std::path::Path,
        name: &str,
        script: &str,
        timeout: std::time::Duration,
    ) -> super::BiRefNetRuntime {
        use std::os::unix::fs::PermissionsExt;
        let executable = directory.join(name);
        std::fs::write(&executable, script).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let model = directory.join("model.gguf");
        std::fs::write(&model, b"GGUF").unwrap();
        super::BiRefNetRuntime {
            executable,
            model,
            backend: "cpu".into(),
            timeout,
            poll_interval: std::time::Duration::from_millis(2),
            launch: super::BiRefNetLaunch::Direct,
            temporary_root: directory.join("cache with spaces"),
        }
    }

    #[test]
    fn launch_mode_detects_flatpak_explicitly() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("flatpak-info");
        assert_eq!(
            super::launch_mode_from(&marker),
            super::BiRefNetLaunch::Direct
        );
        std::fs::write(&marker, "[Instance]\n").unwrap();
        assert_eq!(
            super::launch_mode_from(&marker),
            super::BiRefNetLaunch::FlatpakHost {
                launcher: PathBuf::from("flatpak-spawn"),
            }
        );
    }

    #[test]
    fn host_launcher_preserves_birefnet_arguments_and_spaced_paths() {
        let runtime = super::BiRefNetRuntime {
            executable: PathBuf::from("/host/bin/vision cli"),
            model: PathBuf::from("/host/models/Bi RefNet.gguf"),
            backend: "gpu".into(),
            timeout: std::time::Duration::from_secs(1),
            poll_interval: std::time::Duration::from_millis(1),
            launch: super::BiRefNetLaunch::FlatpakHost {
                launcher: PathBuf::from("/sandbox/bin/flatpak spawn"),
            },
            temporary_root: PathBuf::from("/host/cache/diorama"),
        };
        let command = super::birefnet_command(
            &runtime,
            Path::new("/host/cache/input image.png"),
            Path::new("/host/cache/output mask.png"),
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
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let mut runtime = fake_runtime(
            root.path(),
            "vision-cli",
            "#!/bin/sh\nexit 99\n",
            std::time::Duration::from_secs(5),
        );
        let launcher = root.path().join("flatpak-spawn");
        std::fs::write(&launcher, "#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        runtime.launch = super::BiRefNetLaunch::FlatpakHost { launcher };
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
