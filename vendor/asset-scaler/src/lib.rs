//! Game Asset reduction: direction-merged contours, source-width opacity,
//! tight antialiasing, and Lanczos3 source fill with bounded halo tone-down.

#![doc = include_str!("../README.md")]

mod api;
pub use api::{Cancellation, CancellationToken, Error, GameAssetAa, Result};
use image::{GrayImage, RgbaImage};
pub use line_art::{LineArtComposer, LineArtLayers, Strength};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
mod antialias;
#[cfg(test)]
mod benchmarks;
mod cleanup;
mod color;
mod contours;
mod coverage;
mod detect;
mod field;
mod foreground_halo;
mod halo;
mod ink;
mod lanczos;
mod line_art;
mod opacity;
mod paint;
pub mod pen_aa;
mod raster;
mod silhouette;
mod smoothing;
mod source;
mod strokes;
mod target_cleanup;
#[cfg(test)]
mod tests;
pub const DEFAULT_MEMORY_LIMIT: u64 = 4 * 1024 * 1024 * 1024;
// The source phase keeps the established 512 B/pixel allowance for analysis,
// contour storage and source resampling. Target data is not concurrent
// with analysis, but can contain two linear images, contour ownership/colors,
// support/opacity projections, per-pixel contour importance/tangents, their
// canonicalization copies, per-pixel overlap aggregation, the output and cache;
// 240 B/pixel conservatively
// accounts for that composition peak. This phase-aware estimate is capped at
// four GiB, independently of the decoder and canvas safety limits.
const SOURCE_PHASE_BYTES: u64 = 512;
const TARGET_PHASE_BYTES: u64 = 240;
const FOREGROUND_SOURCE_BYTES: u64 = 96;

/// Colour to use when painting contours over an externally prepared fill.
///
/// `OriginalInk` samples the ink from the unedited source.  `DarkenedFill`
/// samples the already-downscaled fill at the contour pixel and multiplies its
/// linear-light RGB channels by `luminance`.  This deliberately leaves alpha
/// to the normal contour compositor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OutlineColor {
    OriginalInk,
    DarkenedFill { luminance: f64 },
}

/// Controls how ordinary resize calls treat a flat, opaque source canvas.
///
/// The default keeps the established behavior: boundary-connected pixels that
/// match a flat opaque canvas become transparent.  Select
/// [`Self::preserve_opaque_background`] when the source is a complete opaque
/// image, such as a game backdrop.  Source alpha is always respected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResizeOptions {
    remove_opaque_background: bool,
}

impl ResizeOptions {
    /// Preserve a flat opaque source canvas during resize.
    #[must_use]
    pub const fn preserve_opaque_background() -> Self {
        Self {
            remove_opaque_background: false,
        }
    }
}

impl Default for ResizeOptions {
    fn default() -> Self {
        Self {
            remove_opaque_background: true,
        }
    }
}

fn working_set_estimate(sw: u32, sh: u32, w: u32, h: u32) -> u64 {
    u64::from(sw)
        .saturating_mul(u64::from(sh))
        .saturating_mul(SOURCE_PHASE_BYTES)
        .saturating_add(
            u64::from(w)
                .saturating_mul(u64::from(h))
                .saturating_mul(TARGET_PHASE_BYTES),
        )
}

fn check_working_set_budget(sw: u32, sh: u32, w: u32, h: u32, limit: u64) -> Result<()> {
    if working_set_estimate(sw, sh, w, h) > limit {
        return Err(Error::GameAssetMemoryLimit { limit_bytes: limit });
    }
    Ok(())
}

fn check_foreground_working_set_budget(sw: u32, sh: u32, w: u32, h: u32, limit: u64) -> Result<()> {
    let estimate = working_set_estimate(sw, sh, w, h).saturating_add(
        u64::from(sw)
            .saturating_mul(u64::from(sh))
            .saturating_mul(FOREGROUND_SOURCE_BYTES),
    );
    if estimate > limit {
        return Err(Error::GameAssetMemoryLimit { limit_bytes: limit });
    }
    Ok(())
}

/// Chebyshev distance to unsupported foreground or the virtual image exterior.
/// A Chebyshev field matches the square footprint used for rasterized stroke
/// support, and avoids scanning a width-sized neighbourhood at every core.
pub(crate) fn foreground_exterior_distance(
    support: &[bool],
    w: usize,
    h: usize,
    cancel: &dyn Cancellation,
) -> Result<Vec<usize>> {
    let Some(len) = w.checked_mul(h) else {
        return Err(Error::Scaling(
            "Invalid foreground support dimensions".into(),
        ));
    };
    if support.len() != len {
        return Err(Error::Scaling(
            "Invalid foreground support dimensions".into(),
        ));
    }
    let mut unsupported = vec![usize::MAX; len];
    let mut pending = VecDeque::new();
    for (i, &inside) in support.iter().enumerate() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        if !inside {
            unsupported[i] = 0;
            pending.push_back(i);
        }
    }
    while let Some(i) = pending.pop_front() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        let x = i % w;
        let y = i / w;
        let next = unsupported[i] + 1;
        for dy in -1isize..=1 {
            for dx in -1isize..=1 {
                if dx == 0 && dy == 0 {
                    continue;
                }
                let xx = x as isize + dx;
                let yy = y as isize + dy;
                if xx < 0 || yy < 0 || xx >= w as isize || yy >= h as isize {
                    continue;
                }
                let j = yy as usize * w + xx as usize;
                if next < unsupported[j] {
                    unsupported[j] = next;
                    pending.push_back(j);
                }
            }
        }
    }
    Ok((0..len)
        .map(|i| {
            let x = i % w;
            let y = i / w;
            unsupported[i].min((x + 1).min(y + 1).min(w - x).min(h - y))
        })
        .collect())
}
struct Prepared {
    models: Vec<detect::Model>,
    widths: Vec<f64>,
    importances: Vec<f64>,
    contours: contours::Contours,
    /// Detected source ink footprint.
    mask: raster::Mask,
    linear: color::LinearImage,
    silhouette: Option<silhouette::Silhouette>,
    original_is_opaque: bool,
}
struct TargetContours {
    strokes: strokes::Strokes,
    colors: Vec<[f64; 3]>,
}

fn strength_contour_image_pixels(
    core: &GrayImage,
    strengths: &[f64],
    cancel: &dyn Cancellation,
) -> Result<GrayImage> {
    if core.as_raw().len() != strengths.len() {
        return Err(Error::Scaling("Invalid contour mask dimensions".into()));
    }
    let mut pixels = Vec::with_capacity(strengths.len());
    for (i, (&value, &strength)) in core.as_raw().iter().zip(strengths).enumerate() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        pixels.push(if value == 0 {
            255
        } else {
            (255. * (1. - strength)).round() as u8
        });
    }
    GrayImage::from_vec(core.width(), core.height(), pixels)
        .ok_or_else(|| Error::Scaling("Invalid contour mask dimensions".into()))
}

