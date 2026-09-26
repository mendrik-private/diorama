use std::{
    collections::{BTreeMap, BTreeSet},
    f32::consts::FRAC_PI_2,
};

use super::{BrushPoint, Operation, Rotation};

pub const MEASUREMENT_STROKE_WIDTH: f32 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnnotationId(pub u64);

/// A stable address for one editable vertex in a line annotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LineVertex {
    pub annotation: AnnotationId,
    pub index: usize,
}

/// An explicit graph edge. Geometrically coincident vertices are deliberately
/// not linked unless a pen gesture records one of these edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LineLink {
    pub first: LineVertex,
    pub second: LineVertex,
}

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
    RotatedRectangle([Point; 4]),
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
        /// Counter-clockwise angle of the oval's local rectangle, in radians.
        angle: f32,
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
    CreateLinked {
        annotation: Annotation,
        links: Vec<LineLink>,
    },
    Set(Annotation),
    SetLinked {
        annotation: Annotation,
        links: Vec<LineLink>,
    },
    Delete(AnnotationId),
}

#[must_use]
pub fn fold_annotations(
    source_dimensions: (u32, u32),
    operations: &[Operation],
) -> Vec<Annotation> {
    fold_annotation_graph(source_dimensions, operations).annotations
}

#[must_use]
pub fn fold_line_links(source_dimensions: (u32, u32), operations: &[Operation]) -> Vec<LineLink> {
    let graph = fold_annotation_graph(source_dimensions, operations);
    line_links_from_groups(&graph.link_groups)
}

#[derive(Default)]
struct FoldedAnnotationGraph {
    annotations: Vec<Annotation>,
    /// A junction is retained as a set rather than only its input edges. This
    /// means deleting an intervening line cannot disconnect surviving members.
    link_groups: Vec<Vec<LineVertex>>,
}

fn fold_annotation_graph(
    source_dimensions: (u32, u32),
    operations: &[Operation],
) -> FoldedAnnotationGraph {
    let mut dimensions = (source_dimensions.0 as f32, source_dimensions.1 as f32);
    let mut graph = FoldedAnnotationGraph::default();

    for operation in operations {
        match operation {
            Operation::SelectionEdit {
                flattened_annotations,
                ..
            } => {
                graph
                    .annotations
                    .retain(|annotation| !flattened_annotations.contains(&annotation.id));
                normalize_link_groups(&graph.annotations, &mut graph.link_groups);
            }
            Operation::Annotate(AnnotationEdit::Create(annotation)) => {
                replace_annotation(&mut graph.annotations, annotation.clone());
                normalize_link_groups(&graph.annotations, &mut graph.link_groups);
            }
            Operation::Annotate(AnnotationEdit::CreateLinked { annotation, links }) => {
                replace_annotation(&mut graph.annotations, annotation.clone());
                graph
                    .link_groups
                    .extend(links.iter().map(|link| vec![link.first, link.second]));
                normalize_link_groups(&graph.annotations, &mut graph.link_groups);
                propagate_linked_vertices(
                    &mut graph.annotations,
                    &graph.link_groups,
                    annotation.id,
                );
            }
            Operation::Annotate(AnnotationEdit::Set(annotation)) => {
                if set_annotation(&mut graph.annotations, annotation.clone()) {
                    normalize_link_groups(&graph.annotations, &mut graph.link_groups);
                    propagate_linked_vertices(
                        &mut graph.annotations,
                        &graph.link_groups,
                        annotation.id,
                    );
                }
            }
            Operation::Annotate(AnnotationEdit::SetLinked { annotation, links }) => {
                if set_annotation(&mut graph.annotations, annotation.clone()) {
                    graph
                        .link_groups
                        .extend(links.iter().map(|link| vec![link.first, link.second]));
                    normalize_link_groups(&graph.annotations, &mut graph.link_groups);
                    propagate_linked_vertices(
                        &mut graph.annotations,
                        &graph.link_groups,
                        annotation.id,
                    );
                }
            }
            Operation::Annotate(AnnotationEdit::Delete(id)) => {
                graph.annotations.retain(|annotation| annotation.id != *id);
                normalize_link_groups(&graph.annotations, &mut graph.link_groups);
            }
            Operation::Crop {
                x,
                y,
                width,
                height,
            } => {
                transform_all(
                    &mut graph.annotations,
                    TransformKind::Crop(*x as f32, *y as f32),
                );
                dimensions = (*width as f32, *height as f32);
            }
            Operation::Rotate(rotation) => {
                let kind = match rotation {
                    Rotation::Clockwise90 => TransformKind::Clockwise90(dimensions),
                    Rotation::CounterClockwise90 => TransformKind::CounterClockwise90(dimensions),
                };
                transform_all(&mut graph.annotations, kind);
                dimensions = (dimensions.1, dimensions.0);
            }
            Operation::FlipHorizontal => {
                transform_all(
                    &mut graph.annotations,
                    TransformKind::FlipHorizontal(dimensions.0),
                );
            }
            Operation::FlipVertical => {
                transform_all(
                    &mut graph.annotations,
                    TransformKind::FlipVertical(dimensions.1),
                );
            }
            Operation::Scale { width, height, .. } => {
                let sx = *width as f32 / dimensions.0.max(f32::EPSILON);
                let sy = *height as f32 / dimensions.1.max(f32::EPSILON);
                transform_all(&mut graph.annotations, TransformKind::Scale(sx, sy));
                dimensions = (*width as f32, *height as f32);
            }
            Operation::ResizeCanvas { width, height, .. } => {
                let x = crate::tools::canvas_resize::center_offset(dimensions.0 as u32, *width);
                let y = crate::tools::canvas_resize::center_offset(dimensions.1 as u32, *height);
                transform_all(
                    &mut graph.annotations,
                    TransformKind::Crop(-x as f32, -y as f32),
                );
                dimensions = (*width as f32, *height as f32);
            }
            Operation::Palette { .. } => {}
        }
    }

    graph
}

