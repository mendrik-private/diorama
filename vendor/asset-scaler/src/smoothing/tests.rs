// Focused spline geometry tests live here so they can inspect private cubic
// diagnostics without widening the production API.
use super::*;
use crate::{CancellationToken, Error, contours::Contours, raster::Mask};
use std::sync::atomic::{AtomicUsize, Ordering};

fn fit(path: Vec<[f64; 2]>, locked: Vec<usize>, scale: f64) -> SplineFit {
    fit_paths([(7, path, locked)], scale, &CancellationToken::default()).unwrap()
}

fn close(a: [f64; 2], b: [f64; 2], epsilon: f64) -> bool {
    norm(sub(a, b)) <= epsilon
}

fn tangent_start(cubic: Cubic) -> [f64; 2] {
    sub(cubic[1], cubic[0])
}

fn tangent_end(cubic: Cubic) -> [f64; 2] {
    sub(cubic[3], cubic[2])
}

fn g1(left: Cubic, right: Cubic) -> bool {
    let a = unit(tangent_end(left));
    let b = unit(tangent_start(right));
    close(left[3], right[0], 1e-9) && (a[0] * b[1] - a[1] * b[0]).abs() < 1e-8 && dot2(a, b) > 0.999
}

fn sampled_polyline(cubics: &[Cubic], steps: usize) -> Vec<[f64; 2]> {
    let mut result = Vec::new();
    for &cubic in cubics {
        for i in 0..=steps {
            if result.is_empty() || i != 0 {
                result.push(cubic_point(cubic, i as f64 / steps as f64));
            }
        }
    }
    result
}

fn sampled_path(path: &[[f64; 2]], steps: usize) -> Vec<[f64; 2]> {
    let mut result = Vec::new();
    for edge in path.windows(2) {
        for i in 0..=steps {
            if result.is_empty() || i != 0 {
                result.push(add(
                    edge[0],
                    mul(sub(edge[1], edge[0]), i as f64 / steps as f64),
                ));
            }
        }
    }
    result
}

fn sine_path() -> Vec<[f64; 2]> {
    (0..=48)
        .map(|i| {
            let x = i as f64;
            [x, 5. * (x * std::f64::consts::PI / 12.).sin()]
        })
        .collect()
}

#[test]
fn least_squares_fit_is_translation_equivariant() {
    let path = sine_path();
    let translation = [31.25, -17.75];
    let translated = path.iter().map(|&p| add(p, translation)).collect();
    let original = fit(path, vec![], 1.);
    let moved = fit(translated, vec![], 1.);
    assert_eq!(original.cubic_owners, moved.cubic_owners);
    assert_eq!(original.cubic_curves.len(), moved.cubic_curves.len());
    for (curve, (a, b)) in original
        .cubic_curves
        .iter()
        .zip(&moved.cubic_curves)
        .enumerate()
    {
        for point in 0..4 {
            assert!(
                close(add(a[point], translation), b[point], 1e-8),
                "curve {curve}, control {point}: {:?} translated by {:?} differs from {:?}\noriginal={a:?}\nmoved={b:?}",
                a[point],
                translation,
                b[point]
            );
        }
    }
}

#[test]
fn smooth_s_curve_has_g1_at_every_internal_piece_join() {
    let fitted = fit(sine_path(), vec![], 1.);
    assert!(
        fitted.cubic_curves.len() >= 3,
        "fixture must exercise splits"
    );
    for pair in fitted.cubic_curves.windows(2) {
        assert!(g1(pair[0], pair[1]), "smooth recursive join lost G1");
    }
}

#[test]
fn smooth_closed_loop_seam_and_forced_anchors_are_g1() {
    let mut loop_path: Vec<_> = (0..32)
        .map(|i| {
            let t = i as f64 * std::f64::consts::TAU / 32.;
            [12. * t.cos(), 8. * t.sin()]
        })
        .collect();
    loop_path.push(loop_path[0]);
    let fitted = fit(loop_path, vec![8, 16, 24], 1.);
    assert!(fitted.cubic_curves.len() >= 4);
    for pair in fitted.cubic_curves.windows(2) {
        assert!(g1(pair[0], pair[1]), "loop anchor lost G1");
    }
    assert!(g1(
        *fitted.cubic_curves.last().unwrap(),
        fitted.cubic_curves[0]
    ));
}

