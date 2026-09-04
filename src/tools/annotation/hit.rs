use crate::document::{
    Annotation, AnnotationId, Axis, HIGHLIGHT_STROKE_WIDTH, PencilGeometry, Point, Rect, Shape,
};

use super::arrow::curve_points;
use super::geometry::{distance_to_polyline, distance_to_segment};
use super::highlight::sloppy_ellipse;
use super::pencil::{geometry_bounds, outline_points};
use super::text::baseline;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleKind {
    NorthWest,
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
    Start,
    End,
    Vertex(usize),
    Control,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitKind {
    Handle(HandleKind),
    Rotate,
    Body,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub id: AnnotationId,
    pub kind: HitKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorKind {
    Crosshair,
    Move,
    NorthWestSouthEastResize,
    NorthEastSouthWestResize,
    EastWestResize,
    NorthSouthResize,
    Grab,
    Grabbing,
    Rotate,
}

impl CursorKind {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Crosshair => "crosshair",
            Self::Move => "move",
            Self::NorthWestSouthEastResize => "nwse-resize",
            Self::NorthEastSouthWestResize => "nesw-resize",
            Self::EastWestResize => "ew-resize",
            Self::NorthSouthResize => "ns-resize",
            Self::Grab => "grab",
            Self::Grabbing => "grabbing",
            Self::Rotate => "grab",
        }
    }
}

#[must_use]
pub fn cursor_for_hit(hit: Option<Hit>, dragging: bool) -> CursorKind {
    match hit.map(|hit| hit.kind) {
        None => CursorKind::Crosshair,
        Some(HitKind::Body) => CursorKind::Move,
        Some(HitKind::Rotate) => CursorKind::Rotate,
        Some(HitKind::Handle(HandleKind::NorthWest | HandleKind::SouthEast)) => {
            CursorKind::NorthWestSouthEastResize
        }
        Some(HitKind::Handle(HandleKind::NorthEast | HandleKind::SouthWest)) => {
            CursorKind::NorthEastSouthWestResize
        }
        Some(HitKind::Handle(HandleKind::East | HandleKind::West)) => CursorKind::EastWestResize,
        Some(HitKind::Handle(HandleKind::North | HandleKind::South)) => {
            CursorKind::NorthSouthResize
        }
        Some(HitKind::Handle(HandleKind::Control)) if dragging => CursorKind::Grabbing,
        Some(HitKind::Handle(HandleKind::Control)) => CursorKind::Grab,
        Some(HitKind::Handle(HandleKind::Start | HandleKind::End | HandleKind::Vertex(_))) => {
            CursorKind::Crosshair
        }
    }
}

#[must_use]
pub fn hit_test(
    annotations: &[Annotation],
    selected: Option<AnnotationId>,
    point: Point,
    tolerance: f32,
) -> Option<Hit> {
    if let Some(selected) = selected
        && let Some(annotation) = annotations
            .iter()
            .find(|annotation| annotation.id == selected)
    {
        if let Some(kind) = handle_hit(annotation, point, tolerance) {
            return Some(Hit {
                id: selected,
                kind: HitKind::Handle(kind),
            });
        }
        if text_rotation_ring_hit(annotation, point, tolerance) {
            return Some(Hit {
                id: selected,
                kind: HitKind::Rotate,
            });
        }
    }

    annotations.iter().rev().find_map(|annotation| {
        body_hit(annotation, point, tolerance).then_some(Hit {
            id: annotation.id,
            kind: HitKind::Body,
        })
    })
}

