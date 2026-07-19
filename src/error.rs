use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("This image format is not supported")]
    UnsupportedFormat,
    #[error("The image is corrupt or incomplete: {0}")]
    CorruptImage(String),
    #[error("A decoder for this image format is not installed: {0}")]
    MissingDecoder(String),
    #[error("The image dimensions {width}×{height} exceed the configured safety limit")]
    DimensionsTooLarge { width: u32, height: u32 },
    #[error("The decoded image would exceed the configured {limit_bytes} byte memory limit")]
    MemoryLimit { limit_bytes: u64 },
    #[error("The operation was cancelled")]
    Cancelled,
    #[error("The image has no visible content at the selected threshold")]
    NoVisibleContent,
    #[error("The requested crop is outside the image")]
    InvalidCrop,
    #[error("The requested dimensions are invalid")]
    InvalidDimensions,
    #[error("The file changed outside Diorama: {0}")]
    ExternallyChanged(PathBuf),
    #[error("The file was deleted or moved: {0}")]
    FileMissing(PathBuf),
    #[error("The optional local object-selection model is unavailable")]
    AiModelUnavailable,
    #[error("Local object selection failed: {0}")]
    AiInference(String),
    #[error("Could not read or write the file: {0}")]
    Io(#[from] std::io::Error),
    #[error("Could not process the image: {0}")]
    Image(#[from] image::ImageError),
    #[error("Could not load the image safely: {0}")]
    Glycin(String),
}

pub type Result<T> = std::result::Result<T, AppError>;
