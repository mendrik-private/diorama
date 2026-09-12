//! Game Asset v0.1: analyse continuous colour/geometry, render source samples.
//! The renderer never blends or invents colours. See docs/game-asset-scaling.md.

mod analysis;
mod filaments;
mod geometry;
mod gpu;
mod palette_grid;
#[cfg(test)]
mod sampling_experiment;
mod solver;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex};

use image::RgbaImage;

use crate::document::CancellationToken;
use crate::error::{AppError, Result};

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// Prefer dedicated Vulkan wavelets when supported; false is the CPU reference.
    pub gpu_wavelets: bool,
    /// Complete-link CIEDE2000 palette merge threshold; zero keeps every RGB.
    pub palette_delta_e: f32,
    /// Experimental source-colour fill modes and 5x5 target-grid line context.
    /// Disabled by default to preserve nearest-neighbour fills and highlights.
    pub grid_context: bool,
    pub context_weight: f32,
    pub allow_non_uniform: bool,
    pub phases: u32,
    pub alpha_threshold: u8,
    pub orientations: usize,
    pub first_wavelength: f32,
    pub wavelength_multiplier: f32,
    pub log_bandwidth: f32,
    pub channel_weights: [f32; 3],
    pub noise_minimum: f32,
    pub noise_median_multiplier: f32,
    pub tau_colour: f32,
    pub tau_along: f32,
    pub colour_weight: f32,
    pub wavelet_weight: f32,
    pub seed_threshold: f32,
    pub continuation_threshold: f32,
    pub fit_tolerance: f32,
    pub minimum_path_length: f32,
    pub maximum_proposals: usize,
    pub local_sweeps: usize,
    pub search_budget: usize,
    pub gain_margin: f32,
    pub orientation_cost: f32,
    pub evidence_cost: f32,
    pub baseline_cost: f32,
    pub transition_cost: f32,
    pub geometry_cost: f32,
    pub complexity_cost: f32,
    pub coverage_cost: f32,
    pub length_cap: f32,
    pub fit_geometry: bool,
    pub memory_limit: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            gpu_wavelets: true,
            palette_delta_e: 2.0,
            grid_context: false,
            context_weight: 1.4,
            allow_non_uniform: false,
            phases: 4,
            alpha_threshold: 128,
            orientations: 8,
            first_wavelength: 3.0,
            wavelength_multiplier: 2.0,
            log_bandwidth: 0.55,
            channel_weights: [1.0; 3],
            noise_minimum: 0.05,
            noise_median_multiplier: 2.0,
            tau_colour: 12.0,
            tau_along: 12.0,
            colour_weight: 0.65,
            wavelet_weight: 0.35,
            seed_threshold: 0.55,
            continuation_threshold: 0.25,
            fit_tolerance: 0.75,
            minimum_path_length: 2.0,
            maximum_proposals: 8,
            local_sweeps: 2,
            search_budget: 20_000,
            gain_margin: 0.02,
            orientation_cost: 0.2,
            evidence_cost: 0.25,
            baseline_cost: 0.1,
            transition_cost: 0.2,
            geometry_cost: 0.15,
            complexity_cost: 0.02,
            coverage_cost: 1.0,
            length_cap: 32.0,
            fit_geometry: true,
            memory_limit: 512 * 1024 * 1024,
        }
    }
}

