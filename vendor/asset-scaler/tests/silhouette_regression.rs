use asset_scaler::{CancellationToken, GameAssetAa, resize};
use image::{Rgba, RgbaImage};

#[test]
fn transparent_character_support_is_not_eroded_by_internal_ink() {
    // A filled body with a dark boot and a narrow connecting ankle. At 4:1,
    // the boot is only a few pixels wide, and its internal ridges aren't a
    // reliable closed boundary. Source alpha, not those ridges, owns its shape.
    let image = RgbaImage::from_fn(96, 96, |x, y| {
        let body = (24..72).contains(&x) && (8..52).contains(&y);
        let ankle = (32..44).contains(&x) && (48..72).contains(&y);
        let boot = (28..56).contains(&x) && (68..84).contains(&y);
        if boot || ankle {
            Rgba([35, 24, 18, 255])
        } else if body {
            Rgba([140, 90, 70, 255])
        } else {
            Rgba([255, 0, 255, 0])
        }
    });
    for aa in [0, 50, 100] {
        let output = resize(
            &image,
            24,
            24,
            GameAssetAa::new(aa),
            &CancellationToken::default(),
        )
        .unwrap();
        for y in 0..24 {
            for x in 0..24 {
                if (y * 4..y * 4 + 4)
                    .all(|sy| (x * 4..x * 4 + 4).all(|sx| image.get_pixel(sx, sy)[3] == 255))
                {
                    assert_eq!(
                        output.get_pixel(x, y)[3],
                        255,
                        "AA {aa}: opaque source support lost at ({x},{y})"
                    );
                }
            }
        }
    }
}

#[test]
fn off_grid_line_remains_connected() {
    let mut source = RgbaImage::from_pixel(48, 32, Rgba([245, 245, 245, 255]));
    for y in 0..32 {
        for x in 17..19 {
            source.put_pixel(x, y, Rgba([10, 10, 10, 255]));
        }
    }
    let output = resize(
        &source,
        12,
        8,
        GameAssetAa::default(),
        &CancellationToken::default(),
    )
    .unwrap();
    let preserved: Vec<_> = (0..8)
        .map(|y| {
            (3..6).any(|x| {
                let pixel = output.get_pixel(x, y);
                pixel[3] > 0 && pixel[0] < 224
            })
        })
        .collect();
    assert!(
        preserved.iter().all(|&value| value),
        "clipped line rows: {preserved:?}"
    );
}

#[test]
fn opaque_flat_canvas_becomes_transparent_without_eroding_its_foreground() {
    // Keep the image bright enough that edge detection does not manufacture an
    // ink contour: this exercises source-support topology itself.  Each target
    // support cell is fully covered by the 4×4 source footprint.
    let source = RgbaImage::from_fn(64, 64, |x, y| {
        if (20..44).contains(&x) && (20..44).contains(&y) {
            Rgba([200, 200, 200, 255])
        } else {
            Rgba([250, 250, 250, 255])
        }
    });
    for aa in [0, 20, 50, 100] {
        let output = resize(
            &source,
            16,
            16,
            GameAssetAa::new(aa),
            &CancellationToken::default(),
        )
        .unwrap();
        for y in 5..11 {
            for x in 5..11 {
                assert_eq!(
                    output.get_pixel(x, y)[3],
                    255,
                    "AA {aa}: full source support was erased at ({x}, {y})"
                );
            }
        }
        assert_eq!(
            output.get_pixel(4, 7),
            &Rgba([0; 4]),
            "AA {aa}: flat canvas exterior remained visible"
        );
    }
}

#[test]
fn uniform_opaque_canvas_becomes_empty_when_downscaled() {
    let source = RgbaImage::from_pixel(32, 32, Rgba([80, 120, 160, 255]));
    let output = resize(
        &source,
        16,
        16,
        GameAssetAa::new(20),
        &CancellationToken::default(),
    )
    .unwrap();
    assert!(output.pixels().all(|pixel| pixel.0 == [0; 4]));
}
