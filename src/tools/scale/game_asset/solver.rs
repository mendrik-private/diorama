use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::sync::Arc;

use image::{Rgba, RgbaImage};

use super::analysis::{Analysis, Feature, Kind, components, distance, neighbours};
use super::geometry::{self, Point};
use super::{Diagnostics, Grid, Options, Provenance, Scaled};
use crate::document::CancellationToken;
use crate::error::Result;

#[derive(Clone, Debug)]
struct Proposal {
    feature: usize,
    index: usize,
    path: Vec<usize>,
    patch: BTreeMap<usize, usize>,
    support: BTreeSet<usize>,
    members: Arc<BTreeMap<usize, (f32, usize)>>,
    cost: f32,
    value: f32,
}

struct Candidates {
    grid: Grid,
    baseline: Vec<usize>,
    phases: u32,
    cache: BTreeMap<usize, Vec<usize>>,
}

impl Candidates {
    fn phases(&mut self, p: usize) -> &[usize] {
        let baseline = self.baseline[p];
        self.cache.entry(p).or_insert_with(|| {
            let mut result = vec![baseline, self.grid.baseline(p)];
            let n = self.phases as u64;
            for b in 0..n {
                for a in 0..n {
                    let x = ((p % self.grid.width) as u64 * 2 * n + 2 * a + 1)
                        * self.grid.source_width as u64
                        / (self.grid.width as u64 * 2 * n);
                    let y = ((p / self.grid.width) as u64 * 2 * n + 2 * b + 1)
                        * self.grid.source_height as u64
                        / (self.grid.height as u64 * 2 * n);
                    result.push(y as usize * self.grid.source_width + x as usize);
                }
            }
            result.sort_unstable();
            result.dedup();
            result
        })
    }

    fn compatible(
        &mut self,
        p: usize,
        mut predicate: impl FnMut(usize) -> bool,
        cancellation: &CancellationToken,
    ) -> Result<Vec<usize>> {
        let mut result: Vec<_> = self
            .phases(p)
            .iter()
            .copied()
            .filter(|q| predicate(*q))
            .collect();
        if result.is_empty() {
            let (xs, ys) = self.grid.footprint(p);
            for y in ys {
                cancellation.check()?;
                for x in xs.clone() {
                    let q = y * self.grid.source_width + x;
                    if predicate(q) {
                        result.push(q);
                    }
                }
            }
            // Retain every discovered phase for later proposals of this pixel.
            let values = self.cache.entry(p).or_default();
            values.extend(&result);
            values.sort_unstable();
            values.dedup();
        }
        Ok(result)
    }
}

/// Local membership keeps nearby, similarly coloured parallel features distinct.
/// The path index retained here also gives the DP its source traversal order.
fn membership(
    feature: &Feature,
    analysis: &Analysis,
    grid: Grid,
    options: &Options,
    cancellation: &CancellationToken,
) -> Result<BTreeMap<usize, (f32, usize)>> {
    let radius = feature.half_width.ceil().max(1.0) as usize;
    let mut result = BTreeMap::new();
    for (order, &p) in feature.path.iter().enumerate() {
        cancellation.check()?;
        let (x, y) = (p % grid.source_width, p / grid.source_width);
        for iy in y.saturating_sub(radius)..=(y + radius).min(grid.source_height - 1) {
            for ix in x.saturating_sub(radius)..=(x + radius).min(grid.source_width - 1) {
                let q = iy * grid.source_width + ix;
                if !analysis.occupancy[q] || analysis.foreground[q] != feature.component {
                    continue;
                }
                let d = (ix.abs_diff(x) as f32).hypot(iy.abs_diff(y) as f32);
                if d > feature.half_width.max(0.75) {
                    continue;
                }
                if feature.kind == Kind::Stroke
                    && distance(analysis.lab[q], analysis.lab[p]) > options.tau_colour
                {
                    continue;
                }
                let value = (d, order);
                if result.get(&q).is_none_or(|old| value < *old) {
                    result.insert(q, value);
                }
            }
        }
    }
    Ok(result)
}

