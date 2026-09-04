use std::f32::consts::FRAC_PI_2;

use super::{BrushPoint, Operation, Rotation};

pub const HIGHLIGHT_STROKE_WIDTH: f32 = 1.0;
pub const MEASUREMENT_STROKE_WIDTH: f32 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AnnotationId(pub u64);

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    #[must_use]
    pub fn midpoint(self, other: Self) -> Self {
        Self {
            x: (self.x + other.x) / 2.0,
            y: (self.y + other.y) / 2.0,
        }
    }

    #[must_use]
    pub fn distance(self, other: Self) -> f32 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    #[must_use]
    pub fn from_points(first: Point, second: Point) -> Self {
        let x = first.x.min(second.x);
        let y = first.y.min(second.y);
        Self {
            x,
            y,
            width: first.x.max(second.x) - x,
            height: first.y.max(second.y) - y,
        }
    }

    #[must_use]
    pub fn center(self) -> Point {
        Point {
            x: self.x + self.width / 2.0,
            y: self.y + self.height / 2.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrokeStyle {
    pub color: [u8; 4],
    pub width: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PencilGeometry {
    Freehand(Vec<BrushPoint>),
    Line(Vec<Point>),
    Rectangle(Rect),
    Ellipse(Rect),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Pencil {
        geometry: PencilGeometry,
        style: StrokeStyle,
        anti_aliasing: bool,
    },
    Highlight {
        rect: Rect,
        seed: u64,
        style: StrokeStyle,
    },
    Arrow {
        start: Point,
        end: Point,
        control: Point,
        style: StrokeStyle,
    },
    Measurement {
        axis: Axis,
        from: f32,
        to: f32,
        at: f32,
        style: StrokeStyle,
        label_size: f32,
    },
    Text {
        anchor: Point,
        angle: f32,
        font_size: f32,
        bend: f32,
        text: String,
        color: [u8; 4],
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    pub id: AnnotationId,
    pub shape: Shape,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationEdit {
    Create(Annotation),
    Set(Annotation),
    Delete(AnnotationId),
}

#[must_use]
pub fn fold_annotations(
    source_dimensions: (u32, u32),
    operations: &[Operation],
) -> Vec<Annotation> {
    let mut dimensions = (source_dimensions.0 as f32, source_dimensions.1 as f32);
    let mut annotations = Vec::new();

    for operation in operations {
        match operation {
            Operation::Annotate(AnnotationEdit::Create(annotation)) => {
                if let Some(existing) = annotations
                    .iter_mut()
                    .find(|candidate: &&mut Annotation| candidate.id == annotation.id)
                {
                    *existing = annotation.clone();
                } else {
                    annotations.push(annotation.clone());
                }
            }
            Operation::Annotate(AnnotationEdit::Set(annotation)) => {
                if let Some(existing) = annotations
                    .iter_mut()
                    .find(|candidate| candidate.id == annotation.id)
                {
                    *existing = annotation.clone();
                }
            }
            Operation::Annotate(AnnotationEdit::Delete(id)) => {
                annotations.retain(|annotation| annotation.id != *id);
            }
            Operation::Crop {
                x,
                y,
                width,
                height,
            } => {
                transform_all(&mut annotations, TransformKind::Crop(*x as f32, *y as f32));
                dimensions = (*width as f32, *height as f32);
            }
            Operation::Rotate(rotation) => {
                let kind = match rotation {
                    Rotation::Clockwise90 => TransformKind::Clockwise90(dimensions),
                    Rotation::CounterClockwise90 => TransformKind::CounterClockwise90(dimensions),
                };
                transform_all(&mut annotations, kind);
                dimensions = (dimensions.1, dimensions.0);
            }
            Operation::FlipHorizontal => {
                transform_all(
                    &mut annotations,
                    TransformKind::FlipHorizontal(dimensions.0),
                );
            }
            Operation::FlipVertical => {
                transform_all(&mut annotations, TransformKind::FlipVertical(dimensions.1));
            }
            Operation::Scale { width, height, .. } => {
                let sx = *width as f32 / dimensions.0.max(f32::EPSILON);
                let sy = *height as f32 / dimensions.1.max(f32::EPSILON);
                transform_all(&mut annotations, TransformKind::Scale(sx, sy));
                dimensions = (*width as f32, *height as f32);
            }
            Operation::Palette { .. } => {}
        }
    }

    annotations
}

#[derive(Debug, Clone, Copy)]
enum TransformKind {
    Crop(f32, f32),
    Clockwise90((f32, f32)),
    CounterClockwise90((f32, f32)),
    FlipHorizontal(f32),
    FlipVertical(f32),
    Scale(f32, f32),
}

impl TransformKind {
    fn point(self, point: Point) -> Point {
        match self {
            Self::Crop(x, y) => Point {
                x: point.x - x,
                y: point.y - y,
            },
            Self::Clockwise90((_, height)) => Point {
                x: height - point.y,
                y: point.x,
            },
            Self::CounterClockwise90((width, _)) => Point {
                x: point.y,
                y: width - point.x,
            },
            Self::FlipHorizontal(width) => Point {
                x: width - point.x,
                y: point.y,
            },
            Self::FlipVertical(height) => Point {
                x: point.x,
                y: height - point.y,
            },
            Self::Scale(sx, sy) => Point {
                x: point.x * sx,
                y: point.y * sy,
            },
        }
    }

    fn width_scale(self) -> f32 {
        match self {
            Self::Scale(sx, sy) => (sx * sy).sqrt(),
            _ => 1.0,
        }
    }
}

fn transform_all(annotations: &mut [Annotation], transform: TransformKind) {
    for annotation in annotations {
        transform_annotation(annotation, transform);
    }
}

fn transform_annotation(annotation: &mut Annotation, transform: TransformKind) {
    match &mut annotation.shape {
        Shape::Pencil {
            geometry, style, ..
        } => {
            match geometry {
                PencilGeometry::Freehand(points) => {
                    for point in points {
                        let transformed = transform.point(Point {
                            x: point.x,
                            y: point.y,
                        });
                        point.x = transformed.x;
                        point.y = transformed.y;
                    }
                }
                PencilGeometry::Line(points) => {
                    for point in points {
                        *point = transform.point(*point);
                    }
                }
                PencilGeometry::Rectangle(rect) | PencilGeometry::Ellipse(rect) => {
                    let first = transform.point(Point {
                        x: rect.x,
                        y: rect.y,
                    });
                    let second = transform.point(Point {
                        x: rect.x + rect.width,
                        y: rect.y + rect.height,
                    });
                    *rect = Rect::from_points(first, second);
                }
            }
            style.width *= transform.width_scale();
        }
        Shape::Highlight { rect, style, .. } => {
            let first = transform.point(Point {
                x: rect.x,
                y: rect.y,
            });
            let second = transform.point(Point {
                x: rect.x + rect.width,
                y: rect.y + rect.height,
            });
            *rect = Rect::from_points(first, second);
            style.width = HIGHLIGHT_STROKE_WIDTH;
        }
        Shape::Arrow {
            start,
            end,
            control,
            style,
        } => {
            *start = transform.point(*start);
            *end = transform.point(*end);
            *control = transform.point(*control);
            style.width *= transform.width_scale();
        }
        Shape::Measurement {
            axis,
            from,
            to,
            at,
            style,
            label_size,
        } => {
            let (first, second) = measurement_points(*axis, *from, *to, *at);
            let first = transform.point(first);
            let second = transform.point(second);
            if (second.x - first.x).abs() >= (second.y - first.y).abs() {
                *axis = Axis::Horizontal;
                *from = first.x.min(second.x);
                *to = first.x.max(second.x);
                *at = (first.y + second.y) / 2.0;
            } else {
                *axis = Axis::Vertical;
                *from = first.y.min(second.y);
                *to = first.y.max(second.y);
                *at = (first.x + second.x) / 2.0;
            }
            style.width = MEASUREMENT_STROKE_WIDTH;
            let scale = transform.width_scale();
            *label_size *= scale;
        }
        Shape::Text {
            anchor,
            angle,
            font_size,
            bend,
            text,
            ..
        } => {
            let advance = crate::tools::annotation::font::text_advance(text, *font_size);
            let end = Point {
                x: anchor.x + advance * angle.cos(),
                y: anchor.y + advance * angle.sin(),
            };
            match transform {
                TransformKind::FlipHorizontal(_) => {
                    *anchor = transform.point(end);
                    *angle = -*angle;
                }
                TransformKind::FlipVertical(_) => {
                    *anchor = transform.point(*anchor);
                    *angle = -*angle;
                    *bend = -*bend;
                }
                TransformKind::Clockwise90(_) => {
                    *anchor = transform.point(*anchor);
                    *angle += FRAC_PI_2;
                }
                TransformKind::CounterClockwise90(_) => {
                    *anchor = transform.point(*anchor);
                    *angle -= FRAC_PI_2;
                }
                TransformKind::Scale(sx, sy) => {
                    *anchor = transform.point(*anchor);
                    let direction_scale = (sx * angle.cos()).hypot(sy * angle.sin());
                    *angle = (sy * angle.sin()).atan2(sx * angle.cos());
                    *font_size *= direction_scale;
                    *bend *= (sx * sy).sqrt();
                }
                TransformKind::Crop(_, _) => {
                    *anchor = transform.point(*anchor);
                }
            }
        }
    }
}

fn measurement_points(axis: Axis, from: f32, to: f32, at: f32) -> (Point, Point) {
    match axis {
        Axis::Horizontal => (Point { x: from, y: at }, Point { x: to, y: at }),
        Axis::Vertical => (Point { x: at, y: from }, Point { x: at, y: to }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{ProtectedColor, Resampling};

    fn highlight() -> Annotation {
        Annotation {
            id: AnnotationId(1),
            shape: Shape::Highlight {
                rect: Rect {
                    x: 10.0,
                    y: 20.0,
                    width: 30.0,
                    height: 40.0,
                },
                seed: 7,
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: HIGHLIGHT_STROKE_WIDTH,
                },
            },
        }
    }

    fn pencil(id: u64, geometry: PencilGeometry) -> Annotation {
        Annotation {
            id: AnnotationId(id),
            shape: Shape::Pencil {
                geometry,
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 3.0,
                },
                anti_aliasing: true,
            },
        }
    }

    #[test]
    fn create_set_and_delete_are_folded() {
        let original = highlight();
        let mut changed = original.clone();
        let Shape::Highlight { rect, .. } = &mut changed.shape else {
            unreachable!()
        };
        rect.x = 12.0;
        let operations = [
            Operation::Annotate(AnnotationEdit::Create(original)),
            Operation::Annotate(AnnotationEdit::Set(changed.clone())),
        ];
        assert_eq!(fold_annotations((100, 100), &operations), vec![changed]);
        let mut deleted = operations.to_vec();
        deleted.push(Operation::Annotate(AnnotationEdit::Delete(AnnotationId(1))));
        assert!(fold_annotations((100, 100), &deleted).is_empty());
    }

    #[test]
    fn crop_translates_without_discarding_outside_objects() {
        let operations = [
            Operation::Annotate(AnnotationEdit::Create(highlight())),
            Operation::Crop {
                x: 20,
                y: 30,
                width: 10,
                height: 10,
            },
        ];
        let folded = fold_annotations((100, 100), &operations);
        let Shape::Highlight { rect, .. } = folded[0].shape else {
            unreachable!()
        };
        assert_eq!((rect.x, rect.y), (-10.0, -10.0));
    }

    #[test]
    fn geometry_operations_are_reversible() {
        let create = Operation::Annotate(AnnotationEdit::Create(highlight()));
        let twice = fold_annotations(
            (100, 80),
            &[
                create.clone(),
                Operation::FlipHorizontal,
                Operation::FlipHorizontal,
            ],
        );
        assert_eq!(twice, vec![highlight()]);

        let rotations = fold_annotations(
            (100, 80),
            &[
                create,
                Operation::Rotate(Rotation::Clockwise90),
                Operation::Rotate(Rotation::Clockwise90),
                Operation::Rotate(Rotation::Clockwise90),
                Operation::Rotate(Rotation::Clockwise90),
            ],
        );
        assert_eq!(rotations, vec![highlight()]);
    }

    #[test]
    fn palette_operations_do_not_change_annotations() {
        let annotation = highlight();
        let folded = fold_annotations(
            (100, 80),
            &[
                Operation::Annotate(AnnotationEdit::Create(annotation.clone())),
                Operation::Palette {
                    colors: 4,
                    dithering: false,
                    preserve_accents: true,
                    protected: vec![ProtectedColor([1, 2, 3, 4])],
                },
            ],
        );
        assert_eq!(folded, vec![annotation]);
    }

    #[test]
    fn scaling_keeps_the_legacy_highlight_width_field_canonical() {
        let folded = fold_annotations(
            (100, 100),
            &[
                Operation::Annotate(AnnotationEdit::Create(highlight())),
                Operation::Scale {
                    width: 400,
                    height: 100,
                    resampling: Resampling::Nearest,
                },
            ],
        );
        let Shape::Highlight { style, .. } = folded[0].shape else {
            unreachable!()
        };
        // Rendering derives the effective width from the resulting image size.
        assert_eq!(style.width, 1.0);
    }

    #[test]
    fn scaling_keeps_measurement_strokes_one_native_pixel_wide() {
        let measurement = Annotation {
            id: AnnotationId(2),
            shape: Shape::Measurement {
                axis: Axis::Horizontal,
                from: 10.0,
                to: 40.0,
                at: 20.0,
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 12.0,
                },
                label_size: 8.0,
            },
        };
        let folded = fold_annotations(
            (100, 100),
            &[
                Operation::Annotate(AnnotationEdit::Create(measurement)),
                Operation::Scale {
                    width: 400,
                    height: 200,
                    resampling: Resampling::Nearest,
                },
            ],
        );
        let Shape::Measurement { style, .. } = folded[0].shape else {
            unreachable!()
        };
        assert_eq!(style.width, MEASUREMENT_STROKE_WIDTH);
    }

    #[test]
    fn every_pencil_geometry_follows_image_scaling() {
        let annotations = [
            pencil(
                1,
                PencilGeometry::Freehand(vec![
                    BrushPoint {
                        x: 1.0,
                        y: 2.0,
                        pressure: 0.25,
                    },
                    BrushPoint {
                        x: 3.0,
                        y: 4.0,
                        pressure: 0.75,
                    },
                ]),
            ),
            pencil(
                2,
                PencilGeometry::Line(vec![Point { x: 2.0, y: 3.0 }, Point { x: 8.0, y: 9.0 }]),
            ),
            pencil(
                3,
                PencilGeometry::Rectangle(Rect {
                    x: 10.0,
                    y: 20.0,
                    width: 30.0,
                    height: 40.0,
                }),
            ),
            pencil(
                4,
                PencilGeometry::Ellipse(Rect {
                    x: 5.0,
                    y: 6.0,
                    width: 10.0,
                    height: 12.0,
                }),
            ),
        ];
        let mut operations = annotations
            .into_iter()
            .map(|annotation| Operation::Annotate(AnnotationEdit::Create(annotation)))
            .collect::<Vec<_>>();
        operations.push(Operation::Scale {
            width: 400,
            height: 160,
            resampling: Resampling::Nearest,
        });

        let folded = fold_annotations((100, 80), &operations);
        let expected_width = 3.0 * 8.0_f32.sqrt();
        let Shape::Pencil {
            geometry: PencilGeometry::Freehand(points),
            style,
            ..
        } = &folded[0].shape
        else {
            panic!("expected freehand pencil geometry");
        };
        assert_eq!((points[0].x, points[0].y), (4.0, 4.0));
        assert_eq!((points[1].x, points[1].y), (12.0, 8.0));
        assert_eq!((points[0].pressure, points[1].pressure), (0.25, 0.75));
        assert_eq!(style.width, expected_width);

        let Shape::Pencil {
            geometry: PencilGeometry::Line(points),
            style,
            ..
        } = &folded[1].shape
        else {
            panic!("expected pencil line geometry");
        };
        assert_eq!(
            points.as_slice(),
            [Point { x: 8.0, y: 6.0 }, Point { x: 32.0, y: 18.0 }]
        );
        assert_eq!(style.width, expected_width);

        let expected_rectangles = [
            Rect {
                x: 40.0,
                y: 40.0,
                width: 120.0,
                height: 80.0,
            },
            Rect {
                x: 20.0,
                y: 12.0,
                width: 40.0,
                height: 24.0,
            },
        ];
        for (annotation, expected) in folded[2..].iter().zip(expected_rectangles) {
            let Shape::Pencil {
                geometry, style, ..
            } = &annotation.shape
            else {
                panic!("expected pencil rectangle or ellipse geometry");
            };
            let actual = match geometry {
                PencilGeometry::Rectangle(rect) | PencilGeometry::Ellipse(rect) => *rect,
                _ => panic!("expected pencil rectangle or ellipse geometry"),
            };
            assert_eq!(actual, expected);
            assert_eq!(style.width, expected_width);
        }
    }
}
