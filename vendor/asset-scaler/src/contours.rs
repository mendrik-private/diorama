//! Trace curves and join unambiguous, tangent-compatible continuations.
use crate::{Cancellation, Result};
mod merge;
use crate::{
    cleanup,
    detect::{Model, curve_point},
    field::{Spatial, distance2},
    raster::Mask,
    smoothing::{self, SplineFit},
};
use std::collections::HashSet;

/// Target lines shorter than this many pixels are dropped at every size.
/// A fixed target-space minimum removes proportionally more source detail
/// the smaller the output gets.
pub const MIN_LINE_PIXELS: usize = 3;
/// The longest contour, in distinct target pixels, that is dropped.
pub const MAX_SHORT_PIXELS: usize = MIN_LINE_PIXELS - 1;

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
    pub fn new(mask: &Mask, models: &[Model], cancel: &dyn Cancellation) -> Result<Self> {
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
                    crate::raster::line_pixels(a[0], a[1], b[0], b[1], &mut visit);
                }
                pixels.len()
            })
            .collect()
    }
    /// How many distinct contours pass through each trace node.
    fn node_occurrences(&self) -> Vec<usize> {
        let mut occurrences = vec![0usize; self.points.len()];
        for path in &self.paths {
            let mut unique = path.clone();
            unique.sort_unstable();
            unique.dedup();
            for node in unique {
                occurrences[node] += 1;
            }
        }
        occurrences
    }

    /// Contours longer than `max_removed` target pixels. A shorter contour
    /// survives only as a connector: an open piece whose both ends join kept
    /// contours, so dropping it would interrupt a longer drawn line.
    fn selected(&self, scale: [f64; 2], max_removed: usize) -> Vec<bool> {
        let counts = self.pixel_counts(scale);
        let mut joins_long = vec![0usize; self.points.len()];
        for (path, _) in self
            .paths
            .iter()
            .zip(&counts)
            .filter(|&(_, &pixels)| pixels > max_removed)
        {
            let mut unique = path.clone();
            unique.sort_unstable();
            unique.dedup();
            for node in unique {
                joins_long[node] += 1;
            }
        }
        self.paths
            .iter()
            .zip(counts)
            .map(|(path, pixels)| {
                pixels > max_removed
                    || (pixels > 0
                        && match (path.first(), path.last()) {
                            (Some(&first), Some(&last)) => {
                                first != last && joins_long[first] > 0 && joins_long[last] > 0
                            }
                            _ => false,
                        })
            })
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

    /// Explain each retained ordered digital trace with a small collection of
    /// bounded cubic curves. The renderer later approximates them as
    /// quadratics. This consumes the trace graph rather than
    /// the detector's overlapping local patches, so a long hood or bow edge
    /// can become one flowing curve while junction endpoints stay shared.
    pub fn polished(
        &self,
        scale: [f64; 2],
        max_removed: usize,
        cancel: &dyn Cancellation,
    ) -> Result<SplineFit> {
        let selected = self.selected(scale, max_removed);
        // Bound in the least-reduced target axis.  With [1, 0.25] a
        // source-space fit may move by at most the source-sized allowance,
        // rather than by four target pixels along the unreduced axis.
        let scale = scale[0].max(scale[1]);
        let occurrences = self.node_occurrences();
        let mut trace_donors = Vec::new();
        let paths = self
            .paths
            .iter()
            .enumerate()
            .filter(|&(owner, _)| selected[owner])
            .map(|(owner, path)| {
                let points: Vec<_> = path.iter().map(|&node| self.points[node]).collect();
                trace_donors.extend(points.iter().copied().map(|p| (p, owner)));
                (
                    owner,
                    points,
                    path.iter()
                        .enumerate()
                        .filter_map(|(index, &node)| (occurrences[node] > 1).then_some(index))
                        .collect(),
                )
            });
        let mut fitted = smoothing::fit_paths(paths, scale, cancel)?;
        fitted.trace_donors = trace_donors;
        Ok(fitted)
    }

    pub fn sample_owners(&self, samples: &[crate::detect::Sample]) -> Vec<Option<usize>> {
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
        cancel: &dyn Cancellation,
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

#[cfg(test)]
mod tests {
    use super::{Contours, MAX_SHORT_PIXELS};
    use crate::{CancellationToken, raster::Mask};

    #[test]
    fn a_short_connector_between_lines_survives_but_a_short_spur_drops() {
        // Two long lines joined by a short connector (2-3), plus a short
        // dangling spur (1-4) off the first line.
        let contours = Contours {
            points: vec![[0., 0.], [16., 0.], [17., 0.], [36., 0.], [16., 2.]],
            paths: vec![vec![0, 1], vec![1, 2], vec![2, 3], vec![1, 4]],
            lengths: vec![16., 1., 19., 2.],
            assignments: vec![vec![0], vec![1], vec![2], vec![3]],
            source_size: [40, 8],
            bridges: Vec::new(),
        };
        let counts = contours.pixel_counts([0.5, 0.5]);
        assert!(
            counts[1] <= MAX_SHORT_PIXELS && counts[3] <= MAX_SHORT_PIXELS,
            "{counts:?}"
        );
        assert_eq!(
            contours.selected([0.5, 0.5], MAX_SHORT_PIXELS),
            [true, true, true, false]
        );
    }

    #[test]
    fn short_contours_drop_with_resolution_in_both_render_and_fill_paths() {
        let contours = Contours {
            // At 50% scale these project to four and three target pixels.
            points: vec![[1., 1.], [7., 1.], [9., 1.], [13., 1.]],
            paths: vec![vec![0, 1], vec![2, 3]],
            lengths: vec![6., 4.],
            assignments: vec![vec![0], vec![1]],
            source_size: [16, 4],
            bridges: Vec::new(),
        };
        let models = vec![[0.; 10], [1.; 10]];
        let half = [0.5, 0.5];
        assert_eq!(contours.pixel_counts(half), vec![4, 3]);
        let (retained, owners) = contours.retain_with_ids(&models, half, MAX_SHORT_PIXELS);
        assert_eq!(retained.len(), 2);
        assert_eq!(owners, vec![0, 1]);

        let mut footprint = Mask::new(16, 4);
        for x in 1..=7 {
            footprint.data[16 + x] = true;
        }
        for x in 9..=13 {
            footprint.data[16 + x] = true;
        }
        let retained = contours
            .retained_ink_mask(&footprint, half, &CancellationToken::default())
            .unwrap();
        assert!((1..=7).all(|x| retained.data[16 + x]));
        assert!((9..=13).all(|x| retained.data[16 + x]));

        // Both curves project to two pixels at 25%, so resolution removes
        // their redraw and halo-mask membership.
        let quarter = [0.25, 0.25];
        assert_eq!(contours.pixel_counts(quarter), vec![2, 2]);
        let (retained, owners) = contours.retain_with_ids(&models, quarter, MAX_SHORT_PIXELS);
        assert!(retained.is_empty());
        assert!(owners.is_empty());
        let retained = contours
            .retained_ink_mask(&footprint, quarter, &CancellationToken::default())
            .unwrap();
        assert!(retained.data.iter().all(|&on| !on));
    }
}
