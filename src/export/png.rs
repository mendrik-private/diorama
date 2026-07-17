use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use image::ImageEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};

use crate::document::RenderedImage;
use crate::error::Result;

#[derive(Debug, Clone, Copy)]
pub struct PngOptions {
    pub compression: u8,
    pub preserve_metadata: bool,
    pub convert_to_srgb: bool,
}

impl Default for PngOptions {
    fn default() -> Self {
        Self {
            compression: 6,
            preserve_metadata: true,
            convert_to_srgb: false,
        }
    }
}

pub(crate) fn encode(
    writer: &mut dyn Write,
    image: &RenderedImage,
    options: &PngOptions,
) -> Result<()> {
    let compression = match options.compression {
        0..=2 => CompressionType::Fast,
        3..=7 => CompressionType::Default,
        _ => CompressionType::Best,
    };
    let mut encoded = Vec::new();
    PngEncoder::new_with_quality(&mut encoded, compression, FilterType::Adaptive).write_image(
        image.pixels.as_raw(),
        image.pixels.width(),
        image.pixels.height(),
        image::ExtendedColorType::Rgba8,
    )?;
    writer.write_all(&encoded[..33])?;
    if options.convert_to_srgb {
        write_chunk(writer, *b"sRGB", &[0])?;
    } else if options.preserve_metadata
        && let Some(profile) = &image.metadata.icc
    {
        let mut payload = b"ICC Profile\0\0".to_vec();
        let mut compressor = ZlibEncoder::new(Vec::new(), Compression::default());
        compressor.write_all(profile)?;
        payload.extend(compressor.finish()?);
        write_chunk(writer, *b"iCCP", &payload)?;
    }
    if options.preserve_metadata {
        if let Some(exif) = &image.metadata.exif {
            write_chunk(writer, *b"eXIf", strip_exif_prefix(exif))?;
        }
        if let Some(xmp) = &image.metadata.xmp {
            let mut payload = b"XML:com.adobe.xmp\0\0\0\0\0".to_vec();
            payload.extend(xmp);
            write_chunk(writer, *b"iTXt", &payload)?;
        }
    }
    writer.write_all(&encoded[33..])?;
    Ok(())
}

fn strip_exif_prefix(exif: &[u8]) -> &[u8] {
    exif.strip_prefix(b"Exif\0\0").unwrap_or(exif)
}

fn write_chunk(writer: &mut dyn Write, kind: [u8; 4], payload: &[u8]) -> Result<()> {
    let length = u32::try_from(payload.len()).map_err(|_| {
        crate::error::AppError::CorruptImage("Metadata is too large for PNG".to_owned())
    })?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&kind)?;
    writer.write_all(payload)?;
    let mut crc = crc32fast::Hasher::new();
    crc.update(&kind);
    crc.update(payload);
    writer.write_all(&crc.finalize().to_be_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::{PngOptions, encode};
    use crate::document::{Metadata, RenderedImage};

    #[test]
    fn embeds_or_removes_metadata_as_requested() {
        let image = RenderedImage {
            pixels: RgbaImage::from_pixel(1, 1, Rgba([1, 2, 3, 4])),
            metadata: Metadata {
                exif: Some(b"Exif\0\0example-exif".to_vec()),
                xmp: Some(b"example-xmp".to_vec()),
                icc: Some(b"example-icc".to_vec()),
                ..Metadata::default()
            },
        };
        let mut preserved = Vec::new();
        encode(&mut preserved, &image, &PngOptions::default()).unwrap();
        assert!(preserved.windows(4).any(|window| window == b"eXIf"));
        assert!(preserved.windows(4).any(|window| window == b"iTXt"));
        assert!(preserved.windows(4).any(|window| window == b"iCCP"));
        let mut removed = Vec::new();
        encode(
            &mut removed,
            &image,
            &PngOptions {
                preserve_metadata: false,
                ..PngOptions::default()
            },
        )
        .unwrap();
        assert!(!removed.windows(4).any(|window| window == b"eXIf"));
        assert!(!removed.windows(4).any(|window| window == b"iTXt"));
        assert!(!removed.windows(4).any(|window| window == b"iCCP"));
    }
}
