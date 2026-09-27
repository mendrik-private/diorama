use image::{Rgba, RgbaImage};
use tiny_skia::{FillRule, LineCap, LineJoin, Mask, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::document::{
    Annotation, AnnotationId, Axis, CancellationToken, MEASUREMENT_STROKE_WIDTH, Point, Rect,
    Resampling, Shape,
};
use crate::error::{AppError, Result};
use crate::tools::pencil::{blend, paint_stroke};

use super::arrow::{arrow_head, curve_points};
use super::font::{OutlineCommand, glyph_outline, units_per_em};
use super::highlight::{CrayonGeometry, draw_crayon, highlight_stroke_width};
use super::measure::{gap_markers, length_label};
use super::pencil::{geometry_bounds, stroke_for};
use super::pixel_font::{CELL_HEIGHT as PIXEL_LABEL_HEIGHT, for_each_ink_pixel, text_width};
use super::text::glyph_placements;

#[derive(Clone, Copy)]
struct DrawStyle {
    color: [u8; 4],
    transform: Transform,
    anti_aliasing: bool,
}

struct GapLabelLayout {
    text: String,
    origin: Point,
    bounds: Bounds,
}

#[derive(Debug)]
pub struct RenderedOverlay {
    pub pixels: RgbaImage,
    pub bounds: Rect,
}

pub fn composite_annotations(
    image: &mut RgbaImage,
    annotations: &[Annotation],
    cancellation: &CancellationToken,
) -> Result<()> {
    if annotations.is_empty() {
        return Ok(());
    }
    if let Some(overlay) = render_bounded_overlay(image.dimensions(), annotations, cancellation)? {
        composite_overlay(image, overlay, cancellation)?;
    }
    Ok(())
}

pub fn composite_annotation_shapes(
    image: &mut RgbaImage,
    annotations: &[Annotation],
    cancellation: &CancellationToken,
) -> Result<()> {
    if let Some(overlay) = render_subset(image.dimensions(), annotations, &[], None, cancellation)?
    {
        composite_overlay(image, overlay, cancellation)?;
    }
    Ok(())
}

fn composite_overlay(
    image: &mut RgbaImage,
    mut overlay: RenderedOverlay,
    cancellation: &CancellationToken,
) -> Result<()> {
    let origin_x = overlay.bounds.x as u32;
    let origin_y = overlay.bounds.y as u32;
    for (index, (x, y, source)) in overlay.pixels.enumerate_pixels_mut().enumerate() {
        if index % 16_384 == 0 {
            cancellation.check()?;
        }
        let alpha = f32::from(source.0[3]) / 255.0;
        if alpha > 0.0 {
            let destination = image.get_pixel_mut(origin_x + x, origin_y + y);
            *destination = blend(*destination, *source, alpha);
        }
    }
    Ok(())
}

pub fn render_bounded_overlay(
    dimensions: (u32, u32),
    annotations: &[Annotation],
    cancellation: &CancellationToken,
) -> Result<Option<RenderedOverlay>> {
    render_subset(dimensions, annotations, annotations, None, cancellation)
}

pub fn render_annotation_preview(
    dimensions: (u32, u32),
    annotation: &Annotation,
    annotations: &[Annotation],
    cancellation: &CancellationToken,
) -> Result<Option<RenderedOverlay>> {
    let mut context = annotations
        .iter()
        .filter(|candidate| candidate.id != annotation.id)
        .cloned()
        .collect::<Vec<_>>();
    context.push(annotation.clone());
    render_subset(
        dimensions,
        std::slice::from_ref(annotation),
        &context,
        Some(annotation.id),
        cancellation,
    )
}

#[cfg(test)]
fn render_overlay(
    dimensions: (u32, u32),
    annotations: &[Annotation],
    cancellation: &CancellationToken,
) -> Result<RgbaImage> {
    let mut image = RgbaImage::new(dimensions.0, dimensions.1);
    if let Some(overlay) = render_bounded_overlay(dimensions, annotations, cancellation)? {
        let origin_x = overlay.bounds.x as u32;
        let origin_y = overlay.bounds.y as u32;
        for (x, y, pixel) in overlay.pixels.enumerate_pixels() {
            image.put_pixel(origin_x + x, origin_y + y, *pixel);
        }
    }
    Ok(image)
}

fn render_subset(
    dimensions: (u32, u32),
    annotations: &[Annotation],
    marker_context: &[Annotation],
    marker_id: Option<AnnotationId>,
    cancellation: &CancellationToken,
) -> Result<Option<RenderedOverlay>> {
    let markers = gap_markers(marker_context)
        .into_iter()
        .filter(|marker| marker_id.is_none_or(|id| marker.first == id || marker.second == id))
        .collect::<Vec<_>>();
    let Some(bounds) = rendered_bounds(dimensions, annotations, marker_context, &markers) else {
        return Ok(None);
    };
    let mut pixmap = Pixmap::new(bounds.width as u32, bounds.height as u32)
        .ok_or(AppError::InvalidDimensions)?;
    let transform = Transform::from_translate(-bounds.x, -bounds.y);
    for annotation in annotations {
        cancellation.check()?;
        draw_annotation(
            &mut pixmap,
            annotation,
            dimensions,
            bounds,
            transform,
            cancellation,
        )?;
    }
    draw_gap_markers(
        &mut pixmap,
        marker_context,
        &markers,
        dimensions,
        transform,
        cancellation,
    )?;
    let pixmap_dimensions = (pixmap.width(), pixmap.height());
    Ok(Some(RenderedOverlay {
        pixels: unpremultiplied_image(pixmap, pixmap_dimensions),
        bounds,
    }))
}

fn draw_annotation(
    pixmap: &mut Pixmap,
    annotation: &Annotation,
    dimensions: (u32, u32),
    bounds: Rect,
    transform: Transform,
    cancellation: &CancellationToken,
) -> Result<()> {
    match &annotation.shape {
        Shape::Image {
            pixels,
            corners,
            resampling,
        } => draw_image(pixmap, pixels, *corners, *resampling, bounds, cancellation)?,
        Shape::Pencil {
            geometry,
            style,
            anti_aliasing,
        } => {
            let mut stroke = stroke_for(geometry, *style, *anti_aliasing);
            for point in &mut stroke.points {
                point.x -= bounds.x;
                point.y -= bounds.y;
            }
            let current = unpremultiplied_data(pixmap.data(), (pixmap.width(), pixmap.height()));
            let painted = paint_stroke(&current, &stroke, cancellation)?;
            for (destination, source) in pixmap.data_mut().chunks_exact_mut(4).zip(painted.pixels())
            {
                let alpha = u16::from(source[3]);
                destination[0] = ((u16::from(source[0]) * alpha + 127) / 255) as u8;
                destination[1] = ((u16::from(source[1]) * alpha + 127) / 255) as u8;
                destination[2] = ((u16::from(source[2]) * alpha + 127) / 255) as u8;
                destination[3] = source[3];
            }
        }
        Shape::Highlight {
            rect,
            angle,
            seed,
            style,
        } => {
            draw_crayon(
                pixmap,
                CrayonGeometry {
                    rect: *rect,
                    angle: *angle,
                    seed: *seed,
                },
                highlight_stroke_width(style.width),
                style.color,
                transform,
                cancellation,
            )?;
        }
        Shape::Arrow {
            start,
            end,
            control,
            style,
        } => {
            let mut curve = curve_points(*start, *control, *end);
            let head = arrow_head(*start, *control, *end, style.width);
            let head_length = end.distance(head[1].midpoint(head[2]));
            while curve.len() > 2
                && curve
                    .last()
                    .is_some_and(|point| point.distance(*end) < head_length * 0.8)
            {
                curve.pop();
            }
            stroke_polyline(pixmap, &curve, style.color, style.width, transform, true);
            fill_polygon(pixmap, &head, style.color, transform);
        }
        Shape::Measurement {
            axis,
            from,
            to,
            at,
            style,
            label_size: _,
        } => {
            let width = MEASUREMENT_STROKE_WIDTH;
            let (start, end, tick) = match axis {
                Axis::Horizontal => (
                    Point { x: *from, y: *at },
                    Point { x: *to, y: *at },
                    Point {
                        x: 0.0,
                        y: width * 3.0,
                    },
                ),
                Axis::Vertical => (
                    Point { x: *at, y: *from },
                    Point { x: *at, y: *to },
                    Point {
                        x: width * 3.0,
                        y: 0.0,
                    },
                ),
            };
            stroke_polyline(pixmap, &[start, end], style.color, width, transform, false);
            for point in [start, end] {
                stroke_polyline(
                    pixmap,
                    &[
                        Point {
                            x: point.x - tick.x,
                            y: point.y - tick.y,
                        },
                        Point {
                            x: point.x + tick.x,
                            y: point.y + tick.y,
                        },
                    ],
                    style.color,
                    width,
                    transform,
                    false,
                );
            }
            let label = length_label(*from, *to);
            let layout = measurement_label_layout(dimensions, *axis, *from, *to, *at, label);
            draw_pixel_text(pixmap, layout.origin, &layout.text, style.color, transform);
        }
        Shape::Text {
            anchor,
            angle,
            font_size,
            bend,
            text,
            color,
        } => draw_text(
            pixmap,
            *anchor,
            *angle,
            *bend,
            text,
            *font_size,
            DrawStyle {
                color: *color,
                transform,
                anti_aliasing: true,
            },
        ),
    }
    Ok(())
}

fn draw_image(
    pixmap: &mut Pixmap,
    pixels: &RgbaImage,
    corners: [Point; 4],
    resampling: Resampling,
    bounds: Rect,
    cancellation: &CancellationToken,
) -> Result<()> {
    if pixels.width() == 0 || pixels.height() == 0 {
        return Ok(());
    }
    let u = Point {
        x: corners[1].x - corners[0].x,
        y: corners[1].y - corners[0].y,
    };
    let v = Point {
        x: corners[3].x - corners[0].x,
        y: corners[3].y - corners[0].y,
    };
    let determinant = u.x * v.y - u.y * v.x;
    if determinant.abs() <= f32::EPSILON {
        return Ok(());
    }
    let pixmap_width = pixmap.width() as usize;
    let pixmap_height = pixmap.height() as usize;
    let min_x = corners
        .iter()
        .map(|point| point.x)
        .fold(f32::INFINITY, f32::min);
    let max_x = corners
        .iter()
        .map(|point| point.x)
        .fold(f32::NEG_INFINITY, f32::max);
    let min_y = corners
        .iter()
        .map(|point| point.y)
        .fold(f32::INFINITY, f32::min);
    let max_y = corners
        .iter()
        .map(|point| point.y)
        .fold(f32::NEG_INFINITY, f32::max);
    let start_x = (min_x.floor() - bounds.x).max(0.0) as usize;
    let end_x = (max_x.ceil() - bounds.x).min(pixmap_width as f32).max(0.0) as usize;
    let start_y = (min_y.floor() - bounds.y).max(0.0) as usize;
    let end_y = (max_y.ceil() - bounds.y).min(pixmap_height as f32).max(0.0) as usize;
    // Do not prefilter an image whose frame is outside this bounded overlay.
    if start_x >= end_x || start_y >= end_y {
        return Ok(());
    }
    // Filtering occurs straight from the immutable source into the bounded
    // overlay. No intermediate image is allocated for a giant or mostly
    // off-canvas frame.
    // First prefilter only the axes that are shrinking. This delegates the
    // requested Bicubic/Lanczos filter to the established scaler and prevents
    // an anisotropic reduction from turning the affine sampler into an
    // unbounded two-dimensional kernel. The temporary is never larger than
    // the immutable source, even when most of the frame is off canvas.
    let target_dimensions = (
        pixels.width().min(u.x.hypot(u.y).ceil().max(1.0) as u32),
        pixels.height().min(v.x.hypot(v.y).ceil().max(1.0) as u32),
    );
    let scaled = (target_dimensions != pixels.dimensions())
        .then(|| {
            crate::tools::scale::resize(
                pixels,
                target_dimensions.0,
                target_dimensions.1,
                resampling,
                cancellation,
            )
        })
        .transpose()?;
    let source = scaled.as_ref().unwrap_or(pixels);
    let mask_width = end_x - start_x;
    let mask_height = end_y - start_y;
    let mut mask =
        Mask::new(mask_width as u32, mask_height as u32).ok_or(AppError::InvalidDimensions)?;
    let mask_origin = Point {
        x: bounds.x + start_x as f32,
        y: bounds.y + start_y as f32,
    };
    let mut builder = PathBuilder::new();
    builder.move_to(corners[0].x - mask_origin.x, corners[0].y - mask_origin.y);
    for point in &corners[1..] {
        builder.line_to(point.x - mask_origin.x, point.y - mask_origin.y);
    }
    builder.close();
    let Some(path) = builder.finish() else {
        return Ok(());
    };
    mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
    for y in start_y..end_y {
        cancellation.check()?;
        for x in start_x..end_x {
            let index = y * pixmap_width + x;
            let coverage = mask.data()[(y - start_y) * mask_width + x - start_x];
            if coverage == 0 {
                continue;
            }
            let destination = &mut pixmap.data_mut()[index * 4..index * 4 + 4];
            let point = Point {
                x: bounds.x + x as f32 + 0.5,
                y: bounds.y + y as f32 + 0.5,
            };
            let dx = point.x - corners[0].x;
            let dy = point.y - corners[0].y;
            let local_x = (dx * v.y - dy * v.x) / determinant;
            let local_y = (u.x * dy - u.y * dx) / determinant;
            // The antialiased mask can cover a pixel whose center is just
            // outside the frame. Clamp that sample to the source edge so the
            // coverage fades the real edge color instead of a transparent rim.
            if coverage == 255 && (!(0.0..1.0).contains(&local_x) || !(0.0..1.0).contains(&local_y))
            {
                continue;
            }
            let sample = sample_image(
                source,
                local_x.clamp(0.0, 1.0) * source.width() as f32 - 0.5,
                local_y.clamp(0.0, 1.0) * source.height() as f32 - 0.5,
                resampling,
            );
            composite_premultiplied(destination, apply_coverage(sample, coverage));
        }
    }
    Ok(())
}

fn apply_coverage(mut source: [u8; 4], coverage: u8) -> [u8; 4] {
    for channel in &mut source {
        *channel = ((u16::from(*channel) * u16::from(coverage) + 127) / 255) as u8;
    }
    source
}

fn composite_premultiplied(destination: &mut [u8], source: [u8; 4]) {
    let inverse_alpha = u16::from(255 - source[3]);
    for channel in 0..4 {
        destination[channel] = (u16::from(source[channel])
            + (u16::from(destination[channel]) * inverse_alpha + 127) / 255)
            .min(255) as u8;
    }
}

fn sample_image(
    pixels: &RgbaImage,
    source_x: f32,
    source_y: f32,
    resampling: Resampling,
) -> [u8; 4] {
    let near_integer = |value: f32| (value - value.round()).abs() < 0.000_01;
    if near_integer(source_x)
        && near_integer(source_y)
        && source_x >= 0.0
        && source_y >= 0.0
        && source_x < pixels.width() as f32
        && source_y < pixels.height() as f32
    {
        return premultiply(*pixels.get_pixel(source_x.round() as u32, source_y.round() as u32));
    }
    let radius = match resampling {
        Resampling::Lanczos => 3,
        Resampling::Nearest => 0,
        Resampling::Bicubic | Resampling::GameAsset(_) => 2,
    };
    if radius == 0 {
        return premultiply(*pixels.get_pixel(
            source_x.round().clamp(0.0, pixels.width() as f32 - 1.0) as u32,
            source_y.round().clamp(0.0, pixels.height() as f32 - 1.0) as u32,
        ));
    }
    let mut total = [0.0; 4];
    let mut weights = 0.0;
    let start_x = (source_x - radius as f32).floor() as i32;
    let start_y = (source_y - radius as f32).floor() as i32;
    let end_x = (source_x + radius as f32).ceil() as i32;
    let end_y = (source_y + radius as f32).ceil() as i32;
    for y in start_y..=end_y {
        let weight_y = filter_weight(source_y - y as f32, resampling);
        if weight_y == 0.0 {
            continue;
        }
        for x in start_x..=end_x {
            let weight_x = filter_weight(source_x - x as f32, resampling);
            let weight = weight_x * weight_y;
            if weight == 0.0 {
                continue;
            }
            let sample = premultiply(*pixels.get_pixel(
                x.clamp(0, pixels.width() as i32 - 1) as u32,
                y.clamp(0, pixels.height() as i32 - 1) as u32,
            ));
            for channel in 0..4 {
                total[channel] += f32::from(sample[channel]) * weight;
            }
            weights += weight;
        }
    }
    if weights.abs() <= f32::EPSILON {
        return [0; 4];
    }
    let mut sample =
        std::array::from_fn(|channel| (total[channel] / weights).round().clamp(0.0, 255.0) as u8);
    for channel in 0..3 {
        sample[channel] = sample[channel].min(sample[3]);
    }
    sample
}

fn filter_weight(distance: f32, resampling: Resampling) -> f32 {
    let distance = distance.abs();
    match resampling {
        Resampling::Nearest => (distance < 0.5) as u8 as f32,
        Resampling::Bicubic | Resampling::GameAsset(_) => {
            if distance < 1.0 {
                (1.5 * distance - 2.5) * distance * distance + 1.0
            } else if distance < 2.0 {
                ((-0.5 * distance + 2.5) * distance - 4.0) * distance + 2.0
            } else {
                0.0
            }
        }
        Resampling::Lanczos if distance < 3.0 => {
            let pi_distance = std::f32::consts::PI * distance;
            if distance <= f32::EPSILON {
                1.0
            } else {
                (pi_distance.sin() / pi_distance)
                    * ((pi_distance / 3.0).sin() / (pi_distance / 3.0))
            }
        }
        Resampling::Lanczos => 0.0,
    }
}

fn premultiply(pixel: Rgba<u8>) -> [u8; 4] {
    let alpha = u16::from(pixel[3]);
    [
        ((u16::from(pixel[0]) * alpha + 127) / 255) as u8,
        ((u16::from(pixel[1]) * alpha + 127) / 255) as u8,
        ((u16::from(pixel[2]) * alpha + 127) / 255) as u8,
        pixel[3],
    ]
}

fn draw_gap_markers(
    pixmap: &mut Pixmap,
    annotations: &[Annotation],
    markers: &[super::measure::GapMarker],
    dimensions: (u32, u32),
    transform: Transform,
    cancellation: &CancellationToken,
) -> Result<()> {
    for marker in markers {
        cancellation.check()?;
        let Some(first) = annotations
            .iter()
            .find(|annotation| annotation.id == marker.first)
        else {
            continue;
        };
        let Shape::Measurement { style, .. } = &first.shape else {
            continue;
        };
        let stroke_width = MEASUREMENT_STROKE_WIDTH;
        let (start, end, tick) = match marker.axis {
            Axis::Horizontal => (
                Point {
                    x: marker.along,
                    y: marker.from_at,
                },
                Point {
                    x: marker.along,
                    y: marker.to_at,
                },
                Point {
                    x: stroke_width * 2.0,
                    y: 0.0,
                },
            ),
            Axis::Vertical => (
                Point {
                    x: marker.from_at,
                    y: marker.along,
                },
                Point {
                    x: marker.to_at,
                    y: marker.along,
                },
                Point {
                    x: 0.0,
                    y: stroke_width * 2.0,
                },
            ),
        };
        stroke_polyline(
            pixmap,
            &[start, end],
            style.color,
            stroke_width,
            transform,
            false,
        );
        for point in [start, end] {
            stroke_polyline(
                pixmap,
                &[
                    Point {
                        x: point.x - tick.x,
                        y: point.y - tick.y,
                    },
                    Point {
                        x: point.x + tick.x,
                        y: point.y + tick.y,
                    },
                ],
                style.color,
                stroke_width,
                transform,
                false,
            );
        }
        let label = gap_label_layout(dimensions, marker);
        draw_pixel_text(pixmap, label.origin, &label.text, style.color, transform);
    }
    Ok(())
}

fn gap_label_layout(dimensions: (u32, u32), marker: &super::measure::GapMarker) -> GapLabelLayout {
    let text = length_label(marker.from_at, marker.to_at);
    let width = text_width(&text);
    let midpoint = match marker.axis {
        Axis::Horizontal => Point {
            x: marker.along,
            y: (marker.from_at + marker.to_at) / 2.0,
        },
        Axis::Vertical => Point {
            x: (marker.from_at + marker.to_at) / 2.0,
            y: marker.along,
        },
    };
    let max_x = (dimensions.0 as f32 - width).max(0.0);
    let max_y = (dimensions.1 as f32 - PIXEL_LABEL_HEIGHT).max(0.0);
    let origin = match marker.axis {
        Axis::Horizontal => Point {
            x: (midpoint.x + 3.0).clamp(0.0, max_x).round(),
            y: (midpoint.y - PIXEL_LABEL_HEIGHT / 2.0)
                .clamp(0.0, max_y)
                .round(),
        },
        Axis::Vertical => Point {
            x: (midpoint.x - width / 2.0).clamp(0.0, max_x).round(),
            y: (midpoint.y - PIXEL_LABEL_HEIGHT - 3.0)
                .clamp(0.0, max_y)
                .round(),
        },
    };
    let mut bounds = Bounds::point(origin);
    bounds.include(Point {
        x: origin.x + width,
        y: origin.y + PIXEL_LABEL_HEIGHT,
    });
    bounds.expand(1.0);
    GapLabelLayout {
        text,
        origin,
        bounds,
    }
}

fn measurement_label_layout(
    dimensions: (u32, u32),
    axis: Axis,
    from: f32,
    to: f32,
    at: f32,
    text: String,
) -> GapLabelLayout {
    let width = text_width(&text);
    let max_x = (dimensions.0 as f32 - width).max(0.0);
    let max_y = (dimensions.1 as f32 - PIXEL_LABEL_HEIGHT).max(0.0);
    let origin = match axis {
        Axis::Horizontal => Point {
            x: ((from + to - width) / 2.0).clamp(0.0, max_x).round(),
            y: (at - PIXEL_LABEL_HEIGHT - 2.0).clamp(0.0, max_y).round(),
        },
        Axis::Vertical => Point {
            x: (at + 3.0).clamp(0.0, max_x).round(),
            y: ((from + to - PIXEL_LABEL_HEIGHT) / 2.0)
                .clamp(0.0, max_y)
                .round(),
        },
    };
    let mut bounds = Bounds::point(origin);
    bounds.include(Point {
        x: origin.x + width,
        y: origin.y + PIXEL_LABEL_HEIGHT,
    });
    bounds.expand(1.0);
    GapLabelLayout {
        text,
        origin,
        bounds,
    }
}

fn draw_pixel_text(
    pixmap: &mut Pixmap,
    origin: Point,
    text: &str,
    color: [u8; 4],
    transform: Transform,
) {
    let mut builder = PathBuilder::new();
    for_each_ink_pixel(text, |x, y| {
        let x = origin.x + x as f32;
        let y = origin.y + y as f32;
        builder.move_to(x, y);
        builder.line_to(x + 1.0, y);
        builder.line_to(x + 1.0, y + 1.0);
        builder.line_to(x, y + 1.0);
        builder.close();
    });
    if let Some(path) = builder.finish() {
        pixmap.fill_path(
            &path,
            &paint(color, false),
            FillRule::Winding,
            transform,
            None,
        );
    }
}

fn draw_text(
    pixmap: &mut Pixmap,
    anchor: Point,
    angle: f32,
    bend: f32,
    text: &str,
    font_size: f32,
    style: DrawStyle,
) {
    let mut builder = PathBuilder::new();
    for placement in glyph_placements(anchor, angle, bend, text, font_size) {
        let scale = placement.font_size / units_per_em();
        let direction = Point {
            x: placement.tangent_angle.cos(),
            y: placement.tangent_angle.sin(),
        };
        let perpendicular = Point {
            x: -direction.y,
            y: direction.x,
        };
        let map = |x: f32, y: f32| Point {
            x: placement.position.x
                + (x * scale + placement.offset.x - placement.advance / 2.0) * direction.x
                + (-y * scale + placement.offset.y) * perpendicular.x,
            y: placement.position.y
                + (x * scale + placement.offset.x - placement.advance / 2.0) * direction.y
                + (-y * scale + placement.offset.y) * perpendicular.y,
        };
        for command in glyph_outline(placement.glyph) {
            match command {
                OutlineCommand::Move(x, y) => {
                    let point = map(x, y);
                    builder.move_to(point.x, point.y);
                }
                OutlineCommand::Line(x, y) => {
                    let point = map(x, y);
                    builder.line_to(point.x, point.y);
                }
                OutlineCommand::Quad(x1, y1, x, y) => {
                    let control = map(x1, y1);
                    let point = map(x, y);
                    builder.quad_to(control.x, control.y, point.x, point.y);
                }
                OutlineCommand::Curve(x1, y1, x2, y2, x, y) => {
                    let first = map(x1, y1);
                    let second = map(x2, y2);
                    let point = map(x, y);
                    builder.cubic_to(first.x, first.y, second.x, second.y, point.x, point.y);
                }
                OutlineCommand::Close => builder.close(),
            }
        }
    }
    if let Some(path) = builder.finish() {
        let paint = paint(style.color, style.anti_aliasing);
        pixmap.fill_path(&path, &paint, FillRule::Winding, style.transform, None);
    }
}

fn stroke_polyline(
    pixmap: &mut Pixmap,
    points: &[Point],
    color: [u8; 4],
    width: f32,
    transform: Transform,
    anti_aliasing: bool,
) {
    let Some(first) = points.first() else {
        return;
    };
    let mut builder = PathBuilder::new();
    builder.move_to(first.x, first.y);
    for point in points.iter().skip(1) {
        builder.line_to(point.x, point.y);
    }
    let Some(path) = builder.finish() else {
        return;
    };
    let stroke = Stroke {
        width: width.max(0.1),
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    pixmap.stroke_path(
        &path,
        &paint(color, anti_aliasing),
        &stroke,
        transform,
        None,
    );
}

fn fill_polygon<const N: usize>(
    pixmap: &mut Pixmap,
    points: &[Point; N],
    color: [u8; 4],
    transform: Transform,
) {
    let Some(first) = points.first() else {
        return;
    };
    let mut builder = PathBuilder::new();
    builder.move_to(first.x, first.y);
    for point in points.iter().skip(1) {
        builder.line_to(point.x, point.y);
    }
    builder.close();
    if let Some(path) = builder.finish() {
        pixmap.fill_path(
            &path,
            &paint(color, true),
            FillRule::Winding,
            transform,
            None,
        );
    }
}

fn paint(color: [u8; 4], anti_aliasing: bool) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color[0], color[1], color[2], color[3]);
    paint.anti_alias = anti_aliasing;
    paint
}

