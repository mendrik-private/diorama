use super::{AnnotationEdit, AnnotationId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    Clockwise90,
    CounterClockwise90,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resampling {
    Nearest,
    Bicubic,
    GameAsset(GameAssetOptions),
    Lanczos,
}

impl Resampling {
    pub fn downscale_only(self) -> bool {
        matches!(self, Self::GameAsset(_))
    }
}

pub use asset_scaler::GameAssetAa;

/// All Game Asset controls captured by a scale operation. Keeping this value
/// with the operation makes preview, Apply, undo/redo, and export independent
/// from later preference changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameAssetOptions {
    aa: GameAssetAa,
    contour_darkening: u8,
}

impl GameAssetOptions {
    pub fn new(aa: GameAssetAa, contour_darkening: u8) -> Self {
        Self {
            aa,
            contour_darkening: contour_darkening.min(100),
        }
    }

    pub fn aa(self) -> GameAssetAa {
        self.aa
    }

    pub fn contour_darkening(self) -> u8 {
        self.contour_darkening
    }

    pub fn ink_brightness(self) -> f64 {
        1. - f64::from(self.contour_darkening) / 100.
    }
}

impl Default for GameAssetOptions {
    fn default() -> Self {
        Self::new(GameAssetAa::default(), 20)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrushPoint {
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrokePath {
    Smooth,
    Linear,
    Circle,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    pub points: Vec<BrushPoint>,
    pub path: StrokePath,
    pub color: [u8; 4],
    pub width: f32,
    pub anti_aliasing: bool,
    pub opacity: f32,
    pub hardness: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProtectedColor(pub [u8; 4]);

#[derive(Debug, Clone, PartialEq)]
pub enum Operation {
    /// A flattened selection edit. `pixels` is a complete, post-edit canvas so
    /// the operation remains a single undo entry even when it cuts annotations.
    SelectionEdit {
        pixels: std::sync::Arc<image::RgbaImage>,
        flattened_annotations: Vec<AnnotationId>,
    },
    ResizeCanvas {
        width: u32,
        height: u32,
        background: [u8; 4],
    },
    Crop {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    Rotate(Rotation),
    FlipHorizontal,
    FlipVertical,
    Scale {
        width: u32,
        height: u32,
        resampling: Resampling,
    },
    Palette {
        colors: u16,
        dithering: bool,
        preserve_accents: bool,
        protected: Vec<ProtectedColor>,
    },
    Annotate(AnnotationEdit),
}

#[cfg(test)]
mod tests {
    use super::{GameAssetAa, GameAssetOptions};

    #[test]
    fn game_asset_options_default_and_clamp_are_stable() {
        assert_eq!(
            GameAssetOptions::default(),
            GameAssetOptions::new(GameAssetAa::new(50), 20)
        );
        let options = GameAssetOptions::new(GameAssetAa::new(42), 255);
        assert_eq!(options.aa(), GameAssetAa::new(42));
        assert_eq!(options.contour_darkening(), 100);
        assert_eq!(options.ink_brightness(), 0.);
        assert_eq!(
            GameAssetOptions::new(GameAssetAa::new(42), 0).ink_brightness(),
            1.
        );
    }
}
