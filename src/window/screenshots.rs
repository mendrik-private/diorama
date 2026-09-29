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
    // The guide deliberately captures a real application window: its widgets,
    // canvas overlays, dialogs, and annotations all take the normal rendering
    // paths. The fixture source is read-only and lives outside the repository.
    capture("overview.png");

    let lens_texture = window.0.canvas.texture().expect("loaded canvas texture");
    window.set_single_image_lens_active(true);
    window
        .0
        .canvas
        .set_lens(&lens_texture, 0.84, 0.63, 160.0, 4.0, true);
    settle();
    capture("inspect-pixel-lens.png");
    window.0.canvas.clear_lens();

    window.load_comparison(gio::File::for_path(compare_copy));
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
    capture("compare-lenses.png");
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
    capture("edit-selection.png");
    window.set_region_selection(None);

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
    capture("annotate-character.png");

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
    capture("export-options.png");
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
