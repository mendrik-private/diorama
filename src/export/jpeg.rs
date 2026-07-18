use std::io::Write;

use image::codecs::jpeg::JpegEncoder;
use image::{Rgb, RgbImage};

use crate::document::RenderedImage;
use crate::error::Result;

use super::normalized_exif;

#[derive(Debug, Clone, Copy)]
pub struct JpegOptions {
    pub quality: u8,
    pub background: [u8; 3],
    pub preserve_metadata: bool,
}

impl Default for JpegOptions {
    fn default() -> Self {
        Self {
            quality: 92,
            background: [255, 255, 255],
            preserve_metadata: true,
        }
    }
}

pub(crate) fn encode(
    writer: &mut dyn Write,
    image: &RenderedImage,
    options: &JpegOptions,
) -> Result<()> {
    let flattened = RgbImage::from_fn(image.pixels.width(), image.pixels.height(), |x, y| {
        let source = image.pixels.get_pixel(x, y).0;
        let alpha = u16::from(source[3]);
        let inverse = 255 - alpha;
        Rgb([
            ((u16::from(source[0]) * alpha + u16::from(options.background[0]) * inverse) / 255)
                as u8,
            ((u16::from(source[1]) * alpha + u16::from(options.background[1]) * inverse) / 255)
                as u8,
            ((u16::from(source[2]) * alpha + u16::from(options.background[2]) * inverse) / 255)
                as u8,
        ])
    });
    let mut encoded = Vec::new();
    JpegEncoder::new_with_quality(&mut encoded, options.quality).encode_image(&flattened)?;
    writer.write_all(&encoded[..2])?;
    if options.preserve_metadata {
        if let Some(exif) = &image.metadata.exif {
            let mut payload = b"Exif\0\0".to_vec();
            payload.extend(normalized_exif(exif));
            write_segment(writer, 0xE1, &payload)?;
        }
        if let Some(xmp) = &image.metadata.xmp {
            let mut payload = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
            payload.extend(xmp);
            write_segment(writer, 0xE1, &payload)?;
        }
        if let Some(profile) = &image.metadata.icc {
            let chunk_size = 65_519;
            let count = profile.len().div_ceil(chunk_size);
            if count <= 255 {
                for (index, chunk) in profile.chunks(chunk_size).enumerate() {
                    let mut payload = b"ICC_PROFILE\0".to_vec();
                    payload.push((index + 1) as u8);
                    payload.push(count as u8);
                    payload.extend(chunk);
                    write_segment(writer, 0xE2, &payload)?;
                }
            }
        }
    }
    writer.write_all(&encoded[2..])?;
    Ok(())
}

fn write_segment(writer: &mut dyn Write, marker: u8, payload: &[u8]) -> Result<()> {
    let length = u16::try_from(payload.len() + 2).map_err(|_| {
        crate::error::AppError::CorruptImage("Metadata is too large for JPEG".to_owned())
    })?;
    writer.write_all(&[0xFF, marker])?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(payload)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::ImageDecoder;
    use image::codecs::jpeg::JpegDecoder;
    use image::{Rgba, RgbaImage};

    use super::{JpegOptions, encode};
    use crate::document::{Metadata, RenderedImage};

    #[test]
    fn alpha_is_flattened_and_metadata_is_optional() {
        let image = RenderedImage {
            pixels: RgbaImage::from_pixel(1, 1, Rgba([255, 0, 0, 0])),
            metadata: Metadata {
                exif: Some(b"Exif\0\0example".to_vec()),
                ..Metadata::default()
            },
        };
        let mut encoded = Vec::new();
        encode(&mut encoded, &image, &JpegOptions::default()).unwrap();
        assert!(encoded.windows(6).any(|window| window == b"Exif\0\0"));
        let decoded = image::load_from_memory(&encoded).unwrap().into_rgb8();
        let pixel = decoded.get_pixel(0, 0).0;
        assert!(pixel.iter().all(|channel| *channel > 240));
    }

    #[test]
    fn exported_exif_cannot_rotate_normalized_pixels_again() {
        let exif = vec![
            b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0,
            0,
        ];
        let image = RenderedImage {
            pixels: RgbaImage::from_pixel(1, 2, Rgba([1, 2, 3, 255])),
            metadata: Metadata {
                exif: Some(exif),
                ..Metadata::default()
            },
        };
        let mut encoded = Vec::new();
        encode(&mut encoded, &image, &JpegOptions::default()).unwrap();

        let mut decoder = JpegDecoder::new(Cursor::new(encoded)).unwrap();
        let exported_exif = decoder.exif_metadata().unwrap().unwrap();

        assert_eq!(
            image::metadata::Orientation::from_exif_chunk(&exported_exif),
            Some(image::metadata::Orientation::NoTransforms)
        );
    }
}
