//! Pair compatible endpoint tangents without introducing branches or new edges.
#[cfg(test)]
use crate::CancellationToken;
use crate::field::{Spatial, distance2};
use crate::{Cancellation, Result};

fn tangent(points: &[[f64; 2]], path: &[usize], side: usize) -> [f64; 2] {
    let at = |k: usize| points[path[if side == 0 { k } else { path.len() - 1 - k }]];
    let origin = at(0);
    let mut previous = origin;
    let mut traveled = 0.;
    let mut end = at(1);
    for k in 1..path.len() {
        end = at(k);
        let d = distance2(previous, end).sqrt();
        if traveled + d >= 3. {
            let t = (3. - traveled) / d;
            end = [
                previous[0] + t * (end[0] - previous[0]),
                previous[1] + t * (end[1] - previous[1]),
            ];
            break;
        }
        traveled += d;
        previous = end;
    }
    let d = distance2(origin, end).sqrt();
    if d < 1e-9 {
        return [0., 0.];
    }
    [(end[0] - origin[0]) / d, (end[1] - origin[1]) / d]
}

fn join_once(
    points: &[[f64; 2]],
    paths: &[Vec<usize>],
    cancel: &dyn Cancellation,
) -> Result<(Vec<Vec<usize>>, Vec<usize>)> {
    let mut tips = vec![Vec::new(); points.len()];
    let mut tangents = vec![[0.; 2]; paths.len() * 2];
    for (id, path) in paths.iter().enumerate() {
        cancel.check()?;
        if path.len() < 2 || path.first() == path.last() {
            continue;
        }
        for side in 0..2 {
            let tip = id * 2 + side;
            tips[if side == 0 {
                path[0]
            } else {
                *path.last().unwrap()
            }]
            .push(tip);
            tangents[tip] = tangent(points, path, side);
        }
    }
    let lengths: Vec<f64> = paths
        .iter()
        .map(|path| {
            path.windows(2)
                .map(|p| distance2(points[p[0]], points[p[1]]).sqrt())
                .sum()
        })
        .collect();
    // Tiny skeleton arcs inherit a local axis only when the neighborhood is
    // elongated and the arc itself follows that axis. This suppresses staircase
    // direction noise without turning a perpendicular branch into a continuation.
    let spatial = Spatial::new(points.to_vec(), 4.);
    for (node, ends) in tips.iter().enumerate().filter(|(_, ends)| ends.len() > 1) {
        cancel.check()?;
        let origin = points[node];
        let near = spatial.radius(origin, 4.);
        let mut sum = 0.;
        let mut mean = [0.; 2];
        for &i in &near {
            let weight = (-distance2(origin, points[i]) / 8.).exp();
            sum += weight;
            for (k, value) in mean.iter_mut().enumerate() {
                *value += points[i][k] * weight;
            }
        }
        if sum == 0. {
            continue;
        }
        mean = mean.map(|v| v / sum);
        let (mut xx, mut xy, mut yy) = (0., 0., 0.);
        for &i in &near {
            let weight = (-distance2(origin, points[i]) / 8.).exp();
            let x = points[i][0] - mean[0];
            let y = points[i][1] - mean[1];
            xx += weight * x * x;
            xy += weight * x * y;
            yy += weight * y * y;
        }
        let disc = (xx - yy).hypot(2. * xy);
        if xx + yy + disc < 3. * (xx + yy - disc) {
            continue;
        }
        let angle = 0.5 * (2. * xy).atan2(xx - yy);
        let axis = [angle.cos(), angle.sin()];
        for &tip in ends {
            if lengths[tip / 2] > 6. {
                continue;
            }
            let path = &paths[tip / 2];
            let from = points[path[0]];
            let to = points[*path.last().unwrap()];
            let d = distance2(from, to).sqrt();
            if d < 1e-9 {
                continue;
            }
            let sign = if tip.is_multiple_of(2) { 1. } else { -1. };
            let agreement = sign * ((to[0] - from[0]) * axis[0] + (to[1] - from[1]) * axis[1]) / d;
            if agreement.abs() >= 45_f64.to_radians().cos() {
                tangents[tip] = axis.map(|v| v * agreement.signum());
            }
        }
    }
    let mut choices = vec![None; tangents.len()];
    for ends in &tips {
        cancel.check()?;
        for &a in ends {
            let mut candidates: Vec<_> = ends
                .iter()
                .copied()
                .filter(|&b| b / 2 != a / 2)
                .map(|b| {
                    let score = -tangents[a][0] * tangents[b][0] - tangents[a][1] * tangents[b][1];
                    (score, b)
                })
                .filter(|&(score, _)| score >= 30_f64.to_radians().cos())
                .collect();
            // Two tiny routes to the same next junction are local alternatives,
            // not a branching continuation. Prefer the shorter route, then its
            // stable ID, before comparing distinct destinations for ambiguity.
            let destination = |tip: usize| {
                if tip.is_multiple_of(2) {
                    *paths[tip / 2].last().unwrap()
                } else {
                    paths[tip / 2][0]
                }
            };
            let alternatives = candidates.clone();
            candidates.retain(|&(_, b)| {
                !alternatives.iter().any(|&(_, c)| {
                    b != c
                        && destination(b) == destination(c)
                        && lengths[b / 2].max(lengths[c / 2]) <= 12.
                        && (lengths[c / 2], c / 2) < (lengths[b / 2], b / 2)
                })
            });
            candidates.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            if let Some(&(best, b)) = candidates.first()
                && candidates.get(1).is_none_or(|next| best - next.0 >= 0.05)
            {
                choices[a] = Some(b);
            }
        }
    }
    let links: Vec<_> = choices
        .iter()
        .enumerate()
        .map(|(a, &b)| b.filter(|&b| choices[b] == Some(a)))
        .collect();
    let starts = (0..links.len())
        .filter(|&tip| links[tip].is_none())
        .chain(0..links.len());
    let mut owners = vec![usize::MAX; paths.len()];
    let mut joined = Vec::new();
    for start in starts {
        cancel.check()?;
        if owners[start / 2] != usize::MAX {
            continue;
        }
        let mut tip = start;
        let mut path = Vec::new();
        loop {
            let id = tip / 2;
            if owners[id] != usize::MAX {
                break;
            }
            owners[id] = joined.len();
            let vertices = &paths[id];
            let skip = usize::from(!path.is_empty());
            for k in skip..vertices.len() {
                path.push(
                    vertices[if tip.is_multiple_of(2) {
                        k
                    } else {
                        vertices.len() - 1 - k
                    }],
                );
            }
            let Some(next) = links[tip ^ 1] else {
                break;
            };
            tip = next;
        }
        joined.push(path);
    }
    Ok((joined, owners))
}

