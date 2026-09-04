use std::collections::{HashMap, HashSet};

use image::{Rgba, RgbaImage};

use crate::document::{BrushPoint, CancellationToken, Stroke, StrokePath};
use crate::error::{AppError, Result};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PencilShape {
    Line {
        start: BrushPoint,
        end: BrushPoint,
    },
    Rectangle {
        start: BrushPoint,
        end: BrushPoint,
    },
    Circle {
        center: BrushPoint,
        edge: BrushPoint,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimedBrushPoint {
    pub point: BrushPoint,
    pub timestamp_ms: u32,
}

pub fn shape_points(shape: PencilShape) -> Vec<BrushPoint> {
    match shape {
        PencilShape::Line { start, end } => vec![start, end],
        PencilShape::Rectangle { start, end } => vec![
            start,
            BrushPoint {
                x: end.x,
                y: start.y,
                pressure: start.pressure,
            },
            end,
            BrushPoint {
                x: start.x,
                y: end.y,
                pressure: start.pressure,
            },
            start,
        ],
        PencilShape::Circle { center, edge } => vec![center, edge],
    }
}

pub fn adaptive_smooth(
    points: &[TimedBrushPoint],
    base_smoothing: f32,
    speed_smoothing: f32,
    speed_sensitivity: f32,
    passes: usize,
) -> Vec<BrushPoint> {
    if points.len() < 3 {
        return points.iter().map(|sample| sample.point).collect();
    }

    let strengths = points
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let previous = points[index.saturating_sub(1)];
            let next = points[(index + 1).min(points.len() - 1)];
            let elapsed_ms = next.timestamp_ms.wrapping_sub(previous.timestamp_ms).max(1) as f32;
            let speed = distance(previous.point, next.point) / elapsed_ms;
            let speed_factor = 1.0 - (-speed / speed_sensitivity.max(0.001)).exp();

            base_smoothing + speed_smoothing * speed_factor
        })
        .collect::<Vec<_>>();

    let mut result = points.iter().map(|sample| sample.point).collect::<Vec<_>>();
    for _ in 0..passes {
        let mut next = result.clone();
        for index in 1..result.len() - 1 {
            let alpha = (strengths[index] * 0.22).min(0.48);
            let neighbor_midpoint = midpoint(result[index - 1], result[index + 1]);
            next[index].x = result[index].x * (1.0 - alpha) + neighbor_midpoint.x * alpha;
            next[index].y = result[index].y * (1.0 - alpha) + neighbor_midpoint.y * alpha;
        }
        result = next;
    }

    result
}

pub fn sample(image: &RgbaImage, x: u32, y: u32) -> Option<[u8; 4]> {
    (x < image.width() && y < image.height()).then(|| image.get_pixel(x, y).0)
}

pub fn paint_stroke(
    image: &RgbaImage,
    stroke: &Stroke,
    cancellation: &CancellationToken,
) -> Result<RgbaImage> {
    if !stroke.width.is_finite()
        || stroke.width < 1.0
        || !(0.01..=1.0).contains(&stroke.opacity)
        || !(0.0..=1.0).contains(&stroke.hardness)
    {
        return Err(AppError::InvalidDimensions);
    }
    let mut output = image.clone();
    if stroke.points.is_empty() {
        return Ok(output);
    }

    if stroke.width == 1.0 && stroke.hardness == 1.0 && !stroke.anti_aliasing {
        paint_pixel_stroke(&mut output, stroke, cancellation)?;
        return Ok(output);
    }

    let spacing = (stroke.width * 0.2).max(0.25);
    let points = match stroke.path {
        StrokePath::Smooth => smooth_path(&stroke.points, spacing),
        StrokePath::Linear => linear_path(&stroke.points, spacing),
        StrokePath::Circle if stroke.anti_aliasing => smooth_circle_path(&stroke.points, spacing),
        StrokePath::Circle => circle_path(&stroke.points),
    };
    if stroke.anti_aliasing {
        paint_antialiased_stroke(&mut output, &points, stroke, cancellation)?;
        return Ok(output);
    }
    for point in points {
        cancellation.check()?;
        stamp(&mut output, point.x, point.y, point.pressure, stroke);
    }
    Ok(output)
}

