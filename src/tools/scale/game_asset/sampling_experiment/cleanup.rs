//! Test-only crisp edge cleanup. No blur and no change to production scaling.

use image::{Rgba, RgbaImage};

fn appearance(p: Rgba<u8>) -> [f32; 4] {
    let alpha = p[3] as f32 / 255.0;
    [
        p[0] as f32 * alpha,
        p[1] as f32 * alpha,
        p[2] as f32 * alpha,
        p[3] as f32,
    ]
}

fn distance(a: Rgba<u8>, b: Rgba<u8>) -> f32 {
    let (a, b) = (appearance(a), appearance(b));
    (((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)) / 3.0
        + (a[3] - b[3]).powi(2))
    .sqrt()
}

fn neighbour(image: &RgbaImage, x: u32, y: u32, dx: i32, dy: i32) -> Option<Rgba<u8>> {
    let (x, y) = (x.checked_add_signed(dx)?, y.checked_add_signed(dy)?);
    (x < image.width() && y < image.height()).then(|| *image.get_pixel(x, y))
}

// Removing a colour-class pixel must not disconnect its neighbours or join
// two previously separated 4-connected background regions through the centre.
fn simple_point(image: &RgbaImage, x: u32, y: u32, fill: Rgba<u8>) -> bool {
    let centre = *image.get_pixel(x, y);
    let mut foreground = [false; 9];
    for dy in -1..=1 {
        for dx in -1..=1 {
            if dx == 0 && dy == 0 {
                continue;
            }
            let Some(p) = neighbour(image, x, y, dx, dy) else {
                return false;
            };
            foreground[((dy + 1) * 3 + dx + 1) as usize] = distance(p, centre) < distance(p, fill);
        }
    }
    let components = |class: bool, diagonal: bool| {
        let mut labels = [0_u8; 9];
        let mut count = 0;
        for start in 0..9 {
            if start == 4 || foreground[start] != class || labels[start] != 0 {
                continue;
            }
            count += 1;
            labels[start] = count;
            let mut pending = vec![start];
            while let Some(p) = pending.pop() {
                for q in 0..9 {
                    let dx = (p % 3).abs_diff(q % 3);
                    let dy = (p / 3).abs_diff(q / 3);
                    if q != 4
                        && foreground[q] == class
                        && labels[q] == 0
                        && dx <= 1
                        && dy <= 1
                        && (diagonal || dx + dy == 1)
                    {
                        labels[q] = count;
                        pending.push(q);
                    }
                }
            }
        }
        (count, labels)
    };
    if components(true, true).0 != 1 {
        return false;
    }
    let (_, background) = components(false, false);
    let touching: std::collections::BTreeSet<_> = [1, 3, 5, 7]
        .into_iter()
        .map(|p| background[p])
        .filter(|label| *label != 0)
        .collect();
    touching.len() == 1
}

fn elbow_fill(image: &RgbaImage, x: u32, y: u32) -> Option<Rgba<u8>> {
    elbow_fills(image, x, y).into_iter().next()
}

fn elbow_fills(image: &RgbaImage, x: u32, y: u32) -> Vec<Rgba<u8>> {
    let centre = *image.get_pixel(x, y);
    if centre[3] < 32 {
        return Vec::new();
    }
    for (sx, sy) in [(1, 1), (-1, 1), (1, -1), (-1, -1)] {
        let (Some(outside), Some(inside)) = (
            neighbour(image, x, y, -sx, -sy),
            neighbour(image, x, y, sx, sy),
        ) else {
            continue;
        };
        let outer_contrast = distance(centre, outside);
        let inner_contrast = distance(centre, inside);
        let tolerance = 35.0_f32
            .min(outer_contrast * 0.45)
            .min(inner_contrast * 0.45);
        let same = |dx, dy| {
            neighbour(image, x, y, dx, dy).is_some_and(|p| distance(p, centre) < tolerance)
        };
        if !same(sx, 0) || !same(0, sy) {
            continue;
        }
        if same(2 * sx, 0) && same(0, 2 * sy) {
            continue;
        }
        if ![(sx, -sy), (2 * sx, -sy), (2 * sx, -2 * sy)]
            .into_iter()
            .any(|(dx, dy)| same(dx, dy))
            || ![(-sx, sy), (-sx, 2 * sy), (-2 * sx, 2 * sy)]
                .into_iter()
                .any(|(dx, dy)| same(dx, dy))
        {
            continue;
        }
        if outer_contrast.max(inner_contrast) > 55.0 && outer_contrast.min(inner_contrast) > 12.0 {
            return [outside, inside]
                .into_iter()
                .filter(|fill| simple_point(image, x, y, *fill))
                .collect();
        }
    }
    Vec::new()
}

