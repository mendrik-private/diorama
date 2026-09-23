use crate::document::{Annotation, Axis, PencilGeometry, Point, Rect, Shape};

use super::arrow::{chord_relative_control, control_from_chord};
use super::font::text_advance;
use super::hit::HandleKind;
use super::pencil::geometry_bounds;
use super::text::clamped_bend;

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
            PencilGeometry::RotatedRectangle(points) => {
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
            PencilGeometry::RotatedRectangle(points) => {
                // Resize in the rectangle's own basis, including after image scaling/flipping.
                let origin = points[0];
                let u = Point {
                    x: points[1].x - origin.x,
                    y: points[1].y - origin.y,
                };
                let v = Point {
                    x: points[3].x - origin.x,
                    y: points[3].y - origin.y,
                };
                let width = u.x.hypot(u.y);
                let height = v.x.hypot(v.y);
                let determinant = u.x * v.y - u.y * v.x;
                if determinant.abs() > f32::EPSILON {
                    let dx = pointer.x - origin.x;
                    let dy = pointer.y - origin.y;
                    let local = Point {
                        x: (dx * v.y - dy * v.x) / determinant * width,
                        y: (u.x * dy - u.y * dx) / determinant * height,
                    };
                    let resized = resized_rect(
                        Rect {
                            x: 0.0,
                            y: 0.0,
                            width,
                            height,
                        },
                        kind,
                        local,
                        preserve_aspect,
                    );
                    for (point, local) in points.iter_mut().zip(super::pencil::outline_points(
                        &PencilGeometry::Rectangle(resized),
                    )) {
                        *point = Point {
                            x: origin.x + u.x * local.x / width + v.x * local.y / height,
                            y: origin.y + u.y * local.x / width + v.y * local.y / height,
                        };
                    }
                }
            }
            PencilGeometry::Line(points) => {
                let previous = match kind {
                    HandleKind::Start => points.first().copied(),
                    HandleKind::End => points.last().copied(),
                    HandleKind::Vertex(index) => points.get(index).copied(),
                    _ => None,
                };
                if let Some(previous) = previous {
                    // Closing a polyline repeats its first vertex at the end. Keep
                    // that intentional weld when either endpoint is edited.
                    for vertex in points {
                        if *vertex == previous {
                            *vertex = pointer;
                        }
                    }
                }
            }
            PencilGeometry::Rectangle(rect) | PencilGeometry::Ellipse(rect) => {
                *rect = resized_rect(*rect, kind, pointer, preserve_aspect);
            }
            PencilGeometry::Freehand(points) => {
                let original = geometry_bounds(&PencilGeometry::Freehand(points.clone()));
                let resized = resized_rect(original, kind, pointer, preserve_aspect);
                resize_freehand(points, original, resized);
            }
        },
        Shape::Highlight { rect, angle, .. } => {
            let center = rect.center();
            let local_pointer = inverse_rotate_point(pointer, center, *angle);
            let resized = resized_rect(*rect, kind, local_pointer, preserve_aspect);
            // `rect` is the oval's local frame while its center is in image
            // coordinates. Shift the frame so the dragged rotated handle lands
            // precisely beneath the pointer as its local center changes.
            let previous_center = center;
            let next_center = resized.center();
            let rotated_next_center = rotate_point(next_center, previous_center, *angle);
            *rect = Rect {
                x: resized.x + rotated_next_center.x - next_center.x,
                y: resized.y + rotated_next_center.y - next_center.y,
                ..resized
            };
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
                *bend = clamped_bend(*bend, text_advance(text, *font_size));
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
                *bend = clamped_bend(*bend, text_advance(text, *font_size));
            }
            HandleKind::Control => {
                let midpoint = Point {
                    x: anchor.x + text_advance(text, *font_size) * angle.cos() / 2.0,
                    y: anchor.y + text_advance(text, *font_size) * angle.sin() / 2.0,
                };
                // The visible midpoint is at half the quadratic control height.
                let height = (pointer.x - midpoint.x)
                    .mul_add(-angle.sin(), (pointer.y - midpoint.y) * angle.cos());
                *bend = clamped_bend(height * 2.0, text_advance(text, *font_size));
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
pub fn rotated(annotation: &Annotation, delta_angle: f32, snap: bool) -> Annotation {
    let mut changed = annotation.clone();
    if let Shape::Pencil { geometry, .. } = &mut changed.shape {
        if matches!(
            geometry,
            PencilGeometry::Rectangle(_) | PencilGeometry::RotatedRectangle(_)
        ) {
            let outline = super::pencil::outline_points(geometry);
            let center = outline[0].midpoint(outline[2]);
            let delta = rotation_delta(delta_angle, snap);
            *geometry = PencilGeometry::RotatedRectangle(std::array::from_fn(|i| {
                rotate_point(outline[i], center, delta)
            }));
        }
        return changed;
    }
    if let Shape::Arrow {
        start,
        end,
        control,
        ..
    } = &mut changed.shape
    {
        let center = start.midpoint(*end);
        let delta = rotation_delta(delta_angle, snap);
        for point in [start, end, control] {
            *point = rotate_point(*point, center, delta);
        }
        return changed;
    }
    if let Shape::Highlight { angle, .. } = &mut changed.shape {
        *angle += rotation_delta(delta_angle, snap);
        return changed;
    }
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
    let delta = rotation_delta(delta_angle, snap);
    *anchor = rotate_point(*anchor, midpoint, delta);
    *angle += delta;
    changed
}

fn rotation_delta(delta: f32, snap: bool) -> f32 {
    if snap {
        let step = 15.0_f32.to_radians();
        (delta / step).round() * step
    } else {
        delta
    }
}

fn rotate_point(point: Point, center: Point, angle: f32) -> Point {
    let (sin, cos) = angle.sin_cos();
    let x = point.x - center.x;
    let y = point.y - center.y;
    Point {
        x: center.x + x * cos - y * sin,
        y: center.y + x * sin + y * cos,
    }
}

fn inverse_rotate_point(point: Point, center: Point, angle: f32) -> Point {
    rotate_point(point, center, -angle)
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
    fn rectangle_rotation_keeps_editing_and_closed_stroke_geometry() {
        use super::super::hit::{HitKind, handles, hit_test};
        use super::super::pencil::{outline_points, stroke_for};

        let original = Annotation {
            id: AnnotationId(9),
            shape: Shape::Pencil {
                geometry: PencilGeometry::Rectangle(Rect {
                    x: 40.0,
                    y: 40.0,
                    width: 120.0,
                    height: 80.0,
                }),
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 3.0,
                },
                anti_aliasing: true,
            },
        };
        let close = |a: Point, b: Point| assert!(a.distance(b) < 0.001, "{a:?} != {b:?}");
        for annotation in [
            original.clone(),
            rotated(&original, 37.0_f32.to_radians(), false),
        ] {
            let corner = handles(&annotation)[0].1;
            assert_eq!(
                hit_test(
                    std::slice::from_ref(&annotation),
                    Some(annotation.id),
                    corner,
                    8.0
                )
                .unwrap()
                .kind,
                HitKind::Handle(HandleKind::NorthWest)
            );
            let nearby = Point {
                x: corner.x - 12.0,
                y: corner.y,
            };
            assert_eq!(
                hit_test(
                    std::slice::from_ref(&annotation),
                    Some(annotation.id),
                    nearby,
                    8.0
                )
                .unwrap()
                .kind,
                HitKind::Rotate
            );
            assert!(
                !matches!(hit_test(std::slice::from_ref(&annotation), None, nearby, 8.0), Some(hit) if hit.kind == HitKind::Rotate)
            );
        }
        let turned = rotated(&original, std::f32::consts::FRAC_PI_2, false);
        let positions = handles(&turned);
        close(positions[0].1, Point { x: 140.0, y: 20.0 });
        close(positions[4].1, Point { x: 60.0, y: 140.0 });
        let resized = handle_drag(
            &turned,
            HandleKind::SouthEast,
            Point { x: 40.0, y: 180.0 },
            false,
        );
        close(handles(&resized)[0].1, positions[0].1);
        close(handles(&resized)[4].1, Point { x: 40.0, y: 180.0 });
        let restored = rotated(&turned, -std::f32::consts::FRAC_PI_2, false);
        for (actual, expected) in handles(&restored).iter().zip(handles(&original)) {
            close(actual.1, expected.1);
        }
        let snapped = rotated(&original, 38.0_f32.to_radians(), true);
        let expected = rotated(&original, 45.0_f32.to_radians(), false);
        for (actual, expected) in handles(&snapped).iter().zip(handles(&expected)) {
            close(actual.1, expected.1);
        }
        let translated = moved(&turned, Point { x: 10.0, y: -5.0 }, false);
        close(handles(&translated)[0].1, Point { x: 150.0, y: 15.0 });
        let Shape::Pencil {
            geometry,
            style,
            anti_aliasing,
        } = &turned.shape
        else {
            unreachable!()
        };
        let outline = outline_points(geometry);
        assert_eq!(outline.len(), 5);
        assert_eq!(outline.first(), outline.last());
        let stroke = stroke_for(geometry, *style, *anti_aliasing);
        assert_eq!(stroke.points.len(), 5);
        for (point, expected) in stroke.points.iter().zip(outline) {
            close(
                Point {
                    x: point.x,
                    y: point.y,
                },
                expected,
            );
        }
    }

    #[test]
    fn highlight_rotation_keeps_handles_hit_testing_and_resize_in_its_local_frame() {
        use super::super::hit::{HitKind, handles, hit_test};

        let original = Annotation {
            id: AnnotationId(11),
            shape: Shape::Highlight {
                rect: Rect {
                    x: 40.0,
                    y: 40.0,
                    width: 120.0,
                    height: 80.0,
                },
                angle: 0.0,
                seed: 7,
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 1.0,
                },
            },
        };
        let turned = rotated(&original, std::f32::consts::FRAC_PI_2, false);
        let Shape::Highlight { angle, .. } = turned.shape else {
            unreachable!()
        };
        assert!((angle - std::f32::consts::FRAC_PI_2).abs() < 0.001);
        let positions = handles(&turned);
        let northwest = positions[0].1;
        assert!(northwest.distance(Point { x: 140.0, y: 20.0 }) < 0.001);
        assert_eq!(
            hit_test(
                std::slice::from_ref(&turned),
                Some(turned.id),
                northwest,
                8.0
            )
            .expect("selected rotated highlight handle")
            .kind,
            HitKind::Handle(HandleKind::NorthWest)
        );
        let center = Point { x: 100.0, y: 80.0 };
        let ring = Point {
            x: northwest.x + (northwest.x - center.x) / northwest.distance(center) * 12.0,
            y: northwest.y + (northwest.y - center.y) / northwest.distance(center) * 12.0,
        };
        assert_eq!(
            hit_test(std::slice::from_ref(&turned), Some(turned.id), ring, 8.0)
                .expect("selected rotated highlight rotation ring")
                .kind,
            HitKind::Rotate
        );

        let resized = handle_drag(
            &turned,
            HandleKind::SouthEast,
            Point { x: 40.0, y: 180.0 },
            false,
        );
        assert!(handles(&resized)[0].1.distance(northwest) < 0.001);
        assert!(handles(&resized)[4].1.distance(Point { x: 40.0, y: 180.0 }) < 0.001);
        let snapped = rotated(&original, 38.0_f32.to_radians(), true);
        let expected = rotated(&original, 45.0_f32.to_radians(), false);
        assert_eq!(snapped, expected);
        let translated = moved(&turned, Point { x: 10.0, y: -5.0 }, false);
        assert!(
            handles(&translated)[0]
                .1
                .distance(Point { x: 150.0, y: 15.0 })
                < 0.001
        );
    }

    #[test]
    fn arrow_rotation_uses_endpoint_rings_and_preserves_its_curve() {
        use super::super::hit::{HitKind, hit_test};

        let original = Annotation {
            id: AnnotationId(10),
            shape: Shape::Arrow {
                start: Point { x: 20.0, y: 30.0 },
                end: Point { x: 80.0, y: 30.0 },
                control: Point { x: 50.0, y: 50.0 },
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 3.0,
                },
            },
        };
        let ring = Point { x: 8.0, y: 30.0 };
        assert_eq!(
            hit_test(
                std::slice::from_ref(&original),
                Some(original.id),
                ring,
                8.0
            )
            .expect("selected arrow endpoint ring")
            .kind,
            HitKind::Rotate
        );
        assert_eq!(
            hit_test(
                std::slice::from_ref(&original),
                Some(original.id),
                Point { x: 20.0, y: 30.0 },
                8.0
            )
            .expect("arrow endpoint handle")
            .kind,
            HitKind::Handle(HandleKind::Start),
            "endpoint handles take precedence over their rotation rings"
        );
        assert!(
            !matches!(
                hit_test(
                    std::slice::from_ref(&original),
                    Some(original.id),
                    Point { x: 50.0, y: 62.0 },
                    8.0
                ),
                Some(hit) if hit.kind == HitKind::Rotate
            ),
            "the arrow's bend control has no rotation ring"
        );
        assert!(
            !matches!(
                hit_test(std::slice::from_ref(&original), None, ring, 8.0),
                Some(hit) if hit.kind == HitKind::Rotate
            ),
            "rotation rings appear only for selected arrows"
        );

        let turned = rotated(&original, std::f32::consts::FRAC_PI_2, false);
        let Shape::Arrow {
            start,
            end,
            control,
            ..
        } = turned.shape
        else {
            unreachable!()
        };
        assert!(start.distance(Point { x: 50.0, y: 0.0 }) < 0.001);
        assert!(end.distance(Point { x: 50.0, y: 60.0 }) < 0.001);
        assert!(control.distance(Point { x: 30.0, y: 30.0 }) < 0.001);
        assert!(start.midpoint(end).distance(Point { x: 50.0, y: 30.0 }) < 0.001);

        let snapped = rotated(&original, 38.0_f32.to_radians(), true);
        let expected = rotated(&original, 45.0_f32.to_radians(), false);
        let (
            Shape::Arrow {
                start: snapped_start,
                end: snapped_end,
                control: snapped_control,
                ..
            },
            Shape::Arrow {
                start: expected_start,
                end: expected_end,
                control: expected_control,
                ..
            },
        ) = (snapped.shape, expected.shape)
        else {
            unreachable!()
        };
        for (actual, expected) in [
            (snapped_start, expected_start),
            (snapped_end, expected_end),
            (snapped_control, expected_control),
        ] {
            assert!(
                actual.distance(expected) < 0.001,
                "{actual:?} != {expected:?}"
            );
        }
    }

    #[test]
    fn text_bend_handle_tracks_pointer_and_stops_at_limit() {
        let text = "Bending";
        let width = text_advance(text, 24.0);
        let annotation = Annotation {
            id: AnnotationId(1),
            shape: Shape::Text {
                anchor: Point::default(),
                angle: 0.0,
                font_size: 24.0,
                bend: 0.0,
                text: text.to_owned(),
                color: [255, 0, 0, 255],
            },
        };
        for height in [-10000.0_f32, -5.0, 5.0, 10000.0] {
            let changed = handle_drag(
                &annotation,
                HandleKind::Control,
                Point {
                    x: width / 2.0,
                    y: height,
                },
                false,
            );
            let handles = super::super::hit::handles(&changed);
            let (_, midpoint) = handles
                .iter()
                .find(|(kind, _)| *kind == HandleKind::Control)
                .unwrap();
            assert!((midpoint.y - height.clamp(-width / 4.0, width / 4.0)).abs() < 1e-4);
        }
    }

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

        let closed = handle_drag(
            &annotation(PencilGeometry::Line(vec![
                Point { x: 10.0, y: 20.0 },
                Point { x: 30.0, y: 40.0 },
                Point { x: 50.0, y: 20.0 },
                Point { x: 10.0, y: 20.0 },
            ])),
            HandleKind::Start,
            Point { x: 12.0, y: 24.0 },
            false,
        );
        assert!(matches!(
            closed.shape,
            Shape::Pencil {
                geometry: PencilGeometry::Line(points),
                ..
            } if points == [
                Point { x: 12.0, y: 24.0 },
                Point { x: 30.0, y: 40.0 },
                Point { x: 50.0, y: 20.0 },
                Point { x: 12.0, y: 24.0 },
            ]
        ));

        let closed_from_end = handle_drag(
            &annotation(PencilGeometry::Line(vec![
                Point { x: 10.0, y: 20.0 },
                Point { x: 30.0, y: 40.0 },
                Point { x: 50.0, y: 20.0 },
                Point { x: 10.0, y: 20.0 },
            ])),
            HandleKind::End,
            Point { x: 12.0, y: 24.0 },
            false,
        );
        assert!(matches!(
            closed_from_end.shape,
            Shape::Pencil {
                geometry: PencilGeometry::Line(points),
                ..
            } if points.first() == Some(&Point { x: 12.0, y: 24.0 })
                && points.last() == Some(&Point { x: 12.0, y: 24.0 })
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
