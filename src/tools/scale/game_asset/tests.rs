use super::*;
use image::Rgba;

fn check_contract(source: &RgbaImage, result: &Scaled) {
    let grid = Grid::new(
        source,
        result.image.width(),
        result.image.height(),
        &result.options,
    )
    .unwrap();
    assert_eq!(result.provenance.len(), result.image.as_raw().len() / 4);
    for (p, (pixel, provenance)) in result.image.pixels().zip(&result.provenance).enumerate() {
        let (xs, ys) = grid.footprint(p);
        let (x, y) = provenance.source;
        assert!(xs.contains(&(x as usize)) && ys.contains(&(y as usize)));
        if pixel[3] == 0 {
            assert_eq!(pixel.0, [0, 0, 0, 0]);
        } else {
            assert_eq!(pixel[3], 255);
            let (cx, cy) = provenance.palette_source.unwrap_or((x, y));
            let original = source.get_pixel(cx, cy);
            assert!(source.get_pixel(x, y)[3] >= result.options.alpha_threshold);
            assert!(original[3] >= result.options.alpha_threshold);
            assert_eq!(&pixel.0[..3], &original.0[..3]);
            if provenance.palette_source.is_some() {
                use palette::{FromColor, Lab, Srgb};
                let lab = |p: &Rgba<u8>| {
                    let c: Lab = Lab::from_color(Srgb::new(
                        p[0] as f32 / 255.0,
                        p[1] as f32 / 255.0,
                        p[2] as f32 / 255.0,
                    ));
                    [c.l, c.a, c.b]
                };
                assert!(
                    palette_grid::delta_e(lab(original), lab(source.get_pixel(x, y)))
                        < result.options.palette_delta_e
                );
            }
        }
        let q = y as usize * grid.source_width + x as usize;
        assert_eq!(
            provenance.reconstructed,
            q != grid.baseline(p) || provenance.palette_source.is_some()
        );
    }
}

#[test]
fn identity_dimensions_alpha_provenance_and_cancellation() {
    let source = RgbaImage::from_fn(9, 7, |x, y| {
        Rgba([(x * 19) as u8, (y * 23) as u8, 57, (x * 31 + y) as u8])
    });
    let options = Options {
        allow_non_uniform: true,
        ..Default::default()
    };
    let cancellation = CancellationToken::default();
    let identity = resize(&source, 9, 7, &options, &cancellation).unwrap();
    assert_eq!(identity.image, source);
    for (w, h) in [(1, 1), (4, 3), (8, 6), (1, 7), (9, 1)] {
        let a = resize(&source, w, h, &options, &cancellation).unwrap();
        let b = resize(&source, w, h, &options, &cancellation).unwrap();
        check_contract(&source, &a);
        assert_eq!(a.image, b.image);
        assert_eq!(a.provenance, b.provenance);
        assert_eq!(a.diagnostics, b.diagnostics);
    }
    for (w, h) in [(0, 1), (1, 0), (10, 7), (9, 8)] {
        assert!(matches!(
            resize(&source, w, h, &options, &cancellation),
            Err(AppError::InvalidDimensions)
        ));
    }
    assert!(resize(&source, 2, 6, &Options::default(), &cancellation).is_err());
    cancellation.cancel();
    assert!(matches!(
        resize(&source, 9, 7, &options, &cancellation),
        Err(AppError::Cancelled)
    ));
}

#[test]
fn invisible_rgb_does_not_change_the_result() {
    let a = RgbaImage::from_fn(32, 32, |x, y| {
        if (8..24).contains(&x) && (8..24).contains(&y) {
            Rgba([30, 100, 220, 200])
        } else {
            Rgba([255, 0, 255, 0])
        }
    });
    let mut b = a.clone();
    for pixel in b.pixels_mut() {
        if pixel[3] == 0 {
            *pixel = Rgba([0, 255, 0, 0]);
        }
    }
    let options = Options::default();
    let cancellation = CancellationToken::default();
    let a = resize(&a, 8, 8, &options, &cancellation).unwrap();
    let b = resize(&b, 8, 8, &options, &cancellation).unwrap();
    assert_eq!(a.image, b.image);
    assert_eq!(a.diagnostics, b.diagnostics);
}