fn binary(p: Rgba<u8>) -> Rgba<u8> {
    if p[3] < 128 {
        Rgba([0, 0, 0, 0])
    } else {
        Rgba([p[0], p[1], p[2], 255])
    }
}

fn near_outline(image: &RgbaImage, x: u32, y: u32) -> bool {
    (-2..=2).any(|dy| (-2..=2).any(|dx| neighbour(image, x, y, dx, dy).is_some_and(|p| p[3] == 0)))
}

fn broad_support(image: &RgbaImage, x: u32, y: u32) -> bool {
    (-1..=0).any(|dy| {
        (-1..=0).any(|dx| {
            [(dx, dy), (dx + 1, dy), (dx, dy + 1), (dx + 1, dy + 1)]
                .into_iter()
                .all(|(ox, oy)| neighbour(image, x, y, ox, oy).is_some_and(|p| p[3] >= 128))
        })
    })
}

fn clean_alpha(baseline: &RgbaImage) -> RgbaImage {
    RgbaImage::from_fn(baseline.width(), baseline.height(), |x, y| {
        let p = *baseline.get_pixel(x, y);
        // Search neighbouring broad support too, to remove the outer fringe.
        let broad = (-1..=1).any(|dy| {
            (-1..=1).any(|dx| {
                let (Some(nx), Some(ny)) = (x.checked_add_signed(dx), y.checked_add_signed(dy))
                else {
                    return false;
                };
                nx < baseline.width() && ny < baseline.height() && broad_support(baseline, nx, ny)
            })
        });
        if near_outline(baseline, x, y) && broad {
            binary(p)
        } else {
            p
        }
    })
}

/// A changed pixel can affect elbow detectors two cells away; their own
/// continuation probes need another two cells. Keep this bounded to 9x9.
struct ContourChoice {
    patch: RgbaImage,
    x: u32,
    y: u32,
    protected: Vec<(u32, u32)>,
}

impl ContourChoice {
    fn new(image: &RgbaImage, x: u32, y: u32) -> Self {
        let left = x.saturating_sub(4);
        let top = y.saturating_sub(4);
        let right = (x + 5).min(image.width());
        let bottom = (y + 5).min(image.height());
        let patch =
            image::imageops::crop_imm(image, left, top, right - left, bottom - top).to_image();
        let (x, y) = (x - left, y - top);
        let protected = (y.saturating_sub(2)..=(y + 2).min(patch.height() - 1))
            .flat_map(|py| {
                (x.saturating_sub(2)..=(x + 2).min(patch.width() - 1)).map(move |px| (px, py))
            })
            .filter(|(px, py)| elbow_fill(&patch, *px, *py).is_none())
            .collect();
        Self {
            patch,
            x,
            y,
            protected,
        }
    }

    fn allows(&mut self, pixel: Rgba<u8>) -> bool {
        let original = *self.patch.get_pixel(self.x, self.y);
        self.patch.put_pixel(self.x, self.y, pixel);
        let allowed = self
            .protected
            .iter()
            .all(|(x, y)| elbow_fill(&self.patch, *x, *y).is_none());
        self.patch.put_pixel(self.x, self.y, original);
        allowed
    }

