use palette::FromColor as _;

use crate::settings::ColorFormat;

pub(super) fn u8_to_rgba(color: [u8; 4]) -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::new(
        f32::from(color[0]) / 255.0,
        f32::from(color[1]) / 255.0,
        f32::from(color[2]) / 255.0,
        f32::from(color[3]) / 255.0,
    )
}

pub(super) fn rgba_to_u8(color: gtk::gdk::RGBA) -> [u8; 4] {
    [
        (color.red() * 255.0).round() as u8,
        (color.green() * 255.0).round() as u8,
        (color.blue() * 255.0).round() as u8,
        (color.alpha() * 255.0).round() as u8,
    ]
}

pub(super) fn color_format_index(format: ColorFormat) -> u32 {
    match format {
        ColorFormat::Hex => 0,
        ColorFormat::Rgb => 1,
        ColorFormat::Oklab => 2,
        ColorFormat::Hsl => 3,
    }
}

pub(super) fn color_format_at(index: u32) -> ColorFormat {
    match index {
        1 => ColorFormat::Rgb,
        2 => ColorFormat::Oklab,
        3 => ColorFormat::Hsl,
        _ => ColorFormat::Hex,
    }
}

fn format_decimal(value: f32, precision: usize) -> String {
    let threshold = 0.5 * 10.0_f32.powi(-(precision as i32));
    let value = if value.abs() < threshold { 0.0 } else { value };
    let formatted = format!("{value:.precision$}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

pub(super) fn format_color(color: [u8; 4], format: ColorFormat) -> String {
    let [red, green, blue, alpha] = color;
    match format {
        ColorFormat::Hex if alpha == u8::MAX => format!("#{red:02X}{green:02X}{blue:02X}"),
        ColorFormat::Hex => format!("#{red:02X}{green:02X}{blue:02X}{alpha:02X}"),
        ColorFormat::Rgb if alpha == u8::MAX => format!("rgb({red}, {green}, {blue})"),
        ColorFormat::Rgb => format!(
            "rgba({red}, {green}, {blue}, {})",
            format_decimal(f32::from(alpha) / 255.0, 3)
        ),
        ColorFormat::Oklab => {
            let srgb = palette::Srgb::new(
                f32::from(red) / 255.0,
                f32::from(green) / 255.0,
                f32::from(blue) / 255.0,
            );
            let oklab = palette::Oklab::from_color(srgb.into_linear());
            let components = format!(
                "{}% {} {}",
                format_decimal(oklab.l * 100.0, 2),
                format_decimal(oklab.a, 4),
                format_decimal(oklab.b, 4)
            );
            if alpha == u8::MAX {
                format!("oklab({components})")
            } else {
                format!(
                    "oklab({components} / {})",
                    format_decimal(f32::from(alpha) / 255.0, 3)
                )
            }
        }
        ColorFormat::Hsl => {
            let srgb = palette::Srgb::new(
                f32::from(red) / 255.0,
                f32::from(green) / 255.0,
                f32::from(blue) / 255.0,
            );
            let hsl = palette::Hsl::from_color(srgb);
            let components = format!(
                "{} {}% {}%",
                format_decimal(hsl.hue.into_positive_degrees(), 1),
                format_decimal(hsl.saturation * 100.0, 1),
                format_decimal(hsl.lightness * 100.0, 1)
            );
            if alpha == u8::MAX {
                format!("hsl({components})")
            } else {
                format!(
                    "hsl({components} / {})",
                    format_decimal(f32::from(alpha) / 255.0, 3)
                )
            }
        }
    }
}
