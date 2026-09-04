//! Tiny fixed-size bitmap glyphs for measurement labels.
//!
//! The cell is seven image pixels high. Digits occupy a 4x5 grid; lowercase
//! suffix glyphs use x-height and descender rows inside the same cell. This
//! leaves predictable vertical breathing room without scaling or antialiasing.

pub const CELL_HEIGHT: f32 = 7.0;
const GLYPH_WIDTH: u32 = 4;
const ADVANCE: u32 = 5;

pub fn text_width(text: &str) -> f32 {
    let glyphs = text.chars().count() as u32;
    if glyphs == 0 {
        0.0
    } else {
        (glyphs * ADVANCE - (ADVANCE - GLYPH_WIDTH)) as f32
    }
}

pub fn for_each_ink_pixel(text: &str, mut visit: impl FnMut(u32, u32)) {
    for (glyph_index, character) in text.chars().enumerate() {
        let x_offset = glyph_index as u32 * ADVANCE;
        for (row_index, row) in glyph_rows(character).into_iter().enumerate() {
            for column in 0..GLYPH_WIDTH {
                if row & (1 << (GLYPH_WIDTH - 1 - column)) != 0 {
                    visit(x_offset + column, row_index as u32);
                }
            }
        }
    }
}

fn glyph_rows(character: char) -> [u8; 7] {
    match character {
        '0' => [0, 0b0110, 0b1001, 0b1001, 0b1001, 0b0110, 0],
        '1' => [0, 0b0010, 0b0110, 0b0010, 0b0010, 0b0111, 0],
        '2' => [0, 0b0110, 0b1001, 0b0010, 0b0100, 0b1111, 0],
        '3' => [0, 0b1110, 0b0001, 0b0110, 0b0001, 0b1110, 0],
        '4' => [0, 0b0010, 0b0110, 0b1010, 0b1111, 0b0010, 0],
        '5' => [0, 0b1111, 0b1000, 0b1110, 0b0001, 0b1110, 0],
        '6' => [0, 0b0111, 0b1000, 0b1110, 0b1001, 0b0110, 0],
        '7' => [0, 0b1111, 0b0001, 0b0010, 0b0100, 0b0100, 0],
        '8' => [0, 0b0110, 0b1001, 0b0110, 0b1001, 0b0110, 0],
        '9' => [0, 0b0110, 0b1001, 0b0111, 0b0001, 0b1110, 0],
        'p' => [0, 0, 0b1110, 0b1001, 0b1110, 0b1000, 0b1000],
        'x' => [0, 0, 0b1001, 0b0110, 0b0110, 0b1001, 0],
        ' ' => [0; 7],
        _ => [0, 0b0110, 0b1001, 0b0010, 0, 0b0010, 0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurement_font_has_a_fixed_seven_pixel_cell() {
        assert_eq!(CELL_HEIGHT, 7.0);
        assert_eq!(text_width("1"), 4.0);
        assert_eq!(text_width("128px"), 24.0);

        let mut pixels = Vec::new();
        for_each_ink_pixel("128px", |x, y| pixels.push((x, y)));
        assert!(!pixels.is_empty());
        assert!(pixels.iter().all(|&(x, y)| x < 24 && y < 7));
    }

    #[test]
    fn suffix_glyphs_use_lowercase_height_and_a_p_descender() {
        let mut digit_rows = Vec::new();
        let mut p_rows = Vec::new();
        let mut x_rows = Vec::new();
        for_each_ink_pixel("2", |_, y| digit_rows.push(y));
        for_each_ink_pixel("p", |_, y| p_rows.push(y));
        for_each_ink_pixel("x", |_, y| x_rows.push(y));

        assert_eq!(digit_rows.iter().copied().min(), Some(1));
        assert_eq!(digit_rows.iter().copied().max(), Some(5));
        assert_eq!(p_rows.iter().copied().min(), Some(2));
        assert_eq!(p_rows.iter().copied().max(), Some(6));
        assert_eq!(x_rows.iter().copied().min(), Some(2));
        assert_eq!(x_rows.iter().copied().max(), Some(5));
    }
}
