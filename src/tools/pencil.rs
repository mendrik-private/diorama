use image::{Rgba, RgbaImage};

use crate::document::{CancellationToken, Stroke};
use crate::error::{AppError, Result};

pub fn sample(image: &RgbaImage, x: u32, y: u32) -> Option<[u8; 4]> {
    (x < image.width() && y < image.height()).then(|| image.get_pixel(x, y).0)
}

pub fn paint_stroke(
    image: &RgbaImage,
    stroke: &Stroke,
    cancellation: &CancellationToken,
) -> Result<RgbaImage> {
    if !(1.0..=128.0).contains(&stroke.width)
        || !(0.01..=1.0).contains(&stroke.opacity)
        || !(0.0..=1.0).contains(&stroke.hardness)
    {
        return Err(AppError::InvalidDimensions);
    }
    let mut output = image.clone();
    if stroke.points.is_empty() {
        return Ok(output);
    }

    for segment in stroke.points.windows(2) {
        cancellation.check()?;
        let start = segment[0];
        let end = segment[1];
        let distance = (end.x - start.x).hypot(end.y - start.y);
        let steps = (distance / (stroke.width * 0.2).max(0.25)).ceil() as u32;
        for step in 0..=steps.max(1) {
            let t = step as f32 / steps.max(1) as f32;
            let x = start.x + (end.x - start.x) * t;
            let y = start.y + (end.y - start.y) * t;
            let pressure = start.pressure + (end.pressure - start.pressure) * t;
            stamp(&mut output, x, y, pressure, stroke);
        }
    }
    if stroke.points.len() == 1 {
        let point = stroke.points[0];
        stamp(&mut output, point.x, point.y, point.pressure, stroke);
    }
    Ok(output)
}

fn stamp(image: &mut RgbaImage, center_x: f32, center_y: f32, pressure: f32, stroke: &Stroke) {
    let radius = stroke.width * pressure.clamp(0.01, 1.0) / 2.0;
    let min_x = (center_x - radius).floor().max(0.0) as u32;
    let min_y = (center_y - radius).floor().max(0.0) as u32;
    let max_x = (center_x + radius).ceil().min(image.width() as f32 - 1.0) as u32;
    let max_y = (center_y + radius).ceil().min(image.height() as f32 - 1.0) as u32;
    let hard_radius = radius * stroke.hardness;

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let distance = (x as f32 + 0.5 - center_x).hypot(y as f32 + 0.5 - center_y);
            if distance > radius {
                continue;
            }
            let feather = if distance <= hard_radius || hard_radius >= radius {
                1.0
            } else {
                1.0 - (distance - hard_radius) / (radius - hard_radius)
            };
            let source_alpha = f32::from(stroke.color[3]) / 255.0 * stroke.opacity * feather;
            let destination = image.get_pixel_mut(x, y);
            *destination = blend(*destination, Rgba(stroke.color), source_alpha);
        }
    }
}

fn blend(destination: Rgba<u8>, source: Rgba<u8>, source_alpha: f32) -> Rgba<u8> {
    let destination_alpha = f32::from(destination.0[3]) / 255.0;
    let output_alpha = source_alpha + destination_alpha * (1.0 - source_alpha);
    if output_alpha <= f32::EPSILON {
        return Rgba([0, 0, 0, 0]);
    }
    let mut output = [0; 4];
    for (channel, value) in output.iter_mut().take(3).enumerate() {
        let source_value = f32::from(source.0[channel]) / 255.0;
        let destination_value = f32::from(destination.0[channel]) / 255.0;
        *value = (((source_value * source_alpha
            + destination_value * destination_alpha * (1.0 - source_alpha))
            / output_alpha)
            * 255.0)
            .round() as u8;
    }
    output[3] = (output_alpha * 255.0).round() as u8;
    Rgba(output)
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::{paint_stroke, sample};
    use crate::document::{BrushPoint, CancellationToken, Stroke};

    #[test]
    fn samples_exact_rgba() {
        let image = RgbaImage::from_pixel(1, 1, Rgba([12, 34, 56, 78]));
        assert_eq!(sample(&image, 0, 0), Some([12, 34, 56, 78]));
    }

    #[test]
    fn a_stroke_changes_only_nearby_pixels() {
        let image = RgbaImage::from_pixel(20, 20, Rgba([0, 0, 0, 0]));
        let stroke = Stroke {
            points: vec![BrushPoint {
                x: 10.0,
                y: 10.0,
                pressure: 1.0,
            }],
            color: [255, 0, 0, 255],
            width: 3.0,
            opacity: 1.0,
            hardness: 1.0,
        };
        let output = paint_stroke(&image, &stroke, &CancellationToken::default()).unwrap();
        assert!(output.get_pixel(10, 10).0[3] > 0);
        assert_eq!(output.get_pixel(0, 0).0, [0, 0, 0, 0]);
    }
}
