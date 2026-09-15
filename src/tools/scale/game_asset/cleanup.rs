use crate::tools::scale::game_asset::raster::Mask;
use crate::{document::CancellationToken, error::Result};
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

pub fn thin(raw: &Mask, distance: &[f64], cancel: &CancellationToken) -> Result<Mask> {
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
