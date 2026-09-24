use asset_scaler::{
    CancellationToken, Error, GameAssetAa, ResizeOptions, Session, resize,
    resize_with_memory_limit, resize_with_options,
};
use image::{Rgba, RgbaImage};
use std::sync::Arc;

#[test]
fn cached_and_single_shot_callers_share_pixels_and_limits() {
    let image = Arc::new(RgbaImage::from_fn(32, 24, |x, y| {
        Rgba([
            (x * 7) as u8,
            (y * 9) as u8,
            90,
            if x < 8 { 0 } else { 255 },
        ])
    }));
    let token = CancellationToken::default();
    let session = Session::new(image.clone());
    for aa in [0, 50, 100, 0] {
        let aa = GameAssetAa::new(aa);
        assert_eq!(
            resize(&image, 12, 9, aa, &token).unwrap(),
            session.resize(12, 9, aa, &token).unwrap()
        );
    }
    assert!(matches!(
        resize_with_memory_limit(&image, 12, 9, GameAssetAa::default(), &token, 1),
        Err(Error::GameAssetMemoryLimit { limit_bytes: 1 })
    ));
    assert!(matches!(
        Session::with_memory_limit(image.clone(), 1).resize(12, 9, GameAssetAa::default(), &token),
        Err(Error::GameAssetMemoryLimit { limit_bytes: 1 })
    ));
    token.cancel();
    assert!(matches!(
        session.resize(12, 9, GameAssetAa::default(), &token),
        Err(Error::Cancelled)
    ));
    assert!(matches!(
        resize(&image, 32, 24, GameAssetAa::default(), &|| true),
        Err(Error::Cancelled)
    ));
}

#[test]
fn cancellation_during_analysis_never_publishes_a_partial_output() {
    let image = RgbaImage::from_pixel(32, 32, Rgba([0, 0, 0, 255]));
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let cancelled = || calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 3;
    assert!(matches!(
        resize(&image, 16, 16, GameAssetAa::default(), &cancelled),
        Err(Error::Cancelled)
    ));
}

#[test]
fn opaque_background_removal_can_be_disabled_for_cached_and_single_shot_resize() {
    let source = Arc::new(RgbaImage::from_fn(64, 64, |x, y| {
        if (20..44).contains(&x) && (20..44).contains(&y) {
            Rgba([120, 180, 90, 255])
        } else {
            Rgba([240, 230, 220, 255])
        }
    }));
    let transparent = Arc::new(RgbaImage::from_fn(64, 64, |x, y| {
        if (20..44).contains(&x) && (20..44).contains(&y) {
            Rgba([120, 180, 90, 255])
        } else {
            Rgba([240, 0, 220, 0])
        }
    }));
    let cancel = CancellationToken::default();
    let aa = GameAssetAa::new(20);
    let preserve = ResizeOptions::preserve_opaque_background();

    let one_shot = resize_with_options(&source, 16, 16, aa, &cancel, preserve).unwrap();
    let session = Session::with_options(source.clone(), preserve);
    let cached = session.resize(16, 16, aa, &cancel).unwrap();
    assert_eq!(cached, one_shot);
    assert_eq!(session.resize(16, 16, aa, &cancel).unwrap(), cached);
    assert_eq!(one_shot.get_pixel(0, 0), &Rgba([240, 230, 220, 255]));
    assert_eq!(
        one_shot.get_pixel(8, 8)[3],
        255,
        "opaque foreground survived"
    );

    assert_eq!(
        resize(&source, 16, 16, aa, &cancel)
            .unwrap()
            .get_pixel(0, 0),
        &Rgba([0; 4]),
        "the default retains automatic opaque-background removal"
    );
    assert_eq!(
        Session::new(source.clone())
            .resize(16, 16, aa, &cancel)
            .unwrap()
            .get_pixel(0, 0),
        &Rgba([0; 4]),
    );

    let transparent_one_shot =
        resize_with_options(&transparent, 16, 16, aa, &cancel, preserve).unwrap();
    let transparent_cached = Session::with_options(transparent.clone(), preserve)
        .resize(16, 16, aa, &cancel)
        .unwrap();
    assert_eq!(transparent_cached, transparent_one_shot);
    assert_eq!(transparent_one_shot.get_pixel(0, 0)[3], 0);
    assert_eq!(transparent_one_shot.get_pixel(8, 8)[3], 255);

    assert_eq!(
        resize_with_options(&source, 64, 64, aa, &cancel, preserve).unwrap(),
        *source
    );
    assert_eq!(
        session.resize(64, 64, aa, &cancel).unwrap(),
        *source,
        "identity requests bypass both analysis and cached target state"
    );
}

