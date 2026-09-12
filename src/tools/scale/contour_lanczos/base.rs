use super::*;

fn linear(v: u8) -> f64 {
    let v = f64::from(v) / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
fn srgb(v: f64) -> f64 {
    if v <= 0.0031308 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}
fn byte(v: f64) -> u8 {
    (255.0 * v + 0.5).floor() as u8
}
pub(super) fn decode(source: &RgbaImage, cancel: &CancellationToken) -> Result<Vec<P>> {
    let mut p = Vec::with_capacity(source.as_raw().len() / 4);
    for row in source.rows() {
        check(cancel)?;
        for (x, pixel) in row.enumerate() {
            if x % 4096 == 0 {
                check(cancel)?;
            }
            let a = f64::from(pixel[3]) / 255.0;
            p.push([
                a * linear(pixel[0]),
                a * linear(pixel[1]),
                a * linear(pixel[2]),
                a,
            ]);
        }
    }
    Ok(p)
}
fn sinc(t: f64) -> f64 {
    if t == 0.0 {
        1.0
    } else {
        let p = std::f64::consts::PI * t;
        p.sin() / p
    }
}
fn lanczos(t: f64) -> f64 {
    if t.abs() < 3.0 {
        sinc(t) * sinc(t / 3.0)
    } else {
        0.0
    }
}

fn coefficients(
    source: usize,
    target: usize,
    cancel: &CancellationToken,
) -> Result<Vec<Vec<(usize, f64)>>> {
    let scale = target as f64 / source as f64;
    let support = (1.0 / scale).max(1.0);
    let mut table = Vec::with_capacity(target);
    for p in 0..target {
        check(cancel)?;
        let u = (p as f64 + 0.5) / scale - 0.5;
        let first = (u - 3.0 * support).ceil() as i64;
        let last = (u + 3.0 * support).floor() as i64;
        let mut taps = Vec::with_capacity((last - first + 1) as usize);
        let mut sum = 0.0;
        for i in first..=last {
            if (i - first) % 1024 == 0 {
                check(cancel)?;
            }
            let weight = lanczos((u - i as f64) / support);
            sum += weight;
            taps.push((i.clamp(0, source as i64 - 1) as usize, weight));
        }
        if !sum.is_finite() || sum.abs() < 1e-12 {
            return Err(Error::Numerical);
        }
        for (_, w) in &mut taps {
            *w /= sum;
        }
        table.push(taps);
    }
    Ok(table)
}

pub(super) fn resize(
    p: &[P],
    sw: usize,
    sh: usize,
    w: usize,
    h: usize,
    cancel: &CancellationToken,
) -> Result<(RgbaImage, Duration)> {
    let start = Instant::now();
    let wx = coefficients(sw, w, cancel)?;
    let wy = coefficients(sh, h, cancel)?;
    let coefficient_time = start.elapsed();
    let mut horizontal = vec![[0.0; 4]; w * sh];
    for y in 0..sh {
        check(cancel)?;
        for (x, taps) in wx.iter().enumerate() {
            if x % 256 == 0 {
                check(cancel)?;
            }
            for (tap, &(i, weight)) in taps.iter().enumerate() {
                if tap % 1024 == 0 {
                    check(cancel)?;
                }
                for c in 0..4 {
                    horizontal[y * w + x][c] += p[y * sw + i][c] * weight;
                }
            }
        }
    }
    let mut output = RgbaImage::new(w as u32, h as u32);
    for (y, taps) in wy.iter().enumerate() {
        check(cancel)?;
        for x in 0..w {
            if x % 256 == 0 {
                check(cancel)?;
            }
            let mut value = [0.0; 4];
            for (tap, &(i, weight)) in taps.iter().enumerate() {
                if tap % 1024 == 0 {
                    check(cancel)?;
                }
                for c in 0..4 {
                    value[c] += horizontal[i * w + x][c] * weight;
                }
            }
            if value.iter().any(|v| !v.is_finite()) {
                return Err(Error::Numerical);
            }
            let a = value[3].clamp(0.0, 1.0);
            let alpha = byte(a);
            let rgba = if alpha == 0 {
                Rgba([0; 4])
            } else {
                Rgba([
                    byte(srgb(value[0].clamp(0.0, a) / a)),
                    byte(srgb(value[1].clamp(0.0, a) / a)),
                    byte(srgb(value[2].clamp(0.0, a) / a)),
                    alpha,
                ])
            };
            output.put_pixel(x as u32, y as u32, rgba);
        }
    }
    check(cancel)?;
    Ok((output, coefficient_time))
}
