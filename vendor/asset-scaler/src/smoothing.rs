//! Ordered contour paths fitted as bounded, tangent-continuous cubics.
//! The renderer receives a conservative quadratic approximation of each cubic.
use crate::coverage::Quadratic;
use crate::{Cancellation, Result};

/// A compact, source-space description of one fitted contour.  `curves` are
/// deliberately kept separate from detector models: a detector model is a
/// local colour donor, while these curves explain a complete traced stroke.
#[derive(Default)]
pub struct SplineFit {
    pub curves: Vec<Quadratic>,
    pub owners: Vec<usize>,
    pub(crate) cubic_curves: Vec<Cubic>,
    pub(crate) cubic_owners: Vec<usize>,
    /// Original thinned-trace locations.  They are colour donors for fitted
    /// segments that span a gap in the detector's local model coverage.
    pub trace_donors: Vec<([f64; 2], usize)>,
    pub max_error: f64,
}

fn add(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] + b[0], a[1] + b[1]]
}
fn sub(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] - b[0], a[1] - b[1]]
}
fn mul(a: [f64; 2], k: f64) -> [f64; 2] {
    [a[0] * k, a[1] * k]
}
fn dot2(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}
fn norm(a: [f64; 2]) -> f64 {
    dot2(a, a).sqrt()
}
fn distance_to_segment(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let d = sub(b, a);
    let length2 = dot2(d, d);
    let t = if length2 <= 1e-12 {
        0.
    } else {
        (dot2(sub(p, a), d) / length2).clamp(0., 1.)
    };
    norm(sub(p, add(a, mul(d, t))))
}

fn distance_to_polyline(p: [f64; 2], path: &[[f64; 2]]) -> f64 {
    path.windows(2)
        .map(|edge| distance_to_segment(p, edge[0], edge[1]))
        .fold(f64::INFINITY, f64::min)
}

type Cubic = [[f64; 2]; 4];

#[cfg(test)]
fn cubic_point(c: Cubic, t: f64) -> [f64; 2] {
    let mt = 1. - t;
    add(
        add(mul(c[0], mt.powi(3)), mul(c[1], 3. * mt * mt * t)),
        add(mul(c[2], 3. * mt * t * t), mul(c[3], t.powi(3))),
    )
}

fn unit(v: [f64; 2]) -> [f64; 2] {
    let n = norm(v);
    if n <= 1e-12 { [1., 0.] } else { mul(v, 1. / n) }
}

/// Tangent-constrained cubic least squares with chord-length parameters.
/// Independent handle lengths allow every recursive join to share an exact
/// tangent direction without moving the graph's anchors.
fn fit_cubic(path: &[[f64; 2]], start_tangent: [f64; 2], end_tangent: [f64; 2]) -> Cubic {
    let start = path[0];
    let end = *path.last().unwrap();
    let relative_end = sub(end, start);
    let mut lengths = vec![0.; path.len()];
    for i in 1..path.len() {
        lengths[i] = lengths[i - 1] + norm(sub(path[i], path[i - 1]));
    }
    let total = lengths.last().copied().unwrap_or(0.).max(1e-9);
    let mut a00 = 0.;
    let mut a01 = 0.;
    let mut a11 = 0.;
    let mut b0 = 0.;
    let mut b1 = 0.;
    for (&p, &length) in path
        .iter()
        .zip(&lengths)
        .skip(1)
        .take(path.len().saturating_sub(2))
    {
        let t = length / total;
        let b_start = 3. * (1. - t).powi(2) * t;
        let b_end = 3. * (1. - t) * t * t;
        // Solve in a local coordinate frame. It has the same algebra as
        // subtracting (B0+B1)P0+(B2+B3)P3, but avoids cancellation when an
        // illustrator's coordinates are far from the origin.
        let residual = sub(sub(p, start), mul(relative_end, t.powi(3) + b_end));
        a00 += b_start * b_start;
        a01 -= b_start * b_end * dot2(start_tangent, end_tangent);
        a11 += b_end * b_end;
        b0 += b_start * dot2(start_tangent, residual);
        b1 -= b_end * dot2(end_tangent, residual);
    }
    let determinant = a00 * a11 - a01 * a01;
    let fallback = norm(sub(end, start)) / 3.;
    let maximum_handle = total * 2. + 1.;
    let (alpha, beta) = if determinant <= 1e-9 {
        (fallback, fallback)
    } else {
        (
            ((b0 * a11 - b1 * a01) / determinant).clamp(fallback * 0.05, maximum_handle),
            ((a00 * b1 - a01 * b0) / determinant).clamp(fallback * 0.05, maximum_handle),
        )
    };
    [
        start,
        add(start, mul(start_tangent, alpha)),
        sub(end, mul(end_tangent, beta)),
        end,
    ]
}

