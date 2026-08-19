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
    (
        "win.zoom-in",
        &[
            "plus",
            "equal",
            "KP_Add",
            "<Control>plus",
            "<Control>equal",
            "<Control>KP_Add",
        ],
    ),
    (
        "win.zoom-out",
        &[
            "minus",
            "KP_Subtract",
            "<Control>minus",
            "<Control>KP_Subtract",
        ],
    ),
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
    ("win.previous", &["<Alt>Left", "Page_Up"]),
    ("win.next", &["<Alt>Right", "Page_Down"]),
    ("win.delete-file", &["Delete"]),
    ("win.rotate-clockwise", &["r"]),
    ("win.rotate-counterclockwise", &["<Shift>r"]),
    ("win.flip-horizontal", &["h"]),
    ("win.flip-vertical", &["v"]),
    ("win.crop", &["c"]),
    ("win.measure", &["m"]),
    ("win.scale-preview", &["s"]),
    ("win.compare", &["d"]),
    ("win.pencil", &["p"]),
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
    fn control_c_uses_the_contextual_copy_action() {
        assert!(SHORTCUTS.contains(&("win.copy-image", &["<Control>c"])));
        assert!(
            !SHORTCUTS
                .iter()
                .any(|(action, _)| *action == "win.select-object")
        );
    }

    #[test]
    fn zoom_shortcuts_cover_main_and_keypad_keys() {
        let zoom_in = SHORTCUTS
            .iter()
            .find(|(action, _)| *action == "win.zoom-in")
            .map(|(_, shortcuts)| *shortcuts)
            .expect("zoom-in shortcuts");
        let zoom_out = SHORTCUTS
            .iter()
            .find(|(action, _)| *action == "win.zoom-out")
            .map(|(_, shortcuts)| *shortcuts)
            .expect("zoom-out shortcuts");

        assert!(zoom_in.contains(&"equal"));
        assert!(zoom_in.contains(&"KP_Add"));
        assert!(zoom_out.contains(&"minus"));
        assert!(zoom_out.contains(&"KP_Subtract"));
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn configured_accelerators_are_valid_gtk_syntax() {
        gtk::init().expect("GTK display initialization");
        for (action, accelerators) in SHORTCUTS {
            for accelerator in *accelerators {
                assert!(
                    gtk::accelerator_parse(*accelerator).is_some(),
                    "invalid accelerator {accelerator:?} for {action}"
                );
            }
        }
    }

    #[test]
    fn edit_tools_have_single_key_accelerators() {
        assert!(SHORTCUTS.contains(&("win.measure", &["m"])));
        assert!(SHORTCUTS.contains(&("win.scale-preview", &["s"])));
    }

    #[test]
    fn image_navigation_keeps_unmodified_arrows_out_of_global_accelerators() {
        assert!(SHORTCUTS.contains(&("win.previous", &["<Alt>Left", "Page_Up"])));
        assert!(SHORTCUTS.contains(&("win.next", &["<Alt>Right", "Page_Down"])));
        assert!(!SHORTCUTS.iter().any(|(_, accelerators)| {
            accelerators
                .iter()
                .any(|accelerator| matches!(*accelerator, "Left" | "Right"))
        }));
    }

    #[test]
    fn enter_is_scoped_to_the_focused_canvas() {
        assert!(!SHORTCUTS.iter().any(|(_, accelerators)| {
            accelerators
                .iter()
                .any(|accelerator| matches!(*accelerator, "Return" | "KP_Enter"))
        }));
    }
}
