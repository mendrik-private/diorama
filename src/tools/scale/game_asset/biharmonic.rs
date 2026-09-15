//! Biharmonic inpainting of retained ink, including its alpha.
//!
//! Solve the masked principal submatrix of L², where L is the four-neighbor
//! graph Laplacian (reflecting image boundaries). Known samples are Dirichlet
//! data. The interior stencil is 20 at the center, -8 at axial neighbors, 2
//! at diagonals and 1 two pixels along either axis. This matches the independent
//! scikit-image reference; no original masked color participates in the solve.
use super::{color::LinearImage, raster::Mask};
use crate::{
    document::CancellationToken,
    error::{AppError, Result},
};

#[cfg(test)]
mod reference;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod simd;

#[derive(Default)]
struct Row {
    // The one-GiB working-set gate limits sources to fewer than 2^21 pixels.
    // On 64-bit hosts this saves 48 bytes per row, offsetting the extra batched
    // solver workspace without reducing the accepted image dimensions.
    columns: [u32; 13],
    coefficients: [i8; 13],
    len: usize,
}

impl Row {
    fn add(&mut self, column: usize, value: i8) {
        let column = u32::try_from(column).expect("bounded Game Asset pixel index");
        if let Some(i) = self.columns[..self.len].iter().position(|&c| c == column) {
            self.coefficients[i] += value;
        } else {
            self.columns[self.len] = column;
            self.coefficients[self.len] = value;
            self.len += 1;
        }
    }

    fn entries(&self) -> impl Iterator<Item = (usize, f64)> + '_ {
        self.columns[..self.len]
            .iter()
            .zip(&self.coefficients[..self.len])
            .map(|(&c, &v)| (c as usize, f64::from(v)))
    }
}

fn laplacian(pixel: usize, w: usize, h: usize) -> Row {
    let mut row = Row::default();
    let (x, y) = (pixel % w, pixel / w);
    for (dx, dy) in [(0, -1), (-1, 0), (1, 0), (0, 1)] {
        let (xx, yy) = (x as isize + dx, y as isize + dy);
        if xx >= 0 && yy >= 0 && xx < w as isize && yy < h as isize {
            row.add(yy as usize * w + xx as usize, -1);
            row.add(pixel, 1);
        }
    }
    row
}

fn stencil(pixel: usize, w: usize, h: usize) -> Row {
    let mut squared = Row::default();
    for (middle, a) in laplacian(pixel, w, h).entries() {
        for (neighbor, b) in laplacian(middle, w, h).entries() {
            squared.add(neighbor, (a * b) as i8);
        }
    }
    squared
}

#[inline(always)]
fn product(
    rows: &[Row],
    x: &[[f64; 4]],
    out: &mut [[f64; 4]],
    cancel: &CancellationToken,
) -> Result<()> {
    for (i, (row, value)) in rows.iter().zip(out).enumerate() {
        if i % 4096 == 0 {
            cancel.check()?;
        }
        let mut sum = [-0.; 4];
        for (column, coefficient) in row.entries() {
            for (s, v) in sum.iter_mut().zip(x[column]) {
                *s += coefficient * v;
            }
        }
        *value = sum;
    }
    Ok(())
}

#[inline(always)]
fn dot(a: &[[f64; 4]], b: &[[f64; 4]]) -> [f64; 4] {
    let mut sum = [-0.; 4];
    for (a, b) in a.iter().zip(b) {
        for c in 0..4 {
            sum[c] += a[c] * b[c];
        }
    }
    sum
}

/// Jacobi-preconditioned conjugate gradients with an independently recomputed
/// stopping residual. A bounded failed solve reports an error, never old ink.
/// Channels share matrix reads and use SIMD-friendly contiguous lanes, but keep
/// their original summation order, step sizes, restarts and convergence tests.
fn solve(
    rows: &[Row],
    rhs: &[[f64; 4]],
    diagonal: &[f64],
    cancel: &CancellationToken,
) -> Result<Vec<[f64; 4]>> {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        simd::solve(rows, rhs, diagonal, cancel)
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        solve_portable(rows, rhs, diagonal, cancel)
    }
}