#[test]
fn near_identical_source_colours_share_one_palette_entry() {
    let source = RgbaImage::from_fn(16, 16, |_, y| {
        Rgba(if y < 8 {
            [100, 150, 100, 255]
        } else {
            [101, 150, 100, 255]
        })
    });
    let output = resize(
        &source,
        8,
        8,
        &Options::default(),
        &CancellationToken::default(),
    )
    .unwrap();
    let colours: std::collections::BTreeSet<_> = output.image.pixels().map(|p| p.0).collect();
    assert_eq!(
        colours.len(),
        1,
        "sub-threshold source colours should share one source-derived entry"
    );
}

#[test]
fn cached_analysis_matches_independent_resizes_and_respects_options() {
    let source = Arc::new(RgbaImage::from_fn(32, 32, |x, y| {
        if x == 9 || x == y {
            Rgba([120, 45, 180, 180])
        } else {
            Rgba([240, 230, 200, 255])
        }
    }));
    let session = Session::new(source.clone());
    let cancellation = CancellationToken::default();
    for (size, threshold) in [(16, 128), (15, 128), (15, 200)] {
        let options = Options {
            alpha_threshold: threshold,
            ..Default::default()
        };
        let cached = session.resize(size, size, &options, &cancellation).unwrap();
        let reference = resize(&source, size, size, &options, &cancellation).unwrap();
        assert_eq!(cached.image, reference.image);
        assert_eq!(cached.provenance, reference.provenance);
        assert_eq!(cached.diagnostics, reference.diagnostics);
    }
    let options = Options {
        memory_limit: 100,
        ..Default::default()
    };
    assert!(matches!(
        resize(&source, 16, 16, &options, &cancellation),
        Err(AppError::MemoryLimit { .. })
    ));
    cancellation.cancel();
    assert!(matches!(
        session.resize(16, 16, &Options::default(), &cancellation),
        Err(AppError::Cancelled)
    ));
}

#[test]
fn bresenham_is_connected_and_reversible_in_every_octant() {
    for x in -9..=9 {
        for y in -9..=9 {
            let a = geometry::line((0, 0), (x, y));
            let mut b = geometry::line((x, y), (0, 0));
            b.reverse();
            assert_eq!(a, b);
            assert!(
                a.windows(2)
                    .all(|s| (s[0].0 - s[1].0).abs() <= 1 && (s[0].1 - s[1].1).abs() <= 1)
            );
            assert_eq!(a.first(), Some(&(0, 0)));
            assert_eq!(a.last(), Some(&(x, y)));
        }
    }
}

#[test]
fn thin_dark_bright_and_chromatic_lines_survive_missed_nn_phases() {
    for (background, stroke) in [
        ([230, 230, 230, 255], [10, 10, 10, 255]),
        ([10, 10, 10, 255], [240, 240, 240, 255]),
        ([90, 140, 90, 255], [190, 70, 190, 255]),
    ] {
        let mut source = RgbaImage::from_pixel(64, 64, Rgba(background));
        for y in 8..56 {
            source.put_pixel(17, y, Rgba(stroke));
        }
        let result = resize(
            &source,
            16,
            16,
            &Options::default(),
            &CancellationToken::default(),
        )
        .unwrap();
        check_contract(&source, &result);
        let count = result.image.pixels().filter(|p| p.0 == stroke).count();
        assert!(
            count >= 10,
            "expected connected thin stroke, got {count}; {:?}",
            result.diagnostics
        );
        for y in 3..13 {
            assert_eq!(result.image.get_pixel(4, y).0, stroke);
        }
        assert!(
            result.image.pixels().filter(|p| p.0 == stroke).count() <= 14,
            "stroke thickened"
        );
    }
}