fn ordered_labels(
    path: &[usize],
    feature: &Feature,
    members: &BTreeMap<usize, (f32, usize)>,
    analysis: &Analysis,
    candidates: &mut Candidates,
    options: &Options,
    cancellation: &CancellationToken,
) -> Result<Option<(Vec<usize>, f32)>> {
    let grid = candidates.grid;
    let mut rows: Vec<Vec<(usize, f32, usize)>> = Vec::new();
    for (step, &p) in path.iter().enumerate() {
        cancellation.check()?;
        let location = Point {
            x: (p % grid.width) as f32,
            y: (p / grid.width) as f32,
        };
        let source_samples =
            candidates.compatible(p, |q| members.contains_key(&q), cancellation)?;
        if source_samples.is_empty() {
            return Ok(None);
        }
        let tangent = if step + 1 < path.len() {
            let q = path[step + 1];
            ((q / grid.width) as f32 - location.y).atan2((q % grid.width) as f32 - location.x)
        } else if step > 0 {
            let q = path[step - 1];
            (location.y - (q / grid.width) as f32).atan2(location.x - (q % grid.width) as f32)
        } else {
            0.0
        };
        let mut row: Vec<_> = source_samples
            .into_iter()
            .map(|q| {
                let e = analysis.evidence[q][0];
                let transformed = (e.tangent.sin() / grid.sy).atan2(e.tangent.cos() / grid.sx);
                let unary = members[&q].0 / feature.half_width.max(1.0)
                    + if feature.kind == Kind::Stroke {
                        options.orientation_cost * (1.0 - (tangent - transformed).cos().abs())
                            + (1.0 - e.score) * options.evidence_cost
                    } else {
                        options.evidence_cost * (1.0 - e.edge)
                    }
                    + options.baseline_cost * f32::from(q != candidates.baseline[p]);
                (q, unary, usize::MAX)
            })
            .collect();
        // Bound the Viterbi state count, with coordinate ties rather than colour
        // deduplication. An adaptive footprint may contain many eligible pixels.
        row.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        row.truncate(16);
        if let Some(previous) = rows.last() {
            for state in &mut row {
                let mut best = (f32::INFINITY, usize::MAX);
                for (k, &(q, cost, _)) in previous.iter().enumerate() {
                    let order_a = members[&q].1;
                    let order_b = members[&state.0].1;
                    // The last step of a closed contour returns to its anchor.
                    let closing = feature.closed && step + 1 == path.len();
                    if order_b < order_a && !closing {
                        continue;
                    }
                    let expected = distance(
                        analysis.lab[feature.path[order_a]],
                        analysis.lab[feature.path[order_b]],
                    );
                    let jump = (distance(analysis.lab[q], analysis.lab[state.0]) - expected)
                        .max(0.0)
                        / options.tau_along;
                    let candidate_cost = cost + options.transition_cost * jump;
                    if candidate_cost < best.0 {
                        best = (candidate_cost, k);
                    }
                }
                state.1 += best.0;
                state.2 = best.1;
            }
        }
        rows.push(row);
    }
    let Some((mut selected, best)) = rows.last().and_then(|row| {
        row.iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)))
    }) else {
        return Ok(None);
    };
    if !best.1.is_finite() {
        return Ok(None);
    }
    let cost = best.1 / path.len() as f32;
    let mut labels = vec![0; path.len()];
    for i in (0..rows.len()).rev() {
        labels[i] = rows[i][selected].0;
        selected = rows[i][selected].2;
    }
    Ok(Some((labels, cost)))
}

#[derive(Clone, Copy, Debug)]
struct RouteEntry {
    cost: f32,
    state: usize,
}
impl PartialEq for RouteEntry {
    fn eq(&self, other: &Self) -> bool {
        self.cost.total_cmp(&other.cost).is_eq() && self.state == other.state
    }
}
impl Eq for RouteEntry {}
impl PartialOrd for RouteEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RouteEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .cost
            .total_cmp(&self.cost)
            .then(other.state.cmp(&self.state))
    }
}

/// Search output cells and source labels together, inside the source-supported
/// corridor. Unlike rasterize-then-label, this can step around an empty cell.
/// Positive edge costs and monotone source order prevent folds/backtracking.
struct RouteGuide<'a> {
    members: &'a BTreeMap<usize, (f32, usize)>,
    corridor: &'a BTreeSet<usize>,
    fit: &'a [Point],
}

fn routed_path(
    feature: &Feature,
    guide: RouteGuide<'_>,
    analysis: &Analysis,
    candidates: &mut Candidates,
    options: &Options,
    diagnostics: &mut Diagnostics,
    cancellation: &CancellationToken,
) -> Result<Option<Vec<usize>>> {
    let RouteGuide {
        members,
        corridor,
        fit,
    } = guide;
    if feature.closed || fit.len() < 2 {
        return Ok(None);
    }
    let grid = candidates.grid;
    let first = grid.project(feature.path[0]);
    let last = grid.project(*feature.path.last().expect("nonempty feature"));
    let point = |p: usize| Point {
        x: (p % grid.width) as f32,
        y: (p / grid.width) as f32,
    };
    // Four distinct source orders per cell bound the search. Keep alternatives
    // in label selection below; colours are never synthesized.
    let mut nodes = Vec::<(usize, usize, f32)>::new();
    let mut cells = BTreeMap::<usize, std::ops::Range<usize>>::new();
    for &p in corridor {
        cancellation.check()?;
        let deviation = fit
            .windows(2)
            .map(|s| geometry::segment_distance(point(p), s[0], s[1]))
            .fold(f32::INFINITY, f32::min);
        if deviation > options.fit_tolerance {
            continue;
        }
        let mut samples = candidates.compatible(p, |q| members.contains_key(&q), cancellation)?;
        let unary = |q: usize| {
            members[&q].0 / feature.half_width.max(1.0)
                + options.baseline_cost * f32::from(q != candidates.baseline[p])
                + options.geometry_cost * deviation
        };
        samples.sort_unstable_by(|a, b| unary(*a).total_cmp(&unary(*b)).then(a.cmp(b)));
        let start = nodes.len();
        let mut orders = BTreeSet::new();
        for q in samples {
            if orders.insert(members[&q].1) {
                nodes.push((p, q, unary(q)));
                if nodes.len() - start == 4 {
                    break;
                }
            }
        }
        if nodes.len() > start {
            cells.insert(p, start..nodes.len());
        }
    }
    let anchor = |source: usize, endpoint: Point| {
        cells
            .keys()
            .copied()
            .filter(|p| point(*p).distance(endpoint) <= options.fit_tolerance)
            .filter(|p| {
                !analysis.anchors.contains(&source)
                    || (point(*p).x == endpoint.x.round() && point(*p).y == endpoint.y.round())
            })
            .min_by(|a, b| {
                point(*a)
                    .distance(endpoint)
                    .total_cmp(&point(*b).distance(endpoint))
                    .then(a.cmp(b))
            })
    };
    let (Some(start), Some(end)) = (
        anchor(feature.path[0], first),
        anchor(*feature.path.last().expect("nonempty feature"), last),
    ) else {
        return Ok(None);
    };
    if start == end {
        return Ok(None);
    }
    let mut distances = vec![f32::INFINITY; nodes.len()];
    let mut previous = vec![usize::MAX; nodes.len()];
    let mut heap = BinaryHeap::new();
    for state in cells[&start].clone() {
        distances[state] = nodes[state].2;
        heap.push(RouteEntry {
            cost: distances[state],
            state,
        });
    }
    let mut attempts = 0;
    while let Some(RouteEntry { cost, state }) = heap.pop() {
        if cost > distances[state] {
            continue;
        }
        attempts += 1;
        if attempts % 128 == 0 {
            cancellation.check()?;
        }
        if attempts > options.search_budget {
            diagnostics.search_budget_exhausted = true;
            return Ok(None);
        }
        let (p, source, _) = nodes[state];
        if p == end {
            let mut path = Vec::new();
            let mut current = state;
            loop {
                path.push(nodes[current].0);
                if previous[current] == usize::MAX {
                    break;
                }
                current = previous[current];
            }
            path.reverse();
            if !geometry::simple_path(&path, grid.width, grid.height) {
                return Ok(None);
            }
            return Ok(Some(path));
        }
        for neighbour in neighbours(p, grid.width, grid.height, true) {
            let Some(states) = cells.get(&neighbour) else {
                continue;
            };
            for next in states.clone() {
                let (_, q, unary) = nodes[next];
                if members[&q].1 < members[&source].1 {
                    continue;
                }
                let expected = distance(
                    analysis.lab[feature.path[members[&source].1]],
                    analysis.lab[feature.path[members[&q].1]],
                );
                let jump = (distance(analysis.lab[source], analysis.lab[q]) - expected).max(0.0)
                    / options.tau_along;
                let order = members[&q].1;
                let a = grid.project(feature.path[order.saturating_sub(4)]);
                let b = grid.project(feature.path[(order + 4).min(feature.path.len() - 1)]);
                let step = point(neighbour);
                let origin = point(p);
                let dot = ((b.x - a.x) * (step.x - origin.x) + (b.y - a.y) * (step.y - origin.y))
                    / (a.distance(b).max(1e-6) * origin.distance(step));
                let direction_cost = (1.0 - dot.clamp(-1.0, 1.0)) * options.orientation_cost;
                let next_cost = cost
                    + point(p).distance(point(neighbour))
                    + unary
                    + options.transition_cost * jump
                    + direction_cost;
                if next_cost < distances[next] {
                    distances[next] = next_cost;
                    previous[next] = state;
                    heap.push(RouteEntry {
                        cost: next_cost,
                        state: next,
                    });
                }
            }
        }
    }
    Ok(None)
}