/// Fade a completed contour render over its completed fill in premultiplied
/// linear light. This makes opacity zero byte-identical to the fill boundary
/// and fades post-paint halo RGB together with the contour that caused it.
fn blend_contour_opacity(
    fill: &color::LinearImage,
    painted: &RgbaImage,
    opacity: f64,
    cancel: &dyn Cancellation,
) -> Result<RgbaImage> {
    if fill.w != painted.width() as usize || fill.h != painted.height() as usize {
        return Err(Error::Scaling(
            "Invalid contour opacity blend dimensions".into(),
        ));
    }
    if opacity == 0.0 {
        return Ok(RgbaImage::from_fn(fill.w as u32, fill.h as u32, |x, y| {
            color::rgba(fill.pixels[y as usize * fill.w + x as usize])
        }));
    }
    if opacity == 1.0 {
        return Ok(painted.clone());
    }
    let painted = color::LinearImage::from_rgba(painted);
    let mut output = RgbaImage::new(fill.w as u32, fill.h as u32);
    for (i, ((base, top), pixel)) in fill
        .pixels
        .iter()
        .zip(&painted.pixels)
        .zip(output.pixels_mut())
        .enumerate()
    {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        let alpha = base[3] * (1.0 - opacity) + top[3] * opacity;
        if alpha <= 1e-12 {
            pixel.0 = [0; 4];
            continue;
        }
        let color: [f64; 3] = std::array::from_fn(|c| {
            (base[c] * base[3] * (1.0 - opacity) + top[c] * top[3] * opacity) / alpha
        });
        pixel.0 = color::rgba([color[0], color[1], color[2], alpha]).0;
    }
    cancel.check()?;
    Ok(output)
}
struct InkSource<'a> {
    linear: &'a color::LinearImage,
    suppress_unsupported: bool,
    brightness: Option<f64>,
}
struct ForegroundInkResize {
    w: u32,
    h: u32,
    aa: GameAssetAa,
    brightness: f64,
}
struct FinishedFill {
    linear: color::LinearImage,
    foreground_support: Option<Vec<bool>>,
}
struct FillTarget {
    target: (u32, u32),
    aa: GameAssetAa,
    foreground_support: bool,
}
struct FillResize {
    w: u32,
    h: u32,
    aa: GameAssetAa,
    outline_color: OutlineColor,
}
impl Prepared {
    fn new(image: &RgbaImage, cancel: &dyn Cancellation) -> Result<Self> {
        Self::with_options(image, ResizeOptions::default(), cancel)
    }

    fn with_options(
        image: &RgbaImage,
        options: ResizeOptions,
        cancel: &dyn Cancellation,
    ) -> Result<Self> {
        cancel.check()?;
        let samples = detect::detect(image, cancel)?;
        cancel.check()?;
        let models = detect::fit_models(&samples, 0.012, cancel)?;
        let thinned = source::skeleton(
            &models,
            image.width() as usize,
            image.height() as usize,
            cancel,
        )?;
        cancel.check()?;
        let contours = contours::Contours::new(&thinned, &models, cancel)?;
        let mask = ink::ink_mask(image, &samples, &thinned, cancel)?;
        cancel.check()?;
        let widths = opacity::widths(image, &mask, &samples, &contours, cancel)?;
        let importances = opacity::importances(&samples, &contours, &widths);
        let linear = color::LinearImage::from_rgba(image);
        let mut original_is_opaque = true;
        for (i, pixel) in image.pixels().enumerate() {
            if i.is_multiple_of(4096) {
                cancel.check()?;
            }
            original_is_opaque &= pixel[3] == 255;
        }
        let silhouette = silhouette::Silhouette::detect_with_opaque_background_removal(
            image,
            options.remove_opaque_background,
            cancel,
        )?;
        Ok(Self {
            models,
            widths,
            importances,
            contours,
            mask,
            linear,
            silhouette,
            original_is_opaque,
        })
    }
    fn target_contours(
        &self,
        image: &RgbaImage,
        w: u32,
        h: u32,
        aa: GameAssetAa,
        cancel: &dyn Cancellation,
    ) -> Result<TargetContours> {
        self.target_contours_with_ink_source(
            image,
            w,
            h,
            aa,
            InkSource {
                linear: &self.linear,
                suppress_unsupported: false,
                brightness: None,
            },
            cancel,
        )
    }