#[test]
fn gradients_are_not_promoted_into_strokes() {
    let source = RgbaImage::from_fn(32, 32, |x, _| {
        Rgba([(x * 7) as u8, (x * 7) as u8, (x * 7) as u8, 255])
    });
    let options = Options::default();
    let result = resize(&source, 8, 8, &options, &CancellationToken::default()).unwrap();
    let fill_only = resize(
        &source,
        8,
        8,
        &Options {
            context_weight: 0.0,
            ..options
        },
        &CancellationToken::default(),
    )
    .unwrap();
    // Quantization intentionally changes NN samples; line context must not
    // create stripes or reversals in an otherwise monotone palette fill.
    assert_eq!(result.image, fill_only.image);
    check_contract(&source, &result);
    for y in 0..8 {
        for x in 0..8 {
            assert_eq!(result.image.get_pixel(x, y), result.image.get_pixel(x, 0));
            if x > 0 {
                assert!(result.image.get_pixel(x, y)[0] >= result.image.get_pixel(x - 1, y)[0]);
            }
        }
    }
}

#[test]
fn thin_transparent_silhouette_is_reconstructed() {
    let mut source = RgbaImage::new(64, 64);
    for y in 8..56 {
        source.put_pixel(17, y, Rgba([180, 120, 40, 255]));
    }
    let result = resize(
        &source,
        16,
        16,
        &Options::default(),
        &CancellationToken::default(),
    )
    .unwrap();
    check_contract(&source, &result);
    for y in 3..13 {
        assert_eq!(
            result.image.get_pixel(4, y)[3],
            255,
            "missing thin silhouette"
        );
    }
}

#[test]
fn thin_slopes_curves_and_corners_keep_source_supported_pixels() {
    let options = Options::default();
    for (a, b) in [
        ((9, 9), (51, 33)),
        ((9, 51), (33, 9)),
        ((51, 9), (9, 33)),
        ((9, 9), (33, 51)),
    ] {
        let mut source = RgbaImage::from_pixel(64, 64, Rgba([220, 220, 220, 255]));
        for (x, y) in geometry::line(a, b) {
            source.put_pixel(x as u32, y as u32, Rgba([10, 10, 10, 255]));
        }
        let result = resize(&source, 16, 16, &options, &CancellationToken::default()).unwrap();
        check_contract(&source, &result);
        let nn = (0..256)
            .filter(|p| {
                let q = Grid::new(&source, 16, 16, &options).unwrap().baseline(*p);
                source.get_pixel((q % 64) as u32, (q / 64) as u32)[0] == 10
            })
            .count();
        let actual = result.image.pixels().filter(|p| p[0] == 10).count();
        assert!(actual >= nn, "slope lost supported stroke pixels");
    }
    let mut source = RgbaImage::from_pixel(64, 64, Rgba([220, 220, 220, 255]));
    for t in 0..150 {
        let angle = t as f32 / 149.0 * std::f32::consts::PI;
        source.put_pixel(
            (32.0 + 22.0 * angle.cos()).round() as u32,
            (24.0 + 22.0 * angle.sin()).round() as u32,
            Rgba([10, 10, 10, 255]),
        );
    }
    for (x, y) in geometry::line((11, 10), (31, 10))
        .into_iter()
        .chain(geometry::line((31, 10), (31, 23)))
    {
        source.put_pixel(x as u32, y as u32, Rgba([10, 10, 10, 255]));
    }
    let result = resize(&source, 16, 16, &options, &CancellationToken::default()).unwrap();
    check_contract(&source, &result);
}

#[test]
fn separate_components_and_representable_holes_are_not_joined_or_filled() {
    let source = RgbaImage::from_fn(64, 64, |x, y| {
        let ring = (8..28).contains(&x)
            && (8..56).contains(&y)
            && !((12..24).contains(&x) && (16..48).contains(&y));
        let bar = (36..44).contains(&x) && (8..56).contains(&y);
        if ring || bar {
            Rgba([90, 150, 70, 255])
        } else {
            Rgba([0, 0, 0, 0])
        }
    });
    let result = resize(
        &source,
        16,
        16,
        &Options::default(),
        &CancellationToken::default(),
    )
    .unwrap();
    check_contract(&source, &result);
    assert_eq!(result.diagnostics.component_losses, 0);
    assert_eq!(result.diagnostics.hole_losses, 0);
    assert_eq!(result.image.get_pixel(4, 7)[3], 0);
    assert_eq!(result.image.get_pixel(8, 7)[3], 0);
}