// Inline the same bounds-checked implementation into the AVX entry point.
// No reassociation or fused multiply/add: each lane keeps the scalar order.
#[inline(always)]
fn solve_portable(
    rows: &[Row],
    rhs: &[[f64; 4]],
    diagonal: &[f64],
    cancel: &CancellationToken,
) -> Result<Vec<[f64; 4]>> {
    let n = rhs.len();
    let mut x = vec![[0.; 4]; n];
    let norms = dot(rhs, rhs).map(f64::sqrt);
    let tolerance = norms.map(|norm| (norm * 1e-12).max(1e-12));
    let mut active: [bool; 4] = std::array::from_fn(|c| norms[c] > tolerance[c]);
    let mut residual = rhs.to_vec();
    if !active.iter().any(|&a| a) {
        return Ok(x);
    }
    let mut direction: Vec<_> = residual
        .iter()
        .zip(diagonal)
        .map(|(r, d)| r.map(|v| v / d))
        .collect();
    let mut product_buffer = vec![[0.; 4]; n];
    let mut rz = dot(&residual, &direction);
    for iteration in 0..(2 * n + 32).min(4096) {
        cancel.check()?;
        product(rows, &direction, &mut product_buffer, cancel)?;
        let denominator = dot(&direction, &product_buffer);
        let mut alpha = [0.; 4];
        for c in 0..4 {
            if active[c] {
                if denominator[c] <= 0. || !denominator[c].is_finite() {
                    return Err(AppError::Scaling(
                        "Biharmonic system lost positive definiteness".into(),
                    ));
                }
                alpha[c] = rz[c] / denominator[c];
            }
        }
        for i in 0..n {
            for c in 0..4 {
                x[i][c] += alpha[c] * direction[i][c];
                residual[i][c] -= alpha[c] * product_buffer[i][c];
            }
        }
        let norms = dot(&residual, &residual).map(f64::sqrt);
        let restart: [bool; 4] = std::array::from_fn(|c| active[c] && norms[c] <= tolerance[c]);
        if restart.iter().any(|&r| r) {
            product(rows, &x, &mut product_buffer, cancel)?;
            for i in 0..n {
                for c in 0..4 {
                    if restart[c] {
                        residual[i][c] = rhs[i][c] - product_buffer[i][c];
                    }
                }
            }
            let norms = dot(&residual, &residual).map(f64::sqrt);
            for c in 0..4 {
                if restart[c] && norms[c] <= tolerance[c] {
                    active[c] = false;
                    tracing::debug!(
                        unknowns = n,
                        channel = c,
                        iterations = iteration + 1,
                        residual = norms[c],
                        "Biharmonic fill converged"
                    );
                }
            }
            if !active.iter().any(|&a| a) {
                return Ok(x);
            }
        }
        for i in 0..n {
            // The matrix product is dead here; reuse its storage for the
            // preconditioned residual instead of retaining another RGBA vector.
            product_buffer[i] = residual[i].map(|r| r / diagonal[i]);
        }
        let next_rz = dot(&residual, &product_buffer);
        let beta: [f64; 4] = std::array::from_fn(|c| {
            if restart[c] || !active[c] {
                0.
            } else {
                next_rz[c] / rz[c]
            }
        });
        for i in 0..n {
            for c in 0..4 {
                direction[i][c] = product_buffer[i][c] + beta[c] * direction[i][c];
            }
        }
        rz = next_rz;
    }
    Err(AppError::Scaling(
        "Biharmonic texture fill did not converge".into(),
    ))
}