    /// Keep original contour geometry while optionally coloring it from an
    /// aligned foreground. This remains private because ordinary resize calls
    /// must retain their established original-ink behavior.
    fn target_contours_with_ink_source(
        &self,
        image: &RgbaImage,
        w: u32,
        h: u32,
        aa: GameAssetAa,
        ink_source: InkSource<'_>,
        cancel: &dyn Cancellation,
    ) -> Result<TargetContours> {
        let scale = [
            w as f64 / image.width() as f64,
            h as f64 / image.height() as f64,
        ];
        // The foreground compositor coalesces at target resolution, so it
        // must keep every nonempty source fragment until that final cutoff.
        // Ordinary resize retains its established source-resolution cutoff.
        let cutoff = if ink_source.suppress_unsupported {
            0
        } else {
            contours::MAX_SHORT_PIXELS
        };
        let (retained, owners) = self.contours.retain_with_ids(&self.models, scale, cutoff);
        let fitted = (scale[0] < 1. || scale[1] < 1.)
            .then(|| self.contours.polished(scale, cutoff, cancel))
            .transpose()?;
        let (curves, curve_owners) = if let Some(fitted) = &fitted {
            let supported: std::collections::HashSet<_> = owners.iter().copied().collect();
            fitted
                .curves
                .iter()
                .zip(&fitted.owners)
                .filter(|(_, owner)| supported.contains(owner))
                .map(|(curve, &owner)| {
                    (
                        curve.map(|p| {
                            [(p[0] + 0.5) * scale[0] - 0.5, (p[1] + 0.5) * scale[1] - 0.5]
                        }),
                        owner,
                    )
                })
                .unzip::<_, _, Vec<coverage::Quadratic>, Vec<usize>>()
        } else {
            (
                retained
                    .iter()
                    .map(|m| {
                        detect::controls(m, 1.)
                            .map(|p| [(p[0] + 0.5) * scale[0] - 0.5, (p[1] + 0.5) * scale[1] - 0.5])
                    })
                    .collect(),
                owners.clone(),
            )
        };
        cancel.check()?;
        let mut strokes = strokes::render(
            &curves,
            &curve_owners,
            &self.widths,
            &self.importances,
            w,
            h,
            aa,
            cancel,
        )?;
        self.refine_target_importance(&mut strokes, cancel)?;
        let (mut colors, visible) = if let Some(fitted) = &fitted {
            paint::ink_colors_for_curves(
                ink_source.linear,
                &fitted.curves,
                &fitted.owners,
                &retained,
                &owners,
                &fitted.trace_donors,
                &strokes,
                scale,
                cancel,
            )?
        } else if ink_source.suppress_unsupported {
            paint::ink_colors_with_visibility(
                ink_source.linear,
                &retained,
                &retained,
                &owners,
                &strokes,
                scale,
                cancel,
            )?
        } else {
            let colors = paint::ink_colors(
                ink_source.linear,
                &retained,
                &retained,
                &owners,
                &strokes,
                scale,
                cancel,
            )?;
            (colors, vec![true; strokes.coverage.as_raw().len()])
        };
        if ink_source.suppress_unsupported {
            for (i, &supported) in visible.iter().enumerate() {
                if i.is_multiple_of(4096) {
                    cancel.check()?;
                }
                if supported {
                    continue;
                }
                strokes.coverage.as_mut()[i] = 0;
                strokes.core.as_mut()[i] = 0;
                strokes.owners[i] = None;
            }
        } else if let Some((i, _)) = strokes
            .coverage
            .as_raw()
            .iter()
            .zip(&visible)
            .enumerate()
            .find(|&(_, (&coverage, &visible))| coverage != 0 && !visible)
        {
            return Err(Error::Scaling(format!(
                "No visible source ink donor for target pixel {i}"
            )));
        }
        if let Some(brightness) = ink_source.brightness {
            for (i, ink_color) in colors.iter_mut().enumerate() {
                if i.is_multiple_of(4096) {
                    cancel.check()?;
                }
                *ink_color = color::scale_srgb(*ink_color, brightness);
            }
        }
        Ok(TargetContours { strokes, colors })
    }

    fn polished_contour_mask(
        &self,
        source: &RgbaImage,
        w: u32,
        h: u32,
        cancel: &dyn Cancellation,
    ) -> Result<GrayImage> {
        let scale = [
            w as f64 / source.width() as f64,
            h as f64 / source.height() as f64,
        ];
        let fitted = self
            .contours
            .polished(scale, contours::MAX_SHORT_PIXELS, cancel)?;
        let curves = fitted
            .curves
            .iter()
            .map(|curve| {
                curve.map(|p| [(p[0] + 0.5) * scale[0] - 0.5, (p[1] + 0.5) * scale[1] - 0.5])
            })
            .collect::<Vec<_>>();
        let mut strokes = strokes::render(
            &curves,
            &fitted.owners,
            &self.widths,
            &self.importances,
            w,
            h,
            GameAssetAa::new(0),
            cancel,
        )?;
        self.refine_target_importance(&mut strokes, cancel)?;
        let strengths = self.intrinsic_strengths(&strokes, cancel)?;
        strength_contour_image_pixels(&strokes.core, &strengths, cancel)
    }

    fn foreground_contour_mask(
        &self,
        original: &RgbaImage,
        foreground: &RgbaImage,
        w: u32,
        h: u32,
        aa: GameAssetAa,
        cancel: &dyn Cancellation,
    ) -> Result<GrayImage> {
        let fill = color::LinearImage::from_rgba(foreground);
        let silhouette = silhouette::Silhouette::detect(foreground, cancel)?;
        let mut contours = self.target_contours_with_ink_source(
            original,
            w,
            h,
            aa,
            InkSource {
                linear: &fill,
                suppress_unsupported: true,
                brightness: None,
            },
            cancel,
        )?;
        let finished = self.fill_for_target(
            &fill,
            silhouette.as_ref(),
            &contours.strokes,
            FillTarget {
                target: (w, h),
                aa,
                foreground_support: true,
            },
            cancel,
        )?;
        let support = finished
            .foreground_support
            .as_deref()
            .expect("foreground support requested");
        Self::canonicalize_foreground_strokes(
            &mut contours.strokes,
            &mut contours.colors,
            support,
            aa,
            cancel,
        )?;
        // The preview exposes the same intrinsic strengths used by every
        // foreground paint mode. AA changes only coverage at the edge.
        let strengths = self.intrinsic_strengths(&contours.strokes, cancel)?;
        strength_contour_image_pixels(&contours.strokes.core, &strengths, cancel)
    }

    #[allow(clippy::too_many_arguments)]
    fn resize_with_foreground_opacity(
        &self,
        original: &RgbaImage,
        foreground: &RgbaImage,
        w: u32,
        h: u32,
        aa: GameAssetAa,
        contour_opacity: f64,
        cancel: &dyn Cancellation,
    ) -> Result<RgbaImage> {
        let fill = color::LinearImage::from_rgba(foreground);
        let silhouette = silhouette::Silhouette::detect(foreground, cancel)?;
        let mut contours = self.target_contours_with_ink_source(
            original,
            w,
            h,
            aa,
            InkSource {
                linear: &fill,
                suppress_unsupported: true,
                brightness: None,
            },
            cancel,
        )?;
        // This is the established foreground fill/halo baseline. The final
        // opacity blend includes every contour-derived foreground-halo repair.
        let finished = self.fill_for_target(
            &fill,
            silhouette.as_ref(),
            &contours.strokes,
            FillTarget {
                target: (w, h),
                aa,
                foreground_support: true,
            },
            cancel,
        )?;
        let support = finished
            .foreground_support
            .as_deref()
            .expect("foreground support requested");
        Self::canonicalize_foreground_strokes(
            &mut contours.strokes,
            &mut contours.colors,
            support,
            aa,
            cancel,
        )?;
        let strength = self.intrinsic_strengths(&contours.strokes, cancel)?;
        let paint = opacity::apply_pixels(&contours.strokes.coverage, &strength)?;
        let painted = paint::composite(&finished.linear, &contours.colors, &paint);
        let painted = foreground_halo::clean(
            foreground_halo::Inputs {
                support,
                original_is_opaque: self.original_is_opaque,
                fill: &finished.linear,
                baseline: painted,
                core: &contours.strokes.core,
                coverage: &contours.strokes.coverage,
                owners: &contours.strokes.owners,
                colors: &contours.colors,
            },
            cancel,
        )?;
        let result = blend_contour_opacity(&finished.linear, &painted, contour_opacity, cancel)?;
        cancel.check()?;
        Ok(result)
    }
    fn resize_with_fill(
        &self,
        contour_source: &RgbaImage,
        fill_linear: &color::LinearImage,
        fill_silhouette: Option<&silhouette::Silhouette>,
        target: (u32, u32),
        aa: GameAssetAa,
        cancel: &dyn Cancellation,
    ) -> Result<RgbaImage> {
        let (w, h) = target;
        let contours = self.target_contours(contour_source, w, h, aa, cancel)?;
        self.resize_with_fill_from_target(
            fill_linear,
            fill_silhouette,
            contours,
            target,
            aa,
            cancel,
        )
    }

