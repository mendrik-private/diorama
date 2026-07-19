use image::{Rgba, RgbaImage};

use crate::document::{BrushPoint, CancellationToken, Stroke};
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

    let spacing = (stroke.width * 0.2).max(0.25);
    let points = smooth_path(&stroke.points, spacing);
    for point in points {
        cancellation.check()?;
        stamp(&mut output, point.x, point.y, point.pressure, stroke);
    }
    Ok(output)
}

fn smooth_path(points: &[BrushPoint], spacing: f32) -> Vec<BrushPoint> {
    let points = points.iter().copied().fold(Vec::new(), |mut path, point| {
        if path.last() != Some(&point) {
            path.push(point);
        }
        path
    });
    if points.len() <= 1 {
        return points;
    }

    let mut path = Vec::new();
    path.push(points[0]);
    if points.len() == 2 {
        append_linear(&mut path, points[0], points[1], spacing);
        return path;
    }
    append_quadratic(
        &mut path,
        points[0],
        points[0],
        midpoint(points[0], points[1]),
        spacing,
    );
    for index in 1..points.len() - 1 {
        append_quadratic(
            &mut path,
            midpoint(points[index - 1], points[index]),
            points[index],
            midpoint(points[index], points[index + 1]),
            spacing,
        );
    }
    let last = points[points.len() - 1];
    let previous = points[points.len() - 2];
    append_quadratic(&mut path, midpoint(previous, last), last, last, spacing);
    path
}

fn append_linear(path: &mut Vec<BrushPoint>, start: BrushPoint, end: BrushPoint, spacing: f32) {
    let steps = (distance(start, end) / spacing.max(0.01)).ceil().max(1.0) as u32;
    for step in 1..=steps {
        let t = step as f32 / steps as f32;
        path.push(BrushPoint {
            x: start.x + (end.x - start.x) * t,
            y: start.y + (end.y - start.y) * t,
            pressure: start.pressure + (end.pressure - start.pressure) * t,
        });
    }
}

fn append_quadratic(
    path: &mut Vec<BrushPoint>,
    start: BrushPoint,
    control: BrushPoint,
    end: BrushPoint,
    spacing: f32,
) {
    let maximum_speed = 2.0 * distance(start, control).max(distance(control, end));
    let steps = (maximum_speed / spacing.max(0.01)).ceil().max(1.0) as u32;
    for step in 1..=steps {
        let t = step as f32 / steps as f32;
        let one_minus_t = 1.0 - t;
        path.push(BrushPoint {
            x: one_minus_t * one_minus_t * start.x
                + 2.0 * one_minus_t * t * control.x
                + t * t * end.x,
            y: one_minus_t * one_minus_t * start.y
                + 2.0 * one_minus_t * t * control.y
                + t * t * end.y,
            pressure: one_minus_t * one_minus_t * start.pressure
                + 2.0 * one_minus_t * t * control.pressure
                + t * t * end.pressure,
        });
    }
}

fn midpoint(left: BrushPoint, right: BrushPoint) -> BrushPoint {
    BrushPoint {
        x: (left.x + right.x) * 0.5,
        y: (left.y + right.y) * 0.5,
        pressure: (left.pressure + right.pressure) * 0.5,
    }
}

fn distance(left: BrushPoint, right: BrushPoint) -> f32 {
    (right.x - left.x).hypot(right.y - left.y)
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

    use super::{paint_stroke, sample, smooth_path};
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

    #[test]
    fn spline_rounds_interior_corner_and_preserves_endpoints() {
        let points = [
            BrushPoint {
                x: 2.5,
                y: 2.5,
                pressure: 0.5,
            },
            BrushPoint {
                x: 10.5,
                y: 2.5,
                pressure: 0.75,
            },
            BrushPoint {
                x: 10.5,
                y: 10.5,
                pressure: 1.0,
            },
        ];

        let path = smooth_path(&points, 0.25);

        assert_eq!(path.first(), Some(&points[0]));
        assert_eq!(path.last(), Some(&points[2]));
        assert!(
            path.iter().all(|point| {
                (2.5..=10.5).contains(&point.x) && (2.5..=10.5).contains(&point.y)
            })
        );
        assert!(path.iter().any(|point| point.x < 10.5 && point.y > 2.5));
    }

    #[test]
    fn rounded_corner_paints_inside_the_linear_corner() {
        let image = RgbaImage::from_pixel(16, 16, Rgba([0, 0, 0, 0]));
        let stroke = Stroke {
            points: vec![
                BrushPoint {
                    x: 2.5,
                    y: 2.5,
                    pressure: 1.0,
                },
                BrushPoint {
                    x: 10.5,
                    y: 2.5,
                    pressure: 1.0,
                },
                BrushPoint {
                    x: 10.5,
                    y: 10.5,
                    pressure: 1.0,
                },
            ],
            color: [255, 0, 0, 255],
            width: 1.0,
            opacity: 1.0,
            hardness: 1.0,
        };

        let output = paint_stroke(&image, &stroke, &CancellationToken::default()).unwrap();

        assert!(output.get_pixel(9, 3).0[3] > 0);
        assert_eq!(output.get_pixel(0, 0).0, [0, 0, 0, 0]);
    }
}
