//! Trace curves and join unambiguous, tangent-compatible continuations.
use crate::{document::CancellationToken, error::Result};
mod merge;
use crate::tools::scale::game_asset::{
    cleanup,
    detect::{Model, curve_point},
    field::{Spatial, distance2},
    raster::Mask,
};
use std::collections::HashSet;

pub const MAX_SHORT_PIXELS: usize = 3;

pub struct Contours {
    pub lengths: Vec<f64>,
    assignments: Vec<Vec<usize>>,
    points: Vec<[f64; 2]>,
    source_size: [usize; 2],
    paths: Vec<Vec<usize>>,
    bridges: Vec<(usize, Model)>,
}

fn direction(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    let d = distance2(a, b).sqrt();
    [(b[0] - a[0]) / d, (b[1] - a[1]) / d]
}
fn dot(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}

impl Contours {
    pub fn new(mask: &Mask, models: &[Model], cancel: &CancellationToken) -> Result<Self> {
        let pixels: Vec<_> = mask
            .data
            .iter()
            .enumerate()
            .filter(|&(_, v)| *v)
            .map(|(i, _)| i)
            .collect();
        let mut node_at = vec![usize::MAX; mask.data.len()];
        for (i, &pixel) in pixels.iter().enumerate() {
            cancel.check()?;
            node_at[pixel] = i;
        }
        let points: Vec<_> = pixels
            .iter()
            .map(|&i| [(i % mask.w) as f64, (i / mask.w) as f64])
            .collect();
        let tree = Spatial::new(points.clone(), 2.);
        let mut edges = Vec::new();
        let mut adjacent = vec![Vec::new(); points.len()];
        for (i, &pixel) in pixels.iter().enumerate() {
            cancel.check()?;
            let (x, y) = ((pixel % mask.w) as isize, (pixel / mask.w) as isize);
            for (dy, dx) in cleanup::N8 {
                if !mask.at(x + dx, y + dy) {
                    continue;
                }
                // A diagonal is redundant when an orthogonal route exists.
                // Removing that shortcut avoids counting tiny triangles at bends.
                if dx != 0 && dy != 0 && (mask.at(x + dx, y) || mask.at(x, y + dy)) {
                    continue;
                }
                let j = node_at[(y + dy) as usize * mask.w + (x + dx) as usize];
                if j > i {
                    let e = edges.len();
                    edges.push((i, j));
                    adjacent[i].push(e);
                    adjacent[j].push(e);
                }
            }
        }
        // Small raster gaps may split otherwise supported fitted geometry.
        // Only mutually facing endpoints can join, and every quarter-pixel of
        // the connection needs nearby, tangent-compatible source curve evidence.
        let mut support_points = Vec::new();
        let mut support_models = Vec::new();
        for (i, m) in models.iter().enumerate() {
            cancel.check()?;
            let count = (((m[8] - m[7]) / 0.2).ceil() as usize).max(1);
            for k in 0..=count {
                support_points.push(curve_point(
                    m,
                    m[7] + (m[8] - m[7]) * k as f64 / count as f64,
                ));
                support_models.push(i);
            }
        }
        let support = Spatial::new(support_points, 1.);
        let other = |e: usize, i: usize, edges: &[(usize, usize)]| {
            let (a, b) = edges[e];
            if a == i { b } else { a }
        };
        let mut proposals = Vec::new();
        for i in 0..points.len() {
            cancel.check()?;
            if adjacent[i].len() != 1 {
                continue;
            }
            let outward = direction(points[other(adjacent[i][0], i, &edges)], points[i]);
            for j in tree.radius(points[i], 2.5) {
                if j <= i || adjacent[j].len() != 1 || other(adjacent[i][0], i, &edges) == j {
                    continue;
                }
                let d = direction(points[i], points[j]);
                let inward = direction(points[j], points[other(adjacent[j][0], j, &edges)]);
                if dot(outward, d) < 0.8 || dot(inward, d) < 0.8 {
                    continue;
                }
                let length = distance2(points[i], points[j]).sqrt();
                let count = (length / 0.25).ceil() as usize;
                let mut confidence = f64::INFINITY;
                let supported = (0..=count).all(|k| {
                    let t = k as f64 / count as f64;
                    let p = [
                        points[i][0] + t * (points[j][0] - points[i][0]),
                        points[i][1] + t * (points[j][1] - points[i][1]),
                    ];
                    let best = support
                        .radius(p, 0.65)
                        .into_iter()
                        .filter(|&s| {
                            let m = &models[support_models[s]];
                            // Model normal is the local fitted coordinate frame.
                            dot(d, [-m[3], m[2]]).abs() >= 0.8
                        })
                        .min_by(|&a, &b| {
                            distance2(p, support.points[a])
                                .total_cmp(&distance2(p, support.points[b]))
                                .then(a.cmp(&b))
                        });
                    if let Some(s) = best {
                        confidence = confidence.min(models[support_models[s]][9]);
                        true
                    } else {
                        false
                    }
                });
                if supported {
                    proposals.push((length, i, j, confidence));
                }
            }
        }
        proposals.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        let mut bridges = Vec::new();
        for (length, i, j, confidence) in proposals {
            if adjacent[i].len() != 1 || adjacent[j].len() != 1 {
                continue;
            }
            let d = direction(points[i], points[j]);
            bridges.push((
                edges.len(),
                [
                    (points[i][0] + points[j][0]) * 0.5,
                    (points[i][1] + points[j][1]) * 0.5,
                    -d[1],
                    d[0],
                    0.,
                    0.,
                    0.,
                    -length * 0.5,
                    length * 0.5,
                    confidence,
                ],
            ));
            let e = edges.len();
            edges.push((i, j));
            adjacent[i].push(e);
            adjacent[j].push(e);
        }
        // Walk maximal arcs between endpoints/junctions, then remaining loops.
        // Edge visitation preserves every branch and includes the closing edge.
        let mut seen = vec![false; edges.len()];
        let mut paths = Vec::new();
        let mut edge_paths = vec![usize::MAX; edges.len()];
        let starts = (0..points.len())
            .filter(|&i| adjacent[i].len() != 2)
            .chain((0..points.len()).filter(|&i| adjacent[i].len() == 2));
        for start in starts {
            cancel.check()?;
            if adjacent[start].is_empty() {
                paths.push(vec![start]);
                continue;
            }
            for &first in &adjacent[start] {
                if seen[first] {
                    continue;
                }
                let mut path = vec![start];
                let mut i = start;
                let mut e = first;
                loop {
                    seen[e] = true;
                    edge_paths[e] = paths.len();
                    i = other(e, i, &edges);
                    path.push(i);
                    if adjacent[i].len() != 2 {
                        break;
                    }
                    let Some(next) = adjacent[i].iter().copied().find(|&e| !seen[e]) else {
                        break;
                    };
                    e = next;
                }
                paths.push(path);
            }
        }
        let segments = paths;
        let (paths, segment_contours) = merge::join(&points, &segments, cancel)?;
        let lengths: Vec<f64> = paths
            .iter()
            .map(|path| {
                path.windows(2)
                    .map(|pair| distance2(points[pair[0]], points[pair[1]]).sqrt())
                    .sum()
            })
            .collect();
        let mut node_paths = vec![Vec::new(); points.len()];
        for (id, path) in paths.iter().enumerate() {
            for &node in path {
                if node_paths[node].last() != Some(&id) {
                    node_paths[node].push(id);
                }
            }
        }
        let assignments = models
            .iter()
            .map(|m| {
                tree.nearest(curve_point(m, 0.))
                    .filter(|(d, _)| *d <= 1.5)
                    .map(|(_, i)| node_paths[i].clone())
                    .unwrap_or_default()
            })
            .collect();
        let bridges = bridges
            .into_iter()
            .map(|(edge, model)| (segment_contours[edge_paths[edge]], model))
            .collect();
        Ok(Self {
            lengths,
            assignments,
            points,
            source_size: [mask.w, mask.h],
            paths,
            bridges,
        })
    }
    /// Count distinct in-bounds target pixels along the complete projected
    /// polyline. Endpoints, closed-loop joins and repeated visits count once.
    pub fn pixel_counts(&self, scale: [f64; 2]) -> Vec<usize> {
        let [w, h] =
            std::array::from_fn(|i| (self.source_size[i] as f64 * scale[i]).round() as i64);
        let projected: Vec<_> = self
            .points
            .iter()
            .map(|p| std::array::from_fn(|i| ((p[i] + 0.5) * scale[i]).floor() as i64))
            .collect();
        self.paths
            .iter()
            .map(|path| {
                let mut pixels = HashSet::new();
                let mut visit = |x, y| {
                    if x >= 0 && y >= 0 && x < w && y < h {
                        pixels.insert((x, y));
                    }
                };
                if let Some(&node) = path.first() {
                    let [x, y] = projected[node];
                    visit(x, y);
                }
                for pair in path.windows(2) {
                    let (mut a, mut b) = (projected[pair[0]], projected[pair[1]]);
                    // Canonical segment direction makes half-pixel tie choices
                    // independent of how a source contour was traversed.
                    if a > b {
                        std::mem::swap(&mut a, &mut b);
                    }
                    crate::tools::scale::game_asset::raster::line_pixels(
                        a[0], a[1], b[0], b[1], &mut visit,
                    );
                }
                pixels.len()
            })
            .collect()
    }
    fn selected(&self, scale: [f64; 2], max_removed: usize) -> Vec<bool> {
        self.pixel_counts(scale)
            .into_iter()
            .map(|pixels| pixels > max_removed)
            .collect()
    }