#[must_use]
pub fn handles(annotation: &Annotation) -> Vec<(HandleKind, Point)> {
    match &annotation.shape {
        Shape::Pencil {
            geometry: PencilGeometry::Line(points),
            ..
        } => points
            .iter()
            .enumerate()
            .map(|(index, point)| {
                let kind = if index == 0 {
                    HandleKind::Start
                } else if index + 1 == points.len() {
                    HandleKind::End
                } else {
                    HandleKind::Vertex(index)
                };
                (kind, *point)
            })
            .collect(),
        Shape::Pencil { geometry, .. } => rectangular_handles(geometry_bounds(geometry)),
        Shape::Highlight { rect, .. } => rectangular_handles(*rect),
        Shape::Arrow {
            start,
            end,
            control,
            ..
        } => vec![
            (HandleKind::Start, *start),
            (HandleKind::End, *end),
            (HandleKind::Control, *control),
        ],
        Shape::Measurement {
            axis, from, to, at, ..
        } => match axis {
            Axis::Horizontal => vec![
                (HandleKind::Start, Point { x: *from, y: *at }),
                (HandleKind::End, Point { x: *to, y: *at }),
            ],
            Axis::Vertical => vec![
                (HandleKind::Start, Point { x: *at, y: *from }),
                (HandleKind::End, Point { x: *at, y: *to }),
            ],
        },
        Shape::Text {
            anchor,
            angle,
            font_size,
            bend,
            text,
            ..
        } => {
            let curve = baseline(*anchor, *angle, *bend, text, *font_size);
            let end = *curve.last().unwrap_or(anchor);
            let middle = curve[curve.len() / 2];
            vec![
                (HandleKind::Start, *anchor),
                (HandleKind::End, end),
                (HandleKind::Control, middle),
            ]
        }
    }
}

fn rectangular_handles(rect: Rect) -> Vec<(HandleKind, Point)> {
    let left = rect.x;
    let center_x = rect.x + rect.width / 2.0;
    let right = rect.x + rect.width;
    let top = rect.y;
    let center_y = rect.y + rect.height / 2.0;
    let bottom = rect.y + rect.height;
    vec![
        (HandleKind::NorthWest, Point { x: left, y: top }),
        (
            HandleKind::North,
            Point {
                x: center_x,
                y: top,
            },
        ),
        (HandleKind::NorthEast, Point { x: right, y: top }),
        (
            HandleKind::East,
            Point {
                x: right,
                y: center_y,
            },
        ),
        (
            HandleKind::SouthEast,
            Point {
                x: right,
                y: bottom,
            },
        ),
        (
            HandleKind::South,
            Point {
                x: center_x,
                y: bottom,
            },
        ),
        (HandleKind::SouthWest, Point { x: left, y: bottom }),
        (
            HandleKind::West,
            Point {
                x: left,
                y: center_y,
            },
        ),
    ]
}

fn handle_hit(annotation: &Annotation, point: Point, tolerance: f32) -> Option<HandleKind> {
    handles(annotation)
        .into_iter()
        .find_map(|(kind, handle)| (point.distance(handle) <= tolerance).then_some(kind))
}

fn text_rotation_ring_hit(annotation: &Annotation, point: Point, tolerance: f32) -> bool {
    let Shape::Text { .. } = annotation.shape else {
        return false;
    };
    handles(annotation).into_iter().any(|(kind, handle)| {
        matches!(
            kind,
            HandleKind::Start | HandleKind::End | HandleKind::Vertex(_)
        ) && point.distance(handle) > tolerance
            && point.distance(handle) <= tolerance * 2.5
    })
}

