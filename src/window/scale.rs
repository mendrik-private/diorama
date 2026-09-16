use crate::document::Resampling;

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

    #[test]
    #[ignore = "requires a graphical display and compiled schema; run with GSETTINGS_BACKEND=memory"]
    fn game_asset_aa_control_updates_preview_and_committed_operation() {
        adw::init().expect("GTK initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik_private.Diorama.ScaleAaTest")
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
        window.0.scale_button.set_active(true);
        window.0.scale_method.set_selected(0);
        assert!(!window.0.scale_aa_controls.get_visible());
        window.0.scale_method.set_selected(2);
        assert!(window.0.scale_aa_controls.get_visible());
        let top_row = window.0.scale_aa_controls.parent().unwrap();
        assert_eq!(
            top_row.parent().unwrap().first_child(),
            Some(top_row.clone())
        );
        assert_eq!(
            top_row.last_child(),
            Some(window.0.scale_aa_controls.clone().upcast())
        );
        let mut child = window.0.scale_aa_controls.first_child();
        while let Some(widget) = child {
            assert!(!widget.is::<gtk::Scale>(), "AA has no slider");
            child = widget.next_sibling();
        }
        assert_eq!(window.0.scale_aa.value(), 50.);
        assert_eq!(window.0.scale_aa.adjustment().lower(), 0.);
        assert_eq!(window.0.scale_aa.adjustment().upper(), 100.);
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
        let session = crate::tools::scale::game_asset::Session::new(source);
        for percent in [0, 100, 50] {
            window.0.scale_aa.set_value(f64::from(percent));
            assert_eq!(window.0.scale_aa.value(), f64::from(percent));
            let aa = GameAssetAa::new(percent);
            assert_eq!(window.0.scale_resampling.get(), Resampling::GameAsset(aa));
            wait_for_preview();
            assert_eq!(window.0.settings.game_asset_aa(), aa);
            let expected = session
                .resize(32, 27, aa, &CancellationToken::default())
                .unwrap();
            assert_eq!(
                window.0.scale_preview.borrow().as_ref().unwrap().as_ref(),
                &expected
            );
            assert_eq!(window.0.canvas.zoom(), preserved_zoom);
        }
        // Switching methods hides AA without forgetting it or contaminating
        // another method. Rapid AA changes must publish only the latest value.
        window.0.scale_aa.set_value(75.);
        for method in [0, 1, 3] {
            window.0.scale_method.set_selected(method);
            assert!(!window.0.scale_aa_controls.get_visible());
        }
        window.0.scale_method.set_selected(2);
        assert_eq!(window.0.scale_aa.value(), 75.);
        window.0.scale_aa.set_value(0.);
        let obsolete = window
            .0
            .scale_preview_cancellation
            .borrow()
            .as_ref()
            .unwrap()
            .clone();
        window.0.scale_aa.set_value(100.);
        assert!(obsolete.check().is_err());
        wait_for_preview();
        let expected = session
            .resize(32, 27, GameAssetAa::new(100), &CancellationToken::default())
            .unwrap();
        assert_eq!(
            window.0.scale_preview.borrow().as_ref().unwrap().as_ref(),
            &expected
        );
        assert_eq!(window.0.canvas.zoom(), preserved_zoom);

        window.0.scale_aa.grab_focus();
        while context.pending() {
            context.iteration(false);
        }
        assert!(application.accels_for_action("win.zoom-100").is_empty());
        let controllers = window.0.scale_aa.observe_controllers();
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
        for width in [minimum, minimum.max(700), minimum.max(1000)] {
            window.0.canvas_overlay.allocate(width, 600, -1, None);
            let row = &window.0.scale_aa_controls;
            assert!(
                row.width() > 0 && row.width() <= width - 52,
                "requested overlay {width}, actual {}, AA row {}, controls {}, minimum {:?}",
                window.0.canvas_overlay.width(),
                row.width(),
                window.0.scale_controls.width(),
                window
                    .0
                    .scale_controls
                    .measure(gtk::Orientation::Horizontal, -1)
            );
            let bounds = window.0.scale_aa.compute_bounds(row).unwrap();
            assert!(bounds.x() >= 0. && bounds.x() + bounds.width() <= row.width() as f32);
            let bounds = row.compute_bounds(&top_row).unwrap();
            assert_eq!(bounds.y(), 0., "AA stays on the first row");
            assert_eq!(
                bounds.x() + bounds.width(),
                top_row.width() as f32,
                "AA stays at the right"
            );
        }
        if let Ok(path) = std::env::var("DIORAMA_AA_UI_CAPTURE") {
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
                resampling: Resampling::GameAsset(GameAssetAa::new(100))
            }]
        );
        window.0.window.close();
    }
}