fn unpremultiplied_image(pixmap: Pixmap, dimensions: (u32, u32)) -> RgbaImage {
    unpremultiplied_data(pixmap.data(), dimensions)
}

fn unpremultiplied_data(data: &[u8], dimensions: (u32, u32)) -> RgbaImage {
    let mut image = RgbaImage::new(dimensions.0, dimensions.1);
    for (destination, source) in image.pixels_mut().zip(data.chunks_exact(4)) {
        let alpha = u16::from(source[3]);
        if alpha == 0 {
            *destination = Rgba([0, 0, 0, 0]);
        } else {
            *destination = Rgba([
                ((u16::from(source[0]) * 255 + alpha / 2) / alpha).min(255) as u8,
                ((u16::from(source[1]) * 255 + alpha / 2) / alpha).min(255) as u8,
                ((u16::from(source[2]) * 255 + alpha / 2) / alpha).min(255) as u8,
                source[3],
            ]);
        }
    }
    image
}

#[derive(Debug, Clone, Copy)]
struct Bounds {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
}

impl Bounds {
    fn point(point: Point) -> Self {
        Self {
            min_x: point.x,
            min_y: point.y,
            max_x: point.x,
            max_y: point.y,
        }
    }

    fn include(&mut self, point: Point) {
        self.min_x = self.min_x.min(point.x);
        self.min_y = self.min_y.min(point.y);
        self.max_x = self.max_x.max(point.x);
        self.max_y = self.max_y.max(point.y);
    }

