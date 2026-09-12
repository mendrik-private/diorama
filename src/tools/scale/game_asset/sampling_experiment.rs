//! Opt-in visual experiments; none of these selectors changes the app renderer.

use image::{Rgba, RgbaImage};

mod cleanup;

fn phase(source: &RgbaImage, size: u32, dx: f32, dy: f32) -> RgbaImage {
    RgbaImage::from_fn(size, size, |x, y| {
        let sx = ((x as f32 + dx) * source.width() as f32 / size as f32) as u32;
        let sy = ((y as f32 + dy) * source.height() as f32 / size as f32) as u32;
        *source.get_pixel(sx.min(source.width() - 1), sy.min(source.height() - 1))
    })
}

fn ridge_samples(source: &RgbaImage, size: u32, threshold: f32, amount: f32) -> RgbaImage {
    assert_eq!(source.width(), source.height(), "square-only experiment");
    assert!(size > 0 && size <= source.width());
    assert!((0.0..=1.0).contains(&amount));
    let scale = source.width() as f32 / size as f32;
    let radius = (scale * 0.5).round().max(1.0) as i32;
    let luma = |x: i32, y: i32| -> Option<f32> {
        if x < 0 || y < 0 || x >= source.width() as i32 || y >= source.height() as i32 {
            return None;
        }
        let p = source.get_pixel(x as u32, y as u32);
        (p[3] >= 250).then_some(0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32)
    };
    let ridge = |x: i32, y: i32, nx: i32, ny: i32| -> f32 {
        let r = if nx != 0 && ny != 0 {
            (radius as f32 * std::f32::consts::FRAC_1_SQRT_2).round() as i32
        } else {
            radius
        };
        match (
            luma(x, y),
            luma(x + nx * r, y + ny * r),
            luma(x - nx * r, y - ny * r),
        ) {
            (Some(c), Some(a), Some(b)) => (a.min(b) - c).max(0.0),
            _ => 0.0,
        }
    };
    let strength = |x: i32, y: i32| -> f32 {
        [(1, 0), (0, 1), (1, 1), (1, -1)]
            .into_iter()
            .map(|(nx, ny)| {
                let centre = ridge(x, y, nx, ny);
                [-1, 1]
                    .into_iter()
                    .map(|side| {
                        (-1..=1)
                            .map(|shift| {
                                ridge(
                                    x - ny * radius * side + nx * shift,
                                    y + nx * radius * side + ny * shift,
                                    nx,
                                    ny,
                                )
                            })
                            .fold(0.0_f32, f32::max)
                    })
                    .fold(centre, f32::min)
            })
            .fold(0.0_f32, f32::max)
    };
    let mut output = phase(source, size, 0.5, 0.5);
    for y in 0..size {
        for x in 0..size {
            if output.get_pixel(x, y)[3] < 250 {
                continue;
            }
            let cx = (x as f32 + 0.5) * scale;
            let cy = (y as f32 + 0.5) * scale;
            let mut best = (cx as i32, cy as i32);
            let mut best_score = strength(best.0, best.1) + threshold;
            for sy in (y as f32 * scale).ceil() as i32..((y + 1) as f32 * scale).floor() as i32 {
                for sx in (x as f32 * scale).ceil() as i32..((x + 1) as f32 * scale).floor() as i32
                {
                    let distance = ((sx as f32 + 0.5 - cx).powi(2)
                        + (sy as f32 + 0.5 - cy).powi(2))
                        / scale.powi(2);
                    let score = strength(sx, sy) - 80.0 * distance;
                    if score > best_score {
                        best_score = score;
                        best = (sx, sy);
                    }
                }
            }
            let baseline = *output.get_pixel(x, y);
            let ridge_pixel = *source.get_pixel(best.0 as u32, best.1 as u32);
            // The desired contrast is only a selection guide. Output remains
            // an exact source RGBA sample, never this blended guide colour.
            let target: [f32; 3] = std::array::from_fn(|c| {
                baseline[c] as f32 * (1.0 - amount) + ridge_pixel[c] as f32 * amount
            });
            let colour_error = |p: &Rgba<u8>| {
                (0..3)
                    .map(|c| (p[c] as f32 - target[c]).powi(2))
                    .sum::<f32>()
            };
            let mut pixel = baseline;
            let mut error = colour_error(&pixel);
            if ridge_pixel != baseline {
                for sy in (y as f32 * scale).ceil() as u32..((y + 1) as f32 * scale).floor() as u32
                {
                    for sx in
                        (x as f32 * scale).ceil() as u32..((x + 1) as f32 * scale).floor() as u32
                    {
                        let candidate = source.get_pixel(sx, sy);
                        let candidate_error = colour_error(candidate);
                        if candidate[3] == baseline[3] && candidate_error < error {
                            error = candidate_error;
                            pixel = *candidate;
                        }
                    }
                }
            }
            output.put_pixel(x, y, pixel);
        }
    }
    output
}