fn body_hit(annotation: &Annotation, point: Point, tolerance: f32) -> bool {
    match &annotation.shape {
        Shape::Pencil {
            geometry, style, ..
        } => {
            distance_to_polyline(point, &outline_points(geometry)) <= tolerance + style.width / 2.0
        }
        Shape::Highlight { rect, seed, .. } => {
            distance_to_polyline(point, &sloppy_ellipse(*rect, *seed))
                <= tolerance + HIGHLIGHT_STROKE_WIDTH / 2.0
        }
        Shape::Arrow {
            start,
            end,
            control,
            style,
        } => {
            distance_to_polyline(point, &curve_points(*start, *control, *end))
                <= tolerance + style.width / 2.0
        }
        Shape::Measurement {
            axis,
            from,
            to,
            at,
            style,
            ..
        } => {
            let (start, end) = match axis {
                Axis::Horizontal => (Point { x: *from, y: *at }, Point { x: *to, y: *at }),
                Axis::Vertical => (Point { x: *at, y: *from }, Point { x: *at, y: *to }),
            };
            distance_to_segment(point, start, end) <= tolerance + style.width / 2.0
        }
        Shape::Text {
            anchor,
            angle,
            font_size,
            bend,
            text,
            ..
        } => {
            distance_to_polyline(point, &baseline(*anchor, *angle, *bend, text, *font_size))
                <= tolerance + *font_size / 2.0
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::document::{BrushPoint, PencilGeometry, Rect, StrokeStyle};

    use super::*;

    fn selected() -> Annotation {
        Annotation {
            id: AnnotationId(1),
            shape: Shape::Highlight {
                rect: Rect {
                    x: 10.0,
                    y: 10.0,
                    width: 40.0,
                    height: 30.0,
                },
                seed: 1,
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 3.0,
                },
            },
        }
    }

    #[test]
    fn selected_handle_beats_body() {
        let annotation = selected();
        let hit = hit_test(
            &[annotation],
            Some(AnnotationId(1)),
            Point { x: 10.0, y: 10.0 },
            8.0,
        )
        .unwrap();
        assert_eq!(hit.kind, HitKind::Handle(HandleKind::NorthWest));
    }

    #[test]
    fn topmost_body_wins() {
        let first = selected();
        let mut second = first.clone();
        second.id = AnnotationId(2);
        let point = sloppy_ellipse(
            match first.shape {
                Shape::Highlight { rect, .. } => rect,
                _ => unreachable!(),
            },
            1,
        )[0];
        assert_eq!(
            hit_test(&[first, second], None, point, 8.0).unwrap().id,
            AnnotationId(2)
        );
    }

    #[test]
    fn control_handle_cursor_reflects_drag_state() {
        let hit = Some(Hit {
            id: AnnotationId(1),
            kind: HitKind::Handle(HandleKind::Control),
        });

        assert_eq!(cursor_for_hit(hit, false).name(), "grab");
        assert_eq!(cursor_for_hit(hit, true).name(), "grabbing");
    }

    #[test]
    fn pencil_shapes_expose_endpoint_or_bounding_box_handles() {
        let style = StrokeStyle {
            color: [255, 0, 0, 255],
            width: 3.0,
        };
        let annotation = |geometry| Annotation {
            id: AnnotationId(2),
            shape: Shape::Pencil {
                geometry,
                style,
                anti_aliasing: true,
            },
        };
        assert_eq!(
            handles(&annotation(PencilGeometry::Line(vec![
                Point { x: 1.0, y: 2.0 },
                Point { x: 8.0, y: 9.0 },
            ])))
            .len(),
            2
        );
        assert_eq!(
            handles(&annotation(PencilGeometry::Line(vec![
                Point { x: 1.0, y: 2.0 },
                Point { x: 4.0, y: 5.0 },
                Point { x: 8.0, y: 9.0 },
            ]))),
            vec![
                (HandleKind::Start, Point { x: 1.0, y: 2.0 }),
                (HandleKind::Vertex(1), Point { x: 4.0, y: 5.0 }),
                (HandleKind::End, Point { x: 8.0, y: 9.0 }),
            ]
        );
        for geometry in [
            PencilGeometry::Rectangle(Rect {
                x: 1.0,
                y: 2.0,
                width: 7.0,
                height: 9.0,
            }),
            PencilGeometry::Ellipse(Rect {
                x: 1.0,
                y: 2.0,
                width: 7.0,
                height: 9.0,
            }),
            PencilGeometry::Freehand(vec![
                BrushPoint {
                    x: 1.0,
                    y: 2.0,
                    pressure: 0.5,
                },
                BrushPoint {
                    x: 8.0,
                    y: 11.0,
                    pressure: 1.0,
                },
            ]),
        ] {
            assert_eq!(handles(&annotation(geometry)).len(), 8);
        }
    }

    #[test]
    fn a_single_point_pencil_node_can_be_selected() {
        let annotation = Annotation {
            id: AnnotationId(3),
            shape: Shape::Pencil {
                geometry: PencilGeometry::Freehand(vec![BrushPoint {
                    x: 12.5,
                    y: 14.5,
                    pressure: 1.0,
                }]),
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 1.0,
                },
                anti_aliasing: false,
            },
        };

        assert_eq!(
            hit_test(&[annotation], None, Point { x: 12.5, y: 14.5 }, 2.0),
            Some(Hit {
                id: AnnotationId(3),
                kind: HitKind::Body,
            })
        );
    }
}
