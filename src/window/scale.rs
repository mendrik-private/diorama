use std::time::Duration;

use crate::{document::Resampling, i18n::gettext, tools::line_art::Progress};

/// How often a pending Game Asset preview shows the latest generation
/// progress.
pub(super) const SCALE_PROGRESS_REFRESH: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScaleUnit {
    Pixels,
    Percent,
}

pub(super) fn scaled_dimensions(width: u32, height: u32, target_width: u32) -> (u32, u32) {
    let width = width.max(1);
    let height = height.max(1);
    let target_width = target_width.max(1);
    let target_height = ((u64::from(height) * u64::from(target_width) + u64::from(width) / 2)
        / u64::from(width))
    .max(1)
    .min(u64::from(u32::MAX)) as u32;
    (target_width, target_height)
}

pub(super) fn scaled_width_for_height(width: u32, height: u32, target_height: u32) -> u32 {
    let width = width.max(1);
    let height = height.max(1);
    let target_height = target_height.max(1);
    ((u64::from(width) * u64::from(target_height) + u64::from(height) / 2) / u64::from(height))
        .max(1)
        .min(u64::from(u32::MAX)) as u32
}

pub(super) fn dimensions_from_percent(width: u32, height: u32, percent: f64) -> (u32, u32) {
    let factor = percent.max(0.01) / 100.0;
    (
        (f64::from(width.max(1)) * factor)
            .round()
            .clamp(1.0, f64::from(u32::MAX)) as u32,
        (f64::from(height.max(1)) * factor)
            .round()
            .clamp(1.0, f64::from(u32::MAX)) as u32,
    )
}

pub(super) fn scale_unit(index: u32) -> ScaleUnit {
    if index == 1 {
        ScaleUnit::Percent
    } else {
        ScaleUnit::Pixels
    }
}

/// The progress bar's text: whole seconds left, rounded up, or minutes from
/// 90 s. Once the model is done, the preview is still being composed.
pub(super) fn generation_progress_text(progress: Progress) -> String {
    if progress.fraction >= 1. {
        return gettext("Finishing line art…");
    }
    let seconds = progress.remaining.as_secs_f64().ceil().max(1.) as u64;
    if seconds < 90 {
        gettext("Generating line art… ~{seconds} s left").replace("{seconds}", &seconds.to_string())
    } else {
        gettext("Generating line art… ~{minutes} min left")
            .replace("{minutes}", &seconds.div_ceil(60).to_string())
    }
}