#[test]
fn prepared_original_contours_paint_each_fresh_removed_foreground() {
    let original = Arc::new(RgbaImage::from_fn(64, 64, |x, y| {
        if (16..48).contains(&x) && (16..48).contains(&y) {
            if (30..34).contains(&y) {
                Rgba([8, 12, 16, 255])
            } else {
                Rgba([120, 90, 70, 255])
            }
        } else {
            Rgba([240, 240, 240, 255])
        }
    }));
    let foreground = |colour: [u8; 3]| {
        RgbaImage::from_fn(64, 64, |x, y| {
            if (16..48).contains(&x) && (16..48).contains(&y) {
                Rgba([colour[0], colour[1], colour[2], 255])
            } else {
                Rgba([0; 4])
            }
        })
    };
    let session = Session::new(original);
    let cancel = CancellationToken::default();
    session.prepare(&cancel).unwrap();
    let red = foreground([220, 60, 30]);
    let green = foreground([40, 180, 80]);
    for aa in [0, 20, 50, 100] {
        let normal = session
            .resize(32, 32, GameAssetAa::new(aa), &cancel)
            .unwrap();
        let first = session
            .resize_with_foreground(&red, 32, 32, GameAssetAa::new(aa), &cancel)
            .unwrap();
        let second = session
            .resize_with_foreground(&green, 32, 32, GameAssetAa::new(aa), &cancel)
            .unwrap();
        assert!(
            first
                .pixels()
                .zip(second.pixels())
                .any(|(a, b)| a[3] > 0 && b[3] > 0 && a.0 != b.0),
            "AA {aa}: stale foreground result was reused"
        );
        assert_ne!(
            first, normal,
            "AA {aa}: normal cached result was reused for a foreground"
        );
        let foreground_only = resize(&red, 32, 32, GameAssetAa::new(aa), &cancel).unwrap();
        assert_ne!(
            first, foreground_only,
            "AA {aa}: original contour analysis was not used"
        );
        assert_eq!(*first.get_pixel(0, 0), Rgba([0; 4]));
        assert_eq!(*second.get_pixel(31, 31), Rgba([0; 4]));
    }
    assert!(matches!(
        session.resize_with_foreground(
            &RgbaImage::new(1, 1),
            32,
            32,
            GameAssetAa::new(20),
            &cancel,
        ),
        Err(Error::InvalidDimensions)
    ));
}

#[test]
fn foreground_prepare_checks_cancellation_and_its_extra_memory() {
    let source = Arc::new(RgbaImage::from_pixel(32, 32, Rgba([90, 70, 50, 255])));
    let foreground = RgbaImage::from_pixel(32, 32, Rgba([90, 70, 50, 255]));
    let cancelled = CancellationToken::default();
    cancelled.cancel();
    assert!(matches!(
        Session::new(source.clone()).prepare(&cancelled),
        Err(Error::Cancelled)
    ));

    let session = Session::with_memory_limit(source, 600_000);
    let cancel = CancellationToken::default();
    session.prepare(&cancel).unwrap();
    cancel.cancel();
    assert!(matches!(
        session.resize_with_foreground(&foreground, 16, 16, GameAssetAa::new(20), &cancel),
        Err(Error::Cancelled)
    ));
    let cancel = CancellationToken::default();
    assert!(matches!(
        session.resize_with_foreground(&foreground, 16, 16, GameAssetAa::new(20), &cancel),
        Err(Error::GameAssetMemoryLimit {
            limit_bytes: 600_000
        })
    ));
    assert!(matches!(
        session
            .resize_with_foreground_ink(&foreground, 16, 16, GameAssetAa::new(20), 0.8, &cancel,),
        Err(Error::GameAssetMemoryLimit {
            limit_bytes: 600_000
        })
    ));
    assert!(matches!(
        Session::new(Arc::new(RgbaImage::new(0, 0))).prepare(&CancellationToken::default()),
        Err(Error::InvalidDimensions)
    ));
}

