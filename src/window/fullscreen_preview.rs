use super::*;

pub(super) struct FullscreenPreview {
    content: gtk::Widget,
    focus: Option<gtk::Widget>,
    was_fullscreen: bool,
    pub(super) window_size: (i32, i32),
    actions: Vec<(String, bool)>,
}

impl ViewerWindow {
    pub(super) fn show_fullscreen_preview(&self) {
        if self.0.fullscreen_preview.borrow().is_some() {
            return;
        }
        let texture = if let Some(document) = self.0.document.borrow().as_ref() {
            match document.render(&CancellationToken::default()) {
                Ok(rendered) => texture_from_rgba(&rendered.pixels).ok(),
                Err(error) => {
                    self.0.toasts.add_toast(adw::Toast::new(&error.to_string()));
                    return;
                }
            }
        } else {
            self.0.canvas.texture()
        };
        let Some(texture) = texture else {
            return;
        };
        let Some(content) = self.0.window.content() else {
            return;
        };
        self.cancel_annotation_drag();
        if self.0.pencil_drag.borrow().is_some() {
            self.abort_pencil_drag();
        }
        let picture = gtk::Picture::for_paintable(&texture);
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_can_shrink(true);
        picture.set_focusable(true);
        picture.set_cursor_from_name(Some("none"));
        picture.set_alternative_text(Some(&gettext(
            "Fullscreen image preview. Press Escape to return.",
        )));
        let actions = self
            .0
            .window
            .list_actions()
            .iter()
            .filter_map(|name| {
                self.0
                    .window
                    .lookup_action(name)
                    .map(|action| (name.to_string(), action.is_enabled()))
            })
            .collect();
        self.0.fullscreen_preview.replace(Some(FullscreenPreview {
            content,
            focus: gtk::prelude::GtkWindowExt::focus(&self.0.window),
            was_fullscreen: self.0.window.is_fullscreen(),
            window_size: (self.0.window.width(), self.0.window.height()),
            actions,
        }));
        for name in self.0.window.list_actions() {
            if !matches!(name.as_str(), "cancel-tool" | "close") {
                self.set_action_enabled(&name, false);
            }
        }
        self.0.window.set_content(Some(&picture));
        self.0.window.fullscreen();
        picture.grab_focus();
    }

    pub(super) fn leave_fullscreen_preview(&self) -> bool {
        let Some(preview) = self.0.fullscreen_preview.take() else {
            return false;
        };
        self.0.window.set_content(Some(&preview.content));
        if !preview.was_fullscreen {
            self.0.window.unfullscreen();
        }
        for (name, enabled) in preview.actions {
            self.set_action_enabled(&name, enabled);
        }
        self.update_action_states();
        if let Some(focus) = preview.focus {
            focus.grab_focus();
        }
        true
    }

