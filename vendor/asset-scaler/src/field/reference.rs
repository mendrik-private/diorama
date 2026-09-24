// Test-only scalar oracle preserved from 0efe29d5 before pass sharing.
use crate::{Cancellation, Result};

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

    /// SciPy ndimage Gaussian derivative semantics: truncate=4, reflect boundary.
    pub fn gaussian(
        &self,
        sigma: f64,
        dy: u32,
        dx: u32,
        cancel: &dyn Cancellation,
    ) -> Result<Self> {
        let radius = (4. * sigma + 0.5) as isize;
        let kernel = |order| {
            let mut g: Vec<_> = (-radius..=radius)
                .map(|i| (-0.5 * (i as f64 / sigma).powi(2)).exp())
                .collect();
            let sum: f64 = g.iter().sum();
            for (i, v) in g.iter_mut().enumerate() {
                let x = i as f64 - radius as f64;
                *v /= sum;
                *v *= match order {
                    0 => 1.,
                    1 => x / sigma.powi(2), // correlation flips odd derivative
                    2 => x * x / sigma.powi(4) - 1. / sigma.powi(2),
                    _ => unreachable!("only derivatives 0–2 are requested"),
                };
            }
            g
        };
        let ky = kernel(dy);
        let kx = kernel(dx);
        let mut temp = Self::new(self.w, self.h);
        let mut out = Self::new(self.w, self.h);
        for y in 0..self.h {
            cancel.check()?;
            for x in 0..self.w {
                temp.data[y * self.w + x] = ky
                    .iter()
                    .enumerate()
                    .map(|(i, k)| {
                        k * self.data
                            [reflect(y as isize + i as isize - radius, self.h) * self.w + x]
                    })
                    .sum();
            }
        }
        for y in 0..self.h {
            cancel.check()?;
            for x in 0..self.w {
                out.data[y * self.w + x] = kx
                    .iter()
                    .enumerate()
                    .map(|(i, k)| {
                        k * temp.data
                            [y * self.w + reflect(x as isize + i as isize - radius, self.w)]
                    })
                    .sum();
            }
        }
        Ok(out)
    }
}

fn reflect(i: isize, n: usize) -> usize {
    let p = i.rem_euclid(2 * n as isize) as usize;
    if p < n { p } else { 2 * n - 1 - p }
}