pub fn repair(
    source: &LinearImage,
    mask: &Mask,
    cancel: &CancellationToken,
) -> Result<LinearImage> {
    cancel.check()?;
    assert_eq!((source.w, source.h), (mask.w, mask.h));
    let mut indices = vec![usize::MAX; mask.data.len()];
    let mut pixels = Vec::new();
    let mut lower = [f64::INFINITY; 4];
    let mut upper = [f64::NEG_INFINITY; 4];
    for (i, &masked) in mask.data.iter().enumerate() {
        if i % 4096 == 0 {
            cancel.check()?;
        }
        if masked {
            indices[i] = pixels.len();
            pixels.push(i);
        } else {
            let p = premultiplied(source.pixels[i]);
            for c in 0..4 {
                lower[c] = lower[c].min(p[c]);
                upper[c] = upper[c].max(p[c]);
            }
        }
    }
    if pixels.is_empty() {
        return Ok(source.clone());
    }
    if pixels.len() == source.pixels.len() {
        return Err(AppError::Scaling(
            "Biharmonic fill has no known boundary samples".into(),
        ));
    }
    let mut rows = Vec::with_capacity(pixels.len());
    let mut rhs = vec![[0.; 4]; pixels.len()];
    let mut diagonal = vec![0.; pixels.len()];
    for (row_id, &pixel) in pixels.iter().enumerate() {
        if row_id % 1024 == 0 {
            cancel.check()?;
        }
        let mut row = Row::default();
        for (neighbor, coefficient) in stencil(pixel, source.w, source.h).entries() {
            if indices[neighbor] == usize::MAX {
                let p = premultiplied(source.pixels[neighbor]);
                for (c, value) in p.into_iter().enumerate() {
                    rhs[row_id][c] -= coefficient * value;
                }
            } else {
                row.add(indices[neighbor], coefficient as i8);
                if neighbor == pixel {
                    diagonal[row_id] = coefficient;
                }
            }
        }
        rows.push(row);
    }
    let mut repaired = solve(&rows, &rhs, &diagonal, cancel)?;
    for p in &mut repaired {
        for c in 0..4 {
            p[c] = p[c].clamp(lower[c], upper[c]);
        }
    }
    let mut output = source.clone();
    for (i, (&pixel, mut p)) in pixels.iter().zip(repaired).enumerate() {
        if i % 4096 == 0 {
            cancel.check()?;
        }
        p[3] = p[3].clamp(0., 1.);
        for c in 0..3 {
            p[c] = if p[3] > 0. {
                p[c].clamp(0., p[3]) / p[3]
            } else {
                0.
            };
        }
        output.pixels[pixel] = p;
    }
    Ok(output)
}

