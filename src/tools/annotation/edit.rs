use crate::document::{Annotation, Axis, PencilGeometry, Point, Rect, Shape};

use super::arrow::{chord_relative_control, control_from_chord};
use super::font::text_advance;
use super::hit::HandleKind;
use super::pencil::geometry_bounds;

#[must_use]
pub fn moved(annotation: &Annotation, delta: Point, snap: bool) -> Annotation {
    let delta = if snap {
        Point {
            x: delta.x.round(),
            y: delta.y.round(),
        }
    } else {
        delta
    };
    let mut changed = annotation.clone();
    match &mut changed.shape {
        Shape::Pencil { geometry, .. } => match geometry {
            PencilGeometry::Freehand(points) => {
                for point in points {
                    point.x += delta.x;
                    point.y += delta.y;
                }
            }
            PencilGeometry::Line(points) => {
                for point in points {
                    point.x += delta.x;
                    point.y += delta.y;
                }
            }
            PencilGeometry::Rectangle(rect) | PencilGeometry::Ellipse(rect) => {
                rect.x += delta.x;
                rect.y += delta.y;
            }
        },
        Shape::Highlight { rect, .. } => {
            rect.x += delta.x;
            rect.y += delta.y;
        }
        Shape::Arrow {
            start,
            end,
            control,
            ..
        } => {
            for point in [start, end, control] {
                point.x += delta.x;
                point.y += delta.y;
            }
        }
        Shape::Measurement {
            axis, from, to, at, ..
        } => match axis {
            Axis::Horizontal => {
                *from += delta.x;
                *to += delta.x;
                *at += delta.y;
            }
            Axis::Vertical => {
                *from += delta.y;
                *to += delta.y;
                *at += delta.x;
            }
        },
        Shape::Text { anchor, .. } => {
            anchor.x += delta.x;
            anchor.y += delta.y;
        }
    }
    changed
}

#[must_use]
pub fn handle_drag(
    annotation: &Annotation,
    kind: HandleKind,
    pointer: Point,
    preserve_aspect: bool,
) -> Annotation {
    let mut changed = annotation.clone();
    match &mut changed.shape {
        Shape::Pencil { geometry, .. } => match geometry {
            PencilGeometry::Line(points) => match kind {
                HandleKind::Start => {
                    if let Some(start) = points.first_mut() {
                        *start = pointer;
                    }
                }
                HandleKind::End => {
                    if let Some(end) = points.last_mut() {
                        *end = pointer;
                    }
                }
                HandleKind::Vertex(index) => {
                    if let Some(vertex) = points.get_mut(index) {
                        *vertex = pointer;
                    }
                }
                _ => {}
            },
            PencilGeometry::Rectangle(rect) | PencilGeometry::Ellipse(rect) => {
                *rect = resized_rect(*rect, kind, pointer, preserve_aspect);
            }
            PencilGeometry::Freehand(points) => {
                let original = geometry_bounds(&PencilGeometry::Freehand(points.clone()));
                let resized = resized_rect(original, kind, pointer, preserve_aspect);
                resize_freehand(points, original, resized);
            }
        },
        Shape::Highlight { rect, .. } => {
            *rect = resized_rect(*rect, kind, pointer, preserve_aspect);
        }
        Shape::Arrow {
            start,
            end,
            control,
            ..
        } => match kind {
            HandleKind::Control => *control = pointer,
            HandleKind::Start => {
                let relative = chord_relative_control(*start, *end, *control);
                *start = pointer;
                *control = control_from_chord(*start, *end, relative.0, relative.1);
            }
            HandleKind::End => {
                let relative = chord_relative_control(*start, *end, *control);
                *end = pointer;
                *control = control_from_chord(*start, *end, relative.0, relative.1);
            }
            _ => {}
        },
        Shape::Measurement {
            axis, from, to, at, ..
        } => {
            let coordinate = match axis {
                Axis::Horizontal => pointer.x.round(),
                Axis::Vertical => pointer.y.round(),
            };
            match kind {
                HandleKind::Start => *from = coordinate.min(*to),
                HandleKind::End => *to = coordinate.max(*from),
                _ => {}
            }
            *at = at.round();
        }
        Shape::Text {
            anchor,
            angle,
            font_size,
            bend,
            text,
            ..
        } => match kind {
            HandleKind::End => {
                let length_at_one = text_advance(text, 1.0).max(f32::EPSILON);
                *font_size = anchor.distance(pointer) / length_at_one;
                *angle = (pointer.y - anchor.y).atan2(pointer.x - anchor.x);
            }
            HandleKind::Start => {
                let end = Point {
                    x: anchor.x + text_advance(text, *font_size) * angle.cos(),
                    y: anchor.y + text_advance(text, *font_size) * angle.sin(),
                };
                let length_at_one = text_advance(text, 1.0).max(f32::EPSILON);
                *font_size = end.distance(pointer) / length_at_one;
                *angle = (end.y - pointer.y).atan2(end.x - pointer.x);
                *anchor = pointer;
            }
            HandleKind::Control => {
                let midpoint = Point {
                    x: anchor.x + text_advance(text, *font_size) * angle.cos() / 2.0,
                    y: anchor.y + text_advance(text, *font_size) * angle.sin() / 2.0,
                };
                *bend = (pointer.x - midpoint.x)
                    .mul_add(-angle.sin(), (pointer.y - midpoint.y) * angle.cos());
            }
            _ => {}
        },
    }
    changed
}

