//! Frozen scene construction. Opaque RGB samples are either point sampled at
//! pixel centers or a declared 4x4 regular quadrature of a unit box footprint.
//! Geometry is projected from analytic segments, independently of both image
//! sampling and the scaler's detector. No tuned/AI-recovered masks are used.
use super::*;

#[derive(Clone)]
struct Segment {
    a: V,
    b: V,
    width: f64,
    kind: Kind,
}
#[derive(Clone)]
struct Scene {
    name: String,
    category: &'static str,
    segments: Vec<Segment>,
    phase: f64,
    bright: bool,
}

impl Scene {
    fn rgb(&self, q: V) -> [u8; 3] {
        let [x, y] = q;
        let p = self.phase;
        match self.name.as_str() {
            "checkerboard" => {
                if (((x - p) / 3.0).floor() as i64 + ((y - p) / 3.0).floor() as i64) % 2 == 0 {
                    [230, 230, 230]
                } else {
                    [30, 30, 30]
                }
            }
            "equal-luminance-boundary" => {
                if x < 20.0 + p {
                    [255, 0, 0]
                } else {
                    [0, 148, 0]
                }
            }
            "channel-conflict" => [
                if x < 20.0 + p { 30 } else { 230 },
                if y < 20.0 + p { 30 } else { 230 },
                80,
            ],
            "flat-noise" => {
                let v = 100
                    + (((x.floor() as i64 * 17 + y.floor() as i64 * 31).unsigned_abs() % 3) as u8);
                [v; 3]
            }
            "flat" => [100; 3],
            _ => {
                let inside = self.segments.iter().any(|s| {
                    let d = sub(s.b, s.a);
                    let t = (dot(sub(q, s.a), d) / dot(d, d)).clamp(0.0, 1.0);
                    let r = sub(q, offset(s.a, d, t));
                    dot(r, r) < s.width * s.width * 0.25
                });
                if self.name == "color-stripes" {
                    if inside { [255, 0, 0] } else { [0, 148, 0] }
                } else if inside ^ self.bright {
                    [30; 3]
                } else {
                    [230; 3]
                }
            }
        }
    }
    fn source(&self, area: bool) -> RgbaImage {
        RgbaImage::from_fn(41, 41, |x, y| {
            if !area {
                let [r, g, b] = self.rgb([f64::from(x), f64::from(y)]);
                return Rgba([r, g, b, 255]);
            }
            let mut sum = [0_u32; 3];
            for j in 0..4 {
                for i in 0..4 {
                    let value = self.rgb([
                        f64::from(x) + (f64::from(i) + 0.5) / 4.0 - 0.5,
                        f64::from(y) + (f64::from(j) + 0.5) / 4.0 - 0.5,
                    ]);
                    for c in 0..3 {
                        sum[c] += u32::from(value[c]);
                    }
                }
            }
            Rgba([
                ((sum[0] + 8) / 16) as u8,
                ((sum[1] + 8) / 16) as u8,
                ((sum[2] + 8) / 16) as u8,
                255,
            ])
        })
    }
    fn geometry(&self, w: u32, h: u32) -> Vec<Feature> {
        let (sx, sy) = (f64::from(w) / 41.0, f64::from(h) / 41.0);
        let map = |q: V| [(q[0] + 0.5) * sx - 0.5, (q[1] + 0.5) * sy - 0.5];
        let mut out: std::collections::BTreeMap<(Kind, i64, i64), Feature> =
            std::collections::BTreeMap::new();
        let mut all_segments = self.segments.clone();
        for s in &self.segments {
            if s.kind == Kind::Ridge {
                let d = sub(s.b, s.a);
                let n = norm([-d[1], d[0]]).unwrap();
                for side in [-0.5, 0.5] {
                    all_segments.push(Segment {
                        a: offset(s.a, n, side * s.width),
                        b: offset(s.b, n, side * s.width),
                        width: 0.0,
                        kind: Kind::Edge,
                    });
                }
            }
        }
        for (index, s) in all_segments.iter().enumerate() {
            let a = map(s.a);
            let b = map(s.b);
            let d = sub(b, a);
            let n = norm([-d[1], d[0]]).unwrap();
            let major = usize::from(d[1].abs() > d[0].abs());
            let minor = 1 - major;
            for i in a[major].min(b[major]).ceil() as i64..=a[major].max(b[major]).floor() as i64 {
                let t = (i as f64 - a[major]) / d[major];
                let mut q = a;
                q[major] = i as f64;
                q[minor] += t * d[minor];
                // Keep geometry even when the independent evaluator cannot
                // measure near the frame. Missing evidence is not correctness.
                if q[0] < 0.0
                    || q[1] < 0.0
                    || q[0] > f64::from(w) - 1.0
                    || q[1] > f64::from(h) - 1.0
                {
                    continue;
                }
                let key = (
                    s.kind,
                    (q[0] + 0.5).floor() as i64,
                    (q[1] + 0.5).floor() as i64,
                );
                let feature = Feature {
                    id: index * 10_000 + i as usize,
                    kind: s.kind,
                    q,
                    normal: Some(n),
                    strength: 1.0,
                };
                if let Some(previous) = out.get_mut(&key) {
                    if previous
                        .normal
                        .is_some_and(|p| dot(p, n).abs() < 3.0_f64.sqrt() / 2.0)
                    {
                        previous.normal = None;
                    }
                } else {
                    out.insert(key, feature);
                }
            }
        }
        out.into_values().collect()
    }
}