    pub fn retain_with_ids(
        &self,
        models: &[Model],
        scale: [f64; 2],
        max_removed: usize,
    ) -> (Vec<Model>, Vec<usize>) {
        let selected = self.selected(scale, max_removed);
        let mut retained = Vec::new();
        let mut owners = Vec::new();
        for (model, ids) in models.iter().zip(&self.assignments) {
            if let Some(id) = ids
                .iter()
                .copied()
                .filter(|&id| selected[id])
                .max_by(|&a, &b| self.lengths[a].total_cmp(&self.lengths[b]).then(b.cmp(&a)))
            {
                retained.push(*model);
                owners.push(id);
            }
        }
        for (id, model) in &self.bridges {
            if selected[*id] {
                retained.push(*model);
                owners.push(*id);
            }
        }
        (retained, owners)
    }

    pub fn sample_owners(
        &self,
        samples: &[crate::tools::scale::game_asset::detect::Sample],
    ) -> Vec<Option<usize>> {
        let mut owners = vec![None; self.points.len()];
        for (id, path) in self.paths.iter().enumerate() {
            for &node in path {
                if owners[node].is_none_or(|old| self.lengths[id] > self.lengths[old]) {
                    owners[node] = Some(id);
                }
            }
        }
        let tree = Spatial::new(self.points.clone(), 2.);
        samples
            .iter()
            .map(|s| {
                tree.nearest([s[0], s[1]])
                    .filter(|(d, _)| *d <= 1.5)
                    .and_then(|(_, i)| owners[i])
            })
            .collect()
    }

    /// Partition the detected ink footprint by its nearest source trace. Ink
    /// owned by a discarded curve must remain in the source color lookup.
    pub fn retained_ink_mask(
        &self,
        footprint: &Mask,
        scale: [f64; 2],
        cancel: &CancellationToken,
    ) -> Result<Mask> {
        let selected = self.selected(scale, MAX_SHORT_PIXELS);
        let mut retained_node = vec![false; self.points.len()];
        for (id, path) in self.paths.iter().enumerate() {
            for &node in path {
                retained_node[node] |= selected[id];
            }
        }
        let tree = Spatial::new(self.points.clone(), 2.);
        let mut mask = Mask::new(footprint.w, footprint.h);
        for (i, &on) in footprint.data.iter().enumerate() {
            if i % 4096 == 0 {
                cancel.check()?;
            }
            if !on {
                continue;
            }
            let p = [(i % footprint.w) as f64, (i / footprint.w) as f64];
            if let Some((d, node)) = tree.nearest(p) {
                // At an ownership tie with discarded ink, keep the source.
                mask.data[i] = retained_node[node]
                    && tree.radius(p, d + 1e-9).iter().all(|&j| retained_node[j]);
            }
        }
        Ok(mask)
    }
}