fn premultiplied(p: [f64; 4]) -> [f64; 4] {
    [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatched_and_portable_solvers_preserve_boundary_cases() {
        for solver in [solve, solve_portable] {
            let cancel = CancellationToken::default();
            assert!(solver(&[], &[], &[], &cancel).unwrap().is_empty());
            let mut row = Row::default();
            row.add(0, 1);
            let rows = [row];
            let zero = solver(&rows, &[[-0., 0., -0., 0.]], &[1.], &cancel).unwrap();
            assert_eq!(zero[0].map(f64::to_bits), [0; 4]);
            cancel.cancel();
            assert!(matches!(
                solver(&rows, &[[1.; 4]], &[1.], &cancel),
                Err(AppError::Cancelled)
            ));
            let mut invalid = Row::default();
            invalid.add(0, -1);
            assert!(matches!(
                solver(&[invalid], &[[1.; 4]], &[1.], &CancellationToken::default()),
                Err(AppError::Scaling(_))
            ));
        }
    }

    #[test]
    fn batched_channels_match_scalar_iterations_exactly() {
        let cancel = CancellationToken::default();
        for (w, h) in [(1, 31), (31, 1), (9, 7), (17, 19)] {
            for pattern in 0..3 {
                let pixels: Vec<_> = (1..w * h)
                    .filter(|&i| match pattern {
                        0 => i % 3 != 0,
                        1 => i % w > w / 4 && i % w < 3 * w / 4,
                        _ => i % 7 < 2,
                    })
                    .collect();
                let mut indices = vec![usize::MAX; w * h];
                for (row, &pixel) in pixels.iter().enumerate() {
                    indices[pixel] = row;
                }
                let mut rows = Vec::new();
                let mut diagonal = Vec::new();
                for &pixel in &pixels {
                    let mut row = Row::default();
                    for (j, value) in stencil(pixel, w, h).entries() {
                        if indices[j] != usize::MAX {
                            row.add(indices[j], value as i8);
                        }
                        if j == pixel {
                            diagonal.push(value);
                        }
                    }
                    rows.push(row);
                }
                // Zero RHS, different signs/magnitudes, and channels converging
                // at different iterations exercise independent stopping/restart.
                let rhs: Vec<_> = pixels
                    .iter()
                    .map(|&i| [0., (i as f64).sin(), 1., 1e-7 * (i as f64).cos()])
                    .collect();
                let actual = solve(&rows, &rhs, &diagonal, &cancel).unwrap();
                let portable = solve_portable(&rows, &rhs, &diagonal, &cancel).unwrap();
                for (a, b) in actual.iter().zip(&portable) {
                    assert_eq!(a.map(f64::to_bits), b.map(f64::to_bits));
                }
                for c in 0..4 {
                    let channel: Vec<_> = rhs.iter().map(|p| p[c]).collect();
                    let expected = reference::solve(&rows, &channel, &diagonal, &cancel).unwrap();
                    for (p, value) in actual.iter().zip(expected) {
                        assert_eq!(
                            p[c].to_bits(),
                            value.to_bits(),
                            "{w}x{h} pattern {pattern}, channel {c}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn corners_edges_and_disconnected_holes_match_scipy_reference() {
        // Independent scikit-image 0.26.0 float64 result, with reflecting image
        // boundaries and its per-channel known-sample clipping. Not generated
        // from this solver. Includes coupled holes, a corner and an edge.
        let source = LinearImage {
            w: 7,
            h: 5,
            pixels: (0..35)
                .map(|i| {
                    let (x, y) = ((i % 7) as f64, (i / 7) as f64);
                    [
                        0.05 + x * 0.03,
                        0.08 + y * 0.04,
                        0.2 + x * 0.01 + y * 0.02,
                        (x + y + 1.) / 12.,
                    ]
                })
                .collect(),
        };
        let mut mask = Mask::new(7, 5);
        let holes = [0, 1, 7, 17, 24, 34];
        for &i in &holes {
            mask.data[i] = true;
        }
        let expected = [
            [0.0125, 0.02, 0.055, 0.25],
            [0.014264705882352947, 0.02, 0.055, 0.25],
            [0.0125, 0.0215686274509804, 0.055, 0.25],
            [
                0.07027777777777777,
                0.08119047619047619,
                0.13597222222222222,
                0.501984126984127,
            ],
            [
                0.08236111111111111,
                0.11964285714285712,
                0.1715972222222222,
                0.5882936507936507,
            ],
            [0.19166666666666665, 0.2, 0.275, 0.8333333333333334],
        ];
        let output = repair(&source, &mask, &CancellationToken::default()).unwrap();
        for (&i, expected) in holes.iter().zip(expected) {
            let actual = premultiplied(output.pixels[i]);
            for c in 0..4 {
                assert!(
                    (actual[c] - expected[c]).abs() < 1e-10,
                    "pixel {i} channel {c}: {} vs {}",
                    actual[c],
                    expected[c]
                );
            }
        }
        for i in 0..35 {
            if !mask.data[i] {
                assert_eq!(output.pixels[i], source.pixels[i]);
            }
        }
        let mut poisoned = source.clone();
        for &i in &holes {
            poisoned.pixels[i] = [f64::NAN; 4];
        }
        assert_eq!(
            repair(&poisoned, &mask, &CancellationToken::default())
                .unwrap()
                .pixels,
            output.pixels,
            "masked source values must never enter the solve"
        );
    }

    #[test]
    fn cubic_texture_is_reconstructed_instead_of_harmonic_smoothing() {
        let source = LinearImage {
            w: 15,
            h: 13,
            pixels: (0..15 * 13)
                .map(|i| {
                    let (x, y) = ((i % 15) as f64, (i / 15) as f64);
                    [(x / 15.).powi(3), (y / 13.).powi(2), 0.25, 0.7]
                })
                .collect(),
        };
        let mut mask = Mask::new(15, 13);
        for y in 4..9 {
            for x in 4..11 {
                mask.data[y * 15 + x] = true;
            }
        }
        let output = repair(&source, &mask, &CancellationToken::default()).unwrap();
        for (a, b) in output.pixels.iter().zip(&source.pixels) {
            for c in 0..4 {
                assert!((a[c] - b[c]).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn transparent_boundaries_and_single_axis_images_are_supported() {
        for (w, h) in [(9, 1), (1, 9), (9, 7)] {
            let source = LinearImage {
                w,
                h,
                pixels: vec![[0.8, 0.3, 0.7, 0.]; w * h],
            };
            let mut mask = Mask::new(w, h);
            mask.data[w * h / 2] = true;
            let result = repair(&source, &mask, &CancellationToken::default()).unwrap();
            assert_eq!(result.pixels[w * h / 2], [0.; 4]);
        }
    }

    #[test]
    fn missing_boundaries_and_cancellation_are_explicit_errors() {
        let source = LinearImage {
            w: 4,
            h: 3,
            pixels: vec![[0.5; 4]; 12],
        };
        let mask = Mask {
            w: 4,
            h: 3,
            data: vec![true; 12],
        };
        assert!(matches!(
            repair(&source, &mask, &CancellationToken::default()),
            Err(AppError::Scaling(_))
        ));
        let cancel = CancellationToken::default();
        cancel.cancel();
        assert!(matches!(
            repair(&source, &Mask::new(4, 3), &cancel),
            Err(AppError::Cancelled)
        ));
    }
}
