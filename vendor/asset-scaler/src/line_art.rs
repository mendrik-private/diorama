//! Line-art composition at the target size.
//!
//! The application supplies four target-sized, aligned layers: grayscale line
//! art (white is no ink), an opaque fill without ink contours, the alpha of
//! the result, and the original's colours as a reference. Nothing is
//! resampled here. The fill's local colour is first restored towards the
//! reference by an edge-aware correction; the line art, sharpened with an
//! unsharp mask of radius 1, then multiplies it in 8-bit sRGB, like GIMP's
//! Multiply layer mode, and the alpha is attached unchanged.
use crate::{Cancellation, DEFAULT_MEMORY_LIMIT, Error, Result};
use image::{
    GrayImage, ImageBuffer, Rgb, RgbImage, Rgba, RgbaImage,
    imageops::{FilterType, resize},
};

/// Target-sized working set: the four layers, the line art's blur and
/// horizontal pass, the sharpened copy, the cleaning's colour and weight
/// planes (5 × 8 B), and the output.
const TARGET_BYTES: u64 = 96;
/// Gaussian standard deviation of the unsharp mask, in target pixels.
const SHARPEN_RADIUS: f32 = 1.;
/// Kernel half-width: the Gaussian is truncated at three standard deviations.
const KERNEL_RADIUS: usize = 3;
/// Pixels at least this opaque can keep the fill's colour.
const OPAQUE_ALPHA: u8 = 250;
/// Fill colours closer than this (Euclidean, in 8-bit RGB) to the fill's
/// background are background showing through, not the subject.
const BACKGROUND_DISTANCE: f64 = 40.;
/// Line art brighter than this throughout a 3×3 neighbourhood is free of
/// ink; only there does the colour restoration compare colours.
const INK_FREE_THRESHOLD: u8 = 200;
/// Standard deviation, in pixels, of the colour restoration's spatial
/// Gaussian at a working grid whose shorter side is 128 pixels; it scales
/// with that side.
const RESTORE_SIGMA_AT_128: f64 = 3.;
/// The restoration is measured on a working grid whose shorter side is at
/// most this, and its correction is interpolated up to the target.
const RESTORE_GRID_SIDE: u32 = 128;
/// Standard deviation, in 8-bit RGB, of the restoration's range Gaussian:
/// only fill colours this close to a pixel's own lend it their correction.
const RESTORE_RANGE_SIGMA: f64 = 25.;
/// The share of the full spatial kernel the similar, valid samples must
/// cover for the full correction; with less, it is scaled down.
const RESTORE_MIN_SHARE: f64 = 0.15;
/// The restoration's window spans this many spatial standard deviations.
const RESTORE_WINDOW: f64 = 3.;
/// Unsharp-mask strength in percent, clamped to 0–100; the default is 40%.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Strength(u8);

impl Strength {
    pub const fn new(percent: u8) -> Self {
        Self(if percent > 100 { 100 } else { percent })
    }

    pub const fn percent(self) -> u8 {
        self.0
    }

    /// The unsharp-mask amount: 0.5 at 0%, 1.5 at 40% and 3.0 at 100%.
    pub fn amount(self) -> f32 {
        0.5 + 2.5 * f32::from(self.0) / 100.
    }
}

impl Default for Strength {
    fn default() -> Self {
        Self(40)
    }
}

/// Target-sized, aligned layers of one result. The fill is cleaned and the
/// line art's blur computed once, so a strength change only re-sharpens and
/// multiplies.
pub struct LineArtLayers {
    line_art: GrayImage,
    fill: RgbImage,
    alpha: GrayImage,
    blurred: Vec<f32>,
}

impl LineArtLayers {
    /// All layers must have the same, non-zero dimensions. `alpha` is the
    /// result's straight alpha, and `reference` the original's colours that
    /// the fill's local colour is restored towards (see [`clean_fill`]).
    pub fn new(
        line_art: GrayImage,
        fill: RgbImage,
        alpha: GrayImage,
        reference: &RgbImage,
        cancel: &dyn Cancellation,
    ) -> Result<Self> {
        let (w, h) = line_art.dimensions();
        if w == 0
            || h == 0
            || fill.dimensions() != (w, h)
            || alpha.dimensions() != (w, h)
            || reference.dimensions() != (w, h)
        {
            return Err(Error::InvalidDimensions);
        }
        Self::preflight((w, h))?;
        let reference = reference.pixels().map(|pixel| pixel.0).collect::<Vec<_>>();
        let fill = clean_fill(&fill, &line_art, alpha.as_raw(), &reference, cancel)?;
        let blurred = gaussian_blur(&line_art, cancel)?;
        Ok(Self {
            line_art,
            fill,
            alpha,
            blurred,
        })
    }