fn split_cubic(c: Cubic) -> (Cubic, Cubic) {
    let a = mul(add(c[0], c[1]), 0.5);
    let b = mul(add(c[1], c[2]), 0.5);
    let d = mul(add(c[2], c[3]), 0.5);
    let e = mul(add(a, b), 0.5);
    let f = mul(add(b, d), 0.5);
    let mid = mul(add(e, f), 0.5);
    ([c[0], a, e, mid], [mid, f, d, c[3]])
}

fn flatten_cubic(
    c: Cubic,
    output: &mut Vec<[[f64; 2]; 2]>,
    depth: usize,
    cancel: &dyn Cancellation,
) -> Result<()> {
    if depth.is_multiple_of(8) {
        cancel.check()?;
    }
    let chord_length = norm(sub(c[3], c[0]));
    let flatness = [c[1], c[2]]
        .into_iter()
        .map(|p| distance_to_segment(p, c[0], c[3]))
        .fold(0., f64::max);
    if (flatness <= 0.01 && chord_length <= 0.08) || depth >= 24 {
        output.push([c[0], c[3]]);
    } else {
        let (a, b) = split_cubic(c);
        flatten_cubic(a, output, depth + 1, cancel)?;
        flatten_cubic(b, output, depth + 1, cancel)?;
    }
    Ok(())
}

/// Adaptive two-way geometric error.  Flat cubic pieces are below 0.01px and
/// below 0.08px long, so the returned bound covers unsampled parts rather than
/// relying on a fixed number of tessellation samples.
fn cubic_error(c: Cubic, path: &[[f64; 2]], cancel: &dyn Cancellation) -> Result<(f64, usize)> {
    let mut pieces = Vec::new();
    flatten_cubic(c, &mut pieces, 0, cancel)?;
    let mut error = 0.;
    let mut split = path.len() / 2;
    // Source samples are at most 0.2px apart. Distance to a set is
    // 1-Lipschitz; 0.1px plus the 0.01px cubic flatten bound covers every
    // unsampled location along a source edge.
    for (i, edge) in path.windows(2).enumerate() {
        if i.is_multiple_of(64) {
            cancel.check()?;
        }
        let count = (norm(sub(edge[1], edge[0])) / 0.2).ceil().max(1.) as usize;
        for k in 0..=count {
            let p = add(edge[0], mul(sub(edge[1], edge[0]), k as f64 / count as f64));
            let d = pieces
                .iter()
                .map(|&line| distance_to_segment(p, line[0], line[1]))
                .fold(f64::INFINITY, f64::min)
                + 0.11;
            if d > error + 1e-9 {
                split =
                    (i + usize::from(k * 2 >= count)).clamp(1, path.len().saturating_sub(2).max(1));
            }
            error = error.max(d);
        }
    }
    // Flattened curve endpoints are at most 0.08px apart. Their 0.04px
    // covering radius plus the cubic flatten bound bounds all curve points.
    for (i, edge) in pieces.into_iter().enumerate() {
        if i.is_multiple_of(256) {
            cancel.check()?;
        }
        error = error.max(distance_to_polyline(edge[0], path) + 0.05);
        error = error.max(distance_to_polyline(edge[1], path) + 0.05);
    }
    Ok((error, split.clamp(1, path.len().saturating_sub(2).max(1))))
}