fn replace_annotation(annotations: &mut Vec<Annotation>, annotation: Annotation) {
    if let Some(existing) = annotations
        .iter_mut()
        .find(|candidate| candidate.id == annotation.id)
    {
        *existing = annotation;
    } else {
        annotations.push(annotation);
    }
}

fn set_annotation(annotations: &mut [Annotation], annotation: Annotation) -> bool {
    let Some(existing) = annotations
        .iter_mut()
        .find(|candidate| candidate.id == annotation.id)
    else {
        return false;
    };
    *existing = annotation;
    true
}

fn line_vertex_point(annotations: &[Annotation], vertex: LineVertex) -> Option<Point> {
    annotations
        .iter()
        .find(|annotation| annotation.id == vertex.annotation)
        .and_then(|annotation| match &annotation.shape {
            Shape::Pencil {
                geometry: PencilGeometry::Line(points),
                ..
            } => points.get(vertex.index).copied(),
            _ => None,
        })
}

fn set_line_vertex(annotations: &mut [Annotation], vertex: LineVertex, point: Point) {
    let Some(annotation) = annotations
        .iter_mut()
        .find(|annotation| annotation.id == vertex.annotation)
    else {
        return;
    };
    let Shape::Pencil {
        geometry: PencilGeometry::Line(points),
        ..
    } = &mut annotation.shape
    else {
        return;
    };
    if let Some(candidate) = points.get_mut(vertex.index) {
        *candidate = point;
    }
}

fn welded_polyline_vertices(annotations: &[Annotation], vertex: LineVertex) -> Vec<LineVertex> {
    let Some(point) = line_vertex_point(annotations, vertex) else {
        return Vec::new();
    };
    let Some(annotation) = annotations
        .iter()
        .find(|annotation| annotation.id == vertex.annotation)
    else {
        return Vec::new();
    };
    let Shape::Pencil {
        geometry: PencilGeometry::Line(points),
        ..
    } = &annotation.shape
    else {
        return Vec::new();
    };
    points
        .iter()
        .enumerate()
        .filter(|(_, candidate)| **candidate == point)
        .map(|(index, _)| LineVertex {
            annotation: annotation.id,
            index,
        })
        .collect()
}

fn merge_vertex_groups(groups: &[Vec<LineVertex>]) -> Vec<Vec<LineVertex>> {
    let mut merged = Vec::<BTreeSet<LineVertex>>::new();
    for group in groups {
        let mut group = group.iter().copied().collect::<BTreeSet<_>>();
        if group.len() < 2 {
            continue;
        }
        let mut index = 0;
        while index < merged.len() {
            if !group.is_disjoint(&merged[index]) {
                group.append(&mut merged.swap_remove(index));
            } else {
                index += 1;
            }
        }
        merged.push(group);
    }
    merged
        .into_iter()
        .map(|group| group.into_iter().collect())
        .collect()
}

