//! Run in release mode with `--ignored --nocapture --test-threads=1`.
//! Measures completed CPU work, including allocations; excludes image decoding.
use super::*;
use std::{hint::black_box, time::Instant};

#[path = "readme_images.rs"]
mod readme_images;

fn report(case: &str, stage: &str, samples: &mut [f64], pixels: usize) {
    samples.sort_by(f64::total_cmp);
    let percentile = |p: f64| samples[((samples.len() - 1) as f64 * p).round() as usize];
    println!(
        "{case},{stage},n={},median_ms={:.3},p95_ms={:.3},iqr_ms={:.3},ns_per_source_pixel={:.2}",
        samples.len(),
        percentile(0.5) * 1e3,
        percentile(0.95) * 1e3,
        (percentile(0.75) - percentile(0.25)) * 1e3,
        percentile(0.5) * 1e9 / pixels as f64,
    );
}

fn synthetic(w: u32, h: u32) -> RgbaImage {
    RgbaImage::from_fn(w, h, |x, y| {
        let wave = h as f64 * (0.45 + 0.18 * (x as f64 / 37.).sin());
        let dark = (y as f64 - wave).abs() < 2.5 || x % 97 < 3;
        image::Rgba(if dark {
            [24, 18, 30, 255]
        } else {
            [
                130 + (x % 100) as u8,
                100 + (y % 110) as u8,
                170,
                if x < w / 8 { 0 } else { 255 },
            ]
        })
    })
}

/// Run the same first preview in development and release builds. Keep the
/// deadline in the invoking harness, so ordinary CI has no timing assertion.
#[test]
#[ignore = "manual first-preview latency check in either build profile"]
fn game_asset_first_preview() {
    let source = Arc::new(
        image::load_from_memory(include_bytes!("fixtures/elf.png"))
            .unwrap()
            .into_rgba8(),
    );
    let start = Instant::now();
    let output = Session::new(source)
        .resize(
            128,
            128,
            GameAssetAa::default(),
            &CancellationToken::default(),
        )
        .unwrap();
    let elapsed = start.elapsed();
    let expected = image::load_from_memory(include_bytes!("fixtures/elf-aa-128.png"))
        .unwrap()
        .into_rgba8();
    assert_eq!(output, expected);
    println!(
        "800x800 -> 128x128: {:.3} ms, debug_assertions={}, crc32={:08x}",
        elapsed.as_secs_f64() * 1000.,
        cfg!(debug_assertions),
        crc32fast::hash(output.as_raw()),
    );
}

/// Asset paths and output directories are supplied only to this manual harness,
/// never recognized by the renderer. Reuse source analysis, but bypass the
/// one-target cache to measure completed resizes, including the first preview.
#[test]
#[ignore = "manual asset visual comparison and latency measurement"]
fn game_asset_visual_check() {
    let source = match std::env::var("DIORAMA_SCALING_SOURCE") {
        Ok(path) => image::open(path).unwrap().into_rgba8(),
        Err(_) => image::load_from_memory(include_bytes!("fixtures/elf.png"))
            .unwrap()
            .into_rgba8(),
    };
    let directory = std::env::var("DIORAMA_SCALING_ARTIFACTS").unwrap();
    std::fs::create_dir_all(&directory).unwrap();
    let cancel = CancellationToken::default();
    let start = Instant::now();
    let prepared = Prepared::new(&source, &cancel).unwrap();
    println!(
        "source={}x{}, analysis_ms={:.3}",
        source.width(),
        source.height(),
        start.elapsed().as_secs_f64() * 1000.
    );
    for size in [128, 160, 200] {
        let start = Instant::now();
        let result = prepared
            .resize(&source, size, size, GameAssetAa::default(), &cancel)
            .unwrap();
        println!(
            "size={size}, target_ms={:.3}, crc32={:08x}",
            start.elapsed().as_secs_f64() * 1000.,
            crc32fast::hash(result.as_raw())
        );
        result
            .save(format!("{directory}/result-{size}.png"))
            .unwrap();
        image::imageops::resize(
            &result,
            size * 4,
            size * 4,
            image::imageops::FilterType::Nearest,
        )
        .save(format!("{directory}/zoom-{size}.png"))
        .unwrap();
        let start = Instant::now();
        let target = prepared
            .target_contours(&source, size, size, GameAssetAa::default(), &cancel)
            .unwrap();
        println!(
            "size={size}, contours_ms={:.3}",
            start.elapsed().as_secs_f64() * 1000.
        );
        let core = &target.strokes.core;
        let aa = &target.strokes.coverage;
        assert!(
            core.as_raw()
                .iter()
                .zip(aa.as_raw())
                .all(|(&c, &a)| if c == 0 { a <= 43 } else { a >= 243 })
        );
        core.save(format!("{directory}/core-{size}.png")).unwrap();
        aa.save(format!("{directory}/aa-{size}.png")).unwrap();
        if size == 128 {
            use std::io::Write;
            let strength = opacity::calculate(&prepared.widths, core, &target.strokes.owners);
            let applied = opacity::apply(aa, &target.strokes.owners, &strength);
            let mut csv = std::fs::File::create(format!("{directory}/core-pixels.csv")).unwrap();
            writeln!(csv, "x,y,owner,width,strength,aa,applied,ink_r,ink_g,ink_b").unwrap();
            for (i, &on) in core.as_raw().iter().enumerate() {
                if on == 0 {
                    continue;
                }
                let id = target.strokes.owners[i].unwrap();
                let c = target.colors[i];
                let rgb = color::rgba([c[0], c[1], c[2], 1.]);
                writeln!(
                    csv,
                    "{},{},{id},{:.6},{:.6},{},{},{},{},{}",
                    i % 128,
                    i / 128,
                    prepared.widths[id],
                    strength[id],
                    aa.as_raw()[i],
                    applied.as_raw()[i],
                    rgb[0],
                    rgb[1],
                    rgb[2]
                )
                .unwrap();
            }
            let crop = image::imageops::crop_imm(&result, 10, 18, 60, 38).to_image();
            image::imageops::resize(&crop, 780, 494, image::imageops::FilterType::Nearest)
                .save(format!("{directory}/crop-128.png"))
                .unwrap();
        }
    }
}

