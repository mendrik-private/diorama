use std::f32::consts::TAU;

use crate::document::{BrushPoint, PencilGeometry, Point, Rect, Stroke, StrokePath, StrokeStyle};

const SELECTION_ELLIPSE_SEGMENTS: usize = 64;

#[must_use]
pub fn outline_points(geometry: &PencilGeometry) -> Vec<Point> {
    match geometry {
        PencilGeometry::Freehand(points) => points
            .iter()
            .map(|point| Point {
                x: point.x,
                y: point.y,
            })
            .collect(),
        PencilGeometry::Line(points) => points.clone(),
        PencilGeometry::Rectangle(rect) => rectangle_points(*rect),
        PencilGeometry::Ellipse(rect) => ellipse_points(*rect, SELECTION_ELLIPSE_SEGMENTS),
    }
}

#[must_use]
pub fn geometry_bounds(geometry: &PencilGeometry) -> Rect {
    match geometry {
        PencilGeometry::Freehand(points) => point_bounds(points.iter().map(|point| Point {
            x: point.x,
            y: point.y,
        })),
        PencilGeometry::Line(points) => point_bounds(points.iter().copied()),
        PencilGeometry::Rectangle(rect) | PencilGeometry::Ellipse(rect) => *rect,
    }
}

#[must_use]
pub fn stroke_for(geometry: &PencilGeometry, style: StrokeStyle, anti_aliasing: bool) -> Stroke {
    let (points, path) = match geometry {
        PencilGeometry::Freehand(points) => (points.clone(), StrokePath::Smooth),
        PencilGeometry::Line(points) => (
            points.iter().copied().map(brush).collect(),
            StrokePath::Linear,
        ),
        PencilGeometry::Rectangle(rect) => (
            rectangle_points(*rect).into_iter().map(brush).collect(),
            StrokePath::Linear,
        ),
        PencilGeometry::Ellipse(rect) if (rect.width - rect.height).abs() <= f32::EPSILON => {
            let center = rect.center();
            (
                vec![
                    brush(center),
                    brush(Point {
                        x: center.x + rect.width / 2.0,
                        y: center.y,
                    }),
                ],
                StrokePath::Circle,
            )
        }
        PencilGeometry::Ellipse(rect) => {
            let radius = rect.width.max(rect.height) / 2.0;
            let segments = (TAU * radius).ceil().clamp(16.0, 4_096.0) as usize;
            (
                ellipse_points(*rect, segments)
                    .into_iter()
                    .map(brush)
                    .collect(),
                StrokePath::Linear,
            )
        }
    };
    Stroke {
        points,
        path,
        color: style.color,
        width: style.width,
        anti_aliasing,
        opacity: 1.0,
        hardness: 1.0,
    }
}

fn point_bounds(points: impl IntoIterator<Item = Point>) -> Rect {
    let mut points = points.into_iter();
    let Some(first) = points.next() else {
        return Rect::default();
    };
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (first.x, first.y, first.x, first.y);
    for point in points {
        min_x = min_x.min(point.x);
        min_y = min_y.min(point.y);
        max_x = max_x.max(point.x);
        max_y = max_y.max(point.y);
    }
    Rect {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
    }
}

fn rectangle_points(rect: Rect) -> Vec<Point> {
    vec![
        Point {
            x: rect.x,
            y: rect.y,
        },
        Point {
            x: rect.x + rect.width,
            y: rect.y,
        },
        Point {
            x: rect.x + rect.width,
            y: rect.y + rect.height,
        },
        Point {
            x: rect.x,
            y: rect.y + rect.height,
        },
        Point {
            x: rect.x,
            y: rect.y,
        },
    ]
}

fn ellipse_points(rect: Rect, segments: usize) -> Vec<Point> {
    let center = rect.center();
    let radius_x = rect.width / 2.0;
    let radius_y = rect.height / 2.0;
    let mut points = (0..segments)
        .map(|step| {
            let angle = TAU * step as f32 / segments as f32;
            Point {
                x: center.x + radius_x * angle.cos(),
                y: center.y + radius_y * angle.sin(),
            }
        })
        .collect::<Vec<_>>();
    if let Some(first) = points.first().copied() {
        points.push(first);
    }
    points
}

fn brush(point: Point) -> BrushPoint {
    BrushPoint {
        x: point.x,
        y: point.y,
        pressure: 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangle_and_ellipse_outlines_are_closed() {
        let rect = Rect {
            x: 10.0,
            y: 20.0,
            width: 30.0,
            height: 40.0,
        };
        for geometry in [
            PencilGeometry::Rectangle(rect),
            PencilGeometry::Ellipse(rect),
        ] {
            let points = outline_points(&geometry);
            assert_eq!(points.first(), points.last());
        }
    }

    #[test]
    fn square_ellipse_keeps_the_exact_circle_rasterizer() {
        let stroke = stroke_for(
            &PencilGeometry::Ellipse(Rect {
                x: 10.5,
                y: 20.5,
                width: 16.0,
                height: 16.0,
            }),
            StrokeStyle {
                color: [1, 2, 3, 255],
                width: 1.0,
            },
            false,
        );
        assert_eq!(stroke.path, StrokePath::Circle);
        assert_eq!(stroke.points.len(), 2);
    }
}