    pub(super) fn fullscreen_preview_key(
        &self,
        key: gtk::gdk::Key,
        modifiers: gtk::gdk::ModifierType,
    ) -> glib::Propagation {
        if self.0.fullscreen_preview.borrow().is_some() {
            if key == gtk::gdk::Key::Escape {
                self.leave_fullscreen_preview();
            }
            return glib::Propagation::Stop;
        }
        if key != gtk::gdk::Key::space
            || modifiers.intersects(
                gtk::gdk::ModifierType::CONTROL_MASK
                    | gtk::gdk::ModifierType::ALT_MASK
                    | gtk::gdk::ModifierType::SUPER_MASK
                    | gtk::gdk::ModifierType::HYPER_MASK
                    | gtk::gdk::ModifierType::META_MASK
                    | gtk::gdk::ModifierType::SHIFT_MASK,
            )
            || self.0.window.visible_dialog().is_some()
            || gtk::prelude::GtkWindowExt::focus(&self.0.window).is_some_and(|focus| {
                focus.is::<gtk::Text>()
                    || focus.is::<gtk::Entry>()
                    || focus.is::<gtk::TextView>()
                    || focus.is::<gtk::SpinButton>()
            })
        {
            return glib::Propagation::Proceed;
        }
        self.show_fullscreen_preview();
        if self.0.fullscreen_preview.borrow().is_some() {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a graphical display"]
    fn fullscreen_preview_fits_image_blocks_tools_and_restores_comparison() {
        fn wait_until(condition: impl Fn() -> bool) {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !condition() && std::time::Instant::now() < deadline {
                glib::MainContext::default().iteration(false);
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(condition(), "window state transition timed out");
        }
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik_private.Diorama.FullscreenPreviewTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application.register(gio::Cancellable::NONE).unwrap();
        let window = ViewerWindow::new(&application, None);
        let press = |key| {
            let controllers = window.0.window.observe_controllers();
            (0..controllers.n_items())
                .filter_map(|index| controllers.item(index))
                .filter_map(|controller| controller.downcast::<gtk::EventControllerKey>().ok())
                .filter(|controller| {
                    controller.propagation_phase() == gtk::PropagationPhase::Capture
                })
                .any(|controller| {
                    controller.emit_by_name::<bool>(
                        "key-pressed",
                        &[&key, &0_u32, &gtk::gdk::ModifierType::empty()],
                    )
                })
        };
        assert!(!press(gtk::gdk::Key::space));
        assert!(window.0.fullscreen_preview.borrow().is_none());
        let image = image::RgbaImage::from_pixel(400, 200, image::Rgba([60, 100, 140, 255]));
        let texture = texture_from_rgba(&image).unwrap();
        window.0.canvas.set_texture(Some(&texture));
        window.0.rendered.replace(Some(image.clone()));
        window
            .0
            .document
            .replace(Some(Document::new(crate::document::ImageSource {
                pixels: Arc::new(image),
                path: None,
                metadata: crate::document::Metadata::default(),
            })));
        window.0.content_stack.set_visible_child_name("viewer");
        window.0.window.present();
        window.set_tool(Tool::Arrow);
        window.0.lens_button.set_active(true);
        window.enter_compare(
            gio::File::for_path("/images/comparison.png"),
            crate::image::LoadedPreview {
                texture,
                width: 400,
                height: 200,
                metadata: crate::document::Metadata::default(),
                animation_delay: None,
            },
        );
        window.0.canvas_overlay.allocate(1000, 600, -1, None);
        window.layout_compare_panels();
        window.layout_compare_panels();
        let original_content = window.0.window.content().unwrap();
        let original_comparison = window.0.compare_canvas.borrow().clone();
        let original_zoom = window.0.canvas.zoom();
        window.0.canvas.grab_focus();
        for _ in 0..2 {
            let operations = window
                .0
                .document
                .borrow()
                .as_ref()
                .unwrap()
                .operations()
                .len();
            assert!(press(gtk::gdk::Key::space));
            assert!(window.0.fullscreen_preview.borrow().is_some());
            wait_until(|| window.0.window.is_fullscreen());
            let picture = window
                .0
                .window
                .content()
                .unwrap()
                .downcast::<gtk::Picture>()
                .unwrap();
            assert_eq!(picture.content_fit(), gtk::ContentFit::Contain);
            assert!(picture.can_shrink());
            let paintable = picture.paintable().unwrap();
            assert_eq!(
                (paintable.intrinsic_width(), paintable.intrinsic_height()),
                (400, 200)
            );
            assert!(!window.0.canvas.is_ancestor(&window.0.window));
            assert!(!window.0.pencil_controls.is_mapped());
            window.update_action_states();
            for action in ["pencil", "arrow", "tool", "lens", "undo", "zoom-in", "next"] {
                assert!(
                    !window.0.window.lookup_action(action).unwrap().is_enabled(),
                    "{action} must be inactive"
                );
            }
            for key in [
                gtk::gdk::Key::p,
                gtk::gdk::Key::l,
                gtk::gdk::Key::Return,
                gtk::gdk::Key::Right,
                gtk::gdk::Key::space,
            ] {
                assert!(press(key));
            }
            assert_eq!(
                window
                    .0
                    .document
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .operations()
                    .len(),
                operations
            );
            assert!(press(gtk::gdk::Key::Escape));
            assert!(window.0.fullscreen_preview.borrow().is_none());
            wait_until(|| !window.0.window.is_fullscreen());
            assert_eq!(window.0.window.content(), Some(original_content.clone()));
            assert_eq!(*window.0.compare_canvas.borrow(), original_comparison);
            assert_eq!(window.0.canvas.zoom(), original_zoom);
            assert_eq!(window.0.tool.get(), Tool::Arrow);
            assert!(window.0.lens_active.get());
            assert!(window.0.pencil_controls.is_visible());
            assert!(window.0.window.lookup_action("arrow").unwrap().is_enabled());
        }
        window.exit_compare();
        window.0.window.fullscreen();
        wait_until(|| window.0.window.is_fullscreen());
        assert!(press(gtk::gdk::Key::space));
        assert!(press(gtk::gdk::Key::Escape));
        assert!(
            window.0.window.is_fullscreen(),
            "existing F11 fullscreen must be preserved"
        );
        window.0.window.unfullscreen();
        wait_until(|| !window.0.window.is_fullscreen());
        // Space remains text input while an annotation editor owns focus.
        window.open_text_editor(
            None,
            crate::document::AnnotationId(99),
            Point { x: 30.0, y: 30.0 },
            0.0,
            String::new(),
        );
        assert!(!press(gtk::gdk::Key::space));
        assert!(window.0.fullscreen_preview.borrow().is_none());
        window.close_text_editor();
        window.0.window.close();
    }
}