#[test]
fn incompatible_parallel_strokes_report_the_collision() {
    let mut source = RgbaImage::from_pixel(64, 64, Rgba([230, 230, 230, 255]));
    for y in 8..56 {
        for x in [17, 22] {
            source.put_pixel(x, y, Rgba([10, 10, 10, 255]));
        }
    }
    let result = resize(
        &source,
        16,
        16,
        &Options::default(),
        &CancellationToken::default(),
    )
    .unwrap();
    check_contract(&source, &result);
    assert!(
        result.diagnostics.collisions > 0,
        "unrepresentable parallel features need diagnostics"
    );
    assert!(
        result.diagnostics.joins <= result.diagnostics.baseline_joins,
        "introduced a join"
    );
}

/// Test-only graph measurement: a disconnected row of samples is not a line.
fn connected_span(image: &RgbaImage, region: impl Fn(u32, u32) -> bool) -> u32 {
    let mask: Vec<_> = image
        .enumerate_pixels()
        .map(|(x, y, p)| region(x, y) && p[3] >= 128)
        .collect();
    let labels = analysis::components(
        &mask,
        image.width() as usize,
        true,
        &CancellationToken::default(),
    )
    .unwrap();
    let mut spans = std::collections::BTreeMap::<u32, (u32, u32)>::new();
    for (p, label) in labels
        .into_iter()
        .enumerate()
        .filter(|(_, label)| *label != 0)
    {
        let y = p as u32 / image.width();
        let span = spans.entry(label).or_insert((y, y));
        span.0 = span.0.min(y);
        span.1 = span.1.max(y);
    }
    spans.values().map(|(a, b)| b - a + 1).max().unwrap_or(0)
}

#[test]
fn diagonal_alpha_filament_is_one_connected_stroke() {
    let mut source = RgbaImage::new(128, 128);
    for (x, y) in geometry::line((94, 15), (24, 110)) {
        // Varying colour on a connected alpha filament must not split it into
        // unrelated features. No output colour averaging is needed.
        source.put_pixel(
            x as u32,
            y as u32,
            Rgba([60 + (y % 9) as u8 * 9, 40, 20, 255]),
        );
        source.put_pixel(x as u32 + 1, y as u32, Rgba([100, 65, 30, 180]));
    }
    let result = resize(
        &source,
        32,
        32,
        &Options::default(),
        &CancellationToken::default(),
    )
    .unwrap();
    check_contract(&source, &result);
    assert!(
        connected_span(&result.image, |_, y| (5..27).contains(&y)) >= 22,
        "a supported diagonal filament must remain connected"
    );
}