    fn remaining_elbows(&mut self, pixel: Rgba<u8>) -> usize {
        let original = *self.patch.get_pixel(self.x, self.y);
        self.patch.put_pixel(self.x, self.y, pixel);
        let count = (self.y.saturating_sub(2)..=(self.y + 2).min(self.patch.height() - 1))
            .flat_map(|y| {
                (self.x.saturating_sub(2)..=(self.x + 2).min(self.patch.width() - 1))
                    .map(move |x| (x, y))
            })
            .filter(|(x, y)| elbow_fill(&self.patch, *x, *y).is_some())
            .count();
        self.patch.put_pixel(self.x, self.y, original);
        count
    }
}

fn source_replacement(
    source: &RgbaImage,
    image: &RgbaImage,
    x: u32,
    y: u32,
    target: Rgba<u8>,
    preserve_alpha: bool,
) -> Rgba<u8> {
    let original = *image.get_pixel(x, y);
    let mut best = original;
    let initial = distance(original, target);
    let mut error = initial;
    let mut contour = ContourChoice::new(image, x, y);
    let sx = source.width() as f64 / image.width() as f64;
    let sy = source.height() as f64 / image.height() as f64;
    for iy in
        (y as f64 * sy).floor() as u32..(((y + 1) as f64 * sy).ceil() as u32).min(source.height())
    {
        for ix in (x as f64 * sx).floor() as u32
            ..(((x + 1) as f64 * sx).ceil() as u32).min(source.width())
        {
            let p = binary(*source.get_pixel(ix, iy));
            let d = distance(p, target);
            if (!preserve_alpha || p[3] == original[3]) && d < error && contour.allows(p) {
                best = p;
                error = d;
            }
        }
    }
    if error < initial * 0.6 {
        best
    } else {
        original
    }
}

fn clean_stairs(source: &RgbaImage, baseline: &RgbaImage) -> RgbaImage {
    let mut output = baseline.clone();
    for y in 0..output.height() {
        for x in 0..output.width() {
            if !near_outline(baseline, x, y) || !broad_support(baseline, x, y) {
                continue;
            }
            // Evaluate against the current image, so adjacent removals cannot
            // simultaneously destroy the continuation that justified them.
            let original = *output.get_pixel(x, y);
            let mut best = original;
            let mut gain = 0.0;
            for fill in elbow_fills(&output, x, y) {
                let pixel = source_replacement(source, &output, x, y, fill, false);
                let improvement = distance(original, fill) - distance(pixel, fill);
                if improvement > gain
                    && distance(pixel, fill) < distance(pixel, *output.get_pixel(x, y))
                    && simple_point(&output, x, y, pixel)
                {
                    best = pixel;
                    gain = improvement;
                }
            }
            output.put_pixel(x, y, best);
        }
    }
    output
}

