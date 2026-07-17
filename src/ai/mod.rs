use image::RgbaImage;

use crate::document::CancellationToken;
use crate::error::{AppError, Result};

#[derive(Debug, Clone, Copy)]
pub enum PromptKind {
    Foreground,
    Background,
}

#[derive(Debug, Clone, Copy)]
pub struct Prompt {
    pub x: f32,
    pub y: f32,
    pub kind: PromptKind,
}

pub trait SegmentationBackend: Send + Sync {
    fn name(&self) -> &str;
    fn is_installed(&self) -> bool;
    fn segment(
        &self,
        image: &RgbaImage,
        prompts: &[Prompt],
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>>;
}

#[derive(Debug, Default)]
pub struct UnavailableBackend;

impl SegmentationBackend for UnavailableBackend {
    fn name(&self) -> &'static str {
        "Not installed"
    }

    fn is_installed(&self) -> bool {
        false
    }

    fn segment(
        &self,
        _image: &RgbaImage,
        _prompts: &[Prompt],
        _cancellation: &CancellationToken,
    ) -> Result<Vec<u8>> {
        Err(AppError::AiModelUnavailable)
    }
}