fn scenes(phase: f64) -> Vec<Scene> {
    let line = |a, b, width, kind| Segment { a, b, width, kind };
    let mut result = Vec::new();
    for width in [1.0, 2.0, 3.0, 4.0, 8.0, 10.0] {
        for bright in [false, true] {
            result.push(Scene {
                name: format!("axis-w{width}-{}", if bright { "bright" } else { "dark" }),
                category: "isolated",
                phase,
                bright,
                segments: vec![line(
                    [3.0, 20.0 + phase],
                    [37.0, 20.0 + phase],
                    width,
                    Kind::Ridge,
                )],
            });
        }
    }
    for (name, a, b) in [
        ("vertical", [20.0 + phase, 3.0], [20.0 + phase, 37.0]),
        ("diagonal", [3.0, 3.0 + phase], [37.0, 37.0 + phase]),
        ("shallow", [3.0, 15.0 + phase], [37.0, 24.0 + phase]),
    ] {
        result.push(Scene {
            name: name.into(),
            category: "isolated",
            phase,
            bright: false,
            segments: vec![line(a, b, 2.0, Kind::Ridge)],
        });
    }
    for (name, spacing) in [
        ("fence-resolvable", 6),
        ("fence-dense", 2),
        ("color-stripes", 2),
    ] {
        result.push(Scene {
            name: name.into(),
            category: "repetitive",
            phase,
            bright: false,
            segments: (4..38)
                .step_by(spacing)
                .map(|x| {
                    line(
                        [f64::from(x) + phase, 3.0],
                        [f64::from(x) + phase, 37.0],
                        1.0,
                        Kind::Ridge,
                    )
                })
                .collect(),
        });
    }
    for (name, segments) in [
        (
            "T-junction",
            vec![
                line([4.0, 12.0 + phase], [36.0, 12.0 + phase], 2.0, Kind::Ridge),
                line([20.0 + phase, 12.0], [20.0 + phase, 37.0], 2.0, Kind::Ridge),
            ],
        ),
        (
            "X-junction",
            vec![
                line([4.0, 4.0 + phase], [36.0, 36.0 + phase], 2.0, Kind::Ridge),
                line([4.0, 36.0 + phase], [36.0, 4.0 + phase], 2.0, Kind::Ridge),
            ],
        ),
        (
            "fine-text-H",
            vec![
                line([10.0 + phase, 5.0], [10.0 + phase, 35.0], 1.0, Kind::Ridge),
                line([28.0 + phase, 5.0], [28.0 + phase, 35.0], 1.0, Kind::Ridge),
                line([10.0 + phase, 20.0], [28.0 + phase, 20.0], 1.0, Kind::Ridge),
            ],
        ),
        (
            "equal-luminance-boundary",
            vec![line(
                [20.0 + phase, 0.0],
                [20.0 + phase, 40.0],
                0.0,
                Kind::Edge,
            )],
        ),
        (
            "channel-conflict",
            vec![
                line([20.0 + phase, 0.0], [20.0 + phase, 40.0], 0.0, Kind::Edge),
                line([0.0, 20.0 + phase], [40.0, 20.0 + phase], 0.0, Kind::Edge),
            ],
        ),
    ] {
        result.push(Scene {
            name: name.into(),
            category: if name == "equal-luminance-boundary" {
                "isolated"
            } else {
                "junction"
            },
            segments,
            phase,
            bright: false,
        });
    }
    let mut checker = Vec::new();
    for i in (3..40).step_by(3) {
        checker.push(line(
            [f64::from(i) + phase, 0.0],
            [f64::from(i) + phase, 40.0],
            0.0,
            Kind::Edge,
        ));
        checker.push(line(
            [0.0, f64::from(i) + phase],
            [40.0, f64::from(i) + phase],
            0.0,
            Kind::Edge,
        ));
    }
    result.push(Scene {
        name: "checkerboard".into(),
        category: "repetitive",
        segments: checker,
        phase,
        bright: false,
    });
    let points: Vec<V> = (0..=16)
        .map(|i| {
            let t = f64::from(i) * std::f64::consts::PI / 16.0;
            [20.0 + 15.0 * t.cos(), 20.0 + phase + 15.0 * t.sin()]
        })
        .collect();
    result.push(Scene {
        name: "curve-held-out".into(),
        category: "held-out",
        phase,
        bright: false,
        segments: points
            .windows(2)
            .map(|p| line(p[0], p[1], 2.0, Kind::Ridge))
            .collect(),
    });
    for name in ["flat", "flat-noise"] {
        result.push(Scene {
            name: name.into(),
            category: "flat",
            segments: Vec::new(),
            phase,
            bright: false,
        });
    }
    result
}

