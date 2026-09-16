//! Conservative source support extraction for assets with a real exterior.
//!
//! Ridges are not generally closed paths, so source support supplies topology
//! while final digital contour cores bound the target fill. Transparent sources
//! use alpha support and opaque flat canvases use only matching pixels connected
//! to the image boundary as their exterior.
use super::{
    color::{LinearImage, decode},
    raster::Mask,
};
use crate::{
    document::{CancellationToken, GameAssetAa},
    error::Result,
};
use image::{GrayImage, RgbaImage};
use std::collections::VecDeque;

const OPAQUE: u8 = 255;
const BACKGROUND_TOLERANCE: i32 = 3;

pub struct Silhouette {
    pub support: Mask,
    exterior: [f64; 4],
    opaque_foreground: bool,
}

impl Silhouette {
    /// Return `None` when an opaque image has no confidently flat canvas.
    pub fn detect(source: &RgbaImage, cancel: &CancellationToken) -> Result<Option<Self>> {
        cancel.check()?;
        let (w, h) = (source.width() as usize, source.height() as usize);
        if source.pixels().any(|p| p[3] != OPAQUE) {
            let mut support = Mask::new(w, h);
            for (i, p) in source.pixels().enumerate() {
                if i.is_multiple_of(4096) {
                    cancel.check()?;
                }
                support.data[i] = p[3] != 0;
            }
            if support.data.iter().all(|&v| v) {
                return Ok(None);
            }
            return Ok(Some(Self {
                support,
                exterior: [0.; 4],
                opaque_foreground: false,
            }));
        }

        let corners = [
            source.get_pixel(0, 0).0,
            source.get_pixel((w - 1) as u32, 0).0,
            source.get_pixel(0, (h - 1) as u32).0,
            source.get_pixel((w - 1) as u32, (h - 1) as u32).0,
        ];
        let background = median(corners);
        if corners
            .iter()
            .filter(|pixel| close(pixel, &background))
            .count()
            < 3
        {
            return Ok(None);
        }

        let mut exterior = vec![false; w * h];
        let mut queue = VecDeque::new();
        for x in 0..w {
            queue.push_back((x, 0));
            queue.push_back((x, h - 1));
        }
        for y in 1..h.saturating_sub(1) {
            queue.push_back((0, y));
            queue.push_back((w - 1, y));
        }
        let mut visited = 0usize;
        while let Some((x, y)) = queue.pop_front() {
            let i = y * w + x;
            if exterior[i] || !close(&source.get_pixel(x as u32, y as u32).0, &background) {
                continue;
            }
            exterior[i] = true;
            visited += 1;
            if visited.is_multiple_of(4096) {
                cancel.check()?;
            }
            if x > 0 {
                queue.push_back((x - 1, y));
            }
            if x + 1 < w {
                queue.push_back((x + 1, y));
            }
            if y > 0 {
                queue.push_back((x, y - 1));
            }
            if y + 1 < h {
                queue.push_back((x, y + 1));
            }
        }
        if exterior.iter().all(|&v| v) {
            return Ok(None);
        }
        Ok(Some(Self {
            support: Mask {
                w,
                h,
                data: exterior.into_iter().map(|v| !v).collect(),
            },
            exterior: [
                decode(background[0] as f64 / 255.),
                decode(background[1] as f64 / 255.),
                decode(background[2] as f64 / 255.),
                1.,
            ],
            opaque_foreground: true,
        }))
    }

    pub fn isolated(
        &self,
        source: &LinearImage,
        cancel: &CancellationToken,
    ) -> Result<LinearImage> {
        let mut result = source.clone();
        for (i, (pixel, &supported)) in result.pixels.iter_mut().zip(&self.support.data).enumerate()
        {
            if i.is_multiple_of(4096) {
                cancel.check()?;
            }
            if !supported {
                *pixel = [0.; 4];
            }
        }
        Ok(result)
    }

    /// Exact fractional coverage of the source support at an actual target
    /// size.  This is deliberately independent of the (open) ink paths.
    pub fn coverage(&self, w: usize, h: usize, cancel: &CancellationToken) -> Result<Vec<f64>> {
        self.project_mask(&self.support, w, h, cancel)
    }

