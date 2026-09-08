use super::*;
use gtk::cairo;
use gtk::gdk_pixbuf::{Colorspace, Pixbuf};

impl ViewerWindow {
    pub(super) fn print(&self) {
        let Some(texture) = self.0.canvas.texture() else {
            return;
        };
        // Snapshot the document, since the canvas can still be rendering an edit.
        // Animations and view-only images use their currently displayed frame.
        let document = self
            .0
            .animation_frames
            .borrow()
            .is_empty()
            .then(|| self.0.document.borrow().clone())
            .flatten();
        let frame = document.is_none().then(|| rgba_from_texture(&texture));
        let job_name = self.0.title.title();
        let generation = self.0.load_generation.get();
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let image = if let Some(document) = document {
                gio::spawn_blocking(move || document.render(&CancellationToken::default()))
                    .await
                    .map_err(|_| "Print rendering worker panicked".to_owned())
                    .and_then(|result| result.map(|image| image.pixels).map_err(|e| e.to_string()))
            } else {
                frame
                    .flatten()
                    .ok_or_else(|| "Could not read image pixels".to_owned())
            };
            let Some(state) = weak.upgrade() else {
                return;
            };
            if state.load_generation.get() != generation {
                return;
            }
            let result = image.and_then(|image| {
                let (operation, drawing_error) = print_operation(image, &job_name)?;
                let result =
                    operation.run(gtk::PrintOperationAction::PrintDialog, Some(&state.window));
                if let Some(error) = drawing_error.take() {
                    return Err(error.to_string());
                }
                result.map(|_| ()).map_err(|error| error.to_string())
            });
            if let Err(error) = result {
                tracing::warn!(%error, "Could not print image");
                state
                    .toasts
                    .add_toast(adw::Toast::new(&gettext("Could not print image")));
            }
        });
    }
}

type DrawingError = Rc<RefCell<Option<cairo::Error>>>;

fn print_operation(
    image: image::RgbaImage,
    job_name: &str,
) -> Result<(gtk::PrintOperation, DrawingError), String> {
    let pixbuf = print_pixbuf(image)?;
    let operation = gtk::PrintOperation::builder()
        .job_name(job_name)
        .n_pages(1)
        .unit(gtk::Unit::Points)
        .use_full_page(false)
        .embed_page_setup(true)
        .build();
    let drawing_error = DrawingError::default();
    operation.connect_draw_page({
        let drawing_error = drawing_error.clone();
        move |operation, context, _| {
            if let Err(error) = draw_image(
                &context.cairo_context(),
                &pixbuf,
                context.width(),
                context.height(),
            ) {
                drawing_error.replace(Some(error));
                operation.cancel();
            }
        }
    });
    Ok((operation, drawing_error))
}

fn print_pixbuf(image: image::RgbaImage) -> Result<Pixbuf, String> {
    let width = i32::try_from(image.width()).map_err(|e| e.to_string())?;
    let height = i32::try_from(image.height()).map_err(|e| e.to_string())?;
    let stride = width.checked_mul(4).ok_or("Image stride is too large")?;
    if width == 0 || height == 0 {
        return Err("Image is empty".to_owned());
    }
    Ok(Pixbuf::from_bytes(
        &glib::Bytes::from_owned(image.into_raw()),
        Colorspace::Rgb,
        true,
        8,
        width,
        height,
        stride,
    ))
}

fn draw_image(
    context: &cairo::Context,
    image: &Pixbuf,
    page_width: f64,
    page_height: f64,
) -> Result<(), cairo::Error> {
    let width = f64::from(image.width());
    let height = f64::from(image.height());
    let scale = (page_width / width).min(page_height / height);
    context.save()?;
    // Paper is white regardless of the viewer's background or transparency grid.
    context.set_source_rgb(1.0, 1.0, 1.0);
    context.paint()?;
    context.translate(
        (page_width - width * scale) / 2.0,
        (page_height - height * scale) / 2.0,
    );
    context.scale(scale, scale);
    context.set_source_pixbuf(image, 0.0, 0.0);
    context.source().set_extend(cairo::Extend::Pad);
    context.rectangle(0.0, 0.0, width, height);
    context.fill()?;
    context.restore()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printing_fits_and_centers_both_orientations_on_white_paper() {
        for (width, height, margin, inside) in
            [(40, 20, (50, 10), (10, 50)), (20, 40, (10, 50), (50, 10))]
        {
            let image = print_pixbuf(image::RgbaImage::from_pixel(
                width,
                height,
                image::Rgba([255, 0, 0, 128]),
            ))
            .unwrap();
            let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 100, 100).unwrap();
            let context = cairo::Context::new(&surface).unwrap();
            draw_image(&context, &image, 100.0, 100.0).unwrap();
            drop(context);
            let stride = surface.stride() as usize;
            let data = surface.data().unwrap();
            let pixel = |(x, y): (usize, usize)| {
                let offset = y * stride + x * 4;
                u32::from_ne_bytes(data[offset..offset + 4].try_into().unwrap())
            };
            assert_eq!(pixel(margin), 0xffffffff);
            assert_eq!(pixel(inside), 0xffff7f7f);
            assert_eq!(pixel((50, 50)), 0xffff7f7f);
        }
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn native_print_operation_exports_one_page() {
        gtk::init().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("print.pdf");
        let image = image::RgbaImage::from_pixel(40, 20, image::Rgba([255, 0, 0, 255]));
        let (operation, drawing_error) = print_operation(image, "Print test").unwrap();
        operation.set_export_filename(&path);
        assert_eq!(
            operation
                .run(gtk::PrintOperationAction::Export, gtk::Window::NONE)
                .unwrap(),
            gtk::PrintOperationResult::Apply,
        );
        assert!(drawing_error.borrow().is_none());
        assert!(std::fs::read(path).unwrap().starts_with(b"%PDF-"));
        assert_eq!(operation.n_pages_to_print(), 1);
    }
}