#[test]
#[ignore = "requires DIORAMA_GAME_ASSET_INPUT=.../elf2-se.png; crisp cloak cleanup experiment"]
fn elf_cloak_cleanup() {
    let source = image::open(std::env::var("DIORAMA_GAME_ASSET_INPUT").unwrap())
        .unwrap()
        .to_rgba8();
    assert_eq!(source.dimensions(), (800, 800));
    let directory = tempfile::Builder::new()
        .prefix("diorama-cleanup-")
        .tempdir()
        .unwrap()
        .keep();
    let ridge = super::ridge_samples(&source, 128, 30.0, 0.5);
    let crisp = clean_alpha(&ridge);
    let shades = clean_edge_shades(&source, &crisp);
    let stairs = clean_stairs(&source, &shades);
    let cloak_changes = (44..86)
        .flat_map(|y| (26..61).map(move |x| (x, y)))
        .filter(|(x, y)| shades.get_pixel(*x, *y) != stairs.get_pixel(*x, *y))
        .count();
    assert!(
        cloak_changes > 0,
        "the staircase pass must actually affect the reported cloak region"
    );
    // The unobscured middle of the thin bowstring must retain ridge-2 RGBA.
    for y in 38..=46 {
        let sy = (y as f32 + 0.5) * 800.0 / 128.0;
        let centre = (548.0 - (sy - 215.0) * 0.73) * 128.0 / 800.0;
        for x in 0..128 {
            if (x as f32 + 0.5 - centre).abs() < 1.2 {
                assert_eq!(
                    stairs.get_pixel(x, y),
                    ridge.get_pixel(x, y),
                    "thin bowstring altered at {x},{y}"
                );
            }
        }
    }
    for (x, y, p) in stairs.enumerate_pixels() {
        if p[3] == 0 {
            continue;
        }
        assert!(
            ((y as f64 * 6.25).floor() as u32..((y + 1) as f64 * 6.25).ceil() as u32).any(|sy| {
                ((x as f64 * 6.25).floor() as u32..((x + 1) as f64 * 6.25).ceil() as u32).any(
                    |sx| {
                        let original = source.get_pixel(sx, sy);
                        original.0[..3] == p.0[..3]
                            && (original[3] == p[3] || (p[3] == 255 && original[3] >= 128))
                    },
                )
            }),
            "pixel {x},{y} has no source-footprint colour provenance"
        );
    }
    let mut pair = RgbaImage::from_pixel(256, 128, Rgba([82, 82, 82, 255]));
    let mut cloak_pair = RgbaImage::from_pixel(70, 42, Rgba([82, 82, 82, 255]));
    for (i, panel) in [&ridge, &stairs].into_iter().enumerate() {
        image::imageops::overlay(&mut pair, panel, i as i64 * 128, 0);
        let crop = image::imageops::crop_imm(panel, 26, 44, 35, 42).to_image();
        image::imageops::overlay(&mut cloak_pair, &crop, i as i64 * 35, 0);
    }
    image::imageops::resize(&pair, 768, 384, image::imageops::FilterType::Nearest)
        .save(directory.join("before-after-3x.png"))
        .unwrap();
    image::imageops::resize(&cloak_pair, 560, 336, image::imageops::FilterType::Nearest)
        .save(directory.join("cloak-before-after-8x.png"))
        .unwrap();
    if let Ok(before) = std::env::var("DIORAMA_GAME_ASSET_CLEANUP_BEFORE") {
        let old = image::open(std::path::Path::new(&before).join("128-clean.png"))
            .unwrap()
            .to_rgba8();
        assert_eq!(old.dimensions(), stairs.dimensions());
        let mut pair = RgbaImage::from_pixel(256, 128, Rgba([82, 82, 82, 255]));
        let mut cloak_pair = RgbaImage::from_pixel(70, 42, Rgba([82, 82, 82, 255]));
        for (i, panel) in [&old, &stairs].into_iter().enumerate() {
            image::imageops::overlay(&mut pair, panel, i as i64 * 128, 0);
            let crop = image::imageops::crop_imm(panel, 26, 44, 35, 42).to_image();
            image::imageops::overlay(&mut cloak_pair, &crop, i as i64 * 35, 0);
        }
        image::imageops::resize(&pair, 768, 384, image::imageops::FilterType::Nearest)
            .save(directory.join("revision-before-after-3x.png"))
            .unwrap();
        image::imageops::resize(&cloak_pair, 560, 336, image::imageops::FilterType::Nearest)
            .save(directory.join("revision-cloak-before-after-8x.png"))
            .unwrap();
    }
    let panels = [
        ("ridge-2", ridge),
        ("crisp-edges", crisp),
        ("debleed", shades),
        ("clean", stairs),
    ];
    let mut sheet = RgbaImage::from_pixel(512, 128, Rgba([82, 82, 82, 255]));
    let mut cloaks = RgbaImage::from_pixel(35 * 4, 42, Rgba([82, 82, 82, 255]));
    for (i, (name, panel)) in panels.iter().enumerate() {
        panel
            .save(directory.join(format!("128-{name}.png")))
            .unwrap();
        let previous = &panels[i.saturating_sub(1)].1;
        let changed = previous
            .pixels()
            .zip(panel.pixels())
            .filter(|(a, b)| a != b)
            .count();
        let cloak_changes = (44..86)
            .flat_map(|y| (26..61).map(move |x| (x, y)))
            .filter(|(x, y)| previous.get_pixel(*x, *y) != panel.get_pixel(*x, *y))
            .count();
        eprintln!(
            "{name}: {changed} changed pixels vs previous step, {cloak_changes} in cloak crop"
        );
        image::imageops::overlay(&mut sheet, panel, i as i64 * 128, 0);
        let crop = image::imageops::crop_imm(panel, 26, 44, 35, 42).to_image();
        image::imageops::overlay(&mut cloaks, &crop, i as i64 * 35, 0);
    }
    image::imageops::resize(&sheet, 1536, 384, image::imageops::FilterType::Nearest)
        .save(directory.join("comparison-3x.png"))
        .unwrap();
    image::imageops::resize(&cloaks, 1120, 336, image::imageops::FilterType::Nearest)
        .save(directory.join("cloaks-8x.png"))
        .unwrap();
    eprintln!(
        "cloak cleanup: {} (ridge-2 / crisp edges / debleed / debleed + stairs)",
        directory.display()
    );
}