    pub fn project_mask(
        &self,
        mask: &Mask,
        w: usize,
        h: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<f64>> {
        assert_eq!((mask.w, mask.h), (self.support.w, self.support.h));
        let sx = mask.w as f64 / w as f64;
        let sy = mask.h as f64 / h as f64;
        let mut vertical = vec![0.; mask.w * h];
        for y in 0..h {
            cancel.check()?;
            let top = y as f64 * sy;
            let bottom = (y + 1) as f64 * sy;
            for yy in top.floor() as usize..(bottom.ceil() as usize).min(mask.h) {
                let weight = (bottom.min((yy + 1) as f64) - top.max(yy as f64)) / sy;
                for x in 0..mask.w {
                    vertical[y * self.support.w + x] +=
                        f64::from(mask.data[yy * mask.w + x]) * weight;
                }
            }
        }
        let mut result = vec![0.; w * h];
        for y in 0..h {
            cancel.check()?;
            for x in 0..w {
                let left = x as f64 * sx;
                let right = (x + 1) as f64 * sx;
                for xx in left.floor() as usize..(right.ceil() as usize).min(mask.w) {
                    let weight = (right.min((xx + 1) as f64) - left.max(xx as f64)) / sx;
                    result[y * w + x] += vertical[y * self.support.w + xx] * weight;
                }
            }
        }
        Ok(result)
    }

    /// Convert source support to a target fill boundary.  The final digital
    /// contour core blocks an exterior flood, so a colored source fringe on
    /// the outer side of an outline cannot become fill.  Eroded source support
    /// also blocks the flood across missing/open contour fragments; this keeps
    /// the source's holes, gaps and disconnected topology authoritative.
    pub fn target_coverage(
        &self,
        coverage: &[f64],
        retained_ink: &[f64],
        core: &GrayImage,
        aa: GameAssetAa,
        cancel: &CancellationToken,
    ) -> Result<Vec<f64>> {
        let (w, h) = (core.width() as usize, core.height() as usize);
        assert_eq!(coverage.len(), w * h);
        assert_eq!(retained_ink.len(), coverage.len());
        let hard: Vec<_> = coverage.iter().map(|&v| v >= 0.5).collect();
        let mut trusted = vec![false; hard.len()];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if i.is_multiple_of(4096) {
                    cancel.check()?;
                }
                let eroded = hard[i]
                    && (-1..=1).all(|dy| {
                        (-1..=1).all(|dx| {
                            let xx = x as isize + dx;
                            let yy = y as isize + dy;
                            xx >= 0
                                && yy >= 0
                                && xx < w as isize
                                && yy < h as isize
                                && hard[yy as usize * w + xx as usize]
                        })
                    });
                // The source ink mask describes the actual thick region whose
                // color is being repaired.  It remains floodable up to the
                // final core instead of becoming a fixed one-pixel margin.
                trusted[i] = core.as_raw()[i] != 0 || (eroded && retained_ink[i] == 0.);
            }
        }
        // Do not erase an unoutlined one-pixel island merely because a 3x3
        // erosion has no interior.  A component with any core/eroded sample is
        // still eligible for the conservative exterior flood above.
        let mut seen = vec![false; hard.len()];
        let mut checked = 0usize;
        for start in 0..hard.len() {
            if !hard[start] || seen[start] {
                continue;
            }
            let mut component = Vec::new();
            let mut queue = VecDeque::from([start]);
            seen[start] = true;
            let mut has_trusted = false;
            while let Some(i) = queue.pop_front() {
                checked += 1;
                if checked.is_multiple_of(4096) {
                    cancel.check()?;
                }
                has_trusted |= trusted[i];
                component.push(i);
                let (x, y) = (i % w, i / w);
                for (dx, dy) in [(0isize, -1isize), (-1, 0), (1, 0), (0, 1)] {
                    let xx = x as isize + dx;
                    let yy = y as isize + dy;
                    if xx < 0 || yy < 0 || xx >= w as isize || yy >= h as isize {
                        continue;
                    }
                    let j = yy as usize * w + xx as usize;
                    if hard[j] && !seen[j] {
                        seen[j] = true;
                        queue.push_back(j);
                    }
                }
            }
            if !has_trusted {
                for i in component {
                    trusted[i] = true;
                }
            }
        }
        // Manhattan distance to a final core gives a topology-aware direction
        // through a thick erased ink band: exterior may move toward the core,
        // never away from it into an internal retained-ink branch through a
        // broken core. u32 also covers long, thin supported target axes.
        let mut core_distance = vec![u32::MAX; hard.len()];
        let mut core_queue = VecDeque::new();
        for (i, &value) in core.as_raw().iter().enumerate() {
            if value != 0 {
                core_distance[i] = 0;
                core_queue.push_back(i);
            }
        }
        let mut core_visited = 0usize;
        while let Some(i) = core_queue.pop_front() {
            core_visited += 1;
            if core_visited.is_multiple_of(4096) {
                cancel.check()?;
            }
            let (x, y) = (i % w, i / w);
            for (dx, dy) in [(0isize, -1isize), (-1, 0), (1, 0), (0, 1)] {
                let xx = x as isize + dx;
                let yy = y as isize + dy;
                if xx < 0 || yy < 0 || xx >= w as isize || yy >= h as isize {
                    continue;
                }
                let j = yy as usize * w + xx as usize;
                if core_distance[j] == u32::MAX {
                    core_distance[j] = core_distance[i].saturating_add(1);
                    core_queue.push_back(j);
                }
            }
        }
        // Every raw-background target sample stays exterior, including holes.
        // Only boundary-connected exterior may consume an untrusted fringe.
        let mut exterior: Vec<_> = hard.iter().map(|&v| !v).collect();
        let mut queue = VecDeque::new();
        // Seed every raw-background sample, not just the border.  Background
        // may be several target pixels away from the asset; each seed can then
        // consume an adjacent untrusted filled band up to a core/interior wall.
        for (i, &is_exterior) in exterior.iter().enumerate() {
            if is_exterior {
                queue.push_back((i % w, i / w));
            }
        }
        let mut visited = 0usize;
        while let Some((x, y)) = queue.pop_front() {
            visited += 1;
            if visited.is_multiple_of(4096) {
                cancel.check()?;
            }
            for (dx, dy) in [(0isize, -1isize), (-1, 0), (1, 0), (0, 1)] {
                let xx = x as isize + dx;
                let yy = y as isize + dy;
                if xx < 0 || yy < 0 || xx >= w as isize || yy >= h as isize {
                    continue;
                }
                let j = yy as usize * w + xx as usize;
                let toward_core =
                    retained_ink[j] == 0. || core_distance[j] <= core_distance[y * w + x];
                if !exterior[j] && !trusted[j] && toward_core {
                    exterior[j] = true;
                    queue.push_back((xx as usize, yy as usize));
                }
            }
        }
        let intensity = f64::from(aa.percent()) / 100.;
        Ok(coverage
            .iter()
            .enumerate()
            .map(|(i, &area)| {
                if exterior[i] {
                    0.
                } else {
                    let hard = f64::from(hard[i]);
                    hard * (1. - intensity) + area * intensity
                }
            })
            .collect())
    }

    pub fn exterior(&self) -> [f64; 4] {
        self.exterior
    }

    /// Source alpha projected independently of repair.  Repairing an outer
    /// ink path against a transparent exterior must not turn an originally
    /// opaque object translucent.
    pub fn intrinsic_opacity(
        &self,
        source: &LinearImage,
        coverage: &[f64],
        w: usize,
        h: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<f64>> {
        if self.opaque_foreground {
            return Ok(coverage.iter().map(|&v| f64::from(v > 0.)).collect());
        }
        let sx = source.w as f64 / w as f64;
        let sy = source.h as f64 / h as f64;
        let mut vertical = vec![0.; source.w * h];
        for y in 0..h {
            cancel.check()?;
            let top = y as f64 * sy;
            let bottom = (y + 1) as f64 * sy;
            for yy in top.floor() as usize..(bottom.ceil() as usize).min(source.h) {
                let weight = (bottom.min((yy + 1) as f64) - top.max(yy as f64)) / sy;
                for x in 0..source.w {
                    vertical[y * source.w + x] += source.pixels[yy * source.w + x][3] * weight;
                }
            }
        }
        let mut result = vec![0.; w * h];
        for y in 0..h {
            cancel.check()?;
            for x in 0..w {
                let left = x as f64 * sx;
                let right = (x + 1) as f64 * sx;
                for xx in left.floor() as usize..(right.ceil() as usize).min(source.w) {
                    let weight = (right.min((xx + 1) as f64) - left.max(xx as f64)) / sx;
                    result[y * w + x] += vertical[y * source.w + xx] * weight;
                }
            }
        }
        Ok(result
            .into_iter()
            .zip(coverage)
            .map(|(alpha, &support)| if support > 0. { alpha / support } else { 0. })
            .collect())
    }
}

