// Zingl rasterizer portions adapted from the user-provided Python port of
// Alois Zingl, Rasterizing Curves (2016), listings 10 and 11.
// Copyright (c) Alois Zingl
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
// of the Software, and to permit persons to whom the Software is furnished to
// do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

#[derive(Clone)]
pub struct Mask {
    pub w: usize,
    pub h: usize,
    pub data: Vec<bool>,
}

impl Mask {
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            data: vec![false; w * h],
        }
    }
    pub fn at(&self, x: isize, y: isize) -> bool {
        x >= 0
            && y >= 0
            && x < self.w as isize
            && y < self.h as isize
            && self.data[y as usize * self.w + x as usize]
    }
    pub fn pixel(&mut self, x: i64, y: i64) {
        if x >= 0 && y >= 0 && x < self.w as i64 && y < self.h as i64 {
            self.data[y as usize * self.w + x as usize] = true;
        }
    }
}

pub struct Zingl {
    pub image: Mask,
}

fn round(x: f64) -> i64 {
    (x + 0.5).floor() as i64
}

/// Visit the inclusive pixels of a Bresenham line, shared by rendering and
/// projected-contour length measurement.
pub fn line_pixels(mut x0: i64, mut y0: i64, x1: i64, y1: i64, mut plot: impl FnMut(i64, i64)) {
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
    let mut err = dx + dy;
    loop {
        plot(x0, y0);
        if x0 == x1 && y0 == y1 {
            break;
        }
        let twice = 2 * err;
        if twice >= dy {
            err += dy;
            x0 += sx;
        }
        if twice <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

impl Zingl {
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            image: Mask::new(w, h),
        }
    }

    pub fn line(&mut self, x0: i64, y0: i64, x1: i64, y1: i64) {
        line_pixels(x0, y0, x1, y1, |x, y| self.image.pixel(x, y));
    }

    fn segment(
        &mut self,
        mut x0: i64,
        mut y0: i64,
        x1: i64,
        y1: i64,
        mut x2: i64,
        mut y2: i64,
    ) -> Result<(), String> {
        let (mut sx, mut sy) = (x2 - x1, y2 - y1);
        let (mut xx, mut yy) = (x0 - x1, y0 - y1);
        let mut cur = (xx * sy - yy * sx) as f64;
        if xx * sx > 0 || yy * sy > 0 {
            return Err("Non-monotone Zingl quadratic segment".into());
        }
        if sx * sx + sy * sy > xx * xx + yy * yy {
            x2 = x0;
            x0 = sx + x1;
            y2 = y0;
            y0 = sy + y1;
            cur = -cur;
        }
        if cur != 0. {
            xx += sx;
            sx = if x0 < x2 { 1 } else { -1 };
            xx *= sx;
            yy += sy;
            sy = if y0 < y2 { 1 } else { -1 };
            yy *= sy;
            let mut xy = 2 * xx * yy;
            xx *= xx;
            yy *= yy;
            if cur * ((sx * sy) as f64) < 0. {
                xx = -xx;
                yy = -yy;
                xy = -xy;
                cur = -cur;
            }
            let mut dx = 4. * sy as f64 * cur * (x1 - x0) as f64 + (xx - xy) as f64;
            let mut dy = 4. * sx as f64 * cur * (y0 - y1) as f64 + (yy - xy) as f64;
            xx += xx;
            yy += yy;
            let mut err = dx + dy + xy as f64;
            let mut iterations = 0;
            loop {
                self.image.pixel(x0, y0);
                if x0 == x2 && y0 == y2 {
                    return Ok(());
                }
                let y_step = 2. * err < dx;
                if 2. * err > dy {
                    x0 += sx;
                    dx -= xy as f64;
                    dy += yy as f64;
                    err += dy;
                }
                if y_step {
                    y0 += sy;
                    dy -= xy as f64;
                    dx += xx as f64;
                    err += dx;
                }
                iterations += 1;
                if iterations > 10000 {
                    return Err("Quadratic rasterizer failed to terminate".into());
                }
                // Deliberately the supplied Python/PDF condition, not the
                // different dy<dx condition in Zingl's HTML example.
                if !(dy < 0. && dx > 0.) {
                    break;
                }
            }
        }
        self.line(x0, y0, x2, y2);
        Ok(())
    }

    pub fn quadratic(&mut self, points: [[f64; 2]; 3]) -> Result<(), String> {
        let [[mut x0, mut y0], [mut x1, mut y1], [mut x2, mut y2]] = points.map(|p| p.map(round));
        let (mut x, mut y) = (x0 - x1, y0 - y1);
        let mut t = (x0 - 2 * x1 + x2) as f64;
        if x * (x2 - x1) > 0 {
            if y * (y2 - y1) > 0
                && (((y0 - 2 * y1 + y2) as f64 / t) * x as f64).abs() > (y as f64).abs()
            {
                x2 = x0;
                x0 = x + x1;
                y2 = y0;
                y0 = y + y1;
            }
            t = (x0 - x1) as f64 / t;
            let mut r = (1. - t) * ((1. - t) * y0 as f64 + 2. * t * y1 as f64) + t * t * y2 as f64;
            t = (x0 * x2 - x1 * x1) as f64 * t / (x0 - x1) as f64;
            x = round(t);
            y = round(r);
            r = (y1 - y0) as f64 * (t - x0 as f64) / (x1 - x0) as f64 + y0 as f64;
            self.segment(x0, y0, x, round(r), x, y)?;
            r = (y1 - y2) as f64 * (t - x2 as f64) / (x1 - x2) as f64 + y2 as f64;
            x0 = x;
            x1 = x;
            y0 = y;
            y1 = round(r);
        }
        if (y0 - y1) * (y2 - y1) > 0 {
            t = (y0 - 2 * y1 + y2) as f64;
            t = (y0 - y1) as f64 / t;
            let mut r = (1. - t) * ((1. - t) * x0 as f64 + 2. * t * x1 as f64) + t * t * x2 as f64;
            t = (y0 * y2 - y1 * y1) as f64 * t / (y0 - y1) as f64;
            x = round(r);
            y = round(t);
            r = (x1 - x0) as f64 * (t - y0 as f64) / (y1 - y0) as f64 + x0 as f64;
            self.segment(x0, y0, round(r), y, x, y)?;
            r = (x1 - x2) as f64 * (t - y2 as f64) / (y1 - y2) as f64 + x2 as f64;
            x0 = x;
            x1 = round(r);
            y0 = y;
            y1 = y;
        }
        self.segment(x0, y0, x1, y1, x2, y2)
    }
}
