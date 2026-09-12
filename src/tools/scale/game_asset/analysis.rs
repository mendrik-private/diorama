use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::f32::consts::PI;

use image::RgbaImage;
use palette::{FromColor, Lab, Srgb};
use rustfft::{FftPlanner, num_complex::Complex32};

use super::{Grid, Options};
use crate::document::CancellationToken;
use crate::error::{AppError, Result};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ScaleRange {
    wavelengths: Vec<f32>,
    widths: Vec<f32>,
    options: Options,
}

impl ScaleRange {
    pub fn new(grid: Grid, options: &Options) -> Self {
        let reduction = grid.sx.max(grid.sy);
        let cap = grid.source_width.min(grid.source_height) as f32 / 4.0;
        let mut wavelengths = Vec::new();
        let mut wavelength = options.first_wavelength;
        while wavelength <= cap {
            wavelengths.push(wavelength);
            if wavelength >= 4.0 * reduction {
                break;
            }
            wavelength *= options.wavelength_multiplier;
        }
        let mut widths = Vec::new();
        let mut width = 1.0;
        while width <= (2.0 * reduction).min(grid.source_width.max(grid.source_height) as f32) {
            widths.push(width);
            width *= 2.0;
        }
        Self {
            wavelengths,
            widths,
            options: options.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Evidence {
    pub score: f32,
    pub silhouette: f32,
    pub tangent: f32,
    pub half_width: f32,
    pub edge: f32,
}

impl Evidence {
    fn strength(self) -> f32 {
        self.score.max(self.silhouette)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Stroke,
    Silhouette,
}

#[derive(Debug)]
pub(super) struct Feature {
    pub path: Vec<usize>,
    pub kind: Kind,
    pub component: u32,
    pub half_width: f32,
    pub confidence: f32,
    pub closed: bool,
}

pub(super) struct Analysis {
    pub palette: super::palette_grid::Palette,
    pub gpu_wavelets: bool,
    pub lab: Vec<[f32; 3]>,
    pub occupancy: Vec<bool>,
    pub foreground: Vec<u32>,
    pub background: Vec<u32>,
    pub holes: BTreeSet<u32>,
    pub evidence: Vec<[Evidence; 2]>,
    pub features: Vec<Feature>,
    pub anchors: BTreeSet<usize>,
}

pub(super) fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(a, b)| (a - b).powi(2))
        .sum::<f32>()
        .sqrt()
}

pub(super) fn neighbours(
    p: usize,
    width: usize,
    height: usize,
    diagonal: bool,
) -> impl Iterator<Item = usize> {
    let x = p % width;
    let y = p / width;
    (-1_isize..=1).flat_map(move |dy| {
        (-1_isize..=1).filter_map(move |dx| {
            if (dx == 0 && dy == 0) || (!diagonal && dx != 0 && dy != 0) {
                return None;
            }
            let nx = x.checked_add_signed(dx)?;
            let ny = y.checked_add_signed(dy)?;
            (nx < width && ny < height).then_some(ny * width + nx)
        })
    })
}

pub(super) fn components(
    mask: &[bool],
    width: usize,
    diagonal: bool,
    cancellation: &CancellationToken,
) -> Result<Vec<u32>> {
    let height = mask.len() / width;
    let mut labels = vec![0; mask.len()];
    let mut queue = VecDeque::new();
    let mut label = 0;
    for i in 0..mask.len() {
        if i % width == 0 {
            cancellation.check()?;
        }
        if !mask[i] || labels[i] != 0 {
            continue;
        }
        label += 1;
        labels[i] = label;
        queue.push_back(i);
        while let Some(p) = queue.pop_front() {
            if p % width == 0 {
                cancellation.check()?;
            }
            for q in neighbours(p, width, height, diagonal) {
                if mask[q] && labels[q] == 0 {
                    labels[q] = label;
                    queue.push_back(q);
                }
            }
        }
    }
    Ok(labels)
}

const COLOUR_WORKER_STACK_BYTES: usize = 2 * 1024 * 1024;

fn colour_workers(pixels: usize, rows: usize, available: usize, memory_headroom: u64) -> usize {
    if pixels < 65_536 {
        return 1;
    }
    let desired = (available.saturating_mul(4) / 5).max(1).min(rows.max(1));
    // Keep a serial path when the source fits but worker stacks do not. Raising
    // CPU utilization must not make previously supported images exceed the cap.
    (memory_headroom / COLOUR_WORKER_STACK_BYTES as u64)
        .min(desired as u64)
        .max(1) as usize
}

impl Analysis {
    pub fn new(
        source: &RgbaImage,
        grid: Grid,
        options: &Options,
        cancellation: &CancellationToken,
    ) -> Result<Self> {
        let range = ScaleRange::new(grid, options);
        let sw = grid.source_width;
        let sh = grid.source_height;
        let n = sw * sh;
        // FFTs, evidence, topology, graph and candidate working storage all count.
        let pad = range.wavelengths.last().copied().unwrap_or(0.0).ceil() as usize;
        let fw = (sw + 2 * pad).next_power_of_two();
        let fh = (sh + 2 * pad).next_power_of_two();
        let estimate = n as u64 * 272 + fw as u64 * fh as u64 * 56;
        if estimate > options.memory_limit {
            return Err(AppError::MemoryLimit {
                limit_bytes: options.memory_limit,
            });
        }
        let workers = colour_workers(
            n,
            sh,
            std::thread::available_parallelism().map_or(1, |n| n.get()),
            options.memory_limit - estimate,
        );
        let mut lab = vec![[0.0; 3]; n];
        let mut occupancy = vec![false; n];
        for (i, pixel) in source.pixels().enumerate() {
            if i % sw == 0 {
                cancellation.check()?;
            }
            if pixel[3] >= options.alpha_threshold {
                occupancy[i] = true;
                let c: Lab = Lab::from_color(Srgb::new(
                    pixel[0] as f32 / 255.0,
                    pixel[1] as f32 / 255.0,
                    pixel[2] as f32 / 255.0,
                ));
                lab[i] = [c.l, c.a, c.b];
            }
        }
        extend_colours(&mut lab, &occupancy, sw, cancellation)?;
        let foreground = components(&occupancy, sw, true, cancellation)?;
        let background = components(
            &occupancy.iter().map(|p| !p).collect::<Vec<_>>(),
            sw,
            false,
            cancellation,
        )?;
        let mut holes: BTreeSet<_> = background.iter().copied().filter(|id| *id != 0).collect();
        for (i, &id) in background.iter().enumerate() {
            if i < sw || i >= n - sw || i % sw == 0 || i % sw == sw - 1 {
                holes.remove(&id);
            }
        }
        let mut evidence = vec![[Evidence::default(); 2]; n];
        let mut gpu_wavelets = false;
        if occupancy.iter().any(|p| *p) {
            let enabled = options.wavelet_weight > 0.0 && !range.wavelengths.is_empty();
            let gpu = if enabled && options.gpu_wavelets && n >= 65_536 {
                let stacks = if workers > 1 {
                    workers * COLOUR_WORKER_STACK_BYTES
                } else {
                    0
                };
                let budget = options
                    .memory_limit
                    .saturating_sub(n as u64 * 272 + stacks as u64);
                match super::gpu::GpuWavelets::new(
                    &lab,
                    super::gpu::Dimensions { sw, sh, pad },
                    range.wavelengths.len(),
                    budget,
                    cancellation,
                ) {
                    Ok(gpu) => Some(gpu),
                    Err(error) => {
                        cancellation.check()?;
                        tracing::debug!(%error, "Game Asset GPU wavelets unavailable; using CPU");
                        None
                    }
                }
            } else {
                None
            };
            gpu_wavelets = gpu.is_some();
            let mut bank =
                Wavelets::new(&lab, sw, sh, pad, enabled && gpu.is_none(), cancellation)?;
            for orientation in 0..options.orientations {
                cancellation.check()?;
                let tangent = orientation as f32 * PI / options.orientations as f32;
                let wavelets = if let Some(gpu) = &gpu {
                    match gpu.energies(tangent, &range.wavelengths, options, cancellation) {
                        Ok(energy) => {
                            let mut result = vec![(0.0_f32, 0.0_f32); n];
                            let mut strongest = vec![[0.0_f32; 2]; n];
                            for scale in energy.chunks_exact(n) {
                                cancellation.check()?;
                                accumulate_wavelet_energy(
                                    scale,
                                    options,
                                    &mut result,
                                    &mut strongest,
                                );
                            }
                            for p in 0..n {
                                result[p].0 = 0.75 * strongest[p][0] + 0.25 * strongest[p][1];
                            }
                            result
                        }
                        Err(error) => {
                            cancellation.check()?;
                            tracing::warn!(%error, "Game Asset GPU wavelets failed; restarting analysis on CPU");
                            // Never mix a partial GPU analysis with CPU evidence.
                            return Self::new(
                                source,
                                grid,
                                &Options {
                                    gpu_wavelets: false,
                                    ..options.clone()
                                },
                                cancellation,
                            );
                        }
                    }
                } else {
                    bank.responses(tangent, &range.wavelengths, options, cancellation)?
                };
                ColourPass {
                    lab: &lab,
                    occupancy: &occupancy,
                    dimensions: (sw, sh),
                    tangent,
                    wavelets: &wavelets,
                    widths: &range.widths,
                    options,
                    cancellation,
                }
                .apply(&mut evidence, workers)?;
            }
        }
        // The filter wavelength and trial flank spacing are not stroke width.
        // Measure the actual transverse colour profile at each retained ridge.
        for p in 0..n {
            if p % sw == 0 {
                cancellation.check()?;
            }
            for e in &mut evidence[p] {
                if e.strength() < options.continuation_threshold {
                    continue;
                }
                let mut width = 1.0;
                for sign in [-1.0, 1.0] {
                    for step in 1..=(2.0 * e.half_width).ceil() as usize {
                        let x = (p % sw) as f32 - sign * e.tangent.sin() * step as f32;
                        let y = (p / sw) as f32 + sign * e.tangent.cos() * step as f32;
                        if x < 0.0 || y < 0.0 || x >= (sw - 1) as f32 || y >= (sh - 1) as f32 {
                            break;
                        }
                        let q = y.round() as usize * sw + x.round() as usize;
                        if !occupancy[q] || distance(lab[p], lab[q]) > options.tau_colour {
                            break;
                        }
                        width += 1.0;
                    }
                }
                e.half_width = width / 2.0;
            }
        }
        let palette = super::palette_grid::Palette::new(
            source,
            &lab,
            &occupancy,
            options.palette_delta_e,
            cancellation,
        )?;
        let mut result = Self {
            palette,
            gpu_wavelets,
            lab,
            occupancy,
            foreground,
            background,
            holes,
            evidence,
            features: Vec::new(),
            anchors: BTreeSet::new(),
        };
        // Alpha filaments are continuous geometry even where coloured ridge
        // evidence fluctuates. The scale-bank radius keeps this cacheable.
        result.features = super::filaments::trace(
            &result.occupancy,
            &result.foreground,
            sw,
            (range.widths.last().copied().unwrap_or(2.0) / 2.0).max(1.0) as usize,
            cancellation,
        )?;
        result.trace_strokes(sw, sh, options, cancellation)?;
        result.trace_boundaries(sw, sh, cancellation)?;
        let mut endpoints = BTreeMap::<usize, usize>::new();
        for feature in &result.features {
            for p in [feature.path[0], feature.path[feature.path.len() - 1]] {
                *endpoints.entry(p).or_default() += 1;
            }
        }
        result.anchors = endpoints
            .into_iter()
            .filter_map(|(p, count)| (count > 1).then_some(p))
            .collect();
        Ok(result)
    }

    fn trace_strokes(
        &mut self,
        sw: usize,
        sh: usize,
        options: &Options,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let mut ridge = vec![false; sw * sh];
        for (p, is_ridge) in ridge.iter_mut().enumerate() {
            if p % sw == 0 {
                cancellation.check()?;
            }
            let e = self.evidence[p][0];
            if e.strength() < options.continuation_threshold {
                continue;
            }
            let (nx, ny) = (-e.tangent.sin(), e.tangent.cos());
            let at = |sign: f32| {
                let x = (p % sw) as f32 + sign * nx;
                let y = (p / sw) as f32 + sign * ny;
                self.evidence[y.round().clamp(0.0, (sh - 1) as f32) as usize * sw
                    + x.round().clamp(0.0, (sw - 1) as f32) as usize][0]
                    .strength()
            };
            *is_ridge = e.strength() >= at(-1.0) && e.strength() >= at(1.0);
        }
        let mut adjacency = vec![Vec::<usize>::new(); ridge.len()];
        for p in 0..ridge.len() {
            if p % sw == 0 {
                cancellation.check()?;
            }
            if !ridge[p] {
                continue;
            }
            for q in neighbours(p, sw, sh, true).filter(|q| *q > p && ridge[*q]) {
                let step =
                    ((q / sw) as f32 - (p / sw) as f32).atan2((q % sw) as f32 - (p % sw) as f32);
                let compatible = self.evidence[p].iter().any(|a| {
                    self.evidence[q].iter().any(|b| {
                        a.strength() >= options.continuation_threshold
                            && b.strength() >= options.continuation_threshold
                            && (a.tangent - step).cos().abs() >= 0.38
                            && (b.tangent - step).cos().abs() >= 0.38
                            && (a.tangent - b.tangent).cos().abs() >= 0.38
                            && a.half_width.max(b.half_width)
                                <= 2.0 * a.half_width.min(b.half_width)
                    })
                });
                // A diagonal shortcut around an existing orthogonal neighbour
                // would turn a simple corner into a spurious triangle junction.
                let diagonal = p % sw != q % sw && p / sw != q / sw;
                let shortcut =
                    diagonal && (ridge[(p / sw) * sw + q % sw] || ridge[(q / sw) * sw + p % sw]);
                if compatible
                    && !shortcut
                    && self.foreground[p] == self.foreground[q]
                    && distance(self.lab[p], self.lab[q]) <= options.tau_along * 2.0
                {
                    adjacency[p].push(q);
                    adjacency[q].push(p);
                }
            }
        }
        let mut visited = BTreeSet::new();
        let starts = (0..ridge.len())
            .filter(|p| adjacency[*p].len() != 2)
            .chain((0..ridge.len()).filter(|p| adjacency[*p].len() == 2));
        for p in starts {
            if p % sw == 0 {
                cancellation.check()?;
            }
            for &q in &adjacency[p] {
                if visited.contains(&(p.min(q), p.max(q))) {
                    continue;
                }
                let mut path = vec![p];
                let (mut prev, mut current) = (p, q);
                loop {
                    visited.insert((prev.min(current), prev.max(current)));
                    path.push(current);
                    if adjacency[current].len() != 2 || current == p {
                        break;
                    }
                    let next = adjacency[current]
                        .iter()
                        .copied()
                        .find(|v| *v != prev)
                        .expect("degree two");
                    if visited.contains(&(current.min(next), current.max(next))) {
                        break;
                    }
                    prev = current;
                    current = next;
                }
                if path
                    .iter()
                    .all(|i| self.evidence[*i][0].strength() < options.seed_threshold)
                {
                    continue;
                }
                let confidence = path
                    .iter()
                    .map(|i| self.evidence[*i][0].strength())
                    .sum::<f32>()
                    / path.len() as f32;
                let half_width = path
                    .iter()
                    .map(|i| self.evidence[*i][0].half_width)
                    .sum::<f32>()
                    / path.len() as f32;
                let silhouette = path
                    .iter()
                    .filter(|i| self.evidence[**i][0].silhouette > self.evidence[**i][0].score)
                    .count()
                    * 2
                    > path.len();
                self.features.push(Feature {
                    closed: path.first() == path.last(),
                    component: self.foreground[p],
                    path,
                    kind: if silhouette {
                        Kind::Silhouette
                    } else {
                        Kind::Stroke
                    },
                    half_width,
                    confidence,
                });
            }
        }
        Ok(())
    }

    fn trace_boundaries(
        &mut self,
        sw: usize,
        sh: usize,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        // Clockwise cell edges keep foreground on the right. At a diagonal
        // contact choose the greatest right turn, giving deterministic contours.
        type Vertex = (usize, usize);
        let mut edges: BTreeMap<Vertex, Vec<(Vertex, usize)>> = BTreeMap::new();
        for p in 0..self.occupancy.len() {
            if p % sw == 0 {
                cancellation.check()?;
            }
            if !self.occupancy[p] {
                continue;
            }
            let (x, y) = (p % sw, p / sw);
            for (absent, a, b) in [
                (
                    y == 0 || !self.occupancy[p.saturating_sub(sw)],
                    (x, y),
                    (x + 1, y),
                ),
                (
                    x + 1 == sw || !self.occupancy[(p + 1).min(sw * sh - 1)],
                    (x + 1, y),
                    (x + 1, y + 1),
                ),
                (
                    y + 1 == sh || !self.occupancy[(p + sw).min(sw * sh - 1)],
                    (x + 1, y + 1),
                    (x, y + 1),
                ),
                (
                    x == 0 || !self.occupancy[p.saturating_sub(1)],
                    (x, y + 1),
                    (x, y),
                ),
            ] {
                if absent {
                    edges.entry(a).or_default().push((b, p));
                }
            }
        }
        while let Some((&start, _)) = edges.first_key_value() {
            cancellation.check()?;
            let mut vertex = start;
            let mut path = Vec::new();
            let mut direction = (1_i32, 0_i32);
            loop {
                let Some(outgoing) = edges.get_mut(&vertex) else {
                    break;
                };
                let best = outgoing
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, (to, _))| {
                        let d = (to.0 as i32 - vertex.0 as i32, to.1 as i32 - vertex.1 as i32);
                        let cross = direction.0 * d.1 - direction.1 * d.0;
                        let dot = direction.0 * d.0 + direction.1 * d.1;
                        if cross > 0 {
                            3
                        } else if dot > 0 {
                            2
                        } else if cross < 0 {
                            1
                        } else {
                            0
                        }
                    })
                    .map(|(i, _)| i)
                    .expect("nonempty edges");
                let (next, p) = outgoing.remove(best);
                if outgoing.is_empty() {
                    edges.remove(&vertex);
                }
                if path.last() != Some(&p) {
                    path.push(p);
                }
                direction = (
                    next.0 as i32 - vertex.0 as i32,
                    next.1 as i32 - vertex.1 as i32,
                );
                vertex = next;
                if vertex == start {
                    break;
                }
            }
            if path.len() < 2 {
                continue;
            }
            if path.last() != path.first() {
                path.push(path[0]);
            }
            self.features.push(Feature {
                component: self.foreground[path[0]],
                path,
                kind: Kind::Silhouette,
                half_width: 0.5,
                confidence: 1.0,
                closed: true,
            });
        }
        Ok(())
    }
}

