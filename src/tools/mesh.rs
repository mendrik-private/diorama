//! Raster warping driven by freely placed mesh control points.
//!
//! Callers supply corresponding source and destination points in image pixel-centre
//! coordinates. The four image corners are added internally as fixed anchors, so a
//! single control point can deform the entire image while the canvas stays put.

use image::{Rgba, RgbaImage};

use crate::{
    document::{CancellationToken, Point},
    error::{AppError, Result},
    tools::canvas_resize::background,
};

const EPSILON: f64 = 1.0e-8;

/// Returns whether points can be used as mesh controls for an image of this size.
///
/// At least one user control is required. Controls use pixel-centre coordinates and
/// may be unordered, collinear, or lie on an image edge, but cannot overlap another
/// control or one of the internally fixed corner anchors.
#[must_use]
pub fn valid_points(width: u32, height: u32, points: &[Point]) -> bool {
    if width < 2 || height < 2 || points.is_empty() {
        return false;
    }
    let maximum_x = (width - 1) as f32;
    let maximum_y = (height - 1) as f32;
    let anchors = corners(width, height);
    points.iter().enumerate().all(|(index, point)| {
        finite(*point)
            && point.x >= 0.0
            && point.y >= 0.0
            && point.x <= maximum_x
            && point.y <= maximum_y
            && !anchors
                .iter()
                .chain(points[..index].iter())
                .any(|other| distance_squared(*point, *other) <= EPSILON * EPSILON)
    })
}

/// Returns whether corresponding controls preserve every source mesh triangle.
///
/// The source triangulation is used for both states. This makes its connectivity
/// stable while a caller drags a destination control and rejects folds or collapsed
/// triangles before rasterization.
#[must_use]
pub fn valid_deformation(width: u32, height: u32, source: &[Point], target: &[Point]) -> bool {
    if source.len() != target.len()
        || !valid_points(width, height, source)
        || !valid_points(width, height, target)
    {
        return false;
    }
    let nodes = nodes(width, height, source, target);
    let Ok(Some(triangles)) = triangulate(&nodes, None) else {
        return false;
    };
    deformation_preserves_triangles(&nodes, &triangles)
}

/// Inversely rasterizes a piecewise-affine mesh deformation.
///
/// The result has the original dimensions. Every pixel is sampled from the immutable
/// input, using premultiplied bilinear filtering; pixels which cannot be covered or
/// sampled use the canvas background colour.
pub fn warp(
    image: &RgbaImage,
    source: &[Point],
    target: &[Point],
    cancellation: &CancellationToken,
) -> Result<RgbaImage> {
    cancellation.check()?;
    let (width, height) = image.dimensions();
    if source.len() != target.len()
        || !valid_points(width, height, source)
        || !valid_points(width, height, target)
    {
        return Err(AppError::InvalidDimensions);
    }
    let nodes = nodes(width, height, source, target);
    let triangles = triangulate(&nodes, Some(cancellation))?.ok_or(AppError::InvalidDimensions)?;
    if !deformation_preserves_triangles(&nodes, &triangles) {
        return Err(AppError::InvalidDimensions);
    }
    if source == target {
        return Ok(image.clone());
    }
    let fill = background(image);
    let mut output = RgbaImage::from_pixel(width, height, Rgba(fill));
    for triangle in triangles {
        cancellation.check()?;
        rasterize_triangle(&mut output, image, &nodes, triangle, fill, cancellation)?;
    }
    // A transparent corner may carry hidden RGB. The anchors promise exact source
    // corner pixels, so preserve those bytes rather than normalizing through the
    // premultiplied sampler.
    for &(x, y) in &[
        (0, 0),
        (width - 1, 0),
        (width - 1, height - 1),
        (0, height - 1),
    ] {
        output.put_pixel(x, y, *image.get_pixel(x, y));
    }
    Ok(output)
}

#[derive(Clone, Copy)]
struct Node {
    source: Point,
    target: Point,
}

#[derive(Clone, Copy)]
struct Triangle([usize; 3]);

