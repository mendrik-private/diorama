//! Production Game Asset hybrid: direct bicubic interiors and source-palette contour paths.
#[cfg(test)]
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use image::{Rgba, RgbaImage};
use palette::{FromColor, Lab, Srgb};

use super::source_palette::{Palette, delta_e};
use crate::document::CancellationToken;
use crate::document::Resampling;
use crate::error::{AppError, Result};

const MERGE_DELTA_E: f32 = 2.5;
mod contours;
#[cfg(test)]
mod topology;

fn lab(pixel: Rgba<u8>) -> [f32; 3] {
    let p: Lab = Lab::from_color(Srgb::new(
        pixel[0] as f32 / 255.0,
        pixel[1] as f32 / 255.0,
        pixel[2] as f32 / 255.0,
    ));
    [p.l, p.a, p.b]
}

struct Prepared {
    image: RgbaImage,
    colours: Vec<(Rgba<u8>, [f32; 3])>,
}

pub struct Session {
    source: Arc<RgbaImage>,
    prepared: Mutex<Option<Arc<Prepared>>>,
}

pub struct Report {
    pub colours: usize,
    pub stages: Vec<(u32, u32)>,
}

pub struct Output {
    pub image: RgbaImage,
    pub report: Report,
}

impl Session {
    pub fn new(source: Arc<RgbaImage>) -> Self {
        Self {
            source,
            prepared: Mutex::new(None),
        }
    }

    fn prepare(&self, cancellation: &CancellationToken) -> Result<Arc<Prepared>> {
        cancellation.check()?;
        if let Some(prepared) = self.prepared.lock().unwrap().as_ref() {
            return Ok(prepared.clone());
        }
        // Work outside the lock: superseded preview workers remain cancellable.
        let mut labs = Vec::with_capacity(self.source.as_raw().len() / 4);
        let mut occupancy = Vec::with_capacity(labs.capacity());
        for row in self.source.rows() {
            cancellation.check()?;
            for pixel in row {
                labs.push(lab(*pixel));
                occupancy.push(pixel[3] != 0);
            }
        }
        let palette = Palette::new(&self.source, &labs, &occupancy, MERGE_DELTA_E, cancellation)?;
        let colours = palette
            .representatives
            .iter()
            .map(|&q| {
                (
                    Rgba(self.source.as_raw()[q * 4..q * 4 + 4].try_into().unwrap()),
                    labs[q],
                )
            })
            .collect();
        let mut image = self.source.as_ref().clone();
        for (q, pixel) in image.pixels_mut().enumerate() {
            if q % self.source.width() as usize == 0 {
                cancellation.check()?;
            }
            let p = palette.source(q) * 4;
            pixel.0[..3].copy_from_slice(&self.source.as_raw()[p..p + 3]);
        }
        cancellation.check()?;
        let prepared = Arc::new(Prepared { image, colours });
        *self.prepared.lock().unwrap() = Some(prepared.clone());
        Ok(prepared)
    }

    pub fn resize(
        &self,
        width: u32,
        height: u32,
        binary_alpha: bool,
        cancellation: &CancellationToken,
    ) -> Result<Output> {
        cancellation.check()?;
        if width == 0 || height == 0 || width > self.source.width() || height > self.source.height()
        {
            return Err(AppError::InvalidDimensions);
        }
        if self.source.dimensions() == (width, height) {
            return Ok(Output {
                image: self.source.as_ref().clone(),
                report: Report {
                    colours: 0,
                    stages: vec![(width, height)],
                },
            });
        }
        let prepared = self.prepare(cancellation)?;
        // Preserve original interior colours and shading; quantization is only
        // evidence/ink for the contour overlay, never the bicubic input.
        let mut image = super::resize(
            &self.source,
            width,
            height,
            Resampling::Bicubic,
            cancellation,
        )?;
        let report = Report {
            colours: prepared.colours.len(),
            stages: vec![self.source.dimensions(), (width, height)],
        };
        contours::refine(&prepared.image, &mut image, cancellation)?;
        if binary_alpha {
            for row in image.rows_mut() {
                cancellation.check()?;
                for pixel in row {
                    pixel[3] = if pixel[3] >= 128 { 255 } else { 0 };
                }
            }
        }
        cancellation.check()?;
        tracing::info!(palette_colours=report.colours, stages=?report.stages, binary_alpha, "Game Asset bicubic-and-contour hybrid complete");
        Ok(Output { image, report })
    }
}