fn clean_edge_shades(source: &RgbaImage, baseline: &RgbaImage) -> RgbaImage {
    let mut output = baseline.clone();
    for y in 1..baseline.height().saturating_sub(1) {
        for x in 1..baseline.width().saturating_sub(1) {
            let centre = *output.get_pixel(x, y);
            if centre[3] < 250 || !near_outline(baseline, x, y) || !broad_support(baseline, x, y) {
                continue;
            }
            let mut contour = ContourChoice::new(&output, x, y);
            let prior_elbows = contour.remaining_elbows(centre);
            let mut best = centre;
            let mut best_score = (usize::MAX, f32::INFINITY);
            for (dx, dy) in [(1, 0), (0, 1), (1, 1), (1, -1)] {
                let a = neighbour(&output, x, y, dx, dy).unwrap();
                let b = neighbour(&output, x, y, -dx, -dy).unwrap();
                let contrast = distance(a, b);
                if a[3] != centre[3] || b[3] != centre[3] || contrast < 25.0 {
                    continue;
                }
                let (pa, pb, pc) = (appearance(a), appearance(b), appearance(centre));
                let denominator = (0..3).map(|c| (pb[c] - pa[c]).powi(2)).sum::<f32>();
                let t = (0..3)
                    .map(|c| (pc[c] - pa[c]) * (pb[c] - pa[c]))
                    .sum::<f32>()
                    / denominator;
                if !(0.05..=0.25).contains(&t) && !(0.75..=0.95).contains(&t) {
                    continue;
                }
                let residual = ((0..3)
                    .map(|c| (pc[c] - (pa[c] + t * (pb[c] - pa[c]))).powi(2))
                    .sum::<f32>()
                    / 3.0)
                    .sqrt();
                if residual > (contrast * 0.1).clamp(3.0, 10.0) {
                    continue;
                }
                for target in [a, b] {
                    let supported = (-1..=1)
                        .flat_map(|oy| (-1..=1).map(move |ox| (ox, oy)))
                        .filter(|(ox, oy)| {
                            (*ox != 0 || *oy != 0)
                                && neighbour(&output, x, y, *ox, *oy).is_some_and(|p| {
                                    distance(p, target) < (contrast * 0.15).clamp(3.0, 18.0)
                                })
                        })
                        .count();
                    if supported >= 3 {
                        let pixel = source_replacement(source, &output, x, y, target, true);
                        if pixel != centre {
                            let elbows = contour.remaining_elbows(pixel);
                            let further_side = distance(centre, target)
                                > distance(centre, a).min(distance(centre, b)) + 0.01;
                            if further_side
                                && (elbows >= prior_elbows || !simple_point(&output, x, y, pixel))
                            {
                                continue;
                            }
                            let change = distance(centre, pixel);
                            if elbows < best_score.0
                                || (elbows == best_score.0 && change < best_score.1)
                            {
                                best = pixel;
                                best_score = (elbows, change);
                            }
                        }
                    }
                }
            }
            output.put_pixel(x, y, best);
        }
    }
    output
}