    /// Whether layers of `dimensions` fit the working-memory limit; callers
    /// check this before generating them.
    pub fn preflight((w, h): (u32, u32)) -> Result<()> {
        if w == 0 || h == 0 {
            return Err(Error::InvalidDimensions);
        }
        let estimate = u64::from(w)
            .saturating_mul(u64::from(h))
            .saturating_mul(TARGET_BYTES);
        if estimate > DEFAULT_MEMORY_LIMIT {
            return Err(Error::GameAssetMemoryLimit {
                limit_bytes: DEFAULT_MEMORY_LIMIT,
            });
        }
        Ok(())
    }

    pub fn dimensions(&self) -> (u32, u32) {
        self.line_art.dimensions()
    }

    /// The line art as it is multiplied: sharpened at `strength`.
    pub fn line_art(&self, strength: Strength, cancel: &dyn Cancellation) -> Result<GrayImage> {
        cancel.check()?;
        sharpen(&self.line_art, &self.blurred, strength.amount(), cancel)
    }

    /// `rgb = round(cleaned fill · sharpened line art / 255)` with the
    /// layers' alpha, in straight alpha.
    pub fn compose(&self, strength: Strength, cancel: &dyn Cancellation) -> Result<RgbaImage> {
        let line_art = self.line_art(strength, cancel)?;
        multiply(&self.fill, &line_art, self.alpha.as_raw(), cancel)
    }
}

/// The fill's own background colour: the per-channel median of the fill
/// where the foreground is fully transparent, or white if it is nowhere.
fn fill_background(fill: &RgbImage, alpha: &[u8]) -> [f64; 3] {
    std::array::from_fn(|channel| {
        let mut values = fill
            .pixels()
            .zip(alpha)
            .filter(|(_, alpha)| **alpha == 0)
            .map(|(pixel, _)| pixel[channel])
            .collect::<Vec<_>>();
        if values.is_empty() {
            return 255.;
        }
        values.sort_unstable();
        let middle = values.len() / 2;
        if values.len() % 2 == 1 {
            f64::from(values[middle])
        } else {
            (f64::from(values[middle - 1]) + f64::from(values[middle])) / 2.
        }
    })
}

/// Restore the fill's local colour towards `reference`, the original's
/// colours, before it is multiplied: by the edge-aware correction of
/// [`restoration`], weighted by the valid, ink-free pixels. A pixel is valid
/// where `alpha` ≥ `OPAQUE_ALPHA` and its colour is at least
/// `BACKGROUND_DISTANCE` from the fill's own background
/// ([`fill_background`]), and ink-free where the unsharpened line art stays
/// above `INK_FREE_THRESHOLD` throughout a 3×3 neighbourhood. Clamped and
/// rounded once.
fn clean_fill(
    fill: &RgbImage,
    line_art: &GrayImage,
    alpha: &[u8],
    reference: &[[u8; 3]],
    cancel: &dyn Cancellation,
) -> Result<RgbImage> {
    let (w, h) = (fill.width() as usize, fill.height() as usize);
    if line_art.dimensions() != fill.dimensions()
        || alpha.len() != w * h
        || reference.len() != w * h
    {
        return Err(Error::InvalidDimensions);
    }
    let background = fill_background(fill, alpha);
    let mut colours: [Vec<f64>; 3] = std::array::from_fn(|channel| {
        fill.pixels()
            .map(|pixel| f64::from(pixel[channel]))
            .collect()
    });
    let valid = (0..w * h)
        .map(|i| {
            let distance = (0..3)
                .map(|channel| (colours[channel][i] - background[channel]).powi(2))
                .sum::<f64>()
                .sqrt();
            f64::from(u8::from(
                alpha[i] >= OPAQUE_ALPHA && distance >= BACKGROUND_DISTANCE,
            ))
        })
        .collect::<Vec<_>>();
    cancel.check()?;

    // Restore the local colour from valid, ink-free samples.
    let lines = line_art.as_raw();
    let weight = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let ink_free = (y.saturating_sub(1)..=(y + 1).min(h - 1)).all(|ny| {
                (x.saturating_sub(1)..=(x + 1).min(w - 1))
                    .all(|nx| lines[ny * w + nx] > INK_FREE_THRESHOLD)
            });
            if ink_free { valid[i] } else { 0. }
        })
        .collect::<Vec<_>>();
    let correction = restoration(
        &colours,
        reference,
        &weight,
        (fill.width(), fill.height()),
        RESTORE_GRID_SIDE,
        cancel,
    )?;
    for (colour, correction) in colours.iter_mut().zip(&correction) {
        for (colour, correction) in colour.iter_mut().zip(correction) {
            *colour += correction;
        }
    }
    cancel.check()?;
    let mut cleaned = RgbImage::new(fill.width(), fill.height());
    for (i, pixel) in cleaned.pixels_mut().enumerate() {
        for channel in 0..3 {
            pixel[channel] = colours[channel][i].clamp(0., 255.).round() as u8;
        }
    }
    Ok(cleaned)
}

