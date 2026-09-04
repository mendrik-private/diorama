use ttf_parser::GlyphId;

use crate::document::Point;

use super::font::{glyph_advance, glyph_id, text_advance};
use super::geometry::{flatten_quadratic, point_at_distance, polyline_length};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphPlacement {
    pub glyph: GlyphId,
    pub position: Point,
    pub tangent_angle: f32,
    pub font_size: f32,
    pub advance: f32,
}

#[must_use]
pub fn baseline(anchor: Point, angle: f32, bend: f32, text: &str, font_size: f32) -> Vec<Point> {
    let advance = text_advance(text, font_size);
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
    let curve = baseline(anchor, angle, bend, text, font_size);
    let curve_length = polyline_length(&curve);
    let unbent_advance = text_advance(text, font_size).max(f32::EPSILON);
    let mut cursor = 0.0;
    text.chars()
        .map(|character| {
            let glyph = glyph_id(character);
            let advance = glyph_advance(glyph, font_size);
            let center = (cursor + advance / 2.0) / unbent_advance * curve_length;
            cursor += advance;
            let (position, tangent) = point_at_distance(&curve, center);
            GlyphPlacement {
                glyph,
                position,
                tangent_angle: tangent.y.atan2(tangent.x),
                font_size,
                advance,
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
}
