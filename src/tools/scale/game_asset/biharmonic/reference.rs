// Test-only scalar oracle preserved from 0efe29d5 before channel batching.
use super::*;

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
pub(super) fn solve(
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
