use std::f32::consts::{PI, TAU};

use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, Transform};

use crate::document::{CancellationToken, Point, Rect};
use crate::error::{AppError, Result};

// An 8.4-pixel crayon at 1024 px, with enough body for grain on small images.
const WIDTH_RATIO: f32 = 8.4 / 1024.0;

#[must_use]
pub fn highlight_stroke_width(image_dimensions: (u32, u32)) -> f32 {
    let longest_side = image_dimensions.0.max(image_dimensions.1);
    (longest_side as f32 * WIDTH_RATIO).max(2.8)
}

#[derive(Debug, Clone, Copy)]
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1_u32 << 24) as f32
    }

    fn range(&mut self, low: f32, high: f32) -> f32 {
        low + (high - low) * self.unit()
    }
}

#[must_use]
pub fn sloppy_ellipse(rect: Rect, seed: u64) -> Vec<Point> {
    let radius_x = rect.width / 2.0;
    let radius_y = rect.height / 2.0;
    let center = rect.center();
    let minimum_radius = radius_x.min(radius_y).max(0.01);
    let perimeter = PI
        * (3.0 * (radius_x + radius_y)
            - ((3.0 * radius_x + radius_y) * (radius_x + 3.0 * radius_y)).sqrt());
    let mut random = SplitMix64(seed);
    let start = random.range(3.85, 4.25);
    let segments = (perimeter * (4.0 / 3.0) / 2.0).clamp(64.0, 1024.0).ceil() as usize;
    let tilt = random.range(-1.5_f32.to_radians(), 1.5_f32.to_radians());
    let wander_phase = random.range(0.0, TAU);
    let harmonics = [2.0_f32, 3.0, 5.0].map(|frequency| {
        (
            frequency,
            random.range(0.008, 0.018) * minimum_radius,
            random.range(0.0, TAU),
        )
    });

    // One continuous 480-degree gesture. The hand drifts inward on the extra
    // third of a turn, leaving a visible overlapping arc across the top.
    let mut points = Vec::with_capacity(segments + 1);
    for index in 0..=segments {
        let progress = index as f32 / segments as f32;
        let angle = start + progress * TAU * (4.0 / 3.0);
        let noise = harmonics
            .iter()
            .fold(0.0, |sum, (frequency, amplitude, phase)| {
                sum + amplitude * (frequency * angle + phase).sin()
            });
        let drift = 0.20 * progress;
        let scale = 1.075 - drift + 0.012 * (angle + wander_phase).sin();
        let x = (radius_x * scale + noise) * angle.cos();
        let y = (radius_y * scale + noise) * angle.sin();
        points.push(Point {
            x: center.x + x * tilt.cos() - y * tilt.sin(),
            y: center.y + x * tilt.sin() + y * tilt.cos(),
        });
    }
    points
}

#[must_use]
pub fn rotated_sloppy_ellipse(rect: Rect, seed: u64, angle: f32) -> Vec<Point> {
    if angle == 0.0 {
        return sloppy_ellipse(rect, seed);
    }
    let center = rect.center();
    let (sin, cos) = angle.sin_cos();
    sloppy_ellipse(rect, seed)
        .into_iter()
        .map(|point| Point {
            x: center.x + (point.x - center.x) * cos - (point.y - center.y) * sin,
            y: center.y + (point.x - center.x) * sin + (point.y - center.y) * cos,
        })
        .collect()
}

#[must_use]
pub fn rotated_rect_points(rect: Rect, angle: f32) -> [Point; 4] {
    let center = rect.center();
    let (sin, cos) = angle.sin_cos();
    std::array::from_fn(|index| {
        let point = [
            Point {
                x: rect.x,
                y: rect.y,
            },
            Point {
                x: rect.x + rect.width,
                y: rect.y,
            },
            Point {
                x: rect.x + rect.width,
                y: rect.y + rect.height,
            },
            Point {
                x: rect.x,
                y: rect.y + rect.height,
            },
        ][index];
        Point {
            x: center.x + (point.x - center.x) * cos - (point.y - center.y) * sin,
            y: center.y + (point.x - center.x) * sin + (point.y - center.y) * cos,
        }
    })
}

