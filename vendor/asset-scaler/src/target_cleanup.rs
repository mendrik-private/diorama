//! Canonicalize adjacent foreground contour cores at target resolution.

use crate::{Cancellation, Error, Result, cleanup, raster::Mask};
use std::{
    cmp::Reverse,
    collections::{BinaryHeap, VecDeque},
};

/// Thin the complete target contour union, preferring the edge nearest the
/// unsupported foreground or virtual exterior. This never creates pixels.
pub(crate) fn thin_outer(
    raw: &Mask,
    foreground_support: &[bool],
    cancel: &dyn Cancellation,
) -> Result<Mask> {
    cancel.check()?;
    let Some(len) = raw.w.checked_mul(raw.h) else {
        return Err(Error::Scaling(
            "Invalid target contour cleanup inputs".into(),
        ));
    };
    if raw.data.len() != len || foreground_support.len() != len {
        return Err(Error::Scaling(
            "Invalid target contour cleanup inputs".into(),
        ));
    }
    if len == 0 {
        return Ok(raw.clone());
    }

    // Each pixel starts at its distance to the virtual exterior. Unsupported
    // pixels then relax the field to zero, so a nearby real exterior beats a
    // farther image edge while full support remains finite.
    let mut distance = vec![0; len];
    let mut queue = VecDeque::new();
    for y in 0..raw.h {
        for x in 0..raw.w {
            let i = y * raw.w + x;
            if i.is_multiple_of(4096) {
                cancel.check()?;
            }
            distance[i] = (x + 1).min(y + 1).min(raw.w - x).min(raw.h - y);
            if !foreground_support[i] {
                distance[i] = 0;
                queue.push_back(i);
            }
        }
    }
    while let Some(i) = queue.pop_front() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        let x = i % raw.w;
        let y = i / raw.w;
        for (dx, dy) in [(1isize, 0isize), (-1, 0), (0, 1), (0, -1)] {
            let xx = x as isize + dx;
            let yy = y as isize + dy;
            if xx < 0 || yy < 0 || xx >= raw.w as isize || yy >= raw.h as isize {
                continue;
            }
            let j = yy as usize * raw.w + xx as usize;
            let next = distance[i] + 1;
            if next < distance[j] {
                distance[j] = next;
                queue.push_back(j);
            }
        }
    }
    let priority: Vec<_> = distance.iter().map(|&value| value as f64).collect();
    let thinned = cleanup::thin(raw, &priority, cancel)?;
    let mut result = thinned;
    remove_short_terminal_branches(&mut result, cancel)?;
    remove_redundant_thickness(&mut result, &distance, cancel)?;
    move_endpoints_outward(&mut result, raw, &priority, cancel)?;
    remove_short_terminal_branches(&mut result, cancel)?;
    remove_redundant_thickness(&mut result, &distance, cancel)?;
    remove_short_components(&mut result, cancel)?;
    cancel.check()?;
    Ok(result)
}

const N8: [(isize, isize); 8] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];
const N4: [(isize, isize); 4] = [(0, -1), (-1, 0), (1, 0), (0, 1)];

fn neighbors8(mask: &Mask, i: usize) -> Vec<usize> {
    let (x, y) = (i % mask.w, i / mask.w);
    N8.iter()
        .filter_map(|&(dx, dy)| {
            let (xx, yy) = (x as isize + dx, y as isize + dy);
            (xx >= 0 && yy >= 0 && xx < mask.w as isize && yy < mask.h as isize)
                .then(|| yy as usize * mask.w + xx as usize)
        })
        .collect()
}

/// Follow cardinal edges first. A diagonal is an edge only when neither
/// cardinal bridge exists, so a terminal spur beside a vertical or horizontal
/// path does not acquire artificial diagonal junctions.
fn trace_neighbors(mask: &Mask, i: usize) -> Vec<usize> {
    let (x, y) = (i % mask.w, i / mask.w);
    let mut result: Vec<_> = N4
        .iter()
        .filter_map(|&(dx, dy)| {
            let (xx, yy) = (x as isize + dx, y as isize + dy);
            (xx >= 0 && yy >= 0 && xx < mask.w as isize && yy < mask.h as isize)
                .then(|| yy as usize * mask.w + xx as usize)
        })
        .filter(|&j| mask.data[j])
        .collect();
    for &(dx, dy) in &[(-1isize, -1isize), (1, -1), (-1, 1), (1, 1)] {
        let (xx, yy) = (x as isize + dx, y as isize + dy);
        if xx < 0 || yy < 0 || xx >= mask.w as isize || yy >= mask.h as isize {
            continue;
        }
        let diagonal = yy as usize * mask.w + xx as usize;
        let bridge_x = y * mask.w + xx as usize;
        let bridge_y = yy as usize * mask.w + x;
        if mask.data[diagonal] && !mask.data[bridge_x] && !mask.data[bridge_y] {
            result.push(diagonal);
        }
    }
    result
}