#[test]
fn local_ridge_experiment_preserves_samples_and_rejects_isolated_dots() {
    let mut source = RgbaImage::from_fn(48, 48, |x, _| {
        if x < 22 {
            Rgba([60, 120, 50, 255])
        } else {
            Rgba([230, 200, 60, 255])
        }
    });
    for y in 4..44 {
        source.put_pixel(21, y, Rgba([20, 20, 10, 255]));
    }
    source.put_pixel(37, 20, Rgba([0, 0, 0, 255]));
    source.put_pixel(10, 10, Rgba([255, 0, 180, 96]));
    let nearest = phase(&source, 12, 0.5, 0.5);
    let result = ridge_samples(&source, 12, 30.0, 0.5);
    for y in 2..10 {
        assert_eq!(
            nearest.get_pixel(5, y)[0],
            230,
            "NN must miss the separating line"
        );
        assert!(
            result.get_pixel(5, y)[0] < 200,
            "retain contrast at the missed boundary"
        );
    }
    assert_eq!(
        result.get_pixel(9, 5),
        nearest.get_pixel(9, 5),
        "do not promote an isolated dot"
    );
    for (x, y, p) in result.enumerate_pixels() {
        assert_eq!(p[3], nearest.get_pixel(x, y)[3]);
        assert!(
            (y * 4..y * 4 + 4).any(|sy| (x * 4..x * 4 + 4).any(|sx| source.get_pixel(sx, sy) == p))
        );
    }
    let gradient = RgbaImage::from_fn(48, 48, |x, _| {
        let value = (x * 5) as u8;
        Rgba([value, value, value, 255])
    });
    assert_eq!(
        ridge_samples(&gradient, 12, 30.0, 0.5),
        phase(&gradient, 12, 0.5, 0.5)
    );
}

