use std::hint::black_box;
use std::time::{Duration, Instant};

use super::resize;
use crate::document::{CancellationToken, Resampling};
use image::{Rgba, RgbaImage};

#[test]
#[ignore = "optimized scaling benchmark; run with --release --ignored --nocapture"]
fn scaling() {
    for (width, height, target_width, target_height) in
        [(640, 480, 512, 384), (1280, 960, 1024, 768)]
    {
        let image = RgbaImage::from_fn(width, height, |x, y| {
            let noise = x.wrapping_mul(374_761_393) ^ y.wrapping_mul(668_265_263);
            Rgba([noise as u8, (noise >> 8) as u8, (noise >> 16) as u8, 255])
        });
        for method in [Resampling::Bicubic, Resampling::SeamCarving] {
            let mut timings = Vec::new();
            for iteration in 0..4 {
                let start = Instant::now();
                let result = resize(
                    black_box(&image),
                    target_width,
                    target_height,
                    method,
                    &CancellationToken::default(),
                )
                .unwrap();
                let elapsed = start.elapsed();
                assert_eq!(result.dimensions(), (target_width, target_height));
                black_box(result);
                if iteration > 0 {
                    timings.push(elapsed);
                }
            }
            timings.sort();
            println!(
                "{method:?} {width}x{height} -> {target_width}x{target_height}: median {:.3}s, range {:.3}..{:.3}s",
                timings[1].as_secs_f64(),
                timings[0].as_secs_f64(),
                timings[2].as_secs_f64(),
            );
            if let Ok(budget) = std::env::var("DIORAMA_SEAM_BUDGET_MS")
                && method == Resampling::SeamCarving
            {
                assert!(timings[1] < Duration::from_millis(budget.parse().unwrap()));
            }
        }
    }
}
