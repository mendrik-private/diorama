use super::*;
use std::{fmt::Write as _, path::Path};

fn render(image: RgbaImage, w: u32, h: u32) -> (Reference, Arc<Output>) {
    let mut reference = Reference::new_srgb(
        Arc::new(image),
        Settings::default(),
        &CancellationToken::default(),
    )
    .unwrap();
    let output = reference
        .resize(w, h, &CancellationToken::default())
        .unwrap();
    (reference, output)
}

#[test]
fn linear_light_alpha_and_independent_direct_convolution() {
    let cancel = CancellationToken::default();
    for (pixels, expected) in [
        (vec![0, 0, 0, 255, 255, 255, 255, 255], [188, 188, 188, 255]),
        // Ascending f64 tap accumulation lands just below the half-alpha
        // rounding boundary. Do not add an epsilon or change v1's rounding.
        (vec![255, 0, 0, 0, 0, 0, 255, 255], [0, 0, 255, 127]),
    ] {
        let (_, out) = render(RgbaImage::from_raw(2, 1, pixels).unwrap(), 1, 1);
        assert_eq!(out.base.get_pixel(0, 0).0, expected);
    }
    // Deliberately independent direct 2-D oracle, with every clamped tap
    // retained separately. Exercises negative lobes and no intermediate clamp.
    let image = RgbaImage::from_fn(7, 5, |x, y| {
        Rgba([
            (x * 41) as u8,
            (y * 59) as u8,
            ((x + y) * 29) as u8,
            ((x * 17 + y * 53) % 256) as u8,
        ])
    });
    let decode = |v: u8| {
        let x = f64::from(v) / 255.0;
        if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };
    let encode = |x: f64| {
        let x = if x <= 0.0031308 {
            12.92 * x
        } else {
            1.055 * x.powf(1.0 / 2.4) - 0.055
        };
        (x * 255.0 + 0.5).floor() as u8
    };
    let weights = |s: usize, d: usize, p: usize| {
        let h = s as f64 / d as f64;
        let u = (p as f64 + 0.5) * h - 0.5;
        let mut v = Vec::new();
        for i in (u - 3.0 * h).ceil() as i64..=(u + 3.0 * h).floor() as i64 {
            let t = (u - i as f64) / h;
            let sinc = |x: f64| {
                if x == 0.0 {
                    1.0
                } else {
                    (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
                }
            };
            v.push((
                i.clamp(0, s as i64 - 1) as u32,
                if t.abs() < 3.0 {
                    sinc(t) * sinc(t / 3.0)
                } else {
                    0.0
                },
            ));
        }
        let sum: f64 = v.iter().map(|p| p.1).sum();
        for p in &mut v {
            p.1 /= sum;
        }
        v
    };
    let p = base::decode(&image, &cancel).unwrap();
    for w in 1..=7 {
        for h in 1..=5 {
            let actual = base::resize(&p, 7, 5, w, h, &cancel).unwrap().0;
            for y in 0..h {
                for x in 0..w {
                    let mut value = [0.0; 4];
                    for (sy, wy) in weights(5, h, y) {
                        for (sx, wx) in weights(7, w, x) {
                            let pixel = image.get_pixel(sx, sy);
                            let a = f64::from(pixel[3]) / 255.0;
                            for c in 0..3 {
                                value[c] += a * decode(pixel[c]) * wx * wy;
                            }
                            value[3] += a * wx * wy;
                        }
                    }
                    let alpha = value[3].clamp(0.0, 1.0);
                    let alpha_byte = (alpha * 255.0 + 0.5).floor() as u8;
                    let expected = if alpha_byte == 0 {
                        [0; 4]
                    } else {
                        [
                            encode(value[0].clamp(0.0, alpha) / alpha),
                            encode(value[1].clamp(0.0, alpha) / alpha),
                            encode(value[2].clamp(0.0, alpha) / alpha),
                            alpha_byte,
                        ]
                    };
                    assert_eq!(actual.get_pixel(x as u32, y as u32).0, expected);
                }
            }
        }
    }
}

fn proposal(id: usize, q: V, color: usize, z: f64) -> Candidate {
    Candidate {
        id,
        kind: Kind::Ridge,
        q,
        n: [0.0, 1.0],
        sigma: 1.0,
        channel: 0,
        polarity: 1,
        response: 0.1,
        contrast: 0.5,
        z,
        effective_z: z,
        direction: None,
        center: [0.0, 0.0, 0.0, 1.0],
        minus: [1.0; 4],
        plus: [1.0; 4],
        colors: [color, 2, 3],
    }
}

#[test]
fn eligibility_precedes_slot_winner_and_does_not_relocate_peak() {
    let cancel = CancellationToken::default();
    let mut source = RgbaImage::from_pixel(4, 4, Rgba([0, 0, 0, 255]));
    source.put_pixel(0, 0, Rgba([0, 0, 0, 254]));
    let mut reference =
        Reference::new_srgb(Arc::new(source), Settings::default(), &cancel).unwrap();
    reference.candidates = vec![
        proposal(0, [0.49, 0.49], 0, 100.0),
        proposal(1, [0.51, 0.51], 5, 2.0),
    ];
    reference.active = vec![0, 1];
    let g = selection::Geometry {
        w: 2,
        h: 2,
        sx: 0.5,
        sy: 0.5,
    };
    let mut diagnostics = Diagnostics {
        collisions: vec![0; 4],
        ..Default::default()
    };
    let mut base = RgbaImage::from_pixel(2, 2, Rgba([100, 100, 100, 255]));
    let slots = selection::transport(&reference, &base, g, &mut diagnostics, &cancel).unwrap();
    assert!(slots.iter().flatten().all(|e| e.candidate == 1));
    assert!(slots.iter().flatten().any(|e| e.pixel == 0));
    reference.candidates = vec![proposal(0, [1.0, 0.5], 5, 2.0)];
    reference.active = vec![0];
    base.put_pixel(0, 0, Rgba([100, 100, 100, 254]));
    let slots = selection::transport(&reference, &base, g, &mut diagnostics, &cancel).unwrap();
    let e = slots.iter().flatten().next().unwrap();
    assert_eq!(e.pixel, 1);
    assert!(
        (e.score - 2.0 / 3.0).abs() < 1e-14,
        "geometric peak must not move to eligible runner-up"
    );
}

#[test]
fn equal_luminance_color_stroke_and_parallel_owner_determinism() {
    let image = RgbaImage::from_fn(41, 41, |_, y| {
        if (19..22).contains(&y) {
            Rgba([255, 0, 0, 255])
        } else {
            Rgba([0, 148, 0, 255])
        }
    });
    let (reference, out) = render(image, 20, 20);
    assert!(
        reference
            .candidates
            .iter()
            .any(|c| c.kind == Kind::Ridge && c.channel < 3)
    );
    assert_contract(&reference, &out);
    for gap in [1, 2, 3, 5] {
        let image = RgbaImage::from_fn(41, 41, |_, y| {
            if y == 18 || y == 18 + gap {
                Rgba([20, 30, 10, 255])
            } else {
                Rgba([230, 210, 170, 255])
            }
        });
        let (mut reference, a) = render(image, 13, 13);
        reference.clear_target_cache();
        let b = reference
            .resize(13, 13, &CancellationToken::default())
            .unwrap();
        assert_eq!(a.image, b.image);
        assert_eq!(a.diagnostics.owners, b.diagnostics.owners);
        assert_contract(&reference, &a);
    }
}

#[test]
fn confidence_control_preserves_v1_and_gate_caps_before_competition() {
    let source = Arc::new(RgbaImage::from_fn(41, 41, |x, y| {
        let v = if x.abs_diff(20) <= 1 || y.abs_diff(20) <= 1 {
            30
        } else {
            230
        };
        Rgba([v, v, v, 255])
    }));
    let cancel = CancellationToken::default();
    let mut original = Reference::new_srgb(source.clone(), Settings::default(), &cancel).unwrap();
    let mut control = Reference::new_srgb(
        source.clone(),
        Settings {
            measure_direction: true,
            ..Default::default()
        },
        &cancel,
    )
    .unwrap();
    let mut gate = Reference::new_srgb(
        source,
        Settings {
            confidence_gate: true,
            ..Default::default()
        },
        &cancel,
    )
    .unwrap();
    let a = original.resize(20, 20, &cancel).unwrap();
    let b = control.resize(20, 20, &cancel).unwrap();
    let c = gate.resize(20, 20, &cancel).unwrap();
    assert_eq!(a.image, b.image);
    assert_eq!(a.diagnostics.owners, b.diagnostics.owners);
    assert_eq!(original.active, control.active);
    assert_eq!(control.candidates.len(), gate.candidates.len());
    let mut uncertain = 0;
    for (a, b) in control.candidates.iter().zip(&gate.candidates) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.q, b.q);
        assert_eq!(a.n, b.n);
        assert_eq!(a.colors, b.colors);
        assert_eq!(a.z, b.z);
        assert_eq!(a.direction, b.direction);
        assert_eq!(a.effective_z, a.z);
        if !b.direction.unwrap().confident {
            uncertain += 1;
            assert_eq!(b.effective_z, b.z.min(0.75));
        } else {
            assert_eq!(b.effective_z, b.z);
        }
    }
    assert!(uncertain > 0);
    for entry in c.diagnostics.slots.iter().flatten() {
        if !gate.candidates[entry.candidate]
            .direction
            .unwrap()
            .confident
        {
            assert!(entry.score <= 0.75);
        }
    }
    assert_contract(&gate, &c);
}

