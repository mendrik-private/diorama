pub mod arrow;
pub mod edit;
pub mod font;
pub mod geometry;
pub mod highlight;
pub mod hit;
pub mod measure;
pub mod pencil;
pub mod pixel_font;
pub mod render;
pub mod text;

pub use render::{composite_annotation_shapes, composite_annotations, render_annotation_preview};

pub const DEFAULT_ANNOTATION_COLOR: [u8; 4] = [255, 0, 0, 255];