#[test]
#[ignore = "release performance measurement"]
fn game_asset_benchmark() {
    if cfg!(debug_assertions) {
        panic!("benchmark requires --release");
    }
    let elf = image::load_from_memory(include_bytes!("fixtures/elf.png"))
        .unwrap()
        .into_rgba8();
    let cancel = CancellationToken::default();
    // Hypothesis: eliminating repeated Gaussian work and sparse query overhead
    // reduces first-use and target latency. Timings within IQR falsify a win.
    // One untimed pilot warms code/data and fixes the sample count BEFORE the
    // measured run: ~8 seconds per case, at least 7 and at most 31 samples.
    // DIORAMA_BENCH_SAMPLES fixes equal counts for alternating binary A/B runs.
    for (name, source, targets) in [
        ("elf", elf, [(128, 128), (160, 160), (200, 200)]),
        ("odd", synthetic(257, 193), [(64, 48), (128, 96), (257, 96)]),
        (
            "large",
            synthetic(1024, 768),
            [(128, 96), (256, 192), (512, 384)],
        ),
    ] {
        if std::env::var("DIORAMA_BENCH_CASE").is_ok_and(|case| case != name) {
            continue;
        }
        let source = Arc::new(source);
        let pilot = Instant::now();
        let session = Session::new(source.clone());
        let expected: Vec<_> = targets
            .iter()
            .map(|&(w, h)| {
                session
                    .resize(w, h, GameAssetAa::default(), &cancel)
                    .unwrap()
            })
            .collect();
        let count = std::env::var("DIORAMA_BENCH_SAMPLES")
            .ok()
            .map(|n| n.parse::<usize>().unwrap().max(1))
            .unwrap_or_else(|| {
                (8. / pilot.elapsed().as_secs_f64()).round().clamp(7., 31.) as usize
            });
        let mut analysis = Vec::new();
        let mut first = Vec::new();
        let mut target_times = [Vec::new(), Vec::new(), Vec::new()];
        for _ in 0..count {
            let session = Session::new(source.clone());
            let start = Instant::now();
            let prepared = Arc::new(Prepared::new(&source, &cancel).unwrap());
            let preparation = start.elapsed().as_secs_f64();
            analysis.push(preparation);
            session.cache.lock().unwrap().prepared = Some(prepared);
            for (i, &(w, h)) in targets.iter().enumerate() {
                let start = Instant::now();
                let output = black_box(
                    session
                        .resize(w, h, GameAssetAa::default(), &cancel)
                        .unwrap(),
                );
                let elapsed = start.elapsed().as_secs_f64();
                target_times[i].push(elapsed);
                if i == 0 {
                    first.push(preparation + elapsed);
                }
                assert_eq!(output, expected[i], "nondeterministic {name} at {w}x{h}");
            }
        }
        let pixels = source.as_raw().len() / 4;
        println!("{name},crc32={:08x}", crc32fast::hash(expected[0].as_raw()));
        report(name, "analysis", &mut analysis, pixels);
        report(name, "first_resize", &mut first, pixels);
        for (i, times) in target_times.iter_mut().enumerate() {
            report(
                name,
                &format!("target_{}x{}", targets[i].0, targets[i].1),
                times,
                pixels,
            );
        }
    }
}