    fn expand(&mut self, amount: f32) {
        self.min_x -= amount;
        self.min_y -= amount;
        self.max_x += amount;
        self.max_y += amount;
    }

    fn union(&mut self, other: Self) {
        self.include(Point {
            x: other.min_x,
            y: other.min_y,
        });
        self.include(Point {
            x: other.max_x,
            y: other.max_y,
        });
    }
}

fn rendered_bounds(
    dimensions: (u32, u32),
    annotations: &[Annotation],
    marker_context: &[Annotation],
    markers: &[super::measure::GapMarker],
) -> Option<Rect> {
    let mut bounds = annotations
        .iter()
        .filter_map(|annotation| annotation_bounds(annotation, dimensions))
        .reduce(|mut all, next| {
            all.union(next);
            all
        });
    for marker in markers {
        let Some(first) = marker_context
            .iter()
            .find(|annotation| annotation.id == marker.first)
        else {
            continue;
        };
        if !matches!(&first.shape, Shape::Measurement { .. }) {
            continue;
        }
        let mut marker_bounds = match marker.axis {
            Axis::Horizontal => Bounds::point(Point {
                x: marker.along,
                y: marker.from_at,
            }),
            Axis::Vertical => Bounds::point(Point {
                x: marker.from_at,
                y: marker.along,
            }),
        };
        marker_bounds.include(match marker.axis {
            Axis::Horizontal => Point {
                x: marker.along,
                y: marker.to_at,
            },
            Axis::Vertical => Point {
                x: marker.to_at,
                y: marker.along,
            },
        });
        marker_bounds.expand(MEASUREMENT_STROKE_WIDTH * 2.5);
        marker_bounds.union(gap_label_layout(dimensions, marker).bounds);
        if let Some(all) = &mut bounds {
            all.union(marker_bounds);
        } else {
            bounds = Some(marker_bounds);
        }
    }
    let bounds = bounds?;
    let min_x = bounds.min_x.floor().clamp(0.0, dimensions.0 as f32);
    let min_y = bounds.min_y.floor().clamp(0.0, dimensions.1 as f32);
    let max_x = bounds.max_x.ceil().clamp(0.0, dimensions.0 as f32);
    let max_y = bounds.max_y.ceil().clamp(0.0, dimensions.1 as f32);
    (max_x > min_x && max_y > min_y).then_some(Rect {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
    })
}

