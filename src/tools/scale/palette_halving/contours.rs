//! Source silhouette paths, rendered as unfiltered palette-colour strokes.
use super::{delta_e, lab};
use crate::document::CancellationToken;
use crate::error::Result;
use image::{Rgba, RgbaImage};
use std::collections::HashMap;

#[derive(Clone, Copy)]
struct Edge {
    a: (u32, u32),
    b: (u32, u32),
}
#[derive(Clone, Copy)]
struct Node {
    p: [f32; 2],
    normal: [f32; 2],
    ink: Rgba<u8>,
    fill: Rgba<u8>,
}

/// Small spatial Gaussian derivative basis, in target-pixel units. The signed
/// even/odd responses distinguish a dark ridge centre from a monotonic step;
/// this is a derivative approximation, not a full Hilbert/Gabor quadrature bank.
struct Basis {
    luminance: Vec<f32>,
    width: u32,
    height: u32,
    scale: [f32; 2],
    g: [f32; 7],
    d: [f32; 7],
    dd: [f32; 7],
}
impl Basis {
    fn new(source: &RgbaImage, scale: [f32; 2], cancel: &CancellationToken) -> Result<Self> {
        let mut luminance = Vec::with_capacity(source.as_raw().len() / 4);
        for row in source.rows() {
            cancel.check()?;
            luminance.extend(row.map(|p| {
                // Transparent space is a neutral analysis surround only. Its RGB
                // is never copied or used to construct the output palette.
                if p[3] < 128 { 50.0 } else { lab(*p)[0] }
            }));
        }
        let sigma = 0.3_f32;
        let mut g =
            std::array::from_fn(|i| (-0.5 * ((i as f32 - 3.0) * 0.22 / sigma).powi(2)).exp());
        let sum = g.iter().sum::<f32>();
        for v in &mut g {
            *v /= sum;
        }
        let d = std::array::from_fn(|i| -(i as f32 - 3.0) * 0.22 / (sigma * sigma) * g[i]);
        let mut dd = std::array::from_fn(|i| {
            (((i as f32 - 3.0) * 0.22).powi(2) - sigma * sigma) / sigma.powi(4) * g[i]
        });
        let dc = dd.iter().sum::<f32>();
        for i in 0..7 {
            dd[i] -= dc * g[i];
        }
        Ok(Self {
            luminance,
            width: source.width(),
            height: source.height(),
            scale,
            g,
            d,
            dd,
        })
    }
    fn derivatives(&self, p: [f32; 2]) -> [f32; 5] {
        let mut out = [0.0; 5];
        for y in 0..7 {
            for x in 0..7 {
                let sx = ((p[0] + (x as f32 - 3.0) * 0.22) * self.scale[0]).floor() as i32;
                let sy = ((p[1] + (y as f32 - 3.0) * 0.22) * self.scale[1]).floor() as i32;
                let v = if sx < 0 || sy < 0 || sx >= self.width as i32 || sy >= self.height as i32 {
                    50.0
                } else {
                    self.luminance[(sy as u32 * self.width + sx as u32) as usize]
                };
                out[0] += v * self.d[x] * self.g[y];
                out[1] += v * self.g[x] * self.d[y];
                out[2] += v * self.dd[x] * self.g[y];
                out[3] += v * self.d[x] * self.d[y];
                out[4] += v * self.g[x] * self.dd[y];
            }
        }
        out
    }
    fn ridge(&self, boundary: [f32; 2], inward: [f32; 2]) -> Option<([f32; 2], [f32; 2])> {
        let mut best = None;
        for offset in [0.05, 0.2, 0.35, 0.5, 0.65, 0.8] {
            let p = [
                boundary[0] + inward[0] * offset,
                boundary[1] + inward[1] * offset,
            ];
            let [gx, gy, xx, xy, yy] = self.derivatives(p);
            let theta = 0.5 * (2.0 * xy).atan2(xx - yy);
            let mut n = [theta.cos(), theta.sin()];
            if n[0] * inward[0] + n[1] * inward[1] < 0.0 {
                n = [-n[0], -n[1]];
            }
            if n[0] * inward[0] + n[1] * inward[1] < 0.7 {
                continue;
            }
            let value = |p: [f32; 2]| {
                let (x, y) = (
                    (p[0] * self.scale[0]).floor() as i32,
                    (p[1] * self.scale[1]).floor() as i32,
                );
                if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
                    50.0
                } else {
                    self.luminance[(y as u32 * self.width + x as u32) as usize]
                }
            };
            let centre = value(p);
            let flank_a = value([p[0] - n[0] * 0.65, p[1] - n[1] * 0.65]);
            let flank_b = value([p[0] + n[0] * 0.65, p[1] + n[1] * 0.65]);
            if flank_a.min(flank_b) < centre + 6.0 {
                continue;
            }
            let even = 0.09 * (xx * n[0] * n[0] + 2.0 * xy * n[0] * n[1] + yy * n[1] * n[1]);
            let odd = 0.3 * (gx * n[0] + gy * n[1]).abs();
            if even < 2.0 || odd > even * 0.9 {
                continue;
            }
            let score = even - odd;
            if best.is_none_or(|(_, _, s)| score > s) {
                best = Some((p, n, score));
            }
        }
        best.map(|(p, n, _)| (p, n))
    }
}