/// The restoration's spatial standard deviation on a grid of `(w, h)`.
fn restore_sigma((w, h): (u32, u32)) -> f64 {
    (RESTORE_SIGMA_AT_128 * f64::from(w.min(h)) / 128.).max(1.)
}

/// The per-channel colour correction for a fill of `size`, measured by a
/// joint bilateral filter and returned at `size`.
///
/// It is measured on a working grid whose shorter side is at most
/// `grid_side` (the fill itself if it is not larger): the fill, the
/// reference and the weight are reduced to it with Lanczos, and the
/// correction is interpolated back up bilinearly. On the grid, for each
/// pixel p over the neighbours q within `RESTORE_WINDOW` · σ (mirrored at
/// the borders), with σ from [`restore_sigma`]:
///
/// - `k(p, q) = exp(−|q − p|² / 2σ²) · w(q) · exp(−‖fill(q) − fill(p)‖² /
///   2·RESTORE_RANGE_SIGMA²)`, so only similar fill colours contribute;
/// - `corr(p) = Σ k · (reference(q) − fill(q)) / Σ k`;
/// - the correction is `gain · corr` with `gain = clamp(Σ k /
///   (RESTORE_MIN_SHARE · Σ exp(−|q − p|² / 2σ²)), 0, 1)` over the full
///   spatial window, so thin features without similar samples of their
///   own are not tinted by their surroundings.
fn restoration(
    colours: &[Vec<f64>; 3],
    reference: &[[u8; 3]],
    weight: &[f64],
    (w, h): (u32, u32),
    grid_side: u32,
    cancel: &dyn Cancellation,
) -> Result<[Vec<f64>; 3]> {
    let scale = (f64::from(grid_side) / f64::from(w.min(h))).min(1.);
    let grid = (
        ((f64::from(w) * scale).round() as u32).max(1),
        ((f64::from(h) * scale).round() as u32).max(1),
    );
    let pixels = (w * h) as usize;
    // The fill with the weight, and the reference, on the working grid,
    // reduced with Lanczos where it is smaller. The image crate's float
    // resampling clamps to 0–1, so colours are resampled normalized.
    let fill_and_weight = (0..pixels)
        .flat_map(|i| {
            [
                (colours[0][i] / 255.) as f32,
                (colours[1][i] / 255.) as f32,
                (colours[2][i] / 255.) as f32,
                weight[i] as f32,
            ]
        })
        .collect::<Vec<_>>();
    let fill_and_weight = ImageBuffer::<Rgba<f32>, _>::from_raw(w, h, fill_and_weight)
        .expect("the planes have the fill's size");
    let reference = ImageBuffer::<Rgb<f32>, _>::from_raw(
        w,
        h,
        reference
            .iter()
            .flat_map(|colour| colour.map(|value| f32::from(value) / 255.))
            .collect(),
    )
    .expect("the reference has the fill's size");
    let (fill_and_weight, reference) = if grid == (w, h) {
        (fill_and_weight, reference)
    } else {
        (
            resize(&fill_and_weight, grid.0, grid.1, FilterType::Lanczos3),
            resize(&reference, grid.0, grid.1, FilterType::Lanczos3),
        )
    };
    let fill_and_weight = fill_and_weight.into_raw();
    let target = reference.into_raw();
    cancel.check()?;

    let sigma = restore_sigma(grid);
    let radius = (RESTORE_WINDOW * sigma).round() as i64;
    let spatial = (-radius..=radius)
        .map(|offset| (-(offset * offset) as f64 / (2. * sigma * sigma)).exp())
        .collect::<Vec<_>>();
    let full: f64 = spatial.iter().sum::<f64>().powi(2);
    let spatial = spatial
        .iter()
        .map(|&weight| weight as f32)
        .collect::<Vec<_>>();
    // Normalized colours: the range Gaussian's scale in (0–1)².
    let range_scale = (-(255. * 255.) / (2. * RESTORE_RANGE_SIGMA * RESTORE_RANGE_SIGMA)) as f32;
    let (gw, gh) = (i64::from(grid.0), i64::from(grid.1));
    let reflect = |index: i64, len: i64| {
        let period = 2 * len;
        let index = index.rem_euclid(period);
        (if index < len {
            index
        } else {
            period - 1 - index
        }) as usize
    };
    let columns = (0..gw)
        .map(|x| {
            (-radius..=radius)
                .map(|dx| reflect(x + dx, gw))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let gw = gw as usize;
    let mut field = vec![0_f32; gw * gh as usize * 3];
    for y in 0..gh {
        cancel.check()?;
        let rows = (-radius..=radius)
            .map(|dy| reflect(y + dy, gh) * gw)
            .collect::<Vec<_>>();
        for (x, columns) in columns.iter().enumerate() {
            let p = y as usize * gw + x;
            let centre = &fill_and_weight[p * 4..p * 4 + 3];
            let mut den = 0_f64;
            let mut sum = [0_f64; 3];
            for (&row, &spatial_y) in rows.iter().zip(&spatial) {
                for (&column, &spatial_x) in columns.iter().zip(&spatial) {
                    let q = row + column;
                    let sample = &fill_and_weight[q * 4..q * 4 + 4];
                    let w = sample[3];
                    if w <= 0. {
                        continue;
                    }
                    let distance = (sample[0] - centre[0]).powi(2)
                        + (sample[1] - centre[1]).powi(2)
                        + (sample[2] - centre[2]).powi(2);
                    let k = f64::from(spatial_y * spatial_x * w * (distance * range_scale).exp());
                    den += k;
                    let target = &target[q * 3..q * 3 + 3];
                    for c in 0..3 {
                        sum[c] += k * f64::from(target[c] - sample[c]);
                    }
                }
            }
            if den > 0. {
                let gain = (den / (RESTORE_MIN_SHARE * full)).clamp(0., 1.);
                for c in 0..3 {
                    field[p * 3 + c] = (gain * sum[c] / den) as f32;
                }
            }
        }
    }
    // Back to the fill's size, bilinearly; the correction is shifted into
    // 0–1 for the resampling.
    let field = if grid == (w, h) {
        field
    } else {
        let shifted = field
            .iter()
            .map(|value| (value + 1.) / 2.)
            .collect::<Vec<f32>>();
        let image = ImageBuffer::<Rgb<f32>, _>::from_raw(grid.0, grid.1, shifted)
            .expect("the field has the grid's size");
        resize(&image, w, h, FilterType::Triangle)
            .into_raw()
            .into_iter()
            .map(|value| value * 2. - 1.)
            .collect()
    };
    debug_assert_eq!(field.len(), pixels * 3);
    Ok(std::array::from_fn(|channel| {
        field
            .iter()
            .skip(channel)
            .step_by(3)
            .map(|&value| f64::from(value) * 255.)
            .collect()
    }))
}

fn gaussian_kernel() -> [f32; 2 * KERNEL_RADIUS + 1] {
    let mut kernel = std::array::from_fn(|i| {
        let offset = i as f32 - KERNEL_RADIUS as f32;
        (-offset * offset / (2. * SHARPEN_RADIUS * SHARPEN_RADIUS)).exp()
    });
    let sum: f32 = kernel.iter().sum();
    for weight in &mut kernel {
        *weight /= sum;
    }
    kernel
}

/// Separable Gaussian blur of the stored 8-bit values with clamp-to-edge.
fn gaussian_blur(image: &GrayImage, cancel: &dyn Cancellation) -> Result<Vec<f32>> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let kernel = gaussian_kernel();
    let pixels = image.as_raw();
    let tap = |position: usize, offset: usize, len: usize| {
        (position + offset)
            .saturating_sub(KERNEL_RADIUS)
            .min(len - 1)
    };
    let mut horizontal = vec![0f32; w * h];
    for y in 0..h {
        cancel.check()?;
        let row = &pixels[y * w..(y + 1) * w];
        for x in 0..w {
            horizontal[y * w + x] = kernel
                .iter()
                .enumerate()
                .map(|(offset, weight)| weight * f32::from(row[tap(x, offset, w)]))
                .sum();
        }
    }
    let mut blurred = vec![0f32; w * h];
    for y in 0..h {
        cancel.check()?;
        for x in 0..w {
            blurred[y * w + x] = kernel
                .iter()
                .enumerate()
                .map(|(offset, weight)| weight * horizontal[tap(y, offset, h) * w + x])
                .sum();
        }
    }
    Ok(blurred)
}

/// GEGL-style unsharp mask with threshold 0:
/// `clamp(x + amount · (x − blur(x)), 0, 255)`, rounded once.
fn sharpen(
    line_art: &GrayImage,
    blurred: &[f32],
    amount: f32,
    cancel: &dyn Cancellation,
) -> Result<GrayImage> {
    let mut pixels = Vec::with_capacity(blurred.len());
    for (i, (&value, &blur)) in line_art.as_raw().iter().zip(blurred).enumerate() {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        let value = f32::from(value);
        pixels.push((value + amount * (value - blur)).clamp(0., 255.).round() as u8);
    }
    cancel.check()?;
    GrayImage::from_vec(line_art.width(), line_art.height(), pixels)
        .ok_or_else(|| Error::Scaling("Invalid line-art dimensions".into()))
}

/// GIMP Multiply on encoded 8-bit channels, `round(fill · line / 255)` for
/// RGB, with `alpha` attached as straight alpha.
fn multiply(
    fill: &RgbImage,
    line_art: &GrayImage,
    alpha: &[u8],
    cancel: &dyn Cancellation,
) -> Result<RgbaImage> {
    let (w, h) = fill.dimensions();
    if line_art.dimensions() != (w, h) || alpha.len() != w as usize * h as usize {
        return Err(Error::InvalidDimensions);
    }
    let mut output = RgbaImage::new(w, h);
    for (i, (((output, fill), line), &alpha)) in output
        .pixels_mut()
        .zip(fill.pixels())
        .zip(line_art.pixels())
        .zip(alpha)
        .enumerate()
    {
        if i.is_multiple_of(4096) {
            cancel.check()?;
        }
        let line = u16::from(line[0]);
        let product = |channel: u8| ((u16::from(channel) * line + 127) / 255) as u8;
        output.0 = [product(fill[0]), product(fill[1]), product(fill[2]), alpha];
    }
    cancel.check()?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CancellationToken;
    use image::Luma;

    const SIZE: (u32, u32) = (24, 20);

    /// A soft-edged opaque square: 0 outside, 128 on two borders, 255 inside.
    fn alpha() -> GrayImage {
        GrayImage::from_fn(SIZE.0, SIZE.1, |x, y| {
            Luma([if !(4..20).contains(&x) || !(3..17).contains(&y) {
                0
            } else if x == 4 || y == 3 {
                128
            } else {
                255
            }])
        })
    }

    /// A fill whose RGB differs from the reference's.
    fn fill() -> RgbImage {
        RgbImage::from_fn(SIZE.0, SIZE.1, |x, y| {
            Rgb([(x * 9) as u8, (y * 11 + 3) as u8, 255 - (x + y) as u8])
        })
    }

    fn reference() -> RgbImage {
        RgbImage::from_fn(SIZE.0, SIZE.1, |x, _| {
            Rgb([200, 90, 60 + (x as u8 % 7) * 10])
        })
    }

    fn layers(line_art: GrayImage) -> LineArtLayers {
        LineArtLayers::new(line_art, fill(), alpha(), &reference(), &|| false).unwrap()
    }

    /// Sharpen line art of any size as the layers do.
    fn sharpened(line_art: &GrayImage, strength: Strength) -> GrayImage {
        let cancel = CancellationToken::default();
        let blurred = gaussian_blur(line_art, &cancel).unwrap();
        sharpen(line_art, &blurred, strength.amount(), &cancel).unwrap()
    }

    fn flat(value: u8) -> GrayImage {
        GrayImage::from_pixel(SIZE.0, SIZE.1, Luma([value]))
    }

    /// The fill as it is multiplied under `line_art`: cleaned against the
    /// reference.
    fn expected_fill(line_art: &GrayImage) -> RgbImage {
        let reference = reference()
            .pixels()
            .map(|pixel| pixel.0)
            .collect::<Vec<_>>();
        clean(&fill(), line_art, alpha().as_raw(), &reference)
    }

    fn clean(
        fill: &RgbImage,
        line_art: &GrayImage,
        alpha: &[u8],
        reference: &[[u8; 3]],
    ) -> RgbImage {
        clean_fill(
            fill,
            line_art,
            alpha,
            reference,
            &CancellationToken::default(),
        )
        .unwrap()
    }

    #[test]
    fn strength_maps_percent_to_unsharp_amount() {
        assert_eq!(Strength::new(0).amount(), 0.5);
        assert_eq!(Strength::new(40).amount(), 1.5);
        assert_eq!(Strength::new(100).amount(), 3.0);
        assert_eq!(Strength::default(), Strength::new(40));
        assert_eq!(Strength::new(250).percent(), 100);
    }

    #[test]
    fn unsharp_mask_overshoots_a_step_edge_by_the_hand_computed_amount() {
        // Gaussian σ = 1 truncated at ±3 has normalized tail sums
        // 0.300478 (±1..3), 0.058438 (±2..3) and 0.004433 (±3). A 64|192
        // step at amount 1.5 moves each side by 1.5 · 128 · tail.
        let step = GrayImage::from_fn(12, 3, |x, _| Luma([if x < 6 { 64 } else { 192 }]));
        let sharpened = sharpened(&step, Strength::new(40));
        for y in 0..3 {
            let row: Vec<u8> = (0..12).map(|x| sharpened.get_pixel(x, y)[0]).collect();
            assert_eq!(
                row,
                [64, 64, 64, 63, 53, 6, 250, 203, 193, 192, 192, 192],
                "row {y}"
            );
        }
    }

    #[test]
    fn unsharp_mask_leaves_a_flat_image_unchanged() {
        for value in [0, 1, 128, 254, 255] {
            let flat = GrayImage::from_pixel(9, 7, Luma([value]));
            assert_eq!(sharpened(&flat, Strength::new(100)), flat);
        }
    }

    #[test]
    fn white_line_art_leaves_the_cleaned_fill_with_the_given_alpha() {
        let output = layers(flat(255))
            .compose(Strength::new(100), &|| false)
            .unwrap();
        let alpha = alpha();
        assert!(alpha.pixels().any(|alpha| (1..255).contains(&alpha[0])));
        for ((output, fill), alpha) in output
            .pixels()
            .zip(expected_fill(&flat(255)).pixels())
            .zip(alpha.pixels())
        {
            assert_eq!(output.0, [fill[0], fill[1], fill[2], alpha[0]]);
        }
        // Valid, ink-free pixels were restored towards the reference.
        assert_ne!(expected_fill(&flat(255)), fill());
    }

    #[test]
    fn black_line_art_is_black_with_the_given_alpha() {
        let output = layers(flat(0))
            .compose(Strength::default(), &|| false)
            .unwrap();
        for (output, alpha) in output.pixels().zip(alpha().pixels()) {
            assert_eq!(output.0, [0, 0, 0, alpha[0]]);
        }
    }

    #[test]
    fn mid_gray_multiplies_each_channel_exactly() {
        let output = layers(flat(128))
            .compose(Strength::new(0), &|| false)
            .unwrap();
        // Mid-gray line art counts as ink everywhere, so nothing is restored.
        assert_eq!(expected_fill(&flat(128)), fill());
        for ((output, fill), alpha) in output.pixels().zip(fill().pixels()).zip(alpha().pixels()) {
            for channel in 0..3 {
                let expected = (f64::from(fill[channel]) * 128. / 255.).round() as u8;
                assert_eq!(output[channel], expected);
            }
            assert_eq!(output[3], alpha[0]);
        }
        assert_eq!(
            multiply(
                &RgbImage::from_pixel(1, 1, Rgb([200, 255, 1])),
                &GrayImage::from_pixel(1, 1, Luma([128])),
                &[77],
                &CancellationToken::default()
            )
            .unwrap()
            .get_pixel(0, 0)
            .0,
            [100, 128, 1, 77]
        );
    }

    #[test]
    fn the_sharpened_line_art_multiplies_the_cleaned_fill_at_every_strength() {
        let line_art = GrayImage::from_fn(SIZE.0, SIZE.1, |x, y| {
            Luma([if x == 9 || y == 13 { 0 } else { 230 }])
        });
        let layers = layers(line_art.clone());
        let fill = expected_fill(&line_art);
        for strength in [0, 40, 100].map(Strength::new) {
            let sharpened = layers.line_art(strength, &|| false).unwrap();
            let output = layers.compose(strength, &|| false).unwrap();
            for ((output, fill), line) in output.pixels().zip(fill.pixels()).zip(sharpened.pixels())
            {
                for channel in 0..3 {
                    let expected =
                        (f64::from(fill[channel]) * f64::from(line[0]) / 255.).round() as u8;
                    assert_eq!(output[channel], expected);
                }
            }
        }
        assert_ne!(
            layers.line_art(Strength::new(0), &|| false).unwrap(),
            layers.line_art(Strength::new(100), &|| false).unwrap()
        );
    }

    #[test]
    fn dimensions_are_validated() {
        let cancel = CancellationToken::default();
        let (w, h) = SIZE;
        for (line_art, fill, alpha, reference) in [
            ((w, h), (w, h - 1), (w, h), (w, h)),
            ((w, h), (w, h), (w - 1, h), (w, h)),
            ((w, h), (w, h), (w, h), (w, h + 1)),
            ((0, 0), (0, 0), (0, 0), (0, 0)),
        ] {
            assert!(matches!(
                LineArtLayers::new(
                    GrayImage::new(line_art.0, line_art.1),
                    RgbImage::new(fill.0, fill.1),
                    GrayImage::new(alpha.0, alpha.1),
                    &RgbImage::new(reference.0, reference.1),
                    &cancel,
                ),
                Err(Error::InvalidDimensions)
            ));
        }
    }

    #[test]
    fn cancellation_stops_the_cleaning_and_the_composition() {
        let cancelled = CancellationToken::default();
        cancelled.cancel();
        assert!(matches!(
            LineArtLayers::new(flat(255), fill(), alpha(), &reference(), &cancelled),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            layers(flat(255)).compose(Strength::default(), &cancelled),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn the_fill_background_is_the_median_under_full_transparency() {
        let fill = RgbImage::from_fn(5, 1, |x, _| {
            Rgb([[10, 20, 30, 200, 90][x as usize], 7, (x * 10) as u8])
        });
        // Pixels 0, 1, 2 are transparent: medians 20, 7, 10.
        assert_eq!(fill_background(&fill, &[0, 0, 0, 255, 128]), [20., 7., 10.]);
        // An even count averages the middle two.
        assert_eq!(fill_background(&fill, &[0, 0, 255, 0, 255]), [20., 7., 10.]);
        assert_eq!(fill_background(&fill, &[0, 255, 255, 0, 0]), [90., 7., 30.]);
        // Without transparency the background is taken to be white.
        assert_eq!(fill_background(&fill, &[255; 5]), [255.; 3]);
    }

    #[test]
    fn the_restoration_removes_a_local_offset_away_from_ink_only() {
        // The fill is 30 brighter than the foreground. Ink pixels and their
        // neighbours, transparent pixels and background-coloured pixels
        // carry no weight: their wild reference colours must not matter.
        let (w, h) = (40_u32, 32_u32);
        let line_art = GrayImage::from_fn(w, h, |x, _| Luma([if x == 20 { 0 } else { 255 }]));
        let mut alpha = vec![255; (w * h) as usize];
        let fill = RgbImage::from_fn(w, h, |x, y| {
            if y < 3 {
                Rgb([255, 255, 255])
            } else if x > 35 {
                Rgb([250, 252, 248])
            } else {
                Rgb([130, 90, 60])
            }
        });
        let reference = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                if y < 3 || x > 35 || (19..=21).contains(&x) {
                    [255, 0, 255]
                } else {
                    [100, 60, 30]
                }
            })
            .collect::<Vec<_>>();
        // The top rows are transparent white background.
        for value in &mut alpha[..(3 * w) as usize] {
            *value = 0;
        }
        let cleaned = clean(&fill, &line_art, &alpha, &reference);
        for (x, y, pixel) in cleaned.enumerate_pixels() {
            if y >= 6 && x <= 32 && !(19..=21).contains(&x) {
                assert_eq!(pixel.0, [100, 60, 30], "({x}, {y})");
            }
            // The ink column has few samples in reach (σ is 1 pixel here),
            // so its correction is scaled down, never overshooting.
            if y >= 6 && x == 20 {
                assert!((100..130).contains(&pixel[0]), "{pixel:?}");
                assert_eq!(pixel[0] - 100, pixel[1] - 60);
            }
        }
        // Zero weight everywhere (all ink) changes nothing and never
        // divides by zero.
        let ink = GrayImage::from_pixel(w, h, Luma([0]));
        let valid_fill = RgbImage::from_pixel(w, h, Rgb([130, 90, 60]));
        assert_eq!(
            clean(&valid_fill, &ink, &[255; 1280], &reference),
            valid_fill
        );
        assert_eq!(restore_sigma((128, 128)), 3.);
        assert_eq!(restore_sigma((512, 256)), 6.);
        assert_eq!(restore_sigma((32, 32)), 1.);
    }

    /// Target-sized planes for [`restoration`]: a fill, a reference, and a
    /// weight of 1 everywhere except `unweighted`.
    fn planes(
        (w, h): (u32, u32),
        fill: impl Fn(u32, u32) -> [f64; 3],
        reference: impl Fn(u32, u32) -> [u8; 3],
        unweighted: impl Fn(u32, u32) -> bool,
    ) -> ([Vec<f64>; 3], Vec<[u8; 3]>, Vec<f64>) {
        let at = |i: u32| (i % w, i / w);
        let colours = std::array::from_fn(|channel| {
            (0..w * h)
                .map(|i| {
                    let (x, y) = at(i);
                    fill(x, y)[channel]
                })
                .collect()
        });
        let reference = (0..w * h).map(|i| reference(at(i).0, at(i).1)).collect();
        let weight = (0..w * h)
            .map(|i| if unweighted(at(i).0, at(i).1) { 0. } else { 1. })
            .collect();
        (colours, reference, weight)
    }

    #[test]
    fn a_thin_feature_is_not_tinted_by_the_correction_of_its_surroundings() {
        // A green tunic that came out 20 too bright in red and green, and a
        // correct brown strap two pixels wide across it. The strap sits
        // between lines, so it has no weighted samples of its own.
        let size = (48, 48);
        let strap = |x: u32, _: u32| (20..22).contains(&x);
        let (colours, reference, weight) = planes(
            size,
            |x, y| {
                if strap(x, y) {
                    [120., 80., 50.]
                } else {
                    [100., 160., 60.]
                }
            },
            |x, y| {
                if strap(x, y) {
                    [120, 80, 50]
                } else {
                    [80, 140, 60]
                }
            },
            |x, _| (19..23).contains(&x),
        );
        let correction = restoration(&colours, &reference, &weight, size, 128, &|| false).unwrap();
        let at = |x: u32, y: u32| {
            std::array::from_fn::<f64, 3, _>(|c| correction[c][(y * 48 + x) as usize])
        };
        for y in [0, 24, 47] {
            // The strap is not pulled towards the tunic's correction.
            for x in [20, 21] {
                let [r, g, b] = at(x, y);
                assert!(
                    r.abs() < 1. && g.abs() < 1. && b.abs() < 1.,
                    "({x}, {y}): {r} {g} {b}"
                );
            }
            // The tunic, even right next to the strap, is corrected fully.
            for x in [5, 17, 24, 40] {
                let [r, g, b] = at(x, y);
                assert!((r + 20.).abs() < 0.01 && (g + 20.).abs() < 0.01 && b.abs() < 0.01);
            }
        }
    }

    #[test]
    fn little_or_no_confidence_scales_the_correction_down() {
        let size = (128, 128);
        // No weighted sample: no correction, and no NaN.
        let (colours, reference, weight) = planes(
            size,
            |_, _| [100., 100., 100.],
            |_, _| [0, 0, 0],
            |_, _| true,
        );
        let correction = restoration(&colours, &reference, &weight, size, 128, &|| false).unwrap();
        assert!(correction.iter().flatten().all(|&value| value == 0.));
        // A single weighted sample covers only part of the spatial kernel:
        // the correction there is its share of RESTORE_MIN_SHARE.
        let (colours, reference, weight) = planes(
            size,
            |_, _| [100., 100., 100.],
            |_, _| [0, 0, 0],
            |x, y| (x, y) != (60, 60),
        );
        let correction = restoration(&colours, &reference, &weight, size, 128, &|| false).unwrap();
        let sigma = restore_sigma(size);
        let radius = (RESTORE_WINDOW * sigma).round() as i64;
        let full = (-radius..=radius)
            .map(|offset| (-(offset * offset) as f64 / (2. * sigma * sigma)).exp())
            .sum::<f64>()
            .powi(2);
        let gain = 1. / (RESTORE_MIN_SHARE * full);
        assert!(gain < 1.);
        let centre = correction[0][60 * 128 + 60];
        assert!(
            (centre + 100. * gain).abs() < 1e-3,
            "{centre} vs {}",
            -100. * gain
        );
        assert!(correction.iter().flatten().all(|value| value.is_finite()));
    }

    #[test]
    fn the_working_grid_matches_the_direct_correction_for_a_smooth_fill() {
        // At 256² the correction is measured on a 128² grid and
        // interpolated; directly, σ doubles instead.
        let size = (256, 256);
        let (colours, reference, weight) = planes(
            size,
            |x, y| [80. + f64::from(x) / 4., 120. + f64::from(y) / 8., 90.],
            |x, y| {
                [
                    (60. + f64::from(x) / 4.) as u8,
                    (120. + f64::from(y) / 8. + 10.) as u8,
                    (90. - f64::from(x + y) / 32.) as u8,
                ]
            },
            |x, y| (x + y) % 37 == 0,
        );
        let grid = restoration(&colours, &reference, &weight, size, 128, &|| false).unwrap();
        let direct = restoration(&colours, &reference, &weight, size, 256, &|| false).unwrap();
        let (mut total, mut largest) = (0., 0_f64);
        for (grid, direct) in grid.iter().flatten().zip(direct.iter().flatten()) {
            let difference = (grid - direct).abs();
            total += difference;
            largest = largest.max(difference);
        }
        let mean = total / (3. * 256. * 256.);
        assert!(mean < 0.5 && largest < 3., "mean {mean}, largest {largest}");
    }

    /// Prints the time `clean_fill` takes at 128², 512² and 1024²:
    /// `cargo test --lib restoration_timing -- --ignored --nocapture`
    #[test]
    #[ignore = "timing report"]
    fn restoration_timing() {
        for side in [128_u32, 512, 1024] {
            let fill = RgbImage::from_fn(side, side, |x, y| {
                Rgb([(x % 200) as u8 + 20, (y % 180) as u8 + 30, 90])
            });
            let line_art =
                GrayImage::from_fn(side, side, |x, _| Luma([if x % 17 == 0 { 0 } else { 255 }]));
            let alpha = vec![255; (side * side) as usize];
            let reference = vec![[100, 120, 80]; (side * side) as usize];
            let started = std::time::Instant::now();
            clean(&fill, &line_art, &alpha, &reference);
            let whole = started.elapsed();
            let colours = std::array::from_fn(|channel| {
                fill.pixels()
                    .map(|pixel| f64::from(pixel[channel]))
                    .collect()
            });
            let weight = vec![1.; (side * side) as usize];
            let started = std::time::Instant::now();
            restoration(
                &colours,
                &reference,
                &weight,
                (side, side),
                RESTORE_GRID_SIDE,
                &|| false,
            )
            .unwrap();
            eprintln!(
                "{side}²: clean_fill {:.1} ms, of which the restoration {:.1} ms",
                whole.as_secs_f64() * 1e3,
                started.elapsed().as_secs_f64() * 1e3
            );
        }
    }

    #[test]
    fn cleaning_validates_dimensions() {
        let cancel = CancellationToken::default();
        let fill = RgbImage::new(4, 3);
        assert!(matches!(
            clean_fill(
                &fill,
                &GrayImage::new(4, 2),
                &[0; 12],
                &[[0; 3]; 12],
                &cancel
            ),
            Err(Error::InvalidDimensions)
        ));
        assert!(matches!(
            clean_fill(
                &fill,
                &GrayImage::new(4, 3),
                &[0; 11],
                &[[0; 3]; 12],
                &cancel
            ),
            Err(Error::InvalidDimensions)
        ));
    }
}
