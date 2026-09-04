use crate::document::Metadata;

pub fn from_glycin(image: &glycin::Image, frame: &glycin::Frame) -> Metadata {
    let details = image.details();
    let frame_details = frame.details();
    Metadata {
        mime_type: Some(image.mime_type().to_string()),
        exif: details
            .metadata_exif()
            .and_then(|data| data.get_full().ok()),
        xmp: details.metadata_xmp().and_then(|data| data.get_full().ok()),
        icc: frame_details
            .color_icc_profile()
            .and_then(|data| data.get_full().ok()),
    }
}