fn corners(width: u32, height: u32) -> [Point; 4] {
    let right = (width - 1) as f32;
    let bottom = (height - 1) as f32;
    [
        Point { x: 0.0, y: 0.0 },
        Point { x: right, y: 0.0 },
        Point {
            x: right,
            y: bottom,
        },
        Point { x: 0.0, y: bottom },
    ]
}

fn nodes(width: u32, height: u32, source: &[Point], target: &[Point]) -> Vec<Node> {
    let mut nodes: Vec<_> = source
        .iter()
        .zip(target)
        .map(|(&source, &target)| Node { source, target })
        .collect();
    nodes.extend(corners(width, height).map(|point| Node {
        source: point,
        target: point,
    }));
    // The insertion order controls Delaunay's choice for co-circular points. Sort by
    // source position so an unordered user set always produces the same connectivity.
    nodes.sort_by(|left, right| {
        left.source.x.total_cmp(&right.source.x).then_with(|| {
            left.source
                .y
                .total_cmp(&right.source.y)
                .then_with(|| left.target.x.total_cmp(&right.target.x))
                .then_with(|| left.target.y.total_cmp(&right.target.y))
        })
    });
    nodes
}

/// Deterministic Bowyer-Watson triangulation of the source controls and anchors.
fn triangulate(
    nodes: &[Node],
    cancellation: Option<&CancellationToken>,
) -> Result<Option<Vec<Triangle>>> {
    if nodes.len() < 4 {
        return Ok(None);
    }
    let mut positions: Vec<Point> = nodes.iter().map(|node| node.source).collect();
    let mut minimum_x = f64::INFINITY;
    let mut maximum_x = f64::NEG_INFINITY;
    let mut minimum_y = f64::INFINITY;
    let mut maximum_y = f64::NEG_INFINITY;
    for point in &positions {
        minimum_x = minimum_x.min(f64::from(point.x));
        maximum_x = maximum_x.max(f64::from(point.x));
        minimum_y = minimum_y.min(f64::from(point.y));
        maximum_y = maximum_y.max(f64::from(point.y));
    }
    let span = (maximum_x - minimum_x).max(maximum_y - minimum_y);
    if !span.is_finite() || span <= EPSILON {
        return Ok(None);
    }
    let center_x = (minimum_x + maximum_x) * 0.5;
    let center_y = (minimum_y + maximum_y) * 0.5;
    let scale = span * 32.0;
    let first_super = positions.len();
    positions.extend([
        Point {
            x: (center_x - scale) as f32,
            y: (center_y - scale) as f32,
        },
        Point {
            x: center_x as f32,
            y: (center_y + scale) as f32,
        },
        Point {
            x: (center_x + scale) as f32,
            y: (center_y - scale) as f32,
        },
    ]);
    let Some(super_triangle) = ccw_triangle(
        &positions,
        Triangle([first_super, first_super + 1, first_super + 2]),
    ) else {
        return Ok(None);
    };
    let mut triangles = vec![super_triangle];

    for point in 0..nodes.len() {
        if let Some(cancellation) = cancellation {
            cancellation.check()?;
        }
        let bad: Vec<_> = triangles
            .iter()
            .copied()
            .filter(|triangle| circumcircle_contains(&positions, *triangle, point))
            .collect();
        if bad.is_empty() {
            return Ok(None);
        }
        let mut boundary = Vec::new();
        for triangle in &bad {
            for edge in triangle_edges(*triangle) {
                let occurrences = bad
                    .iter()
                    .flat_map(|candidate| triangle_edges(*candidate))
                    .filter(|candidate| *candidate == edge)
                    .count();
                if occurrences == 1 {
                    boundary.push(edge);
                }
            }
        }
        triangles.retain(|triangle| !bad.iter().any(|bad_triangle| bad_triangle.0 == triangle.0));
        for (first, second) in boundary {
            if let Some(triangle) = ccw_triangle(&positions, Triangle([first, second, point])) {
                triangles.push(triangle);
            }
        }
    }
    triangles.retain(|triangle| triangle.0.iter().all(|index| *index < nodes.len()));
    if !source_mesh_is_complete(nodes, &triangles) {
        triangles = incremental_triangulation(nodes, cancellation)?;
    }
    if !source_mesh_is_complete(nodes, &triangles) {
        return Ok(None);
    }
    Ok((!triangles.is_empty()).then_some(triangles))
}

