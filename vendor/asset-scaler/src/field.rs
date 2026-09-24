#[cfg(test)]
use crate::CancellationToken;
use crate::{Cancellation, Result};
use std::collections::HashMap;

#[cfg(test)]
mod reference;

#[derive(Clone)]
pub struct Field {
    pub w: usize,
    pub h: usize,
    pub data: Vec<f64>,
}

impl Field {
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            data: vec![0.; w * h],
        }
    }

    pub fn sample(&self, x: f64, y: f64) -> f64 {
        let x = x.clamp(0., (self.w - 1) as f64);
        let y = y.clamp(0., (self.h - 1) as f64);
        let (ix, iy) = (x.floor() as usize, y.floor() as usize);
        let (jx, jy) = ((ix + 1).min(self.w - 1), (iy + 1).min(self.h - 1));
        let (fx, fy) = (x - ix as f64, y - iy as f64);
        (self.data[iy * self.w + ix] * (1. - fx) + self.data[iy * self.w + jx] * fx) * (1. - fy)
            + (self.data[jy * self.w + ix] * (1. - fx) + self.data[jy * self.w + jx] * fx) * fy
    }

    /// SciPy ndimage Gaussian derivative semantics: truncate=4, reflect boundary.
    pub fn gaussian_derivatives(&self, sigma: f64, cancel: &dyn Cancellation) -> Result<[Self; 6]> {
        let kernels: [_; 3] = std::array::from_fn(|order| gaussian_kernel(sigma, order));
        // Six separable filters need only three distinct vertical passes.
        // Drop each intermediate before constructing the next to bound memory.
        let temp = self.convolve_vertical(&kernels[0], cancel)?;
        let l = temp.convolve_horizontal(&kernels[0], cancel)?;
        let gx = temp.convolve_horizontal(&kernels[1], cancel)?;
        let hxx = temp.convolve_horizontal(&kernels[2], cancel)?;
        drop(temp);
        let temp = self.convolve_vertical(&kernels[1], cancel)?;
        let gy = temp.convolve_horizontal(&kernels[0], cancel)?;
        let hxy = temp.convolve_horizontal(&kernels[1], cancel)?;
        drop(temp);
        let temp = self.convolve_vertical(&kernels[2], cancel)?;
        let hyy = temp.convolve_horizontal(&kernels[0], cancel)?;
        Ok([l, gx, gy, hxx, hyy, hxy])
    }

    fn convolve_vertical(&self, kernel: &[f64], cancel: &dyn Cancellation) -> Result<Self> {
        let radius = (kernel.len() / 2) as isize;
        let mut out = Self::new(self.w, self.h);
        // Match the scalar iterator sum, including its signed-zero identity.
        out.data.fill(-0.);
        for y in 0..self.h {
            cancel.check()?;
            let target = &mut out.data[y * self.w..(y + 1) * self.w];
            for (i, &k) in kernel.iter().enumerate() {
                let yy = reflect(y as isize + i as isize - radius, self.h);
                let source = &self.data[yy * self.w..(yy + 1) * self.w];
                // Contiguous pixels provide independent SIMD lanes. Each
                // pixel still accumulates taps in the original scalar order.
                for (value, &sample) in target.iter_mut().zip(source) {
                    *value += k * sample;
                }
            }
        }
        Ok(out)
    }

    fn convolve_horizontal(&self, kernel: &[f64], cancel: &dyn Cancellation) -> Result<Self> {
        let radius = (kernel.len() / 2) as isize;
        let mut out = Self::new(self.w, self.h);
        if self.w == 0 || self.h == 0 {
            return Ok(out);
        }
        out.data.fill(-0.);
        // Reflect one padded row, reusing its mapping and storage for all rows.
        // This also handles images smaller than the kernel without a slow path.
        let indices: Vec<_> = (-radius..self.w as isize + radius)
            .map(|x| reflect(x, self.w))
            .collect();
        let mut padded = vec![0.; indices.len()];
        for y in 0..self.h {
            cancel.check()?;
            let source = &self.data[y * self.w..(y + 1) * self.w];
            for (sample, &x) in padded.iter_mut().zip(&indices) {
                *sample = source[x];
            }
            let target = &mut out.data[y * self.w..(y + 1) * self.w];
            for (i, &k) in kernel.iter().enumerate() {
                for (value, &sample) in target.iter_mut().zip(&padded[i..i + self.w]) {
                    *value += k * sample;
                }
            }
        }
        Ok(out)
    }
}

fn gaussian_kernel(sigma: f64, order: usize) -> Vec<f64> {
    let radius = (4. * sigma + 0.5) as isize;
    let mut g: Vec<_> = (-radius..=radius)
        .map(|i| (-0.5 * (i as f64 / sigma).powi(2)).exp())
        .collect();
    let sum: f64 = g.iter().sum();
    for (i, v) in g.iter_mut().enumerate() {
        let x = i as f64 - radius as f64;
        *v /= sum;
        *v *= match order {
            0 => 1.,
            1 => x / sigma.powi(2),
            2 => x * x / sigma.powi(4) - 1. / sigma.powi(2),
            _ => unreachable!("only derivatives 0–2 are requested"),
        };
    }
    g
}

