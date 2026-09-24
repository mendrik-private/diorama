use super::*;

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
fn outline_free_fill_uses_the_clean_fill_and_keeps_colour_modes_separate() {
    let original = outlined_fixture(true);
    let fill = RgbaImage::from_fn(original.width(), original.height(), |x, y| {
        let alpha = original.get_pixel(x, y)[3];
        image::Rgba([230, 170, 90, alpha])
    });
    let cancel = CancellationToken::default();
    let prepared = Prepared::new(&original, &cancel).unwrap();
    let target = prepared
        .target_contours(&original, 31, 29, GameAssetAa::new(30), &cancel)
        .unwrap();
    let expected = direct_source_lanczos_oracle(&color::LinearImage::from_rgba(&fill), 31, 29);
    let original_ink = resize_with_outline_free_fill(
        &original,
        &fill,
        31,
        29,
        GameAssetAa::new(30),
        OutlineColor::OriginalInk,
        &cancel,
    )
    .unwrap();
    let darkened_fill = resize_with_outline_free_fill(
        &original,
        &fill,
        31,
        29,
        GameAssetAa::new(30),
        OutlineColor::DarkenedFill { luminance: 0.25 },
        &cancel,
    )
    .unwrap();
    let clean_pixel = target
        .strokes
        .coverage
        .as_raw()
        .iter()
        .enumerate()
        .find_map(|(i, &coverage)| {
            (coverage == 0 && expected.as_raw()[i * 4 + 3] == 255).then_some(i)
        })
        .expect("fixture must contain an opaque unpainted fill pixel");
    assert_eq!(
        &original_ink.as_raw()[clean_pixel * 4..clean_pixel * 4 + 4],
        &expected.as_raw()[clean_pixel * 4..clean_pixel * 4 + 4],
        "clean regions must come solely from the outline-free fill"
    );
    let core_pixel = target
        .strokes
        .core
        .as_raw()
        .iter()
        .position(|&value| value == 255)
        .expect("fixture must retain a contour core");
    assert!(
        darkened_fill.as_raw()[core_pixel * 4] > original_ink.as_raw()[core_pixel * 4],
        "the fill-derived contour must not accidentally reuse original dark ink"
    );
    assert!(matches!(
        resize_with_outline_free_fill(
            &original,
            &RgbaImage::new(1, 1),
            31,
            29,
            GameAssetAa::new(30),
            OutlineColor::OriginalInk,
            &cancel,
        ),
        Err(Error::InvalidDimensions)
    ));
}

#[test]
fn foreground_opacity_preserves_fill_at_zero_and_legacy_ink_at_full() {
    let original = outlined_fixture(false);
    let foreground = RgbaImage::from_fn(original.width(), original.height(), |x, y| {
        let mut pixel = *original.get_pixel(x, y);
        if pixel[3] != 0 {
            pixel[3] = 160;
        }
        pixel
    });
    let cancel = CancellationToken::default();
    let prepared = Prepared::new(&original, &cancel).unwrap();
    let target = (31, 29);
    let aa = GameAssetAa::new(100);
    let fill = color::LinearImage::from_rgba(&foreground);
    let silhouette = silhouette::Silhouette::detect(&foreground, &cancel).unwrap();
    let contours = prepared
        .target_contours_with_ink_source(
            &original,
            target.0,
            target.1,
            aa,
            InkSource {
                linear: &fill,
                suppress_unsupported: true,
                brightness: None,
            },
            &cancel,
        )
        .unwrap();
    let expected_fill = prepared
        .fill_for_target(
            &fill,
            silhouette.as_ref(),
            &contours.strokes,
            FillTarget {
                target,
                aa,
                foreground_support: true,
            },
            &cancel,
        )
        .unwrap()
        .linear;
    let expected_fill = RgbaImage::from_fn(target.0, target.1, |x, y| {
        color::rgba(expected_fill.pixels[y as usize * target.0 as usize + x as usize])
    });
    let zero = prepared
        .resize_with_foreground_opacity(&original, &foreground, target.0, target.1, aa, 0., &cancel)
        .unwrap();
    let half = prepared
        .resize_with_foreground_opacity(
            &original,
            &foreground,
            target.0,
            target.1,
            aa,
            0.5,
            &cancel,
        )
        .unwrap();
    let full = prepared
        .resize_with_foreground_opacity(&original, &foreground, target.0, target.1, aa, 1., &cancel)
        .unwrap();
    let legacy = prepared
        .resize_with_foreground_ink(
            &original,
            &foreground,
            ForegroundInkResize {
                w: target.0,
                h: target.1,
                aa,
                brightness: 1.,
            },
            &cancel,
        )
        .unwrap();
    assert_eq!(zero, expected_fill);
    assert_eq!(full, legacy);
    assert!(zero.pixels().zip(full.pixels()).any(|(a, b)| a != b));
    assert!(
        zero.pixels()
            .zip(half.pixels())
            .zip(full.pixels())
            .any(|((a, b), c)| { a != b && b != c && (1..255).contains(&a[3]) })
    );
}

