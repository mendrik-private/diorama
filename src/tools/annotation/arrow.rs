use crate::document::Point;

use super::geometry::{flatten_quadratic, normalized, quadratic_tangent};

#[must_use]
pub fn curve_points(start: Point, control: Point, end: Point) -> Vec<Point> {
    flatten_quadratic(start, control, end, 64)
}

#[must_use]
pub fn arrow_head(start: Point, control: Point, end: Point, width: f32) -> [Point; 3] {
    let mut tangent = quadratic_tangent(start, control, end, 1.0);
    if tangent.x.hypot(tangent.y) <= f32::EPSILON {
        tangent = Point {
            x: end.x - start.x,
            y: end.y - start.y,
        };
    }
    let direction = normalized(tangent);
    let perpendicular = Point {
        x: -direction.y,
        y: direction.x,
    };
    let length = (5.0 * width).clamp(10.0, 60.0);
    let half_width = length * 25.0_f32.to_radians().tan();
    let base = Point {
        x: end.x - direction.x * length,
        y: end.y - direction.y * length,
    };
    [
        end,
        Point {
            x: base.x + perpendicular.x * half_width,
            y: base.y + perpendicular.y * half_width,
        },
        Point {
            x: base.x - perpendicular.x * half_width,
            y: base.y - perpendicular.y * half_width,
        },
    ]
}

#[must_use]
pub fn chord_relative_control(start: Point, end: Point, control: Point) -> (f32, f32) {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length_squared = dx.mul_add(dx, dy * dy);
    if length_squared <= f32::EPSILON {
        return (0.5, 0.0);
    }
    let cx = control.x - start.x;
    let cy = control.y - start.y;
    (
        cx.mul_add(dx, cy * dy) / length_squared,
        (cx * -dy + cy * dx) / length_squared.sqrt(),
    )
}

#[must_use]
pub fn control_from_chord(start: Point, end: Point, along: f32, perpendicular: f32) -> Point {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length = dx.hypot(dy).max(f32::EPSILON);
    Point {
        x: start.x + dx * along - dy / length * perpendicular,
        y: start.y + dy * along + dx / length * perpendicular,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midpoint_control_produces_a_straight_curve() {
        let points = curve_points(
            Point { x: 0.0, y: 2.0 },
            Point { x: 5.0, y: 2.0 },
            Point { x: 10.0, y: 2.0 },
        );
        assert!(points.iter().all(|point| point.y == 2.0));
    }

    #[test]
    fn chord_coordinates_survive_endpoint_changes() {
        let original = chord_relative_control(
            Point { x: 0.0, y: 0.0 },
            Point { x: 10.0, y: 0.0 },
            Point { x: 4.0, y: 3.0 },
        );
        let control = control_from_chord(
            Point { x: 5.0, y: 5.0 },
            Point { x: 5.0, y: 25.0 },
            original.0,
            original.1,
        );
        let changed =
            chord_relative_control(Point { x: 5.0, y: 5.0 }, Point { x: 5.0, y: 25.0 }, control);
        assert!((original.0 - changed.0).abs() < 1e-5);
        assert!((original.1 - changed.1).abs() < 1e-5);
    }
}