#[test]
fn hard_l_corner_is_anchored_with_distinct_tangents() {
    let path = vec![
        [0., 0.],
        [1., 0.],
        [2., 0.],
        [3., 0.],
        [4., 0.],
        [5., 0.],
        [5., 1.],
        [5., 2.],
        [5., 3.],
        [5., 4.],
        [5., 5.],
    ];
    let fitted = fit(path, vec![], 1.);
    let corner = [5., 0.];
    let join = fitted
        .cubic_curves
        .windows(2)
        .find(|pair| close(pair[0][3], corner, 1e-9) && close(pair[1][0], corner, 1e-9))
        .expect("hard corner must remain an exact fit anchor");
    let a = unit(tangent_end(join[0]));
    let b = unit(tangent_start(join[1]));
    assert!(a[0] * b[1] - a[1] * b[0] > 0.9, "corner was smoothed");
}

#[test]
fn locked_t_junction_anchor_is_never_moved() {
    let path: Vec<_> = (0..=12).map(|x| [x as f64, 4.]).collect();
    let fitted = fit(path, vec![6], 1.);
    let junction = [6., 4.];
    assert!(
        fitted
            .cubic_curves
            .iter()
            .any(|cubic| close(cubic[0], junction, 1e-9) || close(cubic[3], junction, 1e-9))
    );
}

#[test]
fn locked_middle_of_a_three_point_path_is_an_exact_anchor() {
    let fitted = fit(vec![[0., 0.], [1., 0.5], [2., 0.]], vec![1], 1.);
    assert!(
        fitted
            .cubic_curves
            .iter()
            .any(|cubic| { close(cubic[0], [1., 0.5], 1e-9) || close(cubic[3], [1., 0.5], 1e-9) })
    );
}

#[test]
fn closed_square_preserves_the_hard_corner_at_its_seam() {
    let path = vec![
        [0., 0.],
        [2., 0.],
        [4., 0.],
        [4., 2.],
        [4., 4.],
        [2., 4.],
        [0., 4.],
        [0., 2.],
        [0., 0.],
    ];
    let fitted = fit(path, vec![], 1.);
    let incoming = unit(tangent_end(*fitted.cubic_curves.last().unwrap()));
    let outgoing = unit(tangent_start(fitted.cubic_curves[0]));
    assert!(
        (incoming[0] * outgoing[1] - incoming[1] * outgoing[0]).abs() > 0.7,
        "the closing hard corner was incorrectly made G1"
    );
}

#[test]
fn dense_bidirectional_deviation_stays_bounded_at_anisotropic_target_scale() {
    let path: Vec<_> = (0..=40)
        .map(|i| {
            let t = i as f64 * std::f64::consts::PI / 40.;
            [18. * t.cos(), 7. * t.sin()]
        })
        .collect();
    // Callers pass the least-reduced axis to `fit_paths`; [1, 0.25] must use
    // scale 1 so a source-space deviation cannot become a full target pixel.
    let fitted = fit(path.clone(), vec![], 1.);
    let samples = sampled_polyline(&fitted.cubic_curves, 96);
    let dense_source = sampled_path(&path, 20);
    let source_to_fit = dense_source
        .iter()
        .map(|&p| distance_to_polyline(p, &samples))
        .fold(0., f64::max);
    let fit_to_source = samples
        .iter()
        .map(|&p| distance_to_polyline(p, &path))
        .fold(0., f64::max);
    assert!(
        source_to_fit <= 0.8,
        "source vertices drifted {source_to_fit}"
    );
    assert!(
        fit_to_source <= 0.8,
        "between-vertex fit bulged {fit_to_source}"
    );
    let target_samples: Vec<_> = samples.iter().map(|&p| [p[0], p[1] * 0.25]).collect();
    let target_source: Vec<_> = dense_source.iter().map(|&p| [p[0], p[1] * 0.25]).collect();
    let anisotropic_target_error = target_samples
        .iter()
        .map(|&p| distance_to_polyline(p, &target_source))
        .fold(0., f64::max);
    assert!(anisotropic_target_error <= 0.8);
}

#[test]
fn contour_caller_uses_the_largest_axis_for_anisotropic_scales() {
    let mut mask = Mask::new(48, 32);
    for x in 4..44 {
        let y = (15. + 3. * ((x as f64 - 4.) * std::f64::consts::PI / 16.).sin()).round() as usize;
        mask.data[y * mask.w + x] = true;
    }
    let cancel = CancellationToken::default();
    let contours = Contours::new(&mask, &[], &cancel).unwrap();
    let isotropic = contours.polished([1., 1.], 0, &cancel).unwrap();
    let anisotropic = contours.polished([1., 0.25], 0, &cancel).unwrap();
    assert_eq!(isotropic.cubic_curves.len(), anisotropic.cubic_curves.len());
    for (a, b) in isotropic.cubic_curves.iter().zip(&anisotropic.cubic_curves) {
        for point in 0..4 {
            assert!(close(a[point], b[point], 1e-9));
        }
    }
}