#[test]
fn silhouette_hides_flat_canvas_and_transparent_hidden_rgb() {
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
    for (i, &support) in coverage.iter().enumerate() {
        if support < 0.5 && target.strokes.coverage.as_raw()[i] == 0 {
            assert_eq!(&current.as_raw()[i * 4..i * 4 + 4], &[0; 4], "pixel {i}");
        }
    }
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
                    if support == 0.
                        && contours.strokes.coverage.as_raw()[i] == 0
                        && (w, h) != source.dimensions()
                    {
                        assert_eq!(
                            output.get_pixel((i % w as usize) as u32, (i / w as usize) as u32),
                            &image::Rgba([0; 4])
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
                    assert!(target.strokes.coverage.get_pixel(x, y)[0] > 0);
                }
            }
        }
        assert!(unpainted > size * size / 2);
    }
}
#[test]
fn rectangle_previews_reuse_source_analysis() {
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
fn aa_changes_cache_identity_without_reanalyzing_source() {
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
            Err(Error::InvalidDimensions)
        ));
    }
    let cancel = CancellationToken::default();
    cancel.cancel();
    for (w, h) in [(8, 6), (4, 3)] {
        assert!(matches!(
            session.resize(w, h, GameAssetAa::default(), &cancel),
            Err(Error::Cancelled)
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
fn aa_zero_foreground_ink_keeps_owned_core_black_and_uses_exact_darkened_donor_colours() {
    let original = RgbaImage::from_fn(64, 64, |x, y| {
        if (12..52).contains(&x) && (12..52).contains(&y) {
            if y == 31 && (21..43).contains(&x) {
                image::Rgba([0, 0, 0, 255])
            } else {
                image::Rgba([150, 110, 80, 255])
            }
        } else {
            image::Rgba([245, 240, 230, 255])
        }
    });
    let foreground = RgbaImage::from_fn(64, 64, |x, y| {
        if (12..52).contains(&x) && (12..52).contains(&y) {
            image::Rgba([200, 150, 100, 255])
        } else {
            image::Rgba([0, 0, 0, 0])
        }
    });
    let cancel = CancellationToken::default();
    let prepared = Prepared::new(&original, &cancel).unwrap();
    let foreground_linear = color::LinearImage::from_rgba(&foreground);
    let request = ForegroundInkResize {
        w: 32,
        h: 32,
        aa: GameAssetAa::new(0),
        brightness: 0.,
    };
    let black_target = prepared
        .target_contours_with_ink_source(
            &original,
            request.w,
            request.h,
            request.aa,
            InkSource {
                linear: &foreground_linear,
                suppress_unsupported: true,
                brightness: Some(request.brightness),
            },
            &cancel,
        )
        .unwrap();
    let black = prepared
        .resize_with_foreground_ink(&original, &foreground, request, &cancel)
        .unwrap();
    let owned_core: Vec<_> = black_target
        .strokes
        .core
        .as_raw()
        .iter()
        .zip(&black_target.strokes.owners)
        .enumerate()
        .filter_map(|(i, (&core, owner))| (core != 0 && owner.is_some()).then_some(i))
        .collect();
    assert!(
        !owned_core.is_empty(),
        "fixture retains a narrow contour core"
    );
    let owner = black_target.strokes.owners[owned_core[0]].unwrap();
    let intrinsic = opacity::calculate(
        &prepared.widths,
        &black_target.strokes.core,
        &black_target.strokes.owners,
    )[owner];
    let strengths: Vec<_> = [0, 1, 50, 99, 100]
        .into_iter()
        .map(|aa| {
            prepared.foreground_ink_strengths(&black_target.strokes, GameAssetAa::new(aa))[owner]
        })
        .collect();
    assert_eq!(strengths[0], 1.);
    assert_eq!(strengths[4], intrinsic);
    assert!(strengths.windows(2).all(|pair| pair[0] >= pair[1]));
    assert!(strengths[1] < 1. && strengths[3] > intrinsic);
    for i in &owned_core {
        assert_eq!(
            &black.as_raw()[i * 4..i * 4 + 4],
            &[0, 0, 0, 255],
            "AA0 owned core {i} must be solid black"
        );
    }

    let dark_target = prepared
        .target_contours_with_ink_source(
            &original,
            32,
            32,
            GameAssetAa::new(0),
            InkSource {
                linear: &foreground_linear,
                suppress_unsupported: true,
                brightness: Some(0.8),
            },
            &cancel,
        )
        .unwrap();
    let dark = prepared
        .resize_with_foreground_ink(
            &original,
            &foreground,
            ForegroundInkResize {
                w: 32,
                h: 32,
                aa: GameAssetAa::new(0),
                brightness: 0.8,
            },
            &cancel,
        )
        .unwrap();
    let clean = dark_target
        .strokes
        .coverage
        .as_raw()
        .iter()
        .enumerate()
        .find_map(|(i, &coverage)| (coverage == 0 && black.as_raw()[i * 4 + 3] == 255).then_some(i))
        .expect("fixture retains an opaque unpainted interior");
    assert_eq!(
        &black.as_raw()[clean * 4..clean * 4 + 4],
        &dark.as_raw()[clean * 4..clean * 4 + 4],
        "foreground ink brightness leaves the finished fill unchanged"
    );
    for i in owned_core {
        let expected = color::rgba([
            dark_target.colors[i][0],
            dark_target.colors[i][1],
            dark_target.colors[i][2],
            1.,
        ]);
        assert_eq!(
            &dark.as_raw()[i * 4..i * 4 + 4],
            &expected.0,
            "AA0 owned core {i} must use its exact darkened foreground donor"
        );
    }
}

#[test]
fn canonical_cleanup_drops_short_components_without_aa_ghosts() {
    let cancel = CancellationToken::default();
    for aa in [0, 100] {
        let mut strokes = strokes::Strokes {
            core: image::GrayImage::new(8, 4),
            coverage: image::GrayImage::new(8, 4),
            owners: vec![None; 32],
        };
        // Two core pixels plus a formerly antialiased neighbor. The target
        // component is below the three-pixel cutoff and must leave no paint at
        // either AA endpoint.
        for (i, coverage) in [(8 + 1, 255), (8 + 2, 255), (8 + 3, 96)] {
            if coverage == 255 {
                strokes.core.as_mut()[i] = 255;
            }
            strokes.coverage.as_mut()[i] = coverage;
            strokes.owners[i] = Some(0);
        }
        let mut colors = vec![[0.2, 0.1, 0.05]; 32];
        Prepared::canonicalize_foreground_strokes(
            &mut strokes,
            &mut colors,
            &[false; 32],
            GameAssetAa::new(aa),
            &cancel,
        )
        .unwrap();
        for i in [8 + 1, 8 + 2, 8 + 3] {
            assert_eq!(strokes.core.as_raw()[i], 0, "AA{aa} core {i}");
            assert_eq!(strokes.coverage.as_raw()[i], 0, "AA{aa} coverage {i}");
            assert_eq!(strokes.owners[i], None, "AA{aa} owner {i}");
        }
    }
}

struct ForegroundInkBeforeHaloCleanup {
    image: RgbaImage,
    support: Vec<bool>,
    fill: color::LinearImage,
    core: image::GrayImage,
    coverage: image::GrayImage,
    owners: Vec<Option<usize>>,
    colors: Vec<[f64; 3]>,
}

/// Compose the canonical foreground-ink path immediately before halo cleanup.
/// This is an integration-test baseline, not a second halo implementation.
fn foreground_ink_before_halo_cleanup(
    prepared: &Prepared,
    original: &RgbaImage,
    foreground: &RgbaImage,
    request: &ForegroundInkResize,
    cancel: &dyn Cancellation,
) -> ForegroundInkBeforeHaloCleanup {
    let fill_linear = color::LinearImage::from_rgba(foreground);
    let fill_silhouette = silhouette::Silhouette::detect(foreground, cancel)
        .unwrap()
        .expect("the fixture has a foreground silhouette");
    let TargetContours {
        mut strokes,
        mut colors,
    } = prepared
        .target_contours_with_ink_source(
            original,
            request.w,
            request.h,
            request.aa,
            InkSource {
                linear: &fill_linear,
                suppress_unsupported: true,
                brightness: Some(request.brightness),
            },
            cancel,
        )
        .unwrap();
    let FinishedFill {
        linear: fill,
        foreground_support,
    } = prepared
        .fill_for_target(
            &fill_linear,
            Some(&fill_silhouette),
            &strokes,
            FillTarget {
                target: (request.w, request.h),
                aa: request.aa,
                foreground_support: true,
            },
            cancel,
        )
        .unwrap();
    let support = foreground_support.expect("foreground support was requested");
    Prepared::canonicalize_foreground_strokes(
        &mut strokes,
        &mut colors,
        &support,
        request.aa,
        cancel,
    )
    .unwrap();
    let strength = prepared.foreground_ink_strengths(&strokes, request.aa);
    let paint = opacity::apply(&strokes.coverage, &strokes.owners, &strength);
    let baseline = paint::composite(&fill, &colors, &paint);
    ForegroundInkBeforeHaloCleanup {
        image: baseline,
        support,
        fill,
        core: strokes.core,
        coverage: strokes.coverage,
        owners: strokes.owners,
        colors,
    }
}

fn halo_foreground_fixture(semitransparent_original: bool) -> (RgbaImage, RgbaImage) {
    let mut original = RgbaImage::from_fn(64, 64, |x, y| {
        let inside = (12..52).contains(&x) && (12..52).contains(&y);
        let ink = y == 31 && (21..43).contains(&x);
        image::Rgba(if ink {
            [0, 0, 0, 255]
        } else if inside {
            [150, 110, 80, 255]
        } else {
            [220, 230, 245, 255]
        })
    });
    if semitransparent_original {
        original.put_pixel(0, 0, image::Rgba([220, 230, 245, 254]));
    }
    let foreground = RgbaImage::from_fn(64, 64, |x, y| {
        let inside = (12..52).contains(&x) && (12..52).contains(&y);
        let ink = y == 31 && (21..43).contains(&x);
        let fringe = (20..44).contains(&x) && (27..31).contains(&y);
        image::Rgba(if ink {
            [0, 0, 0, 255]
        } else if fringe {
            // A residual from an extracted foreground remains source-supported
            // but falls below the compositor's 0.5 support threshold after
            // resampling, producing a low-alpha exterior next to original ink.
            [255, 255, 255, 20]
        } else if inside {
            [200, 150, 100, 255]
        } else {
            [255, 0, 255, 0]
        })
    });
    (original, foreground)
}

fn assert_foreground_ink_halo_cleanup(aa: u8) {
    let cancel = CancellationToken::default();
    let request = ForegroundInkResize {
        w: 31,
        h: 31,
        aa: GameAssetAa::new(aa),
        brightness: 1.,
    };
    let (w, h) = (request.w, request.h);
    let (original, foreground) = halo_foreground_fixture(false);
    let prepared = Prepared::new(&original, &cancel).unwrap();
    let before =
        foreground_ink_before_halo_cleanup(&prepared, &original, &foreground, &request, &cancel);
    let after = prepared
        .resize_with_foreground_ink(&original, &foreground, request, &cancel)
        .unwrap();
    let fringe = (0..before.support.len())
        .find(|&i| {
            !before.support[i]
                && (0. < before.fill.pixels[i][3] && before.fill.pixels[i][3] < 0.25)
                && before.core.as_raw()[i] == 0
                && before.owners[i].is_none()
                && before.image.as_raw()[i * 4] > 150
                && (-2isize..=2).any(|dy| {
                    (-2isize..=2).any(|dx| {
                        let xx = i % w as usize;
                        let yy = i / w as usize;
                        let xx = xx as isize + dx;
                        let yy = yy as isize + dy;
                        xx >= 0
                            && yy >= 0
                            && xx < w as isize
                            && yy < h as isize
                            && before.core.as_raw()[yy as usize * w as usize + xx as usize] != 0
                            && before.owners[yy as usize * w as usize + xx as usize].is_some()
                    })
                })
        })
        .expect("fixture must produce a bright low-alpha exterior fringe");
    let x = (fringe % w as usize) as u32;
    let y = (fringe / w as usize) as u32;
    let mut donor = None;
    for dy in -2isize..=2 {
        for dx in -2isize..=2 {
            if dx == 0 && dy == 0 {
                continue;
            }
            let xx = x as isize + dx;
            let yy = y as isize + dy;
            if xx < 0 || yy < 0 || xx >= w as isize || yy >= h as isize {
                continue;
            }
            let i = yy as usize * w as usize + xx as usize;
            let Some(owner) = before.owners[i] else {
                continue;
            };
            if before.core.as_raw()[i] == 0 {
                continue;
            }
            let rank = (
                dx * dx + dy * dy,
                std::cmp::Reverse(before.coverage.as_raw()[i]),
                owner,
                i,
            );
            if donor.is_none_or(|(best, _)| rank < best) {
                donor = Some((rank, i));
            }
        }
    }
    let donor = donor
        .map(|(_, i)| i)
        .expect("the fringe must be within two pixels of an owned core donor");
    let expected = color::rgba([
        before.colors[donor][0],
        before.colors[donor][1],
        before.colors[donor][2],
        1.,
    ]);
    assert_eq!(
        &after.as_raw()[fringe * 4..fringe * 4 + 3],
        &expected.0[..3],
        "opaque originals replace exterior fringe RGB with the core donor"
    );
    assert_eq!(
        after.as_raw()[fringe * 4 + 3],
        before.image.as_raw()[fringe * 4 + 3],
        "cleanup retains the baseline alpha byte"
    );
    assert_ne!(
        &after.as_raw()[fringe * 4..fringe * 4 + 3],
        &before.image.as_raw()[fringe * 4..fringe * 4 + 3],
        "the fixture's bright fill fringe is actually repaired"
    );
    for (i, &core) in before.core.as_raw().iter().enumerate() {
        if core != 0 {
            assert_eq!(
                &after.as_raw()[i * 4..i * 4 + 4],
                &before.image.as_raw()[i * 4..i * 4 + 4],
                "core {i} stays frozen"
            );
        }
        if before.support[i] && before.coverage.as_raw()[i] == 0 {
            assert_eq!(
                &after.as_raw()[i * 4..i * 4 + 4],
                &before.image.as_raw()[i * 4..i * 4 + 4],
                "supported interior {i} stays unchanged"
            );
        }
    }

    let (transparent_original, transparent_foreground) = halo_foreground_fixture(true);
    let transparent_prepared = Prepared::new(&transparent_original, &cancel).unwrap();
    let transparent_before = foreground_ink_before_halo_cleanup(
        &transparent_prepared,
        &transparent_original,
        &transparent_foreground,
        &ForegroundInkResize {
            w,
            h,
            aa: GameAssetAa::new(aa),
            brightness: 1.,
        },
        &cancel,
    );
    let transparent_after = transparent_prepared
        .resize_with_foreground_ink(
            &transparent_original,
            &transparent_foreground,
            ForegroundInkResize {
                w,
                h,
                aa: GameAssetAa::new(aa),
                brightness: 1.,
            },
            &cancel,
        )
        .unwrap();
    assert_eq!(transparent_after, transparent_before.image);
}

#[test]
fn foreground_ink_cleans_only_opaque_originals_low_alpha_exterior_fringe() {
    for aa in [0, 100] {
        assert_foreground_ink_halo_cleanup(aa);
    }
}

#[test]
fn cancelled_cached_requests_and_working_set_preflight_are_rejected() {
    let session = Session::new(Arc::new(RgbaImage::new(12, 8)));
    session
        .resize(6, 4, GameAssetAa::default(), &CancellationToken::default())
        .unwrap();
    let cancel = CancellationToken::default();
    cancel.cancel();
    assert!(matches!(
        session.resize(6, 4, GameAssetAa::default(), &cancel),
        Err(Error::Cancelled)
    ));
    assert_eq!(working_set_estimate(2560, 1440, 1280, 720), 2_034_892_800);
    assert!(check_working_set_budget(2560, 1440, 1280, 720, DEFAULT_MEMORY_LIMIT).is_ok());
    let large = Session::new(Arc::new(RgbaImage::new(4096, 4096)));
    let error = large
        .resize(
            128,
            128,
            GameAssetAa::default(),
            &CancellationToken::default(),
        )
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Game Asset scaling would exceed the configured 4294967296 byte working-memory limit"
    );
    assert!(matches!(
        error,
        Error::GameAssetMemoryLimit {
            limit_bytes: DEFAULT_MEMORY_LIMIT
        }
    ));
    assert!(large.cache.lock().unwrap().prepared.is_none());
}