fn shape(features: &[Feature]) -> (usize, usize) {
    let pixels: BTreeSet<(i64, i64)> = features
        .iter()
        .map(|f| ((f.q[0] + 0.5).floor() as i64, (f.q[1] + 0.5).floor() as i64))
        .collect();
    let blocks = pixels
        .iter()
        .filter(|&&(x, y)| {
            [(x + 1, y), (x, y + 1), (x + 1, y + 1)]
                .iter()
                .all(|p| pixels.contains(p))
        })
        .count();
    let mut remaining = pixels;
    let mut components = 0;
    while let Some(start) = remaining.pop_first() {
        components += 1;
        let mut queue = vec![start];
        while let Some((x, y)) = queue.pop() {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let p = (x + dx, y + dy);
                    if remaining.remove(&p) {
                        queue.push(p);
                    }
                }
            }
        }
    }
    (components, blocks)
}

// Known normal-profile evaluator for isolated analytic strokes. The 0.125-px
// bilinear sampling, radius 4, contrast floor .03 and half-height width are
// fixed for base and output. This measures visible width, not mask thickness.
fn profiles(
    image: &RgbaImage,
    geometry: &[Feature],
    bright: bool,
    cancel: &CancellationToken,
) -> Result<(String, String, String, usize)> {
    let p = base::decode(image, cancel)?;
    let w = image.width() as usize;
    let h = image.height() as usize;
    let mut centers = Vec::new();
    let mut widths = Vec::new();
    let mut contrasts = Vec::new();
    let mut duplicates = 0;
    for g in geometry.iter().filter(|g| g.kind == Kind::Ridge) {
        let Some(normal) = g.normal else {
            continue;
        };
        if !inside(offset(g.q, normal, -4.0), w, h) || !inside(offset(g.q, normal, 4.0), w, h) {
            continue;
        }
        let values: Vec<f64> = (-32..=32)
            .map(|i| sample(&p, w, h, offset(g.q, normal, f64::from(i) / 8.0))[0])
            .collect();
        let background = (values[0] + values[64]) * 0.5;
        let response: Vec<f64> = values
            .iter()
            .map(|v| {
                if bright {
                    v - background
                } else {
                    background - v
                }
            })
            .collect();
        let peaks: Vec<usize> = (1..64)
            .filter(|&i| {
                response[i] >= 0.03
                    && response[i] >= response[i - 1]
                    && response[i] > response[i + 1]
            })
            .collect();
        duplicates += peaks.len().saturating_sub(1);
        if let Some(&peak) = peaks.iter().min_by_key(|&&i| i.abs_diff(32)) {
            centers.push((peak as f64 / 8.0 - 4.0).abs());
            contrasts.push(response[peak]);
            let half = response[peak] * 0.5;
            let mut left = peak;
            let mut right = peak;
            while left > 0 && response[left - 1] >= half {
                left -= 1;
            }
            while right < 64 && response[right + 1] >= half {
                right += 1;
            }
            widths.push((right - left + 1) as f64 / 8.0);
        }
    }
    Ok((
        percentile(centers, 0.95),
        percentile(widths, 0.5),
        percentile(contrasts, 0.5),
        duplicates,
    ))
}