fn normalize_link_groups(annotations: &[Annotation], groups: &mut Vec<Vec<LineVertex>>) {
    for group in groups.iter_mut() {
        group.retain(|vertex| line_vertex_point(annotations, *vertex).is_some());
    }
    *groups = merge_vertex_groups(groups);
    loop {
        let Some((first, second)) = (0..groups.len()).find_map(|first| {
            (first + 1..groups.len()).find_map(|second| {
                groups_share_polyline_weld(annotations, &groups[first], &groups[second])
                    .then_some((first, second))
            })
        }) else {
            break;
        };
        let mut merged = std::mem::take(&mut groups[first]);
        merged.append(&mut groups[second]);
        groups.swap_remove(second);
        groups[first] = merged;
        *groups = merge_vertex_groups(groups);
    }
}

fn groups_share_polyline_weld(
    annotations: &[Annotation],
    first: &[LineVertex],
    second: &[LineVertex],
) -> bool {
    first.iter().any(|left| {
        second.iter().any(|right| {
            left.annotation == right.annotation
                && line_vertex_point(annotations, *left) == line_vertex_point(annotations, *right)
        })
    })
}

fn line_links_from_groups(groups: &[Vec<LineVertex>]) -> Vec<LineLink> {
    groups
        .iter()
        .flat_map(|group| {
            group.first().into_iter().flat_map(|first| {
                group[1..].iter().map(move |second| LineLink {
                    first: *first,
                    second: *second,
                })
            })
        })
        .collect()
}

