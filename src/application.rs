use gio::prelude::*;
use gtk::prelude::*;
use libadwaita as adw;

use crate::{APP_ID, window};

const SHORTCUTS: &[(&str, &[&str])] = &[
    ("win.open", &["<Control>o"]),
    ("win.copy-image", &["<Control>c"]),
    ("win.save", &["<Control>s"]),
    ("win.save-as", &["<Control><Shift>s"]),
    ("win.close", &["<Control>w"]),
    ("win.preferences", &["<Control>comma"]),
    ("win.shortcuts", &["<Control>question"]),
    ("win.undo", &["<Control>z"]),
    ("win.redo", &["<Control><Shift>z"]),
    ("win.zoom-in", &["plus", "<Control>plus"]),
    ("win.zoom-out", &["minus", "<Control>minus"]),
    ("win.fit", &["0"]),
    ("win.zoom-100", &["1"]),
    ("win.zoom-200", &["2"]),
    ("win.zoom-300", &["3"]),
    ("win.zoom-400", &["4"]),
    ("win.zoom-500", &["5"]),
    ("win.zoom-600", &["6"]),
    ("win.zoom-700", &["7"]),
    ("win.zoom-800", &["8"]),
    ("win.zoom-900", &["9"]),
    ("win.toggle-filter", &["x"]),
    ("win.previous", &["Left", "Page_Up"]),
    ("win.next", &["Right", "Page_Down"]),
    ("win.delete-file", &["Delete"]),
    ("win.rotate-clockwise", &["r"]),
    ("win.rotate-counterclockwise", &["<Shift>r"]),
    ("win.flip-horizontal", &["h"]),
    ("win.flip-vertical", &["v"]),
    ("win.crop", &["c"]),
    ("win.measure", &["m"]),
    ("win.scale-preview", &["s"]),
    ("win.confirm-crop", &["Return", "KP_Enter"]),
    ("win.compare", &["d"]),
    ("win.pencil", &["p"]),
    ("win.select-object", &["a"]),
    ("win.fullscreen", &["F11"]),
    ("win.cancel-tool", &["Escape"]),
];

pub fn build() -> adw::Application {
    let application = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    application.connect_startup(|application| {
        application.set_resource_base_path(Some("/io/github/mendrik/Diorama"));
        install_accelerators(application);
    });
    application.connect_activate(|application| {
        if let Some(window) = application.active_window() {
            window.present();
        } else {
            window::ViewerWindow::new(application, None).present();
        }
    });
    application.connect_open(|application, files, _hint| {
        window::ViewerWindow::new_with_files(application, files).present();
    });
    application
}

fn install_accelerators(application: &adw::Application) {
    for (action, accelerators) in SHORTCUTS {
        application.set_accels_for_action(action, accelerators);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_c_is_reserved_for_copying_the_full_image() {
        assert!(SHORTCUTS.contains(&("win.copy-image", &["<Control>c"])));
    }

    #[test]
    fn edit_tools_have_single_key_accelerators() {
        assert!(SHORTCUTS.contains(&("win.measure", &["m"])));
        assert!(SHORTCUTS.contains(&("win.scale-preview", &["s"])));
    }
}