/// Arclength over which a trace direction is measured. Thinned rasters turn
/// by 45 or 90 degrees at every staircase step and detector jitter bends them
/// over a few pixels, so only a turn that persists over this reach is drawn.
const CORNER_REACH: f64 = 3.;
/// Cosine below which a persistent turn is a drawn corner (about 63 degrees).
const CORNER_COSINE: f64 = 0.45;

/// The index reached by walking `reach` source pixels from `from`.
fn walk(path: &[[f64; 2]], from: usize, forward: bool, reach: f64) -> usize {
    let mut at = from;
    let mut travelled = 0.;
    while travelled < reach {
        let next = if forward {
            if at + 1 >= path.len() {
                break;
            }
            at + 1
        } else {
            let Some(next) = at.checked_sub(1) else {
                break;
            };
            next
        };
        travelled += norm(sub(path[next], path[at]));
        at = next;
    }
    at
}

fn turn_cosine(path: &[[f64; 2]], i: usize) -> Option<f64> {
    let before = sub(path[i], path[walk(path, i, false, CORNER_REACH)]);
    let after = sub(path[walk(path, i, true, CORNER_REACH)], path[i]);
    let lengths = norm(before) * norm(after);
    (lengths > 1e-8).then(|| dot2(before, after) / lengths)
}

fn corner_indices(path: &[[f64; 2]], closed: bool, locked: &[usize]) -> (Vec<usize>, Vec<usize>) {
    let last = path.len().saturating_sub(1);
    if last < 4 {
        let mut anchors = vec![0, last];
        anchors.extend(locked.iter().copied().filter(|&i| i > 0 && i < last));
        anchors.sort_unstable();
        anchors.dedup();
        return (anchors, if closed { Vec::new() } else { vec![0, last] });
    }
    let mut anchors = vec![0];
    let mut hard = Vec::new();
    // A corner is the sharpest turn within its own measuring reach, so one
    // drawn bend yields one anchor however many staircase steps it spans.
    let cosines: Vec<_> = (0..=last)
        .map(|i| {
            (i >= 2 && i + 2 <= last)
                .then(|| turn_cosine(path, i))
                .flatten()
        })
        .collect();
    for i in 2..last.saturating_sub(1) {
        let Some(cosine) = cosines[i] else {
            continue;
        };
        if cosine >= CORNER_COSINE {
            continue;
        }
        let (from, to) = (
            walk(path, i, false, CORNER_REACH),
            walk(path, i, true, CORNER_REACH),
        );
        let sharpest = (from..=to)
            .all(|j| cosines[j].is_none_or(|other| other > cosine || (other == cosine && j >= i)));
        if sharpest && anchors.last().is_none_or(|&old| i > old + 2) {
            anchors.push(i);
            hard.push(i);
        }
    }
    if closed && anchors.len() == 1 {
        // A smooth loop has no natural endpoint. Four deterministic anchors
        // prevent the degenerate p0 == p2 fit and retain loop closure.
        for i in [last / 4, last / 2, last * 3 / 4] {
            if i > 0 && i < last {
                anchors.push(i);
            }
        }
    }
    if closed {
        let before = sub(path[0], path[last - 2]);
        let after = sub(path[2], path[0]);
        let lengths = norm(before) * norm(after);
        if lengths > 1e-8 && dot2(before, after) / lengths < CORNER_COSINE {
            hard.extend([0, last]);
        }
    }
    anchors.extend(locked.iter().copied().filter(|&i| i > 0 && i < last));
    anchors.push(last);
    if !closed {
        hard.extend([0, last]);
    }
    anchors.sort_unstable();
    anchors.dedup();
    (anchors, hard)
}