fn proposals(
    id: usize,
    analysis: &Analysis,
    grid: Grid,
    options: &Options,
    candidates: &mut Candidates,
    diagnostics: &mut Diagnostics,
    cancellation: &CancellationToken,
) -> Result<Vec<Proposal>> {
    let feature = &analysis.features[id];
    let points: Vec<_> = feature.path.iter().map(|p| grid.project(*p)).collect();
    let length = points.windows(2).map(|p| p[0].distance(p[1])).sum::<f32>();
    if length < options.minimum_path_length {
        return Ok(Vec::new());
    }
    let members = Arc::new(membership(feature, analysis, grid, options, cancellation)?);
    // Reserve half the tolerance for the placement alternatives.
    let fit = if options.fit_geometry {
        geometry::fit(&points, options.fit_tolerance / 2.0)
    } else {
        points.clone()
    };
    let mut corridor = BTreeSet::new();
    for &p in members.keys() {
        let x = p % grid.source_width;
        let y = p / grid.source_width;
        // A source cell may overlap two target footprints on a fractional edge.
        let left = x * grid.width / grid.source_width;
        let right = ((x + 1) * grid.width).div_ceil(grid.source_width);
        let top = y * grid.height / grid.source_height;
        let bottom = ((y + 1) * grid.height).div_ceil(grid.source_height);
        for oy in top..bottom {
            for ox in left..right {
                corridor.insert(oy * grid.width + ox);
            }
        }
    }
    let offsets = &[
        (0.0, 0.0, 0.0),
        (-0.25, 0.0, 0.0),
        (0.25, 0.0, 0.0),
        (-0.5, 0.0, 0.0),
        (0.5, 0.0, 0.0),
        (0.0, -0.5, -0.5),
        (0.0, 0.5, 0.5),
        (0.0, -0.5, 0.5),
    ];
    let mut result = Vec::new();
    let routed = if options.fit_geometry {
        routed_path(
            feature,
            RouteGuide {
                members: &members,
                corridor: &corridor,
                fit: &fit,
            },
            analysis,
            candidates,
            options,
            diagnostics,
            cancellation,
        )?
    } else {
        None
    };
    for (index, &(offset, start_offset, end_offset)) in
        offsets.iter().take(options.maximum_proposals).enumerate()
    {
        cancellation.check()?;
        let mut model = fit.clone();
        for i in 0..model.len() {
            // Shared endpoints are locked to the same rounded graph anchor.
            if i == 0 || i + 1 == model.len() {
                continue;
            }
            let previous = fit[i - 1];
            let next = fit[i + 1];
            let dx = next.x - previous.x;
            let dy = next.y - previous.y;
            let norm = dx.hypot(dy).max(1e-6);
            model[i] = fit[i].shifted(-dy / norm * offset, dx / norm * offset);
        }
        // For a straight segment, translate its interior via two extra knots;
        // endpoints remain locked to retain junction and endpoint constraints.
        if model.len() == 2 && offset != 0.0 {
            let a = model[0];
            let b = model[1];
            let dx = b.x - a.x;
            let dy = b.y - a.y;
            let norm = dx.hypot(dy).max(1e-6);
            model.insert(
                1,
                a.shifted(dx / 3.0 - dy / norm * offset, dy / 3.0 + dx / norm * offset),
            );
            model.insert(
                2,
                a.shifted(
                    2.0 * dx / 3.0 - dy / norm * offset,
                    2.0 * dy / 3.0 + dx / norm * offset,
                ),
            );
        }
        let last = model.len() - 1;
        for (vertex, other, source_endpoint, shift) in [
            (0, 1, feature.path[0], start_offset),
            (
                last,
                last - 1,
                feature.path[feature.path.len() - 1],
                end_offset,
            ),
        ] {
            if shift == 0.0 || feature.closed || analysis.anchors.contains(&source_endpoint) {
                continue;
            }
            let dx = model[other].x - model[vertex].x;
            let dy = model[other].y - model[vertex].y;
            let norm = dx.hypot(dy).max(1e-6);
            model[vertex] = model[vertex].shifted(-dy / norm * shift, dx / norm * shift);
        }
        if points.iter().any(|p| {
            model
                .windows(2)
                .map(|s| geometry::segment_distance(*p, s[0], s[1]))
                .fold(f32::INFINITY, f32::min)
                > options.fit_tolerance
        }) {
            continue;
        }
        let Some(path) = (if index == 0 && routed.is_some() {
            routed.clone()
        } else {
            geometry::raster(&model, grid.width, grid.height)
        }) else {
            continue;
        };
        if path.len() < 2 || !geometry::connected(&path, grid.width) {
            continue;
        }
        let Some((labels, mut cost)) = ordered_labels(
            &path,
            feature,
            &members,
            analysis,
            candidates,
            options,
            cancellation,
        )?
        else {
            diagnostics.unsupported_gaps += 1;
            continue;
        };
        let mut patch: BTreeMap<_, _> = path.iter().copied().zip(labels).collect();
        let tangent = analysis.evidence[feature.path[feature.path.len() / 2]][0].tangent;
        let tangent_scale = (tangent.cos() / grid.sx).hypot(tangent.sin() / grid.sy);
        let projected_width = (2.0 * feature.half_width / (grid.sx * grid.sy * tangent_scale))
            .round()
            .max(1.0) as usize;
        let radius = if feature.kind == Kind::Stroke {
            projected_width.saturating_sub(1) as f32 / 2.0
        } else {
            0.0
        };
        let mut feasible = true;
        if radius > 0.0 {
            for &p in &path {
                let x = p % grid.width;
                let y = p / grid.width;
                let extent = radius.ceil() as usize;
                for iy in y.saturating_sub(extent)..=(y + extent).min(grid.height - 1) {
                    for ix in x.saturating_sub(extent)..=(x + extent).min(grid.width - 1) {
                        let point = Point {
                            x: ix as f32,
                            y: iy as f32,
                        };
                        if model
                            .windows(2)
                            .map(|s| geometry::segment_distance(point, s[0], s[1]))
                            .fold(f32::INFINITY, f32::min)
                            > radius
                        {
                            continue;
                        }
                        let p = iy * grid.width + ix;
                        let samples =
                            candidates.compatible(p, |q| members.contains_key(&q), cancellation)?;
                        if let Some(&q) = samples
                            .iter()
                            .min_by_key(|q| (usize::from(**q != candidates.baseline[p]), **q))
                        {
                            patch.insert(p, q);
                        } else {
                            feasible = false;
                        }
                    }
                }
            }
        }
        if !feasible {
            diagnostics.unsupported_gaps += 1;
            continue;
        }
        let support: BTreeSet<_> = patch.keys().copied().collect();
        // A complete corridor patch removes displaced baseline stroke samples;
        // otherwise an offset proposal would simply double the original line.
        for &p in &corridor {
            if patch.contains_key(&p) || !members.contains_key(&candidates.baseline[p]) {
                continue;
            }
            let original = candidates.baseline[p];
            let side = geometry::side(
                Point {
                    x: (p % grid.width) as f32,
                    y: (p / grid.width) as f32,
                },
                &model,
            );
            let samples = candidates.compatible(
                p,
                |q| {
                    !members.contains_key(&q)
                        && (feature.kind == Kind::Silhouette
                            || side * geometry::side(grid.project(q), &fit) >= 0.0)
                        && (analysis.foreground[q] == feature.component || !analysis.occupancy[q])
                        && (feature.kind == Kind::Silhouette
                            || analysis.evidence[q][0].score < options.continuation_threshold)
                },
                cancellation,
            )?;
            let fill = samples.iter().copied().min_by(|a, b| {
                let score = |q: usize| {
                    distance(analysis.lab[q], analysis.lab[original]) / options.tau_colour
                        + f32::from(analysis.occupancy[q] != analysis.occupancy[original])
                };
                score(*a).total_cmp(&score(*b)).then(a.cmp(b))
            });
            if let Some(q) = fill {
                patch.insert(p, q);
                cost += options.baseline_cost / path.len() as f32;
            } else {
                // A fully occupied stroke footprint cannot be vacated. Retain
                // a wider supported ribbon and charge the added width.
                patch.insert(p, original);
                cost += options.baseline_cost;
            }
        }
        let error = points
            .iter()
            .map(|p| {
                model
                    .windows(2)
                    .map(|s| geometry::segment_distance(*p, s[0], s[1]))
                    .fold(f32::INFINITY, f32::min)
            })
            .sum::<f32>()
            / points.len() as f32;
        cost += options.geometry_cost * error
            + options.complexity_cost * model.len().saturating_sub(2) as f32;
        let value = feature.confidence * length.min(options.length_cap);
        if result.iter().any(|p: &Proposal| p.patch == patch) {
            continue;
        }
        result.push(Proposal {
            feature: id,
            index,
            path,
            patch,
            support,
            members: members.clone(),
            cost,
            value,
        });
    }
    Ok(result)
}

