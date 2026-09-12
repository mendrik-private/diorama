#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    Clockwise90,
    CounterClockwise90,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resampling {
    Nearest,
    Linear,
    Bicubic,
    SeamCarving,
    GameAsset,
    Lanczos,
}

impl Resampling {
    pub fn downscale_only(self) -> bool {
        matches!(self, Self::SeamCarving | Self::GameAsset)
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