fn propagate_linked_vertices(
    annotations: &mut [Annotation],
    link_groups: &[Vec<LineVertex>],
    edited_annotation: AnnotationId,
) {
    let mut updates = BTreeMap::new();
    for group in link_groups {
        let Some(point) = group.iter().find_map(|vertex| {
            (vertex.annotation == edited_annotation)
                .then(|| line_vertex_point(annotations, *vertex))
                .flatten()
        }) else {
            continue;
        };
        for vertex in group {
            for welded in welded_polyline_vertices(annotations, *vertex) {
                updates.insert(welded, point);
            }
        }
    }
    for (vertex, point) in updates {
        set_line_vertex(annotations, vertex, point);
    }
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
                PencilGeometry::RotatedRectangle(points) => {
                    for point in points {
                        *point = transform.point(*point);
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
        Shape::Highlight {
            rect, angle, style, ..
        } => {
            transform_highlight(rect, angle, transform);
            style.width *= transform.width_scale();
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

/// Applies an image-space affine transform to the oval's principal axes.
/// Keeping its eigen axes makes a rotated highlight remain an exact ellipse
/// through unequal scaling, flips, and quarter turns.
fn transform_highlight(rect: &mut Rect, angle: &mut f32, transform: TransformKind) {
    let center = transform.point(rect.center());
    match transform {
        TransformKind::Crop(_, _) => {
            rect.x = center.x - rect.width / 2.0;
            rect.y = center.y - rect.height / 2.0;
            return;
        }
        TransformKind::Clockwise90(_) => {
            rect.x = center.x - rect.width / 2.0;
            rect.y = center.y - rect.height / 2.0;
            *angle = (*angle + FRAC_PI_2).rem_euclid(std::f32::consts::TAU);
            return;
        }
        TransformKind::CounterClockwise90(_) => {
            rect.x = center.x - rect.width / 2.0;
            rect.y = center.y - rect.height / 2.0;
            *angle = (*angle - FRAC_PI_2).rem_euclid(std::f32::consts::TAU);
            return;
        }
        TransformKind::FlipHorizontal(_) => {
            rect.x = center.x - rect.width / 2.0;
            rect.y = center.y - rect.height / 2.0;
            *angle = std::f32::consts::PI - *angle;
            return;
        }
        TransformKind::FlipVertical(_) => {
            rect.x = center.x - rect.width / 2.0;
            rect.y = center.y - rect.height / 2.0;
            *angle = -*angle;
            return;
        }
        // Keep the local frame for a uniform scale. In particular, this avoids
        // swapping a portrait oval's axes during an identity operation, which
        // would alter its seeded irregular outline despite equivalent conics.
        TransformKind::Scale(sx, sy) if sx == sy => {
            let width = rect.width * sx;
            let height = rect.height * sy;
            rect.x = center.x - width / 2.0;
            rect.y = center.y - height / 2.0;
            rect.width = width;
            rect.height = height;
            return;
        }
        TransformKind::Scale(_, _) => {}
    }
    let (sin, cos) = angle.sin_cos();
    let rx = rect.width / 2.0;
    let ry = rect.height / 2.0;
    let map_vector = |vector: Point| match transform {
        TransformKind::Scale(sx, sy) => Point {
            x: vector.x * sx,
            y: vector.y * sy,
        },
        _ => unreachable!("only scaling reaches principal-axis normalization"),
    };
    let u = map_vector(Point {
        x: rx * cos,
        y: rx * sin,
    });
    let v = map_vector(Point {
        x: -ry * sin,
        y: ry * cos,
    });
    let xx = u.x.mul_add(u.x, v.x * v.x);
    let xy = u.x.mul_add(u.y, v.x * v.y);
    let yy = u.y.mul_add(u.y, v.y * v.y);
    let discriminant = ((xx - yy).mul_add(xx - yy, 4.0 * xy * xy)).sqrt();
    let major_squared = ((xx + yy + discriminant) / 2.0).max(0.0);
    let minor_squared = ((xx + yy - discriminant) / 2.0).max(0.0);
    let major_angle = if xy.abs() > f32::EPSILON || (xx - yy).abs() > f32::EPSILON {
        (2.0 * xy).atan2(xx - yy) / 2.0
    } else {
        *angle
    };
    *rect = Rect {
        x: center.x - major_squared.sqrt(),
        y: center.y - minor_squared.sqrt(),
        width: major_squared.sqrt() * 2.0,
        height: minor_squared.sqrt() * 2.0,
    };
    *angle = major_angle;
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
                angle: 0.0,
                seed: 7,
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 1.0,
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

    fn line(id: u64, points: &[(f32, f32)]) -> Annotation {
        pencil(
            id,
            PencilGeometry::Line(points.iter().map(|&(x, y)| Point { x, y }).collect()),
        )
    }

    fn line_points(annotation: &Annotation) -> &[Point] {
        let Shape::Pencil {
            geometry: PencilGeometry::Line(points),
            ..
        } = &annotation.shape
        else {
            panic!("expected line annotation");
        };
        points
    }

    fn vertex(annotation: u64, index: usize) -> LineVertex {
        LineVertex {
            annotation: AnnotationId(annotation),
            index,
        }
    }

    #[test]
    fn setting_a_linked_line_moves_the_entire_transitive_junction() {
        let first = line(1, &[(1.0, 1.0), (9.0, 1.0)]);
        let second = line(2, &[(1.0, 1.0), (1.0, 9.0)]);
        let third = line(3, &[(1.0, 1.0), (8.0, 8.0)]);
        let moved_first = line(1, &[(4.0, 5.0), (12.0, 5.0)]);
        let operations = [
            Operation::Annotate(AnnotationEdit::Create(first)),
            Operation::Annotate(AnnotationEdit::CreateLinked {
                annotation: second,
                links: vec![LineLink {
                    first: vertex(2, 0),
                    second: vertex(1, 0),
                }],
            }),
            Operation::Annotate(AnnotationEdit::CreateLinked {
                annotation: third,
                links: vec![LineLink {
                    first: vertex(3, 0),
                    second: vertex(2, 0),
                }],
            }),
            Operation::Annotate(AnnotationEdit::Set(moved_first)),
        ];

        let annotations = fold_annotations((32, 32), &operations);
        assert_eq!(line_points(&annotations[0])[0], Point { x: 4.0, y: 5.0 });
        assert_eq!(line_points(&annotations[1])[0], Point { x: 4.0, y: 5.0 });
        assert_eq!(line_points(&annotations[2])[0], Point { x: 4.0, y: 5.0 });
        assert_eq!(
            fold_line_links((32, 32), &operations),
            vec![
                LineLink {
                    first: vertex(1, 0),
                    second: vertex(2, 0),
                },
                LineLink {
                    first: vertex(1, 0),
                    second: vertex(3, 0),
                },
            ]
        );
    }

    #[test]
    fn deleting_an_intervening_line_keeps_a_junction_connected() {
        let operations = [
            Operation::Annotate(AnnotationEdit::Create(line(1, &[(1.0, 1.0)]))),
            Operation::Annotate(AnnotationEdit::CreateLinked {
                annotation: line(2, &[(1.0, 1.0)]),
                links: vec![LineLink {
                    first: vertex(2, 0),
                    second: vertex(1, 0),
                }],
            }),
            Operation::Annotate(AnnotationEdit::CreateLinked {
                annotation: line(3, &[(1.0, 1.0)]),
                links: vec![LineLink {
                    first: vertex(3, 0),
                    second: vertex(2, 0),
                }],
            }),
            Operation::Annotate(AnnotationEdit::Delete(AnnotationId(2))),
            Operation::Annotate(AnnotationEdit::CreateLinked {
                annotation: line(4, &[(9.0, 9.0)]),
                links: vec![LineLink {
                    first: vertex(4, 0),
                    second: vertex(1, 99),
                }],
            }),
        ];

        assert_eq!(
            fold_line_links((32, 32), &operations),
            vec![LineLink {
                first: vertex(1, 0),
                second: vertex(3, 0),
            }]
        );
    }

    #[test]
    fn deleting_a_closed_polyline_keeps_its_separately_linked_members_connected() {
        let operations = [
            Operation::Annotate(AnnotationEdit::Create(line(
                1,
                &[(1.0, 1.0), (6.0, 6.0), (1.0, 1.0)],
            ))),
            Operation::Annotate(AnnotationEdit::CreateLinked {
                annotation: line(2, &[(1.0, 1.0)]),
                links: vec![LineLink {
                    first: vertex(2, 0),
                    second: vertex(1, 0),
                }],
            }),
            Operation::Annotate(AnnotationEdit::CreateLinked {
                annotation: line(3, &[(1.0, 1.0)]),
                links: vec![LineLink {
                    first: vertex(3, 0),
                    second: vertex(1, 2),
                }],
            }),
            Operation::Annotate(AnnotationEdit::Delete(AnnotationId(1))),
        ];

        assert_eq!(
            fold_line_links((32, 32), &operations),
            vec![LineLink {
                first: vertex(2, 0),
                second: vertex(3, 0),
            }]
        );
    }

    #[test]
    fn propagation_uses_the_pre_edit_welds_for_each_junction() {
        let first = line(1, &[(0.0, 0.0), (1.0, 0.0)]);
        let second = line(2, &[(0.0, 0.0), (1.0, 0.0)]);
        let moved_first = line(1, &[(1.0, 0.0), (2.0, 0.0)]);
        let operations = [
            Operation::Annotate(AnnotationEdit::Create(first)),
            Operation::Annotate(AnnotationEdit::CreateLinked {
                annotation: second,
                links: vec![
                    LineLink {
                        first: vertex(2, 0),
                        second: vertex(1, 0),
                    },
                    LineLink {
                        first: vertex(2, 1),
                        second: vertex(1, 1),
                    },
                ],
            }),
            Operation::Annotate(AnnotationEdit::Set(moved_first)),
        ];

        let annotations = fold_annotations((32, 32), &operations);
        assert_eq!(
            line_points(&annotations[1]),
            [Point { x: 1.0, y: 0.0 }, Point { x: 2.0, y: 0.0 }]
        );
    }

    #[test]
    fn coincident_vertices_without_a_link_remain_independent() {
        let operations = [
            Operation::Annotate(AnnotationEdit::Create(line(1, &[(3.0, 4.0)]))),
            Operation::Annotate(AnnotationEdit::Create(line(2, &[(3.0, 4.0)]))),
            Operation::Annotate(AnnotationEdit::Set(line(1, &[(8.0, 9.0)]))),
        ];

        let annotations = fold_annotations((32, 32), &operations);
        assert_eq!(line_points(&annotations[0]), [Point { x: 8.0, y: 9.0 }]);
        assert_eq!(line_points(&annotations[1]), [Point { x: 3.0, y: 4.0 }]);
        assert!(fold_line_links((32, 32), &operations).is_empty());
    }

    #[test]
    fn linked_vertices_survive_raster_geometry_operations_together() {
        let operations = [
            Operation::Annotate(AnnotationEdit::Create(line(1, &[(10.0, 20.0)]))),
            Operation::Annotate(AnnotationEdit::CreateLinked {
                annotation: line(2, &[(10.0, 20.0)]),
                links: vec![LineLink {
                    first: vertex(2, 0),
                    second: vertex(1, 0),
                }],
            }),
            Operation::Crop {
                x: 5,
                y: 7,
                width: 90,
                height: 70,
            },
            Operation::Scale {
                width: 180,
                height: 210,
                resampling: Resampling::Nearest,
            },
            Operation::Rotate(Rotation::Clockwise90),
            Operation::FlipHorizontal,
            Operation::FlipVertical,
        ];

        let annotations = fold_annotations((100, 80), &operations);
        assert_eq!(line_points(&annotations[0]), [Point { x: 39.0, y: 170.0 }]);
        assert_eq!(line_points(&annotations[1]), [Point { x: 39.0, y: 170.0 }]);
        assert_eq!(
            fold_line_links((100, 80), &operations),
            vec![LineLink {
                first: vertex(1, 0),
                second: vertex(2, 0),
            }]
        );
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
    fn canvas_resize_translates_annotations_without_scaling() {
        let mut original = highlight();
        let Shape::Highlight { style, .. } = &mut original.shape else {
            unreachable!()
        };
        style.width = 3.0;
        let operations = [
            Operation::Annotate(AnnotationEdit::Create(original.clone())),
            Operation::ResizeCanvas {
                width: 105,
                height: 110,
                background: [0; 4],
            },
        ];
        let mut expected = original;
        if let Shape::Highlight { rect, .. } = &mut expected.shape {
            rect.x += 2.0;
            rect.y += 5.0;
        }
        assert_eq!(fold_annotations((100, 100), &operations), vec![expected]);
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
    fn transformed_rotated_highlight_keeps_its_center_and_ellipse_axes() {
        let mut annotation = highlight();
        let Shape::Highlight { rect, angle, .. } = &mut annotation.shape else {
            unreachable!()
        };
        *rect = Rect {
            x: 20.0,
            y: 30.0,
            width: 80.0,
            height: 30.0,
        };
        *angle = 30.0_f32.to_radians();
        transform_annotation(&mut annotation, TransformKind::Scale(2.0, 0.5));
        let Shape::Highlight { rect, angle, .. } = annotation.shape else {
            unreachable!()
        };
        assert!(rect.center().distance(Point { x: 120.0, y: 22.5 }) < 0.001);
        // The scaled major axis has the expected covariance: it is no longer
        // merely the original angle with independently scaled bounding sides.
        let (sin, cos) = angle.sin_cos();
        let major = Point {
            x: rect.width * cos / 2.0,
            y: rect.width * sin / 2.0,
        };
        let minor = Point {
            x: -rect.height * sin / 2.0,
            y: rect.height * cos / 2.0,
        };
        let xx = major.x.mul_add(major.x, minor.x * minor.x);
        let xy = major.x.mul_add(major.y, minor.x * minor.y);
        let yy = major.y.mul_add(major.y, minor.y * minor.y);
        assert!((xx - 5_025.0).abs() < 0.02);
        assert!((xy - 595.392_3).abs() < 0.02);
        assert!((yy - 142.1875).abs() < 0.02);
    }

    #[test]
    fn uniform_scale_preserves_a_portrait_highlight_frame_exactly() {
        let mut original = highlight();
        let Shape::Highlight { rect, angle, .. } = &mut original.shape else {
            unreachable!()
        };
        *rect = Rect {
            x: 20.0,
            y: 30.0,
            width: 30.0,
            height: 80.0,
        };
        *angle = 17.0_f32.to_radians();

        let mut identity = original.clone();
        transform_annotation(&mut identity, TransformKind::Scale(1.0, 1.0));
        assert_eq!(identity, original);

        transform_annotation(&mut identity, TransformKind::Scale(2.5, 2.5));
        let Shape::Highlight { rect, angle, .. } = identity.shape else {
            unreachable!()
        };
        assert_eq!(
            rect,
            Rect {
                x: 50.0,
                y: 75.0,
                width: 75.0,
                height: 200.0,
            }
        );
        assert_eq!(angle, 17.0_f32.to_radians());
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
    fn scaling_scales_highlight_pen_setting() {
        let mut annotation = highlight();
        let Shape::Highlight { style, .. } = &mut annotation.shape else {
            unreachable!()
        };
        style.width = 3.0;
        let folded = fold_annotations(
            (100, 100),
            &[
                Operation::Annotate(AnnotationEdit::Create(annotation)),
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
        assert_eq!(style.width, 6.0);
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