fn new_elbows(before: &RgbaImage, after: &RgbaImage) -> Vec<(u32, u32)> {
    assert_eq!(before.dimensions(), after.dimensions());
    (0..before.height())
        .flat_map(|y| (0..before.width()).map(move |x| (x, y)))
        .filter(|(x, y)| {
            elbow_fill(before, *x, *y).is_none() && elbow_fill(after, *x, *y).is_some()
        })
        .collect()
}

#[test]
fn deblur_prefers_supported_fill_over_hardening_an_existing_elbow() {
    let black = Rgba([15, 20, 10, 255]);
    let white = Rgba([230, 230, 210, 255]);
    let mut image = RgbaImage::from_pixel(9, 9, white);
    for (x, y) in [
        (7, 1),
        (6, 1),
        (6, 2),
        (5, 2),
        (5, 3),
        (4, 3),
        (4, 4),
        (3, 4),
        (3, 5),
        (2, 5),
        (2, 6),
        (1, 6),
    ] {
        image.put_pixel(x, y, black);
    }
    image.put_pixel(4, 4, Rgba([45, 45, 45, 255]));
    image.put_pixel(6, 6, Rgba([0, 0, 0, 0]));
    let mut source = image::imageops::resize(&image, 27, 27, image::imageops::FilterType::Nearest);
    source.put_pixel(12, 12, black);
    source.put_pixel(13, 12, white);
    assert!(elbow_fill(&image, 4, 4).is_some());
    let output = clean_edge_shades(&source, &image);
    assert_eq!(
        *output.get_pixel(4, 4),
        white,
        "prefer a supported fill that removes the elbow over the closer contour colour"
    );
    assert!(new_elbows(&image, &output).is_empty());
}

#[test]
fn source_colour_choice_rejects_new_elbows_and_tries_another_sample() {
    let black = Rgba([15, 20, 10, 255]);
    let white = Rgba([230, 230, 210, 255]);
    let blurred = Rgba([100, 100, 100, 255]);
    let safe = Rgba([60, 60, 60, 255]);
    let mut image = RgbaImage::from_pixel(9, 9, white);
    for (x, y) in [
        (7, 1),
        (6, 1),
        (6, 2),
        (5, 2),
        (5, 3),
        (4, 3),
        (4, 4),
        (3, 4),
        (3, 5),
        (2, 5),
        (2, 6),
        (1, 6),
    ] {
        image.put_pixel(x, y, black);
    }
    image.put_pixel(4, 4, blurred);
    let mut source = image::imageops::resize(&image, 27, 27, image::imageops::FilterType::Nearest);
    source.put_pixel(12, 12, black);
    source.put_pixel(13, 12, safe);
    for _ in 0..4 {
        let mut choice = ContourChoice::new(&image, 4, 4);
        assert!(
            !choice.allows(black),
            "the closest colour recreates an elbow"
        );
        assert!(
            choice.allows(safe),
            "a less aggressive source sample keeps the contour clean"
        );
        assert!(
            choice.allows(blurred),
            "trial colours must not mutate the patch"
        );
        let pixel = source_replacement(&source, &image, 4, 4, black, true);
        assert_eq!(
            pixel, safe,
            "try the next safe source colour, not only reject the closest"
        );
        let mut output = image.clone();
        output.put_pixel(4, 4, pixel);
        assert!(new_elbows(&image, &output).is_empty());
        image = image::imageops::rotate90(&image);
        source = image::imageops::rotate90(&source);
    }
}