fn paint_pixel_stroke(
    image: &mut RgbaImage,
    stroke: &Stroke,
    cancellation: &CancellationToken,
) -> Result<()> {
    // A one-pixel pen must select logical pixels directly. Subpixel brush stamps can touch both
    // sides of a half-pixel boundary and turn a diagonal into a two-pixel-wide corner.
    let points = match stroke.path {
        StrokePath::Smooth => pixel_perfect_freehand(&stroke.points),
        StrokePath::Linear => bresenham_polyline(&stroke.points),
        StrokePath::Circle => bresenham_circle_path(&stroke.points),
    };
    let mut painted = HashSet::with_capacity(points.len());
    let source_alpha = f32::from(stroke.color[3]) / 255.0 * stroke.opacity;

    for (index, (x, y)) in points.into_iter().enumerate() {
        if index % 4096 == 0 {
            cancellation.check()?;
        }
        if !painted.insert((x, y)) || x < 0 || y < 0 {
            continue;
        }
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            continue;
        };
        if x >= image.width() || y >= image.height() {
            continue;
        }
        let destination = image.get_pixel_mut(x, y);
        *destination = blend(*destination, Rgba(stroke.color), source_alpha);
    }
    Ok(())
}

fn pixel_perfect_freehand(points: &[BrushPoint]) -> Vec<(i64, i64)> {
    let points = brush_pixels(points);
    let Some(&first) = points.first() else {
        return Vec::new();
    };
    let mut path = vec![first];
    for pair in points.windows(2) {
        for point in bresenham_line(pair[0], pair[1]).into_iter().skip(1) {
            append_pixel_perfect(&mut path, point);
        }
    }
    path
}

fn append_pixel_perfect(path: &mut Vec<(i64, i64)>, point: (i64, i64)) {
    if path.last() == Some(&point) {
        return;
    }
    path.push(point);
    while path.len() >= 3 {
        let end = path.len();
        let (start, corner, finish) = (path[end - 3], path[end - 2], path[end - 1]);
        if !is_redundant_corner(start, corner, finish) {
            break;
        }
        // Join the two diagonal pixels directly instead of retaining the orthogonal corner pixel.
        path.remove(end - 2);
    }
}

fn is_redundant_corner(start: (i64, i64), corner: (i64, i64), finish: (i64, i64)) -> bool {
    start.0.abs_diff(finish.0) == 1
        && start.1.abs_diff(finish.1) == 1
        && manhattan_distance(start, corner) == 1
        && manhattan_distance(corner, finish) == 1
}

fn manhattan_distance(left: (i64, i64), right: (i64, i64)) -> u64 {
    left.0.abs_diff(right.0) + left.1.abs_diff(right.1)
}

fn bresenham_polyline(points: &[BrushPoint]) -> Vec<(i64, i64)> {
    let points = brush_pixels(points);
    let Some(&first) = points.first() else {
        return Vec::new();
    };
    let mut path = vec![first];
    for pair in points.windows(2) {
        let segment = bresenham_line(pair[0], pair[1]);
        path.extend(segment.into_iter().skip(1));
    }
    path
}

fn brush_pixels(points: &[BrushPoint]) -> Vec<(i64, i64)> {
    points
        .iter()
        .map(|point| (point.x.floor() as i64, point.y.floor() as i64))
        .fold(Vec::new(), |mut pixels, pixel| {
            if pixels.last() != Some(&pixel) {
                pixels.push(pixel);
            }
            pixels
        })
}

fn bresenham_line(start: (i64, i64), end: (i64, i64)) -> Vec<(i64, i64)> {
    // All-octant error update from the extended Bresenham rasterizer.
    let (mut x, mut y) = start;
    let dx = end.0.abs_diff(x) as i64;
    let step_x = if x < end.0 { 1 } else { -1 };
    let dy = -(end.1.abs_diff(y) as i64);
    let step_y = if y < end.1 { 1 } else { -1 };
    let mut error = dx + dy;
    let mut points = Vec::with_capacity(usize::try_from(dx.max(-dy) + 1).unwrap_or(0));

    loop {
        points.push((x, y));
        if (x, y) == end {
            break;
        }
        let doubled_error = 2 * error;
        if doubled_error >= dy {
            error += dy;
            x += step_x;
        }
        if doubled_error <= dx {
            error += dx;
            y += step_y;
        }
    }
    points
}

