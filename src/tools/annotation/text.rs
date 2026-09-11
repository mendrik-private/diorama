use ttf_parser::GlyphId;

use crate::document::Point;

use super::font::{shape_text, text_advance};
use super::geometry::{flatten_quadratic, point_at_distance, polyline_length};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphPlacement {
    pub glyph: GlyphId,
    pub position: Point,
    pub tangent_angle: f32,
    pub font_size: f32,
    pub advance: f32,
    pub offset: Point,
}

/// A quadratic with control height at most half its chord turns at most 90°.
#[must_use]
pub fn clamped_bend(bend: f32, advance: f32) -> f32 {
    let limit = advance.max(0.0) / 2.0;
    bend.clamp(-limit, limit)
}

#[must_use]
pub fn baseline(anchor: Point, angle: f32, bend: f32, text: &str, font_size: f32) -> Vec<Point> {
    let advance = text_advance(text, font_size);
    baseline_with_advance(anchor, angle, bend, advance)
}

fn baseline_with_advance(anchor: Point, angle: f32, bend: f32, advance: f32) -> Vec<Point> {
    let bend = clamped_bend(bend, advance);
    let direction = Point {
        x: angle.cos(),
        y: angle.sin(),
    };
    let perpendicular = Point {
        x: -direction.y,
        y: direction.x,
    };
    let end = Point {
        x: anchor.x + advance * direction.x,
        y: anchor.y + advance * direction.y,
    };
    let control = Point {
        x: (anchor.x + end.x) / 2.0 + perpendicular.x * bend,
        y: (anchor.y + end.y) / 2.0 + perpendicular.y * bend,
    };
    flatten_quadratic(anchor, control, end, 64)
}

#[must_use]
pub fn glyph_placements(
    anchor: Point,
    angle: f32,
    bend: f32,
    text: &str,
    font_size: f32,
) -> Vec<GlyphPlacement> {
    let shaped = shape_text(text, font_size);
    let unbent_advance = shaped.iter().map(|glyph| glyph.advance).sum::<f32>();
    if unbent_advance <= f32::EPSILON {
        return Vec::new();
    }
    let curve = baseline_with_advance(anchor, angle, bend, unbent_advance);
    let curve_length = polyline_length(&curve);
    let scale = curve_length / unbent_advance;
    let mut cursor = 0.0;
    shaped
        .into_iter()
        .map(|glyph| {
            let advance = glyph.advance * scale;
            let center = cursor + advance / 2.0;
            cursor += advance;
            let (position, tangent) = point_at_distance(&curve, center);
            GlyphPlacement {
                glyph: glyph.glyph,
                position,
                tangent_angle: tangent.y.atan2(tangent.x),
                font_size: font_size * scale,
                advance,
                offset: Point {
                    x: glyph.x_offset * scale,
                    y: -glyph.y_offset * scale,
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_baseline_covers_the_advance_chord() {
        let anchor = Point { x: 10.0, y: 15.0 };
        let points = baseline(anchor, 0.0, 0.0, "Measure", 24.0);
        assert_eq!(points[0], anchor);
        assert!((points.last().unwrap().x - anchor.x - text_advance("Measure", 24.0)).abs() < 1e-3);
        assert!(points.iter().all(|point| (point.y - anchor.y).abs() < 1e-5));
    }

    #[test]
    fn bend_is_limited_in_both_directions_without_moving_endpoints() {
        for text in ["Hi", "A much longer label"] {
            let width = text_advance(text, 24.0);
            for sign in [-1.0, 1.0] {
                let curve = baseline(Point::default(), 0.0, sign * 10000.0, text, 24.0);
                let first = curve[1];
                let last = curve[curve.len() - 1];
                let previous = curve[curve.len() - 2];
                let turn = ((last.y - previous.y).atan2(last.x - previous.x)
                    - first.y.atan2(first.x))
                .abs();
                assert!(turn <= std::f32::consts::FRAC_PI_2);
                assert!(turn > 1.5);
                assert!((last.x - width).abs() < 1e-3);
                assert!(last.y.abs() < 1e-3);
            }
        }
    }

    #[test]
    fn bending_scales_letters_and_spacing_together() {
        let text = "AV A  VA";
        let straight = glyph_placements(Point::default(), 0.0, 0.0, text, 24.0);
        let bent = glyph_placements(Point::default(), 0.0, 10000.0, text, 24.0);
        let ratio = bent[0].font_size / straight[0].font_size;
        assert!(ratio > 1.0 && ratio < 1.15);
        for (straight, bent) in straight.iter().zip(&bent) {
            assert!((bent.advance - straight.advance * ratio).abs() < 1e-4);
            assert!((bent.font_size - straight.font_size * ratio).abs() < 1e-4);
        }
        assert!(glyph_placements(Point::default(), 0.0, 100.0, "", 24.0).is_empty());
        assert!(glyph_placements(Point::default(), 0.0, 100.0, text, 0.0).is_empty());
    }
}
