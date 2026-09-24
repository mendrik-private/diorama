use crate::raster::Mask;
use crate::{Cancellation, Result};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};
use std::sync::LazyLock;

// (dy,dx), exactly matching the supplied clockwise ring order.
pub const N8: [(isize, isize); 8] = [
    (-1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
    (1, 0),
    (1, -1),
    (0, -1),
    (-1, -1),
];
const N4: [(isize, isize); 4] = [(-1, 0), (0, 1), (1, 0), (0, -1)];

pub fn labels(mask: &Mask, foreground: bool, eight: bool) -> (Vec<usize>, Vec<usize>) {
    let mut labels = vec![0; mask.data.len()];
    let mut sizes = vec![0];
    let neighbors: &[(isize, isize)] = if eight { &N8 } else { &N4 };
    let mut queue = Vec::new();
    for i in 0..mask.data.len() {
        if mask.data[i] != foreground || labels[i] != 0 {
            continue;
        }
        let id = sizes.len();
        queue.clear();
        queue.push(i);
        labels[i] = id;
        let mut next = 0;
        while next < queue.len() {
            let p = queue[next];
            next += 1;
            let (x, y) = ((p % mask.w) as isize, (p / mask.w) as isize);
            for &(dy, dx) in neighbors {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= mask.w as isize || ny >= mask.h as isize {
                    continue;
                }
                let j = ny as usize * mask.w + nx as usize;
                if mask.data[j] == foreground && labels[j] == 0 {
                    labels[j] = id;
                    queue.push(j);
                }
            }
        }
        sizes.push(queue.len());
    }
    (labels, sizes)
}

fn ring(mask: &Mask, y: usize, x: usize) -> usize {
    N8.iter()
        .enumerate()
        .map(|(i, &(dy, dx))| usize::from(mask.at(x as isize + dx, y as isize + dy)) << i)
        .sum()
}

struct Tables {
    simple: [bool; 256],
    eligible: [bool; 256],
    junction: [bool; 256],
}
impl Tables {
    fn new() -> Self {
        let mut table = Self {
            simple: [false; 256],
            eligible: [false; 256],
            junction: [false; 256],
        };
        for code in 0..256 {
            let mut local = Mask::new(3, 3);
            let mut ring = [false; 8];
            for (i, &(dy, dx)) in N8.iter().enumerate() {
                ring[i] = (code >> i) & 1 != 0;
                local.data[(1 + dy) as usize * 3 + (1 + dx) as usize] = ring[i];
            }
            let components = labels(&local, true, true).1.len() - 1;
            let mut bg = local.clone();
            for v in &mut bg.data {
                *v = !*v;
            }
            bg.data[4] = false;
            let background = labels(&bg, true, false).0;
            let touching: HashSet<_> = [1, 3, 4, 5, 7]
                .iter()
                .map(|&i| background[i])
                .filter(|&v| v != 0)
                .collect();
            table.simple[code] =
                components == 1 && touching.len() == 1 && ring.iter().filter(|&&v| v).count() > 1;
            table.junction[code] = (0..8).filter(|&i| !ring[i] && ring[(i + 1) % 8]).count() >= 3;
            table.eligible[code] = (0..4).any(|i| ring[2 * i] && ring[(2 * i + 2) % 8]);
        }
        table
    }
    fn removable(&self, code: usize) -> bool {
        self.simple[code] && self.eligible[code] && !self.junction[code]
    }
}

#[derive(Clone, Copy)]
struct Candidate {
    distance: f64,
    y: usize,
    x: usize,
}
impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Candidate {}
impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.distance
            .total_cmp(&other.distance)
            .then_with(|| other.y.cmp(&self.y))
            .then_with(|| other.x.cmp(&self.x))
    }
}

pub fn thin(raw: &Mask, distance: &[f64], cancel: &dyn Cancellation) -> Result<Mask> {
    let mut mask = raw.clone();
    static TABLES: LazyLock<Tables> = LazyLock::new(Tables::new);
    let tables = &*TABLES;
    let mut heap = BinaryHeap::new();
    let push = |heap: &mut BinaryHeap<Candidate>, mask: &Mask, y: usize, x: usize| {
        if mask.data[y * mask.w + x] && tables.removable(ring(mask, y, x)) {
            heap.push(Candidate {
                distance: distance[y * mask.w + x],
                y,
                x,
            });
        }
    };
    for y in 0..mask.h {
        cancel.check()?;
        for x in 0..mask.w {
            push(&mut heap, &mask, y, x);
        }
    }

    while let Some(Candidate { y, x, .. }) = heap.pop() {
        cancel.check()?;
        if !mask.data[y * mask.w + x] || !tables.removable(ring(&mask, y, x)) {
            continue;
        }
        mask.data[y * mask.w + x] = false;

        for (dy, dx) in N8 {
            let (ny, nx) = (y as isize + dy, x as isize + dx);
            if ny >= 0 && nx >= 0 && ny < mask.h as isize && nx < mask.w as isize {
                push(&mut heap, &mask, ny as usize, nx as usize);
            }
        }
    }
    Ok(mask)
}