/// The coordinates below describe the unobscured bowstring in elf2-se.png,
/// not a production detector hint. Compare alpha policies like for like.
#[test]
#[ignore = "requires DIORAMA_GAME_ASSET_INPUT=.../elf2-se.png; writes visual comparisons"]
fn elf_visual_quality() {
    let source = image::open(
        std::env::var("DIORAMA_GAME_ASSET_INPUT")
            .expect("set DIORAMA_GAME_ASSET_INPUT to elf2-se.png"),
    )
    .unwrap()
    .to_rgba8();
    assert_eq!(source.dimensions(), (800, 800));
    let sizes = std::env::var("DIORAMA_GAME_ASSET_SIZES").unwrap_or("96,128,160".into());
    let directory = tempfile::Builder::new()
        .prefix("diorama-game-asset-quality-")
        .tempdir()
        .unwrap()
        .keep();
    eprintln!("visual comparisons: {}", directory.display());
    let mut failures = Vec::new();
    let options = Options {
        grid_context: std::env::var("DIORAMA_GAME_ASSET_GRID_CONTEXT")
            .map(|value| value == "1")
            .unwrap_or(Options::default().grid_context),
        context_weight: std::env::var("DIORAMA_GAME_ASSET_CONTEXT_WEIGHT")
            .ok()
            .map(|v| v.parse().unwrap())
            .unwrap_or(Options::default().context_weight),
        palette_delta_e: std::env::var("DIORAMA_GAME_ASSET_PALETTE_DELTA_E")
            .ok()
            .map(|v| v.parse().unwrap())
            .unwrap_or(Options::default().palette_delta_e),
        ..Default::default()
    };
    for width in sizes.split(',').map(|s| s.parse::<u32>().unwrap()) {
        let start = std::time::Instant::now();
        let result = resize(
            &source,
            width,
            width,
            &options,
            &CancellationToken::default(),
        )
        .unwrap();
        let elapsed = start.elapsed();
        check_contract(&source, &result);
        let mut nearest = super::super::resize(
            &source,
            width,
            width,
            crate::document::Resampling::Nearest,
            &CancellationToken::default(),
        )
        .unwrap();
        nearest
            .save(directory.join(format!("{width}-nearest-rgba.png")))
            .unwrap();
        for p in nearest.pixels_mut() {
            if p[3] < 128 {
                *p = Rgba([0, 0, 0, 0]);
            } else {
                p[3] = 255;
            }
        }
        let top = (220.0 * width as f32 / 800.0).ceil() as u32;
        let bottom = (300.0 * width as f32 / 800.0).floor() as u32;
        let region = |x: u32, y: u32| {
            let sy = (y as f32 + 0.5) * 800.0 / width as f32;
            let centre = (548.0 - (sy - 215.0) * 0.73) * width as f32 / 800.0;
            (top..=bottom).contains(&y) && (x as f32 + 0.5 - centre).abs() < 1.2
        };
        let span = connected_span(&result.image, region);
        let nn_span = connected_span(&nearest, region);
        let feather_lights = |image: &RgbaImage| {
            (100 * width / 800..205 * width / 800)
                .flat_map(|y| (260 * width / 800..350 * width / 800).map(move |x| (x, y)))
                .filter(|(x, y)| {
                    let p = image.get_pixel(*x, *y);
                    p[3] >= 128 && p[0] > 150 && p[1] > 130 && p[2] > 90
                })
                .count()
        };
        let lights = feather_lights(&result.image);
        let nn_lights = feather_lights(&nearest);
        eprintln!("{width}: feather highlights {lights} (NN {nn_lights})");
        if lights * 4 < nn_lights * 3 {
            failures.push((width, lights as u32, nn_lights as u32));
        }
        for y in top..=bottom {
            let thickness = (0..width)
                .filter(|x| region(*x, y) && result.image.get_pixel(*x, y)[3] >= 128)
                .count();
            if thickness > 2 {
                eprintln!("bowstring thickened at {width}px, row {y}: {thickness}");
                failures.push((width, thickness as u32, 2));
            }
        }
        eprintln!(
            "{width}: bowstring span {span}/{} (binary NN {nn_span}), {elapsed:?}",
            bottom - top + 1
        );
        if span != bottom - top + 1 {
            failures.push((width, span, bottom - top + 1));
        }
        result
            .image
            .save(directory.join(format!("{width}-game-asset.png")))
            .unwrap();
        nearest
            .save(directory.join(format!("{width}-nearest-binary.png")))
            .unwrap();
        let mut panels = vec![nearest];
        if let Ok(before) = std::env::var("DIORAMA_GAME_ASSET_BEFORE") {
            let old =
                image::open(std::path::Path::new(&before).join(format!("{width}-game-asset.png")))
                    .unwrap()
                    .to_rgba8();
            assert_eq!(old.dimensions(), (width, width));
            panels.push(old);
        }
        panels.push(result.image);
        let mut sheet =
            RgbaImage::from_pixel(width * panels.len() as u32, width, Rgba([80, 80, 80, 255]));
        for (i, panel) in panels.iter().enumerate() {
            image::imageops::overlay(&mut sheet, panel, i as i64 * i64::from(width), 0);
        }
        image::imageops::resize(
            &sheet,
            sheet.width() * 4,
            width * 4,
            image::imageops::FilterType::Nearest,
        )
        .save(directory.join(format!("{width}-comparison-4x.png")))
        .unwrap();
        let left = 465 * width / 800;
        let upper = 205 * width / 800;
        let crop_width = (575 * width / 800).saturating_sub(left).max(1);
        let crop_height = (315 * width / 800).saturating_sub(upper).max(1);
        let mut detail = RgbaImage::from_pixel(
            crop_width * panels.len() as u32,
            crop_height,
            Rgba([80, 80, 80, 255]),
        );
        for (i, panel) in panels.iter().enumerate() {
            let crop =
                image::imageops::crop_imm(panel, left, upper, crop_width, crop_height).to_image();
            image::imageops::overlay(&mut detail, &crop, i as i64 * i64::from(crop_width), 0);
        }
        image::imageops::resize(
            &detail,
            detail.width() * 12,
            detail.height() * 12,
            image::imageops::FilterType::Nearest,
        )
        .save(directory.join(format!("{width}-bowstring-12x.png")))
        .unwrap();
    }
    assert!(
        failures.is_empty(),
        "visual quality failures (size, observed, reference): {failures:?}"
    );
}