/// Require two-sided continuation and two unlike flanks. A silhouette edge
/// (one transparent flank, one opaque flank) is not a free-standing filament.
#[cfg(test)]
fn thin_line(source: &RgbaImage, labs: &[[f32; 3]], x: u32, y: u32) -> bool {
    let pixel = source.get_pixel(x, y);
    if pixel[3] < 64 {
        return false;
    }
    let centre = labs[(y * source.width() + x) as usize];
    let sample = |dx: i32, dy: i32| {
        let nx = x.checked_add_signed(dx)?;
        let ny = y.checked_add_signed(dy)?;
        if nx >= source.width() || ny >= source.height() {
            return None;
        }
        Some((
            *source.get_pixel(nx, ny),
            labs[(ny * source.width() + nx) as usize],
        ))
    };
    let matches = |p: (Rgba<u8>, [f32; 3])| {
        u16::from(p.0[3]) * 2 >= u16::from(pixel[3]) && delta_e(centre, p.1) < 8.0
    };
    [
        (1, 0),
        (0, 1),
        (1, 1),
        (1, -1),
        (2, 1),
        (1, 2),
        (2, -1),
        (1, -2),
    ]
    .into_iter()
    .any(|(dx, dy)| {
        let (Some(a), Some(b), Some(left), Some(right)) = (
            sample(dx, dy),
            sample(-dx, -dy),
            sample(-dy, dx),
            sample(dy, -dx),
        ) else {
            return false;
        };
        matches(a)
            && matches(b)
            && !matches(left)
            && !matches(right)
            && (left.0[3] >= 128) == (right.0[3] >= 128)
    })
}

#[cfg(test)]
fn half(source: &RgbaImage, cancellation: &CancellationToken) -> Result<RgbaImage> {
    let mut labs = Vec::with_capacity(source.as_raw().len() / 4);
    for row in source.rows() {
        cancellation.check()?;
        labs.extend(row.map(|p| lab(*p)));
    }
    let mut output = RgbaImage::new(source.width() / 2, source.height() / 2);
    for y in 0..output.height() {
        cancellation.check()?;
        for x in 0..output.width() {
            let (sx, sy) = (x * 2, y * 2);
            // Normal centre-aligned NN first, for deterministic ties.
            let candidates = [(sx + 1, sy + 1), (sx, sy + 1), (sx + 1, sy), (sx, sy)];
            let alpha_sum: u32 = candidates
                .iter()
                .map(|&(cx, cy)| u32::from(source.get_pixel(cx, cy)[3]))
                .sum();
            let coverage_alpha = ((alpha_sum + 2) / 4) as u8;
            if alpha_sum == 0 {
                output.put_pixel(x, y, *source.get_pixel(sx + 1, sy + 1));
                continue;
            }
            let mut best = f32::NEG_INFINITY;
            let mut selected = *source.get_pixel(sx + 1, sy + 1);
            let mut selected_line = false;
            for (cx, cy) in candidates {
                let pixel = *source.get_pixel(cx, cy);
                if pixel[3] == 0 && alpha_sum != 0 {
                    continue;
                }
                let a = labs[(cy * source.width() + cx) as usize];
                let mut score = 0.0;
                let mut weight = 0.0;
                for ny in sy.saturating_sub(1)..(sy + 3).min(source.height()) {
                    for nx in sx.saturating_sub(1)..(sx + 3).min(source.width()) {
                        let alpha = source.get_pixel(nx, ny)[3] as f32 / 255.0;
                        if alpha > 0.0 {
                            score += alpha * delta_e(a, labs[(ny * source.width() + nx) as usize]);
                            weight += alpha;
                        }
                    }
                }
                let contrast = score / weight.max(1e-6);
                // A weighted colour medoid represents the block without mixing
                // RGB. Contrast is a tie-break influence, not a winner-takes-all
                // permission for one contour corner to consume an entire cell.
                let block_cost: f32 = candidates
                    .iter()
                    .map(|&(nx, ny)| {
                        let p = source.get_pixel(nx, ny);
                        if p[3] == 0 {
                            return 0.0;
                        }
                        f32::from(p[3]) * delta_e(a, labs[(ny * source.width() + nx) as usize])
                    })
                    .sum::<f32>()
                    / (alpha_sum as f32).max(1.0);
                let line = (contrast > 8.0 || coverage_alpha < pixel[3])
                    && thin_line(source, &labs, cx, cy);
                score = if line {
                    contrast
                } else {
                    contrast * 0.2 - block_cost
                };
                if score > best {
                    best = score;
                    selected = pixel;
                    selected_line = line;
                }
            }
            // Candidates already belong to the quantized palette: nearest ΔE=0.
            if !selected_line {
                selected[3] = coverage_alpha;
            }
            output.put_pixel(x, y, selected);
        }
    }
    Ok(output)
}

