//! Scalar, known-sRGB-only experimental reference. Not routed from the editor.
//! See docs/contour-preserving-lanczos.md; this is not the ordinary RGBA8 scaler.

use crate::document::CancellationToken;
use image::{Rgba, RgbaImage};
use std::{
    cmp::Ordering,
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

mod base;
mod confidence;
mod detector;
#[cfg(test)]
mod evaluation;
mod selection;
#[cfg(test)]
mod tests;

type P = [f64; 4];
type V = [f64; 2];
type Result<T> = std::result::Result<T, Error>;
const COS_22: f64 = 0.9238795325112867;
const COS_45: f64 = std::f64::consts::FRAC_1_SQRT_2;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid source buffer or dimensions; contour Lanczos supports reduction only")]
    Dimensions,
    #[error("Invalid or nonfinite contour Lanczos settings")]
    Settings,
    #[error("Contour Lanczos resource budget exceeded ({0} bytes)")]
    Resource(usize),
    #[error("Nonfinite or singular Lanczos normalization")]
    Numerical,
    #[error("Invalid or materially negative direction-confidence tensor")]
    ConfidenceTensor,
    #[error("Contour evaluation graph exceeds its {0} limit ({1})")]
    EvaluationLimit(&'static str, usize),
    #[error("Contour evaluation assignment encountered an invalid residual graph")]
    EvaluationNumerical,
    #[error("Contour Lanczos cancelled")]
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub protection: bool,
    pub scales: Vec<f64>,
    /// Includes source, working buffers, bounded candidates, diagnostic outputs,
    /// and one cached target. Caller-retained previous outputs are caller-owned.
    pub memory_budget: usize,
    pub candidate_limit: usize,
    /// Amendment 01 control: measure direction, retain v1 scores.
    pub measure_direction: bool,
    /// Amendment 01 opt-in gate. Implies direction measurement; never default.
    pub confidence_gate: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            protection: true,
            scales: vec![0.6, 1.0, 1.6, 2.5, 4.0],
            memory_budget: 1024 * 1024 * 1024,
            candidate_limit: 500_000,
            measure_direction: false,
            confidence_gate: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Ridge,
    Edge,
}

#[derive(Clone, Copy, Debug)]
pub struct Candidate {
    pub id: usize,
    pub kind: Kind,
    pub q: V,
    pub n: V,
    pub sigma: f64,
    pub channel: usize,
    pub polarity: i8,
    pub response: f64,
    pub contrast: f64,
    pub z: f64,
    pub effective_z: f64,
    pub direction: Option<Direction>,
    pub center: P,
    pub minus: P,
    pub plus: P,
    pub colors: [usize; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Direction {
    pub energy: f64,
    pub coherence: f64,
    pub alignment: f64,
    pub confident: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Entry {
    pub candidate: usize,
    pub pixel: usize,
    pub normal: V,
    pub position: V,
    pub score: f64,
    pub distance_squared: f64,
    pub color: usize,
}

#[derive(Default, Debug)]
pub struct Diagnostics {
    pub proposals: usize,
    pub source_retained: usize,
    pub ineligible_splats: usize,
    pub displaced: usize,
    pub associated_flanks: usize,
    pub association_checks: usize,
    pub owner_collisions: usize,
    /// Edge graph links severed by ridge ownership, not an inferred gap count.
    pub collision_broken_links: usize,
    pub slots: Vec<Option<Entry>>,
    pub nms: Vec<Option<Entry>>,
    pub hysteresis: Vec<Option<Entry>>,
    pub components: Vec<usize>,
    pub collisions: Vec<u32>,
    pub owners: Vec<Option<Entry>>,
    pub estimated_peak_bytes: usize,
    pub target_time: Duration,
    pub coefficient_time: Duration,
}

#[derive(Debug)]
pub struct Output {
    pub base: RgbaImage,
    pub image: RgbaImage,
    pub diagnostics: Diagnostics,
}

/// One immutable source revision and detector configuration, plus one target LRU
/// entry (evicted before another target is built). All arithmetic is scalar f64.
/// The caller must actually convert non-sRGB input before using this constructor.
pub struct Reference {
    source: Arc<RgbaImage>,
    settings: Settings,
    p: Vec<P>,
    candidates: Vec<Candidate>,
    active: Vec<usize>,
    pub preparation_time: Duration,
    cached: Option<((u32, u32), Arc<Output>)>,
}

impl Reference {
    pub fn new_srgb(
        source: Arc<RgbaImage>,
        settings: Settings,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        check(cancel)?;
        validate(&source, source.width(), source.height(), &settings)?;
        let start = Instant::now();
        // Source preparation is lazy: identity preserves hidden RGB and does not
        // spend detector work. Disabled protection also never runs the detector.
        Ok(Self {
            source,
            settings,
            p: Vec::new(),
            candidates: Vec::new(),
            active: Vec::new(),
            preparation_time: start.elapsed(),
            cached: None,
        })
    }

    pub fn resize(
        &mut self,
        width: u32,
        height: u32,
        cancel: &CancellationToken,
    ) -> Result<Arc<Output>> {
        check(cancel)?;
        let estimate = validate(&self.source, width, height, &self.settings)?;
        if let Some((size, output)) = &self.cached
            && *size == (width, height)
        {
            check(cancel)?;
            return Ok(output.clone());
        }
        self.cached = None;
        if self.source.dimensions() == (width, height) {
            let output = Arc::new(Output {
                base: (*self.source).clone(),
                image: (*self.source).clone(),
                diagnostics: Diagnostics {
                    estimated_peak_bytes: estimate,
                    ..Default::default()
                },
            });
            check(cancel)?;
            self.cached = Some(((width, height), output.clone()));
            return Ok(output);
        }
        if self.p.is_empty() {
            let start = Instant::now();
            let p = base::decode(&self.source, cancel)?;
            let (candidates, active) = if self.settings.protection {
                detector::detect(
                    &p,
                    self.source.width() as usize,
                    self.source.height() as usize,
                    &self.settings,
                    cancel,
                )?
            } else {
                (Vec::new(), Vec::new())
            };
            check(cancel)?;
            self.p = p;
            self.candidates = candidates;
            self.active = active;
            self.preparation_time = start.elapsed();
        }
        let start = Instant::now();
        let (base, coefficient_time) = base::resize(
            &self.p,
            self.source.width() as usize,
            self.source.height() as usize,
            width as usize,
            height as usize,
            cancel,
        )?;
        let mut diagnostics = Diagnostics {
            coefficient_time,
            proposals: self.candidates.len(),
            source_retained: self.active.len(),
            estimated_peak_bytes: estimate,
            ..Default::default()
        };
        if self.settings.protection {
            selection::reconstruct(self, &base, &mut diagnostics, cancel)?;
        }
        let mut image = base.clone();
        for (i, entry) in diagnostics.owners.iter().enumerate() {
            if i % width as usize == 0 {
                check(cancel)?;
            }
            if let Some(entry) = entry {
                let mut rgba: [u8; 4] = self.source.as_raw()[entry.color * 4..entry.color * 4 + 4]
                    .try_into()
                    .expect("validated source pixel");
                if rgba[3] == 0 {
                    rgba.fill(0);
                }
                image.as_mut()[i * 4..i * 4 + 4].copy_from_slice(&rgba);
            }
        }
        check(cancel)?;
        diagnostics.target_time = start.elapsed();
        let output = Arc::new(Output {
            base,
            image,
            diagnostics,
        });
        self.cached = Some(((width, height), output.clone()));
        Ok(output)
    }

    /// Drop the target cache for a warm-source/cold-target benchmark.
    pub fn clear_target_cache(&mut self) {
        self.cached = None;
    }
}

fn validate(source: &RgbaImage, w: u32, h: u32, settings: &Settings) -> Result<usize> {
    let (sw, sh) = source.dimensions();
    if w == 0 || h == 0 || sw == 0 || sh == 0 || w > sw || h > sh {
        return Err(Error::Dimensions);
    }
    if settings.scales.is_empty()
        || settings.scales.len() > 32
        || settings.candidate_limit == 0
        || settings
            .scales
            .iter()
            .any(|s| !s.is_finite() || *s <= 0.0 || *s > 64.0)
    {
        return Err(Error::Settings);
    }
    let ns = (sw as usize)
        .checked_mul(sh as usize)
        .ok_or(Error::Dimensions)?;
    let nd = (w as usize)
        .checked_mul(h as usize)
        .ok_or(Error::Dimensions)?;
    if ns.checked_mul(4) != Some(source.as_raw().len()) {
        return Err(Error::Dimensions);
    }
    ns.checked_mul(settings.scales.len())
        .and_then(|n| n.checked_mul(8))
        .ok_or(Error::Resource(settings.memory_budget))?;
    // Conservative reservation envelope, including hash bins (source duplicates
    // and association), merge-sort scratch, coefficient tables, graph snapshots,
    // detector buffers, output diagnostics and allocator capacity slack.
    let bytes = if (w, h) == (sw, sh) {
        ns.checked_mul(12)
    } else {
        let features = if settings.protection {
            settings.candidate_limit
        } else {
            0
        };
        let source_bytes =
            if settings.protection && (settings.measure_direction || settings.confidence_gate) {
                352
            } else {
                256
            };
        ns.checked_mul(source_bytes)
            .and_then(|a| nd.checked_mul(4096).and_then(|b| a.checked_add(b)))
            .and_then(|a| {
                features
                    .checked_mul(std::mem::size_of::<Candidate>() + 400)
                    .and_then(|b| a.checked_add(b))
            })
            .and_then(|a| a.checked_add(1024 * 1024))
    }
    .ok_or(Error::Resource(settings.memory_budget))?;
    if bytes > settings.memory_budget {
        return Err(Error::Resource(settings.memory_budget));
    }
    Ok(bytes)
}

fn check(cancel: &CancellationToken) -> Result<()> {
    CANCEL_AFTER_CHECKS.with(|remaining| {
        if let Some(n) = remaining.get() {
            if n == 0 {
                cancel.cancel();
            } else {
                remaining.set(Some(n - 1));
            }
        }
    });
    cancel.check().map_err(|_| Error::Cancelled)
}

// Deterministic, thread-local fault injection in this test-only reference.
// Default None has no effect; it is never shared between fixture workers.
thread_local! {
    static CANCEL_AFTER_CHECKS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}
fn dot(a: V, b: V) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}
fn sub(a: V, b: V) -> V {
    [a[0] - b[0], a[1] - b[1]]
}
fn offset(q: V, n: V, d: f64) -> V {
    [q[0] + n[0] * d, q[1] + n[1] * d]
}
fn norm(v: V) -> Option<V> {
    let l = v[0].hypot(v[1]);
    (l > 1e-12 && l.is_finite()).then(|| [v[0] / l, v[1] / l])
}
fn distance(a: P, b: P) -> f64 {
    a.iter()
        .zip(b)
        .map(|(a, b)| (a - b) * (a - b))
        .sum::<f64>()
        .sqrt()
}
fn tangent(n: V) -> V {
    [-n[1], n[0]]
}
fn inside(q: V, w: usize, h: usize) -> bool {
    q[0] >= 0.0 && q[1] >= 0.0 && q[0] <= (w - 1) as f64 && q[1] <= (h - 1) as f64
}
fn nearest(q: V, w: usize, h: usize) -> usize {
    ((q[1] + 0.5).floor() as usize).min(h - 1) * w + ((q[0] + 0.5).floor() as usize).min(w - 1)
}
fn sample<const N: usize>(data: &[[f64; N]], w: usize, h: usize, q: V) -> [f64; N] {
    let x = q[0].clamp(0.0, (w - 1) as f64);
    let y = q[1].clamp(0.0, (h - 1) as f64);
    let ix = x.floor() as usize;
    let iy = y.floor() as usize;
    let dx = x - ix as f64;
    let dy = y - iy as f64;
    let mut out = [0.0; N];
    for (xx, wx) in [(ix, 1.0 - dx), ((ix + 1).min(w - 1), dx)] {
        for (yy, wy) in [(iy, 1.0 - dy), ((iy + 1).min(h - 1), dy)] {
            for (c, v) in out.iter_mut().enumerate() {
                *v += data[yy * w + xx][c] * wx * wy;
            }
        }
    }
    out
}

/// Bounded runs and cancellable stable merging; no large monolithic sort.
fn sort<T: Copy>(
    items: &mut [T],
    compare: impl Fn(&T, &T) -> Ordering,
    cancel: &CancellationToken,
) -> Result<()> {
    const RUN: usize = 1024;
    for chunk in items.chunks_mut(RUN) {
        check(cancel)?;
        chunk.sort_by(&compare);
    }
    let mut scratch = items.to_vec();
    let mut run = RUN;
    while run < items.len() {
        for start in (0..items.len()).step_by(run * 2) {
            let mid = (start + run).min(items.len());
            let end = (start + 2 * run).min(items.len());
            let (mut a, mut b) = (start, mid);
            for (i, out) in scratch.iter_mut().enumerate().take(end).skip(start) {
                if i % RUN == 0 {
                    check(cancel)?;
                }
                if a < mid && (b == end || compare(&items[a], &items[b]) != Ordering::Greater) {
                    *out = items[a];
                    a += 1;
                } else {
                    *out = items[b];
                    b += 1;
                }
            }
        }
        items.copy_from_slice(&scratch);
        run *= 2;
    }
    check(cancel)
}