#[cfg(test)]
pub(crate) fn contained_in(annotation: &Annotation, dimensions: (u32, u32), rect: Rect) -> bool {
    annotation_bounds(annotation, dimensions).is_some_and(|bounds| {
        bounds.min_x >= rect.x
            && bounds.min_y >= rect.y
            && bounds.max_x <= rect.x + rect.width
            && bounds.max_y <= rect.y + rect.height
    })
}

fn annotation_bounds(annotation: &Annotation, dimensions: (u32, u32)) -> Option<Bounds> {
    match &annotation.shape {
        Shape::Image { corners, .. } => {
            let mut points = corners.iter().copied();
            let mut bounds = Bounds::point(points.next()?);
            points.for_each(|point| bounds.include(point));
            Some(bounds)
        }
        Shape::Pencil {
            geometry, style, ..
        } => {
            let rect = geometry_bounds(geometry);
            let mut bounds = Bounds::point(Point {
                x: rect.x,
                y: rect.y,
            });
            bounds.include(Point {
                x: rect.x + rect.width,
                y: rect.y + rect.height,
            });
            bounds.expand(style.width / 2.0 + 2.0);
            Some(bounds)
        }
        Shape::Highlight {
            rect,
            angle,
            seed,
            style,
        } => {
            let mut points =
                super::highlight::rotated_sloppy_ellipse(*rect, *seed, *angle).into_iter();
            let mut bounds = Bounds::point(points.next()?);
            points.for_each(|point| bounds.include(point));
            bounds.expand(highlight_stroke_width(style.width) / 2.0 + 2.0);
            Some(bounds)
        }
        Shape::Arrow {
            start,
            end,
            control,
            style,
        } => {
            let mut bounds = Bounds::point(*start);
            bounds.include(*end);
            bounds.include(*control);
            arrow_head(*start, *control, *end, style.width)
                .into_iter()
                .for_each(|point| bounds.include(point));
            bounds.expand(style.width / 2.0 + 2.0);
            Some(bounds)
        }
        Shape::Measurement {
            axis,
            from,
            to,
            at,
            style: _,
            label_size: _,
        } => {
            let (start, end) = match axis {
                Axis::Horizontal => (Point { x: *from, y: *at }, Point { x: *to, y: *at }),
                Axis::Vertical => (Point { x: *at, y: *from }, Point { x: *at, y: *to }),
            };
            let mut bounds = Bounds::point(start);
            bounds.include(end);
            bounds.expand(MEASUREMENT_STROKE_WIDTH * 3.5 + 1.0);
            bounds.union(
                measurement_label_layout(
                    dimensions,
                    *axis,
                    *from,
                    *to,
                    *at,
                    length_label(*from, *to),
                )
                .bounds,
            );
            Some(bounds)
        }
        Shape::Text {
            anchor,
            angle,
            font_size,
            bend,
            text,
            ..
        } => {
            let mut bounds = Bounds::point(*anchor);
            let mut rendered_font_size = *font_size;
            for placement in glyph_placements(*anchor, *angle, *bend, text, *font_size) {
                bounds.include(placement.position);
                rendered_font_size = rendered_font_size.max(placement.font_size);
            }
            bounds.expand(rendered_font_size * 1.5 + 2.0);
            Some(bounds)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{AnnotationId, PencilGeometry, Rect, StrokeStyle};

    #[test]
    fn rectangle_deletion_requires_full_annotation_containment() {
        let annotation = Annotation {
            id: AnnotationId(1),
            shape: Shape::Highlight {
                rect: Rect {
                    x: 30.0,
                    y: 30.0,
                    width: 20.0,
                    height: 20.0,
                },
                angle: 0.0,
                seed: 5,
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 3.0,
                },
            },
        };
        assert!(contained_in(
            &annotation,
            (100, 100),
            Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0
            }
        ));
        assert!(!contained_in(
            &annotation,
            (100, 100),
            Rect {
                x: 40.0,
                y: 0.0,
                width: 60.0,
                height: 100.0
            }
        ));
        assert!(!contained_in(
            &annotation,
            (100, 100),
            Rect {
                x: 70.0,
                y: 70.0,
                width: 30.0,
                height: 30.0
            }
        ));
    }

    #[test]
    fn render_changes_pixels_only_near_the_annotation() {
        let annotation = Annotation {
            id: AnnotationId(1),
            shape: Shape::Highlight {
                rect: Rect {
                    x: 20.0,
                    y: 20.0,
                    width: 40.0,
                    height: 30.0,
                },
                angle: 0.0,
                seed: 5,
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 3.0,
                },
            },
        };
        let overlay = render_overlay(
            (100, 100),
            std::slice::from_ref(&annotation),
            &CancellationToken::default(),
        )
        .unwrap();
        assert!(overlay.pixels().any(|pixel| pixel[3] != 0));
        assert_eq!(overlay.get_pixel(0, 0).0, [0, 0, 0, 0]);
        assert_eq!(overlay.get_pixel(99, 99).0, [0, 0, 0, 0]);
        let bounded = render_bounded_overlay(
            (100, 100),
            std::slice::from_ref(&annotation),
            &CancellationToken::default(),
        )
        .unwrap()
        .unwrap();
        assert!(bounded.pixels.width() < 100);
        assert!(bounded.pixels.height() < 100);
    }

    #[test]
    fn rotated_highlight_render_bounds_follow_the_rotated_ellipse() {
        let annotation = Annotation {
            id: AnnotationId(1),
            shape: Shape::Highlight {
                rect: Rect {
                    x: 50.0,
                    y: 60.0,
                    width: 100.0,
                    height: 30.0,
                },
                angle: std::f32::consts::FRAC_PI_2,
                seed: 9,
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 1.0,
                },
            },
        };
        let overlay = render_bounded_overlay(
            (220, 220),
            std::slice::from_ref(&annotation),
            &CancellationToken::default(),
        )
        .expect("render rotated highlight")
        .expect("rotated highlight bounds");
        assert!(overlay.bounds.width < 45.0, "rotated width");
        assert!(overlay.bounds.height > 100.0, "rotated height");
        assert!(overlay.pixels.pixels().any(|pixel| pixel[3] != 0));
    }

    #[test]
    fn editable_pencil_shapes_match_the_existing_pencil_rasterizer() {
        let style = StrokeStyle {
            color: [12, 34, 56, 220],
            width: 3.0,
        };
        let geometries = [
            PencilGeometry::Freehand(vec![
                crate::document::BrushPoint {
                    x: 4.5,
                    y: 6.5,
                    pressure: 0.5,
                },
                crate::document::BrushPoint {
                    x: 12.5,
                    y: 15.5,
                    pressure: 0.75,
                },
                crate::document::BrushPoint {
                    x: 24.5,
                    y: 18.5,
                    pressure: 1.0,
                },
            ]),
            PencilGeometry::Line(vec![Point { x: 4.5, y: 6.5 }, Point { x: 24.5, y: 18.5 }]),
            PencilGeometry::Rectangle(Rect {
                x: 4.5,
                y: 4.5,
                width: 20.0,
                height: 14.0,
            }),
            PencilGeometry::Ellipse(Rect {
                x: 5.5,
                y: 3.5,
                width: 16.0,
                height: 16.0,
            }),
        ];

        for (index, geometry) in geometries.into_iter().enumerate() {
            let annotation = Annotation {
                id: AnnotationId(index as u64 + 1),
                shape: Shape::Pencil {
                    geometry: geometry.clone(),
                    style,
                    anti_aliasing: true,
                },
            };
            let actual =
                render_overlay((32, 24), &[annotation], &CancellationToken::default()).unwrap();
            let expected = paint_stroke(
                &RgbaImage::new(32, 24),
                &stroke_for(&geometry, style, true),
                &CancellationToken::default(),
            )
            .unwrap();

            for (actual, expected) in actual.pixels().zip(expected.pixels()) {
                assert_eq!(actual[3], expected[3], "geometry {index}");
                for channel in 0..3 {
                    let actual_premultiplied =
                        (u16::from(actual[channel]) * u16::from(actual[3]) + 127) / 255;
                    let expected_premultiplied =
                        (u16::from(expected[channel]) * u16::from(expected[3]) + 127) / 255;
                    assert_eq!(
                        actual_premultiplied, expected_premultiplied,
                        "geometry {index}, channel {channel}"
                    );
                }
            }
        }
    }

    #[test]
    fn text_annotation_produces_visible_pixels() {
        let annotation = Annotation {
            id: AnnotationId(1),
            shape: Shape::Text {
                anchor: Point { x: 24.0, y: 64.0 },
                angle: 0.0,
                font_size: 24.0,
                bend: 0.0,
                text: "Visible text".to_owned(),
                color: [255, 0, 0, 255],
            },
        };

        let rendered = render_overlay(
            (240, 100),
            std::slice::from_ref(&annotation),
            &CancellationToken::default(),
        )
        .expect("text overlay");

        assert!(
            rendered.pixels().any(|pixel| pixel[3] != 0),
            "text outline produced no visible pixels"
        );
    }

    #[test]
    fn highlight_width_uses_the_stored_pen_setting_independent_of_image_size() {
        let render = |dimensions, width| {
            render_bounded_overlay(
                dimensions,
                &[Annotation {
                    id: AnnotationId(1),
                    shape: Shape::Highlight {
                        rect: Rect {
                            x: 30.0,
                            y: 25.0,
                            width: 100.0,
                            height: 70.0,
                        },
                        angle: 0.0,
                        seed: 5,
                        style: StrokeStyle {
                            color: [255, 0, 0, 255],
                            width,
                        },
                    },
                }],
                &CancellationToken::default(),
            )
            .unwrap()
            .unwrap()
            .pixels
            .pixels()
            .map(|pixel| u64::from(pixel[3]))
            .sum::<u64>()
        };

        let thin = render((1024, 300), 1.0);
        assert_eq!(thin, render((2048, 300), 1.0));
        let thick = render((1024, 300), 12.0);
        assert!(
            thick > thin * 3 / 2,
            "increasing the pen setting should increase crayon coverage: {thin} -> {thick}"
        );
    }

    #[test]
    fn measurement_preview_contains_dependent_gap_markers() {
        let line = |id, at| Annotation {
            id: AnnotationId(id),
            shape: Shape::Measurement {
                axis: Axis::Horizontal,
                from: 20.0,
                to: 180.0,
                at,
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 3.0,
                },
                label_size: 12.0,
            },
        };
        let stationary = line(1, 20.0);
        let dragged = line(2, 100.0);
        let preview = render_annotation_preview(
            (200, 150),
            &dragged,
            std::slice::from_ref(&stationary),
            &CancellationToken::default(),
        )
        .unwrap()
        .unwrap();
        assert!(preview.bounds.y <= 20.0);
        assert!(preview.bounds.y + preview.bounds.height >= 100.0);
    }

    #[test]
    fn gap_label_clamped_from_the_right_edge_stays_inside_the_overlay() {
        let line = |id, at| Annotation {
            id: AnnotationId(id),
            shape: Shape::Measurement {
                axis: Axis::Horizontal,
                from: 118.0,
                to: 120.0,
                at,
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: MEASUREMENT_STROKE_WIDTH,
                },
                label_size: 24.0,
            },
        };
        let annotations = [line(1, 20.0), line(2, 100.0)];
        let label = length_label(20.0, 100.0);
        let expected_label_x = (120.0 - text_width(&label)).max(0.0).floor();

        let overlay = render_subset(
            (120, 120),
            &[],
            &annotations,
            None,
            &CancellationToken::default(),
        )
        .expect("gap overlay render")
        .expect("paired measurement gap");

        assert!(
            overlay.bounds.x <= expected_label_x,
            "gap label begins at x={expected_label_x}, outside overlay {:?}",
            overlay.bounds
        );
        assert!(
            overlay
                .pixels
                .enumerate_pixels()
                .any(|(x, _, pixel)| { overlay.bounds.x + (x as f32) < 118.0 && pixel[3] == 255 }),
            "edge-clamped gap label produced no visible pixels"
        );
        assert!(
            overlay
                .pixels
                .pixels()
                .all(|pixel| matches!(pixel[3], 0 | 255)),
            "gap label introduced partial antialiasing"
        );
    }

    #[test]
    fn measurement_rendering_is_pixel_exact_without_antialiasing() {
        let measurement = Annotation {
            id: AnnotationId(1),
            shape: Shape::Measurement {
                axis: Axis::Horizontal,
                from: 32.0,
                to: 96.0,
                at: 64.0,
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 1.0,
                },
                label_size: 8.0,
            },
        };
        let rendered = render_overlay(
            (128, 128),
            std::slice::from_ref(&measurement),
            &CancellationToken::default(),
        )
        .expect("measurement overlay");

        assert!(rendered.pixels().any(|pixel| pixel[3] == 255));
        assert!(
            rendered.pixels().all(|pixel| matches!(pixel[3], 0 | 255)),
            "measurement output contained partially covered antialiased pixels"
        );
    }

    #[test]
    fn measurement_label_is_fixed_size_independent_of_text_setting() {
        let small_label = Annotation {
            id: AnnotationId(1),
            shape: Shape::Measurement {
                axis: Axis::Horizontal,
                from: 20.0,
                to: 148.0,
                at: 50.0,
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: MEASUREMENT_STROKE_WIDTH,
                },
                label_size: 6.0,
            },
        };
        let mut large_label = small_label.clone();
        let Shape::Measurement { label_size, .. } = &mut large_label.shape else {
            unreachable!();
        };
        *label_size = 512.0;

        let render = |annotation: &Annotation| {
            render_overlay(
                (180, 80),
                std::slice::from_ref(annotation),
                &CancellationToken::default(),
            )
            .expect("measurement overlay")
        };

        assert_eq!(render(&small_label), render(&large_label));
    }

    #[test]
    fn measurement_label_is_safe_on_an_image_smaller_than_the_text() {
        let measurement = Annotation {
            id: AnnotationId(1),
            shape: Shape::Measurement {
                axis: Axis::Vertical,
                from: 1.0,
                to: 7.0,
                at: 4.0,
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: MEASUREMENT_STROKE_WIDTH,
                },
                label_size: 24.0,
            },
        };

        let rendered = render_overlay(
            (8, 8),
            std::slice::from_ref(&measurement),
            &CancellationToken::default(),
        )
        .expect("small measurement overlay");
        assert!(rendered.pixels().any(|pixel| pixel[3] == 255));
        assert!(rendered.pixels().all(|pixel| matches!(pixel[3], 0 | 255)));
    }

    #[test]
    fn image_rotation_keeps_source_pixel_orientation_and_alpha() {
        let annotation = Annotation {
            id: AnnotationId(91),
            shape: Shape::Image {
                pixels: std::sync::Arc::new(RgbaImage::from_fn(2, 1, |x, _| {
                    if x == 0 {
                        Rgba([200, 30, 10, 128])
                    } else {
                        Rgba([10, 40, 220, 255])
                    }
                })),
                // A clockwise quarter turn around the source frame's center.
                corners: [
                    Point { x: 1.0, y: 0.0 },
                    Point { x: 1.0, y: 2.0 },
                    Point { x: 0.0, y: 2.0 },
                    Point { x: 0.0, y: 0.0 },
                ],
                resampling: Resampling::Lanczos,
            },
        };
        let image = render_overlay(
            (2, 2),
            std::slice::from_ref(&annotation),
            &CancellationToken::default(),
        )
        .unwrap();
        let top = image.get_pixel(0, 0);
        assert_eq!(top[3], 128);
        assert!((i16::from(top[0]) - 200).abs() <= 2);
        assert_eq!(*image.get_pixel(0, 1), Rgba([10, 40, 220, 255]));
    }

    #[test]
    fn image_downscaling_prefilters_per_axis_without_an_unbounded_affine_kernel() {
        let source = RgbaImage::from_fn(4_000, 1, |x, _| {
            let value = if x % 2 == 0 { 0 } else { 255 };
            Rgba([value, value, value, 255])
        });
        for resampling in [Resampling::Bicubic, Resampling::Lanczos] {
            let annotation = Annotation {
                id: AnnotationId(92),
                shape: Shape::Image {
                    pixels: std::sync::Arc::new(source.clone()),
                    corners: [
                        Point { x: 0.0, y: 0.0 },
                        Point { x: 1.0, y: 0.0 },
                        Point { x: 1.0, y: 1.0 },
                        Point { x: 0.0, y: 1.0 },
                    ],
                    resampling,
                },
            };
            let image = render_overlay(
                (1, 1),
                std::slice::from_ref(&annotation),
                &CancellationToken::default(),
            )
            .unwrap();
            let expected = crate::tools::scale::resize(
                &source,
                1,
                1,
                resampling,
                &CancellationToken::default(),
            )
            .unwrap();
            assert_eq!(*image.get_pixel(0, 0), *expected.get_pixel(0, 0));
        }
    }

    #[test]
    fn rotated_image_edges_are_antialiased_without_transparent_color_fringes() {
        let center = Point { x: 2.0, y: 2.0 };
        let angle = 0.31_f32;
        let rotate = |point: Point| {
            let (sin, cos) = angle.sin_cos();
            let x = point.x - center.x;
            let y = point.y - center.y;
            Point {
                x: center.x + x * cos - y * sin,
                y: center.y + x * sin + y * cos,
            }
        };
        let annotation = Annotation {
            id: AnnotationId(93),
            shape: Shape::Image {
                pixels: std::sync::Arc::new(RgbaImage::from_pixel(1, 1, Rgba([255, 0, 0, 255]))),
                corners: [
                    rotate(Point { x: 1.0, y: 1.0 }),
                    rotate(Point { x: 3.0, y: 1.0 }),
                    rotate(Point { x: 3.0, y: 3.0 }),
                    rotate(Point { x: 1.0, y: 3.0 }),
                ],
                resampling: Resampling::Bicubic,
            },
        };
        let image = render_overlay(
            (4, 4),
            std::slice::from_ref(&annotation),
            &CancellationToken::default(),
        )
        .unwrap();
        assert!(image.pixels().any(|pixel| (1..255).contains(&pixel[3])));
        for pixel in image.pixels().filter(|pixel| pixel[3] != 0) {
            assert_eq!(pixel[0], 255);
            assert_eq!(pixel[1], 0);
            assert_eq!(pixel[2], 0);
        }
    }

    #[test]
    fn fully_offcanvas_image_skips_its_filter_and_leaves_the_overlay_unchanged() {
        let mut pixmap = Pixmap::new(2, 2).unwrap();
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        // A cancelled prefilter would return `Cancelled`; the frame rejection
        // must happen first for a distant image sharing an overlay with other
        // annotations.
        draw_image(
            &mut pixmap,
            &RgbaImage::from_pixel(4_000, 1, Rgba([255, 0, 0, 255])),
            [
                Point {
                    x: -10_000.0,
                    y: -10_000.0,
                },
                Point {
                    x: -9_999.0,
                    y: -10_000.0,
                },
                Point {
                    x: -9_999.0,
                    y: -9_999.0,
                },
                Point {
                    x: -10_000.0,
                    y: -9_999.0,
                },
            ],
            Resampling::Lanczos,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 2.0,
            },
            &cancellation,
        )
        .unwrap();
        assert!(pixmap.data().iter().all(|value| *value == 0));
    }
}