    /// Shared fill, silhouette, halo and compositor path after contour colors
    /// have been selected.
    fn resize_with_fill_from_target(
        &self,
        fill_linear: &color::LinearImage,
        fill_silhouette: Option<&silhouette::Silhouette>,
        TargetContours { strokes, colors }: TargetContours,
        target: (u32, u32),
        aa: GameAssetAa,
        cancel: &dyn Cancellation,
    ) -> Result<RgbaImage> {
        let strength = self.intrinsic_strengths(&strokes, cancel)?;
        let paint = opacity::apply_pixels(&strokes.coverage, &strength)?;
        let fill = self
            .fill_for_target(
                fill_linear,
                fill_silhouette,
                &strokes,
                FillTarget {
                    target,
                    aa,
                    foreground_support: false,
                },
                cancel,
            )?
            .linear;
        let result = paint::composite(&fill, &colors, &paint);
        cancel.check()?;
        Ok(result)
    }

    /// Produce the finished fill once, including ordinary halo and silhouette
    /// work. The foreground compositor uses this support to canonicalize its
    /// target contour core before painting.
    fn fill_for_target(
        &self,
        fill_linear: &color::LinearImage,
        fill_silhouette: Option<&silhouette::Silhouette>,
        strokes: &strokes::Strokes,
        request: FillTarget,
        cancel: &dyn Cancellation,
    ) -> Result<FinishedFill> {
        let (w, h) = request.target;
        let scale = [
            w as f64 / self.linear.w as f64,
            h as f64 / self.linear.h as f64,
        ];
        let mut retained_mask = self.contours.retained_ink_mask(&self.mask, scale, cancel)?;
        cancel.check()?;
        let isolated = if let Some(silhouette) = fill_silhouette {
            for (i, (masked, &supported)) in retained_mask
                .data
                .iter_mut()
                .zip(&silhouette.support.data)
                .enumerate()
            {
                if i % 4096 == 0 {
                    cancel.check()?;
                }
                *masked &= supported;
            }
            Some(silhouette.isolated(fill_linear, cancel)?)
        } else {
            None
        };
        let fill_source = isolated.as_ref().unwrap_or(fill_linear);
        let base = lanczos::resize(fill_source, w as usize, h as usize, cancel)?;
        let target_support = if request.foreground_support {
            let mut support = Vec::with_capacity(base.pixels.len());
            for (i, pixel) in base.pixels.iter().enumerate() {
                if i.is_multiple_of(4096) {
                    cancel.check()?;
                }
                support.push(pixel[3] >= 0.5);
            }
            Some(support)
        } else {
            None
        };
        let base = halo::apply(fill_source, &retained_mask, base, &strokes.core, cancel)?;
        let fill = match fill_silhouette {
            Some(silhouette) => silhouette.target_alpha(base, fill_source, request.aa, cancel)?,
            None => base,
        };
        Ok(FinishedFill {
            linear: fill,
            foreground_support: target_support,
        })
    }

    fn resize(
        &self,
        image: &RgbaImage,
        w: u32,
        h: u32,
        aa: GameAssetAa,
        cancel: &dyn Cancellation,
    ) -> Result<RgbaImage> {
        self.resize_with_fill(
            image,
            &self.linear,
            self.silhouette.as_ref(),
            (w, h),
            aa,
            cancel,
        )
    }

    fn resize_with_foreground(
        &self,
        original: &RgbaImage,
        foreground: &RgbaImage,
        w: u32,
        h: u32,
        aa: GameAssetAa,
        cancel: &dyn Cancellation,
    ) -> Result<RgbaImage> {
        let fill_linear = color::LinearImage::from_rgba(foreground);
        let fill_silhouette = silhouette::Silhouette::detect(foreground, cancel)?;
        self.resize_with_fill(
            original,
            &fill_linear,
            fill_silhouette.as_ref(),
            (w, h),
            aa,
            cancel,
        )
    }

    fn resize_with_foreground_ink(
        &self,
        original: &RgbaImage,
        foreground: &RgbaImage,
        request: ForegroundInkResize,
        cancel: &dyn Cancellation,
    ) -> Result<RgbaImage> {
        let fill_linear = color::LinearImage::from_rgba(foreground);
        let fill_silhouette = silhouette::Silhouette::detect(foreground, cancel)?;
        let contours = self.target_contours_with_ink_source(
            original,
            request.w,
            request.h,
            request.aa,
            InkSource {
                linear: &fill_linear,
                suppress_unsupported: true,
                brightness: Some(request.brightness),
            },
            cancel,
        )?;
        self.resize_with_fill_from_target_and_canonical_core(
            &fill_linear,
            fill_silhouette.as_ref(),
            contours,
            (request.w, request.h),
            request.aa,
            cancel,
        )
    }

    /// The foreground-ink-only compositor path reduces all rendered target
    /// cores to one canonical, outer-prioritized mask before painting them.
    fn resize_with_fill_from_target_and_canonical_core(
        &self,
        fill_linear: &color::LinearImage,
        fill_silhouette: Option<&silhouette::Silhouette>,
        mut contours: TargetContours,
        target: (u32, u32),
        aa: GameAssetAa,
        cancel: &dyn Cancellation,
    ) -> Result<RgbaImage> {
        let fill = self.fill_for_target(
            fill_linear,
            fill_silhouette,
            &contours.strokes,
            FillTarget {
                target,
                aa,
                foreground_support: true,
            },
            cancel,
        )?;
        let support = fill
            .foreground_support
            .as_deref()
            .expect("foreground support was requested");
        Self::canonicalize_foreground_strokes(
            &mut contours.strokes,
            &mut contours.colors,
            support,
            aa,
            cancel,
        )?;
        let strength = self.intrinsic_strengths(&contours.strokes, cancel)?;
        let paint = opacity::apply_pixels(&contours.strokes.coverage, &strength)?;
        let baseline = paint::composite(&fill.linear, &contours.colors, &paint);
        foreground_halo::clean(
            foreground_halo::Inputs {
                support,
                original_is_opaque: self.original_is_opaque,
                fill: &fill.linear,
                baseline,
                core: &contours.strokes.core,
                coverage: &contours.strokes.coverage,
                owners: &contours.strokes.owners,
                colors: &contours.colors,
            },
            cancel,
        )
    }