fn resize_freehand(points: &mut [crate::document::BrushPoint], original: Rect, resized: Rect) {
    let map_axis = |value: f32, from: f32, length: f32, to: f32, new_length: f32| {
        if length.abs() <= f32::EPSILON {
            to + new_length / 2.0
        } else {
            to + (value - from) / length * new_length
        }
    };
    for point in points {
        point.x = map_axis(
            point.x,
            original.x,
            original.width,
            resized.x,
            resized.width,
        );
        point.y = map_axis(
            point.y,
            original.y,
            original.height,
            resized.y,
            resized.height,
        );
    }
}

#[must_use]
pub fn rotated_text(annotation: &Annotation, delta_angle: f32, snap: bool) -> Annotation {
    let mut changed = annotation.clone();
    let Shape::Text {
        anchor,
        angle,
        font_size,
        text,
        ..
    } = &mut changed.shape
    else {
        return changed;
    };
    let end = Point {
        x: anchor.x + text_advance(text, *font_size) * angle.cos(),
        y: anchor.y + text_advance(text, *font_size) * angle.sin(),
    };
    let midpoint = anchor.midpoint(end);
    let delta = if snap {
        let step = 15.0_f32.to_radians();
        (delta_angle / step).round() * step
    } else {
        delta_angle
    };
    let cos = delta.cos();
    let sin = delta.sin();
    let relative = Point {
        x: anchor.x - midpoint.x,
        y: anchor.y - midpoint.y,
    };
    *anchor = Point {
        x: midpoint.x + relative.x * cos - relative.y * sin,
        y: midpoint.y + relative.x * sin + relative.y * cos,
    };
    *angle += delta;
    changed
}

