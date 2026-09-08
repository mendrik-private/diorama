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