fn bresenham_circle_path(points: &[BrushPoint]) -> Vec<(i64, i64)> {
    let Some(center) = points.first() else {
        return Vec::new();
    };
    let center = (center.x.floor() as i64, center.y.floor() as i64);
    let Some(edge) = points.get(1) else {
        return vec![center];
    };
    let edge = (edge.x.floor() as i64, edge.y.floor() as i64);
    let radius = ((edge.0 - center.0) as f64)
        .hypot((edge.1 - center.1) as f64)
        .round() as i64;
    if radius == 0 {
        return vec![center];
    }

    // The four-way symmetric integer circle is the circle extension of the same error algorithm.
    let (mut x, mut y) = (-radius, 0);
    let mut error = 2 - 2 * radius;
    let mut path = Vec::with_capacity(usize::try_from(radius.saturating_mul(8)).unwrap_or(0));
    while x < 0 {
        path.extend([
            (center.0 - x, center.1 + y),
            (center.0 - y, center.1 - x),
            (center.0 + x, center.1 - y),
            (center.0 + y, center.1 + x),
        ]);
        let previous_error = error;
        if previous_error <= y {
            y += 1;
            error += y * 2 + 1;
        }
        if previous_error > x || error > y {
            x += 1;
            error += x * 2 + 1;
        }
    }
    path
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

fn linear_path(points: &[BrushPoint], spacing: f32) -> Vec<BrushPoint> {
    let points = points.iter().copied().fold(Vec::new(), |mut path, point| {
        if path.last() != Some(&point) {
            path.push(point);
        }
        path
    });
    let Some(&first) = points.first() else {
        return Vec::new();
    };
    let mut path = vec![first];
    for pair in points.windows(2) {
        append_linear(&mut path, pair[0], pair[1], spacing);
    }
    path
}

fn circle_path(points: &[BrushPoint]) -> Vec<BrushPoint> {
    bresenham_circle_path(points)
        .into_iter()
        .map(|(x, y)| BrushPoint {
            x: x as f32 + 0.5,
            y: y as f32 + 0.5,
            pressure: points.first().map_or(1.0, |point| point.pressure),
        })
        .collect()
}

fn smooth_circle_path(points: &[BrushPoint], spacing: f32) -> Vec<BrushPoint> {
    let Some(&center) = points.first() else {
        return Vec::new();
    };
    let Some(&edge) = points.get(1) else {
        return vec![center];
    };
    let radius = distance(center, edge);
    if radius <= f32::EPSILON {
        return vec![center];
    }
    let steps = (std::f32::consts::TAU * radius / spacing.max(0.01))
        .ceil()
        .max(8.0) as usize;
    (0..steps)
        .map(|step| {
            let angle = std::f32::consts::TAU * step as f32 / steps as f32;
            BrushPoint {
                x: center.x + radius * angle.cos(),
                y: center.y + radius * angle.sin(),
                pressure: center.pressure,
            }
        })
        .collect()
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

fn subdued_edge_coverage(coverage: f32) -> f32 {
    // Keep the fully covered brush core intact while making the AA fringe less visually heavy.
    coverage * coverage * (2.0 - coverage)
}

fn paint_antialiased_stroke(
    image: &mut RgbaImage,
    points: &[BrushPoint],
    stroke: &Stroke,
    cancellation: &CancellationToken,
) -> Result<()> {
    let mut coverage = HashMap::new();
    for (index, point) in points.iter().enumerate() {
        if index % 4096 == 0 {
            cancellation.check()?;
        }
        accumulate_stamp_coverage(
            &mut coverage,
            image.dimensions(),
            point.x,
            point.y,
            point.pressure,
            stroke,
        );
    }

    let source_alpha = f32::from(stroke.color[3]) / 255.0 * stroke.opacity;
    for (index, ((x, y), coverage)) in coverage.into_iter().enumerate() {
        if index % 4096 == 0 {
            cancellation.check()?;
        }
        let destination = image.get_pixel_mut(x, y);
        *destination = blend(*destination, Rgba(stroke.color), source_alpha * coverage);
    }
    Ok(())
}

fn accumulate_stamp_coverage(
    coverage: &mut HashMap<(u32, u32), f32>,
    dimensions: (u32, u32),
    center_x: f32,
    center_y: f32,
    pressure: f32,
    stroke: &Stroke,
) {
    let (width, height) = dimensions;
    if width == 0 || height == 0 {
        return;
    }
    let radius = stroke.width * pressure.clamp(0.01, 1.0) / 2.0;
    let outer_radius = radius + 0.5;
    let min_x = (center_x - outer_radius).floor().max(0.0) as u32;
    let min_y = (center_y - outer_radius).floor().max(0.0) as u32;
    let max_x = (center_x + outer_radius)
        .ceil()
        .min(width.saturating_sub(1) as f32) as u32;
    let max_y = (center_y + outer_radius)
        .ceil()
        .min(height.saturating_sub(1) as f32) as u32;
    let hard_radius = (radius - 0.5).max(0.0) * stroke.hardness;

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let distance = (x as f32 + 0.5 - center_x).hypot(y as f32 + 0.5 - center_y);
            let linear_coverage = if distance <= hard_radius {
                1.0
            } else {
                ((outer_radius - distance) / (outer_radius - hard_radius)).clamp(0.0, 1.0)
            };
            let pixel_coverage = subdued_edge_coverage(linear_coverage);
            if pixel_coverage <= 0.0 {
                continue;
            }
            coverage
                .entry((x, y))
                .and_modify(|value| *value = value.max(pixel_coverage))
                .or_insert(pixel_coverage);
        }
    }
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

pub(crate) fn blend(destination: Rgba<u8>, source: Rgba<u8>, source_alpha: f32) -> Rgba<u8> {
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
    use std::collections::HashSet;

    use image::{Rgba, RgbaImage};

    use super::{
        PencilShape, TimedBrushPoint, adaptive_smooth, bresenham_circle_path, bresenham_line,
        paint_stroke, pixel_perfect_freehand, sample, shape_points, smooth_path,
    };
    use crate::document::{BrushPoint, CancellationToken, Stroke, StrokePath};

    fn painted_pixels(image: &RgbaImage) -> HashSet<(i64, i64)> {
        image
            .enumerate_pixels()
            .filter_map(|(x, y, pixel)| (pixel.0[3] > 0).then_some((i64::from(x), i64::from(y))))
            .collect()
    }

    #[test]
    fn samples_exact_rgba() {
        let image = RgbaImage::from_pixel(1, 1, Rgba([12, 34, 56, 78]));
        assert_eq!(sample(&image, 0, 0), Some([12, 34, 56, 78]));
    }

    #[test]
    fn adaptive_smoothing_increases_with_pointer_speed_and_preserves_endpoints() {
        let samples = |last_timestamp| {
            [
                TimedBrushPoint {
                    point: BrushPoint {
                        x: 0.0,
                        y: 0.0,
                        pressure: 0.25,
                    },
                    timestamp_ms: 0,
                },
                TimedBrushPoint {
                    point: BrushPoint {
                        x: 1.0,
                        y: 1.0,
                        pressure: 0.5,
                    },
                    timestamp_ms: last_timestamp / 2,
                },
                TimedBrushPoint {
                    point: BrushPoint {
                        x: 2.0,
                        y: 0.0,
                        pressure: 0.75,
                    },
                    timestamp_ms: last_timestamp,
                },
            ]
        };

        let slow = adaptive_smooth(&samples(20), 0.3, 1.5, 0.8, 1);
        let fast = adaptive_smooth(&samples(2), 0.3, 1.5, 0.8, 1);

        assert_eq!(slow[0], samples(20)[0].point);
        assert_eq!(slow[2], samples(20)[2].point);
        assert_eq!(slow[1].pressure, samples(20)[1].point.pressure);
        assert!(
            fast[1].y < slow[1].y,
            "the faster point should move farther toward its neighbors"
        );
    }

    #[test]
    fn adaptive_smoothing_handles_duplicate_timestamps() {
        let samples = [
            TimedBrushPoint {
                point: BrushPoint {
                    x: 0.0,
                    y: 0.0,
                    pressure: 1.0,
                },
                timestamp_ms: 5,
            },
            TimedBrushPoint {
                point: BrushPoint {
                    x: 1.0,
                    y: 1.0,
                    pressure: 1.0,
                },
                timestamp_ms: 5,
            },
            TimedBrushPoint {
                point: BrushPoint {
                    x: 2.0,
                    y: 0.0,
                    pressure: 1.0,
                },
                timestamp_ms: 5,
            },
        ];

        let smoothed = adaptive_smooth(&samples, 0.3, 1.5, 0.8, 12);

        assert!(
            smoothed
                .iter()
                .all(|point| point.x.is_finite() && point.y.is_finite())
        );
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
            path: StrokePath::Smooth,
            color: [255, 0, 0, 255],
            width: 3.0,
            anti_aliasing: false,
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
            path: StrokePath::Smooth,
            color: [255, 0, 0, 255],
            width: 3.0,
            anti_aliasing: false,
            opacity: 1.0,
            hardness: 1.0,
        };

        let output = paint_stroke(&image, &stroke, &CancellationToken::default()).unwrap();

        assert!(output.get_pixel(9, 3).0[3] > 0);
        assert_eq!(output.get_pixel(0, 0).0, [0, 0, 0, 0]);
    }

    #[test]
    fn shape_paths_keep_the_line_endpoints_and_close_rectangles() {
        let start = BrushPoint {
            x: 2.5,
            y: 3.5,
            pressure: 1.0,
        };
        let end = BrushPoint {
            x: 8.5,
            y: 9.5,
            pressure: 0.5,
        };

        assert_eq!(shape_points(PencilShape::Line { start, end }), [start, end]);
        assert_eq!(
            shape_points(PencilShape::Rectangle { start, end }),
            [
                start,
                BrushPoint {
                    x: end.x,
                    y: start.y,
                    pressure: start.pressure,
                },
                end,
                BrushPoint {
                    x: start.x,
                    y: end.y,
                    pressure: start.pressure,
                },
                start,
            ]
        );
    }

    #[test]
    fn circle_shape_keeps_the_center_and_edge_for_integer_rasterization() {
        let center = BrushPoint {
            x: 10.5,
            y: 12.5,
            pressure: 0.75,
        };
        let edge = BrushPoint {
            x: 15.5,
            y: 12.5,
            pressure: 1.0,
        };
        let path = shape_points(PencilShape::Circle { center, edge });

        assert_eq!(path, [center, edge]);
    }

    #[test]
    fn extended_bresenham_line_selects_one_exact_pixel_per_major_axis_step() {
        assert_eq!(bresenham_line((2, 2), (2, 2)), [(2, 2)]);
        assert_eq!(
            bresenham_line((1, 1), (6, 3)),
            [(1, 1), (2, 1), (3, 2), (4, 2), (5, 3), (6, 3)]
        );
        assert_eq!(
            bresenham_line((6, 3), (1, 1)),
            [(6, 3), (5, 3), (4, 2), (3, 2), (2, 1), (1, 1)]
        );
    }

    #[test]
    fn freehand_bresenham_removes_the_fat_pixel_at_a_diagonal_turn() {
        let points = [
            BrushPoint {
                x: 1.5,
                y: 1.5,
                pressure: 1.0,
            },
            BrushPoint {
                x: 2.5,
                y: 1.5,
                pressure: 1.0,
            },
            BrushPoint {
                x: 2.5,
                y: 2.5,
                pressure: 1.0,
            },
            BrushPoint {
                x: 3.5,
                y: 3.5,
                pressure: 1.0,
            },
        ];

        assert_eq!(pixel_perfect_freehand(&points), [(1, 1), (2, 2), (3, 3)]);
    }

    #[test]
    fn one_pixel_freehand_and_line_strokes_paint_the_exact_raster_paths() {
        let image = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 0]));
        let paint = |points, path| {
            paint_stroke(
                &image,
                &Stroke {
                    points,
                    path,
                    color: [255, 0, 0, 255],
                    width: 1.0,
                    anti_aliasing: false,
                    opacity: 1.0,
                    hardness: 1.0,
                },
                &CancellationToken::default(),
            )
            .map(|output| painted_pixels(&output))
            .unwrap()
        };

        let freehand = paint(
            vec![
                BrushPoint {
                    x: 1.5,
                    y: 1.5,
                    pressure: 1.0,
                },
                BrushPoint {
                    x: 2.5,
                    y: 1.5,
                    pressure: 1.0,
                },
                BrushPoint {
                    x: 2.5,
                    y: 2.5,
                    pressure: 1.0,
                },
            ],
            StrokePath::Smooth,
        );
        assert_eq!(freehand, HashSet::from([(1, 1), (2, 2)]));

        let line = paint(
            vec![
                BrushPoint {
                    x: 1.5,
                    y: 1.5,
                    pressure: 1.0,
                },
                BrushPoint {
                    x: 6.5,
                    y: 3.5,
                    pressure: 1.0,
                },
            ],
            StrokePath::Linear,
        );
        assert_eq!(
            line,
            HashSet::from([(1, 1), (2, 1), (3, 2), (4, 2), (5, 3), (6, 3)])
        );
    }

    #[test]
    fn extended_bresenham_circle_is_exact_and_symmetric() {
        let center = BrushPoint {
            x: 4.5,
            y: 4.5,
            pressure: 1.0,
        };
        let edge = BrushPoint {
            x: 6.5,
            y: 4.5,
            pressure: 1.0,
        };
        let actual = bresenham_circle_path(&[center, edge])
            .into_iter()
            .collect::<HashSet<_>>();
        let expected = HashSet::from([
            (6, 4),
            (4, 6),
            (2, 4),
            (4, 2),
            (6, 5),
            (3, 6),
            (2, 3),
            (5, 2),
            (5, 6),
            (2, 5),
            (3, 2),
            (6, 3),
        ]);

        assert!(bresenham_circle_path(&[]).is_empty());
        assert_eq!(bresenham_circle_path(&[center]), [(4, 4)]);
        assert_eq!(bresenham_circle_path(&[center, center]), [(4, 4)]);
        assert_eq!(actual, expected);
        for &(x, y) in &actual {
            assert!(actual.contains(&(8 - x, y)));
            assert!(actual.contains(&(x, 8 - y)));
        }
    }

    #[test]
    fn one_pixel_circle_paints_only_the_bresenham_ring() {
        let image = RgbaImage::from_pixel(9, 9, Rgba([0, 0, 0, 0]));
        let stroke = Stroke {
            points: vec![
                BrushPoint {
                    x: 4.5,
                    y: 4.5,
                    pressure: 1.0,
                },
                BrushPoint {
                    x: 6.5,
                    y: 4.5,
                    pressure: 1.0,
                },
            ],
            path: StrokePath::Circle,
            color: [255, 0, 0, 255],
            width: 1.0,
            anti_aliasing: false,
            opacity: 1.0,
            hardness: 1.0,
        };

        let output = paint_stroke(&image, &stroke, &CancellationToken::default()).unwrap();
        let painted = painted_pixels(&output);
        let expected = bresenham_circle_path(&stroke.points)
            .into_iter()
            .collect::<HashSet<_>>();

        assert_eq!(painted, expected);
        for x in 0..8 {
            for y in 0..8 {
                assert!(
                    ![(x, y), (x + 1, y), (x, y + 1), (x + 1, y + 1)]
                        .into_iter()
                        .all(|point| painted.contains(&point)),
                    "fat 2 × 2 block starts at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn antialiasing_adds_partial_coverage_to_freehand_and_circle_edges() {
        let image = RgbaImage::from_pixel(16, 16, Rgba([0, 0, 0, 0]));
        let paint = |points, path| {
            paint_stroke(
                &image,
                &Stroke {
                    points,
                    path,
                    color: [255, 0, 0, 255],
                    width: 1.0,
                    anti_aliasing: true,
                    opacity: 1.0,
                    hardness: 1.0,
                },
                &CancellationToken::default(),
            )
            .unwrap()
        };
        let freehand = paint(
            vec![
                BrushPoint {
                    x: 2.5,
                    y: 2.5,
                    pressure: 1.0,
                },
                BrushPoint {
                    x: 11.5,
                    y: 7.5,
                    pressure: 1.0,
                },
            ],
            StrokePath::Smooth,
        );
        let circle = paint(
            vec![
                BrushPoint {
                    x: 8.5,
                    y: 8.5,
                    pressure: 1.0,
                },
                BrushPoint {
                    x: 13.5,
                    y: 8.5,
                    pressure: 1.0,
                },
            ],
            StrokePath::Circle,
        );

        for output in [&freehand, &circle] {
            assert!(
                output
                    .pixels()
                    .any(|pixel| (1..u8::MAX).contains(&pixel.0[3])),
                "anti-aliased edge should include partially covered pixels"
            );
            assert!(
                output.pixels().any(|pixel| pixel.0[3] == u8::MAX),
                "stroke center should remain fully covered"
            );
        }
    }

    #[test]
    fn antialiasing_uses_a_subdued_fringe_without_weakening_aligned_core() {
        let image = RgbaImage::from_pixel(9, 9, Rgba([0, 0, 0, 0]));
        let paint = |x| {
            paint_stroke(
                &image,
                &Stroke {
                    points: vec![BrushPoint {
                        x,
                        y: 4.5,
                        pressure: 1.0,
                    }],
                    path: StrokePath::Smooth,
                    color: [255, 0, 0, 255],
                    width: 1.0,
                    anti_aliasing: true,
                    opacity: 1.0,
                    hardness: 1.0,
                },
                &CancellationToken::default(),
            )
            .unwrap()
        };

        let aligned = paint(4.5);
        let half_covered = paint(4.0);

        assert_eq!(aligned.get_pixel(4, 4).0[3], u8::MAX);
        assert_eq!(half_covered.get_pixel(3, 4).0[3], 96);
        assert_eq!(half_covered.get_pixel(4, 4).0[3], 96);
    }

    #[test]
    fn paint_width_controls_the_round_brush_diameter() {
        let image = RgbaImage::from_pixel(9, 9, Rgba([0, 0, 0, 0]));
        let paint = |width| {
            paint_stroke(
                &image,
                &Stroke {
                    points: vec![BrushPoint {
                        x: 4.5,
                        y: 4.5,
                        pressure: 1.0,
                    }],
                    path: StrokePath::Smooth,
                    color: [255, 0, 0, 255],
                    width,
                    anti_aliasing: false,
                    opacity: 1.0,
                    hardness: 1.0,
                },
                &CancellationToken::default(),
            )
            .map(|output| painted_pixels(&output))
            .unwrap()
        };

        assert_eq!(paint(1.0), HashSet::from([(4, 4)]));
        assert!(paint(3.0).len() > 1);
    }

    #[test]
    fn linear_rectangle_path_keeps_its_corners_and_closing_edge() {
        let start = BrushPoint {
            x: 2.5,
            y: 3.5,
            pressure: 1.0,
        };
        let end = BrushPoint {
            x: 10.5,
            y: 8.5,
            pressure: 1.0,
        };
        let stroke = Stroke {
            points: shape_points(PencilShape::Rectangle { start, end }),
            path: StrokePath::Linear,
            color: [255, 0, 0, 255],
            width: 1.0,
            anti_aliasing: false,
            opacity: 1.0,
            hardness: 1.0,
        };

        let output = paint_stroke(
            &RgbaImage::from_pixel(16, 16, Rgba([0, 0, 0, 0])),
            &stroke,
            &CancellationToken::default(),
        )
        .unwrap();

        for (x, y) in [(2, 3), (10, 3), (10, 8), (2, 8), (2, 6)] {
            assert!(
                output.get_pixel(x, y).0[3] > 0,
                "missing rectangle at {x},{y}"
            );
        }
    }
}
