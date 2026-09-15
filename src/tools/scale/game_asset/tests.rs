use super::*;
use crate::document::{Document, ImageSource, Metadata, Operation, Resampling};
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
        // The new images are reviewed visual snapshots, NOT independent math
        // oracles. Retain the original Python-produced images to require exact
        // background parity away from the deliberately changed contour band.
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
        for y in 0..size {
            for x in 0..size {
                let near_core =
                    (-2..=2).any(|dy| (-2..=2).any(|dx| core.at(x as isize + dx, y as isize + dy)));
                if !near_core {
                    assert_eq!(
                        result.get_pixel(x, y),
                        legacy.get_pixel(x, y),
                        "background changed at {size}px ({x},{y})"
                    );
                    checked += 1;
                }
                if core.at(x as isize, y as isize) {
                    assert!(target.strokes.coverage.get_pixel(x, y)[0] >= 243);
                }
            }
        }
        assert!(checked > size * size / 2);
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
