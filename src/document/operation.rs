use super::{Annotation, AnnotationEdit, AnnotationId};

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

/// All Game Asset controls captured by a scale operation. Keeping this value
/// with the operation makes preview, Apply, undo/redo, and export independent
/// from later preference changes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GameAssetOptions {
    strength: asset_scaler::Strength,
}

impl GameAssetOptions {
    /// Line-art sharpening strength in percent, clamped to 0–100.
    pub fn new(strength: u8) -> Self {
        Self {
            strength: asset_scaler::Strength::new(strength),
        }
    }

    pub fn strength(self) -> asset_scaler::Strength {
        self.strength
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
    /// Remove a foreground from the raster and lift it into an editable image
    /// in one undo entry. The repaired background includes flattened annotations.
    ExtractSelection {
        background: std::sync::Arc<image::RgbaImage>,
        flattened_annotations: Vec<AnnotationId>,
        annotation: Annotation,
    },
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
    use super::GameAssetOptions;

    #[test]
    fn game_asset_options_default_and_clamp_are_stable() {
        assert_eq!(GameAssetOptions::default(), GameAssetOptions::new(40));
        assert_eq!(GameAssetOptions::new(42).strength().percent(), 42);
        assert_eq!(GameAssetOptions::new(255).strength().percent(), 100);
    }
}
