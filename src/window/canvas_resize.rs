use super::*;
use crate::tools::canvas_resize::background;

impl ViewerWindow {
    pub(super) fn square_up(&self) {
        self.set_tool(Tool::None);
        let Some(image) = self.0.rendered.borrow().clone() else {
            return;
        };
        let size = image.width().max(image.height());
        if image.width() == image.height() {
            return;
        }
        if u64::from(size) * u64::from(size) > DecodeLimits::default().max_decoded_bytes / 4 {
            self.0.toasts.add_toast(adw::Toast::new(&gettext(
                "The square canvas would be too large",
            )));
            return;
        }
        self.apply(Operation::ResizeCanvas {
            width: size,
            height: size,
            background: background(&image),
        });
    }

    pub(super) fn show_canvas_resize(&self) {
        self.set_tool(Tool::None);
        let Some(image) = self.0.rendered.borrow().clone() else {
            return;
        };
        let fill = background(&image);
        let original = image.dimensions();
        let source = Arc::new(image::imageops::thumbnail(&image, 480, 320));
        let dialog = adw::Dialog::builder()
            .title(gettext("Canvas Resize"))
            .content_width(540)
            .build();
        let header = adw::HeaderBar::new();
        let cancel = gtk::Button::with_label(&gettext("Cancel"));
        let apply = gtk::Button::with_label(&gettext("Resize"));
        apply.add_css_class("suggested-action");
        header.pack_start(&cancel);
        header.pack_end(&apply);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();
        let preview = ImageCanvas::default();
        preview.set_background(Background::Checkerboard);
        preview.set_size_request(480, 320);
        content.append(&preview);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let width = spin(
            f64::from(original.0),
            f64::from(original.0.max(65535)),
            f64::from(original.0),
        );
        let height = spin(
            f64::from(original.1),
            f64::from(original.1.max(65535)),
            f64::from(original.1),
        );
        width.set_width_chars(7);
        height.set_width_chars(7);
        for (label, field) in [("_Width", &width), ("_Height", &height)] {
            let label = gtk::Label::with_mnemonic(&gettext(label));
            label.set_mnemonic_widget(Some(field));
            row.append(&label);
            row.append(field);
        }
        content.append(&row);
        let description = gtk::Label::builder().label(gettext("Pixels · Image stays centered at its original size. Added space uses the detected background."))
            .wrap(true).xalign(0.0).build();
        content.append(&description);
        let error = gtk::Label::builder()
            .label(gettext(
                "This canvas exceeds the image memory limit. Choose smaller dimensions.",
            ))
            .wrap(true)
            .visible(false)
            .build();
        error.add_css_class("error");
        content.append(&error);
        let update: Rc<dyn Fn()> = Rc::new({
            let width = width.clone();
            let height = height.clone();
            let apply = apply.clone();
            move || {
                let w = width.value() as u32;
                let h = height.value() as u32;
                let valid =
                    u64::from(w) * u64::from(h) * 4 <= DecodeLimits::default().max_decoded_bytes;
                apply.set_sensitive(valid && (w, h) != original);
                error.set_visible(!valid);
                let factor = (480.0 / f64::from(w)).min(320.0 / f64::from(h));
                let pw = (f64::from(w) * factor).round().max(1.0) as u32;
                let ph = (f64::from(h) * factor).round().max(1.0) as u32;
                let sw = (f64::from(original.0) * factor).round().max(1.0) as u32;
                let sh = (f64::from(original.1) * factor).round().max(1.0) as u32;
                let thumbnail = image::imageops::resize(
                    source.as_ref(),
                    sw,
                    sh,
                    image::imageops::FilterType::Nearest,
                );
                if let Ok(pixels) = crate::tools::canvas_resize::resize(
                    &thumbnail,
                    pw,
                    ph,
                    fill,
                    &CancellationToken::default(),
                ) && let Ok(texture) = texture_from_rgba(&pixels)
                {
                    preview.set_texture(Some(&texture));
                    preview.set_zoom(1.0);
                }
            }
        });
        for field in [&width, &height] {
            let update = update.clone();
            field.connect_value_changed(move |_| update());
        }
        update();
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        outer.append(&header);
        outer.append(&content);
        dialog.set_child(Some(&outer));
        cancel.connect_clicked({
            let dialog = dialog.clone();
            move |_| {
                dialog.close();
            }
        });
        apply.connect_clicked({
            let this = self.clone();
            let dialog = dialog.clone();
            move |_| {
                width.update();
                height.update();
                this.apply(Operation::ResizeCanvas {
                    width: width.value() as u32,
                    height: height.value() as u32,
                    background: fill,
                });
                dialog.close();
            }
        });
        // The dialog owns keyboard input, including digits and native editing shortcuts.
        let suppression = Rc::new(RefCell::new(
            self.0
                .window
                .application()
                .map(|app| crate::application::suppress_accelerators(&app)),
        ));
        dialog.connect_closed(move |_| {
            suppression.borrow_mut().take();
        });
        dialog.present(Some(&self.0.window));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a graphical display"]
    fn canvas_dialog_applies_centered_padding() {
        fn widgets(widget: &gtk::Widget, result: &mut Vec<gtk::Widget>) {
            result.push(widget.clone());
            let mut child = widget.first_child();
            while let Some(current) = child {
                widgets(&current, result);
                child = current.next_sibling();
            }
        }
        adw::init().unwrap();
        let application = adw::Application::builder()
            .application_id("io.github.mendrik_private.Diorama.CanvasResizeTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application.register(gio::Cancellable::NONE).unwrap();
        let window = ViewerWindow::new(&application, None);
        let pixels = image::RgbaImage::from_pixel(8, 6, image::Rgba([20, 40, 60, 255]));
        window
            .0
            .canvas
            .set_texture(Some(&texture_from_rgba(&pixels).unwrap()));
        window.0.rendered.replace(Some(pixels.clone()));
        window
            .0
            .document
            .replace(Some(Document::new(crate::document::ImageSource {
                pixels: Arc::new(pixels),
                path: None,
                metadata: Default::default(),
            })));
        window.0.content_stack.set_visible_child_name("viewer");
        window.update_action_states();
        window.present();
        window.show_canvas_resize();
        let dialog = window
            .0
            .window
            .visible_dialog()
            .expect("canvas resize dialog");
        let mut descendants = Vec::new();
        widgets(dialog.upcast_ref(), &mut descendants);
        let fields: Vec<_> = descendants
            .iter()
            .filter_map(|widget| widget.clone().downcast::<gtk::SpinButton>().ok())
            .collect();
        assert_eq!(fields.len(), 2);
        fields[0].set_value(12.0);
        fields[1].set_value(10.0);
        let apply = descendants
            .iter()
            .filter_map(|widget| widget.clone().downcast::<gtk::Button>().ok())
            .find(|button| button.label().as_deref() == Some("Resize"))
            .unwrap();
        assert!(apply.is_sensitive());
        apply.emit_clicked();
        let document = window.0.document.borrow();
        let document = document.as_ref().unwrap();
        assert!(matches!(
            document.operations().last(),
            Some(Operation::ResizeCanvas {
                width: 12,
                height: 10,
                background: [20, 40, 60, 255]
            })
        ));
        let rendered = document.render(&CancellationToken::default()).unwrap();
        assert_eq!(rendered.pixels.dimensions(), (12, 10));
    }
}