    /// Convert all target contour owners into one exterior-prioritized core.
    /// Removed cores cannot retain full AA coverage; a bounded fringe may only
    /// borrow the nearest surviving canonical core's owner and colour.
    fn canonicalize_foreground_strokes(
        strokes: &mut strokes::Strokes,
        colors: &mut [[f64; 3]],
        support: &[bool],
        aa: GameAssetAa,
        cancel: &dyn Cancellation,
    ) -> Result<()> {
        let mut raw = raster::Mask {
            w: strokes.core.width() as usize,
            h: strokes.core.height() as usize,
            data: strokes
                .core
                .as_raw()
                .iter()
                .map(|&value| value != 0)
                .collect(),
        };
        if colors.len() != raw.data.len()
            || support.len() != raw.data.len()
            || strokes.owners.len() != raw.data.len()
            || strokes.importance.len() != raw.data.len()
            || strokes.tangents.len() != raw.data.len()
            || strokes.footprint.len() != raw.data.len()
        {
            return Err(Error::Scaling(
                "Invalid foreground contour ownership".into(),
            ));
        }
        let exterior_distance = foreground_exterior_distance(support, raw.w, raw.h, cancel)?;
        // This is target-scale evidence after reduced colour-gradient
        // refinement. Cull low-prominence interior noise before it can become
        // a topology-preserving alternate route. Boundary ink remains: its
        // exterior relationship is semantic, rather than texture noise.
        for (i, &distance) in exterior_distance.iter().enumerate() {
            if i.is_multiple_of(4096) {
                cancel.check()?;
            }
            if !raw.data[i] || strokes.importance[i] >= 0.16 {
                continue;
            }
            let boundary = distance as f64 <= strokes.footprint[i].ceil().max(2.);
            if !boundary {
                raw.data[i] = false;
                strokes.core.as_mut()[i] = 0;
                strokes.coverage.as_mut()[i] = 0;
                strokes.owners[i] = None;
            }
        }
        let mut cleanup_importance = strokes.importance.clone();
        for (i, value) in cleanup_importance.iter_mut().enumerate() {
            if i.is_multiple_of(4096) {
                cancel.check()?;
            }
            if !raw.data[i] {
                continue;
            }
            let touches_exterior =
                exterior_distance[i] as f64 <= strokes.footprint[i].ceil().max(2.);
            if touches_exterior {
                *value = 1.;
            }
        }
        let canonical =
            target_cleanup::thin_outer_weighted(&raw, support, &cleanup_importance, cancel)?;
        let previous_coverage = strokes.coverage.as_raw().to_vec();
        let previous_owners = strokes.owners.clone();
        let previous_importance = strokes.importance.clone();
        let previous_tangents = strokes.tangents.clone();
        for (i, &keep) in canonical.data.iter().enumerate() {
            if i.is_multiple_of(4096) {
                cancel.check()?;
            }
            if raw.data[i] && !keep {
                strokes.core.as_mut()[i] = 0;
            }
            if keep {
                strokes.importance[i] = cleanup_importance[i];
                let mut contributors = std::collections::BTreeMap::<usize, f64>::new();
                let x = i % raw.w;
                let y = i / raw.w;
                for dy in -2isize..=2 {
                    for dx in -2isize..=2 {
                        let xx = x as isize + dx;
                        let yy = y as isize + dy;
                        if xx < 0 || yy < 0 || xx >= raw.w as isize || yy >= raw.h as isize {
                            continue;
                        }
                        let j = yy as usize * raw.w + xx as usize;
                        let Some(owner) = previous_owners[j] else {
                            continue;
                        };
                        if !raw.data[j]
                            || canonical.data[j]
                            || owner == previous_owners[i].unwrap_or(owner)
                        {
                            continue;
                        }
                        let (Some(a), Some(b)) = (previous_tangents[i], previous_tangents[j])
                        else {
                            continue;
                        };
                        if (a[0] * b[0] + a[1] * b[1]).abs() < 0.9 {
                            continue;
                        }
                        let along = (dx as f64 * a[0] + dy as f64 * a[1]).abs();
                        if along > 1.1 {
                            continue;
                        }
                        contributors
                            .entry(owner)
                            .and_modify(|value| *value = value.max(previous_importance[j]))
                            .or_insert(previous_importance[j]);
                    }
                }
                let mut values: Vec<_> = contributors.into_values().collect();
                values.push(strokes.importance[i]);
                let strongest = values.iter().copied().fold(0., f64::max);
                let strongest_index = values
                    .iter()
                    .position(|&value| value == strongest)
                    .expect("contour aggregation has one value");
                let residual = values
                    .into_iter()
                    .enumerate()
                    .filter(|&(index, _)| index != strongest_index)
                    .map(|(_, value)| value)
                    .map(|value| 1. - 0.65 * value)
                    .product::<f64>();
                let combined = 1. - (1. - strongest) * residual;
                strokes.importance[i] = combined;
            }
        }
        for i in 0..raw.data.len() {
            if i.is_multiple_of(4096) {
                cancel.check()?;
            }
            if !canonical.data[i] {
                strokes.coverage.as_mut()[i] = 0;
                strokes.owners[i] = None;
            }
        }
        if aa.percent() == 0 {
            return Ok(());
        }
        for (i, &old_coverage) in previous_coverage.iter().enumerate() {
            if i.is_multiple_of(4096) {
                cancel.check()?;
            }
            if canonical.data[i] || old_coverage == 0 {
                continue;
            }
            let x = i % raw.w;
            let y = i / raw.w;
            let donor = (-1isize..=1)
                .flat_map(|dy| (-1isize..=1).map(move |dx| (dx, dy)))
                .filter(|&(dx, dy)| dx != 0 || dy != 0)
                .filter_map(|(dx, dy)| {
                    let xx = x as isize + dx;
                    let yy = y as isize + dy;
                    (xx >= 0 && yy >= 0 && xx < raw.w as isize && yy < raw.h as isize)
                        .then(|| yy as usize * raw.w + xx as usize)
                })
                .filter(|&j| canonical.data[j] && previous_owners[j].is_some())
                .min_by_key(|&j| {
                    let dx = j % raw.w;
                    let dy = j / raw.w;
                    let ddx = dx.abs_diff(x);
                    let ddy = dy.abs_diff(y);
                    (ddx * ddx + ddy * ddy, j)
                });
            if let Some(j) = donor {
                // A removed core may contribute only a pen fringe. Restoring
                // its old full coverage would recreate an interior parallel
                // stroke after canonical outer placement.
                strokes.coverage.as_mut()[i] = if raw.data[i] {
                    let cap = (128_u16 * u16::from(aa.percent()) / 100) as u8;
                    old_coverage.min(cap)
                } else {
                    old_coverage
                };
                strokes.owners[i] = previous_owners[j];
                colors[i] = colors[j];
                strokes.importance[i] = strokes.importance[j];
                strokes.tangents[i] = strokes.tangents[j];
            }
        }
        Ok(())
    }