fn related(a: usize, b: usize, analysis: &Analysis) -> bool {
    if a == b {
        return true;
    }
    let (a, b) = (&analysis.features[a], &analysis.features[b]);
    // Silhouette occupancy may share pixels with a detected rim or contour.
    if a.kind == Kind::Silhouette || b.kind == Kind::Silhouette {
        return a.component == b.component;
    }
    [a.path.first(), a.path.last()]
        .iter()
        .any(|p| [b.path.first(), b.path.last()].contains(p))
}

fn unrelated_joins(owners: &[Option<usize>], analysis: &Analysis, grid: Grid) -> usize {
    let mut joins = BTreeSet::new();
    for (p, owner) in owners.iter().enumerate() {
        let Some(a) = owner else {
            continue;
        };
        for q in neighbours(p, grid.width, grid.height, true).filter(|q| *q > p) {
            if let Some(b) = owners[q]
                && !related(*a, b, analysis)
            {
                joins.insert(((*a).min(b), (*a).max(b)));
            }
        }
    }
    joins.len()
}

fn represented(
    proposal: &Proposal,
    selected: &[usize],
    analysis: &Analysis,
    _grid: Grid,
    options: &Options,
) -> f32 {
    let feature = &analysis.features[proposal.feature];
    continuity_coverage(
        proposal.path.iter().map(|&p| {
            let q = selected[p];
            let expected = proposal.patch[&p];
            (feature.kind == Kind::Silhouette || proposal.members.contains_key(&q))
                && analysis.occupancy[q]
                && analysis.foreground[q] == feature.component
                && (feature.kind == Kind::Silhouette
                    || distance(analysis.lab[q], analysis.lab[expected]) < options.tau_colour)
        }),
        feature.closed,
    )
}

