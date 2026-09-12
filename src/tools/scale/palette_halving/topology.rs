//! Conservative target-grid refinement, authorized only by original source evidence.
use super::{delta_e, lab};
use crate::document::CancellationToken;
use crate::error::Result;
use image::{Rgba, RgbaImage};

fn similar(a: Rgba<u8>, b: Rgba<u8>) -> bool {
    if a[3] < 64 || b[3] < 64 {
        return a[3] < 64 && b[3] < 64;
    }
    delta_e(lab(a), lab(b)) < 6.0
}

fn contrast(a: Rgba<u8>, b: Rgba<u8>) -> bool {
    (a[3] >= 64) != (b[3] >= 64) || a[3] >= 64 && delta_e(lab(a), lab(b)) >= 12.0
}

fn sample(image: &RgbaImage, x: i32, y: i32) -> Option<Rgba<u8>> {
    if x < 0 || y < 0 || x >= image.width() as i32 || y >= image.height() as i32 {
        None
    } else {
        Some(*image.get_pixel(x as u32, y as u32))
    }
}

/// Excess contour pixels beyond two per local 2×2 block (3/4 costs one,
/// 4/4 costs two, so completing an already thick block is penalized too). Filled regions are
/// protected separately by source support and the narrow-corner precondition.
fn thick(
    image: &RgbaImage,
    x: i32,
    y: i32,
    colour: Rgba<u8>,
    replacement: Option<Rgba<u8>>,
) -> usize {
    let mut total = 0;
    for sy in y - 1..=y {
        for sx in x - 1..=x {
            let mut count = 0;
            for dy in 0..2 {
                for dx in 0..2 {
                    let p = if (sx + dx, sy + dy) == (x, y) {
                        replacement.or_else(|| sample(image, x, y))
                    } else {
                        sample(image, sx + dx, sy + dy)
                    };
                    count += usize::from(p.is_some_and(|p| similar(p, colour)));
                }
            }
            total += count.saturating_sub(2);
        }
    }
    total
}

/// A removable corner must be a simple 8-connected point, never an endpoint
/// or a junction with disconnected arms once the centre is removed.
fn redundant(image: &RgbaImage, x: i32, y: i32, colour: Rgba<u8>) -> bool {
    let mut neighbours = Vec::new();
    for dy in -1..=1 {
        for dx in -1..=1 {
            if (dx, dy) != (0, 0)
                && sample(image, x + dx, y + dy).is_some_and(|p| similar(p, colour))
            {
                neighbours.push((dx, dy));
            }
        }
    }
    if !(2..=3).contains(&neighbours.len()) {
        return false;
    }
    let mut reached = vec![false; neighbours.len()];
    reached[0] = true;
    for _ in 0..neighbours.len() {
        for a in 0..neighbours.len() {
            for b in 0..neighbours.len() {
                if reached[a]
                    && (neighbours[a].0 - neighbours[b].0).abs() <= 1
                    && (neighbours[a].1 - neighbours[b].1).abs() <= 1
                {
                    reached[b] = true;
                }
            }
        }
    }
    reached.into_iter().all(|v| v)
}

/// Return an actual quantized source colour and its alpha-weighted cell support.
/// Gap evidence must also be near the segment through the target cell centre.
fn evidence(
    source: &RgbaImage,
    target: (u32, u32),
    x: u32,
    y: u32,
    colour: Rgba<u8>,
    direction: Option<(i32, i32)>,
) -> Option<(Rgba<u8>, f64)> {
    let (wx, wy) = (
        f64::from(source.width()) / f64::from(target.0),
        f64::from(source.height()) / f64::from(target.1),
    );
    let (left, top) = (f64::from(x) * wx, f64::from(y) * wy);
    let (right, bottom) = (left + wx, top + wy);
    let mut support = 0.0;
    let mut selected = None;
    let mut best = f64::INFINITY;
    for sy in top.floor() as u32..(bottom.ceil() as u32).min(source.height()) {
        for sx in left.floor() as u32..(right.ceil() as u32).min(source.width()) {
            let p = *source.get_pixel(sx, sy);
            if !similar(p, colour) {
                continue;
            }
            let (px, py) = (
                (f64::from(sx) + 0.5 - left) / wx - 0.5,
                (f64::from(sy) + 0.5 - top) / wy - 0.5,
            );
            if let Some((dx, dy)) = direction {
                let distance = (px * f64::from(dy) - py * f64::from(dx)).abs()
                    / f64::from(dx * dx + dy * dy).sqrt();
                if distance > 0.35 {
                    continue;
                }
            }
            let area = (right.min(f64::from(sx + 1)) - left.max(f64::from(sx)))
                * (bottom.min(f64::from(sy + 1)) - top.max(f64::from(sy)));
            support += area
                * if colour[3] < 64 {
                    1.0 - f64::from(p[3]) / 255.0
                } else {
                    f64::from(p[3]) / 255.0
                };
            let cost = px * px + py * py;
            if cost < best {
                best = cost;
                selected = Some(p);
            }
        }
    }
    selected.map(|p| (p, support / (wx * wy)))
}