/// A candidate may disappear only when every one of its adjacent core pixels
/// already connects around it inside a 5×5 window. This merges touching target
/// contours and tiny cycles but leaves endpoints and larger loops intact.
fn redundant_pixel(mask: &Mask, i: usize) -> bool {
    if !mask.data[i] {
        return false;
    }
    let neighbors: Vec<_> = neighbors8(mask, i)
        .into_iter()
        .filter(|&neighbor| mask.data[neighbor])
        .collect();
    if neighbors.len() < 2 {
        return false;
    }
    let (x, y) = (i % mask.w, i / mask.w);
    let left = x.saturating_sub(2);
    let top = y.saturating_sub(2);
    let right = (x + 2).min(mask.w - 1);
    let bottom = (y + 2).min(mask.h - 1);
    let mut pending = VecDeque::from([neighbors[0]]);
    let mut seen = vec![neighbors[0]];
    while let Some(current) = pending.pop_front() {
        for neighbor in neighbors8(mask, current) {
            let (nx, ny) = (neighbor % mask.w, neighbor / mask.w);
            if neighbor == i
                || !mask.data[neighbor]
                || nx < left
                || nx > right
                || ny < top
                || ny > bottom
                || seen.contains(&neighbor)
            {
                continue;
            }
            seen.push(neighbor);
            pending.push_back(neighbor);
        }
    }
    neighbors
        .into_iter()
        .all(|neighbor| seen.contains(&neighbor))
}

fn remove_redundant_thickness(
    mask: &mut Mask,
    distance: &[usize],
    cancel: &dyn Cancellation,
) -> Result<()> {
    let mut candidates = BinaryHeap::new();
    for (i, &priority) in distance.iter().enumerate() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        if redundant_pixel(mask, i) {
            candidates.push((priority, Reverse(i)));
        }
    }
    let mut visits = 0usize;
    while let Some((_, Reverse(i))) = candidates.pop() {
        if visits.is_multiple_of(4096) {
            cancel.check()?;
        }
        visits += 1;
        if !redundant_pixel(mask, i) {
            continue;
        }
        mask.data[i] = false;
        for neighbor in neighbors8(mask, i) {
            if redundant_pixel(mask, neighbor) {
                candidates.push((distance[neighbor], Reverse(neighbor)));
            }
        }
    }
    Ok(())
}

fn remove_short_components(mask: &mut Mask, cancel: &dyn Cancellation) -> Result<()> {
    let mut seen = vec![false; mask.data.len()];
    let mut queue = VecDeque::new();
    let mut component = Vec::new();
    for i in 0..mask.data.len() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        if !mask.data[i] || seen[i] {
            continue;
        }
        seen[i] = true;
        queue.push_back(i);
        component.clear();
        while let Some(current) = queue.pop_front() {
            if component.len().is_multiple_of(4096) {
                cancel.check()?;
            }
            component.push(current);
            for neighbor in neighbors8(mask, current) {
                if mask.data[neighbor] && !seen[neighbor] {
                    seen[neighbor] = true;
                    queue.push_back(neighbor);
                }
            }
        }
        if component.len() < 3 {
            for &pixel in &component {
                mask.data[pixel] = false;
            }
        }
    }
    Ok(())
}

/// A short endpoint branch is a downscaled detail, even when it touches a
/// larger component. Remove only one- or two-pixel runs that terminate at a
/// junction; whole paths and loops remain for component pruning and thinning.
fn remove_short_terminal_branches(mask: &mut Mask, cancel: &dyn Cancellation) -> Result<()> {
    loop {
        let mut remove = Vec::new();
        for start in 0..mask.data.len() {
            if start.is_multiple_of(4096) {
                cancel.check()?;
            }
            if !mask.data[start] {
                continue;
            }
            let mut current = start;
            let mut previous = None;
            let mut branch = Vec::new();
            loop {
                let neighbors: Vec<_> = trace_neighbors(mask, current)
                    .into_iter()
                    .filter(|&neighbor| mask.data[neighbor] && Some(neighbor) != previous)
                    .collect();
                if previous.is_none() && neighbors.len() != 1 {
                    break;
                }
                branch.push(current);
                if branch.len() >= 3 || neighbors.len() != 1 {
                    break;
                }
                let next = neighbors[0];
                let onward = trace_neighbors(mask, next)
                    .into_iter()
                    .filter(|&neighbor| mask.data[neighbor] && neighbor != current)
                    .count();
                if onward >= 2 {
                    remove.extend(branch);
                    break;
                }
                if onward == 0 {
                    break;
                }
                previous = Some(current);
                current = next;
            }
        }
        if remove.is_empty() {
            return Ok(());
        }
        remove.sort_unstable();
        remove.dedup();
        for pixel in remove {
            mask.data[pixel] = false;
        }
        cancel.check()?;
    }
}

