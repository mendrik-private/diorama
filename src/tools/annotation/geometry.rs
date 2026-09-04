use crate::document::Point;

#[must_use]
pub fn quadratic(start: Point, control: Point, end: Point, t: f32) -> Point {
    let remaining = 1.0 - t;
    Point {
        x: remaining * remaining * start.x + 2.0 * remaining * t * control.x + t * t * end.x,
        y: remaining * remaining * start.y + 2.0 * remaining * t * control.y + t * t * end.y,
    }
}

#[must_use]
pub fn quadratic_tangent(start: Point, control: Point, end: Point, t: f32) -> Point {
    Point {
        x: 2.0 * ((1.0 - t) * (control.x - start.x) + t * (end.x - control.x)),
        y: 2.0 * ((1.0 - t) * (control.y - start.y) + t * (end.y - control.y)),
    }
}

#[must_use]
pub fn flatten_quadratic(start: Point, control: Point, end: Point, segments: usize) -> Vec<Point> {
    let segments = segments.max(1);
    (0..=segments)
        .map(|index| quadratic(start, control, end, index as f32 / segments as f32))
        .collect()
}

#[must_use]
pub fn polyline_length(points: &[Point]) -> f32 {
    points
        .windows(2)
        .map(|pair| pair[0].distance(pair[1]))
        .sum()
}

#[must_use]
pub fn distance_to_segment(point: Point, start: Point, end: Point) -> f32 {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let denominator = dx.mul_add(dx, dy * dy);
    if denominator <= f32::EPSILON {
        return point.distance(start);
    }
    let t =
        ((point.x - start.x).mul_add(dx, (point.y - start.y) * dy) / denominator).clamp(0.0, 1.0);
    point.distance(Point {
        x: start.x + t * dx,
        y: start.y + t * dy,
    })
}

#[must_use]
pub fn distance_to_polyline(point: Point, points: &[Point]) -> f32 {
    if let [only] = points {
        return point.distance(*only);
    }
    points
        .windows(2)
        .map(|pair| distance_to_segment(point, pair[0], pair[1]))
        .fold(f32::INFINITY, f32::min)
}

#[must_use]
pub fn point_at_distance(points: &[Point], distance: f32) -> (Point, Point) {
    let Some(&first) = points.first() else {
        return (Point::default(), Point { x: 1.0, y: 0.0 });
    };
    let mut remaining = distance.max(0.0);
    for pair in points.windows(2) {
        let length = pair[0].distance(pair[1]);
        if remaining <= length || length <= f32::EPSILON {
            let t = if length <= f32::EPSILON {
                0.0
            } else {
                remaining / length
            };
            return (
                Point {
                    x: pair[0].x + (pair[1].x - pair[0].x) * t,
                    y: pair[0].y + (pair[1].y - pair[0].y) * t,
                },
                Point {
                    x: pair[1].x - pair[0].x,
                    y: pair[1].y - pair[0].y,
                },
            );
        }
        remaining -= length;
    }
    let last = *points.last().unwrap_or(&first);
    (last, Point { x: 1.0, y: 0.0 })
}

#[must_use]
pub fn normalized(vector: Point) -> Point {
    let length = vector.x.hypot(vector.y);
    if length <= f32::EPSILON {
        Point { x: 1.0, y: 0.0 }
    } else {
        Point {
            x: vector.x / length,
            y: vector.y / length,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_point_polyline_can_be_hit() {
        assert_eq!(
            distance_to_polyline(Point { x: 5.0, y: 7.0 }, &[Point { x: 2.0, y: 3.0 }]),
            5.0
        );
    }
}