/// Binomial passes applied between fixed anchors before fitting; four passes
/// approximate a Gaussian with a 1.4px standard deviation along the trace.
const SMOOTHING_PASSES: usize = 4;

/// Remove raster stairs and detector jitter from a span with fixed ends.
/// Returns the smoothed span and the largest vertex displacement, which the
/// caller adds to the fit error so the reported bound still refers to the
/// original trace.
fn smooth_span(span: &[[f64; 2]]) -> (Vec<[f64; 2]>, f64) {
    let mut smoothed = span.to_vec();
    if span.len() < 3 {
        return (smoothed, 0.);
    }
    let mut next = smoothed.clone();
    for _ in 0..SMOOTHING_PASSES {
        for i in 1..span.len() - 1 {
            next[i] = mul(
                add(add(smoothed[i - 1], smoothed[i + 1]), mul(smoothed[i], 2.)),
                0.25,
            );
        }
        std::mem::swap(&mut smoothed, &mut next);
    }
    let displacement = span
        .iter()
        .zip(&smoothed)
        .map(|(&a, &b)| norm(sub(a, b)))
        .fold(0., f64::max);
    (smoothed, displacement)
}

/// The shared tangent of a smooth anchor: both adjacent spans use this same
/// direction, measured symmetrically over the corner reach, so they join G1.
fn anchor_tangent(path: &[[f64; 2]], index: usize, closed: bool) -> [f64; 2] {
    let last = path.len() - 1;
    if closed && (index == 0 || index == last) && last >= 4 {
        return unit(sub(
            path[walk(path, 0, true, CORNER_REACH)],
            path[walk(path, last, false, CORNER_REACH)],
        ));
    }
    unit(sub(
        path[walk(path, index, true, CORNER_REACH)],
        path[walk(path, index, false, CORNER_REACH)],
    ))
}

/// The one-sided tangent at a hard anchor (a corner or a free line end).
/// A raster line end or corner leg often starts with a flat run several
/// pixels long, so it looks up to twice the corner reach into its own span,
/// but never past the span's middle.
fn hard_tangent(span: &[[f64; 2]], start: bool) -> [f64; 2] {
    let length: f64 = span.windows(2).map(|e| norm(sub(e[1], e[0]))).sum();
    let reach = (length * 0.5).min(2. * CORNER_REACH);
    if start {
        unit(sub(span[walk(span, 0, true, reach)], span[0]))
    } else {
        let last = span.len() - 1;
        unit(sub(span[last], span[walk(span, last, false, reach)]))
    }
}

fn fit_span(
    path: &[[f64; 2]],
    tolerance: f64,
    cancel: &dyn Cancellation,
    start_tangent: [f64; 2],
    end_tangent: [f64; 2],
    output: &mut Vec<Cubic>,
    max_error: &mut f64,
) -> Result<()> {
    cancel.check()?;
    if path.len() < 2 {
        return Ok(());
    }
    let mut cubic = fit_cubic(path, start_tangent, end_tangent);
    let (mut error, split) = cubic_error(cubic, path, cancel)?;
    if path.len() <= 2 && error > tolerance {
        // A leaf has no interior vertex at which to split. Retain inherited
        // tangent directions while contracting the handles until its bounded
        // deviation is acceptable.
        let (start, end, first, second) = (cubic[0], cubic[3], cubic[1], cubic[2]);
        for step in 1..=24 {
            cancel.check()?;
            let factor = 0.5f64.powi(step);
            cubic[1] = add(start, mul(sub(first, start), factor));
            cubic[2] = add(end, mul(sub(second, end), factor));
            error = cubic_error(cubic, path, cancel)?.0;
            if error <= tolerance {
                break;
            }
        }
    }
    if error <= tolerance {
        *max_error = (*max_error).max(error);
        output.push(cubic);
        return Ok(());
    }
    // A neighbour chord on a raster staircase is off by up to 45 degrees and
    // would force a kink into both halves; measure over the corner reach.
    let tangent = unit(sub(
        path[walk(path, split, true, CORNER_REACH)],
        path[walk(path, split, false, CORNER_REACH)],
    ));
    fit_span(
        &path[..=split],
        tolerance,
        cancel,
        start_tangent,
        tangent,
        output,
        max_error,
    )?;
    fit_span(
        &path[split..],
        tolerance,
        cancel,
        tangent,
        end_tangent,
        output,
        max_error,
    )
}

