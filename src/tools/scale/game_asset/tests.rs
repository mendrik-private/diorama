use super::*;
use crate::document::{Document, ImageSource, Metadata, Operation, Resampling};
#[test]
fn selected_recipe_matches_reviewed_pixels_at_all_sizes() {
    let source = Arc::new(
        image::load_from_memory(include_bytes!("fixtures/elf.png"))
            .unwrap()
            .into_rgba8(),
    );
    let session = Session::new(source);
    for (size, bytes) in [
        (128, include_bytes!("fixtures/elf-128.png").as_slice()),
        (160, include_bytes!("fixtures/elf-160.png").as_slice()),
        (256, include_bytes!("fixtures/elf-256.png").as_slice()),
    ] {
        let expected = image::load_from_memory(bytes).unwrap().into_rgba8();
        let result = session
            .resize(size, size, &CancellationToken::default())
            .unwrap();
        assert_eq!(
            result.as_raw(),
            expected.as_raw(),
            "reviewed output changed at {size}px"
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
    let preview = session.resize(20, 12, &cancel).unwrap();
    let prepared = session.cache.lock().unwrap().prepared.clone().unwrap();
    assert_eq!(
        preview,
        super::super::resize(&image, 20, 12, Resampling::GameAsset, &cancel).unwrap()
    );
    let mut document = Document::new(ImageSource {
        pixels: image,
        path: None,
        metadata: Metadata::default(),
    });
    document.apply(Operation::Scale {
        width: 20,
        height: 12,
        resampling: Resampling::GameAsset,
    });
    assert_eq!(preview, document.render(&cancel).unwrap().pixels);
    assert_eq!(session.resize(20, 12, &cancel).unwrap(), preview);
    assert_eq!(
        session.resize(1, 10, &cancel).unwrap().dimensions(),
        (1, 10)
    );
    assert!(Arc::ptr_eq(
        &prepared,
        session.cache.lock().unwrap().prepared.as_ref().unwrap()
    ));
}
#[test]
fn validates_dimensions_and_cancellation_without_populating_cache() {
    let session = Session::new(Arc::new(RgbaImage::new(8, 6)));
    for (w, h) in [(0, 3), (4, 0), (9, 3), (4, 7)] {
        assert!(matches!(
            session.resize(w, h, &CancellationToken::default()),
            Err(AppError::InvalidDimensions)
        ));
    }
    let cancel = CancellationToken::default();
    cancel.cancel();
    for (w, h) in [(8, 6), (4, 3)] {
        assert!(matches!(
            session.resize(w, h, &cancel),
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
        Session::new(Arc::new(a)).resize(4, 3, &cancel).unwrap(),
        Session::new(Arc::new(b)).resize(4, 3, &cancel).unwrap()
    );
    let empty = Session::new(Arc::new(RgbaImage::from_pixel(
        9,
        7,
        image::Rgba([255, 0, 70, 0]),
    )))
    .resize(3, 2, &cancel)
    .unwrap();
    assert!(empty.pixels().all(|p| p.0 == [0; 4]));
}

#[test]
fn retained_ink_excludes_short_neighbors_and_correction_stays_in_the_halo() {
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
    let source = RgbaImage::from_fn(24, 12, |x, y| {
        image::Rgba([(x * 10) as u8, (y * 20) as u8, 120, 255])
    });
    let linear = color::LinearImage::from_rgba(&source);
    let mut paint = image::GrayImage::new(12, 6);
    paint.put_pixel(2, 2, image::Luma([153]));
    let all_ink = raster::Mask {
        w: 24,
        h: 12,
        data: vec![true; 24 * 12],
    };
    let empty_ink = raster::Mask::new(24, 12);
    let base = median::project(&linear, &empty_ink, &paint, &cancel).unwrap();
    let no_support = median::project(&linear, &all_ink, &paint, &cancel).unwrap();
    assert_eq!(
        base.pixels, no_support.pixels,
        "no donor information preserves the original lookup"
    );
    let corrected = median::project(&linear, &retained, &paint, &cancel).unwrap();
    for y in 0..6 {
        for x in 0..12 {
            let i = y * 12 + x;
            assert_eq!(base.pixels[i][3], corrected.pixels[i][3]);
            if !(1..=3).contains(&x) || !(1..=3).contains(&y) {
                assert_eq!(base.pixels[i], corrected.pixels[i]);
            }
        }
    }
}

#[test]
fn cancelled_cached_requests_and_oversized_analysis_are_rejected() {
    let session = Session::new(Arc::new(RgbaImage::new(12, 8)));
    session.resize(6, 4, &CancellationToken::default()).unwrap();
    let cancel = CancellationToken::default();
    cancel.cancel();
    assert!(matches!(
        session.resize(6, 4, &cancel),
        Err(AppError::Cancelled)
    ));
    let large = Session::new(Arc::new(RgbaImage::new(2048, 2048)));
    assert!(matches!(
        large.resize(128, 128, &CancellationToken::default()),
        Err(AppError::MemoryLimit { .. })
    ));
    assert!(large.cache.lock().unwrap().prepared.is_none());
}
