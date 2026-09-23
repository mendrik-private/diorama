use super::AnnotationId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    Clockwise90,
    CounterClockwise90,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resampling {
    Nearest,
    Bicubic,
    GameAsset(GameAssetAa),
    Lanczos,
}

impl Resampling {
    pub fn downscale_only(self) -> bool {
        matches!(self, Self::GameAsset(_))
    }
}

pub use asset_scaler::GameAssetAa;

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
use super::AnnotationEdit;
