use std::f32::consts::{PI, TAU};

use crate::document::{Point, Rect};

const IMAGE_SIZE_BAND: u32 = 1024;

#[must_use]
pub fn highlight_stroke_width(image_dimensions: (u32, u32)) -> f32 {
    let longest_side = image_dimensions.0.max(image_dimensions.1);
    (longest_side / IMAGE_SIZE_BAND + 1) as f32
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
    let segments_per_curl = (perimeter / 2.5).clamp(48.0, 320.0).round() as usize;
    let base_tilt = random.range(-2.5_f32.to_radians(), 2.5_f32.to_radians());
    let tilt_drift = random.range(-2.0_f32.to_radians(), 2.0_f32.to_radians());
    let center_drift_x = random.range(-0.02, 0.02) * minimum_radius;
    let center_drift_y = random.range(-0.02, 0.02) * minimum_radius;
    let wander_phase = random.range(0.0, TAU);
    let crossing_phase = random.range(0.0, TAU);
    let half_gap_base = random.range(0.035, 0.045);
    let crossing_amplitude = random.range(0.052, 0.065);
    let harmonics = [2.0_f32, 3.0, 5.0].map(|frequency| {
        (
            frequency,
            random.range(0.008, 0.025) * minimum_radius,
            random.range(0.0, TAU),
        )
    });

    let mut points = Vec::with_capacity((segments_per_curl + 1) * 2);
    for pass in [-1.0_f32, 1.0] {
        for index in 0..=segments_per_curl {
            let progress = index as f32 / segments_per_curl as f32;
            let local_angle = progress * TAU;
            let angle = start + local_angle;
            let noise = harmonics
                .iter()
                .fold(0.0, |sum, (frequency, amplitude, phase)| {
                    sum + amplitude * (frequency * angle + phase).sin()
                });
            let shared_wander = 0.015 * (local_angle + wander_phase).sin();
            let half_gap =
                half_gap_base + crossing_amplitude * (local_angle * 2.0 + crossing_phase).sin();
            let scale = 1.0 + shared_wander + pass * half_gap;
            let radial_x = radius_x * scale + noise;
            let radial_y = radius_y * scale + noise;
            let x = radial_x * angle.cos();
            let y = radial_y * angle.sin();
            let tilt = base_tilt + tilt_drift * (progress - 0.5);
            let cos_tilt = tilt.cos();
            let sin_tilt = tilt.sin();
            points.push(Point {
                x: center.x + center_drift_x * (progress - 0.5) + x * cos_tilt - y * sin_tilt,
                y: center.y + center_drift_y * (progress - 0.5) + x * sin_tilt + y * cos_tilt,
            });
        }
    }
    points
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
    fn ellipse_is_deterministic_and_seeded() {
        assert_eq!(sloppy_ellipse(RECT, 4), sloppy_ellipse(RECT, 4));
        assert_ne!(sloppy_ellipse(RECT, 4), sloppy_ellipse(RECT, 5));
    }

    #[test]
    fn stroke_width_uses_longest_image_side_in_1024_pixel_bands() {
        assert_eq!(highlight_stroke_width((1, 1)), 1.0);
        assert_eq!(highlight_stroke_width((1023, 512)), 1.0);
        assert_eq!(highlight_stroke_width((1024, 512)), 2.0);
        assert_eq!(highlight_stroke_width((1200, 2047)), 2.0);
        assert_eq!(highlight_stroke_width((2048, 1200)), 3.0);
        assert_eq!(highlight_stroke_width((3071, 2500)), 3.0);
        assert_eq!(highlight_stroke_width((3072, 2500)), 4.0);
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
    fn ellipse_tails_stay_visually_related() {
        let minimum_radius = RECT.width.min(RECT.height) / 2.0;
        for seed in 0..64 {
            let points = sloppy_ellipse(RECT, seed);
            assert!(
                points[0].distance(*points.last().unwrap()) <= minimum_radius * 0.32,
                "seed {seed} separated the two curls too far"
            );
        }
    }

    #[test]
    fn ellipse_makes_two_separated_passes_that_cross() {
        let center = RECT.center();
        let radius_x = RECT.width / 2.0;
        let radius_y = RECT.height / 2.0;
        let minimum_radius = radius_x.min(radius_y);
        for seed in 0..64 {
            let points = sloppy_ellipse(RECT, seed);
            let start = points[0];
            let end = *points.last().unwrap();
            assert!(
                start.x < center.x && start.y < center.y,
                "seed {seed} did not start with an upper-left tail"
            );
            assert!(
                end.x < center.x && end.y < center.y,
                "seed {seed} did not finish its second curl near the start angle"
            );

            let mut previous =
                ((start.y - center.y) / radius_y).atan2((start.x - center.x) / radius_x);
            let mut sweep = 0.0;
            for point in points.iter().skip(1) {
                let angle =
                    ((point.y - center.y) / radius_y).atan2((point.x - center.x) / radius_x);
                let delta = (angle - previous + PI).rem_euclid(TAU) - PI;
                sweep += delta;
                previous = angle;
            }
            assert!(
                (sweep - TAU * 2.0).abs() < TAU * 0.1,
                "seed {seed} traced {sweep} radians instead of two curls"
            );

            let points_per_pass = points.len() / 2;
            let gaps = points[..points_per_pass]
                .iter()
                .zip(&points[points_per_pass..])
                .map(|(first, second)| first.distance(*second))
                .collect::<Vec<_>>();
            let average_gap = gaps.iter().sum::<f32>() / gaps.len() as f32;
            assert!(
                average_gap > minimum_radius * 0.06,
                "seed {seed} kept its passes cramped at {average_gap} pixels"
            );
            assert!(
                average_gap < minimum_radius * 0.22,
                "seed {seed} separated its passes excessively by {average_gap} pixels"
            );

            let radial_order = points[..points_per_pass]
                .iter()
                .zip(&points[points_per_pass..])
                .map(|(first, second)| second.distance(center) - first.distance(center))
                .collect::<Vec<_>>();
            let crossings = radial_order
                .windows(2)
                .filter(|pair| pair[0].signum() != pair[1].signum())
                .count();
            assert!(
                crossings >= 2,
                "seed {seed} produced only {crossings} pass crossings"
            );
        }
    }
}
