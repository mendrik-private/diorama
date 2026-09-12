//! Opt-in trial: contrast-selected NN halves followed by ordinary bicubic.
//! This does not change any application scaling method.

use crate::document::{CancellationToken, Resampling};
use crate::error::{AppError, Result};
use image::{Rgba, RgbaImage};

fn next_half(width: u32, height: u32, target: (u32, u32)) -> Option<(u32, u32)> {
    let next = (width / 2, height / 2);
    (next.0 > 0 && next.1 > 0 && next.0 >= target.0 && next.1 >= target.1).then_some(next)
}

fn appearance(pixel: Rgba<u8>) -> [f32; 4] {
    let alpha = pixel[3] as f32 / 255.0;
    [
        pixel[0] as f32 * alpha,
        pixel[1] as f32 * alpha,
        pixel[2] as f32 * alpha,
        pixel[3] as f32,
    ]
}

fn standout(samples: [Rgba<u8>; 4]) -> Rgba<u8> {
    let colours = samples.map(appearance);
    let mut selected = 0;
    let mut best = -1.0_f32;
    for (i, a) in colours.iter().enumerate() {
        // Ignore hidden RGB. Alpha-weighting prevents an empty pixel from
        // winning merely because it differs from three foreground colours.
        let score = (a[3] / 255.0)
            * colours
                .iter()
                .map(|b| {
                    ((a[0] - b[0]).powi(2)
                        + (a[1] - b[1]).powi(2)
                        + (a[2] - b[2]).powi(2)
                        + (a[3] - b[3]).powi(2))
                    .sqrt()
                })
                .sum::<f32>();
        if score > best {
            best = score;
            selected = i;
        }
    }
    // Samples are ordered with the normal NN phase first for stable ties.
    samples[selected]
}

fn half(source: &RgbaImage, cancellation: &CancellationToken) -> Result<RgbaImage> {
    let (width, height) = (source.width() / 2, source.height() / 2);
    if width == 0 || height == 0 {
        return Err(AppError::InvalidDimensions);
    }
    let mut output = RgbaImage::new(width, height);
    for y in 0..height {
        cancellation.check()?;
        // For odd source dimensions, centre each pair on the output grid;
        // do not simply discard the source's last row or column.
        let sy = ((y as f64 + 0.5) * source.height() as f64 / height as f64 - 0.5).floor() as u32;
        let ny = ((y as f64 + 0.5) * source.height() as f64 / height as f64).floor() as u32;
        for x in 0..width {
            let sx = ((x as f64 + 0.5) * source.width() as f64 / width as f64 - 0.5).floor() as u32;
            let right = (sx + 1).min(source.width() - 1);
            let bottom = (sy + 1).min(source.height() - 1);
            let nx = ((x as f64 + 0.5) * source.width() as f64 / width as f64).floor() as u32;
            let other_x = if nx == sx { right } else { sx };
            let other_y = if ny == sy { bottom } else { sy };
            let samples = [
                *source.get_pixel(nx, ny),
                *source.get_pixel(other_x, ny),
                *source.get_pixel(nx, other_y),
                *source.get_pixel(other_x, other_y),
            ];
            output.put_pixel(x, y, standout(samples));
        }
    }
    Ok(output)
}

fn resize(
    source: &RgbaImage,
    width: u32,
    height: u32,
    cancellation: &CancellationToken,
) -> Result<(RgbaImage, Vec<(u32, u32)>)> {
    if width == 0 || height == 0 || width > source.width() || height > source.height() {
        return Err(AppError::InvalidDimensions);
    }
    cancellation.check()?;
    let mut image = source.clone();
    let mut stages = vec![image.dimensions()];
    while next_half(image.width(), image.height(), (width, height)).is_some() {
        image = half(&image, cancellation)?;
        stages.push(image.dimensions());
    }
    if image.dimensions() != (width, height) {
        image = super::resize(&image, width, height, Resampling::Bicubic, cancellation)?;
        stages.push(image.dimensions());
    }
    cancellation.check()?;
    Ok((image, stages))
}

#[test]
fn staged_dimensions_stop_before_undershooting() {
    for (source, target, expected) in [
        (
            (800, 800),
            (128, 128),
            vec![(800, 800), (400, 400), (200, 200), (128, 128)],
        ),
        (
            (800, 800),
            (100, 100),
            vec![(800, 800), (400, 400), (200, 200), (100, 100)],
        ),
        (
            (800, 600),
            (128, 96),
            vec![(800, 600), (400, 300), (200, 150), (128, 96)],
        ),
        ((9, 7), (2, 2), vec![(9, 7), (4, 3), (2, 2)]),
        ((1, 8), (1, 2), vec![(1, 8), (1, 2)]),
    ] {
        let input = RgbaImage::from_pixel(source.0, source.1, Rgba([80, 140, 40, 255]));
        let (output, stages) =
            resize(&input, target.0, target.1, &CancellationToken::default()).unwrap();
        assert_eq!(stages, expected);
        assert_eq!(output.dimensions(), target);
    }
}

