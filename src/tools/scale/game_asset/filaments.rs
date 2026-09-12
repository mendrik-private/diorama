//! Alpha-supported centre lines complement colour ridges. A narrow opaque
//! appendage is one feature even when its texture changes colour repeatedly.
use std::collections::{BTreeMap, BTreeSet};

use super::analysis::{Feature, Kind, neighbours};
use crate::document::CancellationToken;
use crate::error::Result;

/// Bounded Zhang–Suen thinning (CACM 1984, doi:10.1145/357994.358023).
/// We only need narrow appendages; thick interiors are deliberately left alone.
pub(super) fn trace(
    occupancy: &[bool],
    foreground: &[u32],
    width: usize,
    radius: usize,
    cancellation: &CancellationToken,
) -> Result<Vec<Feature>> {
    let radius = radius.clamp(1, u16::MAX as usize - 1);
    let height = occupancy.len() / width;
    if width < 3 || height < 3 || occupancy.iter().all(|v| *v) {
        return Ok(Vec::new());
    }
    let mut mask = occupancy.to_vec();
    let mut depth = vec![0_u16; mask.len()];
    let cap = radius.min(u16::MAX as usize - 1) as u16 + 1;
    for p in 0..mask.len() {
        if p % width == 0 {
            cancellation.check()?;
        }
        if !mask[p] {
            continue;
        }
        let (x, y) = (p % width, p / width);
        depth[p] = if x == 0 || y == 0 || x + 1 == width || y + 1 == height {
            1
        } else {
            [
                depth[p - 1],
                depth[p - width - 1],
                depth[p - width],
                depth[p - width + 1],
            ]
            .into_iter()
            .min()
            .unwrap()
            .saturating_add(1)
            .min(cap)
        };
    }
    for p in (0..mask.len()).rev() {
        if p % width == 0 {
            cancellation.check()?;
        }
        let (x, y) = (p % width, p / width);
        if mask[p] && x > 0 && y > 0 && x + 1 < width && y + 1 < height {
            depth[p] = depth[p].min(
                [
                    depth[p + 1],
                    depth[p + width - 1],
                    depth[p + width],
                    depth[p + width + 1],
                ]
                .into_iter()
                .min()
                .unwrap()
                .saturating_add(1),
            );
        }
    }
    // Restrict work to the original narrow boundary band. Batch deletions are
    // essential: decisions within a subiteration must see the same mask.
    let active: Vec<_> = (0..mask.len())
        .filter(|p| mask[*p] && depth[*p] <= radius as u16)
        .collect();
    let mut remove = Vec::new();
    for _ in 0..radius.saturating_mul(2).saturating_add(2) {
        let mut changed = false;
        for second in [false, true] {
            for (i, &p) in active.iter().enumerate() {
                if i % 4096 == 0 {
                    cancellation.check()?;
                }
                if !mask[p] {
                    continue;
                }
                let x = p % width;
                let y = p / width;
                let at = |dx: isize, dy: isize| {
                    let Some(nx) = x.checked_add_signed(dx) else {
                        return false;
                    };
                    let Some(ny) = y.checked_add_signed(dy) else {
                        return false;
                    };
                    nx < width && ny < height && mask[ny * width + nx]
                };
                let n = [
                    at(0, -1),
                    at(1, -1),
                    at(1, 0),
                    at(1, 1),
                    at(0, 1),
                    at(-1, 1),
                    at(-1, 0),
                    at(-1, -1),
                ];
                let count = n.iter().filter(|v| **v).count();
                let transitions = (0..8).filter(|i| !n[*i] && n[(*i + 1) % 8]).count();
                let blocked = if second {
                    n[0] && n[6] && (n[2] || n[4])
                } else {
                    n[2] && n[4] && (n[0] || n[6])
                };
                if (2..=6).contains(&count) && transitions == 1 && !blocked {
                    remove.push(p);
                }
            }
            changed |= !remove.is_empty();
            for p in remove.drain(..) {
                mask[p] = false;
            }
        }
        if !changed {
            break;
        }
    }
    let mut graph = BTreeMap::<usize, Vec<usize>>::new();
    for (i, &p) in active.iter().enumerate() {
        if i % 4096 == 0 {
            cancellation.check()?;
        }
        if !mask[p] {
            continue;
        }
        let adjacent: Vec<_> = neighbours(p, width, height, true)
            .filter(|q| {
                if !mask[*q] || depth[*q] > radius as u16 {
                    return false;
                }
                // Suppress redundant diagonal edges around a 4-connected corner.
                p % width == q % width
                    || p / width == q / width
                    || (!mask[(p / width) * width + q % width]
                        && !mask[(q / width) * width + p % width])
            })
            .collect();
        graph.insert(p, adjacent);
    }
    let mut visited = BTreeSet::new();
    let mut features = Vec::new();
    let starts = graph
        .keys()
        .filter(|p| graph[p].len() != 2)
        .chain(graph.keys().filter(|p| graph[p].len() == 2));
    for &p in starts {
        cancellation.check()?;
        for &q in &graph[&p] {
            if visited.contains(&(p.min(q), p.max(q))) {
                continue;
            }
            let mut path = vec![p];
            let (mut previous, mut current) = (p, q);
            loop {
                visited.insert((previous.min(current), previous.max(current)));
                path.push(current);
                if graph[&current].len() != 2 || current == p {
                    break;
                }
                let next = *graph[&current].iter().find(|q| **q != previous).unwrap();
                if visited.contains(&(current.min(next), current.max(next))) {
                    break;
                }
                previous = current;
                current = next;
            }
            // Reject short boundary spurs. These are occupancy features, not
            // instructions to turn every pointed corner into a new stroke.
            if path.len() < radius.max(1) * 4 {
                continue;
            }
            let half_width =
                path.iter().map(|p| f32::from(depth[*p])).sum::<f32>() / path.len() as f32;
            features.push(Feature {
                closed: path.first() == path.last(),
                path,
                kind: Kind::Silhouette,
                component: foreground[p],
                half_width: half_width.max(1.0),
                confidence: 1.0,
            });
        }
    }
    Ok(features)
}
