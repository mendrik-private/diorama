use super::*;
use crate::document::{Document, ImageSource, Metadata, Operation, Resampling};

fn legacy_resize(
    prepared: &Prepared,
    source: &RgbaImage,
    w: u32,
    h: u32,
    aa: GameAssetAa,
    cancel: &CancellationToken,
) -> RgbaImage {
    let TargetContours { strokes, colors } = prepared
        .target_contours(source, w, h, aa, cancel)
        .expect("legacy contours");
    let strength = opacity::calculate(&prepared.widths, &strokes.core, &strokes.owners);
    let paint = opacity::apply(&strokes.coverage, &strokes.owners, &strength);
    let scale = [
        w as f64 / source.width() as f64,
        h as f64 / source.height() as f64,
    ];
    let retained = prepared
        .contours
        .retained_ink_mask(&prepared.mask, scale, cancel)
        .expect("legacy retained mask");
    let repaired = biharmonic::repair(&prepared.linear, &retained, cancel).expect("legacy repair");
    let base = project::area(&repaired, w as usize, h as usize, cancel).expect("legacy area");
    paint::composite(&base, &colors, &paint)
}

fn outlined_fixture(transparent: bool) -> RgbaImage {
    RgbaImage::from_fn(64, 60, |x, y| {
        let inside = (17..47).contains(&x) && (14..48).contains(&y);
        let edge = inside && (x == 17 || x == 46 || y == 14 || y == 47);
        if edge {
            image::Rgba([8, 12, 20, 255])
        } else if inside {
            image::Rgba([
                100 + ((x - 18) * 3) as u8,
                50 + ((y - 15) * 4) as u8,
                40 + ((x + y) % 35) as u8,
                255,
            ])
        } else if transparent {
            image::Rgba([235, 1, 99, 0])
        } else {
            image::Rgba([37, 83, 149, 255])
        }
    })
}

