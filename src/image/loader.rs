use std::io::{BufReader, Cursor};
use std::path::Path;
use std::sync::Arc;

use gio::prelude::CancellableExt;
use image::{DynamicImage, ImageDecoder, ImageReader};

use crate::document::{ImageSource, Metadata};
use crate::error::{AppError, Result};

#[derive(Debug, Clone, Copy)]
pub struct DecodeLimits {
    pub max_width: u32,
    pub max_height: u32,
    pub max_decoded_bytes: u64,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        let available = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|contents| {
                contents.lines().find_map(|line| {
                    let value = line.strip_prefix("MemAvailable:")?;
                    value
                        .split_whitespace()
                        .next()?
                        .parse::<u64>()
                        .ok()
                        .and_then(|kilobytes| kilobytes.checked_mul(1024))
                })
            });
        let cache_limit = available
            .map(|bytes| bytes / 4)
            .unwrap_or(512 * 1024 * 1024)
            .min(512 * 1024 * 1024);
        Self {
            max_width: 100_000,
            max_height: 100_000,
            max_decoded_bytes: cache_limit,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadedPreview {
    pub texture: gtk::gdk::Texture,
    pub width: u32,
    pub height: u32,
    pub metadata: Metadata,
    pub animation_delay: Option<std::time::Duration>,
}

#[derive(Debug, Clone)]
pub struct AnimationFrame {
    pub texture: gtk::gdk::Texture,
    pub delay: std::time::Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeProbe {
    pub width: u32,
    pub height: u32,
    pub mime_type: String,
}

pub async fn load_preview(
    file: &gio::File,
    limits: DecodeLimits,
    cancellable: &gio::Cancellable,
) -> Result<LoadedPreview> {
    let mut loader = glycin::Loader::new(file.clone());
    loader.cancellable(cancellable.clone());
    loader.use_expose_base_dir(false);
    let image = loader
        .load()
        .await
        .map_err(|error| classify_glycin_error(&error.to_string()))?;
    let details = image.details();
    enforce_limits(details.width(), details.height(), limits)?;
    let frame = image
        .next_frame()
        .await
        .map_err(|error| classify_glycin_error(&error.to_string()))?;
    let metadata = super::metadata::from_glycin(&image, &frame);
    Ok(LoadedPreview {
        texture: frame.texture(),
        width: frame.width(),
        height: frame.height(),
        metadata,
        animation_delay: frame.delay(),
    })
}

pub async fn decode_animation(
    file: &gio::File,
    limits: DecodeLimits,
    cancellable: &gio::Cancellable,
) -> Result<Vec<AnimationFrame>> {
    let mut loader = glycin::Loader::new(file.clone());
    loader.cancellable(cancellable.clone());
    loader.use_expose_base_dir(false);
    let image = loader
        .load()
        .await
        .map_err(|error| classify_glycin_error(&error.to_string()))?;
    let details = image.details();
    enforce_limits(details.width(), details.height(), limits)?;
    let frame_bytes = u64::from(details.width())
        .checked_mul(u64::from(details.height()))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(AppError::MemoryLimit {
            limit_bytes: limits.max_decoded_bytes,
        })?;
    let mut frames = Vec::new();
    let mut seen = std::collections::HashSet::new();
    loop {
        if cancellable.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        let frame = image
            .next_frame()
            .await
            .map_err(|error| classify_glycin_error(&error.to_string()))?;
        let Some(delay) = frame.delay() else {
            return Ok(Vec::new());
        };
        if let Some(number) = frame.details().n_frame()
            && !seen.insert(number)
        {
            break;
        }
        let next_memory = frame_bytes.saturating_mul((frames.len() + 1) as u64);
        if next_memory > limits.max_decoded_bytes || frames.len() >= 1_000 {
            return Err(AppError::MemoryLimit {
                limit_bytes: limits.max_decoded_bytes,
            });
        }
        frames.push(AnimationFrame {
            texture: frame.texture(),
            delay: delay.max(std::time::Duration::from_millis(10)),
        });
        if frame.details().n_frame().is_none() && frames.len() > 1 {
            break;
        }
    }
    Ok(frames)
}

pub async fn probe_decode(
    file: &gio::File,
    limits: DecodeLimits,
    cancellable: &gio::Cancellable,
) -> Result<DecodeProbe> {
    let mut loader = glycin::Loader::new(file.clone());
    loader.cancellable(cancellable.clone());
    loader.use_expose_base_dir(false);
    let image = loader
        .load()
        .await
        .map_err(|error| classify_glycin_error(&error.to_string()))?;
    let details = image.details();
    enforce_limits(details.width(), details.height(), limits)?;
    let frame = image
        .next_frame()
        .await
        .map_err(|error| classify_glycin_error(&error.to_string()))?;
    enforce_limits(frame.width(), frame.height(), limits)?;
    Ok(DecodeProbe {
        width: frame.width(),
        height: frame.height(),
        mime_type: image.mime_type().to_string(),
    })
}

pub fn decode_headless(path: &Path, limits: DecodeLimits) -> Result<ImageSource> {
    let mut reader = ImageReader::open(path)?.with_guessed_format()?;
    reader.limits(image_limits(limits));
    let dimensions = reader.into_dimensions()?;
    enforce_limits(dimensions.0, dimensions.1, limits)?;

    let mut reader = ImageReader::open(path)?.with_guessed_format()?;
    reader.limits(image_limits(limits));
    let pixels = decode_oriented(reader)?;
    Ok(ImageSource {
        pixels: Arc::new(pixels),
        path: Some(path.to_path_buf()),
        metadata: Metadata::default(),
    })
}

pub fn decode_memory(bytes: Vec<u8>, limits: DecodeLimits) -> Result<ImageSource> {
    let cursor = Cursor::new(bytes.as_slice());
    let mut reader = ImageReader::new(BufReader::new(cursor)).with_guessed_format()?;
    reader.limits(image_limits(limits));
    let dimensions = reader.into_dimensions()?;
    enforce_limits(dimensions.0, dimensions.1, limits)?;

    let cursor = Cursor::new(bytes);
    let mut reader = ImageReader::new(BufReader::new(cursor)).with_guessed_format()?;
    reader.limits(image_limits(limits));
    let pixels = decode_oriented(reader)?;
    Ok(ImageSource {
        pixels: Arc::new(pixels),
        path: None,
        metadata: Metadata::default(),
    })
}

fn decode_oriented(
    reader: ImageReader<impl std::io::BufRead + std::io::Seek>,
) -> Result<image::RgbaImage> {
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    Ok(image.into_rgba8())
}

fn image_limits(limits: DecodeLimits) -> image::Limits {
    let mut image_limits = image::Limits::default();
    image_limits.max_image_width = Some(limits.max_width);
    image_limits.max_image_height = Some(limits.max_height);
    image_limits.max_alloc = Some(limits.max_decoded_bytes);
    image_limits
}

fn enforce_limits(width: u32, height: u32, limits: DecodeLimits) -> Result<()> {
    if width > limits.max_width || height > limits.max_height {
        return Err(AppError::DimensionsTooLarge { width, height });
    }
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(AppError::MemoryLimit {
            limit_bytes: limits.max_decoded_bytes,
        })?;
    if bytes > limits.max_decoded_bytes {
        return Err(AppError::MemoryLimit {
            limit_bytes: limits.max_decoded_bytes,
        });
    }
    Ok(())
}

fn classify_glycin_error(message: &str) -> AppError {
    let lowercase = message.to_lowercase();
    if lowercase.contains("loader") && lowercase.contains("not found") {
        AppError::MissingDecoder(message.to_owned())
    } else if lowercase.contains("unsupported") {
        AppError::UnsupportedFormat
    } else {
        AppError::Glycin(message.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::{DecodeLimits, decode_headless, decode_memory, enforce_limits};

    #[test]
    fn rejects_dimension_and_byte_overflow() {
        let limits = DecodeLimits {
            max_width: 100,
            max_height: 100,
            max_decoded_bytes: 1_000,
        };
        assert!(enforce_limits(101, 1, limits).is_err());
        assert!(enforce_limits(20, 20, limits).is_err());
    }

    #[test]
    fn decodes_memory_with_the_same_limits_as_path_decoding() {
        let image = image::RgbaImage::from_pixel(2, 3, image::Rgba([4, 5, 6, 255]));
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("test PNG should encode");

        let source = decode_memory(png, DecodeLimits::default()).expect("PNG should decode");
        assert_eq!(source.pixels.dimensions(), (2, 3));
        assert_eq!(source.pixels.get_pixel(0, 0).0, [4, 5, 6, 255]);
        assert!(source.path.is_none());
    }

    #[test]
    fn rejects_memory_image_that_exceeds_limit() {
        let image = image::RgbaImage::from_pixel(2, 3, image::Rgba([0, 0, 0, 255]));
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("test PNG should encode");

        let limits = DecodeLimits {
            max_width: 1,
            max_height: 10,
            max_decoded_bytes: 1024,
        };
        assert!(decode_memory(png, limits).is_err());
    }

    #[test]
    fn corrupt_memory_input_is_rejected_without_a_decode_allocation() {
        assert!(decode_memory(vec![0x89, b'P', b'N', b'G'], DecodeLimits::default()).is_err());
    }

    #[test]
    fn path_decoding_enforces_limits_before_full_decode() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("oversized.png");
        image::RgbaImage::new(2, 3)
            .save(&path)
            .expect("test PNG should encode");
        let limits = DecodeLimits {
            max_width: 1,
            max_height: 10,
            max_decoded_bytes: 1024,
        };
        assert!(decode_headless(&path, limits).is_err());
    }
}