struct ColourPass<'a> {
    lab: &'a [[f32; 3]],
    occupancy: &'a [bool],
    dimensions: (usize, usize),
    tangent: f32,
    wavelets: &'a [(f32, f32)],
    widths: &'a [f32],
    options: &'a Options,
    cancellation: &'a CancellationToken,
}

impl ColourPass<'_> {
    fn apply(&self, evidence: &mut [[Evidence; 2]], workers: usize) -> Result<()> {
        self.cancellation.check()?;
        let (sw, sh) = self.dimensions;
        if workers <= 1 {
            return self.rows(evidence, 0);
        }
        let rows = sh.div_ceil(workers.min(sh));
        // Pixels are independent within one orientation. Scoped workers borrow
        // immutable source maps and disjoint output rows; no floating-point
        // reduction crosses a thread boundary. Join before the next orientation
        // so score ties and the top-two evidence order remain unchanged.
        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for (chunk, output) in evidence.chunks_mut(rows * sw).enumerate() {
                handles.push(
                    std::thread::Builder::new()
                        .name("game-asset-colour".into())
                        .stack_size(COLOUR_WORKER_STACK_BYTES)
                        .spawn_scoped(scope, move || self.rows(output, chunk * rows))?,
                );
            }
            for handle in handles {
                match handle.join() {
                    Ok(result) => result?,
                    Err(panic) => std::panic::resume_unwind(panic),
                }
            }
            Ok(())
        })
    }

    fn rows(&self, evidence: &mut [[Evidence; 2]], first_row: usize) -> Result<()> {
        let (sw, _) = self.dimensions;
        let (tx, ty) = (self.tangent.cos(), self.tangent.sin());
        let mut strips = std::array::from_fn(|_| Vec::new());
        for (row, output) in evidence.chunks_exact_mut(sw).enumerate() {
            self.cancellation.check()?;
            let y = first_row + row;
            for (x, evidence) in output.iter_mut().enumerate() {
                let p = y * sw + x;
                if !self.occupancy[p] {
                    continue;
                }
                for &r in self.widths {
                    let (colour, along, support, silhouette) = prominence(
                        self.lab,
                        self.occupancy,
                        self.dimensions,
                        (x, y),
                        (tx, ty, r),
                        self.options,
                        &mut strips,
                    );
                    let response = Evidence {
                        score: (self.options.colour_weight * colour
                            + self.options.wavelet_weight * self.wavelets[p].0)
                            * (0.5 + 0.5 * along)
                            * support,
                        tangent: self.tangent,
                        half_width: r,
                        edge: self.wavelets[p].1,
                        silhouette,
                    };
                    if response.strength() > evidence[0].strength() {
                        evidence[1] = evidence[0];
                        evidence[0] = response;
                    } else if response.strength() > evidence[1].strength() {
                        evidence[1] = response;
                    }
                }
            }
        }
        Ok(())
    }
}

