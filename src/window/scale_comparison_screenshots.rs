//! Opt-in, model-backed captures comparing Bicubic and Game Asset scaling.

use super::*;
use image::GenericImageView;
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc, time::Instant};

const CAPTURE_SIZE: (i32, i32) = (1280, 800);
const TARGET_SIZES: [u32; 3] = [128, 180, 256];

struct RealWorkerGuard;

impl Drop for RealWorkerGuard {
    fn drop(&mut self) {
        crate::tools::line_art::keep_real_worker_warm(false);
    }
}

fn settle_for(duration: std::time::Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn wait_until(timeout: std::time::Duration, description: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + timeout;
    while !condition() && Instant::now() < deadline {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(condition(), "timed out waiting for {description}");
}

fn sha256(path: &Path) -> String {
    let bytes = std::fs::read(path)
        .unwrap_or_else(|error| panic!("read source {} for hashing: {error}", path.display()));
    format!("{:x}", Sha256::digest(bytes))
}

fn capture_window(window: &ViewerWindow, path: &Path) {
    let widget = &window.0.window;
    assert_eq!(
        (widget.width(), widget.height()),
        CAPTURE_SIZE,
        "capture window allocation"
    );
    let mut node = None;
    for _ in 0..20 {
        let snapshot = gtk::Snapshot::new();
        gtk::WidgetPaintable::new(Some(widget)).snapshot(
            &snapshot,
            f64::from(widget.width()),
            f64::from(widget.height()),
        );
        if let Some(next) = snapshot.to_node() {
            node = Some(next);
            break;
        }
        widget.queue_draw();
        glib::MainContext::default().iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let node = node.unwrap_or_else(|| panic!("window snapshot for {}", path.display()));
    let texture = widget.renderer().expect("renderer").render_texture(
        &node,
        Some(&gtk::graphene::Rect::new(
            0.0,
            0.0,
            widget.width() as f32,
            widget.height() as f32,
        )),
    );
    texture
        .save_to_png(path)
        .unwrap_or_else(|error| panic!("save screenshot {}: {error}", path.display()));
    assert_eq!(
        image::open(path)
            .unwrap_or_else(|error| panic!("reopen screenshot {}: {error}", path.display()))
            .dimensions(),
        (CAPTURE_SIZE.0 as u32, CAPTURE_SIZE.1 as u32),
        "saved screenshot dimensions"
    );
}

#[test]
#[ignore = "requires the monster sources, installed FLUX/BiRefNet models, and a graphical display"]
fn capture_monster_scale_comparisons() {
    let source_root = std::env::var("DIORAMA_SCALE_COMPARISON_SOURCE_ROOT")
        .expect("DIORAMA_SCALE_COMPARISON_SOURCE_ROOT");
    let output = std::env::var("DIORAMA_SCREENSHOT_DIR").expect("DIORAMA_SCREENSHOT_DIR");
    let sources = [
        ("cave-spider", "Cave Spider", "06-cave-spider.jpg"),
        ("goblin", "Goblin", "10-goblin.jpg"),
        ("dragon", "Dragon", "46-dragon.jpg"),
    ];

    adw::init().expect("GTK initialization");
    let gtk_settings = gtk::Settings::default().expect("GTK settings");
    gtk_settings.set_gtk_xft_dpi(96 * 1024);
    gtk_settings.set_gtk_font_name(Some("Adwaita Sans 11"));
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight);
    let application = adw::Application::builder()
        .application_id(crate::APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    application
        .register(gio::Cancellable::NONE)
        .expect("registration");

    let fixture = tempfile::Builder::new()
        .prefix("diorama-scale-comparison-")
        .tempdir()
        .expect("comparison fixture directory");
    let bicubic_dir = fixture.path().join("Bicubic (Catmull-Rom)");
    let game_asset_dir = fixture.path().join("Game Asset (Strength 40%)");
    std::fs::create_dir_all(&bicubic_dir).expect("Bicubic fixture directory");
    std::fs::create_dir_all(&game_asset_dir).expect("Game Asset fixture directory");
    std::fs::create_dir_all(&output).expect("screenshot output directory");

    crate::tools::line_art::keep_real_worker_warm(true);
    let _worker = RealWorkerGuard;
    let cancellation = CancellationToken::default();
    let options = GameAssetOptions::new(40);
    assert_eq!(options, GameAssetOptions::default());
    let mut comparisons = Vec::new();
    let mut original_hashes = Vec::new();

    for (slug, display_name, filename) in sources {
        let source_path = Path::new(&source_root).join(filename);
        let original_hash = sha256(&source_path);
        let source = image::open(&source_path)
            .unwrap_or_else(|error| panic!("decode {}: {error}", source_path.display()))
            .into_rgba8();
        assert_eq!(
            source.dimensions(),
            (1024, 1024),
            "{filename} source dimensions"
        );
        assert!(
            source.pixels().all(|pixel| pixel[3] == u8::MAX),
            "{filename} is the expected opaque JPEG source"
        );
        // Session::new binds the real production generator even in a test.
        // Calling the generic Game Asset resize helper here would select its
        // cfg(test) stand-in and make the documentation evidence misleading.
        let game_asset_session =
            crate::tools::scale::game_asset::Session::new(Arc::new(source.clone()));

        for size in TARGET_SIZES {
            eprintln!("Generating {display_name} at {size} × {size}");
            let bicubic = crate::tools::scale::resize(
                &source,
                size,
                size,
                Resampling::Bicubic,
                &cancellation,
            )
            .unwrap_or_else(|error| panic!("Bicubic {display_name} {size}px: {error}"));
            let game_asset = game_asset_session
                .resize(size, size, options, &cancellation, &|progress| {
                    eprintln!("{display_name} {size}px: {progress:?}");
                })
                .unwrap_or_else(|error| panic!("Game Asset {display_name} {size}px: {error}"));
            assert_eq!(bicubic.dimensions(), (size, size));
            assert_eq!(game_asset.dimensions(), (size, size));
            assert_ne!(
                bicubic.as_raw(),
                game_asset.as_raw(),
                "comparison methods unexpectedly produced identical pixels"
            );

            let comparison_name = format!("{display_name} {size}x{size}");
            let bicubic_path = bicubic_dir.join(format!("{comparison_name} - Bicubic.png"));
            let game_asset_path =
                game_asset_dir.join(format!("{comparison_name} - Game Asset.png"));
            bicubic
                .save(&bicubic_path)
                .unwrap_or_else(|error| panic!("save {}: {error}", bicubic_path.display()));
            game_asset
                .save(&game_asset_path)
                .unwrap_or_else(|error| panic!("save {}: {error}", game_asset_path.display()));
            comparisons.push((
                slug,
                size,
                bicubic_path,
                game_asset_path,
                Path::new(&output).join(format!("game-asset-vs-bicubic-{slug}-{size}.png")),
            ));
        }
        original_hashes.push((source_path, original_hash));
    }

    for (slug, size, bicubic_path, game_asset_path, screenshot_path) in comparisons {
        eprintln!("Capturing {slug} at {size} × {size}");
        let window = ViewerWindow::new(&application, Some(gio::File::for_path(&bicubic_path)));
        window
            .0
            .window
            .set_default_size(CAPTURE_SIZE.0, CAPTURE_SIZE.1);
        window.present();
        wait_until(
            std::time::Duration::from_secs(10),
            "Bicubic image load",
            || {
                window
                    .0
                    .rendered
                    .borrow()
                    .as_ref()
                    .is_some_and(|image| image.dimensions() == (size, size))
            },
        );
        assert!(
            (window.0.render_scale.get() - 1.0).abs() < f64::EPSILON,
            "comparison screenshots require a dedicated 1× compositor"
        );
        window.load_comparison(gio::File::for_path(&game_asset_path));
        wait_until(
            std::time::Duration::from_secs(10),
            "Game Asset comparison load",
            || {
                window
                    .0
                    .compare_canvas
                    .borrow()
                    .as_ref()
                    .and_then(ImageCanvas::texture)
                    .is_some_and(|texture| {
                        (texture.width(), texture.height()) == (size as i32, size as i32)
                    })
            },
        );
        window.0.canvas.set_filter(ZoomFilter::Hard);
        window.0.canvas.set_background(Background::Gray);
        let comparison = window
            .0
            .compare_canvas
            .borrow()
            .clone()
            .expect("Game Asset comparison canvas");
        comparison.set_filter(ZoomFilter::Hard);
        comparison.set_background(Background::Gray);
        gio::prelude::ActionGroupExt::activate_action(&window.0.window, "zoom-200", None);
        wait_until(
            std::time::Duration::from_secs(5),
            "200% compare zoom",
            || {
                (window.0.canvas.zoom() - 2.0).abs() < f64::EPSILON
                    && (comparison.zoom() - 2.0).abs() < f64::EPSILON
            },
        );
        wait_until(
            std::time::Duration::from_secs(5),
            "equal compare panes",
            || {
                window
                    .0
                    .compare_paned
                    .borrow()
                    .as_ref()
                    .is_some_and(|paned| {
                        paned.orientation() == gtk::Orientation::Horizontal
                            && (paned.position() - paned.width() / 2).abs() <= 1
                    })
            },
        );
        wait_until(
            std::time::Duration::from_secs(5),
            "initial compare fit",
            || window.0.compare_fit_zooms.get().is_some(),
        );
        settle_for(std::time::Duration::from_secs(3));
        gio::prelude::ActionGroupExt::activate_action(&window.0.window, "zoom-200", None);
        wait_until(
            std::time::Duration::from_secs(5),
            "final 200% compare zoom",
            || {
                (window.0.canvas.zoom() - 2.0).abs() < f64::EPSILON
                    && (comparison.zoom() - 2.0).abs() < f64::EPSILON
            },
        );
        assert_eq!(window.0.canvas.filter(), ZoomFilter::Hard);
        assert_eq!(comparison.filter(), ZoomFilter::Hard);
        settle_for(std::time::Duration::from_secs(1));
        assert!(
            (window.0.canvas.zoom() - 2.0).abs() < f64::EPSILON
                && (comparison.zoom() - 2.0).abs() < f64::EPSILON,
            "both compare panes remain at 200% immediately before capture (left {}, right {})",
            window.0.canvas.zoom(),
            comparison.zoom()
        );
        capture_window(&window, &screenshot_path);
        window.0.window.close();
        settle_for(std::time::Duration::from_millis(100));
    }

    for (path, before) in original_hashes {
        assert_eq!(sha256(&path), before, "source changed: {}", path.display());
    }
}