fn source_alpha_footprint(
    source: &RgbaImage,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> (bool, bool, f64, f64) {
    let sx = source.width() as f64 / w as f64;
    let sy = source.height() as f64 / h as f64;
    let (left, right) = (x as f64 * sx, (x + 1) as f64 * sx);
    let (top, bottom) = (y as f64 * sy, (y + 1) as f64 * sy);
    let mut all_zero = true;
    let mut all_solid = true;
    let mut alpha = 0.;
    let mut support = 0.;
    for yy in top.floor() as u32..(bottom.ceil() as u32).min(source.height()) {
        let yw = (bottom.min((yy + 1) as f64) - top.max(yy as f64)) / sy;
        for xx in left.floor() as u32..(right.ceil() as u32).min(source.width()) {
            let xw = (right.min((xx + 1) as f64) - left.max(xx as f64)) / sx;
            let sample = source.get_pixel(xx, yy)[3];
            all_zero &= sample == 0;
            all_solid &= sample >= 128;
            alpha += f64::from(sample) / 255. * xw * yw;
            support += f64::from(sample != 0) * xw * yw;
        }
    }
    (all_zero, all_solid, alpha, support)
}

#[test]
fn silhouette_removes_legacy_exterior_halo_without_changing_deep_area_color() {
    let source = Arc::new(outlined_fixture(false));
    let cancel = CancellationToken::default();
    let session = Session::new(source.clone());
    let current = session
        .resize(31, 29, GameAssetAa::new(0), &cancel)
        .unwrap();
    let prepared = session.cache.lock().unwrap().prepared.clone().unwrap();
    let legacy = legacy_resize(&prepared, &source, 31, 29, GameAssetAa::new(0), &cancel);
    let silhouette = prepared.silhouette.as_ref().unwrap();
    let coverage = silhouette.coverage(31, 29, &cancel).unwrap();
    let target = prepared
        .target_contours(&source, 31, 29, GameAssetAa::new(0), &cancel)
        .unwrap();
    let background = image::Rgba([37, 83, 149, 255]);
    let halo = (0..coverage.len()).find(|&i| {
        coverage[i] < 0.5
            && target.strokes.coverage.as_raw()[i] == 0
            && *current.get_pixel((i % 31) as u32, (i / 31) as u32) == background
            && current.get_pixel((i % 31) as u32, (i / 31) as u32)
                != legacy.get_pixel((i % 31) as u32, (i / 31) as u32)
    });
    assert!(
        halo.is_some(),
        "fixture must reproduce a legacy exterior halo"
    );

    // This target cell maps entirely inside the smooth fill, away from its
    // outline.  The support path must retain the established AREA interior.
    let old_area = project::area(&prepared.linear, 31, 29, &cancel).unwrap();
    let i = 21 * 31 + 19;
    let expected = color::rgba(old_area.pixels[i]);
    assert_eq!(*current.get_pixel(19, 21), expected);

    let transparent = Arc::new(outlined_fixture(true));
    let transparent_session = Session::new(transparent);
    let transparent_result = transparent_session
        .resize(31, 29, GameAssetAa::new(0), &cancel)
        .unwrap();
    let transparent_prepared = transparent_session
        .cache
        .lock()
        .unwrap()
        .prepared
        .clone()
        .unwrap();
    let transparent_silhouette = transparent_prepared.silhouette.as_ref().unwrap();
    let transparent_coverage = transparent_silhouette.coverage(31, 29, &cancel).unwrap();
    let transparent_target = transparent_prepared
        .target_contours(
            &outlined_fixture(true),
            31,
            29,
            GameAssetAa::new(0),
            &cancel,
        )
        .unwrap();
    for (i, &support) in transparent_coverage.iter().enumerate() {
        if support < 0.5 && transparent_target.strokes.coverage.as_raw()[i] == 0 {
            assert_eq!(transparent_result.as_raw()[i * 4 + 3], 0, "pixel {i}");
            assert_eq!(&transparent_result.as_raw()[i * 4..i * 4 + 4], &[0; 4]);
        }
    }
}

#[test]
fn silhouette_support_handles_every_small_downscale_shape_and_aa() {
    let source = Arc::new(RgbaImage::from_fn(11, 9, |x, y| {
        let inside = (2..9).contains(&x) && (2..7).contains(&y);
        image::Rgba(if inside {
            [80 + x as u8 * 9, 30 + y as u8 * 11, 50, 255]
        } else {
            [37, 83, 149, 255]
        })
    }));
    let session = Session::new(source.clone());
    let cancel = CancellationToken::default();
    for aa in [
        GameAssetAa::new(0),
        GameAssetAa::new(50),
        GameAssetAa::new(100),
    ] {
        for w in 1..=source.width() {
            for h in 1..=source.height() {
                let output = session.resize(w, h, aa, &cancel).unwrap();
                assert_eq!(output.dimensions(), (w, h));
                let prepared = session.cache.lock().unwrap().prepared.clone().unwrap();
                let silhouette = prepared.silhouette.as_ref().unwrap();
                let coverage = silhouette
                    .coverage(w as usize, h as usize, &cancel)
                    .unwrap();
                let contours = prepared
                    .target_contours(&source, w, h, aa, &cancel)
                    .unwrap();
                for (i, &support) in coverage.iter().enumerate() {
                    if support == 0. && contours.strokes.coverage.as_raw()[i] == 0 {
                        assert_eq!(
                            output.get_pixel((i % w as usize) as u32, (i / w as usize) as u32),
                            &image::Rgba([37, 83, 149, 255])
                        );
                    }
                }
            }
        }
    }
    assert_eq!(
        session
            .resize(
                source.width(),
                source.height(),
                GameAssetAa::new(0),
                &cancel
            )
            .unwrap(),
        *source
    );
}

#[test]
fn transparent_intrinsic_alpha_survives_hard_silhouette_coverage() {
    let source = Arc::new(RgbaImage::from_fn(16, 16, |x, y| {
        if (4..12).contains(&x) && (4..12).contains(&y) {
            image::Rgba([120, 40, 200, 128])
        } else {
            image::Rgba([255, 2, 90, 0])
        }
    }));
    let output = Session::new(source)
        .resize(8, 8, GameAssetAa::new(0), &CancellationToken::default())
        .unwrap();
    assert_eq!(*output.get_pixel(3, 3), image::Rgba([120, 40, 200, 128]));
    assert_eq!(*output.get_pixel(0, 0), image::Rgba([0; 4]));
}

#[test]
fn elf_bow_connection_survives_silhouette_clipping_at_all_aa_levels() {
    let source = Arc::new(
        image::load_from_memory(include_bytes!("fixtures/elf.png"))
            .unwrap()
            .into_rgba8(),
    );
    let session = Session::new(source);
    let cancel = CancellationToken::default();
    for aa in [
        GameAssetAa::new(0),
        GameAssetAa::new(50),
        GameAssetAa::new(100),
    ] {
        let output = session.resize(128, 128, aa, &cancel).unwrap();
        for (x, y) in [(80, 66), (81, 66), (80, 67)] {
            assert!(
                output.get_pixel(x, y)[3] >= 240,
                "AA{} erased bow at ({x},{y}): {:?}",
                aa.percent(),
                output.get_pixel(x, y)
            );
        }
    }
}

/// Opt-in visual harness for the real reported asset.  It deliberately has no
/// fixture dependency in the normal test suite; set both variables to write
/// directly comparable legacy and candidate images.
#[test]
#[ignore = "manual external-asset verification"]
fn writes_wizard_halo_artifacts_when_requested() {
    let Ok(input) = std::env::var("DIORAMA_GAME_ASSET_HALO_INPUT") else {
        return;
    };
    let directory = std::env::var("DIORAMA_GAME_ASSET_HALO_ARTIFACTS")
        .expect("artifact directory is required with halo input");
    std::fs::create_dir_all(&directory).unwrap();
    let source = Arc::new(image::open(input).unwrap().into_rgba8());
    let session = Session::new(source.clone());
    let cancel = CancellationToken::default();
    for size in [159, 160, 161] {
        let after = session
            .resize(size, size, GameAssetAa::new(0), &cancel)
            .unwrap();
        let prepared = session.cache.lock().unwrap().prepared.clone().unwrap();
        let before = legacy_resize(&prepared, &source, size, size, GameAssetAa::new(0), &cancel);
        before
            .save(format!("{directory}/wizard-{size}-aa0-before.png"))
            .unwrap();
        after
            .save(format!("{directory}/wizard-{size}-aa0-after.png"))
            .unwrap();
        let x = 70.min(size - 1);
        let y = 12.min(size - 1);
        let width = 55.min(size - x);
        let height = 55.min(size - y);
        for (name, image) in [("before", &before), ("after", &after)] {
            let crop = image::imageops::crop_imm(image, x, y, width, height).to_image();
            image::imageops::resize(
                &crop,
                width * 8,
                height * 8,
                image::imageops::FilterType::Nearest,
            )
            .save(format!("{directory}/wizard-{size}-aa0-{name}-hood-x8.png"))
            .unwrap();
        }
    }
    for &(w, h) in &[
        (32, 32),
        (64, 64),
        (96, 96),
        (128, 128),
        (159, 159),
        (160, 160),
        (161, 161),
        (200, 200),
        (256, 256),
        (512, 512),
        (1253, 1253),
        (1254, 1254),
        (160, 97),
        (97, 160),
    ] {
        assert_eq!(
            session
                .resize(w, h, GameAssetAa::new(0), &cancel)
                .unwrap()
                .dimensions(),
            (w, h)
        );
    }
    let elf = Arc::new(
        image::load_from_memory(include_bytes!("fixtures/elf.png"))
            .unwrap()
            .into_rgba8(),
    );
    let elf_session = Session::new(elf);
    for size in [128, 160, 200] {
        for aa in [GameAssetAa::new(0), GameAssetAa::default()] {
            elf_session
                .resize(size, size, aa, &cancel)
                .unwrap()
                .save(format!(
                    "{directory}/elf-transparent-{size}-aa{}-after.png",
                    aa.percent()
                ))
                .unwrap();
        }
    }
}
#[test]
fn reviewed_manual_aa_and_independent_background_at_all_sizes() {
    let source = Arc::new(
        image::load_from_memory(include_bytes!("fixtures/elf.png"))
            .unwrap()
            .into_rgba8(),
    );
    let session = Session::new(source.clone());
    for (size, bytes, legacy) in [
        (
            128,
            include_bytes!("fixtures/elf-aa-128.png").as_slice(),
            include_bytes!("fixtures/elf-128.png").as_slice(),
        ),
        (
            160,
            include_bytes!("fixtures/elf-aa-160.png").as_slice(),
            include_bytes!("fixtures/elf-160.png").as_slice(),
        ),
        (
            200,
            include_bytes!("fixtures/elf-aa-200.png").as_slice(),
            include_bytes!("fixtures/elf-200.png").as_slice(),
        ),
    ] {
        let expected = image::load_from_memory(bytes).unwrap().into_rgba8();
        let result = session
            .resize(
                size,
                size,
                GameAssetAa::default(),
                &CancellationToken::default(),
            )
            .unwrap();
        if let Ok(directory) = std::env::var("DIORAMA_SCALING_ARTIFACTS") {
            std::fs::create_dir_all(&directory).unwrap();
            result.save(format!("{directory}/elf-{size}.png")).unwrap();
        }
        let differences: Vec<_> = result
            .as_raw()
            .iter()
            .zip(expected.as_raw())
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .collect();
        assert!(
            differences.is_empty(),
            "{size}px: {} changed channels; first {:?}",
            differences.len(),
            differences.first()
        );
        // The reviewed images are visual snapshots, not independent math
        // oracles. Check source alpha footprints directly: solid (all source
        // samples alpha >= 128),
        // unpainted pixels retain legacy parity; empty footprints are exactly
        // transparent; partial support may change at the silhouette edge but
        // must not exceed its independently projected intrinsic alpha.
        let legacy = image::load_from_memory(legacy).unwrap().into_rgba8();
        let prepared = session.cache.lock().unwrap().prepared.clone().unwrap();
        let target = prepared
            .target_contours(
                &source,
                size,
                size,
                GameAssetAa::default(),
                &CancellationToken::default(),
            )
            .unwrap();
        let core = raster::Mask {
            w: size as usize,
            h: size as usize,
            data: target
                .strokes
                .core
                .as_raw()
                .iter()
                .map(|&v| v != 0)
                .collect(),
        };
        let mut checked = 0;
        let mut full_support_checked = 0;
        for y in 0..size {
            for x in 0..size {
                let i = (y * size + x) as usize;
                if target.strokes.coverage.as_raw()[i] == 0 {
                    let (all_zero, all_solid, alpha, support) =
                        source_alpha_footprint(&source, x, y, size, size);
                    if all_zero {
                        assert_eq!(result.get_pixel(x, y), &image::Rgba([0; 4]));
                    } else if all_solid
                        && !(-2..=2)
                            .any(|dy| (-2..=2).any(|dx| core.at(x as isize + dx, y as isize + dy)))
                    {
                        assert_eq!(
                            result.get_pixel(x, y),
                            legacy.get_pixel(x, y),
                            "full-support pixel changed at {size}px ({x},{y})"
                        );
                        full_support_checked += 1;
                    } else {
                        assert!(
                            f64::from(result.get_pixel(x, y)[3]) / 255.
                                <= alpha / support + 1. / 255.,
                            "partial-support alpha grew at {size}px ({x},{y})"
                        );
                    }
                    checked += 1;
                }
                if target.strokes.core.get_pixel(x, y)[0] != 0 {
                    assert!(target.strokes.coverage.get_pixel(x, y)[0] >= 243);
                }
            }
        }
        assert!(checked > size * size / 2);
        assert!(
            full_support_checked > 0,
            "{size}px must exercise full-support legacy parity"
        );
    }
}
#[test]
fn rectangle_preview_and_document_share_one_cached_algorithm() {
    let image = Arc::new(RgbaImage::from_fn(41, 25, |x, y| {
        image::Rgba(if x == 20 {
            [0, 0, 0, 255]
        } else {
            [(x * 6) as u8, (y * 9) as u8, 120, 255]
        })
    }));
    let session = Session::new(image.clone());
    let cancel = CancellationToken::default();
    let preview = session
        .resize(20, 12, GameAssetAa::default(), &cancel)
        .unwrap();
    let prepared = session.cache.lock().unwrap().prepared.clone().unwrap();
    assert_eq!(
        preview,
        super::super::resize(
            &image,
            20,
            12,
            Resampling::GameAsset(Default::default()),
            &cancel
        )
        .unwrap()
    );
    let mut document = Document::new(ImageSource {
        pixels: image,
        path: None,
        metadata: Metadata::default(),
    });
    document.apply(Operation::Scale {
        width: 20,
        height: 12,
        resampling: Resampling::GameAsset(Default::default()),
    });
    assert_eq!(preview, document.render(&cancel).unwrap().pixels);
    assert_eq!(
        session
            .resize(20, 12, GameAssetAa::default(), &cancel)
            .unwrap(),
        preview
    );
    assert_eq!(
        session
            .resize(1, 10, GameAssetAa::default(), &cancel)
            .unwrap()
            .dimensions(),
        (1, 10)
    );
    assert!(Arc::ptr_eq(
        &prepared,
        session.cache.lock().unwrap().prepared.as_ref().unwrap()
    ));
}
#[test]
fn aa_changes_cache_identity_and_survives_commit_undo_redo() {
    let source = Arc::new(RgbaImage::from_fn(96, 80, |x, y| {
        let diagonal = (y as f64 - (0.57 * x as f64 + 10.)).abs() < 3.;
        image::Rgba(if diagonal {
            [8, 10, 4, 255]
        } else {
            [130, 170, 90, 255]
        })
    }));
    let session = Session::new(source.clone());
    let cancel = CancellationToken::default();
    let mut outputs = Vec::new();
    let mut prepared = None;
    for percent in [0, 100, 50, 0] {
        let aa = GameAssetAa::new(percent);
        let preview = session.resize(32, 27, aa, &cancel).unwrap();
        let cache = session.cache.lock().unwrap();
        assert_eq!(cache.target.as_ref().unwrap().0, (32, 27, aa));
        if let Some(old) = &prepared {
            assert!(Arc::ptr_eq(old, cache.prepared.as_ref().unwrap()));
        } else {
            prepared = cache.prepared.clone();
        }
        let cached_result = cache.target.as_ref().unwrap().1.clone();
        drop(cache);
        assert_eq!(session.resize(32, 27, aa, &cancel).unwrap(), preview);
        assert!(Arc::ptr_eq(
            &cached_result,
            &session.cache.lock().unwrap().target.as_ref().unwrap().1
        ));
        let mut document = Document::new(ImageSource {
            pixels: source.clone(),
            path: None,
            metadata: Metadata::default(),
        });
        let operation = Operation::Scale {
            width: 32,
            height: 27,
            resampling: Resampling::GameAsset(aa),
        };
        document.apply(operation.clone());
        assert_eq!(document.render(&cancel).unwrap().pixels, preview);
        assert!(document.undo());
        assert_eq!(document.render(&cancel).unwrap().pixels, *source);
        assert!(document.redo());
        assert_eq!(document.operations(), &[operation]);
        assert_eq!(document.render(&cancel).unwrap().pixels, preview);
        outputs.push(preview);
    }
    assert_ne!(outputs[0], outputs[1], "fixture must exercise AA");
    assert_ne!(outputs[1], outputs[2]);
    assert_eq!(
        outputs[0], outputs[3],
        "switching back must not return stale AA"
    );
}

#[test]
fn validates_dimensions_and_cancellation_without_populating_cache() {
    let session = Session::new(Arc::new(RgbaImage::new(8, 6)));
    for (w, h) in [(0, 3), (4, 0), (9, 3), (4, 7)] {
        assert!(matches!(
            session.resize(w, h, GameAssetAa::default(), &CancellationToken::default()),
            Err(AppError::InvalidDimensions)
        ));
    }
    let cancel = CancellationToken::default();
    cancel.cancel();
    for (w, h) in [(8, 6), (4, 3)] {
        assert!(matches!(
            session.resize(w, h, GameAssetAa::default(), &cancel),
            Err(AppError::Cancelled)
        ));
    }
    assert!(session.cache.lock().unwrap().prepared.is_none());
}
#[test]
fn transparent_rgb_does_not_bleed_and_empty_sources_remain_empty() {
    let a = RgbaImage::from_fn(12, 8, |x, _| {
        image::Rgba(if x < 6 {
            [10, 80, 190, 255]
        } else {
            [255, 0, 70, 0]
        })
    });
    let mut b = a.clone();
    for p in b.pixels_mut() {
        if p[3] == 0 {
            *p = image::Rgba([0; 4]);
        }
    }
    let cancel = CancellationToken::default();
    assert_eq!(
        Session::new(Arc::new(a))
            .resize(4, 3, GameAssetAa::default(), &cancel)
            .unwrap(),
        Session::new(Arc::new(b))
            .resize(4, 3, GameAssetAa::default(), &cancel)
            .unwrap()
    );
    let empty = Session::new(Arc::new(RgbaImage::from_pixel(
        9,
        7,
        image::Rgba([255, 0, 70, 0]),
    )))
    .resize(3, 2, GameAssetAa::default(), &cancel)
    .unwrap();
    assert!(empty.pixels().all(|p| p.0 == [0; 4]));
}

#[test]
fn retained_ink_excludes_short_neighbors() {
    let mut trace = raster::Mask::new(24, 12);
    for x in 2..20 {
        trace.data[5 * 24 + x] = true;
    }
    for x in 8..10 {
        trace.data[7 * 24 + x] = true;
    }
    let cancel = CancellationToken::default();
    let contours = contours::Contours::new(&trace, &[], &cancel).unwrap();
    let retained = contours
        .retained_ink_mask(&trace, [1., 1.], &cancel)
        .unwrap();
    assert!(retained.data[5 * 24 + 8]);
    assert!(!retained.data[7 * 24 + 8]);
}

#[test]
fn cancelled_cached_requests_and_oversized_analysis_are_rejected() {
    let session = Session::new(Arc::new(RgbaImage::new(12, 8)));
    session
        .resize(6, 4, GameAssetAa::default(), &CancellationToken::default())
        .unwrap();
    let cancel = CancellationToken::default();
    cancel.cancel();
    assert!(matches!(
        session.resize(6, 4, GameAssetAa::default(), &cancel),
        Err(AppError::Cancelled)
    ));
    let large = Session::new(Arc::new(RgbaImage::new(2048, 2048)));
    assert!(matches!(
        large.resize(
            128,
            128,
            GameAssetAa::default(),
            &CancellationToken::default()
        ),
        Err(AppError::MemoryLimit { .. })
    ));
    assert!(large.cache.lock().unwrap().prepared.is_none());
}