/// Optional real-asset ablation. Inputs stay outside the repository; output
/// files are comparison artefacts, never source replacements.
#[test]
#[ignore = "requires DIORAMA_GAME_ASSET_INPUT; writes comparison PNGs"]
fn asset_ablation() {
    let Ok(input) = std::env::var("DIORAMA_GAME_ASSET_INPUT") else {
        eprintln!("Set DIORAMA_GAME_ASSET_INPUT to run the optional real-asset comparison");
        return;
    };
    let directory = tempfile::Builder::new()
        .prefix("diorama-game-asset-")
        .tempdir()
        .unwrap()
        .keep();
    let source = image::open(input).unwrap().to_rgba8();
    let width = std::env::var("DIORAMA_GAME_ASSET_WIDTH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(128);
    let height = (source.height() as f64 * width as f64 / source.width() as f64).round() as u32;
    let cancellation = CancellationToken::default();
    let mut baseline = super::super::resize(
        &source,
        width,
        height,
        crate::document::Resampling::Nearest,
        &cancellation,
    )
    .unwrap();
    for p in baseline.pixels_mut() {
        if p[3] < 128 {
            *p = Rgba([0, 0, 0, 0]);
        } else {
            p[3] = 255;
        }
    }
    baseline.save(directory.join("nearest.png")).unwrap();
    let mut sheet = RgbaImage::from_pixel(width * 5, height, Rgba([180, 180, 180, 255]));
    image::imageops::overlay(&mut sheet, &baseline, 0, 0);
    let session = Session::new(Arc::new(source.clone()));
    for (index, (name, options)) in [
        ("full", Options::default()),
        (
            "colour_only",
            Options {
                wavelet_weight: 0.0,
                fit_geometry: false,
                ..Default::default()
            },
        ),
        (
            "no_fitting",
            Options {
                fit_geometry: false,
                ..Default::default()
            },
        ),
        (
            "no_wavelets",
            Options {
                wavelet_weight: 0.0,
                ..Default::default()
            },
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let start = std::time::Instant::now();
        let result = if name == "full" {
            session
                .resize(width, height, &options, &cancellation)
                .unwrap()
        } else {
            resize(&source, width, height, &options, &cancellation).unwrap()
        };
        let elapsed = start.elapsed();
        check_contract(&source, &result);
        let d = &result.diagnostics;
        eprintln!(
            "{name}: {elapsed:?}; retained {}, dropped {}, unresolved {}, gaps {}, collisions {}, baseline breaks {}, final breaks {}, components {}, holes {}",
            d.retained.len(),
            d.dropped.len(),
            d.unresolved.len(),
            d.unsupported_gaps,
            d.collisions,
            d.baseline_connectivity_failures,
            d.connectivity_failures,
            d.component_losses,
            d.hole_losses
        );
        result
            .image
            .save(directory.join(format!("{name}.png")))
            .unwrap();
        image::imageops::overlay(
            &mut sheet,
            &result.image,
            (index as i64 + 1) * width as i64,
            0,
        );
        if name == "full" && width > 1 {
            let next_width = width - 1;
            let next_height = (source.height() as f64 * next_width as f64 / source.width() as f64)
                .round()
                .max(1.0) as u32;
            let started = std::time::Instant::now();
            let warm = session
                .resize(next_width, next_height, &options, &cancellation)
                .unwrap();
            eprintln!(
                "cached full {next_width}x{next_height}: {:?}",
                started.elapsed()
            );
            check_contract(&source, &warm);
        }
    }
    image::imageops::resize(
        &sheet,
        width * 5 * 4,
        height * 4,
        image::imageops::FilterType::Nearest,
    )
    .save(directory.join("comparison-4x.png"))
    .unwrap();
    eprintln!("comparison outputs: {}", directory.display());
}

/// Opt-in, real-pipeline performance gate. No machine-dependent timing in CI.
#[test]
#[ignore = "requires DIORAMA_GAME_ASSET_INPUT; optional latency budget"]
fn asset_latency() {
    let Ok(input) = std::env::var("DIORAMA_GAME_ASSET_INPUT") else {
        return;
    };
    let source = Arc::new(image::open(input).unwrap().to_rgba8());
    let width: u32 = std::env::var("DIORAMA_GAME_ASSET_WIDTH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(128);
    let rounds: usize = std::env::var("DIORAMA_GAME_ASSET_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    assert!(rounds > 0 && width > 1);
    let expected = std::env::var("DIORAMA_GAME_ASSET_EXPECTED_FINGERPRINTS")
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(|s| u32::from_str_radix(s, 16).unwrap())
                .collect::<Vec<_>>()
        });
    if let Some(expected) = &expected {
        assert_eq!(expected.len(), 2, "supply cold,cached fingerprints");
    }
    let backend = std::env::var("DIORAMA_GAME_ASSET_BACKEND").unwrap_or_else(|_| "auto".into());
    let options = Options {
        gpu_wavelets: backend != "cpu",
        ..Default::default()
    };
    let cancellation = CancellationToken::default();
    let mut cold = Vec::new();
    let mut warm = Vec::new();
    for round in 0..rounds {
        let session = Session::new(source.clone());
        for (target, timings) in [(width, &mut cold), (width - 1, &mut warm)] {
            let height = (source.height() as f64 * target as f64 / source.width() as f64)
                .round()
                .max(1.0) as u32;
            let started = std::time::Instant::now();
            let result = session
                .resize(target, height, &options, &cancellation)
                .unwrap();
            let elapsed = started.elapsed().as_secs_f64();
            if backend == "gpu" {
                assert!(result.gpu_wavelets, "GPU backend was not used");
            }
            timings.push(elapsed);
            check_contract(&source, &result);
            // Fingerprint pixels, provenance and diagnostics, outside the timing.
            let mut hash = crc32fast::Hasher::new();
            hash.update(result.image.as_raw());
            for sample in &result.provenance {
                hash.update(&sample.source.0.to_le_bytes());
                hash.update(&sample.source.1.to_le_bytes());
                hash.update(&[u8::from(sample.palette_source.is_some())]);
                if let Some((x, y)) = sample.palette_source {
                    hash.update(&x.to_le_bytes());
                    hash.update(&y.to_le_bytes());
                }
                hash.update(&[u8::from(sample.reconstructed)]);
                hash.update(&(sample.feature.map_or(u64::MAX, |f| f as u64)).to_le_bytes());
            }
            hash.update(format!("{:?}", result.diagnostics).as_bytes());
            let fingerprint = hash.finalize();
            if let Some(expected) = &expected {
                assert_eq!(
                    fingerprint,
                    expected[usize::from(target != width)],
                    "output changed at {target}x{height}"
                );
            }
            eprintln!(
                "round {round}, {target}x{height}: {elapsed:.3}s, fingerprint {:08x}, GPU {}",
                fingerprint, result.gpu_wavelets
            );
        }
    }
    cold.sort_by(f64::total_cmp);
    warm.sort_by(f64::total_cmp);
    eprintln!(
        "median cold {:.3}s, cached {:.3}s",
        cold[rounds / 2],
        warm[rounds / 2]
    );
    if let Ok(limit) = std::env::var("DIORAMA_GAME_ASSET_MAX_SECONDS") {
        let limit: f64 = limit.parse().unwrap();
        assert!(
            cold[rounds / 2] <= limit,
            "cold resize exceeds {limit}s budget"
        );
    }
}

#[test]
#[ignore = "requires DIORAMA_GAME_ASSET_INPUT and hardware GPU; writes comparison PNGs"]
fn gpu_asset_comparison() {
    let Ok(input) = std::env::var("DIORAMA_GAME_ASSET_INPUT") else {
        return;
    };
    let source = Arc::new(image::open(input).unwrap().to_rgba8());
    let width: u32 = std::env::var("DIORAMA_GAME_ASSET_WIDTH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(128);
    let height = (source.height() as f64 * width as f64 / source.width() as f64).round() as u32;
    let directory = tempfile::Builder::new()
        .prefix("diorama-game-asset-gpu-")
        .tempdir()
        .unwrap()
        .keep();
    let cancellation = CancellationToken::default();
    let started = std::time::Instant::now();
    let cpu = resize(
        &source,
        width,
        height,
        &Options {
            gpu_wavelets: false,
            ..Default::default()
        },
        &cancellation,
    )
    .unwrap();
    eprintln!("CPU full: {:?}", started.elapsed());
    let started = std::time::Instant::now();
    let gpu = resize(&source, width, height, &Options::default(), &cancellation).unwrap();
    eprintln!(
        "GPU full including setup/upload/readback: {:?}",
        started.elapsed()
    );
    assert!(gpu.gpu_wavelets, "GPU was not used");
    check_contract(&source, &cpu);
    check_contract(&source, &gpu);
    let repeated = resize(&source, width, height, &Options::default(), &cancellation).unwrap();
    assert!(repeated.gpu_wavelets);
    assert_eq!(
        gpu.image, repeated.image,
        "GPU rendering must be repeatable"
    );
    assert_eq!(gpu.provenance, repeated.provenance);
    assert_eq!(gpu.diagnostics, repeated.diagnostics);
    let changed = cpu
        .image
        .pixels()
        .zip(gpu.image.pixels())
        .filter(|(a, b)| a != b)
        .count();
    eprintln!(
        "CPU/GPU changed pixels: {changed}/{}; CPU retained/dropped/unresolved {}/{}/{}; GPU {}/{}/{}",
        width * height,
        cpu.diagnostics.retained.len(),
        cpu.diagnostics.dropped.len(),
        cpu.diagnostics.unresolved.len(),
        gpu.diagnostics.retained.len(),
        gpu.diagnostics.dropped.len(),
        gpu.diagnostics.unresolved.len()
    );
    cpu.image.save(directory.join("cpu.png")).unwrap();
    gpu.image.save(directory.join("gpu.png")).unwrap();
    let mut sheet = RgbaImage::from_pixel(width * 2, height, Rgba([180, 180, 180, 255]));
    image::imageops::overlay(&mut sheet, &cpu.image, 0, 0);
    image::imageops::overlay(&mut sheet, &gpu.image, width as i64, 0);
    image::imageops::resize(
        &sheet,
        width * 8,
        height * 4,
        image::imageops::FilterType::Nearest,
    )
    .save(directory.join("comparison-4x.png"))
    .unwrap();
    eprintln!("comparison outputs: {}", directory.display());
}