pub(super) fn resampling_index(resampling: Resampling) -> u32 {
    match resampling {
        Resampling::Nearest => 0,
        Resampling::Bicubic => 1,
        Resampling::GameAsset(_) => 2,
        Resampling::Lanczos => 3,
    }
}
pub(super) fn resampling_at(index: u32) -> Resampling {
    match index {
        0 => Resampling::Nearest,
        2 => Resampling::GameAsset(Default::default()),
        3 => Resampling::Lanczos,
        _ => Resampling::Bicubic,
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;

    fn identity_background_remover(
        image: &image::RgbaImage,
        _: &CancellationToken,
    ) -> crate::error::Result<image::RgbaImage> {
        Ok(image.clone())
    }

    fn fixture_pair(
        image: &image::RgbaImage,
        size: (u32, u32),
        cancel: &CancellationToken,
        progress: &dyn Fn(Progress),
    ) -> crate::error::Result<crate::tools::line_art::LineArtPair> {
        crate::tools::scale::game_asset::model_like_test_pair(image, size, cancel, progress)
    }

    #[test]
    fn generation_progress_text_rounds_up_to_seconds_then_minutes() {
        let text = |fraction, seconds| {
            generation_progress_text(Progress {
                fraction,
                remaining: Duration::from_secs_f64(seconds),
            })
        };
        for (seconds, expected) in [
            (0., "Generating line art… ~1 s left"),
            (11.2, "Generating line art… ~12 s left"),
            (89., "Generating line art… ~89 s left"),
            (90., "Generating line art… ~2 min left"),
            (125., "Generating line art… ~3 min left"),
        ] {
            assert_eq!(text(0.5, seconds), expected);
        }
        assert_eq!(text(1., 0.), "Finishing line art…");
    }

    #[test]
    #[ignore = "requires a graphical display and compiled schema; run with GSETTINGS_BACKEND=memory"]
    fn game_asset_strength_control_updates_preview_and_committed_operation() {
        adw::init().expect("GTK initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik_private.Diorama.ScaleStrengthTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application.register(gio::Cancellable::NONE).unwrap();
        application.set_accels_for_action("win.zoom-100", &["1"]);
        let window = ViewerWindow::new(&application, None);
        let source = Arc::new(image::RgbaImage::from_fn(96, 80, |x, y| {
            image::Rgba(if (y as f64 - (0.57 * x as f64 + 10.)).abs() < 3. {
                [8, 10, 4, 255]
            } else {
                [130, 170, 90, 255]
            })
        }));
        window
            .0
            .document
            .replace(Some(Document::new(crate::document::ImageSource {
                pixels: source.clone(),
                path: None,
                metadata: Default::default(),
            })));
        window
            .0
            .canvas
            .set_texture(Some(&texture_from_rgba(&source).unwrap()));
        window.0.rendered.replace(Some((*source).clone()));
        window.0.content_stack.set_visible_child_name("viewer");
        window.update_action_states();
        let remove_background = Arc::new(identity_background_remover);
        let session = Arc::new(crate::tools::scale::game_asset::Session::with_test_workers(
            source.clone(),
            remove_background,
            Arc::new(fixture_pair),
        ));
        window.0.scale_button.set_active(true);
        window.0.scale_method.set_selected(0);
        assert!(!window.0.scale_strength_controls.get_visible());
        window.0.scale_method.set_selected(2);
        assert!(window.0.scale_strength_controls.get_visible());
        let top_row = window.0.scale_strength_controls.parent().unwrap();
        assert_eq!(
            top_row.parent().unwrap().first_child(),
            Some(top_row.clone())
        );
        assert_eq!(
            top_row.last_child(),
            Some(window.0.scale_strength_controls.clone().upcast())
        );
        let mut child = window.0.scale_strength_controls.first_child();
        while let Some(widget) = child {
            assert!(!widget.is::<gtk::Scale>(), "Strength has no slider");
            child = widget.next_sibling();
        }
        assert_eq!(window.0.scale_strength.value(), 40.);
        assert_eq!(window.0.scale_strength.adjustment().lower(), 0.);
        assert_eq!(window.0.scale_strength.adjustment().upper(), 100.);
        assert_eq!(
            window.0.scale_strength.tooltip_text().as_deref(),
            Some("Line-art sharpening: 0% applies a light unsharp mask; 100% the strongest")
        );
        window.0.scale_game_asset.replace(Some(session.clone()));
        window.0.scale_width.set_value(32.);
        window.present();
        let context = glib::MainContext::default();
        let wait_for_preview = || {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while window.0.scale_spinner.get_visible() && std::time::Instant::now() < deadline {
                context.iteration(false);
                std::thread::yield_now();
            }
            assert!(!window.0.scale_spinner.get_visible(), "preview completed");
        };
        wait_for_preview();
        let preserved_zoom = 1.375;
        window.set_scale_preview_zoom(preserved_zoom);
        for percent in [0, 100, 40] {
            window.0.scale_strength.set_value(f64::from(percent));
            assert_eq!(window.0.scale_strength.value(), f64::from(percent));
            let options = GameAssetOptions::new(percent);
            assert_eq!(
                window.0.scale_resampling.get(),
                Resampling::GameAsset(options)
            );
            wait_for_preview();
            assert_eq!(window.0.settings.game_asset_options(), options);
            let expected = session
                .resize(32, 27, options, &CancellationToken::default(), &|_| {})
                .unwrap();
            assert_eq!(
                window.0.scale_preview.borrow().as_ref().unwrap().as_ref(),
                &expected
            );
            assert_eq!(window.0.canvas.zoom(), preserved_zoom);
        }
        // Switching methods hides the paired controls without forgetting them
        // or contaminating another method. Rapid changes publish only the latest value.
        window.0.scale_strength.set_value(75.);
        for method in [0, 1, 3] {
            window.0.scale_method.set_selected(method);
            assert!(!window.0.scale_strength_controls.get_visible());
        }
        window.0.scale_method.set_selected(2);
        assert_eq!(window.0.scale_strength.value(), 75.);
        window.0.scale_strength.set_value(0.);
        let obsolete = window
            .0
            .scale_preview_cancellation
            .borrow()
            .as_ref()
            .unwrap()
            .clone();
        window.0.scale_strength.set_value(100.);
        assert!(obsolete.check().is_err());
        wait_for_preview();
        let expected = session
            .resize(
                32,
                27,
                GameAssetOptions::new(100),
                &CancellationToken::default(),
                &|_| {},
            )
            .unwrap();
        assert_eq!(
            window.0.scale_preview.borrow().as_ref().unwrap().as_ref(),
            &expected
        );
        assert_eq!(window.0.canvas.zoom(), preserved_zoom);

        window.0.scale_strength.grab_focus();
        while context.pending() {
            context.iteration(false);
        }
        assert!(application.accels_for_action("win.zoom-100").is_empty());
        let controllers = window.0.scale_strength.observe_controllers();
        assert!(
            (0..controllers.n_items())
                .filter_map(|i| controllers.item(i))
                .filter_map(|c| c.downcast::<gtk::EventControllerKey>().ok())
                .filter(|c| c.propagation_phase() == gtk::PropagationPhase::Capture)
                .any(|c| c.emit_by_name::<bool>(
                    "key-pressed",
                    &[
                        &gtk::gdk::Key::Escape,
                        &0_u32,
                        &gtk::gdk::ModifierType::empty()
                    ]
                ))
        );
        assert_eq!(
            gtk::prelude::GtkWindowExt::focus(&window.0.window),
            Some(window.0.canvas.clone().upcast())
        );

        // Test at the existing panel's native minimum (which includes its
        // dimension controls), plus medium and wide widths. Do not allocate
        // below GTK's measured minimum and mistake that for a supported size.
        let minimum = window
            .0
            .scale_controls
            .measure(gtk::Orientation::Horizontal, -1)
            .0;
        assert!(
            minimum <= 1000,
            "the Game Asset strength spin stays usable at 1000px"
        );
        for width in [minimum, minimum.max(700), 1000] {
            window.0.canvas_overlay.allocate(width, 600, -1, None);
            let row = &window.0.scale_strength_controls;
            assert!(
                row.width() > 0 && row.width() <= width - 52,
                "requested overlay {width}, actual {}, strength row {}, controls {}, minimum {:?}",
                window.0.canvas_overlay.width(),
                row.width(),
                window.0.scale_controls.width(),
                window
                    .0
                    .scale_controls
                    .measure(gtk::Orientation::Horizontal, -1)
            );
            let bounds = window.0.scale_strength.compute_bounds(row).unwrap();
            assert!(bounds.x() >= 0. && bounds.x() + bounds.width() <= row.width() as f32);
            let bounds = row.compute_bounds(&top_row).unwrap();
            assert_eq!(bounds.y(), 0., "Strength stays on the first row");
            assert_eq!(
                bounds.x() + bounds.width(),
                top_row.width() as f32,
                "Strength stays at the right"
            );
        }
        if let Ok(path) = std::env::var("DIORAMA_STRENGTH_UI_CAPTURE") {
            let paintable = gtk::WidgetPaintable::new(Some(&window.0.window));
            window.0.window.queue_resize();
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            let node = loop {
                context.iteration(false);
                let snapshot = gtk::Snapshot::new();
                paintable.snapshot(
                    &snapshot,
                    window.0.window.width() as f64,
                    window.0.window.height() as f64,
                );
                if let Some(node) = snapshot.to_node() {
                    break node;
                }
                assert!(std::time::Instant::now() < deadline, "window painted");
                std::thread::yield_now();
            };
            window
                .0
                .window
                .renderer()
                .unwrap()
                .render_texture(&node, None)
                .save_to_png(path)
                .unwrap();
        }
        window.confirm_scale_preview();
        assert_eq!(
            window.0.document.borrow().as_ref().unwrap().operations(),
            &[Operation::Scale {
                width: 32,
                height: 27,
                resampling: Resampling::GameAsset(GameAssetOptions::new(100))
            }]
        );
        window.0.window.close();
    }

    #[test]
    #[ignore = "requires a graphical display and compiled schema; run with GSETTINGS_BACKEND=memory"]
    fn game_asset_generation_shows_estimated_progress_and_resets_on_cancellation() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        adw::init().expect("GTK initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik_private.Diorama.ScaleProgressTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application.register(gio::Cancellable::NONE).unwrap();
        let window = ViewerWindow::new(&application, None);
        let source = Arc::new(image::RgbaImage::from_fn(96, 80, |x, y| {
            image::Rgba(if (y as f64 - (0.57 * x as f64 + 10.)).abs() < 3. {
                [8, 10, 4, 255]
            } else {
                [130, 170, 90, 255]
            })
        }));
        window
            .0
            .document
            .replace(Some(Document::new(crate::document::ImageSource {
                pixels: source.clone(),
                path: None,
                metadata: Default::default(),
            })));
        window
            .0
            .canvas
            .set_texture(Some(&texture_from_rgba(&source).unwrap()));
        window.0.rendered.replace(Some((*source).clone()));
        window.0.content_stack.set_visible_child_name("viewer");
        window.update_action_states();
        // Each generation reports an estimate, then runs until released or
        // cancelled.
        let release = Arc::new(AtomicBool::new(false));
        let runs = Arc::new(AtomicUsize::new(0));
        let generator: Arc<crate::tools::scale::game_asset::LineArtGenerator> = {
            let (release, runs) = (release.clone(), runs.clone());
            Arc::new(move |image, size, cancel, progress| {
                runs.fetch_add(1, Ordering::Relaxed);
                progress(Progress {
                    fraction: 0.25,
                    remaining: Duration::from_millis(11_200),
                });
                while !release.load(Ordering::Relaxed) {
                    cancel.check()?;
                    std::thread::sleep(Duration::from_millis(5));
                }
                fixture_pair(image, size, cancel, progress)
            })
        };
        let session = Arc::new(crate::tools::scale::game_asset::Session::with_test_workers(
            source.clone(),
            Arc::new(identity_background_remover),
            generator,
        ));
        window.0.scale_button.set_active(true);
        window.0.scale_method.set_selected(2);
        window.0.scale_game_asset.replace(Some(session.clone()));
        assert!(!window.0.scale_progress.get_visible());
        window.0.scale_width.set_value(32.);
        window.present();

        let context = glib::MainContext::default();
        let wait_until = |condition: &dyn Fn() -> bool, what: &str| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !condition() && std::time::Instant::now() < deadline {
                context.iteration(false);
                std::thread::yield_now();
            }
            assert!(condition(), "{what}");
        };
        let progress = &window.0.scale_progress;
        wait_until(&|| progress.get_visible(), "the estimate is shown");
        assert_eq!(
            progress.text().as_deref(),
            Some("Generating line art… ~12 s left")
        );
        assert_eq!(progress.fraction(), 0.25);
        assert!(window.0.scale_spinner.get_visible());

        // A new size cancels the running generation and resets the bar.
        let obsolete = window
            .0
            .scale_preview_cancellation
            .borrow()
            .as_ref()
            .unwrap()
            .clone();
        window.0.scale_width.set_value(48.);
        assert!(obsolete.check().is_err());
        assert!(!progress.get_visible());
        assert_eq!(progress.fraction(), 0.);
        wait_until(
            &|| progress.get_visible(),
            "the new generation's estimate is shown",
        );
        release.store(true, Ordering::Relaxed);
        wait_until(
            &|| !window.0.scale_spinner.get_visible(),
            "the preview completed",
        );
        assert!(!progress.get_visible());
        assert_eq!(runs.load(Ordering::Relaxed), 2);
        let expected = session
            .resize(
                48,
                40,
                GameAssetOptions::default(),
                &CancellationToken::default(),
                &|_| panic!("the pair is cached"),
            )
            .unwrap();
        assert_eq!(
            window.0.scale_preview.borrow().as_ref().unwrap().as_ref(),
            &expected
        );

        // A Strength change reuses the pair: no generation, no bar.
        window.0.scale_strength.set_value(100.);
        wait_until(
            &|| !window.0.scale_spinner.get_visible(),
            "the re-sharpened preview completed",
        );
        assert!(!progress.get_visible());
        assert_eq!(runs.load(Ordering::Relaxed), 2);
        window.0.window.close();
    }

    #[test]
    #[ignore = "requires a graphical display and compiled schema; run with GSETTINGS_BACKEND=memory"]
    fn game_asset_keeps_the_generation_worker_loaded_only_while_selected() {
        adw::init().expect("GTK initialization");
        let root = tempfile::tempdir().unwrap();
        crate::tools::line_art::fake_worker::install(
            root.path(),
            r#"#!/bin/bash
dir="$(dirname "$0")"
echo serve >> "$dir/launches"
echo '{"event": "stage", "stage": "load"}'
echo '{"event": "ready"}'
while IFS= read -r request; do :; done
echo stopped >> "$dir/launches"
"#,
        );
        let launches = || fs_read(&root.path().join("launches"));
        let application = adw::Application::builder()
            .application_id("io.github.mendrik_private.Diorama.ScaleWorkerTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application.register(gio::Cancellable::NONE).unwrap();
        let window = ViewerWindow::new(&application, None);
        let source = Arc::new(image::RgbaImage::from_pixel(
            96,
            80,
            image::Rgba([130, 170, 90, 255]),
        ));
        window
            .0
            .document
            .replace(Some(Document::new(crate::document::ImageSource {
                pixels: source.clone(),
                path: None,
                metadata: Default::default(),
            })));
        window
            .0
            .canvas
            .set_texture(Some(&texture_from_rgba(&source).unwrap()));
        window.0.rendered.replace(Some((*source).clone()));
        window.0.content_stack.set_visible_child_name("viewer");
        window.update_action_states();
        window.present();
        let context = glib::MainContext::default();
        let wait_for = |expected: &str| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while launches() != expected && std::time::Instant::now() < deadline {
                context.iteration(false);
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(launches(), expected);
        };

        // Another method in the Scale tool loads nothing.
        window.0.scale_method.set_selected(0);
        window.0.scale_button.set_active(true);
        std::thread::sleep(Duration::from_millis(200));
        wait_for("");
        // Choosing Game Asset starts the worker, leaving it stops it.
        window.0.scale_method.set_selected(2);
        wait_for("serve\n");
        window.0.scale_method.set_selected(1);
        wait_for("serve\nstopped\n");
        window.0.scale_method.set_selected(2);
        wait_for("serve\nstopped\nserve\n");
        // Closing the Scale tool stops it too.
        window.0.scale_button.set_active(false);
        wait_for("serve\nstopped\nserve\nstopped\n");
        // Opening the tool with Game Asset selected starts it again.
        window.0.scale_button.set_active(true);
        wait_for("serve\nstopped\nserve\nstopped\nserve\n");
        window.0.scale_button.set_active(false);
        wait_for("serve\nstopped\nserve\nstopped\nserve\nstopped\n");
        window.0.window.close();
    }

    fn fs_read(path: &std::path::Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    #[ignore = "requires a graphical display and compiled schema; run with GSETTINGS_BACKEND=memory"]
    fn game_asset_line_art_toggle_uses_the_preview_worker_without_committing_it() {
        adw::init().expect("GTK initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik_private.Diorama.LineArtPreviewTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application.register(gio::Cancellable::NONE).unwrap();
        let window = ViewerWindow::new(&application, None);
        let source = Arc::new(image::RgbaImage::from_fn(96, 80, |x, y| {
            image::Rgba(if (y as f64 - (0.57 * x as f64 + 10.)).abs() < 3. {
                [8, 10, 4, 255]
            } else {
                [130, 170, 90, 255]
            })
        }));
        window
            .0
            .document
            .replace(Some(Document::new(crate::document::ImageSource {
                pixels: source.clone(),
                path: None,
                metadata: Default::default(),
            })));
        window
            .0
            .canvas
            .set_texture(Some(&texture_from_rgba(&source).unwrap()));
        window.0.rendered.replace(Some((*source).clone()));
        window.0.content_stack.set_visible_child_name("viewer");
        window.update_action_states();
        let session = Arc::new(crate::tools::scale::game_asset::Session::with_test_workers(
            source.clone(),
            Arc::new(identity_background_remover),
            Arc::new(fixture_pair),
        ));
        window.0.scale_button.set_active(true);
        window.0.scale_method.set_selected(2);
        window.0.scale_game_asset.replace(Some(session.clone()));
        window.0.scale_width.set_value(32.);
        window.present();

        let context = glib::MainContext::default();
        let wait_for_preview = || {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while window.0.scale_spinner.get_visible() && std::time::Instant::now() < deadline {
                context.iteration(false);
                std::thread::yield_now();
            }
            assert!(!window.0.scale_spinner.get_visible(), "preview completed");
        };
        wait_for_preview();
        assert!(window.0.scale_show_line_art.get_visible());
        let preserved_zoom = 1.375;
        window.set_scale_preview_zoom(preserved_zoom);

        window.0.scale_show_line_art.set_active(true);
        wait_for_preview();
        let target_line_art = session
            .line_art(
                32,
                27,
                GameAssetOptions::default(),
                &CancellationToken::default(),
                &|_| {},
            )
            .unwrap();
        assert_eq!(
            window.0.scale_preview.borrow().as_ref().unwrap().as_ref(),
            &target_line_art
        );
        assert!(target_line_art.pixels().all(|pixel| {
            let [r, g, b, a] = pixel.0;
            r == g && g == b && a == 255
        }));
        assert!(
            target_line_art.pixels().any(|pixel| pixel[0] < 255),
            "line-art preview includes ink"
        );
        assert!(
            target_line_art
                .pixels()
                .any(|pixel| pixel.0 == [255, 255, 255, 255]),
            "line-art preview retains its white background"
        );
        assert_eq!(window.0.canvas.zoom(), preserved_zoom);

        // The strength control re-renders the sharpened line-art preview.
        window.0.scale_strength.set_value(100.);
        wait_for_preview();
        assert_eq!(
            window.0.scale_preview.borrow().as_ref().unwrap().as_ref(),
            &session
                .line_art(
                    32,
                    27,
                    GameAssetOptions::new(100),
                    &CancellationToken::default(),
                    &|_| {},
                )
                .unwrap()
        );
        window.0.scale_strength.set_value(40.);
        wait_for_preview();

        window.set_scale_original_visible(true);
        assert_eq!(
            (
                window.0.canvas.texture().unwrap().width(),
                window.0.canvas.texture().unwrap().height(),
            ),
            (96, 80)
        );
        window.set_scale_original_visible(false);
        assert_eq!(
            (
                window.0.canvas.texture().unwrap().width(),
                window.0.canvas.texture().unwrap().height(),
            ),
            (32, 27)
        );

        // Source-sized line art is diagnostic work too, instead of the
        // normal immediate source preview shortcut.
        window.0.scale_width.set_value(96.);
        wait_for_preview();
        let source_line_art = session
            .line_art(
                96,
                80,
                GameAssetOptions::default(),
                &CancellationToken::default(),
                &|_| {},
            )
            .unwrap();
        assert_eq!(
            window.0.scale_preview.borrow().as_ref().unwrap().as_ref(),
            &source_line_art
        );
        window.0.scale_show_line_art.set_active(false);
        assert_eq!(
            window.0.scale_preview.borrow().as_ref().unwrap().as_ref(),
            source.as_ref()
        );

        window.0.scale_show_line_art.set_active(true);
        window.0.scale_method.set_selected(0);
        assert!(!window.0.scale_show_line_art.get_visible());
        assert!(!window.0.scale_show_line_art.is_active());
        window.0.scale_method.set_selected(2);
        assert!(window.0.scale_show_line_art.get_visible());
        window.0.scale_width.set_value(32.);
        wait_for_preview();

        let minimum = window
            .0
            .scale_controls
            .measure(gtk::Orientation::Horizontal, -1)
            .0;
        assert!(minimum <= 1000, "line-art toggle remains usable at 1000px");
        for width in [minimum, minimum.max(700), 1000] {
            window.0.canvas_overlay.allocate(width, 600, -1, None);
            let slider_row = window.0.scale_show_line_art.parent().unwrap();
            let bounds = window
                .0
                .scale_show_line_art
                .compute_bounds(&slider_row)
                .unwrap();
            assert!(
                bounds.x() >= 0. && bounds.x() + bounds.width() <= slider_row.width() as f32,
                "line-art toggle stays within the slider row at {width}px"
            );
        }

        window.0.scale_show_line_art.set_active(true);
        wait_for_preview();
        let resampling = window.0.scale_resampling.get();
        window.confirm_scale_preview();
        assert!(!window.0.scale_show_line_art.is_active());
        assert_eq!(
            window.0.document.borrow().as_ref().unwrap().operations(),
            &[Operation::Scale {
                width: 32,
                height: 27,
                resampling,
            }],
            "applying from diagnostics records normal Game Asset scaling"
        );
        window.0.window.close();
    }
}