pub(super) fn join(
    points: &[[f64; 2]],
    paths: &[Vec<usize>],
    cancel: &dyn Cancellation,
) -> Result<(Vec<Vec<usize>>, Vec<usize>)> {
    let mut current = paths.to_vec();
    let mut owners: Vec<_> = (0..paths.len()).collect();
    loop {
        let (next, mapping) = join_once(points, &current, cancel)?;
        for owner in &mut owners {
            *owner = mapping[*owner];
        }
        let finished = next.len() == current.len();
        current = next;
        if finished {
            return Ok((current, owners));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reversed_tiny_segments_join_the_straight_line_at_a_junction() {
        let points = [[0., 0.], [5., 0.], [6., 0.], [7., 0.], [12., 0.], [6., 4.]];
        let paths = vec![vec![0, 1], vec![2, 1], vec![2, 3], vec![4, 3], vec![2, 5]];
        let (joined, owners) = join(&points, &paths, &CancellationToken::default()).unwrap();
        assert_eq!(joined.len(), 2);
        assert_eq!(owners[0], owners[1]);
        assert_eq!(owners[0], owners[2]);
        assert_eq!(owners[0], owners[3]);
        assert_ne!(owners[0], owners[4]);
        assert_eq!(joined[owners[0]], vec![0, 1, 2, 3, 4]);
        assert_eq!(joined.iter().map(|p| p.len() - 1).sum::<usize>(), 5);
    }

    #[test]
    fn sharp_or_ambiguous_forks_remain_separate() {
        let points = [[0., 0.], [-4., 0.], [0., 4.], [4., 1.], [4., -1.]];
        let corner = vec![vec![1, 0], vec![0, 2]];
        assert_eq!(
            join(&points, &corner, &CancellationToken::default())
                .unwrap()
                .0
                .len(),
            2
        );
        let ambiguous = vec![vec![1, 0], vec![0, 3], vec![0, 4]];
        assert_eq!(
            join(&points, &ambiguous, &CancellationToken::default())
                .unwrap()
                .0
                .len(),
            3
        );
    }

    #[test]
    fn tiny_duplicate_routes_do_not_fragment_the_main_line() {
        let points = [[0., 0.], [4., 0.], [5., -1.], [5., 1.], [6., 0.], [10., 0.]];
        let paths = vec![vec![0, 1], vec![1, 2, 4], vec![1, 3, 4], vec![4, 5]];
        let (joined, owners) = join(&points, &paths, &CancellationToken::default()).unwrap();
        assert_eq!(joined.len(), 2);
        assert_eq!(owners[0], owners[3]);
        assert_ne!(owners[1], owners[2]);
        assert_eq!(joined.iter().map(|p| p.len() - 1).sum::<usize>(), 6);
    }

    #[test]
    fn elf_bow_edge_joins_across_repeated_tiny_loops() {
        let mut points = Vec::new();
        let mut paths = Vec::new();
        for line in include_str!("fixtures/bow-paths.txt")
            .lines()
            .filter(|line| !line.starts_with('#'))
        {
            let path = line
                .split_whitespace()
                .map(|word| {
                    let (x, y) = word.split_once(',').unwrap();
                    let p = [x.parse::<f64>().unwrap(), y.parse::<f64>().unwrap()];
                    if let Some(id) = points.iter().position(|q| *q == p) {
                        id
                    } else {
                        points.push(p);
                        points.len() - 1
                    }
                })
                .collect();
            paths.push(path);
        }
        let (joined, _) = join(&points, &paths, &CancellationToken::default()).unwrap();
        let start = points.iter().position(|p| *p == [448., 451.]).unwrap();
        let end = points.iter().position(|p| *p == [484., 430.]).unwrap();
        assert!(
            joined
                .iter()
                .any(|p| p.contains(&start) && p.contains(&end)),
            "the highlighted bow edge must be one contour"
        );
        let edges = |paths: &[Vec<usize>]| {
            let mut edges: Vec<_> = paths
                .iter()
                .flat_map(|p| p.windows(2).map(|e| (e[0].min(e[1]), e[0].max(e[1]))))
                .collect();
            edges.sort_unstable();
            edges
        };
        assert_eq!(edges(&joined), edges(&paths));
    }

    #[test]
    fn compatible_pieces_close_a_loop_without_losing_its_closing_edge() {
        let points: Vec<_> = (0..16)
            .map(|i| {
                let a = i as f64 * std::f64::consts::TAU / 16.;
                [10. * a.cos(), 10. * a.sin()]
            })
            .collect();
        let paths: Vec<_> = (0..16).map(|i| vec![i, (i + 1) % 16]).collect();
        let (joined, owners) = join(&points, &paths, &CancellationToken::default()).unwrap();
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].len(), 17);
        assert_eq!(joined[0].first(), joined[0].last());
        assert!(owners.iter().all(|&id| id == 0));
    }

    #[test]
    fn coincident_direction_does_not_join_spatially_separate_ends() {
        let points = [[0., 0.], [4., 0.], [5., 0.], [9., 0.]];
        let paths = vec![vec![0, 1], vec![2, 3]];
        assert_eq!(
            join(&points, &paths, &CancellationToken::default())
                .unwrap()
                .0,
            paths
        );
    }

    #[test]
    fn empty_singletons_and_closed_paths_keep_their_edges() {
        assert!(
            join(&[], &[], &CancellationToken::default())
                .unwrap()
                .0
                .is_empty()
        );
        let points = [[0., 0.], [1., 0.], [1., 1.]];
        let paths = vec![vec![0], vec![0, 1, 2, 0]];
        assert_eq!(
            join(&points, &paths, &CancellationToken::default())
                .unwrap()
                .0,
            paths
        );
    }
}