#[test]
fn noisy_arc_uses_substantially_fewer_cubics_than_input_edges() {
    let path: Vec<_> = (0..=64)
        .map(|i| {
            let t = i as f64 * std::f64::consts::PI / 64.;
            let noise = match i % 3 {
                0 => -0.12,
                1 => 0.,
                _ => 0.12,
            };
            let radius = 20. + noise;
            [radius * t.cos(), radius * t.sin()]
        })
        .collect();
    let fitted = fit(path.clone(), vec![], 0.5);
    assert!(fitted.cubic_curves.len() * 4 < path.len());
}

#[test]
fn long_fit_observes_cancellation_and_two_point_fit_does_not_overshoot() {
    let long: Vec<_> = (0..20_000)
        .map(|i| [i as f64 * 0.05, (i as f64 * 0.03).sin()])
        .collect();
    let checks = AtomicUsize::new(0);
    let cancel = || checks.fetch_add(1, Ordering::Relaxed) >= 3;
    assert!(matches!(
        fit_paths([(0, long, vec![])], 1., &cancel),
        Err(Error::Cancelled)
    ));

    let fitted = fit(vec![[2., 3.], [12., 3.]], vec![], 1.);
    assert_eq!(fitted.cubic_curves.len(), 1);
    let cubic = fitted.cubic_curves[0];
    for i in 0..=100 {
        let p = cubic_point(cubic, i as f64 / 100.);
        assert!((p[1] - 3.).abs() < 1e-9);
        assert!((2. - 1e-9..=12. + 1e-9).contains(&p[0]));
    }
}

#[test]
fn opposing_inherited_two_point_tangents_contract_within_the_reported_bound() {
    let path = [[0., 0.], [10., 0.]];
    let tolerance = 0.65;
    let start_tangent = [0., 1.];
    let end_tangent = [0., -1.];
    let mut cubics = Vec::new();
    let mut reported_error = 0.;
    fit_span(
        &path,
        tolerance,
        &CancellationToken::default(),
        start_tangent,
        end_tangent,
        &mut cubics,
        &mut reported_error,
    )
    .unwrap();
    assert_eq!(cubics.len(), 1);
    assert!(reported_error <= tolerance);
    let cubic = cubics[0];
    assert!(dot2(unit(tangent_start(cubic)), start_tangent) > 0.999);
    assert!(dot2(unit(tangent_end(cubic)), end_tangent) > 0.999);
    let dense_error = (0..=1024)
        .map(|i| cubic_point(cubic, i as f64 / 1024.))
        .map(|point| distance_to_segment(point, path[0], path[1]))
        .fold(0., f64::max);
    assert!(
        dense_error <= tolerance,
        "two-point leaf deviated {dense_error}, despite reported {reported_error}"
    );
}

#[test]
fn shallow_raster_staircase_is_one_smooth_curve() {
    // A thinned digital line of slope 1/4 alternates flat runs and steps.
    // Neither the steps nor the split tangents may introduce corners.
    let mut mask = Mask::new(64, 24);
    for x in 4..60 {
        mask.data[(4 + (x - 4) / 4) * mask.w + x] = true;
    }
    let cancel = CancellationToken::default();
    let contours = Contours::new(&mask, &[], &cancel).unwrap();
    let fitted = contours.polished([1., 1.], 0, &cancel).unwrap();
    assert!(
        fitted.cubic_curves.len() <= 2,
        "staircase needed {} cubics",
        fitted.cubic_curves.len()
    );
    for pair in fitted.cubic_curves.windows(2) {
        assert!(g1(pair[0], pair[1]), "staircase join lost G1");
    }
    let direction = unit(sub(
        fitted.cubic_curves.last().unwrap()[3],
        fitted.cubic_curves[0][0],
    ));
    for cubic in &fitted.cubic_curves {
        for tangent in [tangent_start(*cubic), tangent_end(*cubic)] {
            assert!(
                dot2(unit(tangent), direction) > 0.97,
                "tangent follows a raster step"
            );
        }
    }
    assert!(fitted.max_error <= 1.0);
}