fn median(mut pixels: [[u8; 4]; 4]) -> [u8; 4] {
    std::array::from_fn(|channel| {
        pixels.sort_unstable_by_key(|pixel| pixel[channel]);
        pixels[2][channel]
    })
}

fn close(pixel: &[u8; 4], background: &[u8; 4]) -> bool {
    pixel[..3]
        .iter()
        .zip(&background[..3])
        .all(|(&a, &b)| (i32::from(a) - i32::from(b)).abs() <= BACKGROUND_TOLERANCE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn boundary_connected_canvas_keeps_enclosed_matching_detail() {
        let mut image = RgbaImage::from_pixel(9, 9, Rgba([30, 60, 90, 255]));
        for y in 2..7 {
            for x in 2..7 {
                image.put_pixel(x, y, Rgba([200, 30, 20, 255]));
            }
        }
        image.put_pixel(4, 4, Rgba([30, 60, 90, 255]));
        let mask = Silhouette::detect(&image, &CancellationToken::default())
            .unwrap()
            .unwrap();
        assert!(mask.support.at(4, 4));
        assert!(!mask.support.at(0, 0));
    }

    #[test]
    fn boundary_mask_keeps_gaps_islands_and_border_touching_foreground() {
        let mut image = RgbaImage::from_pixel(11, 9, Rgba([30, 60, 90, 255]));
        // A border-touching, disconnected red island remains foreground; an
        // open channel remains true exterior rather than becoming a filled hole.
        for y in 0..5 {
            image.put_pixel(5, y, Rgba([210, 40, 30, 255]));
        }
        image.put_pixel(9, 7, Rgba([210, 40, 30, 255]));
        image.put_pixel(5, 2, Rgba([30, 60, 90, 255]));
        let mask = Silhouette::detect(&image, &CancellationToken::default())
            .unwrap()
            .unwrap();
        assert!(mask.support.at(5, 0));
        assert!(mask.support.at(9, 7));
        assert!(!mask.support.at(5, 2));
    }

    #[test]
    fn varied_opaque_corners_do_not_claim_an_exterior() {
        let image = RgbaImage::from_fn(5, 5, |x, y| {
            Rgba([(x * 31) as u8, (y * 47) as u8, (x * y * 9) as u8, 255])
        });
        assert!(
            Silhouette::detect(&image, &CancellationToken::default())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn outer_core_blocks_a_full_coverage_colored_fringe_without_leaking_a_gap() {
        let silhouette = Silhouette {
            support: Mask::new(7, 7),
            exterior: [0.; 4],
            opaque_foreground: true,
        };
        let mut coverage = vec![0.; 49];
        for y in 1..6 {
            for x in 1..6 {
                coverage[y * 7 + x] = 1.;
            }
        }
        let mut core = GrayImage::new(7, 7);
        for y in 1..6 {
            if y != 3 {
                core.put_pixel(2, y, image::Luma([255]));
            }
        }
        let result = silhouette
            .target_coverage(
                &coverage,
                &[0.; 49],
                &core,
                GameAssetAa::new(0),
                &CancellationToken::default(),
            )
            .unwrap();
        assert_eq!(result[3 * 7 + 1], 0., "outer colored fringe is exterior");
        assert_eq!(result[3 * 7 + 3], 1., "eroded support protects core gap");
        assert_eq!(result[4 * 7 + 3], 1., "interior behind outer core remains");
    }

    #[test]
    fn thick_retained_outer_ink_floods_to_the_final_core_near_identity() {
        let silhouette = Silhouette {
            support: Mask::new(9, 9),
            exterior: [0.; 4],
            opaque_foreground: true,
        };
        let mut coverage = vec![0.; 81];
        let mut retained = vec![0.; 81];
        for y in 1..8 {
            for x in 1..8 {
                coverage[y * 9 + x] = 1.;
            }
            for x in 1..4 {
                retained[y * 9 + x] = 1.;
            }
        }
        let mut core = GrayImage::new(9, 9);
        for y in 1..8 {
            core.put_pixel(4, y, image::Luma([255]));
        }
        let result = silhouette
            .target_coverage(
                &coverage,
                &retained,
                &core,
                GameAssetAa::new(0),
                &CancellationToken::default(),
            )
            .unwrap();
        assert_eq!(result[4 * 9 + 3], 0., "thick outer ink is exterior");
        assert_eq!(result[4 * 9 + 5], 1., "fill starts inside final core");
    }

    #[test]
    fn background_moat_reaches_a_distant_full_coverage_outer_ink_band() {
        let silhouette = Silhouette {
            support: Mask::new(15, 15),
            exterior: [0.; 4],
            opaque_foreground: true,
        };
        let mut coverage = vec![0.; 225];
        let mut retained = vec![0.; 225];
        for y in 4..11 {
            for x in 4..11 {
                coverage[y * 15 + x] = 1.;
            }
            retained[y * 15 + 4] = 1.;
        }
        let mut core = GrayImage::new(15, 15);
        for y in 4..11 {
            core.put_pixel(5, y, image::Luma([255]));
        }
        let result = silhouette
            .target_coverage(
                &coverage,
                &retained,
                &core,
                GameAssetAa::new(0),
                &CancellationToken::default(),
            )
            .unwrap();
        assert_eq!(result[7 * 15 + 4], 0.);
        assert_eq!(result[7 * 15 + 6], 1.);
    }

    #[test]
    fn antialias_coverage_is_applied_once() {
        let silhouette = Silhouette {
            support: Mask::new(1, 1),
            exterior: [0.; 4],
            opaque_foreground: true,
        };
        let result = silhouette
            .target_coverage(
                &[0.75],
                &[0.],
                &GrayImage::new(1, 1),
                GameAssetAa::new(50),
                &CancellationToken::default(),
            )
            .unwrap();
        assert_eq!(result, vec![0.875]);
    }

    #[test]
    fn arbitrary_core_gap_cannot_turn_an_internal_retained_ink_branch_into_exterior() {
        let silhouette = Silhouette {
            support: Mask::new(11, 11),
            exterior: [0.; 4],
            opaque_foreground: true,
        };
        let mut coverage = vec![0.; 121];
        let mut retained = vec![0.; 121];
        for y in 1..10 {
            for x in 1..10 {
                coverage[y * 11 + x] = 1.;
            }
            for x in 1..5 {
                retained[y * 11 + x] = 1.;
            }
        }
        for x in 4..9 {
            retained[5 * 11 + x] = 1.;
        }
        let mut core = GrayImage::new(11, 11);
        for y in 1..10 {
            if y != 5 && y != 6 {
                core.put_pixel(4, y, image::Luma([255]));
            }
            core.put_pixel(9, y, image::Luma([255]));
        }
        for x in 4..10 {
            core.put_pixel(x, 1, image::Luma([255]));
            core.put_pixel(x, 9, image::Luma([255]));
        }
        core.put_pixel(9, 5, image::Luma([255]));
        let result = silhouette
            .target_coverage(
                &coverage,
                &retained,
                &core,
                GameAssetAa::new(0),
                &CancellationToken::default(),
            )
            .unwrap();
        assert_eq!(result[5 * 11 + 7], 1., "internal branch remains fill");
    }
}