fn pixel(source: &RgbaImage, x: f32, y: f32) -> Option<Rgba<u8>> {
    if x < 0.0 || y < 0.0 || x >= source.width() as f32 || y >= source.height() as f32 {
        None
    } else {
        Some(*source.get_pixel(x as u32, y as u32))
    }
}

fn evidence(
    source: &RgbaImage,
    boundary: [f32; 2],
    normal: [f32; 2],
    scale: [f32; 2],
    basis: &Basis,
) -> Option<Node> {
    let at = |d: f32| {
        pixel(
            source,
            (boundary[0] + normal[0] * d) * scale[0],
            (boundary[1] + normal[1] * d) * scale[1],
        )
    };
    let ink = [0.08, 0.2, 0.35, 0.5, 0.65]
        .into_iter()
        .filter_map(at)
        .filter(|p| p[3] >= 160)
        .min_by(|a, b| lab(*a)[0].total_cmp(&lab(*b)[0]))?;
    let dark = lab(ink);
    if dark[0] > 25.0 {
        return None;
    }
    let fill = [1.0, 1.25, 1.5, 1.75, 2.0]
        .into_iter()
        .filter_map(at)
        .filter(|p| {
            let c = lab(*p);
            p[3] >= 160 && c[0] > dark[0] + 10.0 && c[1].hypot(c[2]) > 8.0
        })
        .max_by(|a, b| lab(*a)[0].total_cmp(&lab(*b)[0]))?;
    let (p, normal) = basis.ridge(boundary, normal)?;
    Some(Node {
        p,
        normal,
        ink,
        fill,
    })
}