fn reduce_cubic(cubic: Cubic, tolerance: f64, output: &mut Vec<Quadratic>) {
    let control = mul(
        sub(
            add(mul(add(cubic[1], cubic[2]), 3.), [0., 0.]),
            add(cubic[0], cubic[3]),
        ),
        0.25,
    );
    let quadratic = [cubic[0], control, cubic[3]];
    let elevated = [
        quadratic[0],
        mul(add(quadratic[0], mul(quadratic[1], 2.)), 1. / 3.),
        mul(add(mul(quadratic[1], 2.), quadratic[2]), 1. / 3.),
        quadratic[2],
    ];
    let error = cubic
        .into_iter()
        .zip(elevated)
        .map(|(actual, reduced)| norm(sub(actual, reduced)))
        .fold(0., f64::max);
    if error <= tolerance {
        output.push(quadratic);
    } else {
        let (left, right) = split_cubic(cubic);
        reduce_cubic(left, tolerance, output);
        reduce_cubic(right, tolerance, output);
    }
}

/// Fit a set of ordered source paths.  The tolerance is expressed in source
/// pixels so a reduction grants the fit a fraction of one output pixel while
/// near-identity rendering stays close to detector geometry.
pub fn fit_paths(
    paths: impl IntoIterator<Item = (usize, Vec<[f64; 2]>, Vec<usize>)>,
    scale: f64,
    cancel: &dyn Cancellation,
) -> Result<SplineFit> {
    let mut fitted = SplineFit::default();
    // The allowance grows in source pixels for small targets, then caps at
    // 2.25px. Passing the largest target axis keeps its displacement bounded.
    let tolerance = (0.65 / scale.max(0.15)).clamp(1.0, 2.25);
    for (owner, path, locked) in paths {
        cancel.check()?;
        if path.len() < 2 {
            continue;
        }
        let closed = path.first() == path.last();
        let (anchors, hard) = corner_indices(&path, closed, &locked);
        for edge in anchors.windows(2) {
            let (span, displacement) = smooth_span(&path[edge[0]..=edge[1]]);
            let mut pieces = Vec::new();
            let mut span_error = 0.;
            // Smoothing may use at most half of the allowance. A span it would
            // move further keeps its original vertices.
            let (span, displacement) = if displacement <= tolerance * 0.5 {
                (span, displacement)
            } else {
                (path[edge[0]..=edge[1]].to_vec(), 0.)
            };
            fit_span(
                &span,
                tolerance - displacement,
                cancel,
                if hard.contains(&edge[0]) {
                    hard_tangent(&span, true)
                } else {
                    anchor_tangent(&path, edge[0], closed)
                },
                if hard.contains(&edge[1]) {
                    hard_tangent(&span, false)
                } else {
                    anchor_tangent(&path, edge[1], closed)
                },
                &mut pieces,
                &mut span_error,
            )?;
            fitted.max_error = fitted.max_error.max(span_error + displacement);
            let before = fitted.curves.len();
            for cubic in pieces {
                fitted.cubic_curves.push(cubic);
                fitted.cubic_owners.push(owner);
                // The renderer accepts quadratics. Adaptive degree reduction
                // keeps this implementation detail below 0.02 target pixels.
                reduce_cubic(cubic, 0.02 / scale.max(0.15), &mut fitted.curves);
            }
            fitted
                .owners
                .extend(std::iter::repeat_n(owner, fitted.curves.len() - before));
        }
    }
    Ok(fitted)
}

#[cfg(test)]
mod tests;
