//! Application boundary for the shared Game Asset scaler.
use crate::{
    document::{CancellationToken, GameAssetAa},
    error::{AppError, Result},
};
use image::RgbaImage;
use std::sync::Arc;

const SCALER_OPTIONS: asset_scaler::ResizeOptions =
    asset_scaler::ResizeOptions::preserve_opaque_background();

pub struct Session(asset_scaler::Session);

impl Session {
    pub fn new(source: Arc<RgbaImage>) -> Self {
        Self(asset_scaler::Session::with_options(source, SCALER_OPTIONS))
    }

    pub fn resize(
        &self,
        w: u32,
        h: u32,
        aa: GameAssetAa,
        cancel: &CancellationToken,
    ) -> Result<RgbaImage> {
        self.0
            .resize(w, h, aa, &|| cancel.check().is_err())
            .map_err(map_error)
    }
}

pub fn resize(
    image: &RgbaImage,
    w: u32,
    h: u32,
    aa: GameAssetAa,
    cancel: &CancellationToken,
) -> Result<RgbaImage> {
    asset_scaler::resize_with_options(image, w, h, aa, &|| cancel.check().is_err(), SCALER_OPTIONS)
        .map_err(map_error)
}

fn map_error(error: asset_scaler::Error) -> AppError {
    match error {
        asset_scaler::Error::Cancelled => AppError::Cancelled,
        asset_scaler::Error::InvalidDimensions => AppError::InvalidDimensions,
        asset_scaler::Error::GameAssetMemoryLimit { limit_bytes } => {
            AppError::GameAssetMemoryLimit { limit_bytes }
        }
        asset_scaler::Error::Scaling(message) => AppError::Scaling(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Document, ImageSource, Metadata, Operation, Resampling};

    #[test]
    fn shared_preview_matches_document_commit_undo_redo_at_every_aa() {
        let source = Arc::new(RgbaImage::from_fn(96, 80, |x, y| {
            image::Rgba(if (y as f64 - (0.57 * x as f64 + 10.)).abs() < 3. {
                [8, 10, 4, 255]
            } else {
                [130, 170, 90, 255]
            })
        }));
        let session = Session::new(source.clone());
        let cancel = CancellationToken::default();
        let mut outputs = Vec::new();
        for percent in [0, 100, 50, 0] {
            let aa = GameAssetAa::new(percent);
            let preview = session.resize(32, 27, aa, &cancel).unwrap();
            assert_eq!(session.resize(32, 27, aa, &cancel).unwrap(), preview);
            let mut document = Document::new(ImageSource {
                pixels: source.clone(),
                path: None,
                metadata: Metadata::default(),
            });
            let operation = Operation::Scale {
                width: 32,
                height: 27,
                resampling: Resampling::GameAsset(aa),
            };
            document.apply(operation.clone());
            assert_eq!(document.render(&cancel).unwrap().pixels, preview);
            assert!(document.undo());
            assert_eq!(document.render(&cancel).unwrap().pixels, *source);
            assert!(document.redo());
            assert_eq!(document.operations(), &[operation]);
            assert_eq!(document.render(&cancel).unwrap().pixels, preview);
            outputs.push(preview);
        }
        assert_ne!(outputs[0], outputs[1]);
        assert_ne!(outputs[1], outputs[2]);
        assert_eq!(outputs[0], outputs[3]);
    }

    #[test]
    fn shared_scaler_preserves_an_opaque_canvas() {
        let source = Arc::new(RgbaImage::from_fn(64, 64, |x, y| {
            if (20..44).contains(&x) && (20..44).contains(&y) {
                image::Rgba([120, 180, 90, 255])
            } else {
                image::Rgba([240, 230, 220, 255])
            }
        }));
        let cancel = CancellationToken::default();
        let one_shot = resize(&source, 16, 16, GameAssetAa::new(20), &cancel).unwrap();
        let cached = Session::new(source)
            .resize(16, 16, GameAssetAa::new(20), &cancel)
            .unwrap();
        assert_eq!(cached, one_shot);
        assert_eq!(one_shot.get_pixel(0, 0), &image::Rgba([240, 230, 220, 255]));

        let mut document = Document::new(ImageSource {
            pixels: Arc::new(RgbaImage::from_fn(64, 64, |x, y| {
                if (20..44).contains(&x) && (20..44).contains(&y) {
                    image::Rgba([120, 180, 90, 255])
                } else {
                    image::Rgba([240, 230, 220, 255])
                }
            })),
            path: None,
            metadata: Metadata::default(),
        });
        document.apply(Operation::Scale {
            width: 16,
            height: 16,
            resampling: Resampling::GameAsset(GameAssetAa::new(20)),
        });
        assert_eq!(document.render(&cancel).unwrap().pixels, cached);
    }

    #[test]
    fn shared_scaler_keeps_explicit_source_alpha() {
        let source = Arc::new(RgbaImage::from_fn(64, 64, |x, y| {
            if (20..44).contains(&x) && (20..44).contains(&y) {
                image::Rgba([120, 180, 90, 255])
            } else {
                image::Rgba([240, 0, 220, 0])
            }
        }));
        let cancel = CancellationToken::default();
        let one_shot = resize(&source, 16, 16, GameAssetAa::new(20), &cancel).unwrap();
        let cached = Session::new(source)
            .resize(16, 16, GameAssetAa::new(20), &cancel)
            .unwrap();
        assert_eq!(cached, one_shot);
        assert_eq!(one_shot.get_pixel(0, 0)[3], 0);
        assert_eq!(one_shot.get_pixel(8, 8)[3], 255);
    }

    #[test]
    fn shared_scaler_errors_preserve_application_semantics() {
        let image = Arc::new(RgbaImage::new(16, 16));
        let session = Session::new(image.clone());
        let cancel = CancellationToken::default();
        assert!(matches!(
            session.resize(17, 16, Default::default(), &cancel),
            Err(AppError::InvalidDimensions)
        ));
        session.resize(8, 8, Default::default(), &cancel).unwrap();
        cancel.cancel();
        assert!(matches!(
            session.resize(8, 8, Default::default(), &cancel),
            Err(AppError::Cancelled)
        ));
        assert!(matches!(
            resize(&image, 16, 16, Default::default(), &cancel),
            Err(AppError::Cancelled)
        ));
        assert!(matches!(
            map_error(asset_scaler::Error::GameAssetMemoryLimit { limit_bytes: 42 }),
            AppError::GameAssetMemoryLimit { limit_bytes: 42 }
        ));
    }
}