/// Equal pixel counts must not make scattered samples as valuable as a line.
/// Streaming keeps this allocation-free in the greedy ranking hot path.
fn continuity_coverage(present: impl Iterator<Item = bool>, closed: bool) -> f32 {
    let mut length = 0;
    let mut count = 0;
    let mut run = 0;
    let mut longest = 0;
    let mut prefix = 0;
    let mut prefix_open = true;
    for supported in present {
        length += 1;
        if supported {
            count += 1;
            run += 1;
            longest = longest.max(run);
            if prefix_open {
                prefix = run;
            }
        } else {
            run = 0;
            prefix_open = false;
        }
    }
    // Missing endpoints and disconnected runs count as lost coverage, so a
    // short easy fragment cannot outrank the full supported source feature.
    if closed {
        longest = longest.max((prefix + run).min(length));
    }
    (count + longest) as f32 / (2 * length.max(1)) as f32
}

// Short details have no fitted proposal, but may already survive the NN
// baseline. Report those samples accurately instead of calling them dropped.
fn unfitted_coverage(
    feature: &Feature,
    selected: &[usize],
    analysis: &Analysis,
    grid: Grid,
    options: &Options,
) -> f32 {
    let mut cells = BTreeMap::<usize, bool>::new();
    for &q in &feature.path {
        let point = grid.project(q);
        let x = point.x.round().clamp(0.0, (grid.width - 1) as f32) as usize;
        let y = point.y.round().clamp(0.0, (grid.height - 1) as f32) as usize;
        let p = y * grid.width + x;
        let s = selected[p];
        let d = ((q % grid.source_width).abs_diff(s % grid.source_width) as f32)
            .hypot((q / grid.source_width).abs_diff(s / grid.source_width) as f32);
        let present = analysis.occupancy[s]
            && analysis.foreground[s] == feature.component
            && (feature.kind == Kind::Silhouette
                || (d <= feature.half_width.max(0.75)
                    && distance(analysis.lab[s], analysis.lab[q]) <= options.tau_colour));
        *cells.entry(p).or_default() |= present;
    }
    cells.values().filter(|v| **v).count() as f32 / cells.len().max(1) as f32
}

fn gain(
    proposal: &Proposal,
    selected: &[usize],
    analysis: &Analysis,
    grid: Grid,
    options: &Options,
) -> f32 {
    proposal.value
        * options.coverage_cost
        * (1.0 - represented(proposal, selected, analysis, grid, options))
        - proposal.cost
}

struct TopologyLoss {
    components: BTreeSet<(u32, u32)>,
    holes: BTreeSet<u32>,
}