/// Min-sum chain inference, with previous direction in the state. Every step
/// is 8-connected inside a subpixel source corridor; stays allow oversampling.
/// Turn costs weaken at source corners instead of banning all three-pixel Ls.
fn joint_path(nodes: &[Node], size: (u32, u32)) -> Vec<(u32, u32, usize)> {
    const DIR: [(i32, i32); 9] = [
        (1, 0),
        (1, 1),
        (0, 1),
        (-1, 1),
        (-1, 0),
        (-1, -1),
        (0, -1),
        (1, -1),
        (0, 0),
    ];
    let layers: Vec<Vec<(i32, i32)>> = nodes
        .iter()
        .map(|n| {
            let (x, y) = (n.p[0].floor() as i32, n.p[1].floor() as i32);
            let mut cells = Vec::new();
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (cx, cy) = (x + dx, y + dy);
                    if cx >= 0
                        && cy >= 0
                        && cx < size.0 as i32
                        && cy < size.1 as i32
                        && (cx as f32 + 0.5 - n.p[0]).hypot(cy as f32 + 0.5 - n.p[1]) <= 0.9
                    {
                        cells.push((cx, cy));
                    }
                }
            }
            cells
        })
        .collect();
    if layers.iter().any(Vec::is_empty) {
        return Vec::new();
    }
    let unary = |i: usize, cell: (i32, i32)| {
        let n = nodes[i];
        let d = [cell.0 as f32 + 0.5 - n.p[0], cell.1 as f32 + 0.5 - n.p[1]];
        3.0 * (d[0] * n.normal[0] + d[1] * n.normal[1]).powi(2) + d[0] * d[0] + d[1] * d[1]
    };
    let mut cost = vec![f32::INFINITY; layers[0].len() * 9];
    for (c, &p) in layers[0].iter().enumerate() {
        cost[c * 9 + 8] = unary(0, p);
    }
    let mut parents = vec![vec![]];
    for i in 1..nodes.len() {
        let mut next = vec![f32::INFINITY; layers[i].len() * 9];
        let mut back = vec![usize::MAX; next.len()];
        let continuation = (nodes[i].normal[0] * nodes[i - 1].normal[0]
            + nodes[i].normal[1] * nodes[i - 1].normal[1])
            .abs()
            .powi(4);
        for (b, &q) in layers[i].iter().enumerate() {
            for (a, &p) in layers[i - 1].iter().enumerate() {
                let step = (q.0 - p.0, q.1 - p.1);
                let Some(direction) = DIR.iter().position(|&d| d == step) else {
                    continue;
                };
                for (old, &previous_direction) in DIR.iter().enumerate() {
                    let previous = a * 9 + old;
                    if !cost[previous].is_finite() {
                        continue;
                    }
                    let to = b * 9 + if direction == 8 { old } else { direction };
                    let turn = if old == 8 || direction == 8 {
                        0.0
                    } else {
                        let (u, v) = (previous_direction, step);
                        let dot = (u.0 * v.0 + u.1 * v.1) as f32
                            / (((u.0 * u.0 + u.1 * u.1) * (v.0 * v.0 + v.1 * v.1)) as f32).sqrt();
                        2.0 * continuation * (1.0 - dot)
                    };
                    let value = cost[previous] + unary(i, q) + turn;
                    if value < next[to] {
                        next[to] = value;
                        back[to] = previous;
                    }
                }
            }
        }
        cost = next;
        parents.push(back);
    }
    let Some((mut state, &value)) = cost.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1))
    else {
        return Vec::new();
    };
    if !value.is_finite() {
        return Vec::new();
    }
    let mut path = Vec::new();
    for i in (0..nodes.len()).rev() {
        let (x, y) = layers[i][state / 9];
        if path
            .last()
            .is_none_or(|&(px, py, _)| px != x as u32 || py != y as u32)
        {
            path.push((x as u32, y as u32, i));
        }
        if i > 0 {
            state = parents[i][state];
        }
    }
    path.reverse();
    path
}

