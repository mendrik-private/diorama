use super::*;
use crate::document::{Document, ImageSource, Metadata, Operation, Resampling};

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

fn source_alpha_footprint(source: &RgbaImage, x: u32, y: u32, w: u32, h: u32) -> (bool, f64, f64) {
    let sx = source.width() as f64 / w as f64;
    let sy = source.height() as f64 / h as f64;
    let (left, right) = (x as f64 * sx, (x + 1) as f64 * sx);
    let (top, bottom) = (y as f64 * sy, (y + 1) as f64 * sy);
    let mut all_zero = true;
    let mut alpha = 0.;
    let mut support = 0.;
    for yy in top.floor() as u32..(bottom.ceil() as u32).min(source.height()) {
        let yw = (bottom.min((yy + 1) as f64) - top.max(yy as f64)) / sy;
        for xx in left.floor() as u32..(right.ceil() as u32).min(source.width()) {
            let xw = (right.min((xx + 1) as f64) - left.max(xx as f64)) / sx;
            let sample = source.get_pixel(xx, yy)[3];
            all_zero &= sample == 0;
            alpha += f64::from(sample) / 255. * xw * yw;
            support += f64::from(sample != 0) * xw * yw;
        }
    }
    (all_zero, alpha, support)
}

/// Independent test oracle for direct source sampling. This intentionally
/// performs its own f32 premultiplied conversion and `image` Lanczos call,
/// rather than invoking the production Game Asset helper.
fn direct_source_lanczos_oracle(source: &color::LinearImage, w: u32, h: u32) -> RgbaImage {
    let samples: Vec<f32> = source
        .pixels
        .iter()
        .flat_map(|sample| {
            let alpha = sample[3] as f32;
            [
                sample[0] as f32 * alpha,
                sample[1] as f32 * alpha,
                sample[2] as f32 * alpha,
                alpha,
            ]
        })
        .collect();
    let premultiplied = image::ImageBuffer::<image::Rgba<f32>, Vec<f32>>::from_vec(
        source.w as u32,
        source.h as u32,
        samples,
    )
    .expect("linear source dimensions match its samples");
    let resized =
        image::imageops::resize(&premultiplied, w, h, image::imageops::FilterType::Lanczos3);
    RgbaImage::from_fn(w, h, |x, y| {
        let sample = resized.get_pixel(x, y);
        let alpha = f64::from(sample[3]).clamp(0., 1.);
        color::rgba(if alpha <= 1e-8 {
            [0.; 4]
        } else {
            [
                (f64::from(sample[0]) / alpha).clamp(0., 1.),
                (f64::from(sample[1]) / alpha).clamp(0., 1.),
                (f64::from(sample[2]) / alpha).clamp(0., 1.),
                alpha,
            ]
        })
    })
}

#[test]
fn silhouette_keeps_exterior_and_transparent_hidden_rgb_isolated() {
    let source = Arc::new(outlined_fixture(false));
    let cancel = CancellationToken::default();
    let session = Session::new(source.clone());
    let current = session
        .resize(31, 29, GameAssetAa::new(0), &cancel)
        .unwrap();
    let prepared = session.cache.lock().unwrap().prepared.clone().unwrap();
    let silhouette = prepared.silhouette.as_ref().unwrap();
    let coverage = silhouette.coverage(31, 29, &cancel).unwrap();
    let target = prepared
        .target_contours(&source, 31, 29, GameAssetAa::new(0), &cancel)
        .unwrap();
    assert!(
        (0..coverage.len()).any(|i| {
            coverage[i] < 0.5
                && target.strokes.coverage.as_raw()[i] == 0
                && current.as_raw()[i * 4..i * 4 + 4] == [37, 83, 149, 255]
        }),
        "fixture must preserve an unpainted opaque exterior"
    );
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

#[test]
fn direct_lanczos_source_oracle_and_silhouette_support_hold_at_all_sizes() {
    let source = Arc::new(
        image::load_from_memory(include_bytes!("fixtures/elf.png"))
            .unwrap()
            .into_rgba8(),
    );
    let session = Session::new(source.clone());
    for size in [128, 160, 200] {
        let cancel = CancellationToken::default();
        let result = session
            .resize(size, size, GameAssetAa::default(), &cancel)
            .unwrap();
        let prepared = session.cache.lock().unwrap().prepared.clone().unwrap();
        let fill_source = prepared
            .silhouette
            .as_ref()
            .map(|silhouette| silhouette.isolated(&prepared.linear, &cancel).unwrap())
            .unwrap_or_else(|| prepared.linear.clone());
        let base = lanczos::resize(&fill_source, size as usize, size as usize, &cancel).unwrap();
        let actual = RgbaImage::from_fn(size, size, |x, y| {
            color::rgba(base.pixels[y as usize * base.w + x as usize])
        });
        assert_eq!(
            actual,
            direct_source_lanczos_oracle(&fill_source, size, size),
            "independent direct source Lanczos oracle at {size}px"
        );

        let target = prepared
            .target_contours(&source, size, size, GameAssetAa::default(), &cancel)
            .unwrap();
        let mut unpainted = 0;
        for y in 0..size {
            for x in 0..size {
                let i = (y * size + x) as usize;
                if target.strokes.coverage.as_raw()[i] == 0 {
                    let (all_zero, alpha, support) =
                        source_alpha_footprint(&source, x, y, size, size);
                    if all_zero {
                        assert_eq!(result.get_pixel(x, y), &image::Rgba([0; 4]));
                    } else {
                        assert!(
                            f64::from(result.get_pixel(x, y)[3]) / 255.
                                <= alpha / support + 1. / 255.,
                            "partial-support alpha grew at {size}px ({x},{y})"
                        );
                    }
                    unpainted += 1;
                }
                if target.strokes.core.get_pixel(x, y)[0] != 0 {
                    assert!(target.strokes.coverage.get_pixel(x, y)[0] >= 243);
                }
            }
        }
        assert!(unpainted > size * size / 2);
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