fn topology_loss(
    selected: &[usize],
    analysis: &Analysis,
    grid: Grid,
    cancellation: &CancellationToken,
) -> Result<TopologyLoss> {
    let mask: Vec<_> = selected.iter().map(|p| analysis.occupancy[*p]).collect();
    let foreground = components(&mask, grid.width, true, cancellation)?;
    let background = components(
        &mask.iter().map(|v| !v).collect::<Vec<_>>(),
        grid.width,
        false,
        cancellation,
    )?;
    let mut component_mapping: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    let mut output_owners: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    let mut border = BTreeSet::new();
    for (p, &q) in selected.iter().enumerate() {
        if foreground[p] != 0 {
            component_mapping
                .entry(analysis.foreground[q])
                .or_default()
                .insert(foreground[p]);
            output_owners
                .entry(foreground[p])
                .or_default()
                .insert(analysis.foreground[q]);
        }
        if p < grid.width
            || p >= selected.len() - grid.width
            || p % grid.width == 0
            || p % grid.width == grid.width - 1
        {
            border.insert(background[p]);
        }
    }
    let source_components: BTreeSet<_> = analysis
        .foreground
        .iter()
        .copied()
        .filter(|p| *p != 0)
        .collect();
    let mut component_losses = BTreeSet::new();
    for id in source_components {
        match component_mapping.get(&id) {
            None => {
                component_losses.insert((id, 0));
            }
            Some(ids) if ids.len() > 1 => {
                component_losses.insert((id, id));
            }
            _ => {}
        }
    }
    for ids in output_owners.values() {
        for a in ids {
            for b in ids.range(a + 1..) {
                component_losses.insert((*a, *b));
            }
        }
    }
    let mut represented_holes = BTreeSet::new();
    for (p, &q) in selected.iter().enumerate() {
        if background[p] != 0 && !border.contains(&background[p]) {
            represented_holes.insert(analysis.background[q]);
        }
    }
    Ok(TopologyLoss {
        components: component_losses,
        holes: analysis
            .holes
            .difference(&represented_holes)
            .copied()
            .collect(),
    })
}