pub(super) fn refine(
    source: &RgbaImage,
    output: &mut RgbaImage,
    cancel: &CancellationToken,
) -> Result<()> {
    cancel.check()?;
    let scale = [
        source.width() as f32 / output.width() as f32,
        source.height() as f32 / output.height() as f32,
    ];
    let basis = Basis::new(source, scale, cancel)?;
    let occupied = |x: i32, y: i32| {
        x >= 0
            && y >= 0
            && x < source.width() as i32
            && y < source.height() as i32
            && source.get_pixel(x as u32, y as u32)[3] >= 128
    };
    let mut edges = Vec::new();
    for y in 0..source.height() {
        cancel.check()?;
        for x in 0..source.width() {
            if !occupied(x as i32, y as i32) {
                continue;
            }
            if !occupied(x as i32, y as i32 - 1) {
                edges.push(Edge {
                    a: (x, y),
                    b: (x + 1, y),
                });
            }
            if !occupied(x as i32 + 1, y as i32) {
                edges.push(Edge {
                    a: (x + 1, y),
                    b: (x + 1, y + 1),
                });
            }
            if !occupied(x as i32, y as i32 + 1) {
                edges.push(Edge {
                    a: (x + 1, y + 1),
                    b: (x, y + 1),
                });
            }
            if !occupied(x as i32 - 1, y as i32) {
                edges.push(Edge {
                    a: (x, y + 1),
                    b: (x, y),
                });
            }
        }
    }
    let mut outgoing = HashMap::<(u32, u32), Vec<usize>>::new();
    for (i, e) in edges.iter().enumerate() {
        outgoing.entry(e.a).or_default().push(i);
    }
    let mut visited = vec![false; edges.len()];
    let mut paths = Vec::<Vec<Node>>::new();
    for start in 0..edges.len() {
        if visited[start] {
            continue;
        }
        cancel.check()?;
        let mut current = start;
        let mut run = Vec::<Node>::new();
        loop {
            if visited[current] {
                break;
            }
            if current.is_multiple_of(256) {
                cancel.check()?;
            }
            visited[current] = true;
            let edge = edges[current];
            let (dx, dy) = (
                edge.b.0 as i32 - edge.a.0 as i32,
                edge.b.1 as i32 - edge.a.1 as i32,
            );
            let p = [
                (edge.a.0 as f32 + edge.b.0 as f32) * 0.5 / scale[0],
                (edge.a.1 as f32 + edge.b.1 as f32) * 0.5 / scale[1],
            ];
            // Estimate the inward normal across a target-pixel neighbourhood,
            // rather than alternating horizontal/vertical on source stair steps.
            let mut gradient = [0.0_f32; 2];
            for oy in -1..=1 {
                for ox in -1..=1 {
                    let alpha = pixel(
                        source,
                        (p[0] + ox as f32 * 0.6) * scale[0],
                        (p[1] + oy as f32 * 0.6) * scale[1],
                    )
                    .map_or(0.0, |p| f32::from(p[3]));
                    gradient[0] += ox as f32 * alpha;
                    gradient[1] += oy as f32 * alpha;
                }
            }
            let length = gradient[0].hypot(gradient[1]);
            let normal = if length > 1.0 {
                [gradient[0] / length, gradient[1] / length]
            } else {
                [-dy as f32, dx as f32]
            };
            if let Some(node) = evidence(source, p, normal, scale, &basis) {
                let split = run.last().is_some_and(|old| {
                    delta_e(lab(old.ink), lab(node.ink)) > 8.0
                        || delta_e(lab(old.fill), lab(node.fill)) > 18.0
                        || (old.p[0] - node.p[0]).hypot(old.p[1] - node.p[1]) > 1.5
                });
                if split && !run.is_empty() {
                    paths.push(std::mem::take(&mut run));
                }
                if run
                    .last()
                    .is_none_or(|old| (old.p[0] - node.p[0]).hypot(old.p[1] - node.p[1]) >= 0.4)
                {
                    run.push(node);
                }
            } else if !run.is_empty() {
                paths.push(std::mem::take(&mut run));
            }
            let Some(next) = outgoing.get(&edge.b).and_then(|ids| {
                ids.iter()
                    .copied()
                    .filter(|i| !visited[*i])
                    .min_by_key(|i| {
                        let e = edges[*i];
                        let (nx, ny) = (e.b.0 as i32 - e.a.0 as i32, e.b.1 as i32 - e.a.1 as i32);
                        match dx * ny - dy * nx {
                            1 => 0,
                            0 => 1,
                            _ => 2,
                        }
                    })
            }) else {
                break;
            };
            current = next;
        }
        if !run.is_empty() {
            paths.push(run);
        }
    }
    let baseline = output.clone();
    let mut strokes = HashMap::<(u32, u32), (Rgba<u8>, Node)>::new();
    for nodes in paths.iter().filter(|p| p.len() >= 4) {
        cancel.check()?;
        let length = nodes
            .windows(2)
            .map(|w| (w[0].p[0] - w[1].p[0]).hypot(w[0].p[1] - w[1].p[1]))
            .sum::<f32>();
        if length < 2.0 {
            continue;
        }
        let mut inks: Vec<_> = nodes.iter().map(|n| n.ink).collect();
        inks.sort_by(|a, b| lab(*a)[0].total_cmp(&lab(*b)[0]));
        let mut ink = inks[inks.len() / 4];
        ink[3] = 255;
        for (x, y, index) in joint_path(nodes, output.dimensions()) {
            strokes.entry((x, y)).or_insert((ink, nodes[index]));
        }
    }
    // Resolve from a snapshot; fill restoration must never overwrite any path.
    let mut fills = HashMap::<(u32, u32), Rgba<u8>>::new();
    let mut ordered: Vec<_> = strokes.iter().collect();
    ordered.sort_by_key(|(p, _)| **p);
    for (&(x, y), &(_, node)) in ordered {
        cancel.check()?;
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                if nx < 0
                    || ny < 0
                    || nx >= output.width() as i32
                    || ny >= output.height() as i32
                    || strokes.contains_key(&(nx as u32, ny as u32))
                {
                    continue;
                }
                let side = (nx as f32 + 0.5 - node.p[0]) * node.normal[0]
                    + (ny as f32 + 0.5 - node.p[1]) * node.normal[1];
                let current = *baseline.get_pixel(nx as u32, ny as u32);
                if side < -0.25 {
                    if pixel(
                        source,
                        (nx as f32 + 0.5) * scale[0],
                        (ny as f32 + 0.5) * scale[1],
                    )
                    .is_none_or(|p| p[3] < 128)
                    {
                        fills
                            .entry((nx as u32, ny as u32))
                            .or_insert(Rgba([0, 0, 0, 0]));
                    }
                } else if side > 0.35 && current[3] >= 128 {
                    let (a, b, c) = (lab(node.ink), lab(node.fill), lab(current));
                    let v = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                    let t = ((c[0] - a[0]) * v[0] + (c[1] - a[1]) * v[1] + (c[2] - a[2]) * v[2])
                        / (v.iter().map(|v| v * v).sum::<f32>()).max(1e-6);
                    let residual = ((c[0] - a[0] - t * v[0]).powi(2)
                        + (c[1] - a[1] - t * v[1]).powi(2)
                        + (c[2] - a[2] - t * v[2]).powi(2))
                    .sqrt();
                    if (0.05..0.8).contains(&t) && residual < 5.0 {
                        let mut fill = node.fill;
                        fill[3] = current[3];
                        fills.entry((nx as u32, ny as u32)).or_insert(fill);
                    }
                }
            }
        }
    }
    for ((x, y), p) in fills {
        output.put_pixel(x, y, p);
    }
    for (&(x, y), &(p, _)) in &strokes {
        output.put_pixel(x, y, p);
    }
    cancel.check()?;
    tracing::debug!(
        pixels = strokes.len(),
        "Game Asset source silhouette contour paths"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chain_is_connected_inside_the_source_corridor() {
        let nodes: Vec<_> = (0..30)
            .map(|i| Node {
                p: [1.5 + i as f32 * 0.35, 1.5 + i as f32 * 0.18],
                normal: [-0.457, 0.889],
                ink: Rgba([5, 5, 5, 255]),
                fill: Rgba([70, 140, 30, 255]),
            })
            .collect();
        let points = joint_path(&nodes, (16, 16));
        assert!(points.len() >= 10);
        for w in points.windows(2) {
            assert!(w[0].0.abs_diff(w[1].0) <= 1 && w[0].1.abs_diff(w[1].1) <= 1);
        }
        for &(x, y, i) in &points {
            assert!((x as f32 + 0.5 - nodes[i].p[0]).hypot(y as f32 + 0.5 - nodes[i].p[1]) <= 0.9);
        }
        let mut corner: Vec<_> = (1..=6)
            .map(|x| Node {
                p: [x as f32 + 0.5, 1.5],
                normal: [0.0, 1.0],
                ink: Rgba([5, 5, 5, 255]),
                fill: Rgba([70, 140, 30, 255]),
            })
            .collect();
        corner.extend((2..=6).map(|y| Node {
            p: [6.5, y as f32 + 0.5],
            normal: [1.0, 0.0],
            ink: Rgba([5, 5, 5, 255]),
            fill: Rgba([70, 140, 30, 255]),
        }));
        let path = joint_path(&corner, (10, 10));
        assert!(
            path.iter().any(|&(x, y, _)| (x, y) == (6, 1)),
            "retain a source-supported right-angle corner"
        );
    }
    #[test]
    fn derivative_phase_rejects_steps_and_locates_dark_ridges() {
        let cancel = CancellationToken::default();
        for step in [false, true] {
            let source = RgbaImage::from_fn(128, 128, |x, _| {
                if if step { x >= 62 } else { (62..66).contains(&x) } {
                    Rgba([5, 5, 5, 255])
                } else {
                    Rgba([80, 140, 40, 255])
                }
            });
            let basis = Basis::new(&source, [4.0, 4.0], &cancel).unwrap();
            let ridge = basis.ridge([15.5, 16.0], [1.0, 0.0]);
            assert_eq!(ridge.is_some(), !step);
            if let Some((p, n)) = ridge {
                assert!((p[0] - 16.0).abs() < 0.4);
                assert!(n[0] > 0.95);
            }
        }
    }
    #[test]
    fn outlined_diagonal_has_source_ink_and_less_blend_colour() {
        let source = RgbaImage::from_fn(128, 128, |x, y| {
            let boundary = 32 + y / 2;
            if x < boundary {
                Rgba([0, 0, 0, 0])
            } else if x < boundary + 4 {
                Rgba([5 + (y % 3) as u8, 8, 3, 255])
            } else if x < boundary + 7 {
                Rgba([20, 40, 10, 255])
            } else {
                Rgba([60, 120, 30, 255])
            }
        });
        let mut output =
            image::imageops::resize(&source, 32, 32, image::imageops::FilterType::Nearest);
        let before = output.clone();
        refine(&source, &mut output, &CancellationToken::default()).unwrap();
        assert_ne!(before, output);
        let before_mud = before.pixels().filter(|p| p.0 == [20, 40, 10, 255]).count();
        let after_mud = output.pixels().filter(|p| p.0 == [20, 40, 10, 255]).count();
        assert!(after_mud < before_mud);
        assert!(
            output
                .pixels()
                .filter(|p| p[3] > 0)
                .all(|p| source.pixels().any(|s| s.0[..3] == p.0[..3]))
        );
        let cancel = CancellationToken::default();
        cancel.cancel();
        let before = output.clone();
        assert!(refine(&source, &mut output, &cancel).is_err());
        assert_eq!(before, output);
    }
    #[test]
    fn soft_unoutlined_shading_is_untouched() {
        let source =
            RgbaImage::from_fn(64, 64, |x, y| Rgba([80 + x as u8, 100 + y as u8, 40, 255]));
        let mut output =
            image::imageops::resize(&source, 16, 16, image::imageops::FilterType::Nearest);
        let before = output.clone();
        refine(&source, &mut output, &CancellationToken::default()).unwrap();
        assert_eq!(before, output);
    }
}