#[test]
#[ignore = "requires DIORAMA_GAME_ASSET_INPUT=.../elf2-se.png; visual ridge experiment"]
fn elf_local_ridges() {
    let source = image::open(std::env::var("DIORAMA_GAME_ASSET_INPUT").unwrap())
        .unwrap()
        .to_rgba8();
    assert_eq!(source.dimensions(), (800, 800));
    let directory = tempfile::Builder::new()
        .prefix("diorama-ridges-")
        .tempdir()
        .unwrap()
        .keep();
    for size in [128, 160] {
        let mut sheet = RgbaImage::from_pixel(size * 4, size, Rgba([82, 82, 82, 255]));
        let mut heads = RgbaImage::from_pixel(112, 30, Rgba([82, 82, 82, 255]));
        let baseline = phase(&source, size, 0.5, 0.5);
        for (i, amount) in [0.0, 0.35, 0.5, 0.7].into_iter().enumerate() {
            let start = std::time::Instant::now();
            let output = if i == 0 {
                baseline.clone()
            } else {
                ridge_samples(&source, size, 30.0, amount)
            };
            assert!(
                baseline
                    .pixels()
                    .zip(output.pixels())
                    .all(|(a, b)| a[3] == b[3])
            );
            eprintln!(
                "{size} amount {amount}: {:?}, {} changed pixels",
                start.elapsed(),
                baseline
                    .pixels()
                    .zip(output.pixels())
                    .filter(|(a, b)| a != b)
                    .count()
            );
            output
                .save(directory.join(format!("{size}-ridge-{i}.png")))
                .unwrap();
            if size == 128 && i == 2 {
                let mut pair = RgbaImage::from_pixel(256, 128, Rgba([82, 82, 82, 255]));
                image::imageops::overlay(&mut pair, &baseline, 0, 0);
                image::imageops::overlay(&mut pair, &output, 128, 0);
                image::imageops::resize(&pair, 768, 384, image::imageops::FilterType::Nearest)
                    .save(directory.join("128-before-after-3x.png"))
                    .unwrap();
                let mut head_pair = RgbaImage::from_pixel(56, 30, Rgba([82, 82, 82, 255]));
                for (i, panel) in [&baseline, &output].into_iter().enumerate() {
                    let head = image::imageops::crop_imm(panel, 50, 16, 28, 30).to_image();
                    image::imageops::overlay(&mut head_pair, &head, i as i64 * 28, 0);
                }
                image::imageops::resize(&head_pair, 560, 300, image::imageops::FilterType::Nearest)
                    .save(directory.join("128-head-before-after-10x.png"))
                    .unwrap();
            }
            image::imageops::overlay(&mut sheet, &output, i as i64 * size as i64, 0);
            if size == 128 {
                let head = image::imageops::crop_imm(&output, 50, 16, 28, 30).to_image();
                image::imageops::overlay(&mut heads, &head, i as i64 * 28, 0);
            }
        }
        image::imageops::resize(
            &sheet,
            sheet.width() * 3,
            size * 3,
            image::imageops::FilterType::Nearest,
        )
        .save(directory.join(format!("{size}-ridges-3x.png")))
        .unwrap();
        if size == 128 {
            image::imageops::resize(&heads, 896, 240, image::imageops::FilterType::Nearest)
                .save(directory.join("heads-8x.png"))
                .unwrap();
        }
    }
    eprintln!(
        "ridge comparisons: {} (NN / amounts 0.35, 0.5, 0.7)",
        directory.display()
    );
}

#[test]
#[ignore = "requires DIORAMA_GAME_ASSET_INPUT=.../elf2-se.png; visual sampling experiment"]
fn elf_sampling_offsets() {
    let source = image::open(std::env::var("DIORAMA_GAME_ASSET_INPUT").unwrap())
        .unwrap()
        .to_rgba8();
    assert_eq!(source.dimensions(), (800, 800));
    let directory = tempfile::Builder::new()
        .prefix("diorama-sampling-")
        .tempdir()
        .unwrap()
        .keep();
    let mut sheet = RgbaImage::from_pixel(384, 384, Rgba([82, 82, 82, 255]));
    let mut heads = RgbaImage::from_pixel(84, 90, Rgba([82, 82, 82, 255]));
    for (j, dy) in [0.25, 0.5, 0.75].into_iter().enumerate() {
        for (i, dx) in [0.25, 0.5, 0.75].into_iter().enumerate() {
            let output = phase(&source, 128, dx, dy);
            output
                .save(directory.join(format!("128-phase-{dx}-{dy}.png")))
                .unwrap();
            image::imageops::overlay(&mut sheet, &output, i as i64 * 128, j as i64 * 128);
            let head = image::imageops::crop_imm(&output, 50, 16, 28, 30).to_image();
            image::imageops::overlay(&mut heads, &head, i as i64 * 28, j as i64 * 30);
        }
    }
    let nearest = super::super::resize(
        &source,
        128,
        128,
        crate::document::Resampling::Nearest,
        &crate::document::CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(phase(&source, 128, 0.5, 0.5), nearest);
    image::imageops::resize(&sheet, 1152, 1152, image::imageops::FilterType::Nearest)
        .save(directory.join("phases-3x.png"))
        .unwrap();
    image::imageops::resize(&heads, 672, 720, image::imageops::FilterType::Nearest)
        .save(directory.join("heads-8x.png"))
        .unwrap();
    eprintln!(
        "sampling comparisons: {} (x offset increases across, y down; centre = NN)",
        directory.display()
    );
}