    fn intrinsic_strengths(
        &self,
        strokes: &strokes::Strokes,
        cancel: &dyn Cancellation,
    ) -> Result<Vec<f64>> {
        opacity::pixel_strengths_cancellable(
            &strokes.importance,
            &strokes.tangents,
            [
                strokes.core.width() as f64 / self.linear.w as f64,
                strokes.core.height() as f64 / self.linear.h as f64,
            ],
            cancel,
        )
    }

    /// Measure colour edges after reduction, but only at the already accepted
    /// source-derived contour pixels. This preserves ridge geometry and donor
    /// ownership while allowing broad hat/cape boundaries to survive the
    /// target scale that naturally averages away fine hatching.
    fn refine_target_importance(
        &self,
        strokes: &mut strokes::Strokes,
        cancel: &dyn Cancellation,
    ) -> Result<()> {
        let (w, h) = (
            strokes.core.width() as usize,
            strokes.core.height() as usize,
        );
        let reduced = lanczos::resize(&self.linear, w, h, cancel)?;
        let colour = |x: isize, y: isize| {
            if x < 0 || y < 0 || x >= w as isize || y >= h as isize {
                return [1.; 3];
            }
            let pixel = reduced.pixels[y as usize * w + x as usize];
            [
                pixel[0] * pixel[3] + 1. - pixel[3],
                pixel[1] * pixel[3] + 1. - pixel[3],
                pixel[2] * pixel[3] + 1. - pixel[3],
            ]
        };
        let sobel = |x: isize, y: isize| {
            let mut gx = [0.; 3];
            let mut gy = [0.; 3];
            for (dy, row_weight) in [(-1, 1.), (0, 2.), (1, 1.)] {
                for (dx, column_weight) in [-1, 0, 1].into_iter().zip([-1., 0., 1.]) {
                    let value = colour(x + dx, y + dy);
                    for channel in 0..3 {
                        gx[channel] += row_weight * column_weight * value[channel];
                    }
                }
            }
            for (dy, row_weight) in [-1, 0, 1].into_iter().zip([-1., 0., 1.]) {
                for (dx, column_weight) in [-1, 0, 1].into_iter().zip([1., 2., 1.]) {
                    let value = colour(x + dx, y + dy);
                    for channel in 0..3 {
                        gy[channel] += row_weight * column_weight * value[channel];
                    }
                }
            }
            (gx.into_iter().map(|v| v * v).sum::<f64>()
                + gy.into_iter().map(|v| v * v).sum::<f64>())
            .sqrt()
                / 5.657
        };
        for i in 0..strokes.importance.len() {
            if i.is_multiple_of(4096) {
                cancel.check()?;
            }
            if strokes.core.as_raw()[i] == 0 {
                continue;
            }
            let x = (i % w) as isize;
            let y = (i / w) as isize;
            // A ridge core can lie in the middle of a wide dark band. Search
            // two target samples around it so either shoulder supplies the
            // Sobel evidence instead of mistaking the flat center for noise.
            let edge = (-2..=2)
                .flat_map(|dy| (-2..=2).map(move |dx| sobel(x + dx, y + dy)))
                .fold(0., f64::max);
            if let (Some(owner), Some(tangent)) = (strokes.owners[i], strokes.tangents[i]) {
                let scale = [
                    w as f64 / self.linear.w as f64,
                    h as f64 / self.linear.h as f64,
                ];
                let source_length = (tangent[0] / scale[0].max(1e-9))
                    .hypot(tangent[1] / scale[1].max(1e-9))
                    .max(1e-9);
                let tangent = [
                    tangent[0] / scale[0].max(1e-9) / source_length,
                    tangent[1] / scale[1].max(1e-9) / source_length,
                ];
                let normal_scale = scale[0] * scale[1]
                    / (tangent[0] * scale[0])
                        .hypot(tangent[1] * scale[1])
                        .max(1e-9);
                strokes.footprint[i] = (self.widths[owner] * normal_scale * 0.5 + 1.).max(2.);
            }
            // The reduced-edge floor is deliberately local. It cannot make a
            // weak long path strong away from a supported target boundary.
            strokes.importance[i] = strokes.importance[i].max((edge * 0.82).clamp(0., 0.98));
        }
        Ok(())
    }

    fn resize_over_fill(
        &self,
        original: &RgbaImage,
        fill_source: &RgbaImage,
        request: FillResize,
        cancel: &dyn Cancellation,
    ) -> Result<RgbaImage> {
        let TargetContours { strokes, colors } =
            self.target_contours(original, request.w, request.h, request.aa, cancel)?;
        let strength = self.intrinsic_strengths(&strokes, cancel)?;
        let paint = opacity::apply_pixels(&strokes.coverage, &strength)?;
        let fill_source = color::LinearImage::from_rgba(fill_source);
        let base = lanczos::resize(&fill_source, request.w as usize, request.h as usize, cancel)?;
        let fill = if let Some(silhouette) = &self.silhouette {
            let source_coverage =
                silhouette.coverage(request.w as usize, request.h as usize, cancel)?;
            let coverage = silhouette.target_coverage(&source_coverage, request.aa, cancel)?;
            let mut pixels = Vec::with_capacity(base.pixels.len());
            for (i, (pixel, &support)) in base.pixels.iter().zip(&coverage).enumerate() {
                if i % 4096 == 0 {
                    cancel.check()?;
                }
                pixels.push([pixel[0], pixel[1], pixel[2], pixel[3] * support]);
            }
            color::LinearImage {
                w: request.w as usize,
                h: request.h as usize,
                pixels,
            }
        } else {
            base
        };
        let colors = match request.outline_color {
            OutlineColor::OriginalInk => colors,
            OutlineColor::DarkenedFill { luminance } => {
                let luminance = if luminance.is_finite() {
                    luminance.clamp(0., 1.)
                } else {
                    0.
                };
                fill.pixels
                    .iter()
                    .map(|p| [p[0] * luminance, p[1] * luminance, p[2] * luminance])
                    .collect()
            }
        };
        cancel.check()?;
        Ok(paint::composite(&fill, &colors, &paint))
    }
}
#[derive(Default)]
struct Cache {
    prepared: Option<Arc<Prepared>>,
    target: Option<((u32, u32, GameAssetAa), Arc<RgbaImage>)>,
}
/// One source analysis and one target result per preview session. Heavy work
/// stays outside the lock so an obsolete preview can be cancelled promptly.
pub struct Session {
    source: Arc<RgbaImage>,
    cache: Mutex<Cache>,
    memory_limit: u64,
    options: ResizeOptions,
}
impl Session {
    pub fn new(source: Arc<RgbaImage>) -> Self {
        Self::with_options(source, ResizeOptions::default())
    }

