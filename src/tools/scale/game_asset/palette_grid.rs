//! A source-only palette and fixed target-grid colour selection. Context can
//! promote a supported line, but never contributes a colour absent from a cell.
use std::collections::{BTreeMap, HashMap};

use image::RgbaImage;
use palette::{Lab, color_difference::Ciede2000};

use super::analysis::{Analysis, distance};
use super::{Grid, Options, geometry};
use crate::document::CancellationToken;
use crate::error::Result;

const TRANSPARENT: u32 = u32::MAX;

pub(super) fn delta_e(a: [f32; 3], b: [f32; 3]) -> f32 {
    let a: Lab = Lab::new(a[0], a[1], a[2]);
    a.difference(Lab::new(b[0], b[1], b[2]))
}

pub(super) struct Palette {
    pub labels: Vec<u32>,
    pub representatives: Vec<usize>,
}

impl Palette {
    pub fn new(
        source: &RgbaImage,
        lab: &[[f32; 3]],
        occupancy: &[bool],
        threshold: f32,
        cancellation: &CancellationToken,
    ) -> Result<Self> {
        let mut histogram = BTreeMap::<[u8; 3], (usize, usize)>::new();
        for (q, pixel) in source.pixels().enumerate() {
            if q % source.width() as usize == 0 {
                cancellation.check()?;
            }
            if !occupancy[q] {
                continue;
            }
            let rgb = [pixel[0], pixel[1], pixel[2]];
            histogram.entry(rgb).or_insert((q, 0)).1 += 1;
        }
        let mut colours: Vec<_> = histogram.into_iter().collect();
        // Frequent source colours become fixed representatives. No centroid
        // updates, transitive unions, palette-size cap, or rare-colour pruning.
        colours.sort_unstable_by(|(a, (_, na)), (b, (_, nb))| nb.cmp(na).then(a.cmp(b)));
        let mut representatives = Vec::new();
        let mut members = Vec::<Vec<usize>>::new();
        let mut bins = HashMap::<(i16, i16, i16), Vec<usize>>::new();
        let mut assignments = HashMap::new();
        let bin = |p: [f32; 3]| {
            (
                (p[0] / 4.0).floor() as i16,
                (p[1] / 8.0).floor() as i16,
                (p[2] / 8.0).floor() as i16,
            )
        };
        for (rgb, (q, _)) in colours {
            cancellation.check()?;
            let (l, a, b) = bin(lab[q]);
            let mut best: Option<(usize, f32)> = None;
            if threshold > 0.0 {
                // The spatial shortlist may retain extra entries; only exact
                // CIEDE2000 checks authorize a merge. Complete-link membership
                // prevents a chain of near neighbours swallowing an isolate.
                for dl in -1..=1 {
                    for da in -1..=1 {
                        for db in -1..=1 {
                            if let Some(ids) = bins.get(&(l + dl, a + da, b + db)) {
                                for &id in ids {
                                    let d = delta_e(lab[q], lab[representatives[id]]);
                                    if d < threshold
                                        && best.is_none_or(|(old, cost)| {
                                            d < cost || (d == cost && id < old)
                                        })
                                        && members[id]
                                            .iter()
                                            .all(|p| delta_e(lab[q], lab[*p]) < threshold)
                                    {
                                        best = Some((id, d));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let id = best.map(|(id, _)| id).unwrap_or_else(|| {
                let id = representatives.len();
                representatives.push(q);
                members.push(Vec::new());
                bins.entry((l, a, b)).or_default().push(id);
                id
            });
            members[id].push(q);
            assignments.insert(rgb, id as u32);
        }
        let mut labels = vec![TRANSPARENT; occupancy.len()];
        for (q, pixel) in source.pixels().enumerate() {
            if q % source.width() as usize == 0 {
                cancellation.check()?;
            }
            if occupancy[q] {
                labels[q] = assignments[&[pixel[0], pixel[1], pixel[2]]];
            }
        }
        Ok(Self {
            labels,
            representatives,
        })
    }

    pub fn source(&self, sample: usize) -> usize {
        let label = self.labels[sample];
        if label == TRANSPARENT {
            sample
        } else {
            self.representatives[label as usize]
        }
    }
}

#[derive(Clone, Default)]
struct Mode {
    label: u32,
    sample: usize,
    weight: f32,
    fill_weight: f32,
    density: f32,
    moments: [f32; 5],
    family_weight: f32,
    family_moments: [f32; 5],
}

impl Mode {
    fn shape(&self) -> (f32, [f32; 5]) {
        if self.family_weight > 0.0 {
            (self.family_weight, self.family_moments)
        } else {
            (self.weight, self.moments)
        }
    }
    fn line_extent(&self, dx: f32, dy: f32) -> f32 {
        let (weight, moments) = self.shape();
        let [x, y, xx, xy, yy] = moments.map(|v| v / weight.max(1e-6));
        let (xx, xy, yy) = ((xx - x * x).max(0.0), xy - x * y, (yy - y * y).max(0.0));
        let along = dx * dx * xx + 2.0 * dx * dy * xy + dy * dy * yy;
        let across = dy * dy * xx - 2.0 * dx * dy * xy + dx * dx * yy;
        ((along - across) / along.max(1e-6)).clamp(0.0, 1.0) * (12.0 * along).sqrt().clamp(0.0, 1.0)
    }
}

struct Cell {
    modes: Vec<Mode>,
    fill: usize,
}

/// Density chooses a source-colour medoid from the dominant local colour
/// family, not an average between light, dark and background pixels.
fn cell(
    p: usize,
    grid: Grid,
    analysis: &Analysis,
    cancellation: &CancellationToken,
) -> Result<Cell> {
    let mut groups = BTreeMap::<u32, Mode>::new();
    let (xs, ys) = grid.footprint(p);
    let (ox, oy) = ((p % grid.width) as f32, (p / grid.width) as f32);
    for y in ys {
        cancellation.check()?;
        let wy = ((y + 1) as f32).min((oy + 1.0) * grid.sy) - (y as f32).max(oy * grid.sy);
        for x in xs.clone() {
            let wx = ((x + 1) as f32).min((ox + 1.0) * grid.sx) - (x as f32).max(ox * grid.sx);
            let weight = wx.max(0.0) * wy.max(0.0);
            if weight == 0.0 {
                continue;
            }
            let q = y * grid.source_width + x;
            let label = analysis.palette.labels[q];
            let point = grid.project(q);
            let (dx, dy) = (point.x - ox, point.y - oy);
            let m = groups.entry(label).or_insert_with(|| Mode {
                label,
                sample: q,
                ..Default::default()
            });
            let old = grid.project(m.sample);
            if dx * dx + dy * dy < (old.x - ox).powi(2) + (old.y - oy).powi(2) {
                m.sample = q;
            }
            m.weight += weight;
            // Prefer the cell's core for fills, while retaining full-footprint
            // samples and unweighted geometry for thin boundary/line evidence.
            m.fill_weight += weight * (-4.0 * (dx * dx + dy * dy)).exp();
            for (sum, v) in m
                .moments
                .iter_mut()
                .zip([dx, dy, dx * dx, dx * dy, dy * dy])
            {
                *sum += weight * v;
            }
        }
    }
    let mut modes: Vec<_> = groups.into_values().collect();
    for i in 0..modes.len() {
        if i % 32 == 0 {
            cancellation.check()?;
        }
        let mut density = 0.0;
        let mut family_weight = 0.0;
        let mut family_moments = [0.0; 5];
        for other in &modes {
            let d = colour_distance(&modes[i], other, analysis);
            let affinity = if modes[i].label == TRANSPARENT || other.label == TRANSPARENT {
                f32::from(modes[i].label == other.label)
            } else {
                // This is fill-mode support, NOT palette merging: the winning
                // colour remains an unmodified source-derived palette entry.
                (1.0 - distance(
                    analysis.lab[analysis.palette.source(modes[i].sample)],
                    analysis.lab[analysis.palette.source(other.sample)],
                ) / 12.0)
                    .max(0.0)
                    .powi(2)
            };
            density += other.fill_weight * affinity;
            let geometry_affinity = (1.0 - d / 24.0).max(0.0).powi(2);
            family_weight += other.weight * geometry_affinity;
            for (sum, value) in family_moments.iter_mut().zip(other.moments) {
                *sum += value * geometry_affinity;
            }
        }
        modes[i].density = density;
        modes[i].family_weight = family_weight;
        modes[i].family_moments = family_moments;
    }
    // Colour selection does not get to dilate a silhouette. Alpha routing is
    // handled by the topology-checked filament/contour solver after this pass.
    let occupied = analysis.occupancy[grid.baseline(p)];
    let fill = (0..modes.len())
        .filter(|i| (modes[*i].label != TRANSPARENT) == occupied)
        .max_by(|a, b| {
            modes[*a]
                .density
                .total_cmp(&modes[*b].density)
                .then(modes[*a].weight.total_cmp(&modes[*b].weight))
                .then(b.cmp(a))
        })
        .expect("nonempty footprint");
    if modes.len() > 32 {
        // This bounds per-cell line search, not the source palette. Every
        // colour contributes to fill density before choosing alternatives.
        modes.swap(0, fill);
        let reference = modes[0].clone();
        let priority = |m: &Mode| {
            let diagonal = std::f32::consts::FRAC_1_SQRT_2;
            let extent = [
                (1.0, 0.0),
                (0.0, 1.0),
                (diagonal, diagonal),
                (diagonal, -diagonal),
            ]
            .into_iter()
            .map(|(x, y)| m.line_extent(x, y))
            .fold(0.0_f32, f32::max);
            m.density / reference.density.max(1e-6)
                + extent * (colour_distance(m, &reference, analysis) / 12.0 - 1.0).clamp(0.0, 2.0)
        };
        modes[1..].sort_unstable_by(|a, b| {
            priority(b)
                .total_cmp(&priority(a))
                .then(a.label.cmp(&b.label))
        });
        modes.truncate(32);
        return Ok(Cell { modes, fill: 0 });
    }
    Ok(Cell { modes, fill })
}

fn colour_distance(a: &Mode, b: &Mode, analysis: &Analysis) -> f32 {
    if a.label == b.label {
        return 0.0;
    }
    if a.label == TRANSPARENT || b.label == TRANSPARENT {
        return 50.0;
    }
    distance(
        analysis.lab[analysis.palette.source(a.sample)],
        analysis.lab[analysis.palette.source(b.sample)],
    )
}

pub(super) fn select(
    grid: Grid,
    analysis: &Analysis,
    options: &Options,
    cancellation: &CancellationToken,
) -> Result<Vec<usize>> {
    if !options.grid_context {
        return Ok((0..grid.width * grid.height)
            .map(|p| grid.baseline(p))
            .collect());
    }
    let mut cells = Vec::with_capacity(grid.width * grid.height);
    for p in 0..grid.width * grid.height {
        cells.push(cell(p, grid, analysis, cancellation)?);
    }
    if options.context_weight == 0.0 || (grid.width < 3 && grid.height < 3) {
        return Ok(cells.iter().map(|c| c.modes[c.fill].sample).collect());
    }
    let mut lines = Vec::new();
    for dy in -2..=2 {
        let line = geometry::line((-2, dy), (2, -dy));
        for path in [line.clone(), line.iter().map(|(x, y)| (*y, *x)).collect()] {
            if !lines.contains(&path) {
                lines.push(path);
            }
        }
    }
    let mut selected = Vec::with_capacity(cells.len());
    for (p, cell) in cells.iter().enumerate() {
        if p % grid.width == 0 {
            cancellation.check()?;
        }
        let mut best = cell.fill;
        let mut best_score = 1.0;
        let fill = &cell.modes[cell.fill];
        for (id, mode) in cell.modes.iter().enumerate() {
            if id % 32 == 0 {
                cancellation.check()?;
            }
            if mode.label == TRANSPARENT || fill.label == TRANSPARENT {
                continue;
            }
            // A dominant highlight can itself be a line. Compare EVERY mode
            // with its supported competitors, not only alternatives to fill.
            let area = cell.modes.iter().map(|m| m.weight).sum::<f32>();
            let contrast = cell
                .modes
                .iter()
                .map(|other| {
                    colour_distance(mode, other, analysis)
                        * (other.weight / (area * 0.1).max(1e-6)).min(1.0)
                })
                .fold(0.0_f32, f32::max);
            if contrast < options.tau_colour {
                continue;
            }
            let mut strongest = 0.0_f32;
            for line in &lines {
                let (dx, dy) = (line[4].0 as f32, line[4].1 as f32);
                let norm = dx.hypot(dy);
                let (dx, dy) = (dx / norm, dy / norm);
                let mut support = 0.0;
                let mut before = false;
                let mut after = false;
                let mut count = 0;
                let mut intercept = 0.0;
                let mut strengths = [0.0_f32; 5];
                for (step, &(lx, ly)) in line.iter().enumerate() {
                    let Some(x) = (p % grid.width).checked_add_signed(lx as isize) else {
                        continue;
                    };
                    let Some(y) = (p / grid.width).checked_add_signed(ly as isize) else {
                        continue;
                    };
                    if x >= grid.width || y >= grid.height {
                        continue;
                    }
                    let neighbour = &cells[y * grid.width + x];
                    let evidence: &[Mode] = if step == 2 {
                        std::slice::from_ref(mode)
                    } else {
                        &neighbour.modes
                    };
                    let supported = evidence
                        .iter()
                        .filter(|m| m.label != TRANSPARENT)
                        .map(|m| {
                            let agreement = (1.0
                                - colour_distance(mode, m, analysis)
                                    / options.tau_along.max(contrast * 0.5))
                            .max(0.0);
                            (m, agreement * m.line_extent(dx, dy))
                        })
                        .max_by(|(_, a), (_, b)| a.total_cmp(b));
                    let Some((member, strength)) = supported else {
                        continue;
                    };
                    strengths[step] = strength;
                    support += strength;
                    let (weight, moments) = member.shape();
                    let cx = lx as f32 + moments[0] / weight;
                    let cy = ly as f32 + moments[1] / weight;
                    intercept += strength * (-dy * cx + dx * cy);
                    if strength > 0.25 {
                        count += 1;
                        before |= step < 2;
                        after |= step > 2;
                    }
                }
                // Raster phase belongs to the source line, not independently
                // to every cell that happens to contain some of its colour.
                // The major-axis rounding interval is Bresenham's half pixel.
                let centred = (intercept / support.max(1e-6)).abs() <= 0.5 * dx.abs().max(dy.abs());
                let connected_run = strengths[2] >= 0.45
                    && strengths
                        .windows(3)
                        .any(|run| run.iter().all(|v| *v >= 0.35));
                if count >= 3 && before && after && centred && connected_run {
                    strongest = strongest.max(support / 5.0);
                }
            }
            let score = mode.density / fill.density.max(1e-6)
                + options.context_weight
                    * (contrast / options.tau_colour - 1.0).clamp(0.0, 2.0)
                    * strongest;
            if score > best_score {
                best_score = score;
                best = id;
            }
        }
        selected.push(cell.modes[best].sample);
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;
    use palette::{FromColor, Srgb};

    #[test]
    fn palette_keeps_isolates_exact_colours_and_complete_link_distance() {
        let source = RgbaImage::from_fn(128, 1, |x, _| {
            Rgba(if x == 127 {
                [255, 0, 180, 255]
            } else if x == 126 {
                [0, 255, 0, 0]
            } else {
                let v = 90 + (x % 35) as u8;
                [v, v, v, 255]
            })
        });
        let lab: Vec<_> = source
            .pixels()
            .map(|p| {
                let c: Lab = Lab::from_color(Srgb::new(
                    p[0] as f32 / 255.0,
                    p[1] as f32 / 255.0,
                    p[2] as f32 / 255.0,
                ));
                [c.l, c.a, c.b]
            })
            .collect();
        let occupancy: Vec<_> = source.pixels().map(|p| p[3] >= 128).collect();
        let cancellation = CancellationToken::default();
        let palette = Palette::new(&source, &lab, &occupancy, 2.0, &cancellation).unwrap();
        assert!(palette.representatives.len() < 36);
        assert_eq!(palette.labels[126], TRANSPARENT);
        assert_eq!(
            palette.source(127),
            127,
            "a rare chromatic isolate must remain its own entry"
        );
        for a in 0..128 {
            if !occupancy[a] {
                continue;
            }
            assert!(occupancy[palette.source(a)]);
            assert!(delta_e(lab[a], lab[palette.source(a)]) < 2.0);
            for b in 0..128 {
                if occupancy[b] && palette.labels[a] == palette.labels[b] {
                    assert!(
                        delta_e(lab[a], lab[b]) < 2.0,
                        "transitive drift through a colour ramp"
                    );
                }
            }
        }
        cancellation.cancel();
        assert!(Palette::new(&source, &lab, &occupancy, 2.0, &cancellation).is_err());
    }

    #[test]
    fn a_point_or_isotropic_fill_is_not_line_evidence() {
        let dot = Mode {
            weight: 1.0,
            ..Default::default()
        };
        assert_eq!(dot.line_extent(1.0, 0.0), 0.0);
        let fill = Mode {
            moments: [0.0, 0.0, 0.08, 0.0, 0.08],
            ..dot.clone()
        };
        assert_eq!(fill.line_extent(1.0, 0.0), 0.0);
        let line = Mode {
            moments: [0.0, 0.0, 0.08, 0.0, 0.0],
            ..dot
        };
        assert!(line.line_extent(1.0, 0.0) > 0.9);
        assert_eq!(line.line_extent(0.0, 1.0), 0.0);
    }

    #[test]
    fn default_grid_keeps_nearest_samples_before_feature_repair() {
        let mut source = RgbaImage::from_pixel(40, 40, Rgba([220, 220, 220, 255]));
        for y in 4..36 {
            source.put_pixel(17, y, Rgba([10, 10, 10, 255]));
        }
        let options = Options::default();
        let grid = Grid::new(&source, 10, 10, &options).unwrap();
        let cancellation = CancellationToken::default();
        let analysis = Analysis::new(&source, grid, &options, &cancellation).unwrap();
        let selected = select(grid, &analysis, &options, &cancellation).unwrap();
        assert_eq!(
            selected,
            (0..100).map(|p| grid.baseline(p)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn context_promotes_a_supported_line_but_not_an_isolated_extreme() {
        let mut source = RgbaImage::from_pixel(40, 40, Rgba([220, 220, 220, 255]));
        for y in 4..36 {
            source.put_pixel(17, y, Rgba([10, 10, 10, 255]));
        }
        source.put_pixel(29, 17, Rgba([255, 0, 180, 255]));
        let options = Options {
            grid_context: true,
            ..Default::default()
        };
        let grid = Grid::new(&source, 10, 10, &options).unwrap();
        let cancellation = CancellationToken::default();
        let analysis = Analysis::new(&source, grid, &options, &cancellation).unwrap();
        let fills = select(
            grid,
            &analysis,
            &Options {
                context_weight: 0.0,
                ..options.clone()
            },
            &cancellation,
        )
        .unwrap();
        let lines = select(grid, &analysis, &options, &cancellation).unwrap();
        for y in 3..7 {
            let p = y * 10 + 4;
            assert_eq!(
                source.get_pixel((fills[p] % 40) as u32, (fills[p] / 40) as u32)[0],
                220
            );
            assert_eq!(
                source.get_pixel((lines[p] % 40) as u32, (lines[p] / 40) as u32)[0],
                10
            );
        }
        let p = lines[4 * 10 + 7];
        assert_eq!(
            source.get_pixel((p % 40) as u32, (p / 40) as u32).0,
            [220, 220, 220, 255]
        );
        let isolate = 17 * 40 + 29;
        assert_eq!(analysis.palette.source(isolate), isolate);
    }
}
