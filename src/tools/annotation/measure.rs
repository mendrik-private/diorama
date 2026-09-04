use crate::document::{Annotation, AnnotationId, Axis, Shape};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GapMarker {
    pub first: AnnotationId,
    pub second: AnnotationId,
    pub axis: Axis,
    pub along: f32,
    pub from_at: f32,
    pub to_at: f32,
}

#[must_use]
pub fn gap_markers(annotations: &[Annotation]) -> Vec<GapMarker> {
    let mut lines = annotations
        .iter()
        .filter_map(|annotation| match annotation.shape {
            Shape::Measurement {
                axis, from, to, at, ..
            } => Some((annotation.id, axis, from, to, at)),
            _ => None,
        })
        .collect::<Vec<_>>();
    lines.sort_by(|left, right| {
        (left.1 as u8)
            .cmp(&(right.1 as u8))
            .then_with(|| left.4.total_cmp(&right.4))
    });

    let mut result = Vec::new();
    for (index, first) in lines.iter().enumerate() {
        for second in lines.iter().skip(index + 1) {
            if first.1 != second.1 {
                break;
            }
            let overlap_from = first.2.max(second.2);
            let overlap_to = first.3.min(second.3);
            if overlap_to <= overlap_from || (second.4 - first.4).abs() < 1.0 {
                continue;
            }
            let blocked = lines.iter().any(|candidate| {
                candidate.1 == first.1
                    && candidate.4 > first.4
                    && candidate.4 < second.4
                    && candidate.3.min(overlap_to) > candidate.2.max(overlap_from)
            });
            if !blocked {
                result.push(GapMarker {
                    first: first.0,
                    second: second.0,
                    axis: first.1,
                    along: (overlap_from + overlap_to) / 2.0,
                    from_at: first.4,
                    to_at: second.4,
                });
            }
        }
    }
    result
}

#[must_use]
pub fn length_label(from: f32, to: f32) -> String {
    format!("{}px", (to - from).abs().round() as u32)
}

#[cfg(test)]
mod tests {
    use crate::document::{Point, StrokeStyle};

    use super::*;

    fn line(id: u64, from: f32, to: f32, at: f32) -> Annotation {
        let _unused = Point::default();
        Annotation {
            id: AnnotationId(id),
            shape: Shape::Measurement {
                axis: Axis::Horizontal,
                from,
                to,
                at,
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 3.0,
                },
                label_size: 24.0,
            },
        }
    }

    #[test]
    fn adjacent_overlapping_lines_pair() {
        assert_eq!(
            gap_markers(&[line(1, 0.0, 100.0, 2.0), line(2, 20.0, 80.0, 20.0)]).len(),
            1
        );
        assert!(gap_markers(&[line(1, 0.0, 10.0, 2.0), line(2, 20.0, 30.0, 20.0)]).is_empty());
    }

    #[test]
    fn intersecting_middle_line_blocks_only_its_neighbors() {
        let markers = gap_markers(&[
            line(1, 0.0, 100.0, 0.0),
            line(2, 25.0, 75.0, 10.0),
            line(3, 0.0, 100.0, 20.0),
        ]);
        assert_eq!(markers.len(), 2);
        assert!(
            !markers
                .iter()
                .any(|marker| marker.first == AnnotationId(1) && marker.second == AnnotationId(3))
        );
    }

    #[test]
    fn a_disjoint_middle_line_does_not_block_outer_pair() {
        let markers = gap_markers(&[
            line(1, 0.0, 10.0, 0.0),
            line(2, 30.0, 40.0, 10.0),
            line(3, 0.0, 10.0, 20.0),
        ]);
        assert!(
            markers
                .iter()
                .any(|marker| marker.first == AnnotationId(1) && marker.second == AnnotationId(3))
        );
    }

    #[test]
    fn length_label_is_compact_lowercase_pixels() {
        assert_eq!(length_label(0.0, 23.0), "23px");
        assert_eq!(length_label(128.0, 0.0), "128px");
    }
}