    /// Create a cached session with the selected resize behavior.
    #[must_use]
    pub fn with_options(source: Arc<RgbaImage>, options: ResizeOptions) -> Self {
        Self::with_memory_limit_and_options(source, DEFAULT_MEMORY_LIMIT, options)
    }

    /// Create a cached session with a caller-selected working-memory limit.
    pub fn with_memory_limit(source: Arc<RgbaImage>, memory_limit: u64) -> Self {
        Self::with_memory_limit_and_options(source, memory_limit, ResizeOptions::default())
    }

    /// Create a cached session with a caller-selected memory limit and resize
    /// behavior.
    #[must_use]
    pub fn with_memory_limit_and_options(
        source: Arc<RgbaImage>,
        memory_limit: u64,
        options: ResizeOptions,
    ) -> Self {
        Self {
            source,
            memory_limit,
            options,
            cache: Mutex::new(Cache::default()),
        }
    }

    fn prepared(&self, cancel: &dyn Cancellation) -> Result<Arc<Prepared>> {
        if let Some(prepared) = self
            .cache
            .lock()
            .expect("Game Asset cache poisoned")
            .prepared
            .clone()
        {
            return Ok(prepared);
        }
        let prepared = Arc::new(Prepared::with_options(&self.source, self.options, cancel)?);
        cancel.check()?;
        self.cache
            .lock()
            .expect("Game Asset cache poisoned")
            .prepared = Some(prepared.clone());
        Ok(prepared)
    }

    /// Analyze the original artwork before an external background remover
    /// changes its alpha or colours.
    pub fn prepare(&self, cancel: &dyn Cancellation) -> Result<()> {
        cancel.check()?;
        let (sw, sh) = self.source.dimensions();
        if sw == 0 || sh == 0 {
            return Err(Error::InvalidDimensions);
        }
        check_working_set_budget(sw, sh, 1, 1, self.memory_limit)?;
        self.prepared(cancel)?;
        cancel.check()
    }

    /// Render the vector-fitted source traces as a white-background diagnostic
    /// mask with grayscale weighted by each contour's established opacity.
    pub fn polished_contour_mask(
        &self,
        w: u32,
        h: u32,
        cancel: &dyn Cancellation,
    ) -> Result<GrayImage> {
        cancel.check()?;
        let (sw, sh) = self.source.dimensions();
        if w == 0 || h == 0 || sw == 0 || sh == 0 || w > sw || h > sh {
            return Err(Error::InvalidDimensions);
        }
        check_working_set_budget(sw, sh, w, h, self.memory_limit)?;
        let mask = self
            .prepared(cancel)?
            .polished_contour_mask(&self.source, w, h, cancel)?;
        cancel.check()?;
        Ok(mask)
    }

    /// Return the canonical, foreground-supported target contour core with
    /// grayscale weighted by each contour's established intrinsic opacity.
    pub fn foreground_contour_mask(
        &self,
        foreground: &RgbaImage,
        w: u32,
        h: u32,
        aa: GameAssetAa,
        cancel: &dyn Cancellation,
    ) -> Result<GrayImage> {
        cancel.check()?;
        let (sw, sh) = self.source.dimensions();
        if foreground.dimensions() != (sw, sh)
            || w == 0
            || h == 0
            || sw == 0
            || sh == 0
            || w > sw
            || h > sh
        {
            return Err(Error::InvalidDimensions);
        }
        check_foreground_working_set_budget(sw, sh, w, h, self.memory_limit)?;
        let mask = self.prepared(cancel)?.foreground_contour_mask(
            &self.source,
            foreground,
            w,
            h,
            aa,
            cancel,
        )?;
        cancel.check()?;
        Ok(mask)
    }
    pub fn resize(
        &self,
        w: u32,
        h: u32,
        aa: GameAssetAa,
        cancel: &dyn Cancellation,
    ) -> Result<RgbaImage> {
        cancel.check()?;
        let (sw, sh) = self.source.dimensions();
        if w == 0 || h == 0 || sw == 0 || sh == 0 || w > sw || h > sh {
            return Err(Error::InvalidDimensions);
        }
        if (w, h) == (sw, sh) {
            return Ok((*self.source).clone());
        }
        check_working_set_budget(sw, sh, w, h, self.memory_limit)?;
        let cached = {
            let cache = self.cache.lock().expect("Game Asset cache poisoned");
            if let Some((key, result)) = &cache.target
                && *key == (w, h, aa)
            {
                return Ok((**result).clone());
            }
            cache.prepared.clone()
        };
        let prepared = cached.map(Ok).unwrap_or_else(|| self.prepared(cancel))?;
        let result = prepared.resize(&self.source, w, h, aa, cancel)?;
        cancel.check()?;
        self.cache.lock().expect("Game Asset cache poisoned").target =
            Some(((w, h, aa), Arc::new(result.clone())));
        Ok(result)
    }

    /// Resize a background-removed foreground while retaining contours and
    /// source-width ink measured from the original session artwork.
    pub fn resize_with_foreground(
        &self,
        foreground: &RgbaImage,
        w: u32,
        h: u32,
        aa: GameAssetAa,
        cancel: &dyn Cancellation,
    ) -> Result<RgbaImage> {
        cancel.check()?;
        let (sw, sh) = self.source.dimensions();
        if foreground.dimensions() != (sw, sh)
            || w == 0
            || h == 0
            || sw == 0
            || sh == 0
            || w > sw
            || h > sh
        {
            return Err(Error::InvalidDimensions);
        }
        if (w, h) == (sw, sh) {
            return Ok(foreground.clone());
        }
        check_foreground_working_set_budget(sw, sh, w, h, self.memory_limit)?;
        let prepared = self.prepared(cancel)?;
        let result = prepared.resize_with_foreground(&self.source, foreground, w, h, aa, cancel)?;
        cancel.check()?;
        Ok(result)
    }

