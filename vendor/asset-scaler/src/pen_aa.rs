//! The editor pen's subpixel edge profile, shared with Game Asset contours.

/// Returns the opacity at `distance` from a circular brush centreline.
///
/// Pixel centres inside the hard core are fully covered. The half-pixel
/// exterior fringe follows the pen's subdued cubic response, so neighbouring
/// segments combine by taking their maximum coverage.
pub fn coverage_for_distance(distance: f64, width: f64, hardness: f64) -> f64 {
    if !distance.is_finite() || !width.is_finite() || width <= 0. {
        return 0.;
    }
    let radius = width * 0.5;
    let outer_radius = radius + 0.5;
    let hard_radius = (radius - 0.5).max(0.) * hardness.clamp(0., 1.);
    let linear = if distance <= hard_radius {
        1.
    } else {
        ((outer_radius - distance) / (outer_radius - hard_radius)).clamp(0., 1.)
    };
    linear * linear * (2. - linear)
}

/// `f32` adapter preserving the editor's established pen arithmetic.
pub fn coverage_for_distance_f32(distance: f32, width: f32, hardness: f32) -> f32 {
    if !distance.is_finite() || !width.is_finite() || width <= 0. {
        return 0.;
    }
    let radius = width * 0.5;
    let outer_radius = radius + 0.5;
    let hard_radius = (radius - 0.5).max(0.) * hardness.clamp(0., 1.);
    let linear = if distance <= hard_radius {
        1.
    } else {
        ((outer_radius - distance) / (outer_radius - hard_radius)).clamp(0., 1.)
    };
    linear * linear * (2. - linear)
}

#[cfg(test)]
mod tests {
    use super::{coverage_for_distance, coverage_for_distance_f32};

    #[test]
    fn unit_pen_has_a_hard_center_and_subdued_one_pixel_fringe() {
        assert_eq!(coverage_for_distance(0., 1., 0.), 1.);
        assert_eq!(coverage_for_distance(1., 1., 0.), 0.);
        assert_eq!(coverage_for_distance(0.5, 1., 0.), 0.375);
    }

    #[test]
    fn f32_adapter_matches_the_pen_profile_byte_for_byte() {
        for (distance, width, hardness) in [
            (0., 1., 1.),
            (0.5, 1., 1.),
            (0.75, 2.5, 0.25),
            (1.25, 4.75, 0.8),
        ] {
            let radius = width * 0.5_f32;
            let outer_radius = radius + 0.5;
            let hard_radius = (radius - 0.5).max(0.) * hardness;
            let linear = if distance <= hard_radius {
                1.
            } else {
                ((outer_radius - distance) / (outer_radius - hard_radius)).clamp(0., 1.)
            };
            let expected = linear * linear * (2. - linear);
            assert_eq!(
                coverage_for_distance_f32(distance, width, hardness),
                expected
            );
        }
    }
}
