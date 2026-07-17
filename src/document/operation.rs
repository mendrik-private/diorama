#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    Clockwise90,
    CounterClockwise90,
    HalfTurn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resampling {
    Nearest,
    Linear,
    Bicubic,
    SeamCarving,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrushPoint {
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    pub points: Vec<BrushPoint>,
    pub color: [u8; 4],
    pub width: f32,
    pub opacity: f32,
    pub hardness: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProtectedColor(pub [u8; 4]);

#[derive(Debug, Clone, PartialEq)]
pub enum Operation {
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
    Pencil(Stroke),
    SelectionCutout {
        width: u32,
        height: u32,
        alpha_mask: Vec<u8>,
        inverted: bool,
    },
}