#[test]
fn odd_dimensions_keep_nn_ties_and_sample_the_last_corner() {
    let source = RgbaImage::from_fn(7, 7, |x, _| {
        if x % 2 == 0 {
            Rgba([20, 20, 20, 255])
        } else {
            Rgba([220, 220, 220, 255])
        }
    });
    let cancellation = CancellationToken::default();
    assert_eq!(
        half(&source, &cancellation).unwrap(),
        super::resize(&source, 3, 3, Resampling::Nearest, &cancellation).unwrap()
    );
    let mut source = RgbaImage::from_pixel(7, 7, Rgba([100, 100, 100, 255]));
    let isolate = Rgba([255, 0, 200, 255]);
    source.put_pixel(6, 6, isolate);
    assert_eq!(
        *half(&source, &cancellation).unwrap().get_pixel(2, 2),
        isolate
    );
}

#[test]
fn standout_keeps_dark_bright_and_chromatic_isolates() {
    for (fill, feature) in [
        ([220, 220, 220, 255], [10, 10, 10, 255]),
        ([10, 10, 10, 255], [240, 240, 240, 255]),
        ([80, 140, 40, 255], [220, 20, 180, 255]),
    ] {
        for i in 0..4 {
            let mut samples = [Rgba(fill); 4];
            samples[i] = Rgba(feature);
            assert_eq!(standout(samples), Rgba(feature));
        }
    }
    assert_eq!(standout([Rgba([1, 2, 3, 255]); 4]), Rgba([1, 2, 3, 255]));
    let visible = Rgba([40, 80, 20, 180]);
    assert_eq!(
        standout([
            visible,
            Rgba([255, 0, 0, 0]),
            Rgba([0, 255, 0, 0]),
            Rgba([0, 0, 255, 0])
        ]),
        visible
    );
}

#[test]
fn halves_copy_rgba_and_finish_with_the_existing_bicubic() {
    let source = RgbaImage::from_fn(16, 12, |x, y| {
        Rgba([(x * 13) as u8, (y * 17) as u8, 40, ((x + y) * 9) as u8])
    });
    let cancellation = CancellationToken::default();
    let intermediate = half(&source, &cancellation).unwrap();
    for (x, y, p) in intermediate.enumerate_pixels() {
        assert!((0..2).any(|dy| (0..2).any(|dx| source.get_pixel(x * 2 + dx, y * 2 + dy) == p)));
    }
    let expected = super::resize(&intermediate, 7, 5, Resampling::Bicubic, &cancellation).unwrap();
    assert_eq!(resize(&source, 7, 5, &cancellation).unwrap().0, expected);
    assert_eq!(resize(&source, 16, 12, &cancellation).unwrap().0, source);
    for dims in [(0, 1), (1, 0), (17, 12), (16, 13)] {
        assert!(resize(&source, dims.0, dims.1, &cancellation).is_err());
    }
    cancellation.cancel();
    assert!(matches!(
        resize(&source, 16, 12, &cancellation),
        Err(AppError::Cancelled)
    ));
    assert!(matches!(
        resize(&source, 4, 3, &cancellation),
        Err(AppError::Cancelled)
    ));
}

#[test]
#[ignore = "requires DIORAMA_GAME_ASSET_INPUT=.../elf2-se.png; exports contrast-halving trial"]
fn elf_contrast_halving() {
    let source = image::open(std::env::var("DIORAMA_GAME_ASSET_INPUT").unwrap())
        .unwrap()
        .to_rgba8();
    assert_eq!(source.dimensions(), (800, 800));
    let directory = tempfile::Builder::new()
        .prefix("diorama-halving-")
        .tempdir()
        .unwrap()
        .keep();
    let cancellation = CancellationToken::default();
    let first = half(&source, &cancellation).unwrap();
    first.save(directory.join("400-standout.png")).unwrap();
    let second = half(&first, &cancellation).unwrap();
    second.save(directory.join("200-standout.png")).unwrap();
    for size in [128, 160] {
        let start = std::time::Instant::now();
        let (output, stages) = resize(&source, size, size, &cancellation).unwrap();
        eprintln!("{size}: {stages:?}, {:?}", start.elapsed());
        assert_eq!(
            stages,
            vec![(800, 800), (400, 400), (200, 200), (size, size)]
        );
        output
            .save(directory.join(format!("{size}-standout-bicubic.png")))
            .unwrap();
        let nearest =
            super::resize(&source, size, size, Resampling::Nearest, &cancellation).unwrap();
        nearest
            .save(directory.join(format!("{size}-nearest-rgba.png")))
            .unwrap();
        let bicubic =
            super::resize(&source, size, size, Resampling::Bicubic, &cancellation).unwrap();
        let mut sheet = RgbaImage::from_pixel(size * 3, size, Rgba([82, 82, 82, 255]));
        let mut heads = RgbaImage::from_pixel(28 * 3, 30, Rgba([82, 82, 82, 255]));
        for (i, panel) in [&nearest, &bicubic, &output].into_iter().enumerate() {
            image::imageops::overlay(&mut sheet, panel, i as i64 * size as i64, 0);
            if size == 128 {
                let head = image::imageops::crop_imm(panel, 50, 16, 28, 30).to_image();
                image::imageops::overlay(&mut heads, &head, i as i64 * 28, 0);
            }
        }
        image::imageops::resize(
            &sheet,
            size * 9,
            size * 3,
            image::imageops::FilterType::Nearest,
        )
        .save(directory.join(format!("{size}-comparison-3x.png")))
        .unwrap();
        if size == 128 {
            image::imageops::resize(&heads, 672, 240, image::imageops::FilterType::Nearest)
                .save(directory.join("128-heads-8x.png"))
                .unwrap();
        }
    }
    eprintln!(
        "halving trial: {} (NN / direct bicubic / standout halves + bicubic)",
        directory.display()
    );
}
