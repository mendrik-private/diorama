//! Opt-in captures of the real window, rendered on a Wayland compositor.
use super::*;

#[test]
#[ignore = "requires DIORAMA_SCREENSHOT_SOURCE, DIORAMA_SCREENSHOT_DIR and a graphical display"]
fn capture_store_screenshots() {
    let Ok(source) = std::env::var("DIORAMA_SCREENSHOT_SOURCE") else {
        return;
    };
    let output = std::env::var("DIORAMA_SCREENSHOT_DIR").expect("screenshot output directory");
    adw::init().expect("GTK initialization");
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight);
    let application = adw::Application::builder()
        .application_id(crate::APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    application
        .register(gio::Cancellable::NONE)
        .expect("registration");
    let window = ViewerWindow::new(&application, Some(gio::File::for_path(source)));
    window.0.window.set_default_size(1280, 800);
    window.present();
    let settle = || {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < end {
            glib::MainContext::default().iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    settle();
    assert!(window.0.rendered.borrow().is_some(), "source image loaded");
    gio::prelude::ActionGroupExt::activate_action(&window.0.window, "fit", None);
    settle();
    let capture = |name: &str| {
        let widget = &window.0.window;
        let snapshot = gtk::Snapshot::new();
        gtk::WidgetPaintable::new(Some(widget)).snapshot(
            &snapshot,
            widget.width() as f64,
            widget.height() as f64,
        );
        let node = snapshot.to_node().expect("window snapshot");
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
            .save_to_png(std::path::Path::new(&output).join(name))
            .expect("PNG capture");
    };
    std::fs::create_dir_all(&output).expect("screenshot directory");
    capture("browse.png");
    let (image_width, image_height) = window.0.rendered.borrow().as_ref().unwrap().dimensions();
    window.set_region_selection(Some(CropOverlay {
        x: image_width / 4,
        y: image_height / 4,
        width: image_width / 2,
        height: image_height / 2,
        image_width,
        image_height,
    }));
    settle();
    capture("selection.png");
    window.0.window.close();
}

#[test]
#[ignore = "requires two screenshot sources, a screenshot directory, and a graphical display"]
fn capture_guide_screenshots() {
    let Ok(source) = std::env::var("DIORAMA_SCREENSHOT_SOURCE") else {
        return;
    };
    let output = std::env::var("DIORAMA_SCREENSHOT_DIR").expect("screenshot output directory");
    let compare_source =
        std::env::var("DIORAMA_SCREENSHOT_COMPARE_SOURCE").expect("comparison screenshot source");
    adw::init().expect("GTK initialization");
    // The workstation's interactive desktop intentionally uses a 192 DPI
    // scale. Guide PNGs are ordinary 1× web images, so pin their GTK font
    // metrics to the standard 96 DPI desktop before any widgets are made.
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
        .prefix("diorama-guide-")
        .tempdir()
        .expect("guide fixture directory");
    let source_copy = fixture.path().join("red-wyvern.png");
    let compare_copy = fixture.path().join("stone-gargoyle.png");
    std::fs::copy(source, &source_copy).expect("copy primary guide fixture");
    std::fs::copy(compare_source, &compare_copy).expect("copy comparison guide fixture");
    // These derived fixtures exist only for screenshots.  Keeping them in the
    // test tempdir lets the guide show Crop to Content on a transparent border
    // without altering the supplied artwork.
    let source_pixels = image::open(&source_copy)
        .expect("open primary guide fixture")
        .into_rgba8();
    let mut padded_pixels =
        image::RgbaImage::new(source_pixels.width() + 128, source_pixels.height() + 128);
    for (x, y, pixel) in source_pixels.enumerate_pixels() {
        padded_pixels.put_pixel(x + 64, y + 64, *pixel);
    }
    let padded_copy = fixture.path().join("wyvern-with-transparent-border.png");
    padded_pixels
        .save(&padded_copy)
        .expect("write padded crop fixture");
    let nearest_copy = fixture.path().join("nearest-neighbor-256px.png");
    let lanczos_copy = fixture.path().join("lanczos-256px.png");
    let cancel = CancellationToken::default();
    crate::tools::scale::resize(&source_pixels, 256, 256, Resampling::Nearest, &cancel)
        .expect("make nearest-neighbor comparison fixture")
        .save(&nearest_copy)
        .expect("write nearest-neighbor comparison fixture");
    crate::tools::scale::resize(&source_pixels, 256, 256, Resampling::Lanczos, &cancel)
        .expect("make Lanczos comparison fixture")
        .save(&lanczos_copy)
        .expect("write Lanczos comparison fixture");
    let window = ViewerWindow::new(&application, Some(gio::File::for_path(source_copy)));
    window.0.window.set_default_size(1280, 800);
    window.present();
    let settle = || {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < end {
            glib::MainContext::default().iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    settle();
    assert!(window.0.rendered.borrow().is_some(), "source image loaded");
    assert!(
        (window.0.render_scale.get() - 1.0).abs() < f64::EPSILON,
        "guide screenshots require a dedicated 1× compositor"
    );
    gio::prelude::ActionGroupExt::activate_action(&window.0.window, "fit", None);
    settle();
    let capture = |window: &ViewerWindow, name: &str| {
        let widget = &window.0.window;
        // A transient unmapped frame is possible immediately after a compare
        // pane or dialog changes. Let GTK finish one or two frames rather than
        // emitting an empty guide asset.
        let mut node = None;
        for _ in 0..20 {
            let snapshot = gtk::Snapshot::new();
            gtk::WidgetPaintable::new(Some(widget)).snapshot(
                &snapshot,
                widget.width() as f64,
                widget.height() as f64,
            );
            if let Some(next) = snapshot.to_node() {
                node = Some(next);
                break;
            }
            widget.queue_draw();
            glib::MainContext::default().iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let node = node.unwrap_or_else(|| panic!("window snapshot for {name}"));
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
            .save_to_png(std::path::Path::new(&output).join(name))
            .expect("PNG capture");
    };
    let capture_dialog = |window: &ViewerWindow, dialog: &adw::Dialog, name: &str| {
        let widget: &gtk::Widget = dialog.upcast_ref();
        let parent = widget.parent().expect("dialog host");
        let mut node = None;
        for _ in 0..20 {
            let snapshot = gtk::Snapshot::new();
            parent.snapshot_child(widget, &snapshot);
            if let Some(next) = snapshot.to_node() {
                node = Some(next);
                break;
            }
            widget.queue_draw();
            glib::MainContext::default().iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let node = node.unwrap_or_else(|| panic!("dialog snapshot for {name}"));
        let bounds = node.bounds();
        let texture = window
            .0
            .window
            .renderer()
            .expect("dialog renderer")
            .render_texture(&node, Some(&bounds));
        texture
            .save_to_png(std::path::Path::new(&output).join(name))
            .expect("dialog PNG capture");
    };
    std::fs::create_dir_all(&output).expect("screenshot directory");
    // The guide deliberately captures a real application window: its widgets,
    // canvas overlays, dialogs, and annotations all take the normal rendering
    // paths. The fixture source is read-only and lives outside the repository.
    capture(&window, "overview.png");
    if std::env::var_os("DIORAMA_SCREENSHOT_OVERVIEW_ONLY").is_some() {
        window.0.window.close();
        return;
    }
    if std::env::var_os("DIORAMA_SCREENSHOT_DIALOGS_ONLY").is_some() {
        window.show_canvas_resize();
        settle();
        let dialog = window
            .0
            .window
            .visible_dialog()
            .expect("canvas resize dialog");
        capture_dialog(&window, &dialog, "canvas-resize.png");
        dialog.close();
        settle();
        window.show_preferences();
        settle();
        let dialog = window
            .0
            .window
            .visible_dialog()
            .expect("preferences dialog");
        capture_dialog(&window, &dialog, "preferences.png");
        dialog.close();
        settle();
        let document = window
            .0
            .document
            .borrow()
            .clone()
            .expect("editable document");
        let snapshot = ExportSnapshot {
            operations: document.operations().into(),
            document,
            source_file: window.0.current_file.borrow().clone(),
            load_generation: window.0.load_generation.get(),
        };
        window.show_export_options(snapshot, std::path::PathBuf::from("wyvern-guide.jpg"));
        settle();
        let dialog = window
            .0
            .window
            .visible_dialog()
            .expect("export options dialog");
        capture_dialog(&window, &dialog, "export-options.png");
        dialog.close();
        window.0.window.close();
        return;
    }

    let lens_texture = window.0.canvas.texture().expect("loaded canvas texture");
    window.set_single_image_lens_active(true);
    window
        .0
        .canvas
        .set_lens(&lens_texture, 0.84, 0.63, 160.0, 4.0, true);
    settle();
    capture(&window, "inspect-pixel-lens.png");
    window.0.canvas.clear_lens();

    window.load_comparison(gio::File::for_path(compare_copy));
    settle();
    window.present();
    settle();
    let comparison = window
        .0
        .compare_canvas
        .borrow()
        .clone()
        .expect("comparison image loaded");
    let primary_texture = window.0.canvas.texture().expect("primary texture");
    let comparison_texture = comparison.texture().expect("comparison texture");
    window.set_single_image_lens_active(true);
    // This matches the live comparison motion handler: each pane magnifies
    // its own image at a synchronized position; only the source shows a crosshair.
    window
        .0
        .canvas
        .set_lens(&primary_texture, 0.62, 0.40, 220.0, 4.0, true);
    comparison.set_lens(&comparison_texture, 0.62, 0.40, 220.0, 4.0, false);
    settle();
    capture(&window, "compare-lenses.png");
    comparison.clear_lens();
    window.0.canvas.clear_lens();
    window.exit_compare();
    window.set_single_image_lens_active(false);
    settle();

    let (image_width, image_height) = window.0.rendered.borrow().as_ref().unwrap().dimensions();
    window.set_region_selection(Some(CropOverlay {
        x: image_width / 5,
        y: image_height / 5,
        width: image_width * 3 / 5,
        height: image_height * 3 / 5,
        image_width,
        image_height,
    }));
    settle();
    capture(&window, "edit-selection.png");
    window.set_region_selection(None);

    // Scaling controls are captured on the normal interactive preview.  The
    // paired 256 px files were created by the same scaler and then opened in
    // Diorama's comparison view, at actual pixels, so the method differences
    // are visible rather than merely listed in prose.
    window.set_tool(Tool::Scale);
    settle();
    window.0.scale_method.set_selected(0);
    window.0.scale_width.set_value(512.0);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while (window.0.scale_preview.borrow().is_none() || window.0.scale_spinner.is_visible())
        && std::time::Instant::now() < deadline
    {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        window.0.scale_preview.borrow().is_some(),
        "nearest preview completed"
    );
    settle();
    capture(&window, "scale-options.png");
    // Reopen the two real 256 px outputs so both sides of the comparison are
    // ordinary Diorama documents, with their method names in the window title.
    window.set_tool(Tool::None);
    window.load_with_fit(gio::File::for_path(&nearest_copy), true);
    settle();
    assert_eq!(
        window
            .0
            .rendered
            .borrow()
            .as_ref()
            .map(image::GenericImageView::dimensions),
        Some((256, 256)),
        "nearest comparison image loaded"
    );
    window.load_comparison(gio::File::for_path(lanczos_copy));
    settle();
    let comparison = window
        .0
        .compare_canvas
        .borrow()
        .clone()
        .expect("Lanczos comparison image loaded");
    assert_eq!(
        comparison
            .texture()
            .map(|texture| (texture.width(), texture.height())),
        Some((256, 256)),
        "Lanczos comparison dimensions"
    );
    gio::prelude::ActionGroupExt::activate_action(&window.0.window, "zoom-200", None);
    settle();
    capture(&window, "scaling-method-comparison.png");
    window.exit_compare();
    window.load_with_fit(
        gio::File::for_path(fixture.path().join("red-wyvern.png")),
        true,
    );
    settle();
    assert_eq!(
        window
            .0
            .rendered
            .borrow()
            .as_ref()
            .map(image::GenericImageView::dimensions),
        Some((1024, 1024)),
        "Game Asset source reloaded"
    );
    window.set_tool(Tool::Scale);
    settle();

    // This uses the production local FLUX/BiRefNet path when the model cache
    // is available.  A test-layer stand-in would make this a misleading guide
    // illustration, so failure is explicit instead of silently fabricating a
    // Game Asset result.
    window.0.scale_method.set_selected(2);
    window.0.scale_width.set_value(512.0);
    settle();
    capture(&window, "game-asset-options.png");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    while (window.0.scale_preview.borrow().is_none() || window.0.scale_spinner.is_visible())
        && std::time::Instant::now() < deadline
    {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        window
            .0
            .scale_preview
            .borrow()
            .as_ref()
            .is_some_and(|preview| preview.dimensions() == (512, 512)),
        "Game Asset preview completed"
    );
    let game_asset_result = window
        .0
        .scale_preview
        .borrow()
        .as_ref()
        .expect("Game Asset preview")
        .as_ref()
        .clone();
    capture(&window, "game-asset-result.png");
    window.0.scale_show_line_art.set_active(true);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while (window.0.scale_preview.borrow().is_none() || window.0.scale_spinner.is_visible())
        && std::time::Instant::now() < deadline
    {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        window
            .0
            .scale_preview
            .borrow()
            .as_ref()
            .is_some_and(|preview| {
                preview.dimensions() == (512, 512)
                    && preview
                        .pixels()
                        .all(|pixel| pixel[0] == pixel[1] && pixel[1] == pixel[2])
                    && preview.as_ref() != &game_asset_result
            }),
        "Game Asset line-art preview completed"
    );
    capture(&window, "game-asset-line-art.png");
    window.set_tool(Tool::None);
    settle();

    // The transformation actions render their actual combined result.  The
    // temporary document is discarded with the capture window afterwards.
    gio::prelude::ActionGroupExt::activate_action(&window.0.window, "rotate-clockwise", None);
    gio::prelude::ActionGroupExt::activate_action(&window.0.window, "flip-horizontal", None);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !window.rendered_is_current() && std::time::Instant::now() < deadline {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        window.rendered_is_current(),
        "transformation render completed"
    );
    capture(&window, "flip-and-rotate.png");
    gio::prelude::ActionGroupExt::activate_action(&window.0.window, "undo", None);
    gio::prelude::ActionGroupExt::activate_action(&window.0.window, "undo", None);
    settle();

    // Grid guides, a placed node, and the displaced preview all take the live
    // mesh workflow.  Apply is intentionally omitted: this screenshot records
    // the editable draft state that the guide describes.
    window.set_tool(Tool::MeshPoints);
    window.0.isometric_grid_button.set_active(true);
    let node = window
        .0
        .canvas
        .widget_point_for_image(Point { x: 520.0, y: 470.0 })
        .expect("mesh node is visible");
    window.mesh_click(f64::from(node.x()), f64::from(node.y()));
    window.begin_mesh_warp();
    let moved = window
        .0
        .canvas
        .widget_point_for_image(Point { x: 570.0, y: 500.0 })
        .expect("mesh destination is visible");
    assert!(window.mesh_begin_drag(f64::from(node.x()), f64::from(node.y())));
    window.mesh_drag_to(f64::from(moved.x()), f64::from(moved.y()));
    window.mesh_end_drag();
    settle();
    capture(&window, "mesh-warp-grid.png");
    window.set_tool(Tool::None);
    settle();

    // This independent window gives Crop to Content a transparent 64 px border
    // to detect.  It is a derived fixture, never a modified source original.
    let crop_window = ViewerWindow::new(&application, Some(gio::File::for_path(padded_copy)));
    crop_window.0.window.set_default_size(1280, 800);
    crop_window.present();
    settle();
    assert!(
        crop_window.0.rendered.borrow().is_some(),
        "crop fixture loaded"
    );
    crop_window.crop_to_content();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while crop_window.0.window.visible_dialog().is_none() && std::time::Instant::now() < deadline {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let crop_dialog = crop_window
        .0
        .window
        .visible_dialog()
        .expect("crop confirmation opened");
    settle();
    capture_dialog(&crop_window, &crop_dialog, "crop-detected-content.png");
    crop_dialog.close();
    settle();
    crop_window.0.window.close();
    settle();
    // AdwDialogHost belongs to its parent window. Start a fresh guide window
    // after the independent crop confirmation so subsequent dialogs always
    // receive an allocation on the capture surface.
    let window = ViewerWindow::new(
        &application,
        Some(gio::File::for_path(fixture.path().join("red-wyvern.png"))),
    );
    window.0.window.set_default_size(1280, 800);
    window.present();
    settle();

    window.show_canvas_resize();
    settle();
    fn descendants(widget: &gtk::Widget, result: &mut Vec<gtk::Widget>) {
        result.push(widget.clone());
        let mut child = widget.first_child();
        while let Some(current) = child {
            descendants(&current, result);
            child = current.next_sibling();
        }
    }
    let dialog = window
        .0
        .window
        .visible_dialog()
        .expect("canvas resize dialog");
    let mut widgets = Vec::new();
    descendants(dialog.upcast_ref(), &mut widgets);
    let fields: Vec<_> = widgets
        .iter()
        .filter_map(|widget| widget.clone().downcast::<gtk::SpinButton>().ok())
        .collect();
    assert_eq!(fields.len(), 2, "canvas resize dimensions");
    fields[0].set_value(1280.0);
    fields[1].set_value(1024.0);
    settle();
    let resize = widgets
        .iter()
        .filter_map(|widget| widget.clone().downcast::<gtk::Button>().ok())
        .find(|button| button.label().as_deref() == Some("Resize"))
        .expect("canvas resize apply button");
    assert!(resize.is_sensitive(), "canvas resize growth is valid");
    capture_dialog(&window, &dialog, "canvas-resize.png");
    dialog.close();
    settle();

    window.show_preferences();
    settle();
    let preferences_dialog = window
        .0
        .window
        .visible_dialog()
        .expect("preferences dialog");
    capture_dialog(&window, &preferences_dialog, "preferences.png");
    preferences_dialog.close();
    settle();

    let annotations = [
        Annotation {
            id: AnnotationId(10_001),
            shape: Shape::Highlight {
                rect: Rect {
                    x: 805.0,
                    y: 575.0,
                    width: 190.0,
                    height: 180.0,
                },
                angle: -0.25,
                seed: 10_001,
                style: StrokeStyle {
                    // Cyan stays legible against the wyvern's red face.
                    color: [0, 212, 255, 255],
                    width: 6.0,
                },
            },
        },
        Annotation {
            id: AnnotationId(10_002),
            shape: Shape::Highlight {
                rect: Rect {
                    x: 92.0,
                    y: 75.0,
                    width: 360.0,
                    height: 315.0,
                },
                angle: 0.1,
                seed: 10_002,
                style: StrokeStyle {
                    // Yellow contrasts with both the dark membrane and red wing.
                    color: [255, 225, 70, 255],
                    width: 6.0,
                },
            },
        },
        Annotation {
            id: AnnotationId(10_003),
            shape: Shape::Arrow {
                start: Point { x: 905.0, y: 365.0 },
                control: Point { x: 875.0, y: 405.0 },
                end: Point { x: 885.0, y: 635.0 },
                style: StrokeStyle {
                    color: [0, 212, 255, 255],
                    width: 9.0,
                },
            },
        },
        Annotation {
            id: AnnotationId(10_004),
            shape: Shape::Arrow {
                start: Point { x: 530.0, y: 98.0 },
                control: Point { x: 425.0, y: 70.0 },
                end: Point { x: 326.0, y: 190.0 },
                style: StrokeStyle {
                    color: [255, 225, 70, 255],
                    width: 9.0,
                },
            },
        },
        Annotation {
            id: AnnotationId(10_005),
            shape: Shape::Text {
                anchor: Point { x: 70.0, y: 95.0 },
                angle: -0.10,
                font_size: 42.0,
                bend: -60.0,
                text: "Wing membrane".to_owned(),
                color: [255, 225, 70, 255],
            },
        },
    ];
    for annotation in annotations {
        window.apply(Operation::Annotate(AnnotationEdit::Create(annotation)));
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !window.rendered_is_current() && std::time::Instant::now() < deadline {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(window.rendered_is_current(), "annotation render completed");
    window.0.pencil_color.set([0, 212, 255, 255]);
    window
        .0
        .color_button
        .set_rgba(&u8_to_rgba([0, 212, 255, 255]));
    window.set_tool(Tool::Highlight);
    settle();
    capture(&window, "annotate-character.png");

    let document = window
        .0
        .document
        .borrow()
        .clone()
        .expect("editable document");
    let snapshot = ExportSnapshot {
        operations: document.operations().into(),
        document,
        source_file: window.0.current_file.borrow().clone(),
        load_generation: window.0.load_generation.get(),
    };
    window.show_export_options(snapshot, std::path::PathBuf::from("wyvern-guide.jpg"));
    settle();
    let export_dialog = window
        .0
        .window
        .visible_dialog()
        .expect("export options dialog");
    capture_dialog(&window, &export_dialog, "export-options.png");
    export_dialog.close();
    settle();

    // Remove Background turns the selected fragment into a movable image
    // annotation over a repaired background. Use a clean source window so the
    // inference sees artwork, not the guide's annotation examples.
    let removal_window = ViewerWindow::new(
        &application,
        Some(gio::File::for_path(fixture.path().join("red-wyvern.png"))),
    );
    removal_window.0.window.set_default_size(1280, 800);
    removal_window.present();
    settle();
    let (image_width, image_height) = removal_window
        .0
        .rendered
        .borrow()
        .as_ref()
        .expect("background-removal source loaded")
        .dimensions();
    removal_window.set_tool(Tool::Select);
    let crop = CropOverlay {
        x: 20,
        y: 40,
        width: image_width - 40,
        height: image_height - 84,
        image_width,
        image_height,
    };
    removal_window.set_region_selection(Some(crop));
    removal_window.start_selection_preparation(crop);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    while removal_window.0.prepared_selection.borrow().is_none()
        && std::time::Instant::now() < deadline
    {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        removal_window.0.prepared_selection.borrow().is_some(),
        "background removal completed"
    );
    removal_window.activate_prepared_selection();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !removal_window.rendered_is_current() && std::time::Instant::now() < deadline {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let id = removal_window
        .0
        .selected_annotation
        .get()
        .expect("cutout is selected");
    let annotation = removal_window
        .0
        .document
        .borrow()
        .as_ref()
        .expect("editable document")
        .annotations()
        .into_iter()
        .find(|annotation| annotation.id == id)
        .expect("selected cutout annotation");
    removal_window.apply(Operation::Annotate(AnnotationEdit::Set(
        crate::tools::annotation::edit::moved(&annotation, Point { x: -120.0, y: 40.0 }, true),
    )));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !removal_window.rendered_is_current() && std::time::Instant::now() < deadline {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        removal_window.rendered_is_current(),
        "moved cutout render completed"
    );
    capture(&removal_window, "background-removal-cutout.png");
    removal_window.0.window.close();
    window.0.window.close();
}

#[test]
#[ignore = "requires DIORAMA_MESH_SCREENSHOT and a graphical display"]
fn capture_mesh_workflow() {
    let Ok(output) = std::env::var("DIORAMA_MESH_SCREENSHOT") else {
        return;
    };
    adw::init().expect("GTK initialization");
    let application = adw::Application::builder()
        .application_id("io.github.mendrik_private.Diorama.MeshCapture")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    application.register(gio::Cancellable::NONE).unwrap();
    let window = ViewerWindow::new(&application, None);
    let image = image::RgbaImage::from_fn(480, 320, |x, y| {
        image::Rgba([
            ((x / 16) % 2 * 80 + 80) as u8,
            ((y / 16) % 2 * 80 + 80) as u8,
            170,
            255,
        ])
    });
    window
        .0
        .document
        .replace(Some(Document::new(crate::document::ImageSource {
            pixels: Arc::new(image.clone()),
            path: None,
            metadata: crate::document::Metadata::default(),
        })));
    window.0.rendered.replace(Some(image.clone()));
    window
        .0
        .rendered_generation
        .set(window.0.render_generation.get());
    window
        .0
        .canvas
        .set_texture(Some(&texture_from_rgba(&image).unwrap()));
    window.0.content_stack.set_visible_child_name("viewer");
    window.0.window.set_default_size(900, 640);
    window.present();
    let settle = || {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < end {
            glib::MainContext::default().iteration(false);
        }
    };
    settle();
    let point = |x, y| {
        window
            .0
            .canvas
            .widget_point_for_image(Point { x, y })
            .unwrap()
    };
    window.set_tool(Tool::MeshPoints);
    assert!(window.0.mesh_controls.is_visible());
    window.0.square_grid_button.set_active(true);
    assert!(window.0.square_grid_button.is_active());
    assert!(!window.0.dimetric_grid_button.is_active());
    assert!(!window.0.isometric_grid_button.is_active());
    assert_eq!(
        window
            .0
            .mesh
            .borrow()
            .as_ref()
            .and_then(|session| session.grid),
        Some(crate::canvas::MeshGrid::Square)
    );
    window.0.dimetric_grid_button.set_active(true);
    assert!(window.0.dimetric_grid_button.is_active());
    assert!(!window.0.square_grid_button.is_active());
    assert!(!window.0.isometric_grid_button.is_active());
    assert_eq!(
        window
            .0
            .mesh
            .borrow()
            .as_ref()
            .and_then(|session| session.grid),
        Some(crate::canvas::MeshGrid::Dimetric)
    );
    window.0.isometric_grid_button.set_active(true);
    assert!(window.0.isometric_grid_button.is_active());
    assert!(!window.0.square_grid_button.is_active());
    assert!(!window.0.dimetric_grid_button.is_active());
    assert_eq!(
        window
            .0
            .mesh
            .borrow()
            .as_ref()
            .and_then(|session| session.grid),
        Some(crate::canvas::MeshGrid::Isometric)
    );
    window.0.isometric_grid_button.set_active(false);
    assert!(!window.0.square_grid_button.is_active());
    assert!(!window.0.dimetric_grid_button.is_active());
    assert!(!window.0.isometric_grid_button.is_active());
    assert_eq!(
        window
            .0
            .mesh
            .borrow()
            .as_ref()
            .and_then(|session| session.grid),
        None
    );
    window.0.isometric_grid_button.set_active(true);
    assert!(window.0.isometric_grid_button.is_active());
    assert_eq!(
        window
            .0
            .mesh
            .borrow()
            .as_ref()
            .and_then(|session| session.grid),
        Some(crate::canvas::MeshGrid::Isometric)
    );
    let node = point(240.0, 160.0);
    window.mesh_click(f64::from(node.x()), f64::from(node.y()));
    assert_eq!(window.mesh_point_count(), 1);
    assert!(window.0.warp_mesh_button.is_sensitive());
    window.begin_mesh_warp();
    assert!(window.mesh_is_warped());
    assert!(!window.0.warp_mesh_button.is_sensitive());

    let moved = point(270.0, 180.0);
    assert!(window.mesh_begin_drag(f64::from(node.x()), f64::from(node.y())));
    window.mesh_drag_to(f64::from(moved.x()), f64::from(moved.y()));
    window.mesh_end_drag();
    let expected = crate::tools::mesh::warp(
        &image,
        &[Point { x: 240.0, y: 160.0 }],
        &[Point { x: 270.0, y: 180.0 }],
        &CancellationToken::default(),
    )
    .unwrap();

    settle();
    let widget = &window.0.window;
    let snapshot = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(widget)).snapshot(
        &snapshot,
        widget.width() as f64,
        widget.height() as f64,
    );
    let texture = widget.renderer().unwrap().render_texture(
        snapshot.to_node().unwrap(),
        Some(&gtk::graphene::Rect::new(
            0.0,
            0.0,
            widget.width() as f32,
            widget.height() as f32,
        )),
    );
    texture.save_to_png(std::path::Path::new(&output)).unwrap();

    // Clear cancels an in-flight preview, invalidates its callback, and is local undo history.
    window.0.clear_mesh_button.emit_clicked();
    assert_eq!(window.mesh_point_count(), 0);
    assert!(!window.mesh_is_warped());
    settle();
    assert_eq!(window.0.rendered.borrow().as_ref(), Some(&image));
    assert_eq!(
        rgba_from_texture(&window.0.canvas.texture().unwrap()),
        Some(image.clone())
    );
    gio::prelude::ActionGroupExt::activate_action(&window.0.window, "undo", None);
    assert_eq!(window.mesh_point_count(), 1);
    assert!(window.mesh_is_warped());
    gio::prelude::ActionGroupExt::activate_action(&window.0.window, "redo", None);
    assert_eq!(window.mesh_point_count(), 0);
    gio::prelude::ActionGroupExt::activate_action(&window.0.window, "undo", None);

    // The visible Apply button queues its latest target while its preview is in flight.
    window.0.apply_mesh_button.emit_clicked();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    while (window.0.mesh.borrow().is_some() || !window.rendered_is_current())
        && std::time::Instant::now() < deadline
    {
        glib::MainContext::default().iteration(false);
    }
    assert!(matches!(
        window.0.document.borrow().as_ref().unwrap().operations(),
        [Operation::SelectionEdit { .. }]
    ));
    assert_eq!(window.0.rendered.borrow().as_ref(), Some(&expected));

    // Global history remains available after a committed mesh edit.
    gio::prelude::ActionGroupExt::activate_action(&window.0.window, "undo", None);
    settle();
    assert_eq!(window.0.rendered.borrow().as_ref(), Some(&image));
    gio::prelude::ActionGroupExt::activate_action(&window.0.window, "redo", None);
    settle();
    assert_eq!(window.0.rendered.borrow().as_ref(), Some(&expected));
    gio::prelude::ActionGroupExt::activate_action(&window.0.window, "undo", None);
    settle();

    // Enter reaches the same confirm path from the canvas key controller.
    window.set_tool(Tool::MeshPoints);
    let node = point(240.0, 160.0);
    window.mesh_click(f64::from(node.x()), f64::from(node.y()));
    window.begin_mesh_warp();
    assert!(window.mesh_begin_drag(f64::from(node.x()), f64::from(node.y())));
    window.mesh_drag_to(f64::from(moved.x()), f64::from(moved.y()));
    window.mesh_end_drag();
    let handled = (0..window.0.canvas.observe_controllers().n_items())
        .filter_map(|index| window.0.canvas.observe_controllers().item(index))
        .filter_map(|controller| controller.downcast::<gtk::EventControllerKey>().ok())
        .any(|controller| {
            controller.emit_by_name::<bool>(
                "key-pressed",
                &[
                    &gtk::gdk::Key::Return,
                    &0_u32,
                    &gtk::gdk::ModifierType::empty(),
                ],
            )
        });
    assert!(handled, "Enter reaches the mesh key controller");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    while (window.0.mesh.borrow().is_some() || !window.rendered_is_current())
        && std::time::Instant::now() < deadline
    {
        glib::MainContext::default().iteration(false);
    }
    assert_eq!(window.0.rendered.borrow().as_ref(), Some(&expected));

    // Escape cancels an in-flight preview and its later callback cannot replace the canvas.
    gio::prelude::ActionGroupExt::activate_action(&window.0.window, "undo", None);
    settle();
    window.set_tool(Tool::MeshPoints);
    let node = point(240.0, 160.0);
    window.mesh_click(f64::from(node.x()), f64::from(node.y()));
    window.begin_mesh_warp();
    assert!(window.mesh_begin_drag(f64::from(node.x()), f64::from(node.y())));
    window.mesh_drag_to(f64::from(moved.x()), f64::from(moved.y()));
    gio::prelude::ActionGroupExt::activate_action(&window.0.window, "cancel-tool", None);
    settle();
    assert!(window.0.mesh.borrow().is_none());
    assert_eq!(window.0.rendered.borrow().as_ref(), Some(&image));
    assert_eq!(
        rgba_from_texture(&window.0.canvas.texture().unwrap()),
        Some(image.clone())
    );
    window.0.window.close();
}
