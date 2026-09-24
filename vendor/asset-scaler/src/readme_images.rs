//! Manual documentation exporter, using the production pipeline's private stages.
use super::*;
use image::{GrayImage, Luma, imageops};

#[test]
#[ignore = "regenerate README images from docs/images/elf-source.png"]
fn generate_readme_images() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/images");
    let source = image::open(directory.join("elf-source.png"))?.into_rgba8();
    assert_eq!(source.dimensions(), (800, 800));
    let cancel = CancellationToken::default();
    let prepared = Prepared::new(&source, &cancel)?;
    let aa = GameAssetAa::default();
    let target = prepared.target_contours(&source, 200, 200, aa, &cancel)?;
    target
        .strokes
        .core
        .save(directory.join("elf-contours.png"))?;
    target
        .strokes
        .coverage
        .save(directory.join("elf-contour-coverage.png"))?;

    // Match the silhouette support coverage used by resize.
    let silhouette = prepared.silhouette.as_ref().expect("elf has source alpha");
    let source_coverage = silhouette.coverage(200, 200, &cancel)?;
    let coverage = silhouette.target_coverage(&source_coverage, aa, &cancel)?;
    GrayImage::from_fn(200, 200, |x, y| {
        Luma([(coverage[(y * 200 + x) as usize] * 255.).round() as u8])
    })
    .save(directory.join("elf-fill-mask.png"))?;

    // This is the actual color resample before halo correction and composition.
    let isolated = silhouette.isolated(&prepared.linear, &cancel)?;
    let base = lanczos::resize(&isolated, 200, 200, &cancel)?;
    RgbaImage::from_fn(200, 200, |x, y| {
        color::rgba(base.pixels[(y * 200 + x) as usize])
    })
    .save(directory.join("elf-fill.png"))?;

    let mut outputs = Vec::new();
    for percent in [0, 50, 100] {
        let output = prepared.resize(&source, 200, 200, GameAssetAa::new(percent), &cancel)?;
        assert_eq!(output.dimensions(), (200, 200));
        output.save(directory.join(format!("elf-aa-{percent}.png")))?;
        // Bake nearest-neighbor enlargement into PNGs: GitHub removes inline CSS.
        let detail = imageops::crop_imm(&output, 78, 44, 50, 60).to_image();
        imageops::resize(&detail, 200, 240, imageops::FilterType::Nearest)
            .save(directory.join(format!("elf-aa-{percent}-detail.png")))?;
        outputs.push(output);
    }
    assert_ne!(
        outputs[0], outputs[1],
        "AA comparison must show distinct pixels"
    );
    assert_ne!(
        outputs[1], outputs[2],
        "AA comparison must show distinct pixels"
    );
    Ok(())
}
