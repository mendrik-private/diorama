use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn distance(self, other: Self) -> f32 {
        (self.x - other.x).hypot(self.y - other.y)
    }
    pub fn shifted(self, x: f32, y: f32) -> Self {
        Self {
            x: self.x + x,
            y: self.y + y,
        }
    }
}

pub(super) fn segment_distance(p: Point, a: Point, b: Point) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = dx * dx + dy * dy;
    let t = if len > 1e-8 {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / len).clamp(0.0, 1.0)
    } else {
        0.0
    };
    p.distance(a.shifted(dx * t, dy * t))
}

pub(super) fn side(p: Point, path: &[Point]) -> f32 {
    path.windows(2)
        .min_by(|a, b| segment_distance(p, a[0], a[1]).total_cmp(&segment_distance(p, b[0], b[1])))
        .map_or(0.0, |s| {
            (s[1].x - s[0].x) * (p.y - s[0].y) - (s[1].y - s[0].y) * (p.x - s[0].x)
        })
}

/// A bounded-error piecewise linear fit. Each segment is also a degree-one
/// Bezier curve, so flattening introduces zero additional error. Split at the
/// greatest supported deviation; ties retain the earliest source point. This
/// keeps corners and avoids unsupported curvature or iterative spline drift.
pub(super) fn fit(points: &[Point], tolerance: f32) -> Vec<Point> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut keep = BTreeSet::from([0, points.len() - 1]);
    let mut stack = vec![(0, points.len() - 1)];
    while let Some((first, last)) = stack.pop() {
        let mut farthest = None;
        let mut deviation = tolerance;
        for i in first + 1..last {
            let d = segment_distance(points[i], points[first], points[last]);
            if d > deviation {
                deviation = d;
                farthest = Some(i);
            }
        }
        if let Some(i) = farthest {
            keep.insert(i);
            stack.push((first, i));
            stack.push((i, last));
        }
    }
    keep.into_iter().map(|i| points[i]).collect()
}

/// Symmetric integer Bresenham. Canonical lexicographic endpoint order makes
/// ties independent of traversal direction; both axes step on equality.
pub(super) fn line(a: (i32, i32), b: (i32, i32)) -> Vec<(i32, i32)> {
    let reverse = a > b;
    let (mut p, end) = if reverse { (b, a) } else { (a, b) };
    let dx = (end.0 - p.0).abs();
    let dy = -(end.1 - p.1).abs();
    let sx = if p.0 < end.0 { 1 } else { -1 };
    let sy = if p.1 < end.1 { 1 } else { -1 };
    let mut error = dx + dy;
    let mut result = Vec::new();
    loop {
        result.push(p);
        if p == end {
            break;
        }
        let twice = 2 * error;
        if twice >= dy {
            error += dy;
            p.0 += sx;
        }
        if twice <= dx {
            error += dx;
            p.1 += sy;
        }
    }
    if reverse {
        result.reverse();
    }
    result
}

pub(super) fn raster(points: &[Point], width: usize, height: usize) -> Option<Vec<usize>> {
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for (segment_index, segment) in points.windows(2).enumerate() {
        let pixels = line(
            (segment[0].x.round() as i32, segment[0].y.round() as i32),
            (segment[1].x.round() as i32, segment[1].y.round() as i32),
        );
        for (pixel_index, &(x, y)) in pixels.iter().enumerate() {
            if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
                return None;
            }
            let p = y as usize * width + x as usize;
            if result.last() == Some(&p) {
                continue;
            }
            // Only the final closing vertex may repeat. A fold in the digital
            // curve must not be promoted as a successfully reconstructed path.
            if !seen.insert(p) {
                let closing = result.first() == Some(&p)
                    && segment_index + 2 == points.len()
                    && pixel_index + 1 == pixels.len();
                if !closing {
                    return None;
                }
            }
            result.push(p);
        }
    }
    simple_path(&result, width, height).then_some(result)
}

/// Both fixed rasters and searched routes obey the same no-fold/no-self-touch
/// rule. Only a final closing vertex may repeat.
pub(super) fn simple_path(path: &[usize], width: usize, height: usize) -> bool {
    if width == 0 || !connected(path, width) || path.iter().any(|p| *p / width >= height) {
        return false;
    }
    let closed = path.len() > 1 && path.first() == path.last();
    let count = path.len() - usize::from(closed);
    let positions: BTreeMap<_, _> = path
        .iter()
        .take(count)
        .enumerate()
        .map(|(i, p)| (*p, i))
        .collect();
    if positions.len() != count {
        return false;
    }
    for (i, &p) in path.iter().take(count).enumerate() {
        for q in super::analysis::neighbours(p, width, height, true) {
            if let Some(&j) = positions.get(&q) {
                let separation = i.abs_diff(j);
                if separation > 2 && (!closed || count - separation > 2) {
                    return false;
                }
            }
        }
    }
    true
}

pub(super) fn connected(path: &[usize], width: usize) -> bool {
    !path.is_empty()
        && path.windows(2).all(|p| {
            (p[0] % width).abs_diff(p[1] % width) <= 1 && (p[0] / width).abs_diff(p[1] / width) <= 1
        })
}