#[test]
fn foreground_ink_validates_inputs_and_preserves_identity() {
    let source = Arc::new(RgbaImage::from_pixel(32, 32, Rgba([90, 70, 50, 255])));
    let foreground = RgbaImage::from_pixel(32, 32, Rgba([40, 120, 200, 255]));
    let session = Session::new(source);
    let cancel = CancellationToken::default();

    assert_eq!(
        session
            .resize_with_foreground_ink(&foreground, 32, 32, GameAssetAa::new(20), 0.8, &cancel)
            .unwrap(),
        foreground,
        "an identity request must not alter foreground pixels"
    );
    for brightness in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
        assert!(matches!(
            session.resize_with_foreground_ink(
                &foreground,
                16,
                16,
                GameAssetAa::new(20),
                brightness,
                &cancel,
            ),
            Err(Error::Scaling(_))
        ));
    }
    assert!(matches!(
        session.resize_with_foreground_ink(
            &RgbaImage::new(1, 1),
            16,
            16,
            GameAssetAa::new(20),
            0.8,
            &cancel,
        ),
        Err(Error::InvalidDimensions)
    ));
    let cancelled = CancellationToken::default();
    cancelled.cancel();
    assert!(matches!(
        session.resize_with_foreground_ink(
            &foreground,
            16,
            16,
            GameAssetAa::new(20),
            0.8,
            &cancelled,
        ),
        Err(Error::Cancelled)
    ));
}

#[test]
fn foreground_opacity_validates_inputs_and_cancellation() {
    let source = Arc::new(RgbaImage::from_pixel(32, 32, Rgba([90, 70, 50, 255])));
    let foreground = RgbaImage::from_pixel(32, 32, Rgba([40, 120, 200, 255]));
    let session = Session::new(source);
    let cancel = CancellationToken::default();
    assert_eq!(
        session
            .resize_with_foreground_opacity(&foreground, 32, 32, GameAssetAa::new(20), 0.5, &cancel)
            .unwrap(),
        foreground,
    );
    for opacity in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
        assert!(matches!(
            session.resize_with_foreground_opacity(
                &foreground,
                16,
                16,
                GameAssetAa::new(20),
                opacity,
                &cancel,
            ),
            Err(Error::Scaling(_))
        ));
    }
    assert!(matches!(
        session.resize_with_foreground_opacity(
            &RgbaImage::new(1, 1),
            16,
            16,
            GameAssetAa::new(20),
            0.5,
            &cancel,
        ),
        Err(Error::InvalidDimensions)
    ));
    cancel.cancel();
    assert!(matches!(
        session.resize_with_foreground_opacity(
            &foreground,
            16,
            16,
            GameAssetAa::new(20),
            0.5,
            &cancel
        ),
        Err(Error::Cancelled)
    ));
}

#[test]
fn foreground_ink_uses_only_visible_donors_and_darkens_only_contours() {
    let source = Arc::new(RgbaImage::from_fn(64, 64, |x, y| {
        if (12..52).contains(&x) && (12..52).contains(&y) {
            if (30..34).contains(&y) {
                Rgba([8, 12, 16, 255])
            } else {
                Rgba([120, 90, 70, 255])
            }
        } else {
            Rgba([245, 240, 230, 255])
        }
    }));
    let foreground = RgbaImage::from_fn(64, 64, |x, y| {
        if (12..52).contains(&x) && (12..52).contains(&y) {
            // A pale donor is still valid foreground ink; it must not be
            // replaced by the white opaque background of the original.
            Rgba([220, 190, 160, 255])
        } else {
            Rgba([0; 4])
        }
    });
    let session = Session::new(source.clone());
    let cancel = CancellationToken::default();
    let full_transparent = RgbaImage::new(64, 64);
    let empty = session
        .resize_with_foreground_ink(
            &full_transparent,
            32,
            32,
            GameAssetAa::new(20),
            0.8,
            &cancel,
        )
        .unwrap();
    assert!(empty.pixels().all(|pixel| *pixel == Rgba([0; 4])));

    let bright = session
        .resize_with_foreground_ink(&foreground, 32, 32, GameAssetAa::new(20), 1.0, &cancel)
        .unwrap();
    let dark = session
        .resize_with_foreground_ink(&foreground, 32, 32, GameAssetAa::new(20), 0.8, &cancel)
        .unwrap();
    assert!(
        bright
            .pixels()
            .zip(dark.pixels())
            .all(|(before, after)| before[3] == after[3]),
        "ink brightness must not change alpha"
    );
    assert!(
        bright
            .pixels()
            .zip(dark.pixels())
            .any(|(before, after)| before[3] > 0 && before.0 != after.0),
        "20% sRGB darkening must affect a rendered contour"
    );
    assert!(
        bright
            .pixels()
            .zip(dark.pixels())
            .any(|(before, after)| before[3] == 255 && before.0 == after.0),
        "the foreground fill must remain unmodified"
    );
    assert!(
        dark.pixels()
            .filter(|pixel| pixel[3] > 0)
            .all(|pixel| { !(pixel[0] > 235 && pixel[1] > 230 && pixel[2] > 220) }),
        "transparent foreground regions may not restore original white background ink"
    );
}