impl Options {
    fn validate(&self) -> Result<()> {
        let positive = [
            self.first_wavelength,
            self.tau_colour,
            self.tau_along,
            self.fit_tolerance,
            self.minimum_path_length,
            self.length_cap,
        ];
        let nonnegative = [
            self.palette_delta_e,
            self.context_weight,
            self.noise_minimum,
            self.noise_median_multiplier,
            self.colour_weight,
            self.wavelet_weight,
            self.gain_margin,
            self.orientation_cost,
            self.evidence_cost,
            self.baseline_cost,
            self.transition_cost,
            self.geometry_cost,
            self.complexity_cost,
            self.coverage_cost,
        ];
        if self.phases == 0
            || self.phases > 16
            || self.orientations == 0
            || self.orientations > 16
            || self.alpha_threshold == 0
            || self.maximum_proposals == 0
            || self.maximum_proposals > 8
            || self.local_sweeps > 2
            || self.search_budget == 0
            || !self.wavelength_multiplier.is_finite()
            || self.wavelength_multiplier <= 1.0
            || !(0.0..1.0).contains(&self.log_bandwidth)
            || !positive.iter().all(|v| v.is_finite() && *v > 0.0)
            || !nonnegative
                .iter()
                .chain(&self.channel_weights)
                .all(|v| v.is_finite() && *v >= 0.0)
            || self.channel_weights.iter().sum::<f32>() == 0.0
            || !(0.0..=1.0).contains(&self.seed_threshold)
            || !(0.0..=self.seed_threshold).contains(&self.continuation_threshold)
        {
            return Err(AppError::InvalidDimensions);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    /// Local evidence sample, always inside this output cell's footprint.
    pub source: (u32, u32),
    /// Actual output RGB source when palette merging changes the local colour.
    pub palette_source: Option<(u32, u32)>,
    pub reconstructed: bool,
    pub feature: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diagnostics {
    pub retained: Vec<usize>,
    pub dropped: Vec<usize>,
    pub unresolved: Vec<usize>,
    pub unsupported_gaps: usize,
    pub collisions: usize,
    pub baseline_joins: usize,
    pub joins: usize,
    pub baseline_connectivity_failures: usize,
    pub connectivity_failures: usize,
    pub component_losses: usize,
    pub hole_losses: usize,
    pub search_budget_exhausted: bool,
}

#[derive(Debug)]
pub struct Scaled {
    pub gpu_wavelets: bool,
    pub image: RgbaImage,
    pub provenance: Vec<Provenance>,
    pub diagnostics: Diagnostics,
    pub options: Options,
}

#[derive(Clone, Copy, Debug)]
struct Grid {
    source_width: usize,
    source_height: usize,
    width: usize,
    height: usize,
    sx: f32,
    sy: f32,
}

impl Grid {
    fn new(source: &RgbaImage, width: u32, height: u32, options: &Options) -> Result<Self> {
        options.validate()?;
        if width == 0 || height == 0 || width > source.width() || height > source.height() {
            return Err(AppError::InvalidDimensions);
        }
        // Integer target dimensions may differ by up to half a rounded pixel.
        let expected_h = (f64::from(width) * f64::from(source.height()) / f64::from(source.width()))
            .round()
            .max(1.0) as u32;
        let expected_w = (f64::from(height) * f64::from(source.width())
            / f64::from(source.height()))
        .round()
        .max(1.0) as u32;
        if !options.allow_non_uniform && height != expected_h && width != expected_w {
            return Err(AppError::InvalidDimensions);
        }
        Ok(Self {
            source_width: source.width() as usize,
            source_height: source.height() as usize,
            width: width as usize,
            height: height as usize,
            sx: source.width() as f32 / width as f32,
            sy: source.height() as f32 / height as f32,
        })
    }

    fn project(self, index: usize) -> geometry::Point {
        geometry::Point {
            x: (index % self.source_width) as f32 / self.sx + 0.5 / self.sx - 0.5,
            y: (index / self.source_width) as f32 / self.sy + 0.5 / self.sy - 0.5,
        }
    }

    fn baseline(self, index: usize) -> usize {
        let x = index % self.width;
        let y = index / self.width;
        let i = ((2 * x + 1) as u64 * self.source_width as u64 / (2 * self.width) as u64) as usize;
        let j =
            ((2 * y + 1) as u64 * self.source_height as u64 / (2 * self.height) as u64) as usize;
        j * self.source_width + i
    }

    fn footprint(self, index: usize) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
        let x = (index % self.width) as u64;
        let y = (index / self.width) as u64;
        let w = self.width as u64;
        let h = self.height as u64;
        let sw = self.source_width as u64;
        let sh = self.source_height as u64;
        (
            (x * sw / w) as usize..((x + 1) * sw).div_ceil(w) as usize,
            (y * sh / h) as usize..((y + 1) * sh).div_ceil(h) as usize,
        )
    }
}

pub fn resize(
    source: &RgbaImage,
    width: u32,
    height: u32,
    options: &Options,
    cancellation: &CancellationToken,
) -> Result<Scaled> {
    let grid = Grid::new(source, width, height, options)?;
    cancellation.check()?;
    if source.dimensions() == (width, height) {
        return Ok(identity(source, options));
    }
    let analysis = analysis::Analysis::new(source, grid, options, cancellation)?;
    solver::render(source, grid, &analysis, options, cancellation)
}

fn identity(source: &RgbaImage, options: &Options) -> Scaled {
    Scaled {
        gpu_wavelets: false,
        image: source.clone(),
        provenance: (0..source.as_raw().len() / 4)
            .map(|i| Provenance {
                palette_source: None,
                source: (
                    (i % source.width() as usize) as u32,
                    (i / source.width() as usize) as u32,
                ),
                reconstructed: false,
                feature: None,
            })
            .collect(),
        diagnostics: Diagnostics::default(),
        options: options.clone(),
    }
}

/// Cache source analysis for consecutive preview sizes sharing the same scale
/// bank. Cancellation never publishes partially computed evidence.
pub struct Session {
    source: Arc<RgbaImage>,
    analysis: Mutex<Option<(analysis::ScaleRange, analysis::Analysis)>>,
}

impl Session {
    pub fn new(source: Arc<RgbaImage>) -> Self {
        Self {
            source,
            analysis: Mutex::new(None),
        }
    }

    pub fn resize(
        &self,
        width: u32,
        height: u32,
        options: &Options,
        cancellation: &CancellationToken,
    ) -> Result<Scaled> {
        let grid = Grid::new(&self.source, width, height, options)?;
        cancellation.check()?;
        if self.source.dimensions() == (width, height) {
            return Ok(identity(&self.source, options));
        }
        let range = analysis::ScaleRange::new(grid, options);
        let mut cache = self.analysis.lock().map_err(|_| AppError::Cancelled)?;
        cancellation.check()?;
        if cache.as_ref().is_none_or(|(key, _)| *key != range) {
            *cache = None;
            let analysis = analysis::Analysis::new(&self.source, grid, options, cancellation)?;
            *cache = Some((range, analysis));
        }
        solver::render(
            &self.source,
            grid,
            &cache.as_ref().expect("initialized analysis").1,
            options,
            cancellation,
        )
    }
}
