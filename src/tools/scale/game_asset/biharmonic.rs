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

#[derive(Default)]
struct Row {
    columns: [usize; 13],
    coefficients: [i8; 13],
    len: usize,
}

impl Row {
    fn add(&mut self, column: usize, value: i8) {
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
            .map(|(&c, &v)| (c, f64::from(v)))
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

fn product(rows: &[Row], x: &[f64], out: &mut [f64], cancel: &CancellationToken) -> Result<()> {
    for (i, (row, value)) in rows.iter().zip(out).enumerate() {
        if i % 4096 == 0 {
            cancel.check()?;
        }
        *value = row
            .entries()
            .map(|(column, coefficient)| coefficient * x[column])
            .sum();
    }
    Ok(())
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}

/// Jacobi-preconditioned conjugate gradients with an independently recomputed
/// stopping residual. A bounded failed solve reports an error, never old ink.
fn solve(
    rows: &[Row],
    rhs: &[f64],
    diagonal: &[f64],
    cancel: &CancellationToken,
) -> Result<Vec<f64>> {
    let n = rhs.len();
    let mut x = vec![0.; n];
    let tolerance = (dot(rhs, rhs).sqrt() * 1e-12).max(1e-12);
    let mut residual = rhs.to_vec();
    if dot(&residual, &residual).sqrt() <= tolerance {
        return Ok(x);
    }
    let mut z: Vec<_> = residual.iter().zip(diagonal).map(|(r, d)| r / d).collect();
    let mut direction = z.clone();
    let mut product_buffer = vec![0.; n];
    let mut rz = dot(&residual, &z);
    for iteration in 0..(2 * n + 32).min(4096) {
        cancel.check()?;
        product(rows, &direction, &mut product_buffer, cancel)?;
        let denominator = dot(&direction, &product_buffer);
        if denominator <= 0. || !denominator.is_finite() {
            return Err(AppError::Scaling(
                "Biharmonic system lost positive definiteness".into(),
            ));
        }
        let alpha = rz / denominator;
        for i in 0..n {
            x[i] += alpha * direction[i];
            residual[i] -= alpha * product_buffer[i];
        }
        let mut restart = false;
        if dot(&residual, &residual).sqrt() <= tolerance {
            product(rows, &x, &mut product_buffer, cancel)?;
            for i in 0..n {
                residual[i] = rhs[i] - product_buffer[i];
            }
            let norm = dot(&residual, &residual).sqrt();
            if norm <= tolerance {
                tracing::debug!(
                    unknowns = n,
                    iterations = iteration + 1,
                    residual = norm,
                    "Biharmonic fill converged"
                );
                return Ok(x);
            }
            restart = true;
        }
        for i in 0..n {
            z[i] = residual[i] / diagonal[i];
        }
        let next_rz = dot(&residual, &z);
        let beta = if restart { 0. } else { next_rz / rz };
        for i in 0..n {
            direction[i] = z[i] + beta * direction[i];
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
    let mut repaired = vec![[0.; 4]; pixels.len()];
    for c in 0..4 {
        let channel: Vec<_> = rhs.iter().map(|p| p[c]).collect();
        let values = solve(&rows, &channel, &diagonal, cancel)?;
        for (p, value) in repaired.iter_mut().zip(values) {
            p[c] = value.clamp(lower[c], upper[c]);
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