#[test]
#[ignore = "requires DIORAMA_GAME_ASSET_INPUT=.../elf2-se.png; regression for new cleanup staircases"]
fn elf_deblur_does_not_create_stairs() {
    let source = image::open(std::env::var("DIORAMA_GAME_ASSET_INPUT").unwrap())
        .unwrap()
        .to_rgba8();
    let ridge = super::ridge_samples(&source, 128, 30.0, 0.5);
    let crisp = clean_alpha(&ridge);
    let shades = clean_edge_shades(&source, &crisp);
    let introduced = new_elbows(&crisp, &shades);
    assert!(
        introduced.is_empty(),
        "deblurring introduced staircase elbows: {introduced:?}"
    );
    let stairs = clean_stairs(&source, &shades);
    let introduced = new_elbows(&shades, &stairs);
    assert!(
        introduced.is_empty(),
        "thinning relocated staircase elbows: {introduced:?}"
    );
}

#[test]
fn staircase_elbows_are_distinguished_from_real_right_angle_corners() {
    let white = Rgba([230, 230, 210, 255]);
    let black = Rgba([15, 20, 10, 255]);
    let mut steps = RgbaImage::from_pixel(9, 9, white);
    for (x, y) in [
        (7, 1),
        (6, 1),
        (6, 2),
        (5, 2),
        (5, 3),
        (4, 3),
        (4, 4),
        (3, 4),
        (3, 5),
        (2, 5),
        (2, 6),
        (1, 6),
    ] {
        steps.put_pixel(x, y, black);
    }
    assert_eq!(
        elbow_fill(&steps, 4, 4),
        Some(white),
        "redundant elbow along a diagonal should be cleaned"
    );
    let mut corner = RgbaImage::from_pixel(9, 9, white);
    for x in 2..7 {
        corner.put_pixel(x, 2, black);
    }
    for y in 2..7 {
        corner.put_pixel(2, y, black);
    }
    assert_eq!(
        elbow_fill(&corner, 2, 2),
        None,
        "a real right-angle corner is intentional"
    );
    let mut dot = RgbaImage::from_pixel(9, 9, white);
    dot.put_pixel(4, 4, black);
    assert_eq!(elbow_fill(&dot, 4, 4), None, "keep isolated details");
}

#[test]
fn fringe_cleanup_keeps_thin_rgba_features() {
    let mut thin = RgbaImage::new(12, 12);
    for p in 2..10 {
        thin.put_pixel(p, p, Rgba([30, 45, 15, 96]));
    }
    assert_eq!(clean_alpha(&thin), thin);
    let mut cloak = RgbaImage::new(12, 12);
    for y in 2..10 {
        for x in 4..10 {
            cloak.put_pixel(x, y, Rgba([30, 70, 15, 253]));
        }
        cloak.put_pixel(3, y, Rgba([5, 10, 2, 64]));
    }
    let crisp = clean_alpha(&cloak);
    assert_eq!(crisp.get_pixel(3, 5)[3], 0, "remove outer contour bleed");
    assert_eq!(crisp.get_pixel(4, 5)[3], 255, "keep an opaque contour");
    assert_eq!(
        crisp.get_pixel(7, 5),
        cloak.get_pixel(7, 5),
        "do not modify the interior"
    );
}

#[test]
fn cleanup_keeps_bridges_holes_and_small_images() {
    let black = Rgba([10, 10, 10, 255]);
    let white = Rgba([220, 220, 220, 255]);
    let mut bridge = RgbaImage::from_pixel(7, 7, white);
    for x in 1..6 {
        bridge.put_pixel(x, 3, black);
    }
    assert!(!simple_point(&bridge, 3, 3, white));
    let mut ring = RgbaImage::from_pixel(7, 7, white);
    for p in 2..5 {
        ring.put_pixel(2, p, black);
        ring.put_pixel(4, p, black);
        ring.put_pixel(p, 2, black);
        ring.put_pixel(p, 4, black);
    }
    assert!(!simple_point(&ring, 3, 2, white));
    for size in 1..=3 {
        let image = RgbaImage::from_pixel(size, size, black);
        assert_eq!(clean_stairs(&image, &image), image);
        assert_eq!(clean_edge_shades(&image, &image), image);
    }
}