/// Render on a separate, tightly bounded layer so grain never erases another
/// annotation. Texture coordinates follow the oval, independent of overlay bounds.
#[derive(Debug, Clone, Copy)]
pub(super) struct CrayonGeometry {
    pub rect: Rect,
    pub angle: f32,
    pub seed: u64,
}

pub(super) fn draw_crayon(
    destination: &mut Pixmap,
    geometry: CrayonGeometry,
    width: f32,
    color: [u8; 4],
    transform: Transform,
    cancellation: &CancellationToken,
) -> Result<()> {
    let CrayonGeometry { rect, angle, seed } = geometry;
    let points = rotated_sloppy_ellipse(rect, seed, angle);
    let padding = width / 2.0 + 2.0;
    let left = points
        .iter()
        .map(|p| p.x + transform.tx)
        .fold(f32::INFINITY, f32::min);
    let top = points
        .iter()
        .map(|p| p.y + transform.ty)
        .fold(f32::INFINITY, f32::min);
    let right = points
        .iter()
        .map(|p| p.x + transform.tx)
        .fold(f32::NEG_INFINITY, f32::max);
    let bottom = points
        .iter()
        .map(|p| p.y + transform.ty)
        .fold(f32::NEG_INFINITY, f32::max);
    let left = (left - padding).floor().max(0.0) as u32;
    let top = (top - padding).floor().max(0.0) as u32;
    let right = (right + padding)
        .ceil()
        .clamp(0.0, destination.width() as f32) as u32;
    let bottom = (bottom + padding)
        .ceil()
        .clamp(0.0, destination.height() as f32) as u32;
    if right <= left || bottom <= top {
        return Ok(());
    }
    let mut layer = Pixmap::new(right - left, bottom - top).ok_or(AppError::InvalidDimensions)?;
    let local_transform =
        Transform::from_translate(transform.tx - left as f32, transform.ty - top as f32);
    let mut random = SplitMix64(seed ^ 0xC4A7_0A5E);
    let phase = random.range(0.0, TAU);
    let radii: Vec<_> = (0..points.len())
        .map(|index| {
            let progress = index as f32 / (points.len() - 1) as f32;
            let taper = (progress / 0.025)
                .min((1.0 - progress) / 0.055)
                .clamp(0.0, 1.0);
            width
                * 0.5
                * (0.78 + 0.12 * (progress * TAU * 2.0 + phase).sin() + random.range(-0.07, 0.07))
                * (0.18 + 0.82 * taper)
        })
        .collect();
    // A ragged, light outer edge surrounds the denser wax deposited at the core.
    for (scale, opacity) in [(1.0, 0.38), (0.72, 0.90)] {
        let mut outline = Vec::with_capacity(points.len() * 2);
        for (index, point) in points.iter().enumerate() {
            let before = points[index.saturating_sub(1)];
            let after = points[(index + 1).min(points.len() - 1)];
            let length = before.distance(after).max(0.001);
            let radius = radii[index] * scale;
            outline.push((
                Point {
                    x: point.x - (after.y - before.y) / length * radius,
                    y: point.y + (after.x - before.x) / length * radius,
                },
                Point {
                    x: point.x + (after.y - before.y) / length * radius,
                    y: point.y - (after.x - before.x) / length * radius,
                },
            ));
        }
        let mut builder = PathBuilder::new();
        builder.move_to(outline[0].0.x, outline[0].0.y);
        for point in outline
            .iter()
            .skip(1)
            .map(|pair| pair.0)
            .chain(outline.iter().rev().map(|pair| pair.1))
        {
            builder.line_to(point.x, point.y);
        }
        builder.close();
        if let Some(path) = builder.finish() {
            let mut paint = Paint::default();
            paint.set_color_rgba8(
                color[0],
                color[1],
                color[2],
                (f32::from(color[3]) * opacity) as u8,
            );
            layer.fill_path(&path, &paint, FillRule::Winding, local_transform, None);
        }
    }
    let grain_size = (width / 7.0).max(0.65);
    let layer_width = layer.width() as usize;
    let inverse_rotation = if angle == 0.0 {
        None
    } else {
        let (sin, cos) = (-angle).sin_cos();
        Some((rect.center(), sin, cos))
    };
    for (index, pixel) in layer.data_mut().chunks_exact_mut(4).enumerate() {
        if index % 16_384 == 0 {
            cancellation.check()?;
        }
        if pixel[3] == 0 {
            continue;
        }
        let (x, y) = if let Some((center, sin, cos)) = inverse_rotation {
            let point = Point {
                x: (index % layer_width) as f32 + left as f32 - transform.tx,
                y: (index / layer_width) as f32 + top as f32 - transform.ty,
            };
            (
                (point.x - center.x) * cos - (point.y - center.y) * sin + center.x - rect.x,
                (point.x - center.x) * sin + (point.y - center.y) * cos + center.y - rect.y,
            )
        } else {
            (
                (index % layer_width) as f32 + left as f32 - transform.tx - rect.x,
                (index / layer_width) as f32 + top as f32 - transform.ty - rect.y,
            )
        };
        let fine = grain(seed, x / grain_size, y / grain_size);
        let coarse = grain(
            seed ^ 0x51A7,
            x / (grain_size * 2.7),
            y / (grain_size * 2.7),
        );
        // Sparse paper flecks and mottled pigment, including inside the stroke.
        let opacity = if fine < 0.13 {
            0.10
        } else {
            0.62 + 0.38 * fine
        };
        let opacity = opacity * (0.80 + 0.20 * coarse);
        for channel in pixel {
            *channel = (f32::from(*channel) * opacity).round() as u8;
        }
    }
    // Live previews start transparent. Copying their premultiplied pixels is
    // equivalent to source-over, without blending the oval's large empty center.
    if destination.pixels().iter().all(|pixel| pixel.alpha() == 0) {
        let destination_stride = destination.width() as usize * 4;
        let layer_stride = layer.width() as usize * 4;
        for (row, source) in layer.data().chunks_exact(layer_stride).enumerate() {
            cancellation.check()?;
            let start = (top as usize + row) * destination_stride + left as usize * 4;
            destination.data_mut()[start..start + layer_stride].copy_from_slice(source);
        }
        return Ok(());
    }
    destination.draw_pixmap(
        left as i32,
        top as i32,
        layer.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    Ok(())
}

fn grain(seed: u64, x: f32, y: f32) -> f32 {
    let x = x.floor() as i64 as u64;
    let y = y.floor() as i64 as u64;
    SplitMix64(seed ^ x.wrapping_mul(0x9E37_79B9) ^ y.wrapping_mul(0x85EB_CA6B)).unit()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECT: Rect = Rect {
        x: 10.0,
        y: 20.0,
        width: 100.0,
        height: 60.0,
    };

    #[test]
    fn crayon_grain_survives_moving_clipping_and_compositing() {
        let render = |rect, angle, transform, background| {
            let mut image = Pixmap::new(180, 120).unwrap();
            image.fill(background);
            draw_crayon(
                &mut image,
                CrayonGeometry {
                    rect,
                    angle,
                    seed: 4,
                },
                12.0,
                [240, 20, 20, 255],
                transform,
                &CancellationToken::default(),
            )
            .unwrap();
            image
        };
        let clear = tiny_skia::Color::TRANSPARENT;
        let original = render(RECT, 0.0, Transform::identity(), clear);
        assert_eq!(original, render(RECT, 0.0, Transform::identity(), clear));
        let translated = render(
            Rect {
                x: RECT.x + 17.0,
                y: RECT.y + 9.0,
                ..RECT
            },
            0.0,
            Transform::identity(),
            clear,
        );
        for y in 0..100 {
            for x in 0..150 {
                assert_eq!(original.pixel(x, y), translated.pixel(x + 17, y + 9));
            }
        }
        let clipped = render(RECT, 0.0, Transform::from_translate(-40.0, -30.0), clear);
        for y in 0..90 {
            for x in 0..140 {
                assert_eq!(original.pixel(x + 40, y + 30), clipped.pixel(x, y));
            }
        }
        let on_blue = render(
            RECT,
            0.0,
            Transform::identity(),
            tiny_skia::Color::from_rgba8(0, 0, 255, 255),
        );
        let mut composited = Pixmap::new(180, 120).unwrap();
        composited.fill(tiny_skia::Color::from_rgba8(0, 0, 255, 255));
        composited.draw_pixmap(
            0,
            0,
            original.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        assert_eq!(on_blue, composited);
        assert!(on_blue.pixels().iter().all(|pixel| pixel.alpha() == 255));
        assert_eq!(on_blue.pixel(60, 50).unwrap().blue(), 255);
        // Grain varies even along the center of the stroke, not just its AA edges.
        let alphas: Vec<_> = sloppy_ellipse(RECT, 4)
            .iter()
            .skip(10)
            .take(100)
            .map(|p| {
                original
                    .pixel(p.x.round() as u32, p.y.round() as u32)
                    .unwrap()
                    .alpha()
            })
            .collect();
        assert!(alphas.iter().filter(|&&alpha| alpha < 60).count() > 5);
        assert!(alphas.iter().filter(|&&alpha| alpha > 150).count() > 5);
    }

    #[test]
    #[ignore = "writes a visual preview to /tmp/diorama-highlight-preview.png"]
    fn export_crayon_preview() {
        let mut image = Pixmap::new(1000, 720).unwrap();
        image.fill(tiny_skia::Color::WHITE);
        for (index, seed) in [4, 9, 42].into_iter().enumerate() {
            draw_crayon(
                &mut image,
                CrayonGeometry {
                    rect: Rect {
                        x: 70.0,
                        y: 30.0 + index as f32 * 230.0,
                        width: 850.0,
                        height: 170.0,
                    },
                    angle: 0.0,
                    seed,
                },
                highlight_stroke_width((1000, 720)),
                [225, 25, 30, 255],
                Transform::identity(),
                &CancellationToken::default(),
            )
            .unwrap();
        }
        let rgba = image.data().to_vec();
        image::save_buffer(
            "/tmp/diorama-highlight-preview.png",
            &rgba,
            1000,
            720,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }

    #[test]
    fn ellipse_is_deterministic_and_seeded() {
        assert_eq!(sloppy_ellipse(RECT, 4), sloppy_ellipse(RECT, 4));
        assert_ne!(sloppy_ellipse(RECT, 4), sloppy_ellipse(RECT, 5));
    }

    #[test]
    fn zero_angle_uses_the_original_ellipse_points_exactly() {
        assert_eq!(
            rotated_sloppy_ellipse(RECT, 4, 0.0),
            sloppy_ellipse(RECT, 4)
        );
    }

    #[test]
    fn stroke_width_scales_continuously_with_image_resolution() {
        assert_eq!(highlight_stroke_width((1, 1)), 2.8);
        assert_eq!(highlight_stroke_width((512, 300)), 4.2);
        assert_eq!(highlight_stroke_width((1024, 512)), 8.4);
        assert_eq!(highlight_stroke_width((512, 2048)), 16.8);
        assert_eq!(highlight_stroke_width((4096, 2048)), 33.6);
        assert!(highlight_stroke_width((1024, 512)) - highlight_stroke_width((1023, 512)) < 0.02);
    }

    #[test]
    fn moving_preserves_relative_wobble() {
        let moved = Rect {
            x: RECT.x + 13.0,
            y: RECT.y - 7.0,
            ..RECT
        };
        for (left, right) in sloppy_ellipse(RECT, 9)
            .into_iter()
            .zip(sloppy_ellipse(moved, 9))
        {
            assert!((right.x - left.x - 13.0).abs() < 1e-4);
            assert!((right.y - left.y + 7.0).abs() < 1e-4);
        }
    }

    #[test]
    fn ellipse_has_one_full_turn_and_a_separated_overlapping_arc() {
        let center = RECT.center();
        let radius_x = RECT.width / 2.0;
        let radius_y = RECT.height / 2.0;
        for seed in 0..64 {
            let points = sloppy_ellipse(RECT, seed);
            assert!(points[0].x < center.x && points[0].y < center.y);
            let angle = |point: Point| {
                ((point.y - center.y) / radius_y).atan2((point.x - center.x) / radius_x)
            };
            let sweep: f32 = points
                .windows(2)
                .map(|pair| (angle(pair[1]) - angle(pair[0]) + PI).rem_euclid(TAU) - PI)
                .sum();
            assert!(
                (sweep.to_degrees() - 480.0).abs() < 8.0,
                "seed {seed} traced {} degrees",
                sweep.to_degrees()
            );
            let full_turn = (points.len() - 1) * 3 / 4;
            assert!(
                points[0].distance(points[full_turn]) > radius_y * 0.04,
                "seed {seed} hid the overlap on the first pass"
            );
            assert!(points.last().unwrap().distance(points[points.len() - 2]) < radius_y * 0.2);
        }
    }
}