/// Area-weighted palette medoid: alpha coverage may blend, RGB never does.
#[cfg(test)]
fn final_grid(
    source: &RgbaImage,
    width: u32,
    height: u32,
    cancellation: &CancellationToken,
) -> Result<RgbaImage> {
    cancellation.check()?;
    if width == 0 || height == 0 || width > source.width() || height > source.height() {
        return Err(AppError::InvalidDimensions);
    }
    let mut labs = Vec::with_capacity(source.as_raw().len() / 4);
    for row in source.rows() {
        cancellation.check()?;
        labs.extend(row.map(|p| lab(*p)));
    }
    let mut output = RgbaImage::new(width, height);
    let scale_x = f64::from(source.width()) / f64::from(width);
    let scale_y = f64::from(source.height()) / f64::from(height);
    // Group identical RGB so support is colour coverage, not pixel frequency.
    let mut groups = HashMap::<[u8; 3], (u32, u32, f64)>::new();
    for y in 0..height {
        cancellation.check()?;
        let (top, bottom) = (f64::from(y) * scale_y, f64::from(y + 1) * scale_y);
        for x in 0..width {
            groups.clear();
            let (left, right) = (f64::from(x) * scale_x, f64::from(x + 1) * scale_x);
            let (mx, my) = ((left + right) * 0.5, (top + bottom) * 0.5);
            let mut mass = 0.0;
            for sy in top.floor() as u32..(bottom.ceil() as u32).min(source.height()) {
                cancellation.check()?;
                let overlap_y = bottom.min(f64::from(sy + 1)) - top.max(f64::from(sy));
                for sx in left.floor() as u32..(right.ceil() as u32).min(source.width()) {
                    let p = source.get_pixel(sx, sy);
                    if p[3] == 0 {
                        continue;
                    }
                    let overlap_x = right.min(f64::from(sx + 1)) - left.max(f64::from(sx));
                    let weight = overlap_x * overlap_y * f64::from(p[3]);
                    if weight <= 0.0 {
                        continue;
                    }
                    mass += weight;
                    let group = groups.entry([p[0], p[1], p[2]]).or_insert((sx, sy, 0.0));
                    group.2 += weight;
                    let distance = |px: u32, py: u32| {
                        (f64::from(px) + 0.5 - mx).powi(2) + (f64::from(py) + 0.5 - my).powi(2)
                    };
                    if distance(sx, sy) < distance(group.0, group.1) {
                        group.0 = sx;
                        group.1 = sy;
                    }
                }
            }
            if mass == 0.0 {
                continue;
            }
            let coverage = (mass / (scale_x * scale_y)).round().clamp(0.0, 255.0) as u8;
            let mut colours: Vec<_> = groups.values().copied().collect();
            // Stable ties and bounded candidate work for extreme/odd reductions.
            colours.sort_unstable_by(|a, b| {
                b.2.total_cmp(&a.2).then(a.1.cmp(&b.1)).then(a.0.cmp(&b.0))
            });
            let mut best = f32::NEG_INFINITY;
            let mut chosen = Rgba([0, 0, 0, 0]);
            for &(sx, sy, _) in colours.iter().take(16) {
                let p = *source.get_pixel(sx, sy);
                let a = labs[(sy * source.width() + sx) as usize];
                let mut cost = 0.0;
                for &(nx, ny, w) in &colours {
                    cost += w as f32 * delta_e(a, labs[(ny * source.width() + nx) as usize]);
                }
                cost /= mass as f32;
                let (cx, cy) = (mx.floor() as u32, my.floor() as u32);
                let mut contrast = 0.0;
                let mut context_mass = 0.0;
                for ny in cy.saturating_sub(2)..cy.saturating_add(3).min(source.height()) {
                    for nx in cx.saturating_sub(2)..cx.saturating_add(3).min(source.width()) {
                        let alpha = f32::from(source.get_pixel(nx, ny)[3]);
                        if alpha > 0.0 {
                            contrast +=
                                alpha * delta_e(a, labs[(ny * source.width() + nx) as usize]);
                            context_mass += alpha;
                        }
                    }
                }
                contrast /= context_mass.max(1.0);
                let owns_sample = f64::from(sx) + 0.5 >= left
                    && f64::from(sx) + 0.5 < right
                    && f64::from(sy) + 0.5 >= top
                    && f64::from(sy) + 0.5 < bottom;
                let line = owns_sample
                    && (contrast > 8.0 || coverage < p[3])
                    && thin_line(source, &labs, sx, sy);
                let score = if line {
                    contrast
                } else {
                    0.2 * contrast - cost
                };
                if score > best {
                    best = score;
                    chosen = p;
                    if !line {
                        chosen[3] = coverage;
                    }
                }
            }
            output.put_pixel(x, y, chosen);
        }
    }
    cancellation.check()?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hybrid_unoutlined_image_matches_direct_bicubic() {
        let source =
            RgbaImage::from_fn(64, 64, |x, y| Rgba([80 + x as u8, 100 + y as u8, 40, 255]));
        let cancel = CancellationToken::default();
        for size in [16, 19] {
            let expected =
                super::super::resize(&source, size, size, Resampling::Bicubic, &cancel).unwrap();
            let actual =
                super::super::resize(&source, size, size, Resampling::GameAsset, &cancel).unwrap();
            assert_eq!(
                actual, expected,
                "unoutlined interior must remain byte-identical to direct bicubic"
            );
        }
    }

    #[test]
    fn hybrid_repaints_source_ink_but_preserves_interior_texture() {
        let source = RgbaImage::from_fn(128, 128, |x, y| {
            if !(16..112).contains(&x) || !(16..112).contains(&y) {
                Rgba([0, 0, 0, 0])
            } else if !(20..108).contains(&x) || !(20..108).contains(&y) {
                Rgba([5, 8, 3, 255])
            } else {
                Rgba([
                    80 + (x / 2) as u8,
                    120 + (y / 4) as u8,
                    30 + ((x + y) % 12) as u8,
                    255,
                ])
            }
        });
        let session = Session::new(Arc::new(source.clone()));
        let cancel = CancellationToken::default();
        let expected = super::super::resize(&source, 32, 32, Resampling::Bicubic, &cancel).unwrap();
        let result = session.resize(32, 32, false, &cancel).unwrap();
        assert_ne!(
            result.image, expected,
            "the outline should actually be repainted"
        );
        for y in 8..24 {
            for x in 8..24 {
                assert_eq!(result.image.get_pixel(x, y), expected.get_pixel(x, y));
            }
        }
        let palette = session.prepare(&cancel).unwrap();
        for (a, b) in expected.pixels().zip(result.image.pixels()) {
            if a != b && b[3] > 0 {
                assert!(palette.colours.iter().any(|(p, _)| p.0[..3] == b.0[..3]));
            }
        }
    }

    #[test]
    fn final_grid_never_blends_palette_rgb() {
        let source = RgbaImage::from_fn(16, 16, |x, y| {
            if (x / 3 + y / 3) % 2 == 0 {
                Rgba([10, 20, 30, 255])
            } else {
                Rgba([220, 180, 80, 255])
            }
        });
        let out = final_grid(&source, 5, 5, &CancellationToken::default()).unwrap();
        assert!(
            out.pixels().all(|p| source.pixels().any(|s| s == p)),
            "final reduction introduced blended colours"
        );
    }

    #[test]
    fn final_grid_preserves_area_and_owned_thin_diagonal() {
        let cancel = CancellationToken::default();
        let source = RgbaImage::from_fn(19, 13, |x, _| {
            Rgba([20, 80, 40, if x < 7 { 255 } else { 0 }])
        });
        let output = final_grid(&source, 7, 5, &cancel).unwrap();
        let expected_alpha = 255.0 * 7.0 / 19.0;
        let actual_alpha = output.pixels().map(|p| f64::from(p[3])).sum::<f64>() / 35.0;
        assert!((actual_alpha - expected_alpha).abs() < 0.5);
        assert!(
            output
                .pixels()
                .filter(|p| p[3] > 0)
                .all(|p| p.0[..3] == [20, 80, 40])
        );
        let diagonal = RgbaImage::from_fn(32, 32, |x, y| {
            Rgba([10, 10, 10, if x == y { 255 } else { 0 }])
        });
        let output = final_grid(&diagonal, 13, 13, &cancel).unwrap();
        for i in 1..12 {
            assert_eq!(output.get_pixel(i, i)[3], 255);
        }
        for (x, y, p) in output.enumerate_pixels() {
            if x != y {
                assert!(p[3] < 128);
            }
        }
        assert!(final_grid(&source, 0, 1, &cancel).is_err());
        cancel.cancel();
        assert!(matches!(
            final_grid(&source, 7, 5, &cancel),
            Err(AppError::Cancelled)
        ));
    }

    #[test]
    fn coverage_does_not_promote_contour_corners() {
        let cancel = CancellationToken::default();
        let input = RgbaImage::from_fn(16, 16, |x, y| {
            if x + y < 15 {
                Rgba([10, 10, 10, 255])
            } else {
                Rgba([180, 180, 180, 255])
            }
        });
        let output = half(&input, &cancel).unwrap();
        // A broad diagonal boundary is not a thin line: minority corners
        // must not turn a predominantly light cell fully dark.
        for (x, y, p) in output.enumerate_pixels() {
            let count = (0..2)
                .flat_map(|dy| (0..2).map(move |dx| (dx, dy)))
                .filter(|&(dx, dy)| input.get_pixel(2 * x + dx, 2 * y + dy)[0] == 10)
                .count();
            if count == 1 {
                assert_eq!(p[0], 180);
            }
        }
        let alpha = RgbaImage::from_fn(8, 8, |x, y| {
            Rgba([80, 140, 40, if x + y < 7 { 255 } else { 0 }])
        });
        let reduced = half(&alpha, &cancel).unwrap();
        let before: u32 = alpha.pixels().map(|p| u32::from(p[3])).sum();
        let after: u32 = reduced.pixels().map(|p| u32::from(p[3])).sum();
        assert!(before.abs_diff(after * 4) <= 16);
    }

    #[test]
    fn coverage_preserves_connected_thin_lines() {
        for transparent in [false, true] {
            let source = RgbaImage::from_fn(32, 32, |x, y| {
                if x == y {
                    Rgba([10, 10, 10, 255])
                } else {
                    Rgba([220, 220, 220, if transparent { 0 } else { 255 }])
                }
            });
            let first = half(&source, &CancellationToken::default()).unwrap();
            let second = half(&first, &CancellationToken::default()).unwrap();
            for i in 1..7 {
                assert_eq!(*second.get_pixel(i, i), Rgba([10, 10, 10, 255]));
            }
        }
    }

    #[test]
    fn palette_is_pairwise_bounded_and_keeps_isolates_and_alpha() {
        let source = RgbaImage::from_fn(32, 8, |x, y| {
            if x == 31 && y == 7 {
                Rgba([240, 0, 200, 201])
            } else {
                Rgba([80 + x as u8, 140, 40, 100 + y as u8])
            }
        });
        let cancel = CancellationToken::default();
        let session = Session::new(Arc::new(source.clone()));
        let prepared = session.prepare(&cancel).unwrap();
        assert_eq!(prepared.image.get_pixel(31, 7), source.get_pixel(31, 7));
        for (original, quantized) in source.pixels().zip(prepared.image.pixels()) {
            assert_eq!(original[3], quantized[3]);
            assert!(delta_e(lab(*original), lab(*quantized)) < MERGE_DELTA_E);
            assert!(source.pixels().any(|p| p.0[..3] == quantized.0[..3]));
        }
        for (i, a) in prepared.image.pixels().enumerate() {
            for (j, b) in prepared.image.pixels().enumerate() {
                if a.0[..3] == b.0[..3] {
                    assert!(
                        delta_e(
                            lab(source.pixels().nth(i).copied().unwrap()),
                            lab(source.pixels().nth(j).copied().unwrap())
                        ) < MERGE_DELTA_E
                    );
                }
            }
        }
        assert!(Arc::ptr_eq(&prepared, &session.prepare(&cancel).unwrap()));
    }

    #[test]
    fn halves_do_not_inflate_isolates_and_finish_with_palette_grid() {
        let cancel = CancellationToken::default();
        for (fill, accent) in [
            ([220, 220, 220, 255], [10, 10, 10, 255]),
            ([10, 10, 10, 255], [240, 240, 240, 255]),
            ([80, 140, 40, 255], [240, 0, 200, 255]),
            ([0, 255, 0, 0], [30, 40, 50, 180]),
        ] {
            for (x, y) in [(2, 2), (3, 2), (2, 3), (3, 3)] {
                let mut source = RgbaImage::from_pixel(8, 8, Rgba(fill));
                source.put_pixel(x, y, Rgba(accent));
                assert_eq!(
                    *half(&source, &cancel).unwrap().get_pixel(1, 1),
                    if fill[3] == 0 {
                        Rgba([accent[0], accent[1], accent[2], 45])
                    } else {
                        Rgba(fill)
                    }
                );
            }
        }
        let source = RgbaImage::from_fn(16, 12, |x, y| {
            Rgba([(x * 13) as u8, (y * 17) as u8, 40, ((x + y) * 9) as u8])
        });
        let session = Session::new(Arc::new(source.clone()));
        let prepared = session.prepare(&cancel).unwrap();
        let mut expected =
            super::super::resize(&source, 7, 5, Resampling::Bicubic, &cancel).unwrap();
        contours::refine(&prepared.image, &mut expected, &cancel).unwrap();
        assert_eq!(
            session.resize(7, 5, false, &cancel).unwrap().image,
            expected
        );
        assert_eq!(
            session.resize(16, 12, false, &cancel).unwrap().image,
            source
        );
        for (w, h) in [(0, 1), (1, 0), (17, 12), (16, 13)] {
            assert!(session.resize(w, h, false, &cancel).is_err());
        }
        let odd = Session::new(Arc::new(RgbaImage::new(9, 7)))
            .resize(2, 2, false, &cancel)
            .unwrap();
        assert_eq!(odd.report.stages, [(9, 7), (2, 2)]);
        cancel.cancel();
        assert!(matches!(
            session.resize(7, 5, false, &cancel),
            Err(AppError::Cancelled)
        ));
    }

    #[test]
    fn binary_alpha_changes_only_alpha() {
        let source = RgbaImage::from_fn(16, 16, |x, y| Rgba([80, 140, 40, ((x + y) * 8) as u8]));
        let session = Session::new(Arc::new(source));
        let cancel = CancellationToken::default();
        let coverage = session.resize(5, 5, false, &cancel).unwrap().image;
        let binary = session.resize(5, 5, true, &cancel).unwrap().image;
        for (a, b) in coverage.pixels().zip(binary.pixels()) {
            assert_eq!(a.0[..3], b.0[..3]);
            assert_eq!(b[3], if a[3] >= 128 { 255 } else { 0 });
        }
    }

    #[test]
    #[ignore = "requires DIORAMA_GAME_ASSET_INPUT; writes coverage/binary alpha visual comparisons"]
    fn elf_hybrid_comparison() {
        let source = image::open(std::env::var("DIORAMA_GAME_ASSET_INPUT").unwrap())
            .unwrap()
            .to_rgba8();
        let directory = tempfile::Builder::new()
            .prefix("diorama-hybrid-")
            .tempdir()
            .unwrap()
            .keep();
        eprintln!("Comparison directory: {}", directory.display());
        let cancel = CancellationToken::default();
        let session = Session::new(Arc::new(source.clone()));
        let start = std::time::Instant::now();
        let prepared = session.prepare(&cancel).unwrap();
        eprintln!(
            "Palette: {} colours, {:?}",
            prepared.colours.len(),
            start.elapsed()
        );
        prepared
            .image
            .save(directory.join("800-quantized.png"))
            .unwrap();
        for size in [128, 160] {
            let start = std::time::Instant::now();
            let output = session.resize(size, size, false, &cancel).unwrap();
            eprintln!("{size}: {:?}, {:?}", output.report.stages, start.elapsed());
            assert_eq!(output.report.stages, [(800, 800), (size, size)]);
            let bicubic =
                super::super::resize(&source, size, size, Resampling::Bicubic, &cancel).unwrap();
            bicubic
                .save(directory.join(format!("{size}-bicubic.png")))
                .unwrap();
            output
                .image
                .save(directory.join(format!("{size}-coverage.png")))
                .unwrap();
            if let Ok(baseline) = std::env::var("DIORAMA_COMPARE_BASELINE") {
                let old = image::open(
                    std::path::Path::new(&baseline).join(format!("{size}-coverage.png")),
                )
                .unwrap()
                .to_rgba8();
                assert_eq!(old.dimensions(), output.image.dimensions());
                let mut methods = RgbaImage::from_pixel(size * 3, size, Rgba([82, 82, 82, 255]));
                for (i, panel) in [&old, &bicubic, &output.image].into_iter().enumerate() {
                    image::imageops::overlay(&mut methods, panel, i as i64 * i64::from(size), 0);
                }
                image::imageops::resize(
                    &methods,
                    size * 9,
                    size * 3,
                    image::imageops::FilterType::Nearest,
                )
                .save(directory.join(format!("{size}-methods-3x.png")))
                .unwrap();
                if size == 128 {
                    let mut cloak = RgbaImage::from_pixel(64, 50, Rgba([82, 82, 82, 255]));
                    for (i, panel) in [&old, &output.image].into_iter().enumerate() {
                        let crop = image::imageops::crop_imm(panel, 26, 42, 32, 50).to_image();
                        image::imageops::overlay(&mut cloak, &crop, i as i64 * 32, 0);
                    }
                    image::imageops::resize(&cloak, 512, 400, image::imageops::FilterType::Nearest)
                        .save(directory.join("128-cloak-before-after-8x.png"))
                        .unwrap();
                }
                let mut sheet = RgbaImage::from_pixel(size * 2, size, Rgba([82, 82, 82, 255]));
                image::imageops::overlay(&mut sheet, &old, 0, 0);
                image::imageops::overlay(&mut sheet, &output.image, i64::from(size), 0);
                image::imageops::resize(
                    &sheet,
                    size * 6,
                    size * 3,
                    image::imageops::FilterType::Nearest,
                )
                .save(directory.join(format!("{size}-before-after-3x.png")))
                .unwrap();
            }
            let mut binary = output.image.clone();
            for pixel in binary.pixels_mut() {
                pixel[3] = if pixel[3] >= 128 { 255 } else { 0 };
            }
            binary
                .save(directory.join(format!("{size}-binary.png")))
                .unwrap();
            let mut sheet = RgbaImage::from_pixel(size * 2, size, Rgba([82, 82, 82, 255]));
            let mut heads = RgbaImage::from_pixel(56, 30, Rgba([82, 82, 82, 255]));
            for (i, panel) in [&output.image, &binary].into_iter().enumerate() {
                image::imageops::overlay(&mut sheet, panel, i as i64 * size as i64, 0);
                if size == 128 {
                    image::imageops::overlay(
                        &mut heads,
                        &image::imageops::crop_imm(panel, 50, 16, 28, 30).to_image(),
                        i as i64 * 28,
                        0,
                    );
                }
            }
            image::imageops::resize(
                &sheet,
                size * 6,
                size * 3,
                image::imageops::FilterType::Nearest,
            )
            .save(directory.join(format!("{size}-comparison-3x.png")))
            .unwrap();
            if size == 128 {
                image::imageops::resize(&heads, 448, 240, image::imageops::FilterType::Nearest)
                    .save(directory.join("128-heads-8x.png"))
                    .unwrap();
            }
        }
    }

    #[test]
    fn production_dispatch_matches_palette_halving() {
        let source = RgbaImage::from_fn(16, 16, |x, y| {
            Rgba([(x * 16) as u8, (y * 16) as u8, 40, 255])
        });
        let cancel = CancellationToken::default();
        let expected = Session::new(Arc::new(source.clone()))
            .resize(5, 5, false, &cancel)
            .unwrap();
        assert_eq!(expected.report.stages, [(16, 16), (5, 5)]);
        assert_eq!(
            super::super::resize(&source, 5, 5, Resampling::GameAsset, &cancel).unwrap(),
            expected.image
        );
    }
}