#[test]
fn foreground_ink_is_opt_in_and_cannot_change_legacy_foreground_output() {
    let source = Arc::new(RgbaImage::from_fn(64, 64, |x, y| {
        if (12..52).contains(&x) && (12..52).contains(&y) {
            if y == 32 || x == 13 {
                Rgba([8, 12, 16, 255])
            } else {
                Rgba([130, 95, 70, 255])
            }
        } else {
            Rgba([245, 240, 230, 255])
        }
    }));
    let foreground = RgbaImage::from_fn(64, 64, |x, y| {
        if (12..52).contains(&x) && (12..52).contains(&y) {
            Rgba([180, 100, 60, 255])
        } else {
            Rgba([7, 9, 11, 0])
        }
    });
    let session = Session::new(source);
    let cancel = CancellationToken::default();
    let before = session
        .resize_with_foreground(&foreground, 32, 32, GameAssetAa::new(20), &cancel)
        .unwrap();
    let ink = session
        .resize_with_foreground_ink(&foreground, 32, 32, GameAssetAa::new(20), 0.8, &cancel)
        .unwrap();
    let after = session
        .resize_with_foreground(&foreground, 32, 32, GameAssetAa::new(20), &cancel)
        .unwrap();
    assert_eq!(after, before, "the foreground-ink path is opt-in");
    assert_ne!(ink, before, "foreground ink is routed separately");
}

#[test]
fn foreground_ink_coalesces_adjacent_target_runs_at_every_scale() {
    let source = Arc::new(RgbaImage::from_fn(96, 96, |x, y| {
        if (12..84).contains(&x) && (12..84).contains(&y) {
            if (42..=45).contains(&y) && (24..72).contains(&x) && (y == 42 || y == 45) {
                Rgba([0, 0, 0, 255])
            } else {
                Rgba([140, 90, 60, 255])
            }
        } else {
            Rgba([240, 235, 220, 255])
        }
    }));
    let foreground = RgbaImage::from_fn(96, 96, |x, y| {
        if (12..84).contains(&x) && (12..84).contains(&y) {
            Rgba([190, 130, 80, 255])
        } else {
            Rgba([0; 4])
        }
    });
    let session = Session::new(source);
    let cancel = CancellationToken::default();
    for size in [48, 32, 24] {
        let output = session
            .resize_with_foreground_ink(&foreground, size, size, GameAssetAa::new(0), 0., &cancel)
            .unwrap();
        let dark = |x: u32, y: u32| {
            let pixel = output.get_pixel(x, y);
            pixel[3] > 200 && pixel[0] < 24 && pixel[1] < 24 && pixel[2] < 24
        };
        assert!(
            (0..size).any(|y| (0..size).filter(|&x| dark(x, y)).count() >= size as usize / 4),
            "{size}px fixture must retain the combined contour"
        );
        for y in 0..size - 1 {
            for x in 0..size - 1 {
                assert!(
                    !(dark(x, y) && dark(x + 1, y) && dark(x, y + 1) && dark(x + 1, y + 1)),
                    "{size}px adjacent contours must collapse before AA0 paint"
                );
            }
        }
    }
}