fn prominence(
    lab: &[[f32; 3]],
    occupancy: &[bool],
    dimensions: (usize, usize),
    position: (usize, usize),
    direction_width: (f32, f32, f32),
    options: &Options,
    strips: &mut [Vec<[f32; 3]>; 3],
) -> (f32, f32, f32, f32) {
    let (sw, sh) = dimensions;
    let (tx, ty, r) = direction_width;
    let (nx, ny) = (-ty, tx);
    let (x, y) = (position.0 as f32, position.1 as f32);
    let [minus, centre, plus] = strips;
    minus.clear();
    centre.clear();
    plus.clear();
    let mut real = [0_usize; 3];
    let mut along = 0.0;
    let count = r.max(1.0) as i32;
    for step in -count..=count {
        for (k, offset) in [-r, 0.0, r].into_iter().enumerate() {
            let ix = (x + step as f32 * tx + offset * nx)
                .round()
                .clamp(0.0, (sw - 1) as f32) as usize;
            let iy = (y + step as f32 * ty + offset * ny)
                .round()
                .clamp(0.0, (sh - 1) as f32) as usize;
            let p = iy * sw + ix;
            real[k] += usize::from(occupancy[p]);
            match k {
                0 => minus.push(lab[p]),
                1 => {
                    centre.push(lab[p]);
                    if occupancy[p] {
                        along += (1.0
                            - distance(lab[position.1 * sw + position.0], lab[p])
                                / options.tau_along)
                            .max(0.0);
                    }
                }
                _ => plus.push(lab[p]),
            }
        }
    }
    let c = median_lab(centre);
    let a = median_lab(minus);
    let b = median_lab(plus);
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let len = ab.iter().map(|v| v * v).sum::<f32>();
    let t = if len > 1e-8 {
        ((0..3).map(|i| (c[i] - a[i]) * ab[i]).sum::<f32>() / len).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut contrast = distance(c, [a[0] + t * ab[0], a[1] + t * ab[1], a[2] + t * ab[2]]);
    // At a silhouette the real inward flank is sufficient for a rim light.
    if real[0] == 0 && real[2] > 0 {
        contrast = distance(c, b);
    }
    if real[2] == 0 && real[0] > 0 {
        contrast = distance(c, a);
    }
    let support = real[1] as f32 / (2 * count + 1) as f32;
    (
        (contrast / options.tau_colour).clamp(0.0, 1.0),
        along / real[1].max(1) as f32,
        support,
        // A foreground filament between two empty flanks is silhouette
        // evidence, independent of Lab colour and bright/dark polarity.
        if real[0] == 0 && real[2] == 0 {
            support
        } else {
            0.0
        },
    )
}

fn median_lab(values: &mut [[f32; 3]]) -> [f32; 3] {
    let mut result = [0.0; 3];
    let middle = values.len() / 2;
    for (c, value) in result.iter_mut().enumerate() {
        // Only the median value is consumed: ordering the whole strip (three
        // times per channel per pixel/width/orientation) does unnecessary work.
        let (_, median, _) = values.select_nth_unstable_by(middle, |a, b| a[c].total_cmp(&b[c]));
        *value = median[c];
    }
    result
}

/// Exact squared Euclidean nearest-foreground transform, with row-major ties.
fn extend_colours(
    lab: &mut [[f32; 3]],
    occupancy: &[bool],
    width: usize,
    cancellation: &CancellationToken,
) -> Result<()> {
    let height = lab.len() / width;
    let mut nearest = vec![usize::MAX; lab.len()];
    for y in 0..height {
        cancellation.check()?;
        let mut last = None;
        for x in 0..width {
            let p = y * width + x;
            if occupancy[p] {
                last = Some(p);
            }
            if let Some(q) = last {
                nearest[p] = q;
            }
        }
        last = None;
        for x in (0..width).rev() {
            let p = y * width + x;
            if occupancy[p] {
                last = Some(p);
            }
            if let Some(q) = last
                && (nearest[p] == usize::MAX || q - p < p - nearest[p])
            {
                nearest[p] = q;
            }
        }
    }
    for x in 0..width {
        cancellation.check()?;
        let mut sites: Vec<usize> = Vec::new();
        let mut breaks: Vec<f64> = Vec::new();
        let cost = |y: usize| ((x as i64 - (nearest[y * width + x] % width) as i64).pow(2)) as f64;
        for y in 0..height {
            if nearest[y * width + x] == usize::MAX {
                continue;
            }
            let mut start = f64::NEG_INFINITY;
            while let Some(&p) = sites.last() {
                start =
                    (cost(y) + (y * y) as f64 - cost(p) - (p * p) as f64) / (2.0 * (y - p) as f64);
                if start > *breaks.last().expect("parallel envelopes") {
                    break;
                }
                sites.pop();
                breaks.pop();
            }
            if sites.is_empty() {
                start = f64::NEG_INFINITY;
            }
            sites.push(y);
            breaks.push(start);
        }
        if sites.is_empty() {
            continue;
        }
        let mut k = 0;
        for y in 0..height {
            while k + 1 < sites.len() && breaks[k + 1] < (y as f64) {
                k += 1;
            }
            if !occupancy[y * width + x] {
                lab[y * width + x] = lab[nearest[sites[k] * width + x]];
            }
        }
    }
    Ok(())
}

struct Wavelets {
    width: usize,
    height: usize,
    sw: usize,
    sh: usize,
    pad: usize,
    spectra: Vec<Vec<Complex32>>,
    planner: FftPlanner<f32>,
}

impl Wavelets {
    fn new(
        lab: &[[f32; 3]],
        sw: usize,
        sh: usize,
        pad: usize,
        enabled: bool,
        cancellation: &CancellationToken,
    ) -> Result<Self> {
        let width = (sw + 2 * pad).next_power_of_two();
        let height = (sh + 2 * pad).next_power_of_two();
        let mut result = Self {
            width,
            height,
            sw,
            sh,
            pad,
            spectra: Vec::new(),
            planner: FftPlanner::new(),
        };
        if !enabled {
            return Ok(result);
        }
        let reflect = |v: isize, n: usize| {
            let period = (2 * n) as isize;
            let q = v.rem_euclid(period) as usize;
            if q < n { q } else { 2 * n - 1 - q }
        };
        for channel in [0, 1, 2] {
            let mut data = vec![Complex32::default(); width * height];
            for y in 0..height {
                cancellation.check()?;
                for x in 0..width {
                    data[y * width + x].re = lab[reflect(y as isize - pad as isize, sh) * sw
                        + reflect(x as isize - pad as isize, sw)][channel];
                }
            }
            result.transform(&mut data, false, cancellation)?;
            result.spectra.push(data);
        }
        Ok(result)
    }

    fn transform(
        &mut self,
        data: &mut [Complex32],
        inverse: bool,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let row = if inverse {
            self.planner.plan_fft_inverse(self.width)
        } else {
            self.planner.plan_fft_forward(self.width)
        };
        let col = if inverse {
            self.planner.plan_fft_inverse(self.height)
        } else {
            self.planner.plan_fft_forward(self.height)
        };
        let mut scratch = vec![
            Complex32::default();
            row.get_inplace_scratch_len()
                .max(col.get_inplace_scratch_len())
        ];
        for values in data.chunks_exact_mut(self.width) {
            cancellation.check()?;
            row.process_with_scratch(values, &mut scratch);
        }
        // Gather a small band of adjacent columns in one row-major pass. A
        // single strided column at a time repeatedly fetches the same cache
        // lines from the entire image. The FFT itself and its operation order
        // are unchanged, and this needs only a small band, not another image.
        const BAND: usize = 16;
        let mut columns = vec![Complex32::default(); self.height * BAND.min(self.width)];
        for x in (0..self.width).step_by(BAND) {
            cancellation.check()?;
            let count = BAND.min(self.width - x);
            for y in 0..self.height {
                for dx in 0..count {
                    columns[dx * self.height + y] = data[y * self.width + x + dx];
                }
            }
            for column in columns.chunks_exact_mut(self.height).take(count) {
                col.process_with_scratch(column, &mut scratch);
            }
            for y in 0..self.height {
                for dx in 0..count {
                    data[y * self.width + x + dx] = columns[dx * self.height + y];
                }
            }
        }
        if inverse {
            let scale = 1.0 / (self.width * self.height) as f32;
            for z in data {
                *z *= scale;
            }
        }
        Ok(())
    }

    fn responses(
        &mut self,
        tangent: f32,
        wavelengths: &[f32],
        options: &Options,
        cancellation: &CancellationToken,
    ) -> Result<Vec<(f32, f32)>> {
        let n = self.sw * self.sh;
        let mut result = vec![(0.0_f32, 0.0_f32); n];
        if self.spectra.is_empty() {
            return Ok(result);
        }
        let mut strongest = vec![[0.0_f32; 2]; n];
        let normal = tangent + PI / 2.0;
        for &wavelength in wavelengths {
            let mut filter = vec![0.0; self.width * self.height];
            for y in 0..self.height {
                cancellation.check()?;
                let fy = if y <= self.height / 2 {
                    y as f32
                } else {
                    y as f32 - self.height as f32
                } / self.height as f32;
                for x in 0..self.width {
                    let fx = if x <= self.width / 2 {
                        x as f32
                    } else {
                        x as f32 - self.width as f32
                    } / self.width as f32;
                    let radius = fx.hypot(fy);
                    if radius == 0.0 {
                        continue;
                    }
                    let angle = (fy.atan2(fx) - normal + PI).rem_euclid(2.0 * PI) - PI;
                    let angular = 0.5
                        * (1.0
                            + (angle.abs() * options.orientations as f32 / 2.0)
                                .min(PI)
                                .cos());
                    let radial = (-((radius * wavelength).ln().powi(2))
                        / (2.0 * options.log_bandwidth.ln().powi(2)))
                    .exp();
                    filter[y * self.width + x] = radial * angular / (1.0 + (radius / 0.4).powi(20));
                }
            }
            let mut energy = vec![[0.0_f32; 2]; n];
            for c in 0..3 {
                let mut response: Vec<_> = self.spectra[c]
                    .iter()
                    .zip(&filter)
                    .map(|(z, f)| z * f)
                    .collect();
                self.transform(&mut response, true, cancellation)?;
                for y in 0..self.sh {
                    cancellation.check()?;
                    for x in 0..self.sw {
                        let z = response[(y + self.pad) * self.width + x + self.pad];
                        energy[y * self.sw + x][0] += options.channel_weights[c] * z.re * z.re;
                        energy[y * self.sw + x][1] += options.channel_weights[c] * z.im * z.im;
                    }
                }
            }
            accumulate_wavelet_energy(&energy, options, &mut result, &mut strongest);
        }
        for p in 0..n {
            result[p].0 = 0.75 * strongest[p][0] + 0.25 * strongest[p][1];
        }
        Ok(result)
    }
}

fn accumulate_wavelet_energy(
    energy: &[[f32; 2]],
    options: &Options,
    result: &mut [(f32, f32)],
    strongest: &mut [[f32; 2]],
) {
    let mut amplitudes: Vec<_> = energy.iter().map(|v| (v[0] + v[1]).sqrt()).collect();
    let mid = amplitudes.len() / 2;
    amplitudes.select_nth_unstable_by(mid, f32::total_cmp);
    let noise = options
        .noise_minimum
        .max(options.noise_median_multiplier * amplitudes[mid]);
    for p in 0..energy.len() {
        let (e, o) = (energy[p][0].sqrt(), energy[p][1].sqrt());
        let amplitude = (e * e + o * o).sqrt() + 1e-6;
        let line = ((e - o - noise) / amplitude).clamp(0.0, 1.0);
        result[p].1 = result[p]
            .1
            .max(((o - e - noise) / amplitude).clamp(0.0, 1.0));
        if line > strongest[p][0] {
            strongest[p][1] = strongest[p][0];
            strongest[p][0] = line;
        } else {
            strongest[p][1] = strongest[p][1].max(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "set DIORAMA_GAME_ASSET_GPU_TEST=1 for hardware validation"]
    fn gpu_wavelet_scores_match_cpu_reference() {
        if std::env::var_os("DIORAMA_GAME_ASSET_GPU_TEST").is_none() {
            return;
        }
        let cancellation = CancellationToken::default();
        let options = Options::default();
        for (sw, sh) in [(31, 23), (64, 64), (129, 65), (1025, 31), (31, 1025)] {
            let lab: Vec<_> = (0..sw * sh)
                .map(|p| {
                    let x = (p % sw) as f32;
                    let y = (p / sw) as f32;
                    [
                        50.0 + 30.0 * (x * 0.3).sin(),
                        20.0 * (y * 0.5).cos(),
                        (p % 17) as f32,
                    ]
                })
                .collect();
            let wavelengths = [3.0, 6.0, 12.0];
            let gpu = super::super::gpu::GpuWavelets::new(
                &lab,
                super::super::gpu::Dimensions { sw, sh, pad: 12 },
                wavelengths.len(),
                options.memory_limit,
                &cancellation,
            )
            .expect("hardware GPU wavelets");
            let mut cpu = Wavelets::new(&lab, sw, sh, 12, true, &cancellation).unwrap();
            for orientation in 0..8 {
                let tangent = orientation as f32 * PI / 8.0;
                let expected = cpu
                    .responses(tangent, &wavelengths, &options, &cancellation)
                    .unwrap();
                let energies = gpu
                    .energies(tangent, &wavelengths, &options, &cancellation)
                    .unwrap();
                let mut actual = vec![(0.0_f32, 0.0_f32); sw * sh];
                let mut strongest = vec![[0.0_f32; 2]; sw * sh];
                for scale in energies.chunks_exact(sw * sh) {
                    accumulate_wavelet_energy(scale, &options, &mut actual, &mut strongest);
                }
                let mut maximum = 0.0_f32;
                for p in 0..sw * sh {
                    actual[p].0 = 0.75 * strongest[p][0] + 0.25 * strongest[p][1];
                    maximum = maximum
                        .max((actual[p].0 - expected[p].0).abs())
                        .max((actual[p].1 - expected[p].1).abs());
                }
                eprintln!("{sw}x{sh}, orientation {orientation}: max score error {maximum}");
                assert!(maximum < 0.001, "GPU/CPU score disagreement: {maximum}");
            }
        }
    }

    #[test]
    fn colour_workers_use_eighty_percent_with_work_and_memory_limits() {
        assert_eq!(colour_workers(800 * 800, 800, 32, u64::MAX), 25);
        assert_eq!(colour_workers(800 * 800, 800, 16, u64::MAX), 12);
        assert_eq!(colour_workers(800 * 800, 800, 8, u64::MAX), 6);
        assert_eq!(colour_workers(800 * 800, 800, 1, u64::MAX), 1);
        assert_eq!(colour_workers(64 * 64, 64, 32, u64::MAX), 1);
        assert_eq!(colour_workers(80_000, 2, 32, u64::MAX), 2);
        assert_eq!(colour_workers(800 * 800, 800, 32, 0), 1);
        assert_eq!(
            colour_workers(800 * 800, 800, 32, 3 * COLOUR_WORKER_STACK_BYTES as u64),
            3
        );
    }

    #[test]
    fn parallel_colour_evidence_matches_serial_and_checks_cancellation() {
        let (width, height) = (65, 37);
        let n = width * height;
        let lab: Vec<_> = (0..n)
            .map(|p| {
                [
                    (p % 101) as f32,
                    (p % 37) as f32 - 18.0,
                    (p % 23) as f32 - 11.0,
                ]
            })
            .collect();
        let occupancy: Vec<_> = (0..n).map(|p| p % 13 != 0).collect();
        let wavelets: Vec<_> = (0..n).map(|p| ((p % 7) as f32 / 7.0, 0.5)).collect();
        let options = Options::default();
        let cancellation = CancellationToken::default();
        let mut serial = vec![[Evidence::default(); 2]; n];
        let mut parallel = serial.clone();
        let mut many_workers = serial.clone();
        for orientation in 0..8 {
            let pass = ColourPass {
                lab: &lab,
                occupancy: &occupancy,
                dimensions: (width, height),
                tangent: orientation as f32 * PI / 8.0,
                wavelets: &wavelets,
                widths: &[1.0, 2.0, 4.0],
                options: &options,
                cancellation: &cancellation,
            };
            pass.apply(&mut serial, 1).unwrap();
            pass.apply(&mut parallel, 4).unwrap();
            pass.apply(&mut many_workers, 25).unwrap();
            assert_eq!(serial, parallel);
            assert_eq!(serial, many_workers);
        }
        cancellation.cancel();
        let pass = ColourPass {
            lab: &lab,
            occupancy: &occupancy,
            dimensions: (width, height),
            tangent: 0.0,
            wavelets: &wavelets,
            widths: &[1.0],
            options: &options,
            cancellation: &cancellation,
        };
        assert!(matches!(
            pass.apply(&mut parallel, 4),
            Err(AppError::Cancelled)
        ));
        assert_eq!(
            serial, parallel,
            "cancelled analysis must not update evidence"
        );
    }

    #[test]
    fn selected_strip_medians_match_full_sort() {
        for len in 1..130 {
            let values: Vec<_> = (0..len)
                .map(|i| {
                    [
                        ((i * 37) % 17) as f32,
                        -(((i * 13) % 11) as f32),
                        i as f32 / 7.0,
                    ]
                })
                .collect();
            let actual = median_lab(&mut values.clone());
            for channel in 0..3 {
                let mut sorted = values.clone();
                sorted.sort_by(|a, b| a[channel].total_cmp(&b[channel]));
                assert_eq!(actual[channel], sorted[len / 2][channel]);
            }
        }
    }

    #[test]
    fn banded_fft_matches_single_column_reference_exactly() {
        let cancellation = CancellationToken::default();
        for width in [1, 8, 16, 32, 64] {
            for height in [1, 8, 32] {
                for inverse in [false, true] {
                    let mut actual: Vec<_> = (0..width * height)
                        .map(|i| Complex32::new((i % 37) as f32 / 7.0, (i % 13) as f32 / 11.0))
                        .collect();
                    let mut expected = actual.clone();
                    let mut bank = Wavelets {
                        width,
                        height,
                        sw: width,
                        sh: height,
                        pad: 0,
                        spectra: Vec::new(),
                        planner: FftPlanner::new(),
                    };
                    bank.transform(&mut actual, inverse, &cancellation).unwrap();
                    let mut planner = FftPlanner::<f32>::new();
                    let direction = if inverse {
                        rustfft::FftDirection::Inverse
                    } else {
                        rustfft::FftDirection::Forward
                    };
                    let row = planner.plan_fft(width, direction);
                    let col = planner.plan_fft(height, direction);
                    for values in expected.chunks_exact_mut(width) {
                        row.process(values);
                    }
                    let mut column = vec![Complex32::default(); height];
                    for x in 0..width {
                        for y in 0..height {
                            column[y] = expected[y * width + x];
                        }
                        col.process(&mut column);
                        for y in 0..height {
                            expected[y * width + x] = column[y];
                        }
                    }
                    if inverse {
                        let scale = 1.0 / (width * height) as f32;
                        for z in &mut expected {
                            *z *= scale;
                        }
                    }
                    assert_eq!(actual, expected, "{width}x{height}, inverse={inverse}");
                }
            }
        }
    }
}