fn resized_rect(rect: Rect, kind: HandleKind, pointer: Point, preserve_aspect: bool) -> Rect {
    const MINIMUM_SIZE: f32 = 4.0;
    let left = rect.x;
    let top = rect.y;
    let right = rect.x + rect.width;
    let bottom = rect.y + rect.height;
    let moves_left = matches!(
        kind,
        HandleKind::NorthWest | HandleKind::West | HandleKind::SouthWest
    );
    let moves_right = matches!(
        kind,
        HandleKind::NorthEast | HandleKind::East | HandleKind::SouthEast
    );
    let moves_top = matches!(
        kind,
        HandleKind::NorthWest | HandleKind::North | HandleKind::NorthEast
    );
    let moves_bottom = matches!(
        kind,
        HandleKind::SouthWest | HandleKind::South | HandleKind::SouthEast
    );
    if !(moves_left || moves_right || moves_top || moves_bottom) {
        return rect;
    }

    let mut width = if moves_left {
        right - pointer.x.min(right - MINIMUM_SIZE)
    } else if moves_right {
        pointer.x.max(left + MINIMUM_SIZE) - left
    } else {
        rect.width
    };
    let mut height = if moves_top {
        bottom - pointer.y.min(bottom - MINIMUM_SIZE)
    } else if moves_bottom {
        pointer.y.max(top + MINIMUM_SIZE) - top
    } else {
        rect.height
    };

    if preserve_aspect && rect.width > f32::EPSILON && rect.height > f32::EPSILON {
        let aspect = rect.width / rect.height;
        if (moves_left || moves_right) && (moves_top || moves_bottom) {
            if width / height > aspect {
                height = width / aspect;
            } else {
                width = height * aspect;
            }
        } else if moves_left || moves_right {
            height = width / aspect;
        } else {
            width = height * aspect;
        }
    }

    let x = if moves_left {
        right - width
    } else if moves_right {
        left
    } else {
        rect.x + (rect.width - width) / 2.0
    };
    let y = if moves_top {
        bottom - height
    } else if moves_bottom {
        top
    } else {
        rect.y + (rect.height - height) / 2.0
    };
    Rect {
        x,
        y,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{AnnotationId, BrushPoint, StrokeStyle};

    #[test]
    fn highlight_resize_keeps_the_opposite_edge_pinned_at_minimum_size() {
        let rect = Rect {
            x: 10.0,
            y: 20.0,
            width: 40.0,
            height: 20.0,
        };
        assert_eq!(
            resized_rect(rect, HandleKind::West, Point { x: 49.0, y: 30.0 }, false),
            Rect {
                x: 46.0,
                y: 20.0,
                width: 4.0,
                height: 20.0,
            }
        );
    }

    #[test]
    fn aspect_locked_edge_resize_expands_about_the_unmoved_axis_center() {
        let rect = Rect {
            x: 10.0,
            y: 20.0,
            width: 40.0,
            height: 20.0,
        };
        assert_eq!(
            resized_rect(rect, HandleKind::East, Point { x: 70.0, y: 30.0 }, true),
            Rect {
                x: 10.0,
                y: 15.0,
                width: 60.0,
                height: 30.0,
            }
        );
    }

    #[test]
    fn freehand_bounding_handle_scales_points_and_preserves_pressure() {
        let annotation = Annotation {
            id: AnnotationId(1),
            shape: Shape::Pencil {
                geometry: PencilGeometry::Freehand(vec![
                    BrushPoint {
                        x: 10.0,
                        y: 20.0,
                        pressure: 0.25,
                    },
                    BrushPoint {
                        x: 30.0,
                        y: 40.0,
                        pressure: 0.75,
                    },
                ]),
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 3.0,
                },
                anti_aliasing: true,
            },
        };

        let changed = handle_drag(
            &annotation,
            HandleKind::SouthEast,
            Point { x: 50.0, y: 80.0 },
            false,
        );
        let Shape::Pencil {
            geometry: PencilGeometry::Freehand(points),
            ..
        } = changed.shape
        else {
            panic!("expected freehand pencil annotation");
        };
        assert_eq!((points[0].x, points[0].y), (10.0, 20.0));
        assert_eq!((points[1].x, points[1].y), (50.0, 80.0));
        assert_eq!((points[0].pressure, points[1].pressure), (0.25, 0.75));
    }

    #[test]
    fn pencil_line_rectangle_and_ellipse_resize_through_their_handles() {
        let annotation = |geometry| Annotation {
            id: AnnotationId(2),
            shape: Shape::Pencil {
                geometry,
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 3.0,
                },
                anti_aliasing: true,
            },
        };

        let line = handle_drag(
            &annotation(PencilGeometry::Line(vec![
                Point { x: 10.0, y: 20.0 },
                Point { x: 30.0, y: 40.0 },
            ])),
            HandleKind::End,
            Point { x: 50.0, y: 60.0 },
            false,
        );
        assert!(matches!(
            line.shape,
            Shape::Pencil {
                geometry: PencilGeometry::Line(points),
                ..
            } if points.last() == Some(&Point { x: 50.0, y: 60.0 })
        ));

        let polyline = handle_drag(
            &annotation(PencilGeometry::Line(vec![
                Point { x: 10.0, y: 20.0 },
                Point { x: 20.0, y: 30.0 },
                Point { x: 30.0, y: 40.0 },
            ])),
            HandleKind::Vertex(1),
            Point { x: 22.0, y: 35.0 },
            false,
        );
        assert!(matches!(
            polyline.shape,
            Shape::Pencil {
                geometry: PencilGeometry::Line(points),
                ..
            } if points == [
                Point { x: 10.0, y: 20.0 },
                Point { x: 22.0, y: 35.0 },
                Point { x: 30.0, y: 40.0 },
            ]
        ));

        for geometry in [
            PencilGeometry::Rectangle(Rect {
                x: 10.0,
                y: 20.0,
                width: 20.0,
                height: 20.0,
            }),
            PencilGeometry::Ellipse(Rect {
                x: 10.0,
                y: 20.0,
                width: 20.0,
                height: 20.0,
            }),
        ] {
            let changed = handle_drag(
                &annotation(geometry),
                HandleKind::SouthEast,
                Point { x: 50.0, y: 70.0 },
                false,
            );
            let Shape::Pencil { geometry, .. } = changed.shape else {
                panic!("expected pencil geometry");
            };
            let rect = match geometry {
                PencilGeometry::Rectangle(rect) | PencilGeometry::Ellipse(rect) => rect,
                _ => panic!("expected bounded pencil geometry"),
            };
            assert_eq!(
                rect,
                Rect {
                    x: 10.0,
                    y: 20.0,
                    width: 40.0,
                    height: 50.0,
                }
            );
        }
    }
}