fn valid_edit(
    proposal: &Proposal,
    selected: &[usize],
    owners: &[Option<usize>],
    accepted: &BTreeMap<usize, Proposal>,
    analysis: &Analysis,
    grid: Grid,
    cancellation: &CancellationToken,
) -> Result<bool> {
    for (&p, &q) in &proposal.patch {
        let (xs, ys) = grid.footprint(p);
        if !xs.contains(&(q % grid.source_width)) || !ys.contains(&(q / grid.source_width)) {
            return Ok(false);
        }
        if let Some(owner) = owners[p]
            && !related(owner, proposal.feature, analysis)
            && q != selected[p]
        {
            return Ok(false);
        }
        if analysis.occupancy[q] {
            for n in neighbours(p, grid.width, grid.height, true) {
                let neighbour = proposal.patch.get(&n).copied().unwrap_or(selected[n]);
                if analysis.occupancy[neighbour]
                    && analysis.foreground[q] != analysis.foreground[neighbour]
                {
                    // Diagnose existing joins, but never introduce a new one.
                    if !analysis.occupancy[selected[p]]
                        || analysis.foreground[selected[p]] != analysis.foreground[q]
                    {
                        return Ok(false);
                    }
                }
                if proposal.support.contains(&p)
                    && let Some(other) = owners[n]
                    && !related(other, proposal.feature, analysis)
                    && owners[p] != Some(proposal.feature)
                {
                    return Ok(false);
                }
            }
        }
    }
    for previous in accepted.values() {
        if previous.feature == proposal.feature {
            continue;
        }
        for &p in &previous.path {
            if let Some(&q) = proposal.patch.get(&p)
                && q != selected[p]
                && (!analysis.occupancy[q]
                    || analysis.foreground[q] != analysis.foreground[selected[p]]
                    || !previous.members.contains_key(&q))
            {
                return Ok(false);
            }
        }
    }
    if proposal
        .patch
        .iter()
        .any(|(p, q)| analysis.occupancy[*q] != analysis.occupancy[selected[*p]])
    {
        let before = topology_loss(selected, analysis, grid, cancellation)?;
        let mut tentative = selected.to_vec();
        for (&p, &q) in &proposal.patch {
            tentative[p] = q;
        }
        let after = topology_loss(&tentative, analysis, grid, cancellation)?;
        if !after.components.is_subset(&before.components) || !after.holes.is_subset(&before.holes)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn render(
    source: &RgbaImage,
    grid: Grid,
    analysis: &Analysis,
    options: &Options,
    cancellation: &CancellationToken,
) -> Result<Scaled> {
    let mut selected = super::palette_grid::select(grid, analysis, options, cancellation)?;
    let baseline = selected.clone();
    let mut source_owner = vec![None; analysis.occupancy.len()];
    let mut diagnostics = Diagnostics::default();
    let mut candidates = Candidates {
        grid,
        baseline: baseline.clone(),
        phases: options.phases,
        cache: BTreeMap::new(),
    };
    let mut all = Vec::new();
    for id in 0..analysis.features.len() {
        cancellation.check()?;
        if all.len() >= options.search_budget {
            diagnostics.search_budget_exhausted = true;
            break;
        }
        all.extend(proposals(
            id,
            analysis,
            grid,
            options,
            &mut candidates,
            &mut diagnostics,
            cancellation,
        )?);
    }
    let mut has_proposals = vec![false; analysis.features.len()];
    let mut best_baseline = vec![0.0_f32; analysis.features.len()];
    for p in &all {
        has_proposals[p.feature] = true;
        best_baseline[p.feature] =
            best_baseline[p.feature].max(represented(p, &baseline, analysis, grid, options));
    }
    for (id, feature) in analysis.features.iter().enumerate() {
        if !has_proposals[id] {
            best_baseline[id] = unfitted_coverage(feature, &baseline, analysis, grid, options);
        }
    }
    let mut tagged = BTreeSet::new();
    for proposal in &all {
        if !tagged.insert(proposal.feature)
            || analysis.features[proposal.feature].kind == Kind::Silhouette
        {
            continue;
        }
        for &q in proposal.members.keys() {
            if source_owner[q].is_none() {
                source_owner[q] = Some(proposal.feature);
            }
        }
    }
    let base_owners: Vec<_> = baseline.iter().map(|q| source_owner[*q]).collect();
    let mut owners = base_owners.clone();
    diagnostics.baseline_joins = unrelated_joins(&base_owners, analysis, grid);
    diagnostics.baseline_connectivity_failures =
        best_baseline.iter().filter(|value| **value < 1.0).count();
    let mut accepted = BTreeMap::new();
    let mut attempts = 0;
    let mut ranking = Vec::new();
    let mut changed = true;
    loop {
        cancellation.check()?;
        // Rejection changes neither pixels nor gains. Rank once per accepted
        // image revision, then consume rejected alternatives without rescanning
        // every path. Acceptance invalidates the entire ranking, including any
        // previously rejected proposals, exactly as in the exhaustive solver.
        if changed {
            ranking.clear();
            for p in &all {
                cancellation.check()?;
                if !accepted.contains_key(&p.feature) {
                    let score = gain(p, &selected, analysis, grid, options);
                    if score > options.gain_margin {
                        ranking.push((p, score));
                    }
                }
            }
            ranking.sort_unstable_by(|(a, sa), (b, sb)| {
                sa.total_cmp(sb)
                    .then(b.feature.cmp(&a.feature))
                    .then(b.index.cmp(&a.index))
            });
            changed = false;
        }
        let Some((proposal, _)) = ranking.pop() else {
            break;
        };
        attempts += 1;
        if attempts > options.search_budget {
            diagnostics.search_budget_exhausted = true;
            break;
        }
        if valid_edit(
            proposal,
            &selected,
            &owners,
            &accepted,
            analysis,
            grid,
            cancellation,
        )? {
            for (&p, &q) in &proposal.patch {
                selected[p] = q;
                owners[p] = if proposal.support.contains(&p) {
                    Some(proposal.feature)
                } else {
                    source_owner[q]
                };
            }
            accepted.insert(proposal.feature, proposal.clone());
            changed = true;
        } else {
            diagnostics.collisions += 1;
        }
    }
    // Two bounded local sweeps can replace one conflicting accepted feature
    // together with a previously rejected feature, preserving all other patches.
    for _ in 0..options.local_sweeps {
        let mut improved = false;
        for alternative in &all {
            cancellation.check()?;
            if attempts >= options.search_budget {
                diagnostics.search_budget_exhausted = true;
                break;
            }
            let conflicts: BTreeSet<_> = alternative
                .patch
                .keys()
                .filter_map(|p| owners[*p])
                .filter(|id| *id != alternative.feature)
                .collect();
            if conflicts.len() > 1 {
                continue;
            }
            let replaced = conflicts.first().copied().or_else(|| {
                accepted
                    .contains_key(&alternative.feature)
                    .then_some(alternative.feature)
            });
            let Some(replaced) = replaced else {
                continue;
            };
            if !accepted.contains_key(&replaced) {
                continue;
            }
            for second in all
                .iter()
                .filter(|p| p.feature == replaced || p.feature == alternative.feature)
                .take(options.maximum_proposals)
            {
                if second.feature == alternative.feature && replaced != alternative.feature {
                    continue;
                }
                attempts += 1;
                if attempts > options.search_budget {
                    break;
                }
                let mut trial = baseline.clone();
                let mut trial_owners = base_owners.clone();
                let mut trial_accepted = accepted.clone();
                trial_accepted.remove(&replaced);
                trial_accepted.remove(&alternative.feature);
                for p in trial_accepted.values() {
                    for (&i, &q) in &p.patch {
                        trial[i] = q;
                        trial_owners[i] = if p.support.contains(&i) {
                            Some(p.feature)
                        } else {
                            source_owner[q]
                        };
                    }
                }
                let old_value = accepted
                    .values()
                    .filter(|p| p.feature == replaced || p.feature == alternative.feature)
                    .map(|p| gain(p, &baseline, analysis, grid, options))
                    .sum::<f32>();
                let new_value = gain(alternative, &trial, analysis, grid, options)
                    + if second.feature != alternative.feature {
                        gain(second, &trial, analysis, grid, options)
                    } else {
                        0.0
                    };
                if new_value <= old_value + options.gain_margin {
                    continue;
                }
                let mut valid = true;
                for p in [alternative, second] {
                    if trial_accepted.contains_key(&p.feature) {
                        continue;
                    }
                    if !valid_edit(
                        p,
                        &trial,
                        &trial_owners,
                        &trial_accepted,
                        analysis,
                        grid,
                        cancellation,
                    )? {
                        valid = false;
                        break;
                    }
                    for (&i, &q) in &p.patch {
                        trial[i] = q;
                        trial_owners[i] = if p.support.contains(&i) {
                            Some(p.feature)
                        } else {
                            source_owner[q]
                        };
                    }
                    trial_accepted.insert(p.feature, p.clone());
                }
                if valid {
                    selected = trial;
                    owners = trial_owners;
                    accepted = trial_accepted;
                    improved = true;
                    break;
                }
            }
        }
        if !improved || diagnostics.search_budget_exhausted {
            break;
        }
    }
    for (id, &has_proposal) in has_proposals.iter().enumerate() {
        let represented = if !has_proposal {
            unfitted_coverage(&analysis.features[id], &selected, analysis, grid, options)
        } else {
            all.iter()
                .filter(|p| p.feature == id)
                .map(|p| represented(p, &selected, analysis, grid, options))
                .fold(0.0, f32::max)
        };
        if represented >= 1.0 {
            diagnostics.retained.push(id);
        } else if represented > 0.0 {
            diagnostics.unresolved.push(id);
            diagnostics.connectivity_failures += 1;
        } else {
            diagnostics.dropped.push(id);
        }
    }
    let losses = topology_loss(&selected, analysis, grid, cancellation)?;
    diagnostics.component_losses = losses.components.len();
    diagnostics.hole_losses = losses.holes.len();
    diagnostics.joins = unrelated_joins(&owners, analysis, grid);
    let mut image = RgbaImage::new(grid.width as u32, grid.height as u32);
    let mut provenance = Vec::with_capacity(selected.len());
    for (p, q) in selected.into_iter().enumerate() {
        if p % grid.width == 0 {
            cancellation.check()?;
        }
        let coord = (
            (q % grid.source_width) as u32,
            (q / grid.source_width) as u32,
        );
        let colour_source = analysis.palette.source(q);
        let colour_coord = (
            (colour_source % grid.source_width) as u32,
            (colour_source / grid.source_width) as u32,
        );
        let source_pixel = source.get_pixel(colour_coord.0, colour_coord.1);
        let palette_source = (analysis.occupancy[q]
            && source_pixel.0[..3] != source.get_pixel(coord.0, coord.1).0[..3])
            .then_some(colour_coord);
        let pixel = if analysis.occupancy[q] {
            Rgba([source_pixel[0], source_pixel[1], source_pixel[2], 255])
        } else {
            Rgba([0, 0, 0, 0])
        };
        image.put_pixel((p % grid.width) as u32, (p / grid.width) as u32, pixel);
        provenance.push(Provenance {
            source: coord,
            palette_source,
            reconstructed: q != grid.baseline(p) || palette_source.is_some(),
            feature: owners[p],
        });
    }
    Ok(Scaled {
        gpu_wavelets: analysis.gpu_wavelets,
        image,
        provenance,
        diagnostics,
        options: options.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_prefers_connected_runs_and_wraps_closed_paths() {
        let score = |p: &[bool], closed| continuity_coverage(p.iter().copied(), closed);
        assert!(
            score(&[true, true, true, false, false], false)
                > score(&[true, false, true, false, true], false)
        );
        assert_eq!(score(&[true; 5], false), 1.0);
        assert_eq!(score(&[false; 5], false), 0.0);
        assert_eq!(score(&[], false), 0.0);
        assert_eq!(
            score(&[true, false, true, true], true),
            score(&[false, true, true, true], true)
        );
        assert_eq!(score(&[false, true, true], true), 2.0 / 3.0);
        assert!(
            !geometry::simple_path(&[0, 1, 2, 7, 6], 5, 5),
            "fold touches its start"
        );
        assert!(
            !geometry::simple_path(&[0, 1, 0, 1], 5, 5),
            "repeated interior cell"
        );
    }

    #[test]
    fn route_avoids_unsupported_raster_cells_and_honours_budget() {
        let source = RgbaImage::new(20, 20);
        let options = Options::default();
        let grid = Grid::new(&source, 5, 5, &options).unwrap();
        let path = vec![
            2 * 20 + 4,
            6 * 20 + 6,
            10 * 20 + 7,
            14 * 20 + 10,
            18 * 20 + 14,
        ];
        let analysis = Analysis {
            palette: super::super::palette_grid::Palette {
                labels: vec![0; 400],
                representatives: vec![0],
            },
            gpu_wavelets: false,
            lab: vec![[30.0; 3]; 400],
            occupancy: vec![true; 400],
            foreground: vec![1; 400],
            background: vec![0; 400],
            holes: BTreeSet::new(),
            evidence: vec![Default::default(); 400],
            anchors: BTreeSet::new(),
            features: vec![Feature {
                path: path.clone(),
                kind: Kind::Stroke,
                component: 1,
                half_width: 1.0,
                confidence: 1.0,
                closed: false,
            }],
        };
        let members = path
            .iter()
            .enumerate()
            .map(|(i, p)| (*p, (0.0, i)))
            .collect();
        let corridor = BTreeSet::from([1, 6, 11, 17, 23]);
        let fit = [grid.project(path[0]), grid.project(*path.last().unwrap())];
        let mut candidates = Candidates {
            grid,
            baseline: (0..25).map(|p| grid.baseline(p)).collect(),
            phases: 4,
            cache: BTreeMap::new(),
        };
        let cancellation = CancellationToken::default();
        let raster = geometry::raster(&fit, 5, 5).unwrap();
        assert!(
            ordered_labels(
                &raster,
                &analysis.features[0],
                &members,
                &analysis,
                &mut candidates,
                &options,
                &cancellation
            )
            .unwrap()
            .is_none()
        );
        let mut diagnostics = Diagnostics::default();
        let guide = || RouteGuide {
            members: &members,
            corridor: &corridor,
            fit: &fit,
        };
        let routed = routed_path(
            &analysis.features[0],
            guide(),
            &analysis,
            &mut candidates,
            &options,
            &mut diagnostics,
            &cancellation,
        )
        .unwrap()
        .unwrap();
        assert_eq!(routed, vec![1, 6, 11, 17, 23]);
        assert!(geometry::connected(&routed, 5));
        assert!(
            ordered_labels(
                &routed,
                &analysis.features[0],
                &members,
                &analysis,
                &mut candidates,
                &options,
                &cancellation
            )
            .unwrap()
            .is_some()
        );
        let limited = Options {
            search_budget: 1,
            ..options
        };
        assert!(
            routed_path(
                &analysis.features[0],
                guide(),
                &analysis,
                &mut candidates,
                &limited,
                &mut diagnostics,
                &cancellation
            )
            .unwrap()
            .is_none()
        );
        assert!(diagnostics.search_budget_exhausted);
        cancellation.cancel();
        assert!(
            routed_path(
                &analysis.features[0],
                guide(),
                &analysis,
                &mut candidates,
                &limited,
                &mut diagnostics,
                &cancellation
            )
            .is_err()
        );
    }
}