    /// Resize an externally extracted foreground and paint detected source
    /// contours with the requested alpha. `contour_opacity=0.0` returns the
    /// established foreground fill; `1.0` includes full contour paint and its
    /// post-paint fringe repair.
    pub fn resize_with_foreground_opacity(
        &self,
        foreground: &RgbaImage,
        w: u32,
        h: u32,
        aa: GameAssetAa,
        contour_opacity: f64,
        cancel: &dyn Cancellation,
    ) -> Result<RgbaImage> {
        cancel.check()?;
        if !contour_opacity.is_finite() || !(0.0..=1.0).contains(&contour_opacity) {
            return Err(Error::Scaling(
                "foreground contour opacity must be finite and within 0.0..=1.0".into(),
            ));
        }
        let (sw, sh) = self.source.dimensions();
        if foreground.dimensions() != (sw, sh)
            || w == 0
            || h == 0
            || sw == 0
            || sh == 0
            || w > sw
            || h > sh
        {
            return Err(Error::InvalidDimensions);
        }
        if (w, h) == (sw, sh) {
            return Ok(foreground.clone());
        }
        check_foreground_working_set_budget(sw, sh, w, h, self.memory_limit)?;
        let result = self.prepared(cancel)?.resize_with_foreground_opacity(
            &self.source,
            foreground,
            w,
            h,
            aa,
            contour_opacity,
            cancel,
        )?;
        cancel.check()?;
        Ok(result)
    }

    /// Resize an aligned extracted foreground using its visible colours for
    /// original-geometry ink, while leaving the resampled fill unchanged.
    ///
    /// `brightness` scales only contour RGB in displayed sRGB space and must
    /// be finite in the inclusive range `0.0..=1.0`. Foreground pixels with
    /// no alpha-supported donor suppress their corresponding contour pixels.
    pub fn resize_with_foreground_ink(
        &self,
        foreground: &RgbaImage,
        w: u32,
        h: u32,
        aa: GameAssetAa,
        brightness: f64,
        cancel: &dyn Cancellation,
    ) -> Result<RgbaImage> {
        cancel.check()?;
        if !brightness.is_finite() || !(0.0..=1.0).contains(&brightness) {
            return Err(Error::Scaling(
                "foreground ink brightness must be finite and within 0.0..=1.0".into(),
            ));
        }
        let (sw, sh) = self.source.dimensions();
        if foreground.dimensions() != (sw, sh)
            || w == 0
            || h == 0
            || sw == 0
            || sh == 0
            || w > sw
            || h > sh
        {
            return Err(Error::InvalidDimensions);
        }
        if (w, h) == (sw, sh) {
            return Ok(foreground.clone());
        }
        check_foreground_working_set_budget(sw, sh, w, h, self.memory_limit)?;
        let prepared = self.prepared(cancel)?;
        let result = prepared.resize_with_foreground_ink(
            &self.source,
            foreground,
            ForegroundInkResize {
                w,
                h,
                aa,
                brightness,
            },
            cancel,
        )?;
        cancel.check()?;
        Ok(result)
    }
}

/// Reduce a borrowed image once, avoiding a cloned source buffer or session cache.
pub fn resize(
    image: &RgbaImage,
    width: u32,
    height: u32,
    aa: GameAssetAa,
    cancel: &dyn Cancellation,
) -> Result<RgbaImage> {
    resize_with_options(image, width, height, aa, cancel, ResizeOptions::default())
}

/// Reduce a borrowed image once with the selected resize behavior.
pub fn resize_with_options(
    image: &RgbaImage,
    width: u32,
    height: u32,
    aa: GameAssetAa,
    cancel: &dyn Cancellation,
    options: ResizeOptions,
) -> Result<RgbaImage> {
    resize_with_memory_limit_and_options(
        image,
        width,
        height,
        aa,
        cancel,
        DEFAULT_MEMORY_LIMIT,
        options,
    )
}

/// Single-shot reduction with a caller-selected working-memory budget.
pub fn resize_with_memory_limit(
    image: &RgbaImage,
    width: u32,
    height: u32,
    aa: GameAssetAa,
    cancel: &dyn Cancellation,
    memory_limit: u64,
) -> Result<RgbaImage> {
    resize_with_memory_limit_and_options(
        image,
        width,
        height,
        aa,
        cancel,
        memory_limit,
        ResizeOptions::default(),
    )
}

/// Single-shot reduction with a caller-selected working-memory budget and
/// resize behavior.
pub fn resize_with_memory_limit_and_options(
    image: &RgbaImage,
    width: u32,
    height: u32,
    aa: GameAssetAa,
    cancel: &dyn Cancellation,
    memory_limit: u64,
    options: ResizeOptions,
) -> Result<RgbaImage> {
    cancel.check()?;
    let (sw, sh) = image.dimensions();
    if sw == 0 || sh == 0 || width == 0 || height == 0 || width > sw || height > sh {
        return Err(Error::InvalidDimensions);
    }
    if (sw, sh) == (width, height) {
        return Ok(image.clone());
    }
    check_working_set_budget(sw, sh, width, height, memory_limit)?;
    let prepared = Prepared::with_options(image, options, cancel)?;
    let output = prepared.resize(image, width, height, aa, cancel)?;
    cancel.check()?;
    Ok(output)
}

/// Downscale an externally generated, outline-free fill while tracing contours
/// from `original` and repainting them on top.
///
/// The two sources must have identical dimensions.  This keeps Qwen or another
/// editor's colour-only result aligned with contours traced from the untouched
/// artwork.  The fill itself is resampled in linear light with Lanczos3.
pub fn resize_with_outline_free_fill(
    original: &RgbaImage,
    outline_free_fill: &RgbaImage,
    width: u32,
    height: u32,
    aa: GameAssetAa,
    outline_color: OutlineColor,
    cancel: &dyn Cancellation,
) -> Result<RgbaImage> {
    cancel.check()?;
    if original.dimensions() != outline_free_fill.dimensions() {
        return Err(Error::InvalidDimensions);
    }
    let (sw, sh) = original.dimensions();
    if sw == 0 || sh == 0 || width == 0 || height == 0 || width > sw || height > sh {
        return Err(Error::InvalidDimensions);
    }
    check_working_set_budget(sw, sh, width, height, DEFAULT_MEMORY_LIMIT)?;
    let prepared = Prepared::new(original, cancel)?;
    prepared.resize_over_fill(
        original,
        outline_free_fill,
        FillResize {
            w: width,
            h: height,
            aa,
            outline_color,
        },
        cancel,
    )
}
