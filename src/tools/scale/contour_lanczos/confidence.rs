//! Amendment 01, frozen parameter set 2026-09-12. This optional tensor is
//! confidence evidence, never a source of replacement geometry or colors.
use super::*;

pub(super) fn matrix(
    planes: &[Vec<[f64; 1]>],
    w: usize,
    h: usize,
    sigma: f64,
    cancel: &CancellationToken,
) -> Result<Vec<[f64; 3]>> {
    let mut products = vec![[0.0; 4]; w * h];
    for plane in planes {
        for y in 0..h {
            check(cancel)?;
            for x in 0..w {
                let (g, _) = detector::derivatives(plane, w, h, x, y);
                products[y * w + x][0] += g[0] * g[0];
                products[y * w + x][1] += g[0] * g[1];
                products[y * w + x][2] += g[1] * g[1];
            }
        }
    }
    let mut output = vec![[0.0; 3]; w * h];
    for channel in 0..3 {
        let smoothed = detector::gaussian(&products, w, h, channel, sigma.max(1.0), cancel)?;
        for (i, (out, value)) in output.iter_mut().zip(smoothed).enumerate() {
            if i % 1024 == 0 {
                check(cancel)?;
            }
            out[channel] = value[0];
        }
    }
    Ok(output)
}

pub(super) fn direction(j: [f64; 3], normal: V) -> Result<Direction> {
    let [a, b, d] = j;
    if j.iter().any(|v| !v.is_finite()) {
        return Err(Error::ConfidenceTensor);
    }
    let mid = (a + d) * 0.5;
    let radius = ((a - d) * 0.5).hypot(b);
    let (mut mu1, mut mu2) = (mid + radius, mid - radius);
    if mu1 < -1e-12 || mu2 < -1e-12 {
        return Err(Error::ConfidenceTensor);
    }
    mu1 = mu1.max(0.0);
    mu2 = mu2.max(0.0);
    let energy = mu1 + mu2;
    let coherence = if energy > 1e-12 {
        (mu1 - mu2) / energy
    } else {
        0.0
    };
    let principal = if energy <= 1e-12 || mu1 == mu2 {
        None
    } else if b == 0.0 {
        Some(if a > d { [1.0, 0.0] } else { [0.0, 1.0] })
    } else {
        let p = [b, mu1 - a];
        let q = [mu1 - d, b];
        norm(if dot(p, p) >= dot(q, q) { p } else { q })
    };
    let alignment = principal.map_or(0.0, |n| dot(n, normal).abs());
    Ok(Direction {
        energy,
        coherence,
        alignment,
        confident: coherence >= 0.5 && alignment >= 3.0_f64.sqrt() / 2.0,
    })
}

#[test]
fn direction_uncertain_is_not_flat_or_texture() {
    assert!(!direction([0.0; 3], [1.0, 0.0]).unwrap().confident);
    assert!(!direction([1.0, 0.0, 1.0], [1.0, 0.0]).unwrap().confident);
    assert!(direction([1.0, 0.0, 0.0], [1.0, 0.0]).unwrap().confident);
    assert!(!direction([1.0, 0.0, 0.0], [0.0, 1.0]).unwrap().confident);
    assert!(direction([-1e-14, 0.0, 1.0], [0.0, 1.0]).is_ok());
    assert!(direction([-1e-5, 0.0, 1.0], [0.0, 1.0]).is_err());
}

#[test]
fn mixed_derivative_is_not_an_axis_aligned_ridge_detector() {
    let w = 9;
    let h = 9;
    let f: Vec<[f64; 1]> = (0..w * h)
        .map(|i| [((i % w) as f64 - 4.0).powi(2)])
        .collect();
    let (gradient, hessian) = detector::derivatives(&f, w, h, 4, 4);
    assert_eq!(gradient, [0.0, 0.0]);
    assert_eq!(hessian, [2.0, 0.0, 0.0]);
}