#[test]
fn cancellation_during_preparation_and_target_never_publishes_partial_output() {
    let source = Arc::new(stroke(81, 81, 0.3, 20.0, 2.0, false));
    let cancel = CancellationToken::default();
    let mut reference = Reference::new_srgb(source, Settings::default(), &cancel).unwrap();
    CANCEL_AFTER_CHECKS.with(|n| n.set(Some(300)));
    let result = reference.resize(40, 40, &cancel);
    CANCEL_AFTER_CHECKS.with(|n| n.set(None));
    assert!(matches!(result, Err(Error::Cancelled)));
    assert!(reference.p.is_empty());
    assert!(reference.cached.is_none());
    let cancel = CancellationToken::default();
    let original = reference.resize(40, 40, &cancel).unwrap();
    CANCEL_AFTER_CHECKS.with(|n| n.set(Some(100)));
    let result = reference.resize(39, 39, &cancel);
    CANCEL_AFTER_CHECKS.with(|n| n.set(None));
    assert!(matches!(result, Err(Error::Cancelled)));
    assert!(reference.cached.is_none());
    assert_eq!(original.image.dimensions(), (40, 40));
    assert_contract(&reference, &original);
}
pub(super) fn assert_contract(reference: &Reference, output: &Output) {
    assert_eq!(output.base.dimensions(), output.image.dimensions());
    for (i, (base, actual)) in output.base.pixels().zip(output.image.pixels()).enumerate() {
        if let Some(Some(entry)) = output.diagnostics.owners.get(i) {
            assert_eq!(
                &actual.0,
                &reference.source.as_raw()[entry.color * 4..entry.color * 4 + 4]
            );
            assert_eq!(actual[3], 255);
            assert_eq!(base[3], 255);
        } else {
            assert_eq!(base, actual, "changed outside mask at {i}");
        }
    }
    let g = selection::Geometry {
        w: output.image.width() as usize,
        h: output.image.height() as usize,
        sx: f64::from(output.image.width()) / f64::from(reference.source.width()),
        sy: f64::from(output.image.height()) / f64::from(reference.source.height()),
    };
    for layer in output.diagnostics.nms.chunks(g.w * g.h) {
        for a in layer.iter().flatten() {
            let x = a.pixel % g.w;
            let y = a.pixel / g.w;
            for yy in y.saturating_sub(1)..=(y + 1).min(g.h - 1) {
                for xx in x.saturating_sub(1)..=(x + 1).min(g.w - 1) {
                    let j = yy * g.w + xx;
                    if j > a.pixel
                        && let Some(b) = &layer[j]
                    {
                        assert!(
                            !selection::conflict(a, b, g),
                            "cross-normal competing lanes"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn identity_constants_and_degenerate_dimensions() {
    let arbitrary = RgbaImage::from_fn(11, 7, |x, y| {
        Rgba([
            (x * 19) as u8,
            (y * 31) as u8,
            91,
            if x % 2 == 0 { 0 } else { 173 },
        ])
    });
    let (_, out) = render(arbitrary.clone(), 11, 7);
    assert_eq!(out.image, arbitrary);
    for (sw, sh) in [(1, 19), (19, 1), (17, 23), (18, 24)] {
        for (w, h) in [(1, 1), (sw, 1), (1, sh), ((sw / 2).max(1), (sh / 2).max(1))] {
            for color in [[0, 0, 0, 255], [255, 255, 255, 255], [31, 97, 201, 255]] {
                let (reference, out) = render(RgbaImage::from_pixel(sw, sh, Rgba(color)), w, h);
                assert!(out.image.pixels().all(|p| p.0 == color));
                assert!(out.diagnostics.owners.iter().all(Option::is_none));
                assert_contract(&reference, &out);
            }
        }
    }
}

#[test]
fn hidden_rgb_and_transparency_do_not_protect_silhouettes() {
    let source = RgbaImage::from_fn(32, 32, |x, _| {
        if (14..18).contains(&x) {
            Rgba([200, 80, 40, 140])
        } else {
            Rgba([0, 0, 0, 0])
        }
    });
    let random = RgbaImage::from_fn(32, 32, |x, y| {
        if source.get_pixel(x, y)[3] == 0 {
            Rgba([(x * 17) as u8, (y * 11) as u8, 237, 0])
        } else {
            *source.get_pixel(x, y)
        }
    });
    let (a, aa) = render(source, 13, 19);
    let (b, bb) = render(random, 13, 19);
    assert_eq!(aa.image, bb.image);
    assert_eq!(aa.image, aa.base);
    assert_eq!(aa.diagnostics.owners, bb.diagnostics.owners);
    assert_eq!(a.candidates.len(), b.candidates.len());
    for (a, b) in a.candidates.iter().zip(&b.candidates) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.z, b.z);
        assert_eq!(a.colors, b.colors);
    }
}

#[test]
fn disabled_base_repeatability_and_resource_errors() {
    let source = Arc::new(stroke(49, 41, 0.31, 20.25, 2.0, false));
    let cancel = CancellationToken::default();
    let mut plain = Reference::new_srgb(
        source.clone(),
        Settings {
            protection: false,
            ..Default::default()
        },
        &cancel,
    )
    .unwrap();
    let mut protected = Reference::new_srgb(source.clone(), Settings::default(), &cancel).unwrap();
    let p = plain.resize(21, 17, &cancel).unwrap();
    let a = protected.resize(21, 17, &cancel).unwrap();
    assert_eq!(p.image, a.base);
    assert!(plain.candidates.is_empty());
    protected.clear_target_cache();
    let b = protected.resize(21, 17, &cancel).unwrap();
    assert_eq!(a.image, b.image);
    assert_eq!(a.diagnostics.owners, b.diagnostics.owners);
    assert_contract(&protected, &a);
    assert!(matches!(
        protected.resize(0, 1, &cancel),
        Err(Error::Dimensions)
    ));
    assert!(matches!(
        protected.resize(50, 41, &cancel),
        Err(Error::Dimensions)
    ));
    assert!(matches!(
        Reference::new_srgb(
            source.clone(),
            Settings {
                memory_budget: 1,
                ..Default::default()
            },
            &cancel
        ),
        Err(Error::Resource(_))
    ));
    assert!(matches!(
        Reference::new_srgb(
            source.clone(),
            Settings {
                scales: vec![f64::NAN],
                ..Default::default()
            },
            &cancel
        ),
        Err(Error::Settings)
    ));
    let mut limited = Reference::new_srgb(
        source,
        Settings {
            candidate_limit: 1,
            ..Default::default()
        },
        &cancel,
    )
    .unwrap();
    assert!(matches!(
        limited.resize(20, 20, &cancel),
        Err(Error::Resource(_))
    ));
    assert!(limited.p.is_empty());
    cancel.cancel();
    assert!(matches!(
        protected.resize(21, 17, &cancel),
        Err(Error::Cancelled)
    ));
}

// Independently rendered analytic strip: supersampled source coverage, NOT the
// detector, transport, or a precomputed replacement mask. Quarter-pixel phase
// and oblique widths are continuous geometric input parameters.
fn stroke(w: u32, h: u32, slope: f64, intercept: f64, width: f64, bright: bool) -> RgbaImage {
    RgbaImage::from_fn(w, h, |x, y| {
        let mut hits = 0;
        for iy in 0..4 {
            for ix in 0..4 {
                let xx = f64::from(x) + (f64::from(ix) + 0.5) / 4.0 - 0.5;
                let yy = f64::from(y) + (f64::from(iy) + 0.5) / 4.0 - 0.5;
                if (yy - slope * xx - intercept).abs() / (1.0 + slope * slope).sqrt() < width * 0.5
                {
                    hits += 1;
                }
            }
        }
        let v = if bright {
            30 + hits * 13
        } else {
            238 - hits * 13
        };
        Rgba([v as u8, v as u8, v as u8, 255])
    })
}

#[test]
fn step_is_edge_not_ridge_and_selects_supported_side() {
    let source = RgbaImage::from_fn(48, 32, |x, _| {
        if x < 24 {
            Rgba([30, 80, 110, 255])
        } else {
            Rgba([220, 180, 70, 255])
        }
    });
    let (reference, out) = render(source, 19, 13);
    assert!(reference.candidates.iter().any(|c| c.kind == Kind::Edge));
    assert!(reference.candidates.iter().all(|c| c.kind != Kind::Ridge));
    assert!(out.diagnostics.owners.iter().any(Option::is_some));
    assert_contract(&reference, &out);
    for e in out.diagnostics.owners.iter().flatten() {
        let c = &reference.candidates[e.candidate];
        let p = [(e.pixel % 19) as f64, (e.pixel / 19) as f64];
        let u = [
            (p[0] + 0.5) * 48.0 / 19.0 - 0.5,
            (p[1] + 0.5) * 32.0 / 13.0 - 0.5,
        ];
        assert_eq!(
            e.color,
            if dot(sub(u, c.q), c.n) > 0.0 {
                c.colors[2]
            } else {
                c.colors[1]
            }
        );
    }
}

#[test]
fn weak_noise_and_ramp_remain_base() {
    for image in [
        RgbaImage::from_fn(64, 32, |x, y| {
            let v = 100 + ((x * 13 + y * 7) % 3) as u8;
            Rgba([v, v, v, 255])
        }),
        RgbaImage::from_fn(64, 32, |x, _| {
            let v = 60 + x as u8;
            Rgba([v, v, v, 255])
        }),
    ] {
        let (_, out) = render(image, 29, 13);
        assert_eq!(out.image, out.base);
        assert!(out.diagnostics.owners.iter().all(Option::is_none));
    }
}

#[test]
fn inverse_transpose_and_weak_seed_graph() {
    let g = selection::Geometry {
        w: 8,
        h: 8,
        sx: 0.3,
        sy: 0.7,
    };
    let n = norm([2.0, 3.0]).unwrap();
    let t = tangent(n);
    assert!(dot(g.normal(n), [t[0] * g.sx, t[1] * g.sy]).abs() < 1e-14);
    let mut candidates = Vec::new();
    let mut entries = vec![None; 64];
    for (i, pixel) in [17, 18, 19].into_iter().enumerate() {
        candidates.push(Candidate {
            id: i,
            kind: Kind::Ridge,
            q: [i as f64, 3.0],
            n: [0.0, 1.0],
            sigma: 1.0,
            channel: 0,
            polarity: 1,
            response: 0.1,
            contrast: 0.5,
            z: 1.0,
            effective_z: 1.0,
            direction: None,
            center: [0.0, 0.0, 0.0, 1.0],
            minus: [1.0; 4],
            plus: [1.0; 4],
            colors: [0; 3],
        });
        entries[pixel] = Some(Entry {
            candidate: i,
            pixel,
            normal: [0.0, 1.0],
            position: [0.0, 0.0],
            score: 0.6,
            distance_squared: 0.0,
            color: 0,
        });
    }
    let cancel = CancellationToken::default();
    let (weak, _) = selection::hysteresis(&entries, &candidates, g, &cancel).unwrap();
    assert!(weak.iter().all(Option::is_none));
    entries[17].as_mut().unwrap().score = 1.0;
    let (seeded, _) = selection::hysteresis(&entries, &candidates, g, &cancel).unwrap();
    assert_eq!(seeded.iter().flatten().count(), 3);
}

#[test]
fn detector_stroke_widths_and_geometric_invariant_sweep() {
    for width in [1.0, 2.0, 3.0, 4.0, 8.0, 10.0] {
        for bright in [false, true] {
            let (reference, out) = render(stroke(41, 41, 0.0, 20.0, width, bright), 20, 20);
            let ridges = reference
                .candidates
                .iter()
                .filter(|c| c.kind == Kind::Ridge)
                .count();
            eprintln!(
                "axis width={width} bright={bright} ridge proposals={ridges} protected={}",
                out.diagnostics.owners.iter().flatten().count()
            );
            if width <= 4.0 {
                assert!(ridges > 0);
            }
            assert_contract(&reference, &out);
        }
    }
    for slope in [0.0, 0.2, 0.5, 1.0, 2.0] {
        for phase in [0.0, 0.25, 0.5, 0.75] {
            let source = Arc::new(stroke(
                41,
                41,
                slope,
                20.0 - 20.0 * slope + phase,
                2.0,
                false,
            ));
            let mut reference =
                Reference::new_srgb(source, Settings::default(), &CancellationToken::default())
                    .unwrap();
            for (w, h) in [
                (37, 37),
                (31, 31),
                (20, 20),
                (10, 10),
                (4, 4),
                (13, 27),
                (41, 19),
            ] {
                let out = reference
                    .resize(w, h, &CancellationToken::default())
                    .unwrap();
                assert_contract(&reference, &out);
            }
        }
    }
}

pub(super) fn export(reference: &Reference, out: &Output, dir: &Path, prefix: &str) {
    let w = out.image.width();
    let h = out.image.height();
    let n = (w * h) as usize;
    out.base
        .save(dir.join(format!("{prefix}-base.png")))
        .unwrap();
    out.image
        .save(dir.join(format!("{prefix}-final.png")))
        .unwrap();
    let map = |entries: &[Option<Entry>]| {
        RgbaImage::from_fn(w, h, |x, y| {
            match entries.get((y * w + x) as usize).copied().flatten() {
                Some(e) if reference.candidates[e.candidate].kind == Kind::Ridge => {
                    Rgba([255, 100, 40, 255])
                }
                Some(_) => Rgba([40, 160, 255, 255]),
                None => Rgba([0, 0, 0, 255]),
            }
        })
    };
    let mask = map(&out.diagnostics.owners);
    mask.save(dir.join(format!("{prefix}-mask.png"))).unwrap();
    for (kind, entries) in [
        ("nms", &out.diagnostics.nms),
        ("hysteresis", &out.diagnostics.hysteresis),
    ] {
        for (layer, entries) in entries.chunks(n).enumerate() {
            map(entries)
                .save(dir.join(format!("{prefix}-{kind}-{layer}.png")))
                .unwrap();
        }
    }
    let collisions = RgbaImage::from_fn(w, h, |x, y| {
        let value = out
            .diagnostics
            .collisions
            .get((y * w + x) as usize)
            .copied()
            .unwrap_or(0)
            .min(255) as u8;
        Rgba([value, 0, 0, 255])
    });
    collisions
        .save(dir.join(format!("{prefix}-collisions.png")))
        .unwrap();
    let mut panel = RgbaImage::new(w * 3, h);
    for (column, img) in [&out.base, &mask, &out.image].into_iter().enumerate() {
        image::imageops::replace(&mut panel, img, i64::from(w) * column as i64, 0);
    }
    panel
        .save(dir.join(format!("{prefix}-comparison.png")))
        .unwrap();
    image::imageops::resize(&panel, w * 9, h * 3, image::imageops::FilterType::Nearest)
        .save(dir.join(format!("{prefix}-comparison-3x.png")))
        .unwrap();
    let mut provenance =
        String::from("stage\tpixel\tcandidate_id\tkind\tscore\tsource_color_index\tcomponent\n");
    for (stage, entries) in [
        ("slot", &out.diagnostics.slots),
        ("nms", &out.diagnostics.nms),
        ("hysteresis", &out.diagnostics.hysteresis),
        ("owner", &out.diagnostics.owners),
    ] {
        for (i, e) in entries.iter().enumerate() {
            if let Some(e) = e {
                let c = &reference.candidates[e.candidate];
                writeln!(
                    provenance,
                    "{stage}\t{}\t{}\t{:?}\t{}\t{}\t{}",
                    e.pixel,
                    c.id,
                    c.kind,
                    e.score,
                    e.color,
                    if stage == "hysteresis" {
                        out.diagnostics.components[i]
                    } else {
                        0
                    }
                )
                .unwrap();
            }
        }
    }
    std::fs::write(dir.join(format!("{prefix}-provenance.tsv")), provenance).unwrap();
}

#[test]
#[ignore = "exports artwork diagnostics and release timing samples"]
fn artwork_reference_comparison() {
    let input = std::env::var("DIORAMA_CONTOUR_INPUT")
        .unwrap_or_else(|_| "/home/mendrik/Downloads/elf2-se.png".into());
    let source = Arc::new(image::open(&input).unwrap().to_rgba8());
    let dir = tempfile::Builder::new()
        .prefix("diorama-contour-lanczos-")
        .tempdir()
        .unwrap()
        .keep();
    let cancel = CancellationToken::default();
    let mode = std::env::var("DIORAMA_CONTOUR_CONFIDENCE").unwrap_or_else(|_| "none".into());
    assert!(["none", "control", "gate"].contains(&mode.as_str()));
    let settings = Settings {
        measure_direction: mode != "none",
        confidence_gate: mode == "gate",
        ..Default::default()
    };
    let mut reference = Reference::new_srgb(source.clone(), settings.clone(), &cancel).unwrap();
    let mut report = String::new();
    let benchmark_only = std::env::var_os("DIORAMA_CONTOUR_BENCH_ONLY").is_some();
    writeln!(report,"input={input}; mode={mode}; scalar reference, 1 worker; diagnostic matching excluded from render timings").unwrap();
    for size in [128, 160] {
        let height = (u64::from(source.height()) * u64::from(size) / u64::from(source.width()))
            .max(1) as u32;
        let out = reference.resize(size, height, &cancel).unwrap();
        assert_contract(&reference, &out);
        if !benchmark_only {
            export(&reference, &out, &dir, &size.to_string());
            evaluation::diagnostics(&reference, &out, &dir, &size.to_string(), &cancel).unwrap();
        }
        writeln!(report,"{size}: preparation {:?}; target {:?}; proposals {}; retained {}; ineligible {}; owners {}; displaced {}; flank associations {}; association checks {}; owner collisions {}; collision broken links {}; conservative peak envelope {} bytes",
            reference.preparation_time,out.diagnostics.target_time,out.diagnostics.proposals,out.diagnostics.source_retained,
            out.diagnostics.ineligible_splats,out.diagnostics.owners.iter().flatten().count(),out.diagnostics.displaced,
            out.diagnostics.associated_flanks,out.diagnostics.association_checks,out.diagnostics.owner_collisions,
            out.diagnostics.collision_broken_links,out.diagnostics.estimated_peak_bytes).unwrap();
        let mut warm = Vec::new();
        let mut plain_times = Vec::new();
        let mut cached = Vec::new();
        let mut plain = Reference::new_srgb(
            source.clone(),
            Settings {
                protection: false,
                ..Default::default()
            },
            &cancel,
        )
        .unwrap();
        plain.resize(size, height, &cancel).unwrap();
        let mut coefficients = Vec::new();
        for _ in 0..10 {
            reference.clear_target_cache();
            let start = Instant::now();
            let output = reference.resize(size, height, &cancel).unwrap();
            coefficients.push(output.diagnostics.coefficient_time);
            warm.push(start.elapsed());
            let start = Instant::now();
            reference.resize(size, height, &cancel).unwrap();
            cached.push(start.elapsed());
            plain.clear_target_cache();
            let start = Instant::now();
            plain.resize(size, height, &cancel).unwrap();
            plain_times.push(start.elapsed());
        }
        for (label, mut times) in [
            ("warm source", warm),
            ("identical base", plain_times),
            ("cached target", cached),
            ("coefficient setup (included in target)", coefficients),
        ] {
            times.sort();
            writeln!(
                report,
                "{size} {label}: median {:?}, p95 {:?} (10 samples)",
                times[5], times[9]
            )
            .unwrap();
        }
    }
    let mut cold = Vec::new();
    let mut preparation = Vec::new();
    for _ in 0..10 {
        let start = Instant::now();
        let mut fresh = Reference::new_srgb(source.clone(), settings.clone(), &cancel).unwrap();
        let height = (u64::from(source.height()) * 128 / u64::from(source.width())).max(1) as u32;
        fresh.resize(128, height, &cancel).unwrap();
        cold.push(start.elapsed());
        preparation.push(fresh.preparation_time);
    }
    for (label, mut times) in [
        ("cold 128 total", cold),
        ("cold source preparation", preparation),
    ] {
        times.sort();
        writeln!(
            report,
            "{label}: median {:?}, p95 {:?} (10 samples)",
            times[5], times[9]
        )
        .unwrap();
    }
    if !benchmark_only {
        let mut proposals = String::from(
            "id\tkind\tx\ty\tnx\tny\tsigma\tchannel\tpolarity\tresponse\tcontrast\tz\tcenter_rgba_p\tminus_rgba_p\tplus_rgba_p\tcolor_coordinates\teffective_z\tdirection_energy_coherence_alignment_confident\n",
        );
        for c in &reference.candidates {
            writeln!(
            proposals,
            "{}\t{:?}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:?}\t{:?}\t{:?}\t{:?}\t{}\t{:?}",
            c.id,
            c.kind,
            c.q[0],
            c.q[1],
            c.n[0],
            c.n[1],
            c.sigma,
            c.channel,
            c.polarity,
            c.response,
            c.contrast,
            c.z,
            c.center,
            c.minus,
            c.plus,
            c.colors,
            c.effective_z,
            c.direction
        )
        .unwrap();
        }
        std::fs::write(dir.join("source-proposals.tsv"), proposals).unwrap();
    }
    std::fs::write(dir.join("report.txt"), &report).unwrap();
    eprintln!("{}\n{report}", dir.display());
}