/// Fill enclosed background components whose every pixel lies within
/// `max_depth` 8-connected steps of the foreground. Components touching the
/// image border are exterior and never filled; wider holes are drawn shapes.
pub fn fill_slivers(mask: &Mask, max_depth: usize, cancel: &dyn Cancellation) -> Result<Mask> {
    let (labels, sizes) = labels(mask, false, false);
    let (w, h) = (mask.w, mask.h);
    let mut fill = vec![true; sizes.len()];
    fill[0] = false;
    let mut depth = vec![usize::MAX; mask.data.len()];
    let mut queue = std::collections::VecDeque::new();
    for i in 0..mask.data.len() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        if mask.data[i] {
            continue;
        }
        let (x, y) = ((i % w) as isize, (i / w) as isize);
        if x == 0 || y == 0 || x as usize + 1 == w || y as usize + 1 == h {
            fill[labels[i]] = false;
        }
        if N8.iter().any(|&(dy, dx)| mask.at(x + dx, y + dy)) {
            depth[i] = 1;
            queue.push_back(i);
        }
    }
    while let Some(i) = queue.pop_front() {
        let (x, y) = ((i % w) as isize, (i / w) as isize);
        if depth[i] > max_depth {
            fill[labels[i]] = false;
        }
        for (dy, dx) in N8 {
            let (nx, ny) = (x + dx, y + dy);
            if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                continue;
            }
            let j = ny as usize * w + nx as usize;
            if !mask.data[j] && depth[j] == usize::MAX {
                depth[j] = depth[i] + 1;
                queue.push_back(j);
            }
        }
    }
    let mut out = mask.clone();
    for (i, value) in out.data.iter_mut().enumerate() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        *value |= fill[labels[i]];
    }
    Ok(out)
}

/// The graph neighbors used by contour tracing: 8-connected, except that a
/// diagonal is redundant when an orthogonal route between them exists.
fn skeleton_neighbors(mask: &Mask, i: usize) -> impl Iterator<Item = usize> + '_ {
    let (x, y) = ((i % mask.w) as isize, (i / mask.w) as isize);
    N8.into_iter()
        .filter(move |&(dy, dx)| {
            mask.at(x + dx, y + dy)
                && !(dx != 0 && dy != 0 && (mask.at(x + dx, y) || mask.at(x, y + dy)))
        })
        .map(move |(dy, dx)| (y + dy) as usize * mask.w + (x + dx) as usize)
}

/// Direction from `from` along the unbranched skeleton through `first`,
/// measured over at most three pixels.
fn branch_direction(mask: &Mask, from: usize, first: usize) -> [f64; 2] {
    let (mut previous, mut at) = (from, first);
    for _ in 1..3 {
        let next: Vec<_> = skeleton_neighbors(mask, at)
            .filter(|&j| j != previous)
            .collect();
        let [next] = next[..] else {
            break;
        };
        (previous, at) = (at, next);
    }
    let dx = (at % mask.w) as f64 - (from % mask.w) as f64;
    let dy = (at / mask.w) as f64 - (from / mask.w) as f64;
    let length = dx.hypot(dy).max(1e-9);
    [dx / length, dy / length]
}