/// Inserts controls into a pair of fixed rectangle triangles. This is used only
/// when floating-point Delaunay cavity selection leaves a hull sliver uncovered.
/// It is deterministic, keeps every source control, and preserves the anchor
/// rectangle exactly.
fn incremental_triangulation(
    nodes: &[Node],
    cancellation: Option<&CancellationToken>,
) -> Result<Vec<Triangle>> {
    let source: Vec<_> = nodes.iter().map(|node| node.source).collect();
    let left = nodes
        .iter()
        .map(|node| node.source.x)
        .fold(f32::INFINITY, f32::min);
    let right = nodes
        .iter()
        .map(|node| node.source.x)
        .fold(f32::NEG_INFINITY, f32::max);
    let top = nodes
        .iter()
        .map(|node| node.source.y)
        .fold(f32::INFINITY, f32::min);
    let bottom = nodes
        .iter()
        .map(|node| node.source.y)
        .fold(f32::NEG_INFINITY, f32::max);
    let corner_index = |x, y| {
        nodes
            .iter()
            .position(|node| node.source.x == x && node.source.y == y)
    };
    let Some(top_left) = corner_index(left, top) else {
        return Ok(Vec::new());
    };
    let Some(top_right) = corner_index(right, top) else {
        return Ok(Vec::new());
    };
    let Some(bottom_right) = corner_index(right, bottom) else {
        return Ok(Vec::new());
    };
    let Some(bottom_left) = corner_index(left, bottom) else {
        return Ok(Vec::new());
    };
    let Some(first) = ccw_triangle(&source, Triangle([top_left, top_right, bottom_right])) else {
        return Ok(Vec::new());
    };
    let Some(second) = ccw_triangle(&source, Triangle([top_left, bottom_right, bottom_left]))
    else {
        return Ok(Vec::new());
    };
    let corners = [top_left, top_right, bottom_right, bottom_left];
    let mut triangles = vec![first, second];
    for point in 0..nodes.len() {
        if corners.contains(&point) {
            continue;
        }
        if let Some(cancellation) = cancellation {
            cancellation.check()?;
        }
        let mut next = Vec::with_capacity(triangles.len() + 4);
        let mut inserted = false;
        for triangle in triangles {
            let [first, second, third] = triangle.0;
            let point_position = nodes[point].source;
            let edges = [
                (first, second, third),
                (second, third, first),
                (third, first, second),
            ];
            let Some(edge) = edges.iter().find(|(start, end, _)| {
                orientation(nodes[*start].source, nodes[*end].source, point_position).abs()
                    <= EPSILON
            }) else {
                if point_in_triangle(point_position, nodes, triangle) {
                    for (start, end) in [(first, second), (second, third), (third, first)] {
                        let candidate = ccw_triangle(&source, Triangle([start, end, point]));
                        let Some(candidate) = candidate else {
                            return Ok(Vec::new());
                        };
                        next.push(candidate);
                    }
                    inserted = true;
                } else {
                    next.push(triangle);
                }
                continue;
            };
            if !point_in_triangle(point_position, nodes, triangle) {
                next.push(triangle);
                continue;
            }
            let &(start, end, opposite) = edge;
            for candidate in [
                Triangle([start, point, opposite]),
                Triangle([point, end, opposite]),
            ] {
                let Some(candidate) = ccw_triangle(&source, candidate) else {
                    return Ok(Vec::new());
                };
                next.push(candidate);
            }
            inserted = true;
        }
        if !inserted {
            return Ok(Vec::new());
        }
        triangles = next;
    }
    Ok(triangles)
}

fn ccw_triangle(points: &[Point], mut triangle: Triangle) -> Option<Triangle> {
    let [first, second, third] = triangle.0;
    let area = orientation(points[first], points[second], points[third]);
    if area.abs() <= EPSILON {
        return None;
    }
    if area < 0.0 {
        triangle.0.swap(1, 2);
    }
    Some(triangle)
}