fn reflect(i: isize, n: usize) -> usize {
    let p = i.rem_euclid(2 * n as isize) as usize;
    if p < n { p } else { 2 * n - 1 - p }
}

/// Exact-radius / exact-nearest queries with deterministic index tie-breaking.
/// Uniform bins replace cKDTree without changing the geometric query.
pub struct Spatial {
    pub points: Vec<[f64; 2]>,
    bins: HashMap<(i32, i32), Vec<usize>>,
    cell: f64,
}

impl Spatial {
    pub fn new(points: Vec<[f64; 2]>, cell: f64) -> Self {
        let mut bins: HashMap<_, Vec<_>> = HashMap::new();
        for (i, p) in points.iter().enumerate() {
            bins.entry(((p[0] / cell).floor() as i32, (p[1] / cell).floor() as i32))
                .or_default()
                .push(i);
        }
        Self { points, bins, cell }
    }

    pub fn radius(&self, p: [f64; 2], r: f64) -> Vec<usize> {
        let mut out = Vec::new();
        for y in ((p[1] - r) / self.cell).floor() as i32..=((p[1] + r) / self.cell).floor() as i32 {
            for x in
                ((p[0] - r) / self.cell).floor() as i32..=((p[0] + r) / self.cell).floor() as i32
            {
                if let Some(ids) = self.bins.get(&(x, y)) {
                    out.extend(
                        ids.iter()
                            .copied()
                            .filter(|&i| distance2(p, self.points[i]) <= r * r),
                    );
                }
            }
        }
        out.sort_unstable();
        out
    }

    pub fn nearest(&self, p: [f64; 2]) -> Option<(f64, usize)> {
        if self.points.is_empty() {
            return None;
        }
        let (bx, by) = (
            (p[0] / self.cell).floor() as i32,
            (p[1] / self.cell).floor() as i32,
        );
        let mut best = (f64::INFINITY, usize::MAX);
        for r in 0i32.. {
            for y in by - r..=by + r {
                for x in bx - r..=bx + r {
                    if r > 0 && x != bx - r && x != bx + r && y != by - r && y != by + r {
                        continue;
                    }
                    if let Some(ids) = self.bins.get(&(x, y)) {
                        for &i in ids {
                            let candidate = (distance2(p, self.points[i]), i);
                            if candidate < best {
                                best = candidate;
                            }
                        }
                    }
                }
            }
            let boundary = (p[0] - (bx - r) as f64 * self.cell)
                .min((bx + r + 1) as f64 * self.cell - p[0])
                .min(p[1] - (by - r) as f64 * self.cell)
                .min((by + r + 1) as f64 * self.cell - p[1]);
            if best.0 < boundary * boundary {
                return Some((best.0.sqrt(), best.1));
            }
        }
        unreachable!()
    }
}

pub fn distance2(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radius_membership_and_order_are_independent_of_bin_width() {
        let mut points: Vec<_> = (0..257)
            .map(|i| [(i * 97 % 311) as f64 - 155., (i * 43 % 277) as f64 - 138.])
            .collect();
        points.extend([[0., 0.], [0., 0.], [1., 0.], [-1., 0.]]);
        for cell in [2., 5., 16., 100.] {
            let tree = Spatial::new(points.clone(), cell);
            for p in [[0., 0.], [-100., 33.5], [155., -138.]] {
                for r in [0., 1., 1.5, 10., 101.] {
                    let expected: Vec<_> = points
                        .iter()
                        .enumerate()
                        .filter(|&(_, &q)| distance2(p, q) <= r * r)
                        .map(|(i, _)| i)
                        .collect();
                    assert_eq!(tree.radius(p, r), expected);
                }
            }
        }
    }

    #[test]
    fn shared_derivatives_match_scalar_taps_and_reflection_exactly() {
        let cancel = CancellationToken::default();
        for (w, h) in [(1, 1), (1, 23), (23, 1), (3, 5), (17, 19), (65, 33)] {
            for data in [
                vec![0.; w * h],
                vec![-0.; w * h],
                vec![1.; w * h],
                (0..w * h)
                    .map(|i| ((i * 65537 % 257) as f64 - 128.) / 129.)
                    .collect(),
            ] {
                let source = Field {
                    w,
                    h,
                    data: data.clone(),
                };
                let scalar = reference::Field { w, h, data };
                for sigma in [0.65, 1., 1.5, 2.2] {
                    let actual = source.gaussian_derivatives(sigma, &cancel).unwrap();
                    for (field, (dy, dx)) in
                        actual
                            .iter()
                            .zip([(0, 0), (0, 1), (1, 0), (0, 2), (2, 0), (1, 1)])
                    {
                        let expected = scalar.gaussian(sigma, dy, dx, &cancel).unwrap();
                        for (i, (a, b)) in field.data.iter().zip(&expected.data).enumerate() {
                            assert_eq!(
                                a.to_bits(),
                                b.to_bits(),
                                "{w}x{h}, sigma={sigma}, ({dy},{dx}), pixel {i}"
                            );
                        }
                    }
                }
            }
        }
        let cancelled = CancellationToken::default();
        cancelled.cancel();
        assert!(
            Field::new(3, 5)
                .gaussian_derivatives(1., &cancelled)
                .is_err()
        );
    }
}