/// Remove terminal branches of at most `max_pixels` that end at a junction.
///
/// Texture and shading beside an outline produce short spurs whose free end
/// is not a drawn line end. Shortest branches go first, and every junction
/// keeps at least two branches. A short branch that continues a longer one
/// straight through its junction is a line end, not a spur, so a line is
/// never cut and a small star-shaped feature loses only its excess arms.
pub fn prune_spurs(mask: &Mask, max_pixels: usize, cancel: &dyn Cancellation) -> Result<Mask> {
    let mut out = mask.clone();
    for _ in 0..3 {
        let degree = |mask: &Mask, i: usize| skeleton_neighbors(mask, i).count();
        let mut spurs = Vec::new();
        for start in 0..out.data.len() {
            if start.is_multiple_of(4096) {
                cancel.check()?;
            }
            if !out.data[start] || degree(&out, start) != 1 {
                continue;
            }
            let mut branch = vec![start];
            let mut previous = usize::MAX;
            let mut at = start;
            let junction = loop {
                let next: Vec<_> = skeleton_neighbors(&out, at)
                    .filter(|&j| j != previous)
                    .collect();
                let [next] = next[..] else {
                    break None;
                };
                if degree(&out, next) > 2 {
                    break Some(next);
                }
                if branch.len() >= max_pixels || degree(&out, next) != 2 {
                    break None;
                }
                previous = at;
                at = next;
                branch.push(at);
            };
            if let Some(junction) = junction {
                spurs.push((branch.len(), start, junction, branch));
            }
        }
        if spurs.is_empty() {
            break;
        }
        spurs.sort_unstable();
        let short: std::collections::HashSet<_> = spurs
            .iter()
            .map(|(_, _, _, branch)| *branch.last().unwrap())
            .collect();
        let mut remaining = vec![0usize; out.data.len()];
        for (_, _, junction, _) in &spurs {
            remaining[*junction] = degree(&out, *junction);
        }
        let mut removed = false;
        for (_, _, junction, branch) in spurs {
            if remaining[junction] <= 2 {
                continue;
            }
            let first = *branch.last().unwrap();
            let direction = branch_direction(&out, junction, first);
            let continues_long_branch = skeleton_neighbors(&out, junction)
                .filter(|&j| j != first && !short.contains(&j))
                .any(|j| {
                    let other = branch_direction(&out, junction, j);
                    direction[0] * other[0] + direction[1] * other[1] < -0.7
                });
            if continues_long_branch {
                continue;
            }
            remaining[junction] -= 1;
            for pixel in branch {
                out.data[pixel] = false;
            }
            removed = true;
        }
        if !removed {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CancellationToken;

    fn mask(rows: &[&str]) -> Mask {
        let mut mask = Mask::new(rows[0].len(), rows.len());
        for (y, row) in rows.iter().enumerate() {
            for (x, cell) in row.bytes().enumerate() {
                mask.data[y * mask.w + x] = cell == b'#';
            }
        }
        mask
    }

    #[test]
    fn slivers_fill_but_drawn_holes_and_the_exterior_stay_open() {
        let cancel = CancellationToken::default();
        let raw = mask(&[
            "..............",
            ".#######......",
            ".#.....#.####.",
            ".#######.#..#.",
            ".........#..#.",
            ".#####...#..#.",
            ".#...#...####.",
            ".#...#........",
            ".#...#........",
            ".#####........",
            "..............",
        ]);
        let filled = fill_slivers(&raw, 1, &cancel).unwrap();
        // The one-pixel slit between two overlapping fits is filled.
        assert!((2..7).all(|x| filled.data[2 * raw.w + x]));
        // Every pixel of a 2px-wide enclosed channel touches ink, so it is a
        // sliver too; a 3x3 hole has an interior and is a drawn shape.
        assert!((3..6).all(|y| filled.data[y * raw.w + 10] && filled.data[y * raw.w + 11]));
        assert!((6..9).all(|y| (2..5).all(|x| !filled.data[y * raw.w + x])));
        // Border-connected background is exterior.
        assert!(!filled.data[0]);
        assert_eq!(
            filled.data.iter().filter(|&&on| on).count(),
            raw.data.iter().filter(|&&on| on).count() + 5 + 6
        );
    }

    #[test]
    fn short_spurs_drop_while_lines_and_long_branches_remain() {
        let cancel = CancellationToken::default();
        let raw = mask(&[
            "...............",
            "......#........",
            "......#.....#..",
            "......#.....#..",
            "......#.....#..",
            "###############",
            "............#..",
            "............#..",
            "...............",
        ]);
        let pruned = prune_spurs(&raw, 4, &cancel).unwrap();
        // The four-pixel spur above x=6 is fuzz, and so is the short stroke
        // crossing at x=12. The line's own two-pixel end beyond that crossing
        // continues it straight through the junction and survives.
        assert!((1..5).all(|y| !pruned.data[y * raw.w + 6]));
        assert!((2..5).all(|y| !pruned.data[y * raw.w + 12]));
        assert!((6..8).all(|y| !pruned.data[y * raw.w + 12]));
        assert!((0..15).all(|x| pruned.data[5 * raw.w + x]), "line was cut");
    }

    #[test]
    fn a_star_of_short_arms_keeps_one_line_through_its_junction() {
        let cancel = CancellationToken::default();
        let raw = mask(&[
            ".....", //
            "..#..", //
            "..#..", //
            "#####", //
            "..#..", //
            "..#..", //
            ".....",
        ]);
        let pruned = prune_spurs(&raw, 4, &cancel).unwrap();
        let (_, sizes) = labels(&pruned, true, true);
        assert_eq!(sizes.len(), 2, "the feature must stay one component");
        assert!(pruned.data[3 * raw.w + 2], "junction removed");
        assert!(pruned.data.iter().filter(|&&on| on).count() >= 5);
    }
}