fn point_in_triangle(point: Point, nodes: &[Node], triangle: Triangle) -> bool {
    let [first, second, third] = triangle.0;
    orientation(nodes[first].source, nodes[second].source, point) >= -EPSILON
        && orientation(nodes[second].source, nodes[third].source, point) >= -EPSILON
        && orientation(nodes[third].source, nodes[first].source, point) >= -EPSILON
}

fn triangle_edges(triangle: Triangle) -> [(usize, usize); 3] {
    let [first, second, third] = triangle.0;
    [
        ordered_edge(first, second),
        ordered_edge(second, third),
        ordered_edge(third, first),
    ]
}

fn ordered_edge(first: usize, second: usize) -> (usize, usize) {
    if first < second {
        (first, second)
    } else {
        (second, first)
    }
}

fn circumcircle_contains(points: &[Point], triangle: Triangle, point: usize) -> bool {
    let [first, second, third] = triangle.0;
    let point = points[point];
    let ax = f64::from(points[first].x) - f64::from(point.x);
    let ay = f64::from(points[first].y) - f64::from(point.y);
    let bx = f64::from(points[second].x) - f64::from(point.x);
    let by = f64::from(points[second].y) - f64::from(point.y);
    let cx = f64::from(points[third].x) - f64::from(point.x);
    let cy = f64::from(points[third].y) - f64::from(point.y);
    let determinant = (ax * ax + ay * ay) * (bx * cy - by * cx)
        - (bx * bx + by * by) * (ax * cy - ay * cx)
        + (cx * cx + cy * cy) * (ax * by - ay * bx);
    determinant > EPSILON
}

fn rasterize_triangle(
    output: &mut RgbaImage,
    image: &RgbaImage,
    nodes: &[Node],
    triangle: Triangle,
    fill: [u8; 4],
    cancellation: &CancellationToken,
) -> Result<()> {
    let [first, second, third] = triangle.0;
    let target = [
        nodes[first].target,
        nodes[second].target,
        nodes[third].target,
    ];
    let Some((left, top, right, bottom)) = pixel_bounds(&target, output.width(), output.height())
    else {
        return Ok(());
    };
    let area = orientation(target[0], target[1], target[2]);
    if area <= EPSILON {
        return Err(AppError::InvalidDimensions);
    }
    for y in top..=bottom {
        cancellation.check()?;
        for x in left..=right {
            let point = Point {
                x: x as f32,
                y: y as f32,
            };
            let first_weight = orientation(target[1], target[2], point) / area;
            let second_weight = orientation(target[2], target[0], point) / area;
            let third_weight = 1.0 - first_weight - second_weight;
            if first_weight < -EPSILON || second_weight < -EPSILON || third_weight < -EPSILON {
                continue;
            }
            let source = Point {
                x: (f64::from(nodes[first].source.x) * first_weight
                    + f64::from(nodes[second].source.x) * second_weight
                    + f64::from(nodes[third].source.x) * third_weight) as f32,
                y: (f64::from(nodes[first].source.y) * first_weight
                    + f64::from(nodes[second].source.y) * second_weight
                    + f64::from(nodes[third].source.y) * third_weight) as f32,
            };
            output.put_pixel(x, y, Rgba(bilinear_premultiplied(image, source, fill)));
        }
    }
    Ok(())
}

