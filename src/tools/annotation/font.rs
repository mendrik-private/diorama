use std::sync::OnceLock;

use ttf_parser::{Face, GlyphId, OutlineBuilder};

static FACE: OnceLock<Face<'static>> = OnceLock::new();
static FONT_BYTES: &[u8] = include_bytes!("../../../data/fonts/Excalifont-Regular.ttf");
static SHAPING_FACE: OnceLock<rustybuzz::Face<'static>> = OnceLock::new();

pub struct ShapedGlyph {
    pub glyph: GlyphId,
    pub advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
}

/// Keep discretionary substitutions consistent with the inline editor.
pub const FONT_FEATURES: &str = "kern=1,liga=0,clig=0,calt=0";

#[must_use]
pub fn shape_text(text: &str, font_size: f32) -> Vec<ShapedGlyph> {
    let face = SHAPING_FACE.get_or_init(|| {
        rustybuzz::Face::from_slice(FONT_BYTES, 0).expect("embedded Excalifont is valid")
    });
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    let features: Vec<_> = FONT_FEATURES
        .split(',')
        .map(|feature| feature.parse().expect("valid font feature"))
        .collect();
    let shaped = rustybuzz::shape(face, &features, buffer);
    let scale = font_size / units_per_em();
    shaped
        .glyph_infos()
        .iter()
        .zip(shaped.glyph_positions())
        .map(|(info, position)| ShapedGlyph {
            glyph: GlyphId(info.glyph_id as u16),
            advance: position.x_advance as f32 * scale,
            x_offset: position.x_offset as f32 * scale,
            y_offset: position.y_offset as f32 * scale,
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutlineCommand {
    Move(f32, f32),
    Line(f32, f32),
    Quad(f32, f32, f32, f32),
    Curve(f32, f32, f32, f32, f32, f32),
    Close,
}

#[derive(Default)]
struct Commands(Vec<OutlineCommand>);

impl OutlineBuilder for Commands {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(OutlineCommand::Move(x, y));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(OutlineCommand::Line(x, y));
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.push(OutlineCommand::Quad(x1, y1, x, y));
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.push(OutlineCommand::Curve(x1, y1, x2, y2, x, y));
    }

    fn close(&mut self) {
        self.0.push(OutlineCommand::Close);
    }
}

#[must_use]
pub fn face() -> &'static Face<'static> {
    FACE.get_or_init(|| Face::parse(FONT_BYTES, 0).expect("embedded Excalifont is valid"))
}

#[cfg(test)]
#[must_use]
pub fn glyph_id(character: char) -> GlyphId {
    face().glyph_index(character).unwrap_or(GlyphId(0))
}

#[must_use]
pub fn units_per_em() -> f32 {
    f32::from(face().units_per_em())
}

#[cfg(test)]
#[must_use]
pub fn glyph_advance(glyph: GlyphId, font_size: f32) -> f32 {
    f32::from(face().glyph_hor_advance(glyph).unwrap_or(0)) * font_size / units_per_em()
}

#[must_use]
pub fn text_advance(text: &str, font_size: f32) -> f32 {
    shape_text(text, font_size)
        .iter()
        .map(|glyph| glyph.advance)
        .sum()
}

#[must_use]
pub fn glyph_outline(glyph: GlyphId) -> Vec<OutlineCommand> {
    let mut commands = Commands::default();
    let _ = face().outline_glyph(glyph, &mut commands);
    commands.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_font_parses_and_advance_is_monotone() {
        assert!(face().number_of_glyphs() > 100);
        for character in "0123456789px".chars() {
            let glyph = glyph_id(character);
            assert_ne!(glyph, GlyphId(0), "missing ASCII glyph {character:?}");
            assert!(
                !glyph_outline(glyph).is_empty(),
                "ASCII glyph {character:?} has no outline"
            );
        }
        assert!(text_advance("hello", 24.0) > text_advance("hell", 24.0));
        assert!(text_advance("x", 48.0) > text_advance("x", 24.0));
    }

    #[test]
    fn shaping_applies_excalifont_pair_kerning() {
        // Excalifont does not adjust every conventional kerning pair (e.g. AV).
        let has_adjusted_pair = ('A'..='z').any(|left| {
            ('A'..='z').any(|right| {
                let unkerned =
                    glyph_advance(glyph_id(left), 24.0) + glyph_advance(glyph_id(right), 24.0);
                (text_advance(&format!("{left}{right}"), 24.0) - unkerned).abs() > 0.01
            })
        });
        assert!(
            has_adjusted_pair,
            "font pair positioning must affect advances"
        );
        assert!(text_advance("A  V", 24.0) > text_advance("A V", 24.0));
    }
}