#[test]
#[ignore = "frozen phase/scale/model/family ablation and artifact export"]
fn frozen_confidence_ablation() {
    let dir = tempfile::Builder::new()
        .prefix("diorama-contour-ablation-")
        .tempdir()
        .unwrap()
        .keep();
    let cancel = CancellationToken::default();
    let mut csv = String::from(
        "family,category,phase,sampling,w,h,variant,comparison,kind,junction,representation,tp,fp,fn,precision,recall,iou,f1,status,median_displacement,p95_displacement\n",
    );
    let mut shapes = String::from(
        "family,phase,sampling,w,h,variant,kind,mask_components,filled_2x2,geometry_components,lost_geometry_samples\n",
    );
    let mut profile_csv = String::from(
        "family,phase,sampling,w,h,variant,image,p95_center_error,median_visible_FWHM,median_contrast,duplicate_peaks\n",
    );
    let mut pooled: std::collections::BTreeMap<(String, String, Kind), Counts> =
        std::collections::BTreeMap::new();
    for phase in [0.0, 0.25, 0.5, 0.75] {
        for scene in scenes(phase) {
            for area in [false, true] {
                let sampling = if area {
                    "box-4x4-quadrature"
                } else {
                    "point-center"
                };
                let source = Arc::new(scene.source(area));
                for gate in [false, true] {
                    let variant = if gate { "confidence-gate" } else { "control" };
                    let mut reference = Reference::new_srgb(
                        source.clone(),
                        Settings {
                            measure_direction: true,
                            confidence_gate: gate,
                            ..Default::default()
                        },
                        &cancel,
                    )
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
                        let out = reference.resize(w, h, &cancel).unwrap();
                        tests::assert_contract(&reference, &out);
                        let geometry = scene.geometry(w, h);
                        let mask =
                            from_entries(&reference, &out.diagnostics.owners, w as usize, false);
                        let b = measured(&out.base, &cancel).unwrap();
                        let o = measured(&out.image, &cancel).unwrap();
                        for kind in [Kind::Ridge, Kind::Edge] {
                            for junction in [false, true] {
                                let g: Vec<_> = geometry
                                    .iter()
                                    .filter(|f| f.kind == kind && f.normal.is_none() == junction)
                                    .cloned()
                                    .collect();
                                let keys: BTreeSet<(i64, i64)> = geometry
                                    .iter()
                                    .filter(|f| f.normal.is_none())
                                    .map(|f| {
                                        (
                                            (f.q[0] + 0.5).floor() as i64,
                                            (f.q[1] + 0.5).floor() as i64,
                                        )
                                    })
                                    .collect();
                                for (name, q) in [("G-v-M", &mask), ("G-v-B", &b), ("G-v-O", &o)] {
                                    let q: Vec<_> = q
                                        .iter()
                                        .filter(|f| {
                                            f.kind == kind
                                                && keys.contains(&(
                                                    (f.q[0] + 0.5).floor() as i64,
                                                    (f.q[1] + 0.5).floor() as i64,
                                                )) == junction
                                        })
                                        .cloned()
                                        .collect();
                                    let counts=compare(&g,&q,&format!("{},{},{phase},{sampling},{w},{h},{variant},{name},{kind:?},{junction}",scene.name,scene.category),&mut csv,&cancel).unwrap();
                                    if !junction {
                                        pooled
                                            .entry((
                                                scene.name.clone(),
                                                format!("{variant}-{name}"),
                                                kind,
                                            ))
                                            .or_default()
                                            .add(counts);
                                    }
                                }
                            }
                            let mm: Vec<_> =
                                mask.iter().filter(|f| f.kind == kind).cloned().collect();
                            let gg: Vec<_> = geometry
                                .iter()
                                .filter(|f| f.kind == kind)
                                .cloned()
                                .collect();
                            let (components, blocks) = shape(&mm);
                            let (gc, _) = shape(&gg);
                            let matched = matching(&gg, &mm, 1.0, &cancel).unwrap().len();
                            writeln!(shapes,"{},{phase},{sampling},{w},{h},{variant},{kind:?},{components},{blocks},{gc},{}",scene.name,gg.len()-matched).unwrap();
                        }
                        if scene.category == "isolated" {
                            for (name, image) in [("B", &out.base), ("O", &out.image)] {
                                let (center, width, contrast, peaks) =
                                    profiles(image, &geometry, scene.bright, &cancel).unwrap();
                                writeln!(profile_csv,"{},{phase},{sampling},{w},{h},{variant},{name},{center},{width},{contrast},{peaks}",scene.name).unwrap();
                            }
                        }
                        if phase == 0.25 && area && (w, h) == (20, 20) {
                            let prefix = format!("{}-{variant}", scene.name);
                            tests::export(&reference, &out, &dir, &prefix);
                            write_features(&dir, &format!("{prefix}-G"), &geometry, w, h);
                        }
                    }
                }
            }
        }
    }
    let mut pooled_csv =
        String::from("family,variant-comparison,kind,tp,fp,fn,precision,recall,iou,f1,status\n");
    for ((family, variant, kind), counts) in pooled {
        writeln!(pooled_csv, "{family},{variant},{kind:?},{}", counts.csv()).unwrap();
    }
    for (name, contents) in [
        ("metrics.csv", csv),
        ("shape.csv", shapes),
        ("profiles.csv", profile_csv),
        ("pooled.csv", pooled_csv),
    ] {
        std::fs::write(dir.join(name), contents).unwrap();
    }
    eprintln!("frozen ablation: {}", dir.display());
}