fn move_endpoints_outward(
    result: &mut Mask,
    raw: &Mask,
    distance: &[f64],
    cancel: &dyn Cancellation,
) -> Result<()> {
    const N8: [(isize, isize); 8] = [
        (-1, -1),
        (0, -1),
        (1, -1),
        (-1, 0),
        (1, 0),
        (-1, 1),
        (0, 1),
        (1, 1),
    ];
    for i in 0..result.data.len() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        if !result.data[i] {
            continue;
        }
        let (x, y) = (i % result.w, i / result.w);
        let neighbors: Vec<_> = N8
            .iter()
            .filter_map(|&(dx, dy)| {
                let xx = x as isize + dx;
                let yy = y as isize + dy;
                (xx >= 0 && yy >= 0 && xx < result.w as isize && yy < result.h as isize)
                    .then(|| yy as usize * result.w + xx as usize)
            })
            .filter(|&j| result.data[j])
            .collect();
        if neighbors.len() != 1 {
            continue;
        }
        let connection = neighbors[0];
        let replacement = N8
            .iter()
            .filter_map(|&(dx, dy)| {
                let xx = x as isize + dx;
                let yy = y as isize + dy;
                (xx >= 0 && yy >= 0 && xx < result.w as isize && yy < result.h as isize)
                    .then(|| yy as usize * result.w + xx as usize)
            })
            .filter(|&j| raw.data[j] && !result.data[j] && distance[j] < distance[i])
            .filter(|&j| {
                let (jx, jy) = (j % result.w, j / result.w);
                jx.abs_diff(connection % result.w) <= 1 && jy.abs_diff(connection / result.w) <= 1
            })
            .filter(|&j| {
                let (jx, jy) = (j % result.w, j / result.w);
                N8.iter()
                    .filter_map(|&(dx, dy)| {
                        let xx = jx as isize + dx;
                        let yy = jy as isize + dy;
                        (xx >= 0 && yy >= 0 && xx < result.w as isize && yy < result.h as isize)
                            .then(|| yy as usize * result.w + xx as usize)
                    })
                    .filter(|&k| result.data[k])
                    .all(|k| k == connection || k == i)
            })
            .min_by(|&a, &b| distance[a].total_cmp(&distance[b]).then(a.cmp(&b)));
        if let Some(j) = replacement {
            result.data[i] = false;
            result.data[j] = true;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CancellationToken;

    fn mask(w: usize, h: usize, points: &[(usize, usize)]) -> Mask {
        let mut mask = Mask::new(w, h);
        for &(x, y) in points {
            mask.data[y * w + x] = true;
        }
        mask
    }

    fn no_redundant_corner(mask: &Mask) -> bool {
        (0..mask.h.saturating_sub(1)).all(|y| {
            (0..mask.w.saturating_sub(1)).all(|x| {
                [
                    y * mask.w + x,
                    y * mask.w + x + 1,
                    (y + 1) * mask.w + x,
                    (y + 1) * mask.w + x + 1,
                ]
                .into_iter()
                .filter(|&i| mask.data[i])
                .count()
                    <= 2
            })
        })
    }

    fn component_sizes(mask: &Mask) -> Vec<usize> {
        cleanup::labels(mask, true, true)
            .1
            .into_iter()
            .skip(1)
            .collect()
    }

    fn has_redundant_pixel(mask: &Mask) -> bool {
        (0..mask.data.len()).any(|i| redundant_pixel(mask, i))
    }

    fn trace_cycle_rank(mask: &Mask) -> isize {
        let nodes = mask.data.iter().filter(|&&on| on).count() as isize;
        let edges = (0..mask.data.len())
            .filter(|&i| mask.data[i])
            .map(|i| trace_neighbors(mask, i).len() as isize)
            .sum::<isize>()
            / 2;
        let mut seen = vec![false; mask.data.len()];
        let mut components = 0isize;
        for i in 0..mask.data.len() {
            if !mask.data[i] || seen[i] {
                continue;
            }
            components += 1;
            let mut pending = vec![i];
            seen[i] = true;
            while let Some(current) = pending.pop() {
                for neighbor in trace_neighbors(mask, current) {
                    if !seen[neighbor] {
                        seen[neighbor] = true;
                        pending.push(neighbor);
                    }
                }
            }
        }
        edges - nodes + components
    }

    #[test]
    fn parallel_cores_collapse_to_the_outer_line() {
        let mut raw = Mask::new(7, 4);
        for x in 1..=5 {
            raw.data[raw.w + x] = true;
            raw.data[2 * raw.w + x] = true;
        }
        let mut support = vec![true; raw.data.len()];
        support[..raw.w].fill(false);
        let cleaned = thin_outer(&raw, &support, &CancellationToken::default()).unwrap();
        assert!(cleaned.data[raw.w + 1..raw.w + 6].iter().all(|&on| on));
        assert!(
            cleaned.data[2 * raw.w + 1..2 * raw.w + 6]
                .iter()
                .all(|&on| !on)
        );
        assert!(no_redundant_corner(&cleaned));
    }

    #[test]
    fn full_support_and_border_masks_have_finite_outer_priority() {
        let raw = mask(
            5,
            5,
            &[(0, 0), (1, 0), (0, 1), (1, 1), (2, 2), (3, 3), (4, 4)],
        );
        let cleaned = thin_outer(&raw, &[true; 25], &CancellationToken::default()).unwrap();
        assert!(cleaned.data.iter().any(|&on| on));
        assert!(no_redundant_corner(&cleaned));
        assert!(
            cleaned
                .data
                .iter()
                .zip(&raw.data)
                .all(|(&after, &before)| !after || before)
        );
    }

    #[test]
    fn diagonal_and_steep_parallel_union_becomes_one_thin_connected_path() {
        let raw = mask(
            8,
            8,
            &[
                (1, 1),
                (2, 1),
                (1, 2),
                (2, 2),
                (2, 3),
                (3, 3),
                (2, 4),
                (3, 4),
                (3, 5),
                (4, 5),
                (3, 6),
                (4, 6),
            ],
        );
        let cleaned = thin_outer(&raw, &[false; 64], &CancellationToken::default()).unwrap();
        assert!(no_redundant_corner(&cleaned), "{:?}", cleaned.data);
        assert!(!has_redundant_pixel(&cleaned));
        assert_eq!(component_sizes(&cleaned).len(), 1, "{:?}", cleaned.data);
        assert!(component_sizes(&cleaned)[0] >= 3, "{:?}", cleaned.data);
        assert!(
            cleaned
                .data
                .iter()
                .zip(&raw.data)
                .all(|(&after, &before)| !after || before)
        );
    }

    #[test]
    fn diagonal_touching_parallel_paths_collapse_tiny_cycles_but_keep_large_loops() {
        let raw = mask(
            9,
            8,
            &[
                (1, 1),
                (2, 2),
                (3, 3),
                (4, 4),
                (5, 5),
                (3, 1),
                (4, 2),
                (5, 3),
                (6, 4),
                (7, 5),
            ],
        );
        let cleaned = thin_outer(&raw, &[false; 72], &CancellationToken::default()).unwrap();
        assert_eq!(component_sizes(&cleaned).len(), 1, "{:?}", cleaned.data);
        assert!(component_sizes(&cleaned)[0] >= 4, "{:?}", cleaned.data);
        assert!(!has_redundant_pixel(&cleaned), "{:?}", cleaned.data);
        assert_eq!(trace_cycle_rank(&cleaned), 0, "{:?}", cleaned.data);
        assert!(
            (0..cleaned.data.len())
                .filter(|&i| cleaned.data[i])
                .all(|i| trace_neighbors(&cleaned, i).len() <= 2),
            "{:?}",
            cleaned.data
        );

        let mut loop_points = Vec::new();
        for x in 1..=7 {
            loop_points.push((x, 1));
            loop_points.push((x, 7));
        }
        for y in 2..7 {
            loop_points.push((1, y));
            loop_points.push((7, y));
        }
        let large_loop = mask(9, 9, &loop_points);
        let cleaned = thin_outer(&large_loop, &[false; 81], &CancellationToken::default()).unwrap();
        assert_eq!(component_sizes(&cleaned).len(), 1, "{:?}", cleaned.data);
        assert!(component_sizes(&cleaned)[0] >= 20, "{:?}", cleaned.data);
        let (background, _) = cleanup::labels(&cleaned, false, false);
        assert_ne!(background[0], background[4 * 9 + 4]);
    }

    #[test]
    fn redundant_l_on_a_long_diagonal_is_collapsed_without_breaking_the_path() {
        let raw = mask(6, 6, &[(1, 1), (2, 1), (2, 2), (3, 3), (4, 4)]);
        let cleaned = thin_outer(&raw, &[false; 36], &CancellationToken::default()).unwrap();
        assert!(!has_redundant_pixel(&cleaned), "{:?}", cleaned.data);
        assert_eq!(component_sizes(&cleaned).len(), 1, "{:?}", cleaned.data);
        assert!(component_sizes(&cleaned)[0] >= 3, "{:?}", cleaned.data);
    }

    #[test]
    fn a_two_by_two_detail_collapses_below_the_minimum_component_size() {
        let raw = mask(4, 4, &[(1, 1), (2, 1), (1, 2), (2, 2)]);
        let cleaned = thin_outer(&raw, &[false; 16], &CancellationToken::default()).unwrap();
        assert!(cleaned.data.iter().all(|&on| !on));
    }

    #[test]
    fn joined_short_fragments_survive_but_isolated_pairs_drop() {
        let raw = mask(9, 4, &[(1, 1), (2, 1), (3, 2), (6, 2), (7, 2)]);
        let cleaned = thin_outer(&raw, &[false; 36], &CancellationToken::default()).unwrap();
        assert!(cleaned.data[1 + 9] || cleaned.data[2 + 9] || cleaned.data[3 + 18]);
        assert!(component_sizes(&cleaned).iter().all(|&size| size >= 3));
        assert!(!cleaned.data[6 + 18] && !cleaned.data[7 + 18]);
    }

    #[test]
    fn terminal_branches_shorter_than_three_pixels_drop_without_cutting_paths() {
        let cancel = CancellationToken::default();
        let mut short = mask(
            9,
            9,
            &[
                (4, 1),
                (4, 2),
                (4, 3),
                (4, 4),
                (4, 5),
                (4, 6),
                (4, 7),
                (2, 4),
                (3, 4),
            ],
        );
        remove_short_terminal_branches(&mut short, &cancel).unwrap();
        assert!(!short.data[4 * 9 + 2] && !short.data[4 * 9 + 3]);
        assert!((1..=7).all(|y| short.data[y * 9 + 4]));

        let mut long = mask(
            9,
            9,
            &[
                (4, 1),
                (4, 2),
                (4, 3),
                (4, 4),
                (4, 5),
                (4, 6),
                (4, 7),
                (1, 4),
                (2, 4),
                (3, 4),
            ],
        );
        remove_short_terminal_branches(&mut long, &cancel).unwrap();
        assert!((1..=3).all(|x| long.data[4 * 9 + x]));

        let mut diagonal = mask(6, 6, &[(1, 1), (2, 2), (3, 3), (4, 4)]);
        remove_short_terminal_branches(&mut diagonal, &cancel).unwrap();
        assert_eq!(
            diagonal.data,
            mask(6, 6, &[(1, 1), (2, 2), (3, 3), (4, 4)]).data
        );

        let loop_pixels = [(2, 2), (3, 2), (3, 3), (2, 3)];
        let mut closed_loop = mask(6, 6, &loop_pixels);
        remove_short_terminal_branches(&mut closed_loop, &cancel).unwrap();
        assert_eq!(closed_loop.data, mask(6, 6, &loop_pixels).data);
    }

    #[test]
    fn distant_already_thin_components_are_unchanged() {
        let raw = mask(10, 4, &[(1, 1), (2, 1), (3, 1), (6, 2), (7, 2), (8, 2)]);
        let cleaned = thin_outer(&raw, &[false; 40], &CancellationToken::default()).unwrap();
        assert_eq!(cleaned.data, raw.data);
    }

    #[test]
    fn empty_invalid_and_cancelled_inputs_are_explicit() {
        let empty = Mask::new(0, 0);
        assert!(
            thin_outer(&empty, &[], &CancellationToken::default())
                .unwrap()
                .data
                .is_empty()
        );
        let invalid = Mask {
            w: 2,
            h: 2,
            data: vec![false; 3],
        };
        assert!(thin_outer(&invalid, &[false; 3], &CancellationToken::default()).is_err());
        let raw = Mask::new(2, 2);
        assert!(thin_outer(&raw, &[false; 3], &CancellationToken::default()).is_err());
        let cancel = CancellationToken::default();
        cancel.cancel();
        assert!(matches!(
            thin_outer(&raw, &[false; 4], &cancel),
            Err(Error::Cancelled)
        ));
    }
}