fn bilinear_premultiplied(image: &RgbaImage, point: Point, fill: [u8; 4]) -> [u8; 4] {
    if !finite(point) {
        return fill;
    }
    let left = point.x.floor() as i64;
    let top = point.y.floor() as i64;
    let horizontal = f64::from(point.x) - left as f64;
    let vertical = f64::from(point.y) - top as f64;
    let upper_left = premultiplied_pixel(image, left, top, fill);
    let upper_right = premultiplied_pixel(image, left + 1, top, fill);
    let lower_left = premultiplied_pixel(image, left, top + 1, fill);
    let lower_right = premultiplied_pixel(image, left + 1, top + 1, fill);
    let mut result = [0.0; 4];
    for channel in 0..4 {
        result[channel] = upper_left[channel] * (1.0 - horizontal) * (1.0 - vertical)
            + upper_right[channel] * horizontal * (1.0 - vertical)
            + lower_left[channel] * (1.0 - horizontal) * vertical
            + lower_right[channel] * horizontal * vertical;
    }
    if result[3] <= 0.0 {
        return [0; 4];
    }
    let alpha = result[3].clamp(0.0, 1.0);
    let mut rgba = [0; 4];
    for channel in 0..3 {
        rgba[channel] = (result[channel] / alpha * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    rgba[3] = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
    rgba
}

fn premultiplied_pixel(image: &RgbaImage, x: i64, y: i64, fill: [u8; 4]) -> [f64; 4] {
    let pixel = if x < 0 || y < 0 || x >= i64::from(image.width()) || y >= i64::from(image.height())
    {
        fill
    } else {
        image.get_pixel(x as u32, y as u32).0
    };
    let alpha = f64::from(pixel[3]) / 255.0;
    [
        f64::from(pixel[0]) / 255.0 * alpha,
        f64::from(pixel[1]) / 255.0 * alpha,
        f64::from(pixel[2]) / 255.0 * alpha,
        alpha,
    ]
}

fn pixel_bounds(points: &[Point], width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
    if width == 0 || height == 0 {
        return None;
    }
    let minimum_x = points
        .iter()
        .map(|point| point.x)
        .fold(f32::INFINITY, f32::min);
    let minimum_y = points
        .iter()
        .map(|point| point.y)
        .fold(f32::INFINITY, f32::min);
    let maximum_x = points
        .iter()
        .map(|point| point.x)
        .fold(f32::NEG_INFINITY, f32::max);
    let maximum_y = points
        .iter()
        .map(|point| point.y)
        .fold(f32::NEG_INFINITY, f32::max);
    let left = minimum_x.ceil().max(0.0) as u32;
    let top = minimum_y.ceil().max(0.0) as u32;
    let right = maximum_x.floor().min((width - 1) as f32) as u32;
    let bottom = maximum_y.floor().min((height - 1) as f32) as u32;
    (left <= right && top <= bottom).then_some((left, top, right, bottom))
}

fn triangle_area(nodes: &[Node], triangle: Triangle, point: impl Fn(Node) -> Point) -> f64 {
    let [first, second, third] = triangle.0;
    orientation(
        point(nodes[first]),
        point(nodes[second]),
        point(nodes[third]),
    )
}

fn deformation_preserves_triangles(nodes: &[Node], triangles: &[Triangle]) -> bool {
    triangles.iter().all(|triangle| {
        let source_area = triangle_area(nodes, *triangle, |node| node.source);
        let target_area = triangle_area(nodes, *triangle, |node| node.target);
        source_area.abs() > EPSILON
            && target_area.abs() > EPSILON
            && source_area.signum() == target_area.signum()
    })
}

fn source_mesh_is_complete(nodes: &[Node], triangles: &[Triangle]) -> bool {
    if triangles.is_empty() {
        return false;
    }
    let mut referenced = vec![false; nodes.len()];
    let doubled_area: f64 = triangles
        .iter()
        .map(|triangle| {
            for index in triangle.0 {
                referenced[index] = true;
            }
            triangle_area(nodes, *triangle, |node| node.source)
        })
        .sum();
    if referenced.iter().any(|referenced| !referenced) {
        return false;
    }
    let minimum_x = nodes
        .iter()
        .map(|node| f64::from(node.source.x))
        .fold(f64::INFINITY, f64::min);
    let maximum_x = nodes
        .iter()
        .map(|node| f64::from(node.source.x))
        .fold(f64::NEG_INFINITY, f64::max);
    let minimum_y = nodes
        .iter()
        .map(|node| f64::from(node.source.y))
        .fold(f64::INFINITY, f64::min);
    let maximum_y = nodes
        .iter()
        .map(|node| f64::from(node.source.y))
        .fold(f64::NEG_INFINITY, f64::max);
    let expected = 2.0 * (maximum_x - minimum_x) * (maximum_y - minimum_y);
    (doubled_area - expected).abs() <= expected.max(1.0) * 1.0e-6
}

fn orientation(first: Point, second: Point, third: Point) -> f64 {
    (f64::from(second.x) - f64::from(first.x)) * (f64::from(third.y) - f64::from(first.y))
        - (f64::from(second.y) - f64::from(first.y)) * (f64::from(third.x) - f64::from(first.x))
}

fn distance_squared(first: Point, second: Point) -> f64 {
    let x = f64::from(first.x - second.x);
    let y = f64::from(first.y - second.y);
    x * x + y * y
}

fn finite(point: Point) -> bool {
    point.x.is_finite() && point.y.is_finite()
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::{valid_deformation, valid_points, warp};
    use crate::{
        document::{CancellationToken, Point},
        error::AppError,
    };

    fn point(x: f32, y: f32) -> Point {
        Point { x, y }
    }

    #[test]
    fn identity_preserves_every_pixel_exactly() {
        let image = RgbaImage::from_fn(8, 7, |x, y| {
            Rgba([x as u8 * 19, y as u8 * 23, 11, (x * 17 + y * 13) as u8])
        });
        let controls = [point(3.0, 3.0), point(1.0, 5.0)];
        assert_eq!(
            warp(&image, &controls, &controls, &CancellationToken::default()).unwrap(),
            image
        );
    }

    #[test]
    fn a_single_control_warps_its_neighbourhood_and_pins_the_control_and_corners() {
        let image = RgbaImage::from_fn(9, 9, |x, y| Rgba([x as u8 * 20, y as u8 * 20, 5, 255]));
        let warped = warp(
            &image,
            &[point(4.0, 4.0)],
            &[point(6.0, 4.0)],
            &CancellationToken::default(),
        )
        .unwrap();
        assert_eq!(*warped.get_pixel(6, 4), *image.get_pixel(4, 4));
        assert_ne!(*warped.get_pixel(5, 4), *image.get_pixel(5, 4));
        for &(x, y) in &[(0, 0), (8, 0), (8, 8), (0, 8)] {
            assert_eq!(*warped.get_pixel(x, y), *image.get_pixel(x, y));
        }
    }

    #[test]
    fn unordered_collinear_and_edge_controls_are_supported() {
        let image = RgbaImage::from_fn(10, 8, |x, y| Rgba([x as u8 * 17, y as u8 * 23, 60, 255]));
        let source = [
            point(8.0, 3.0),
            point(5.0, 0.0),
            point(2.0, 3.0),
            point(6.0, 3.0),
        ];
        let target = [
            point(7.0, 3.0),
            point(5.0, 1.0),
            point(3.0, 3.0),
            point(6.0, 4.0),
        ];
        assert!(valid_deformation(10, 8, &source, &target));
        let warped = warp(&image, &source, &target, &CancellationToken::default()).unwrap();
        let reversed_source: Vec<_> = source.into_iter().rev().collect();
        let reversed_target: Vec<_> = target.into_iter().rev().collect();
        assert_eq!(
            warp(
                &image,
                &reversed_source,
                &reversed_target,
                &CancellationToken::default()
            )
            .unwrap(),
            warped
        );
    }

    #[test]
    fn rejects_duplicates_out_of_bounds_and_folds() {
        assert!(!valid_points(8, 8, &[point(1.0, 1.0), point(1.0, 1.0)]));
        assert!(!valid_points(8, 8, &[point(0.0, 0.0)]));
        assert!(!valid_points(8, 8, &[point(-0.1, 2.0)]));
        let source = [point(2.0, 2.0)];
        let target = [point(7.0, 7.0)];
        assert!(!valid_deformation(8, 8, &source, &target));
        assert!(matches!(
            warp(
                &RgbaImage::new(8, 8),
                &source,
                &target,
                &CancellationToken::default()
            ),
            Err(AppError::InvalidDimensions)
        ));
        let source = [point(2.0, 2.0), point(5.0, 5.0)];
        let crossed = [point(5.0, 5.0), point(2.0, 2.0)];
        assert!(valid_points(8, 8, &crossed));
        assert!(
            !valid_deformation(8, 8, &source, &crossed),
            "crossing interior controls must be rejected as a fold"
        );
    }

    #[test]
    fn filtering_uses_premultiplied_alpha_and_does_not_mutate_the_input() {
        let mut image = RgbaImage::from_pixel(8, 6, Rgba([0, 0, 0, 0]));
        image.put_pixel(3, 3, Rgba([255, 0, 0, 0]));
        image.put_pixel(4, 3, Rgba([0, 0, 255, 255]));
        let original = image.clone();
        let warped = warp(
            &image,
            &[point(3.0, 3.0)],
            &[point(3.5, 3.0)],
            &CancellationToken::default(),
        )
        .unwrap();
        assert_eq!(image, original);
        let pixel = warped.get_pixel(4, 3);
        assert!(
            pixel[0] < 20,
            "hidden transparent red contaminated the sample"
        );
        assert!(pixel[2] > 100, "visible blue did not survive filtering");
    }

    #[test]
    fn cancellation_is_checked_before_identity() {
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        assert!(matches!(
            warp(
                &RgbaImage::new(4, 4),
                &[point(2.0, 2.0)],
                &[point(2.0, 2.0)],
                &cancellation
            ),
            Err(AppError::Cancelled)
        ));
    }

    #[test]
    fn boundary_motion_covers_the_whole_canvas_without_alpha_inflation() {
        let image = RgbaImage::from_pixel(9, 7, Rgba([0, 0, 255, 128]));
        let warped = warp(
            &image,
            &[point(4.0, 0.0)],
            &[point(4.0, 1.0)],
            &CancellationToken::default(),
        )
        .unwrap();
        assert!(
            warped
                .pixels()
                .all(|pixel| *pixel == Rgba([0, 0, 255, 128]))
        );
    }

    #[test]
    fn boundary_control_moves_foreground_without_leaving_the_old_pixel_behind() {
        let background = Rgba([12, 34, 56, 255]);
        let foreground = Rgba([230, 20, 80, 255]);
        let mut image = RgbaImage::from_pixel(9, 7, background);
        image.put_pixel(4, 0, foreground);
        let warped = warp(
            &image,
            &[point(4.0, 0.0)],
            &[point(4.0, 2.0)],
            &CancellationToken::default(),
        )
        .unwrap();
        assert_eq!(*warped.get_pixel(4, 0), background);
        assert_eq!(*warped.get_pixel(4, 2), foreground);
    }

    #[test]
    fn fixed_corners_preserve_hidden_rgb_exactly() {
        let mut image = RgbaImage::from_pixel(7, 6, Rgba([10, 20, 30, 255]));
        image.put_pixel(0, 0, Rgba([201, 2, 99, 0]));
        image.put_pixel(6, 0, Rgba([3, 202, 98, 0]));
        image.put_pixel(6, 5, Rgba([4, 3, 203, 0]));
        image.put_pixel(0, 5, Rgba([202, 203, 4, 0]));
        let warped = warp(
            &image,
            &[point(3.0, 3.0)],
            &[point(4.0, 3.0)],
            &CancellationToken::default(),
        )
        .unwrap();
        for &(x, y) in &[(0, 0), (6, 0), (6, 5), (0, 5)] {
            assert_eq!(*warped.get_pixel(x, y), *image.get_pixel(x, y));
        }
    }

    #[test]
    fn triangulation_references_every_node_and_covers_the_source_rectangle() {
        let mut seed = 0xC0FFEE_u32;
        for &(width, height) in &[(11, 9), (17, 5), (5, 19)] {
            let mut controls = Vec::new();
            for index in 0..48 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let x = if index % 5 == 0 {
                    0.001 + index as f32 * 0.000_01
                } else {
                    (seed % ((width - 1) * 1_000)) as f32 / 1_000.0
                };
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let y = if index % 4 == 0 {
                    0.001 + index as f32 * 0.000_01
                } else {
                    (seed % ((height - 1) * 1_000)) as f32 / 1_000.0
                };
                controls.push(point(x, y));
            }
            controls.extend([
                point((width - 1) as f32 * 0.3, 0.0),
                point(0.0, (height - 1) as f32 * 0.5),
                point((width - 1) as f32, (height - 1) as f32 * 0.7),
            ]);
            assert!(valid_points(width, height, &controls));
            let nodes = super::nodes(width, height, &controls, &controls);
            let triangles = super::triangulate(&nodes, None)
                .unwrap()
                .expect("valid controls produce a complete mesh");
            assert!(super::source_mesh_is_complete(&nodes, &triangles));
        }
    }
}