pub(super) fn refine(
    source: &RgbaImage,
    output: &mut RgbaImage,
    cancel: &CancellationToken,
) -> Result<()> {
    cancel.check()?;
    let dims = output.dimensions();
    // Snapshot eligibility, then revalidate against committed neighbours. Edits
    // cannot trigger a cascade of newly eligible corners or gap extensions.
    let baseline = output.clone();
    let mut trimmed = 0;
    for y in 1..output.height().saturating_sub(1) {
        cancel.check()?;
        for x in 1..output.width().saturating_sub(1) {
            let (ix, iy) = (x as i32, y as i32);
            let current = *baseline.get_pixel(x, y);
            if current[3] < 64
                || !redundant(&baseline, ix, iy, current)
                || !redundant(output, ix, iy, current)
            {
                continue;
            }
            let old_penalty = thick(output, ix, iy, current, None);
            if old_penalty == 0 {
                continue;
            }
            let old_support = evidence(source, dims, x, y, current, None).map_or(0.0, |(_, w)| w);
            if old_support >= 0.45 {
                continue;
            }
            let mut best = None;
            for (dx, dy) in [(1, 1), (-1, 1), (1, -1), (-1, -1)] {
                let fill = *baseline.get_pixel((ix + dx) as u32, (iy + dy) as u32);
                if !contrast(current, fill) {
                    continue;
                }
                let Some((mut candidate, weight)) = evidence(source, dims, x, y, fill, None) else {
                    continue;
                };
                if weight < 0.4 || weight <= old_support {
                    continue;
                }
                if candidate[3] >= 64 {
                    candidate[3] = output.get_pixel(x, y)[3];
                }
                let after = thick(output, ix, iy, current, Some(candidate));
                let fill_neighbours = (-1..=1)
                    .flat_map(|dy| (-1..=1).map(move |dx| (dx, dy)))
                    .filter(|&(dx, dy)| {
                        (dx, dy) != (0, 0)
                            && sample(output, ix + dx, iy + dy)
                                .is_some_and(|p| similar(p, candidate))
                    })
                    .count();
                if after >= old_penalty
                    || fill_neighbours <= 4
                        && thick(output, ix, iy, candidate, Some(candidate))
                            > thick(output, ix, iy, candidate, None)
                {
                    continue;
                }
                if best.is_none_or(|(_, w)| weight > w) {
                    best = Some((candidate, weight));
                }
            }
            if let Some((candidate, _)) = best {
                output.put_pixel(x, y, candidate);
                trimmed += 1;
            }
        }
    }
    let baseline = output.clone();
    let mut bridged = 0;
    for y in 1..output.height().saturating_sub(1) {
        cancel.check()?;
        for x in 1..output.width().saturating_sub(1) {
            let (ix, iy) = (x as i32, y as i32);
            let current = *baseline.get_pixel(x, y);
            let mut best = None;
            for (dx, dy) in [(1, 0), (0, 1), (1, 1), (1, -1)] {
                let a = *baseline.get_pixel((ix - dx) as u32, (iy - dy) as u32);
                let b = *baseline.get_pixel((ix + dx) as u32, (iy + dy) as u32);
                if a[3] < 128 || b[3] < 128 || !similar(a, b) || !contrast(current, a) {
                    continue;
                }
                if !similar(*output.get_pixel((ix - dx) as u32, (iy - dy) as u32), a)
                    || !similar(*output.get_pixel((ix + dx) as u32, (iy + dy) as u32), b)
                {
                    continue;
                }
                // Broad fill on opposite sides of a line is not a broken
                // contour. Both endpoints must be narrow across the segment.
                let narrow_endpoints = [-1, 1].into_iter().all(|side| {
                    [-1, 1].into_iter().all(|flank| {
                        sample(
                            &baseline,
                            ix + side * dx - flank * dy,
                            iy + side * dy + flank * dx,
                        )
                        .is_some_and(|p| !similar(p, a))
                    })
                });
                if !narrow_endpoints {
                    continue;
                }
                let Some((candidate, weight)) = evidence(source, dims, x, y, a, Some((dx, dy)))
                else {
                    continue;
                };
                if weight < 0.02 || candidate[3] < 64 || !similar(candidate, b) {
                    continue;
                }
                if thick(output, ix, iy, candidate, Some(candidate))
                    > thick(output, ix, iy, candidate, None)
                {
                    continue;
                }
                if best.is_none_or(|(_, w)| weight > w) {
                    best = Some((candidate, weight));
                }
            }
            if let Some((candidate, _)) = best {
                output.put_pixel(x, y, candidate);
                bridged += 1;
            }
        }
    }
    cancel.check()?;
    tracing::debug!(
        trimmed,
        bridged,
        "Game Asset source-supported topology refinement"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const LINE: Rgba<u8> = Rgba([10, 10, 10, 255]);
    const FILL: Rgba<u8> = Rgba([200, 180, 100, 255]);

    #[test]
    fn rejects_off_segment_evidence_fattening_and_background_bridges() {
        for extra_neighbour in [false, true] {
            let mut output = RgbaImage::from_pixel(7, 7, FILL);
            output.put_pixel(2, 3, LINE);
            output.put_pixel(4, 3, LINE);
            if extra_neighbour {
                output.put_pixel(3, 2, LINE);
            }
            let mut source =
                image::imageops::resize(&output, 28, 28, image::imageops::FilterType::Nearest);
            source.put_pixel(13, if extra_neighbour { 13 } else { 15 }, LINE);
            refine(&source, &mut output, &CancellationToken::default()).unwrap();
            assert_eq!(*output.get_pixel(3, 3), FILL);
        }
        let mut output = RgbaImage::from_pixel(7, 7, FILL);
        output.put_pixel(3, 3, LINE);
        let source = RgbaImage::from_pixel(28, 28, FILL);
        refine(&source, &mut output, &CancellationToken::default()).unwrap();
        assert_eq!(*output.get_pixel(3, 3), LINE);
        let cancel = CancellationToken::default();
        cancel.cancel();
        let before = output.clone();
        assert!(refine(&source, &mut output, &cancel).is_err());
        assert_eq!(before, output);
    }

    #[test]
    fn bridges_supported_gaps_in_all_directions_but_not_empty_ones() {
        for (dx, dy) in [(1, 0), (0, 1), (1, 1), (1, -1)] {
            for supported in [false, true] {
                let mut output = RgbaImage::from_pixel(7, 7, FILL);
                output.put_pixel((3 - dx) as u32, (3 - dy) as u32, LINE);
                output.put_pixel((3 + dx) as u32, (3 + dy) as u32, LINE);
                let mut source =
                    image::imageops::resize(&output, 28, 28, image::imageops::FilterType::Nearest);
                if supported {
                    source.put_pixel(13, 13, LINE);
                }
                refine(&source, &mut output, &CancellationToken::default()).unwrap();
                assert_eq!(*output.get_pixel(3, 3), if supported { LINE } else { FILL });
            }
        }
    }

    #[test]
    fn trims_only_weak_redundant_corners() {
        for strong in [false, true] {
            let mut output = RgbaImage::from_pixel(7, 7, FILL);
            for (x, y) in [(3, 3), (3, 2), (2, 3)] {
                output.put_pixel(x, y, LINE);
            }
            let mut source =
                image::imageops::resize(&output, 28, 28, image::imageops::FilterType::Nearest);
            if !strong {
                for y in 12..16 {
                    for x in 12..16 {
                        source.put_pixel(x, y, FILL);
                    }
                }
                source.put_pixel(12, 12, LINE);
            }
            refine(&source, &mut output, &CancellationToken::default()).unwrap();
            assert_eq!(*output.get_pixel(3, 3), if strong { LINE } else { FILL });
            assert_eq!(*output.get_pixel(3, 2), LINE);
            assert_eq!(*output.get_pixel(2, 3), LINE);
        }
    }
}
