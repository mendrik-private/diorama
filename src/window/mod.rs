use std::cell::{Cell, RefCell};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::{AdwDialogExt, AlertDialogExt, ComboRowExt, PreferencesGroupExt};
use gio::prelude::*;
use gtk::prelude::*;
use libadwaita as adw;

use crate::canvas::{Background, CropOverlay, ImageCanvas, MiniMap, ZoomFilter};
use crate::compare::{SplitOrientation, choose_split};
use crate::document::{
    BrushPoint, CancellationToken, Document, Operation, Resampling, Rotation, Stroke,
};
use crate::export::{ExportOptions, JpegOptions, PngOptions};
use crate::image::{
    AnimationFrame, DecodeLimits, decode_animation, decode_headless, decode_memory, load_preview,
};
use crate::navigation::DirectorySequence;
use crate::settings::Settings;

#[derive(Clone)]
pub struct ViewerWindow(Rc<WindowState>);

struct HeaderWidgets {
    header: adw::HeaderBar,
    animation_controls: gtk::Box,
    animation_play_button: gtk::Button,
    pencil_button: gtk::ToggleButton,
    lens_button: gtk::ToggleButton,
    color_button: gtk::ColorDialogButton,
    edit_button: gtk::ToggleButton,
}

#[derive(Clone, Copy)]
struct EditDrag {
    crop: CropOverlay,
    start_screen_x: f64,
    start_screen_y: f64,
    scale: bool,
    anchor_x: f64,
    anchor_y: f64,
    start_width: f64,
    start_height: f64,
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
}

fn edit_edge_hit(rect: gtk::graphene::Rect, x: f32, y: f32) -> (bool, bool, bool, bool) {
    const EDGE: f32 = 12.0;
    let within_vertical_span = y >= rect.y() - EDGE && y <= rect.y() + rect.height() + EDGE;
    let within_horizontal_span = x >= rect.x() - EDGE && x <= rect.x() + rect.width() + EDGE;
    let left = within_vertical_span && (x - rect.x()).abs() <= EDGE;
    let right = within_vertical_span && (x - (rect.x() + rect.width())).abs() <= EDGE;
    let top = within_horizontal_span && (y - rect.y()).abs() <= EDGE;
    let bottom = within_horizontal_span && (y - (rect.y() + rect.height())).abs() <= EDGE;
    (left, right, top, bottom)
}

fn edit_resize_cursor(rect: gtk::graphene::Rect, x: f32, y: f32) -> &'static str {
    let (left, right, top, bottom) = edit_edge_hit(rect, x, y);
    match (left, right, top, bottom) {
        (true, _, true, _) | (_, true, _, true) => "nwse-resize",
        (_, true, true, _) | (true, _, _, true) => "nesw-resize",
        (true, _, _, _) | (_, true, _, _) => "ew-resize",
        (_, _, true, _) | (_, _, _, true) => "ns-resize",
        _ => "default",
    }
}

fn corner_scale(drag: EditDrag, x: f64, y: f64) -> f64 {
    let horizontal = (x - drag.anchor_x).abs() / drag.start_width.max(1.0);
    let vertical = (y - drag.anchor_y).abs() / drag.start_height.max(1.0);
    if (horizontal - 1.0).abs() >= (vertical - 1.0).abs() {
        horizontal
    } else {
        vertical
    }
    .clamp(0.05, 64.0)
}

struct WindowState {
    window: adw::ApplicationWindow,
    canvas: ImageCanvas,
    scrolled: gtk::ScrolledWindow,
    title: adw::WindowTitle,
    toasts: adw::ToastOverlay,
    settings: Settings,
    current_file: RefCell<Option<gio::File>>,
    sequence: RefCell<Option<DirectorySequence>>,
    cancellable: RefCell<Option<gio::Cancellable>>,
    render_cancellation: RefCell<Option<CancellationToken>>,
    load_generation: Cell<u64>,
    render_generation: Cell<u64>,
    document: RefCell<Option<Document>>,
    rendered: RefCell<Option<image::RgbaImage>>,
    source_modified: RefCell<Option<std::time::SystemTime>>,
    close_approved: Cell<bool>,
    pencil_active: Cell<bool>,
    pencil_points: RefCell<Vec<BrushPoint>>,
    pencil_start: Cell<(f64, f64)>,
    pencil_color: Cell<[u8; 4]>,
    pencil_button: gtk::ToggleButton,
    lens_button: gtk::ToggleButton,
    color_button: gtk::ColorDialogButton,
    edit_button: gtk::ToggleButton,
    edit_crop: RefCell<Option<CropOverlay>>,
    edit_drag: Cell<Option<EditDrag>>,
    edit_scale: Cell<f64>,
    compare_canvas: RefCell<Option<ImageCanvas>>,
    compare_rendered: RefCell<Option<image::RgbaImage>>,
    compare_scrolled: RefCell<Option<gtk::ScrolledWindow>>,
    compare_paned: RefCell<Option<gtk::Paned>>,
    compare_locked: Cell<bool>,
    syncing_compare: Cell<bool>,
    lens_diameter: Cell<f32>,
    lens_magnification: Cell<f32>,
    lens_active: Cell<bool>,
    preview_cache: RefCell<lru::LruCache<String, crate::image::LoadedPreview>>,
    directory_monitor: RefCell<Option<gio::FileMonitor>>,
    prefetch_cancellables: RefCell<Vec<gio::Cancellable>>,
    animation_cancellable: RefCell<Option<gio::Cancellable>>,
    animation_frames: RefCell<Vec<AnimationFrame>>,
    animation_index: Cell<usize>,
    animation_paused: Cell<bool>,
    animation_controls: gtk::Box,
    animation_play_button: gtk::Button,
    export_cancellation: RefCell<Option<CancellationToken>>,
    transform_controls: gtk::Box,
    zoom_controls: gtk::Box,
    zoom_label: gtk::MenuButton,
    minimap: MiniMap,
}

impl ViewerWindow {
    pub fn new(application: &adw::Application, file: Option<gio::File>) -> Self {
        let settings = Settings::default();
        let canvas = ImageCanvas::default();
        canvas.set_filter(settings.zoom_filter());
        canvas.set_background(settings.background());
        canvas.set_zoom(settings.last_zoom());
        canvas.set_halign(gtk::Align::Center);
        canvas.set_valign(gtk::Align::Center);

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .hexpand(true)
            .vexpand(true)
            .child(&canvas)
            .build();
        scrolled.set_margin_top(10);
        scrolled.set_margin_bottom(10);
        scrolled.set_margin_start(10);
        scrolled.set_margin_end(10);
        let canvas_overlay = gtk::Overlay::new();
        canvas_overlay.set_child(Some(&scrolled));
        let transforms = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        transforms.add_css_class("linked");
        transforms.set_visible(false);
        transforms.set_halign(gtk::Align::End);
        transforms.set_valign(gtk::Align::End);
        transforms.set_margin_end(26);
        transforms.set_margin_bottom(26);
        transforms.append(&button(
            "object-rotate-left-symbolic",
            "Rotate Left",
            "win.rotate-counterclockwise",
        ));
        transforms.append(&button(
            "object-rotate-right-symbolic",
            "Rotate Right",
            "win.rotate-clockwise",
        ));
        transforms.append(&button(
            "object-flip-horizontal-symbolic",
            "Flip Horizontally",
            "win.flip-horizontal",
        ));
        transforms.append(&button(
            "object-flip-vertical-symbolic",
            "Flip Vertically",
            "win.flip-vertical",
        ));
        canvas_overlay.add_overlay(&transforms);
        let zoom_controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        zoom_controls.add_css_class("linked");
        zoom_controls.set_halign(gtk::Align::End);
        zoom_controls.set_valign(gtk::Align::End);
        zoom_controls.set_margin_end(26);
        zoom_controls.set_margin_bottom(26);
        zoom_controls.append(&button("zoom-out-symbolic", "Zoom Out", "win.zoom-out"));
        let zoom_label = gtk::MenuButton::builder()
            .label("100%")
            .tooltip_text("Zoom presets (0: Fit; 1–9: 100%–900%)")
            .build();
        zoom_label.set_margin_start(8);
        zoom_label.set_margin_end(8);
        let zoom_menu = gio::Menu::new();
        zoom_menu.append(Some("Fit to Window (0)"), Some("win.fit"));
        zoom_menu.append(Some("25%"), Some("win.zoom-25"));
        zoom_menu.append(Some("50%"), Some("win.zoom-50"));
        zoom_menu.append(Some("75%"), Some("win.zoom-75"));
        for (percent, action) in [
            (100, "win.zoom-100"),
            (200, "win.zoom-200"),
            (300, "win.zoom-300"),
            (400, "win.zoom-400"),
            (500, "win.zoom-500"),
            (600, "win.zoom-600"),
            (700, "win.zoom-700"),
            (800, "win.zoom-800"),
            (900, "win.zoom-900"),
        ] {
            zoom_menu.append(
                Some(&format!("{percent}% ({})", percent / 100)),
                Some(action),
            );
        }
        zoom_label.set_menu_model(Some(&zoom_menu));
        zoom_controls.append(&zoom_label);
        zoom_controls.append(&button("zoom-in-symbolic", "Zoom In", "win.zoom-in"));
        canvas_overlay.add_overlay(&zoom_controls);
        let minimap = MiniMap::default();
        minimap.set_size_request(160, 120);
        minimap.set_halign(gtk::Align::Start);
        minimap.set_valign(gtk::Align::Start);
        minimap.set_margin_start(20);
        minimap.set_margin_top(20);
        minimap.set_tooltip_text(Some("Image overview — click to pan"));
        minimap.set_visible(false);
        canvas_overlay.add_overlay(&minimap);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&canvas_overlay));

        let title = adw::WindowTitle::builder()
            .title("Image Viewer")
            .subtitle("Open an image to begin")
            .build();
        let header_widgets = build_header(&title);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header_widgets.header);
        content.append(&toasts);
        let (width, height) = settings.window_size();
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title("Image Viewer")
            .default_width(width)
            .default_height(height)
            .content(&content)
            .build();
        if settings.maximized() {
            window.maximize();
        }

        let lens_diameter = settings.compare_lens_size();
        let lens_magnification = settings.compare_lens_magnification();
        let this = Self(Rc::new(WindowState {
            window,
            canvas,
            scrolled,
            title,
            toasts,
            settings,
            current_file: RefCell::new(None),
            sequence: RefCell::new(None),
            cancellable: RefCell::new(None),
            render_cancellation: RefCell::new(None),
            load_generation: Cell::new(0),
            render_generation: Cell::new(0),
            document: RefCell::new(None),
            rendered: RefCell::new(None),
            source_modified: RefCell::new(None),
            close_approved: Cell::new(false),
            pencil_active: Cell::new(false),
            pencil_points: RefCell::new(Vec::new()),
            pencil_start: Cell::new((0.0, 0.0)),
            pencil_color: Cell::new([0, 0, 0, 255]),
            pencil_button: header_widgets.pencil_button,
            lens_button: header_widgets.lens_button,
            color_button: header_widgets.color_button,
            edit_button: header_widgets.edit_button,
            edit_crop: RefCell::new(None),
            edit_drag: Cell::new(None),
            edit_scale: Cell::new(1.0),
            compare_canvas: RefCell::new(None),
            compare_rendered: RefCell::new(None),
            compare_scrolled: RefCell::new(None),
            compare_paned: RefCell::new(None),
            compare_locked: Cell::new(true),
            syncing_compare: Cell::new(false),
            lens_diameter: Cell::new(lens_diameter),
            lens_magnification: Cell::new(lens_magnification),
            lens_active: Cell::new(false),
            preview_cache: RefCell::new(lru::LruCache::new(
                NonZeroUsize::new(3).expect("three is non-zero"),
            )),
            directory_monitor: RefCell::new(None),
            prefetch_cancellables: RefCell::new(Vec::new()),
            animation_cancellable: RefCell::new(None),
            animation_frames: RefCell::new(Vec::new()),
            animation_index: Cell::new(0),
            animation_paused: Cell::new(false),
            animation_controls: header_widgets.animation_controls,
            animation_play_button: header_widgets.animation_play_button,
            transform_controls: transforms,
            zoom_controls,
            zoom_label,
            minimap,
            export_cancellation: RefCell::new(None),
        }));
        this.install_actions();
        this.install_tool_controls();
        this.install_gestures();
        this.install_minimap();
        this.connect_single_image_lens();
        this.install_state_persistence();
        if let Some(file) = file {
            this.load(file);
        }
        this
    }

    pub fn present(&self) {
        self.0.window.present();
    }

    fn load(&self, file: gio::File) {
        if self
            .0
            .document
            .borrow()
            .as_ref()
            .is_some_and(Document::is_dirty)
        {
            let this = self.clone();
            self.confirm_discard("Discard unsaved edits and open another image?", move || {
                if let Some(document) = this.0.document.borrow_mut().as_mut() {
                    document.restore_original();
                }
                this.load(file.clone());
            });
            return;
        }
        if let Some(previous) = self.0.cancellable.borrow_mut().take() {
            previous.cancel();
        }
        if let Some(previous) = self.0.animation_cancellable.borrow_mut().take() {
            previous.cancel();
        }
        self.0.animation_frames.borrow_mut().clear();
        self.0.animation_controls.set_visible(false);
        self.exit_compare();
        let cancellable = gio::Cancellable::new();
        self.0.cancellable.replace(Some(cancellable.clone()));
        let generation = self.0.load_generation.get().wrapping_add(1);
        self.0.load_generation.set(generation);
        self.0.current_file.replace(Some(file.clone()));
        if let Some(parent) = file.parent() {
            self.0.settings.set_last_open_folder(&parent);
        }
        self.0.document.replace(None);
        self.0.rendered.replace(None);
        self.0.source_modified.replace(
            file.path()
                .and_then(|path| std::fs::metadata(path).ok())
                .and_then(|metadata| metadata.modified().ok()),
        );
        self.0.title.set_title(&file.basename().map_or_else(
            || file.uri().to_string(),
            |name| name.to_string_lossy().into_owned(),
        ));
        self.0.title.set_subtitle("Loading…");

        let decode = file.path().map(|path| {
            gio::spawn_blocking(move || decode_headless(&path, DecodeLimits::default()))
        });
        let cache_key = file.uri().to_string();
        let cached = self.0.preview_cache.borrow_mut().get(&cache_key).cloned();
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let preview = if let Some(preview) = cached {
                Ok(preview)
            } else {
                load_preview(&file, DecodeLimits::default(), &cancellable).await
            };
            let Some(state) = weak.upgrade() else {
                return;
            };
            if state.load_generation.get() != generation || cancellable.is_cancelled() {
                return;
            }
            match preview {
                Ok(preview) => {
                    state.canvas.set_texture(Some(&preview.texture));
                    ViewerWindow(state.clone()).fit(false);
                    state
                        .title
                        .set_subtitle(&format!("{} × {} · 100%", preview.width, preview.height));
                    if preview.animation_delay.is_some() {
                        ViewerWindow(state.clone()).start_animation(file.clone(), generation);
                    }
                    let editable = if let Some(decode) = decode {
                        decode.await
                    } else {
                        let bytes_file = file.clone();
                        match bytes_file.load_bytes_future().await {
                            Ok((bytes, _)) => {
                                gio::spawn_blocking(move || {
                                    decode_memory(bytes.as_ref().to_vec(), DecodeLimits::default())
                                })
                                .await
                            }
                            Err(error) => {
                                tracing::warn!(%error, "Could not read GIO-backed image for editing");
                                state.toasts.add_toast(adw::Toast::new(
                                    "This image can be viewed but could not be read for editing",
                                ));
                                return;
                            }
                        }
                    };
                    if state.load_generation.get() != generation || cancellable.is_cancelled() {
                        return;
                    }
                    match editable {
                        Ok(Ok(mut source)) => {
                            source.metadata = preview.metadata.clone();
                            let document = Document::new(source);
                            state
                                .rendered
                                .replace(Some(document.source().pixels.as_ref().clone()));
                            state.document.replace(Some(document));
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(%error, "Editable decode unavailable");
                            state.toasts.add_toast(adw::Toast::new(
                                "This image can be viewed but its decoder does not support editing",
                            ));
                        }
                        Err(_) => tracing::warn!("Editable decode worker panicked"),
                    }
                    let fallback = state.settings.folder_sort();
                    let sequence_file = file.clone();
                    let weak = Rc::downgrade(&state);
                    glib::spawn_future_local(async move {
                        let sequence = gio::spawn_blocking(move || {
                            DirectorySequence::build(&sequence_file, fallback)
                        })
                        .await;
                        if let Some(state) = weak.upgrade() {
                            match sequence {
                                Ok(Ok(sequence)) => {
                                    state.sequence.replace(Some(sequence));
                                    let this = ViewerWindow(state.clone());
                                    this.prefetch_neighbors();
                                    this.monitor_directory();
                                }
                                Ok(Err(error)) => {
                                    tracing::debug!(%error, "Directory navigation unavailable")
                                }
                                Err(_) => tracing::warn!("Directory navigation worker panicked"),
                            };
                        }
                    });
                }
                Err(error) => {
                    state.title.set_subtitle("Could not open image");
                    state.toasts.add_toast(adw::Toast::new(&error.to_string()));
                }
            }
        });
    }

    fn install_actions(&self) {
        self.add_action("open", {
            let this = self.clone();
            move || {
                let mut builder = gtk::FileDialog::builder().title("Open Image").modal(true);
                if let Some(folder) = this.0.settings.last_open_folder() {
                    builder = builder.initial_folder(&folder);
                }
                let dialog = builder.build();
                let parent = this.0.window.clone();
                let this = this.clone();
                glib::spawn_future_local(async move {
                    if let Ok(file) = dialog.open_future(Some(&parent)).await {
                        this.load(file);
                    }
                });
            }
        });
        self.add_action("close", {
            let window = self.0.window.clone();
            move || window.close()
        });
        self.add_action("zoom-in", {
            let this = self.clone();
            move || this.set_zoom(this.0.canvas.zoom() * 1.25)
        });
        self.add_action("zoom-out", {
            let this = self.clone();
            move || this.set_zoom(this.0.canvas.zoom() / 1.25)
        });
        self.add_action("actual-size", {
            let this = self.clone();
            move || this.set_zoom(1.0)
        });
        for (name, zoom) in [
            ("zoom-25", 0.25),
            ("zoom-50", 0.5),
            ("zoom-75", 0.75),
            ("zoom-100", 1.0),
            ("zoom-200", 2.0),
            ("zoom-300", 3.0),
            ("zoom-400", 4.0),
            ("zoom-500", 5.0),
            ("zoom-600", 6.0),
            ("zoom-700", 7.0),
            ("zoom-800", 8.0),
            ("zoom-900", 9.0),
        ] {
            let this = self.clone();
            self.add_action(name, move || this.set_zoom(zoom));
        }
        self.add_action("fit", {
            let this = self.clone();
            move || this.fit(false)
        });
        self.add_action("fill", {
            let this = self.clone();
            move || this.fit(true)
        });
        self.add_action("toggle-filter", {
            let this = self.clone();
            move || {
                let filter = match this.0.canvas.filter() {
                    ZoomFilter::Soft => ZoomFilter::Hard,
                    ZoomFilter::Hard => ZoomFilter::Soft,
                };
                this.0.canvas.set_filter(filter);
                if let Some(canvas) = this.0.compare_canvas.borrow().as_ref() {
                    canvas.set_filter(filter);
                }
                this.0.settings.set_zoom_filter(filter);
            }
        });
        self.add_action("previous", {
            let this = self.clone();
            move || this.navigate(false)
        });
        self.add_action("next", {
            let this = self.clone();
            move || this.navigate(true)
        });
        self.add_action("fullscreen", {
            let window = self.0.window.clone();
            move || {
                if window.is_fullscreen() {
                    window.unfullscreen();
                } else {
                    window.fullscreen();
                }
            }
        });
        self.add_action("play-pause", {
            let this = self.clone();
            move || this.toggle_animation()
        });
        self.add_action("previous-frame", {
            let this = self.clone();
            move || this.step_animation(false)
        });
        self.add_action("next-frame", {
            let this = self.clone();
            move || this.step_animation(true)
        });

        self.add_action("save", {
            let this = self.clone();
            move || this.save(false)
        });
        self.add_action("save-as", {
            let this = self.clone();
            move || this.save(true)
        });
        self.add_action("cancel-export", {
            let this = self.clone();
            move || {
                if let Some(cancellation) = this.0.export_cancellation.borrow_mut().take() {
                    cancellation.cancel();
                }
            }
        });
        self.add_action("undo", {
            let this = self.clone();
            move || {
                let changed = this
                    .0
                    .document
                    .borrow_mut()
                    .as_mut()
                    .is_some_and(Document::undo);
                if changed {
                    this.render_document();
                }
            }
        });
        self.add_action("redo", {
            let this = self.clone();
            move || {
                let changed = this
                    .0
                    .document
                    .borrow_mut()
                    .as_mut()
                    .is_some_and(Document::redo);
                if changed {
                    this.render_document();
                }
            }
        });
        self.add_action("rotate-clockwise", {
            let this = self.clone();
            move || this.apply(Operation::Rotate(Rotation::Clockwise90))
        });
        self.add_action("rotate-counterclockwise", {
            let this = self.clone();
            move || this.apply(Operation::Rotate(Rotation::CounterClockwise90))
        });
        self.add_action("flip-horizontal", {
            let this = self.clone();
            move || this.apply(Operation::FlipHorizontal)
        });
        self.add_action("flip-vertical", {
            let this = self.clone();
            move || this.apply(Operation::FlipVertical)
        });
        self.add_action("crop", {
            let this = self.clone();
            move || {
                this.0
                    .edit_button
                    .set_active(!this.0.edit_button.is_active())
            }
        });
        self.add_action("crop-content", {
            let this = self.clone();
            move || this.crop_to_content()
        });
        self.add_action("scale", {
            let this = self.clone();
            move || this.show_scale_dialog()
        });
        self.add_action("palette", {
            let this = self.clone();
            move || this.show_palette_dialog()
        });
        self.add_action("pencil", {
            let this = self.clone();
            move || {
                this.0
                    .pencil_button
                    .set_active(!this.0.pencil_button.is_active())
            }
        });
        self.add_action("cancel-tool", {
            let this = self.clone();
            move || {
                this.0.pencil_button.set_active(false);
                this.0.pencil_points.borrow_mut().clear();
                this.0.canvas.set_accessible_label("Image canvas");
                if let Some(cancellation) = this.0.render_cancellation.borrow_mut().take() {
                    cancellation.cancel();
                }
                this.0
                    .toasts
                    .add_toast(adw::Toast::new("Active tool cancelled"));
            }
        });
        self.add_action("preferences", {
            let this = self.clone();
            move || this.show_preferences()
        });
        self.add_action("shortcuts", {
            let this = self.clone();
            move || this.show_shortcuts()
        });
        self.add_action("properties", {
            let this = self.clone();
            move || this.show_properties()
        });
        self.add_action("about", {
            let window = self.0.window.clone();
            move || {
                adw::AboutDialog::builder()
                    .application_name("Image Viewer")
                    .application_icon(crate::APP_ID)
                    .version(env!("CARGO_PKG_VERSION"))
                    .developer_name("Diorama contributors")
                    .license_type(gtk::License::Gpl30)
                    .website("https://github.com/mendrik/diorama")
                    .issue_url("https://github.com/mendrik/diorama/issues")
                    .build()
                    .present(Some(&window));
            }
        });
        self.add_action("compare", {
            let this = self.clone();
            move || this.choose_comparison()
        });
        self.add_action("lens", {
            let this = self.clone();
            move || this.toggle_single_image_lens()
        });
        self.add_action("select-object", {
            let toasts = self.0.toasts.clone();
            move || {
                toasts.add_toast(adw::Toast::new(
                    "The optional local object-selection model is not installed",
                ))
            }
        });
    }

    fn add_action(&self, name: &str, callback: impl Fn() + 'static) {
        let action = gio::SimpleAction::new(name, None);
        action.connect_activate(move |_, _| callback());
        self.0.window.add_action(&action);
    }

    fn apply(&self, operation: Operation) {
        let Some(mut document) = self.0.document.borrow().clone() else {
            self.0
                .toasts
                .add_toast(adw::Toast::new("Open an editable image first"));
            return;
        };
        document.apply(operation);
        self.render_candidate(document, true);
    }

    fn install_tool_controls(&self) {
        self.0.pencil_button.connect_toggled({
            let this = self.clone();
            move |button| this.set_pencil_active(button.is_active())
        });
        self.0.lens_button.connect_toggled({
            let this = self.clone();
            move |button| this.set_single_image_lens_active(button.is_active())
        });
        self.0.color_button.connect_rgba_notify({
            let this = self.clone();
            move |button| this.0.pencil_color.set(rgba_to_u8(button.rgba()))
        });
        self.0.edit_button.connect_toggled({
            let this = self.clone();
            move |button| this.set_edit_active(button.is_active())
        });
    }

    fn set_pencil_active(&self, active: bool) {
        if active && self.0.rendered.borrow().is_none() {
            self.0.pencil_button.set_active(false);
            self.0
                .toasts
                .add_toast(adw::Toast::new("Open an editable image first"));
            return;
        }
        self.0.pencil_active.set(active);
        self.0.canvas.set_accessible_label(if active {
            "Image canvas, Pencil tool active"
        } else {
            "Image canvas"
        });
    }

    fn preview_pencil_stroke(&self) {
        let Some(image) = self.0.rendered.borrow().clone() else {
            return;
        };
        self.paint_pencil_preview(&self.0.canvas, &image);
    }

    fn preview_comparison_pencil_stroke(&self, canvas: &ImageCanvas) {
        let Some(image) = self.0.compare_rendered.borrow().clone() else {
            return;
        };
        self.paint_pencil_preview(canvas, &image);
    }

    fn paint_pencil_preview(
        &self,
        canvas: &ImageCanvas,
        image: &image::RgbaImage,
    ) -> Option<image::RgbaImage> {
        let stroke = Stroke {
            points: self.0.pencil_points.borrow().clone(),
            color: self.0.pencil_color.get(),
            width: 1.0,
            opacity: 1.0,
            hardness: 1.0,
        };
        if let Ok(preview) =
            crate::tools::pencil::paint_stroke(image, &stroke, &CancellationToken::default())
            && let Ok(texture) = texture_from_rgba(&preview)
        {
            canvas.set_texture(Some(&texture));
            if canvas == &self.0.canvas {
                self.update_minimap();
            }
            return Some(preview);
        }
        None
    }

    fn commit_comparison_pencil_stroke(&self, canvas: &ImageCanvas) {
        let Some(image) = self.0.compare_rendered.borrow().clone() else {
            return;
        };
        if let Some(preview) = self.paint_pencil_preview(canvas, &image) {
            self.0.compare_rendered.replace(Some(preview));
        }
    }

    fn set_edit_active(&self, active: bool) {
        self.0.transform_controls.set_visible(active);
        self.0.zoom_controls.set_visible(!active);
        self.0.lens_button.set_active(false);
        self.0.lens_active.set(false);
        self.0.canvas.set_cursor_from_name(None);
        self.0.canvas.clear_lens();
        if let Some(canvas) = self.0.compare_canvas.borrow().as_ref() {
            canvas.clear_lens();
        }
        self.0.lens_button.set_sensitive(!active);
        if !active {
            self.0.canvas.set_preview_scale(1.0);
            self.0.edit_scale.set(1.0);
            self.0.canvas.set_crop_overlay(None);
            self.0.edit_crop.replace(None);
            return;
        }
        let Some((width, height)) = self
            .0
            .rendered
            .borrow()
            .as_ref()
            .map(image::GenericImageView::dimensions)
        else {
            self.0.edit_button.set_active(false);
            self.0
                .toasts
                .add_toast(adw::Toast::new("Open an editable image first"));
            return;
        };
        let crop = CropOverlay {
            x: 0,
            y: 0,
            width,
            height,
            image_width: width,
            image_height: height,
        };
        self.0.edit_crop.replace(Some(crop));
        self.0.canvas.set_crop_overlay(Some(crop));
        self.fit(false);
    }

    fn render_document(&self) {
        let Some(document) = self.0.document.borrow().clone() else {
            return;
        };
        self.render_candidate(document, false);
    }

    fn render_candidate(&self, document: Document, commit_on_success: bool) {
        if let Some(previous) = self.0.render_cancellation.borrow_mut().take() {
            previous.cancel();
        }
        let cancellation = CancellationToken::default();
        self.0
            .render_cancellation
            .replace(Some(cancellation.clone()));
        let generation = self.0.render_generation.get().wrapping_add(1);
        self.0.render_generation.set(generation);
        if !commit_on_success {
            self.update_title();
        }

        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || {
                let rendered = document.render(&cancellation);
                (document, rendered)
            })
            .await;
            let Some(state) = weak.upgrade() else {
                return;
            };
            if state.render_generation.get() != generation {
                return;
            }
            match result {
                Ok((document, Ok(rendered))) => {
                    let dimensions = rendered.pixels.dimensions();
                    match texture_from_rgba(&rendered.pixels) {
                        Ok(texture) => {
                            state.document.replace(Some(document));
                            if commit_on_success {
                                ViewerWindow(state.clone()).update_title();
                            }
                            state.canvas.set_texture(Some(&texture));
                            state.rendered.replace(Some(rendered.pixels));
                            ViewerWindow(state.clone()).update_minimap();
                            state.title.set_subtitle(&format!(
                                "{} × {} · {:.0}%",
                                dimensions.0,
                                dimensions.1,
                                state.canvas.zoom() * 100.0
                            ));
                        }
                        Err(error) => state.toasts.add_toast(adw::Toast::new(&error)),
                    }
                }
                Ok((_, Err(crate::error::AppError::Cancelled))) => {}
                Ok((_, Err(error))) => state.toasts.add_toast(adw::Toast::new(&error.to_string())),
                Err(_) => state
                    .toasts
                    .add_toast(adw::Toast::new("Image processing worker failed")),
            }
        });
    }

    fn update_title(&self) {
        let Some(file) = self.0.current_file.borrow().clone() else {
            return;
        };
        let mut title = file.basename().map_or_else(
            || file.uri().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        if self
            .0
            .document
            .borrow()
            .as_ref()
            .is_some_and(Document::is_dirty)
        {
            title.push_str(" •");
        }
        self.0.title.set_title(&title);
    }

    fn save(&self, force_dialog: bool) {
        let Some(document) = self.0.document.borrow().clone() else {
            self.0
                .toasts
                .add_toast(adw::Toast::new("Open an editable image first"));
            return;
        };
        let current = self.0.current_file.borrow().clone();
        let direct_path = (!force_dialog)
            .then(|| current.as_ref().and_then(gio::File::path))
            .flatten()
            .filter(|path| export_options(path, &self.0.settings).is_some());

        if let Some(path) = direct_path {
            if self.source_changed(&path) {
                self.0.toasts.add_toast(adw::Toast::new(
                    "The file changed externally; use Save As to avoid overwriting it",
                ));
                return;
            }
            self.export_document(document, path);
            return;
        }

        let dialog = gtk::FileDialog::builder()
            .title("Save Image")
            .initial_name("image.png")
            .modal(true)
            .build();
        let parent = self.0.window.clone();
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(file) = dialog.save_future(Some(&parent)).await {
                if let Some(path) = file.path() {
                    this.show_export_options(document, path);
                } else {
                    this.0.toasts.add_toast(adw::Toast::new(
                        "This location does not support atomic export",
                    ));
                }
            }
        });
    }

    fn export_document(&self, document: Document, path: PathBuf) {
        let Some(options) = export_options(&path, &self.0.settings) else {
            self.0.toasts.add_toast(adw::Toast::new(
                "Choose a file name ending in .png, .jpg, or .jpeg",
            ));
            return;
        };
        self.export_document_with_options(document, path, options);
    }

    fn show_export_options(&self, document: Document, path: PathBuf) {
        let Some(defaults) = export_options(&path, &self.0.settings) else {
            self.0.toasts.add_toast(adw::Toast::new(
                "Choose a file name ending in .png, .jpg, or .jpeg",
            ));
            return;
        };
        let dialog = adw::Dialog::builder()
            .title("Export Options")
            .content_width(420)
            .build();
        let header = adw::HeaderBar::new();
        let cancel = gtk::Button::with_label("Cancel");
        let export = gtk::Button::with_label("Export");
        export.add_css_class("suggested-action");
        header.pack_start(&cancel);
        header.pack_end(&export);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();
        let preserve =
            gtk::CheckButton::with_label("Preserve compatible metadata and color profile");
        preserve.set_active(self.0.settings.preserve_metadata());
        content.append(&preserve);
        let jpeg_background = gtk::DropDown::from_strings(&["White", "Gray", "Black"]);
        let convert_srgb = gtk::CheckButton::with_label("Convert color profile to sRGB");
        let control: gtk::Widget = match &defaults {
            ExportOptions::Png(options) => {
                let compression =
                    gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 9.0, 1.0);
                compression.set_value(f64::from(options.compression));
                compression.set_digits(0);
                compression.set_hexpand(true);
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                row.append(&gtk::Label::new(Some("Compression")));
                row.append(&compression);
                content.append(&convert_srgb);
                row.upcast()
            }
            ExportOptions::Jpeg(options) => {
                let quality = gtk::Scale::with_range(gtk::Orientation::Horizontal, 1.0, 100.0, 1.0);
                quality.set_value(f64::from(options.quality));
                quality.set_digits(0);
                quality.set_hexpand(true);
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                row.append(&gtk::Label::new(Some("Quality")));
                row.append(&quality);
                jpeg_background.set_selected(match options.background {
                    [128, 128, 128] => 1,
                    [0, 0, 0] => 2,
                    _ => 0,
                });
                let background_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                background_row.append(&gtk::Label::new(Some("Transparency background")));
                background_row.append(&jpeg_background);
                content.append(&background_row);
                row.upcast()
            }
        };
        content.append(&control);
        if matches!(defaults, ExportOptions::Jpeg(_)) {
            content.append(&gtk::Label::new(Some(
                "Transparent pixels are composited onto the saved JPEG background.",
            )));
        }
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
        let this = self.clone();
        let export_dialog = dialog.clone();
        export.connect_clicked(move |_| {
            let preserve_metadata = preserve.is_active();
            let value = control
                .downcast_ref::<gtk::Box>()
                .and_then(|row| row.last_child())
                .and_then(|widget| widget.downcast::<gtk::Scale>().ok())
                .map_or(0.0, |scale| scale.value());
            let options = match defaults.clone() {
                ExportOptions::Png(mut options) => {
                    options.compression = value as u8;
                    options.preserve_metadata = preserve_metadata;
                    options.convert_to_srgb = convert_srgb.is_active();
                    this.0.settings.set_png_compression(options.compression);
                    ExportOptions::Png(options)
                }
                ExportOptions::Jpeg(mut options) => {
                    options.quality = value as u8;
                    options.preserve_metadata = preserve_metadata;
                    options.background = match jpeg_background.selected() {
                        1 => [128, 128, 128],
                        2 => [0, 0, 0],
                        _ => [255, 255, 255],
                    };
                    this.0.settings.set_jpeg_quality(options.quality);
                    this.0.settings.set_jpeg_background(options.background);
                    ExportOptions::Jpeg(options)
                }
            };
            this.0.settings.set_preserve_metadata(preserve_metadata);
            this.export_document_with_options(document.clone(), path.clone(), options);
            export_dialog.close();
        });
        dialog.present(Some(&self.0.window));
    }

    fn export_document_with_options(
        &self,
        document: Document,
        path: PathBuf,
        options: ExportOptions,
    ) {
        if let Some(previous) = self.0.export_cancellation.borrow_mut().take() {
            previous.cancel();
        }
        let cancellation = CancellationToken::default();
        self.0
            .export_cancellation
            .replace(Some(cancellation.clone()));
        let worker_cancellation = cancellation.clone();
        let worker_path = path.clone();
        self.0.toasts.add_toast(
            adw::Toast::builder()
                .title("Exporting image…")
                .button_label("Cancel")
                .action_name("win.cancel-export")
                .build(),
        );
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || {
                let rendered = document.render(&worker_cancellation)?;
                crate::export::export(&rendered, &worker_path, &options, &worker_cancellation)
            })
            .await;
            let Some(state) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(Ok(())) => {
                    if let Some(document) = state.document.borrow_mut().as_mut() {
                        document.mark_saved();
                    }
                    state.source_modified.replace(
                        std::fs::metadata(&path)
                            .ok()
                            .and_then(|metadata| metadata.modified().ok()),
                    );
                    state.toasts.add_toast(adw::Toast::new("Image saved"));
                    let title = state
                        .current_file
                        .borrow()
                        .as_ref()
                        .and_then(gio::File::basename)
                        .map_or_else(
                            || "Image Viewer".to_owned(),
                            |name| name.to_string_lossy().into_owned(),
                        );
                    state.title.set_title(&title);
                }
                Ok(Err(error)) => state.toasts.add_toast(adw::Toast::new(&error.to_string())),
                Err(_) => state
                    .toasts
                    .add_toast(adw::Toast::new("Export worker failed")),
            }
        });
    }

    fn source_changed(&self, path: &Path) -> bool {
        let current = std::fs::metadata(path)
            .ok()
            .and_then(|metadata| metadata.modified().ok());
        current.is_some() && current != *self.0.source_modified.borrow()
    }

    fn crop_to_content(&self) {
        let Some(image) = self.0.rendered.borrow().clone() else {
            self.0
                .toasts
                .add_toast(adw::Toast::new("Open an editable image first"));
            return;
        };
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || {
                if image.pixels().any(|pixel| pixel.0[3] < 255) {
                    crate::tools::crop::alpha_content_bounds(&image, 1).map(Some)
                } else {
                    crate::tools::crop::opaque_content_bounds(&image, 16)
                }
            })
            .await;
            let Some(state) = weak.upgrade() else {
                return;
            };
            let bounds = match result {
                Ok(Ok(Some(bounds))) => bounds,
                Ok(Ok(None)) => {
                    state.toasts.add_toast(adw::Toast::new(
                        "The background could not be identified with enough confidence",
                    ));
                    return;
                }
                Ok(Err(error)) => {
                    state.toasts.add_toast(adw::Toast::new(&error.to_string()));
                    return;
                }
                Err(_) => {
                    state
                        .toasts
                        .add_toast(adw::Toast::new("Content detection worker failed"));
                    return;
                }
            };
            let dialog = adw::AlertDialog::builder()
                .heading("Crop to detected content?")
                .body(format!(
                    "Detected bounds: x {}, y {}, {} × {} pixels",
                    bounds.x, bounds.y, bounds.width, bounds.height
                ))
                .close_response("cancel")
                .default_response("apply")
                .build();
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("apply", "Apply");
            dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
            let weak = Rc::downgrade(&state);
            dialog.connect_response(None, move |_, response| {
                if response == "apply"
                    && let Some(state) = weak.upgrade()
                {
                    let this = ViewerWindow(state);
                    this.apply(Operation::Crop {
                        x: bounds.x,
                        y: bounds.y,
                        width: bounds.width,
                        height: bounds.height,
                    });
                }
            });
            dialog.present(Some(&state.window));
        });
    }

    fn show_scale_dialog(&self) {
        let Some((width, height)) = self
            .0
            .rendered
            .borrow()
            .as_ref()
            .map(image::GenericImageView::dimensions)
        else {
            self.0
                .toasts
                .add_toast(adw::Toast::new("Open an editable image first"));
            return;
        };
        let dialog = adw::Dialog::builder()
            .title("Scale Image")
            .content_width(400)
            .build();
        let header = adw::HeaderBar::new();
        let cancel = gtk::Button::with_label("Cancel");
        let apply = gtk::Button::with_label("Scale");
        apply.add_css_class("suggested-action");
        header.pack_start(&cancel);
        header.pack_end(&apply);
        let grid = gtk::Grid::builder()
            .row_spacing(8)
            .column_spacing(12)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();
        let target_width = spin(1.0, 100_000.0, f64::from(width));
        let target_height = spin(1.0, 100_000.0, f64::from(height));
        let lock = gtk::CheckButton::builder()
            .label("Lock aspect ratio")
            .active(true)
            .build();
        let mode =
            gtk::DropDown::from_strings(&["Nearest Neighbor", "Linear", "Bicubic", "Seam Carving"]);
        mode.set_selected(2);
        grid.attach(&gtk::Label::new(Some("Width")), 0, 0, 1, 1);
        grid.attach(&target_width, 1, 0, 1, 1);
        grid.attach(&gtk::Label::new(Some("Height")), 0, 1, 1, 1);
        grid.attach(&target_height, 1, 1, 1, 1);
        grid.attach(&lock, 0, 2, 2, 1);
        grid.attach(&gtk::Label::new(Some("Resampling")), 0, 3, 1, 1);
        grid.attach(&mode, 1, 3, 1, 1);
        let changing = Rc::new(Cell::new(false));
        target_width.connect_value_changed({
            let target_height = target_height.clone();
            let lock = lock.clone();
            let changing = changing.clone();
            move |control| {
                if lock.is_active() && !changing.replace(true) {
                    target_height.set_value(control.value() * f64::from(height) / f64::from(width));
                    changing.set(false);
                }
            }
        });
        target_height.connect_value_changed({
            let target_width = target_width.clone();
            let lock = lock.clone();
            move |control| {
                if lock.is_active() && !changing.replace(true) {
                    target_width.set_value(control.value() * f64::from(width) / f64::from(height));
                    changing.set(false);
                }
            }
        });
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header);
        content.append(&grid);
        dialog.set_child(Some(&content));
        cancel.connect_clicked({
            let dialog = dialog.clone();
            move |_| {
                dialog.close();
            }
        });
        let this = self.clone();
        let apply_dialog = dialog.clone();
        apply.connect_clicked(move |_| {
            let target_width = target_width.value() as u32;
            let target_height = target_height.value() as u32;
            let resampling = match mode.selected() {
                0 => Resampling::Nearest,
                1 => Resampling::Linear,
                3 => Resampling::SeamCarving,
                _ => Resampling::Bicubic,
            };
            if resampling == Resampling::SeamCarving
                && (target_width > width || target_height > height)
            {
                this.0.toasts.add_toast(adw::Toast::new(
                    "Seam carving currently supports shrinking only",
                ));
                return;
            }
            if target_width > width || target_height > height {
                this.0.toasts.add_toast(adw::Toast::new(
                    "Scaling up may reduce perceived image quality",
                ));
            }
            this.apply(Operation::Scale {
                width: target_width,
                height: target_height,
                resampling,
            });
            apply_dialog.close();
        });
        dialog.present(Some(&self.0.window));
    }

    fn show_palette_dialog(&self) {
        if self.0.rendered.borrow().is_none() {
            self.0
                .toasts
                .add_toast(adw::Toast::new("Open an editable image first"));
            return;
        }
        let dialog = adw::Dialog::builder()
            .title("Reduce Palette")
            .content_width(420)
            .build();
        let header = adw::HeaderBar::new();
        let cancel = gtk::Button::with_label("Cancel");
        let apply = gtk::Button::with_label("Apply");
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
        let colors = gtk::Scale::with_range(gtk::Orientation::Horizontal, 2.0, 256.0, 1.0);
        colors.set_value(16.0);
        colors.set_digits(0);
        colors.set_hexpand(true);
        let count = gtk::SpinButton::with_range(2.0, 256.0, 1.0);
        count.set_value(16.0);
        colors
            .bind_property("value", &count, "value")
            .bidirectional()
            .build();
        let count_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        count_row.append(&gtk::Label::new(Some("Colors")));
        count_row.append(&colors);
        count_row.append(&count);
        let dithering = gtk::CheckButton::with_label("Dithering");
        let accents = gtk::CheckButton::with_label("Preserve accents and isolated colors");
        accents.set_active(true);
        content.append(&count_row);
        content.append(&dithering);
        content.append(&accents);
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
        let this = self.clone();
        let apply_dialog = dialog.clone();
        apply.connect_clicked(move |_| {
            this.apply(Operation::Palette {
                colors: count.value() as u16,
                dithering: dithering.is_active(),
                preserve_accents: accents.is_active(),
                protected: Vec::new(),
            });
            apply_dialog.close();
        });
        dialog.present(Some(&self.0.window));
    }

    fn show_preferences(&self) {
        let dialog = adw::Dialog::builder()
            .title("Preferences")
            .content_width(420)
            .build();
        let header = adw::HeaderBar::new();
        let done = gtk::Button::with_label("Done");
        done.add_css_class("suggested-action");
        header.pack_end(&done);
        let group = adw::PreferencesGroup::builder()
            .title("Viewing")
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();
        let filter = adw::SwitchRow::builder()
            .title("Hard zoom")
            .subtitle("Keep pixel edges sharp with nearest-neighbor rendering")
            .active(self.0.canvas.filter() == ZoomFilter::Hard)
            .build();
        group.add(&filter);
        let background = adw::ComboRow::builder()
            .title("Transparency background")
            .model(&gtk::StringList::new(&[
                "Checkerboard",
                "White",
                "Gray",
                "Black",
            ]))
            .selected(match self.0.canvas.background() {
                Background::Checkerboard => 0,
                Background::White => 1,
                Background::Gray => 2,
                Background::Black => 3,
            })
            .build();
        group.add(&background);
        let lens_size = adw::ComboRow::builder()
            .title("Lens size")
            .subtitle("Diameter of the pixel-inspection lens")
            .model(&gtk::StringList::new(&["Small", "Medium", "Large"]))
            .selected(lens_size_index(self.0.lens_diameter.get()))
            .build();
        group.add(&lens_size);
        let resampling = adw::ComboRow::builder()
            .title("Scaling method")
            .model(&gtk::StringList::new(&[
                "Nearest",
                "Linear",
                "Bicubic",
                "Seam carving",
            ]))
            .selected(match self.0.settings.scale_resampling() {
                Resampling::Nearest => 0,
                Resampling::Linear => 1,
                Resampling::Bicubic => 2,
                Resampling::SeamCarving => 3,
            })
            .build();
        group.add(&resampling);
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        outer.append(&header);
        outer.append(&group);
        dialog.set_child(Some(&outer));
        let this = self.clone();
        let apply_dialog = dialog.clone();
        done.connect_clicked(move |_| {
            let zoom_filter = if filter.is_active() {
                ZoomFilter::Hard
            } else {
                ZoomFilter::Soft
            };
            let background = match background.selected() {
                1 => Background::White,
                2 => Background::Gray,
                3 => Background::Black,
                _ => Background::Checkerboard,
            };
            let lens_diameter = match lens_size.selected() {
                1 => 280.0,
                2 => 400.0,
                _ => 180.0,
            };
            this.0.canvas.set_filter(zoom_filter);
            this.0.canvas.set_background(background);
            if let Some(canvas) = this.0.compare_canvas.borrow().as_ref() {
                canvas.set_filter(zoom_filter);
                canvas.set_background(background);
            }
            this.0.lens_diameter.set(lens_diameter);
            this.0.settings.set_zoom_filter(zoom_filter);
            this.0.settings.set_background(background);
            this.0.settings.set_compare_lens_size(lens_diameter);
            this.0
                .settings
                .set_scale_resampling(match resampling.selected() {
                    0 => Resampling::Nearest,
                    1 => Resampling::Linear,
                    3 => Resampling::SeamCarving,
                    _ => Resampling::Bicubic,
                });
            apply_dialog.close();
        });
        dialog.present(Some(&self.0.window));
    }

    fn show_shortcuts(&self) {
        let dialog = adw::ShortcutsDialog::new();
        for (title, shortcuts) in [
            (
                "General",
                vec![
                    ("Open", "<Control>o"),
                    ("Save", "<Control>s"),
                    ("Save As", "<Control><Shift>s"),
                    ("Close", "<Control>w"),
                    ("Preferences", "<Control>comma"),
                ],
            ),
            (
                "Viewing",
                vec![
                    ("Zoom In", "plus"),
                    ("Zoom Out", "minus"),
                    ("Fit to Window", "0"),
                    ("Zoom 100%–900%", "1–9"),
                    ("Toggle Soft/Hard Zoom", "x"),
                    ("Previous Image", "Left"),
                    ("Next Image", "Right"),
                ],
            ),
            (
                "Editing",
                vec![
                    ("Undo", "<Control>z"),
                    ("Redo", "<Control><Shift>z"),
                    ("Rotate Clockwise", "r"),
                    ("Rotate Counterclockwise", "<Shift>r"),
                    ("Flip Horizontally", "h"),
                    ("Flip Vertically", "v"),
                    ("Crop", "c"),
                    ("Pencil", "p"),
                    ("Exit Active Tool", "Escape"),
                ],
            ),
        ] {
            let section = adw::ShortcutsSection::new(Some(title));
            for (item_title, accelerator) in shortcuts {
                section.add(adw::ShortcutsItem::new(item_title, accelerator));
            }
            dialog.add(section);
        }
        dialog.present(Some(&self.0.window));
    }

    fn show_properties(&self) {
        let Some(document) = self.0.document.borrow().clone() else {
            self.0
                .toasts
                .add_toast(adw::Toast::new("Open an editable image first"));
            return;
        };
        let (width, height) = self
            .0
            .rendered
            .borrow()
            .as_ref()
            .map_or((0, 0), image::GenericImageView::dimensions);
        let source = document.source();
        let location = source.path.as_ref().map_or_else(
            || "GIO location".to_owned(),
            |path| path.display().to_string(),
        );
        let metadata = &source.metadata;
        let body = format!(
            "Dimensions: {width} × {height}\nLocation: {location}\nFormat: {}\nEXIF: {} · XMP: {} · ICC profile: {}",
            metadata.mime_type.as_deref().unwrap_or("Unknown"),
            if metadata.exif.is_some() { "Yes" } else { "No" },
            if metadata.xmp.is_some() { "Yes" } else { "No" },
            if metadata.icc.is_some() { "Yes" } else { "No" },
        );
        let dialog = adw::AlertDialog::builder()
            .heading("Image Properties")
            .body(body)
            .close_response("close")
            .build();
        dialog.add_response("close", "Close");
        dialog.present(Some(&self.0.window));
    }

    fn start_animation(&self, file: gio::File, generation: u64) {
        let cancellable = gio::Cancellable::new();
        self.0
            .animation_cancellable
            .replace(Some(cancellable.clone()));
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let frames = decode_animation(&file, DecodeLimits::default(), &cancellable).await;
            let Some(state) = weak.upgrade() else {
                return;
            };
            if state.load_generation.get() != generation || cancellable.is_cancelled() {
                return;
            }
            let frames = match frames {
                Ok(frames) if frames.len() > 1 => frames,
                Ok(_) => return,
                Err(error) => {
                    tracing::debug!(%error, "Animation playback unavailable");
                    return;
                }
            };
            state.animation_frames.replace(frames);
            state.animation_index.set(0);
            state.animation_paused.set(false);
            state.animation_controls.set_visible(true);
            ViewerWindow(state.clone()).sync_animation_play_button();
            loop {
                if state.load_generation.get() != generation || cancellable.is_cancelled() {
                    break;
                }
                if state.animation_paused.get() {
                    glib::timeout_future(std::time::Duration::from_millis(50)).await;
                    continue;
                }
                let delay = state
                    .animation_frames
                    .borrow()
                    .get(state.animation_index.get())
                    .map_or(std::time::Duration::from_millis(100), |frame| frame.delay);
                glib::timeout_future(delay).await;
                if state.animation_paused.get() {
                    continue;
                }
                let count = state.animation_frames.borrow().len();
                if count == 0 {
                    break;
                }
                let next = (state.animation_index.get() + 1) % count;
                state.animation_index.set(next);
                if let Some(frame) = state.animation_frames.borrow().get(next) {
                    state.canvas.set_texture(Some(&frame.texture));
                    ViewerWindow(state.clone()).update_minimap();
                }
            }
        });
    }

    fn step_animation(&self, forward: bool) {
        let frames = self.0.animation_frames.borrow();
        if frames.is_empty() {
            return;
        }
        self.0.animation_paused.set(true);
        self.sync_animation_play_button();
        let current = self.0.animation_index.get();
        let next = if forward {
            (current + 1) % frames.len()
        } else {
            current.checked_sub(1).unwrap_or(frames.len() - 1)
        };
        self.0.animation_index.set(next);
        self.0.canvas.set_texture(Some(&frames[next].texture));
        self.update_minimap();
    }

    fn toggle_animation(&self) {
        if self.0.animation_frames.borrow().is_empty() {
            return;
        }
        self.0.animation_paused.set(!self.0.animation_paused.get());
        self.sync_animation_play_button();
    }

    fn sync_animation_play_button(&self) {
        let paused = self.0.animation_paused.get();
        self.0.animation_play_button.set_icon_name(if paused {
            "media-playback-start-symbolic"
        } else {
            "media-playback-pause-symbolic"
        });
        self.0
            .animation_play_button
            .set_tooltip_text(Some(if paused {
                "Play animation"
            } else {
                "Stop animation"
            }));
    }

    fn prefetch_neighbors(&self) {
        for cancellable in self.0.prefetch_cancellables.borrow_mut().drain(..) {
            cancellable.cancel();
        }
        let neighbors = self
            .0
            .sequence
            .borrow()
            .as_ref()
            .map_or_else(Vec::new, DirectorySequence::neighbors);
        for file in neighbors {
            let key = file.uri().to_string();
            if self.0.preview_cache.borrow_mut().contains(&key) {
                continue;
            }
            let cancellable = gio::Cancellable::new();
            self.0
                .prefetch_cancellables
                .borrow_mut()
                .push(cancellable.clone());
            let weak = Rc::downgrade(&self.0);
            glib::spawn_future_local(async move {
                if let Ok(preview) =
                    load_preview(&file, DecodeLimits::default(), &cancellable).await
                    && !cancellable.is_cancelled()
                    && let Some(state) = weak.upgrade()
                {
                    state.preview_cache.borrow_mut().put(key, preview);
                }
            });
        }
    }

    fn monitor_directory(&self) {
        self.0.directory_monitor.replace(None);
        let Some(parent) = self
            .0
            .current_file
            .borrow()
            .as_ref()
            .and_then(gio::File::parent)
        else {
            return;
        };
        let Ok(monitor) =
            parent.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        else {
            return;
        };
        monitor.connect_changed({
            let weak = Rc::downgrade(&self.0);
            move |_, _, _, _| {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                let Some(current) = state.current_file.borrow().clone() else {
                    return;
                };
                let fallback = state.settings.folder_sort();
                let weak = Rc::downgrade(&state);
                glib::spawn_future_local(async move {
                    let result =
                        gio::spawn_blocking(move || DirectorySequence::build(&current, fallback))
                            .await;
                    if let Some(state) = weak.upgrade() {
                        match result {
                            Ok(Ok(sequence)) => {
                                state.sequence.replace(Some(sequence));
                                ViewerWindow(state).prefetch_neighbors();
                            }
                            Ok(Err(crate::error::AppError::FileMissing(_))) => {
                                state.toasts.add_toast(adw::Toast::new(
                                    "The current file was moved or deleted",
                                ))
                            }
                            Ok(Err(error)) => {
                                tracing::debug!(%error, "Could not refresh directory navigation");
                            }
                            Err(_) => tracing::warn!("Directory monitor worker panicked"),
                        }
                    }
                });
            }
        });
        self.0.directory_monitor.replace(Some(monitor));
    }

    fn choose_comparison(&self) {
        if self.0.canvas.texture().is_none() {
            self.0
                .toasts
                .add_toast(adw::Toast::new("Open the first image before comparing"));
            return;
        }
        let dialog = gtk::FileDialog::builder()
            .title("Choose Comparison Image")
            .modal(true)
            .build();
        let parent = self.0.window.clone();
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(file) = dialog.open_future(Some(&parent)).await {
                this.load_comparison(file);
            }
        });
    }

    fn load_comparison(&self, file: gio::File) {
        let cancellable = gio::Cancellable::new();
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let preview = load_preview(&file, DecodeLimits::default(), &cancellable).await;
            let Some(state) = weak.upgrade() else {
                return;
            };
            match preview {
                Ok(preview) => ViewerWindow(state).enter_compare(preview),
                Err(error) => state.toasts.add_toast(adw::Toast::new(&error.to_string())),
            }
        });
    }

    fn enter_compare(&self, preview: crate::image::LoadedPreview) {
        self.exit_compare();
        let Some(primary) = self.0.canvas.texture() else {
            return;
        };
        let compare_canvas = ImageCanvas::default();
        compare_canvas.set_texture(Some(&preview.texture));
        compare_canvas.set_filter(self.0.canvas.filter());
        compare_canvas.set_background(self.0.canvas.background());
        compare_canvas.set_zoom(self.0.canvas.zoom());
        compare_canvas.set_tooltip_text(Some("Comparison image panel"));
        self.0.canvas.set_tooltip_text(Some("Primary image panel"));
        compare_canvas.set_accessible_label("Comparison image B");
        self.0.canvas.set_accessible_label("Primary image A");
        let compare_scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .hexpand(true)
            .vexpand(true)
            .child(&compare_canvas)
            .build();
        let orientation = match choose_split(
            (primary.width() as u32, primary.height() as u32),
            (preview.width, preview.height),
        ) {
            SplitOrientation::Vertical => gtk::Orientation::Horizontal,
            SplitOrientation::Horizontal => gtk::Orientation::Vertical,
        };
        let paned = gtk::Paned::builder()
            .orientation(orientation)
            .wide_handle(true)
            .shrink_start_child(false)
            .shrink_end_child(false)
            .build();

        self.0.toasts.set_child(None::<&gtk::Widget>);
        paned.set_start_child(Some(&self.0.scrolled));
        paned.set_end_child(Some(&compare_scrolled));
        let toolbar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
            .halign(gtk::Align::Center)
            .build();
        toolbar.add_css_class("toolbar");
        let lock = gtk::ToggleButton::builder()
            .icon_name("changes-prevent-symbolic")
            .tooltip_text("Synchronize Pan and Zoom")
            .active(true)
            .build();
        let close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Exit Compare Mode")
            .build();
        toolbar.append(&lock);
        toolbar.append(&close);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&toolbar);
        root.append(&paned);
        self.0.toasts.set_child(Some(&root));
        self.0.compare_canvas.replace(Some(compare_canvas.clone()));
        self.0
            .compare_scrolled
            .replace(Some(compare_scrolled.clone()));
        self.0.compare_paned.replace(Some(paned.clone()));
        self.0.compare_locked.set(true);
        self.0
            .compare_rendered
            .replace(rgba_from_texture(&preview.texture));

        lock.connect_toggled({
            let this = self.clone();
            move |button| this.0.compare_locked.set(button.is_active())
        });
        close.connect_clicked({
            let this = self.clone();
            move |_| this.exit_compare()
        });
        self.connect_compare_adjustments(&compare_scrolled);
        self.connect_lens(&self.0.canvas, &compare_canvas, &preview.texture);
        self.connect_lens(&compare_canvas, &self.0.canvas, &primary);
        self.install_comparison_pencil_gestures(&compare_canvas);
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.connect_scroll({
            let this = self.clone();
            move |controller, _dx, dy| {
                if controller
                    .current_event_state()
                    .contains(gtk::gdk::ModifierType::CONTROL_MASK)
                {
                    let factor = if dy < 0.0 { 1.25 } else { 0.8 };
                    this.set_zoom(this.0.canvas.zoom() * factor);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        });
        compare_canvas.add_controller(scroll);
        paned.connect_map(move |paned| {
            let available = if paned.orientation() == gtk::Orientation::Horizontal {
                paned.width()
            } else {
                paned.height()
            };
            paned.set_position(available / 2);
        });
    }

    fn exit_compare(&self) {
        self.0.canvas.clear_lens();
        self.0.canvas.set_marker(None);
        if let Some(canvas) = self.0.compare_canvas.borrow().as_ref() {
            canvas.clear_lens();
            canvas.set_marker(None);
        }
        if let Some(paned) = self.0.compare_paned.borrow_mut().take() {
            self.0.toasts.set_child(None::<&gtk::Widget>);
            paned.set_start_child(None::<&gtk::Widget>);
            paned.set_end_child(None::<&gtk::Widget>);
            self.0.toasts.set_child(Some(&self.0.scrolled));
        }
        self.0.compare_scrolled.replace(None);
        self.0.compare_canvas.replace(None);
        self.0.compare_rendered.replace(None);
        self.0.canvas.set_tooltip_text(Some("Image canvas"));
        self.0.canvas.set_accessible_label("Image canvas");
        let this = self.clone();
        glib::idle_add_local_once(move || this.update_minimap());
    }

    fn connect_compare_adjustments(&self, compare: &gtk::ScrolledWindow) {
        for (source, target) in [
            (self.0.scrolled.hadjustment(), compare.hadjustment()),
            (self.0.scrolled.vadjustment(), compare.vadjustment()),
            (compare.hadjustment(), self.0.scrolled.hadjustment()),
            (compare.vadjustment(), self.0.scrolled.vadjustment()),
        ] {
            let this = self.clone();
            source.connect_value_changed(move |source| {
                if !this.0.compare_locked.get() || this.0.syncing_compare.replace(true) {
                    return;
                }
                sync_adjustment(source, &target);
                this.0.syncing_compare.set(false);
            });
        }
    }

    fn toggle_single_image_lens(&self) {
        if self.0.edit_button.is_active() {
            return;
        }
        self.0
            .lens_button
            .set_active(!self.0.lens_button.is_active());
    }

    fn set_single_image_lens_active(&self, active: bool) {
        if active && self.0.edit_button.is_active() {
            self.0.lens_button.set_active(false);
            return;
        }
        if self.0.canvas.texture().is_none() {
            if active {
                self.0.lens_button.set_active(false);
                self.0
                    .toasts
                    .add_toast(adw::Toast::new("Open an image before using the lens"));
            }
            return;
        }
        self.0.lens_active.set(active);
        self.0.canvas.set_cursor_from_name(active.then_some("none"));
        if !active {
            self.0.canvas.clear_lens();
        }
    }

    fn connect_single_image_lens(&self) {
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion({
            let this = self.clone();
            move |_, x, y| {
                if !this.0.lens_active.get() || this.0.compare_canvas.borrow().is_some() {
                    return;
                }
                let Some(texture) = this.0.canvas.texture() else {
                    return;
                };
                let Some((normalized_x, normalized_y)) = this.0.canvas.normalized_at(x, y) else {
                    this.0.canvas.clear_lens();
                    return;
                };
                this.0.canvas.set_lens(
                    &texture,
                    normalized_x,
                    normalized_y,
                    this.0.lens_diameter.get(),
                    4.0,
                );
            }
        });
        motion.connect_leave({
            let canvas = self.0.canvas.clone();
            move |_| canvas.clear_lens()
        });
        self.0.canvas.add_controller(motion);
    }

    fn connect_lens(
        &self,
        source: &ImageCanvas,
        target: &ImageCanvas,
        target_texture: &gtk::gdk::Texture,
    ) {
        let Some(source_texture) = source.texture() else {
            return;
        };
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion({
            let this = self.clone();
            let source = source.clone();
            let source_texture = source_texture.clone();
            let target = target.clone();
            let target_texture = target_texture.clone();
            move |_, x, y| {
                if this.0.edit_button.is_active() {
                    source.clear_lens();
                    target.clear_lens();
                    return;
                }
                let Some((normalized_x, normalized_y)) = source.normalized_at(x, y) else {
                    source.clear_lens();
                    target.clear_lens();
                    return;
                };
                let magnification = this.0.lens_magnification.get();
                source.set_lens(
                    &source_texture,
                    normalized_x,
                    normalized_y,
                    this.0.lens_diameter.get(),
                    magnification,
                );
                target.set_lens(
                    &target_texture,
                    normalized_x,
                    normalized_y,
                    this.0.lens_diameter.get(),
                    magnification,
                );
            }
        });
        motion.connect_leave({
            let source = source.clone();
            let target = target.clone();
            move |_| {
                source.clear_lens();
                target.clear_lens();
            }
        });
        source.add_controller(motion);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.connect_scroll({
            let this = self.clone();
            move |controller, _, dy| {
                let state = controller.current_event_state();
                if state.contains(gtk::gdk::ModifierType::ALT_MASK) {
                    let next = (this.0.lens_magnification.get()
                        * if dy < 0.0 { 1.1 } else { 1.0 / 1.1 })
                    .clamp(1.0, 16.0);
                    this.0.lens_magnification.set(next);
                    this.0.settings.set_compare_lens_magnification(next);
                    glib::Propagation::Stop
                } else if state.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
                    let next = (this.0.lens_diameter.get() + if dy < 0.0 { 12.0 } else { -12.0 })
                        .clamp(64.0, 512.0);
                    this.0.lens_diameter.set(next);
                    this.0.settings.set_compare_lens_size(next);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        });
        source.add_controller(scroll);
    }

    fn set_zoom(&self, zoom: f64) {
        self.0.canvas.set_zoom(zoom);
        self.0
            .zoom_label
            .set_label(&format!("{:.0}%", self.0.canvas.zoom() * 100.0));
        self.0.settings.set_last_zoom(self.0.canvas.zoom());
        if self.0.compare_locked.get()
            && let Some(compare) = self.0.compare_canvas.borrow().as_ref()
        {
            compare.set_zoom(zoom);
        }
        self.update_minimap();
    }

    fn install_minimap(&self) {
        let click = gtk::GestureClick::new();
        click.set_button(1);
        click.connect_pressed({
            let this = self.clone();
            move |_, _, x, y| this.pan_from_minimap(x, y)
        });
        self.0.minimap.add_controller(click);
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        let drag_start = Rc::new(Cell::new((0.0, 0.0)));
        drag.connect_drag_begin({
            let drag_start = drag_start.clone();
            move |_, x, y| drag_start.set((x, y))
        });
        drag.connect_drag_update({
            let this = self.clone();
            let drag_start = drag_start.clone();
            move |_, dx, dy| {
                let (x, y) = drag_start.get();
                this.pan_from_minimap(x + dx, y + dy);
            }
        });
        self.0.minimap.add_controller(drag);
        for adjustment in [self.0.scrolled.hadjustment(), self.0.scrolled.vadjustment()] {
            let this = self.clone();
            adjustment.connect_value_changed(move |_| this.update_minimap());
        }
        self.0.scrolled.connect_notify_local(Some("width"), {
            let this = self.clone();
            move |_, _| this.update_minimap()
        });
        self.0.scrolled.connect_notify_local(Some("height"), {
            let this = self.clone();
            move |_, _| this.update_minimap()
        });
    }

    fn update_minimap(&self) {
        let horizontal = self.0.scrolled.hadjustment();
        let vertical = self.0.scrolled.vadjustment();
        let horizontal_overflows = horizontal.upper() - horizontal.lower() > horizontal.page_size();
        let vertical_overflows = vertical.upper() - vertical.lower() > vertical.page_size();
        self.0.minimap.set_visible(
            self.0.canvas.texture().is_some() && (horizontal_overflows || vertical_overflows),
        );
        let content_width = (horizontal.upper() - horizontal.lower()).max(1.0);
        let content_height = (vertical.upper() - vertical.lower()).max(1.0);
        self.0.minimap.set_texture(self.0.canvas.texture().as_ref());
        self.0.minimap.set_viewport(Some((
            ((horizontal.value() - horizontal.lower()) / content_width) as f32,
            ((vertical.value() - vertical.lower()) / content_height) as f32,
            (horizontal.page_size() / content_width) as f32,
            (vertical.page_size() / content_height) as f32,
        )));
    }

    fn pan_from_minimap(&self, x: f64, y: f64) {
        let Some(image_bounds) = self.0.minimap.image_bounds() else {
            return;
        };
        let normalized_x =
            ((x as f32 - image_bounds.x()) / image_bounds.width().max(1.0)).clamp(0.0, 1.0) as f64;
        let normalized_y =
            ((y as f32 - image_bounds.y()) / image_bounds.height().max(1.0)).clamp(0.0, 1.0) as f64;
        let horizontal = self.0.scrolled.hadjustment();
        let vertical = self.0.scrolled.vadjustment();
        let horizontal_range =
            (horizontal.upper() - horizontal.lower() - horizontal.page_size()).max(0.0);
        let vertical_range = (vertical.upper() - vertical.lower() - vertical.page_size()).max(0.0);
        let horizontal_target =
            normalized_x * (horizontal.upper() - horizontal.lower()) - horizontal.page_size() / 2.0;
        let vertical_target =
            normalized_y * (vertical.upper() - vertical.lower()) - vertical.page_size() / 2.0;
        horizontal.set_value(horizontal.lower() + horizontal_target.clamp(0.0, horizontal_range));
        vertical.set_value(vertical.lower() + vertical_target.clamp(0.0, vertical_range));
    }

    fn zoom_at(&self, factor: f64, position: Option<(f64, f64)>) {
        let old_zoom = self.0.canvas.zoom();
        let new_zoom = (old_zoom * factor).clamp(0.01, 64.0);
        let applied_factor = new_zoom / old_zoom;
        let horizontal = self.0.scrolled.hadjustment();
        let vertical = self.0.scrolled.vadjustment();
        let (content_x, content_y) = position.unwrap_or((
            horizontal.value() + horizontal.page_size() / 2.0,
            vertical.value() + vertical.page_size() / 2.0,
        ));
        let viewport_x = content_x - horizontal.value();
        let viewport_y = content_y - vertical.value();
        self.set_zoom(new_zoom);
        glib::idle_add_local_once(move || {
            horizontal.set_value(content_x * applied_factor - viewport_x);
            vertical.set_value(content_y * applied_factor - viewport_y);
        });
    }

    fn navigate(&self, forward: bool) {
        if self
            .0
            .document
            .borrow()
            .as_ref()
            .is_some_and(Document::is_dirty)
        {
            let this = self.clone();
            self.confirm_discard("Discard unsaved edits and open another image?", move || {
                if let Some(document) = this.0.document.borrow_mut().as_mut() {
                    document.restore_original();
                }
                this.navigate(forward);
            });
            return;
        }
        let next = self.0.sequence.borrow_mut().as_mut().and_then(|sequence| {
            if forward {
                sequence.next_image().cloned()
            } else {
                sequence.previous().cloned()
            }
        });
        if let Some(file) = next {
            self.load(file);
        }
    }

    fn fit(&self, fill: bool) {
        let Some(texture) = self.0.canvas.texture() else {
            return;
        };
        let width = f64::from(self.0.scrolled.width().max(1));
        let height = f64::from(self.0.scrolled.height().max(1));
        let horizontal = width / f64::from(texture.width());
        let vertical = height / f64::from(texture.height());
        self.set_zoom(if fill {
            horizontal.max(vertical)
        } else {
            horizontal.min(vertical)
        });
    }

    fn install_gestures(&self) {
        let zoom = gtk::GestureZoom::new();
        let start_zoom = Rc::new(Cell::new(1.0));
        zoom.connect_begin({
            let canvas = self.0.canvas.clone();
            let start_zoom = start_zoom.clone();
            move |_, _| start_zoom.set(canvas.zoom())
        });
        zoom.connect_scale_changed({
            let this = self.clone();
            move |_, scale| this.set_zoom(start_zoom.get() * scale)
        });
        self.0.canvas.add_controller(zoom);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.connect_scroll({
            let this = self.clone();
            move |controller, _dx, dy| {
                if controller
                    .current_event_state()
                    .contains(gtk::gdk::ModifierType::CONTROL_MASK)
                {
                    let position = controller
                        .current_event()
                        .and_then(|event| event.position());
                    let factor = if dy < 0.0 { 1.25 } else { 0.8 };
                    this.zoom_at(factor, position);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        });
        self.0.canvas.add_controller(scroll);

        let edit_cursor = gtk::EventControllerMotion::new();
        edit_cursor.connect_motion({
            let this = self.clone();
            move |_, x, y| {
                if this.0.lens_active.get() {
                    this.0.canvas.set_cursor_from_name(Some("none"));
                    return;
                }
                if !this.0.edit_button.is_active() {
                    return;
                }
                let cursor = this
                    .0
                    .edit_crop
                    .borrow()
                    .and_then(|crop| this.0.canvas.crop_display_bounds(crop))
                    .map_or("default", |rect| {
                        edit_resize_cursor(rect, x as f32, y as f32)
                    });
                this.0.canvas.set_cursor_from_name(Some(cursor));
            }
        });
        edit_cursor.connect_leave({
            let this = self.clone();
            move |_| {
                this.0
                    .canvas
                    .set_cursor_from_name(this.0.lens_active.get().then_some("none"))
            }
        });
        self.0.canvas.add_controller(edit_cursor);

        let edit_drag = gtk::GestureDrag::new();
        edit_drag.set_button(1);
        edit_drag.connect_drag_begin({
            let this = self.clone();
            move |gesture, x, y| {
                if !this.0.edit_button.is_active() {
                    return;
                }
                let Some(crop) = *this.0.edit_crop.borrow() else {
                    return;
                };
                let (screen_x, screen_y) = (x, y);
                let Some(rect) = this.0.canvas.crop_display_bounds(crop) else {
                    return;
                };
                let (left, right, top, bottom) =
                    edit_edge_hit(rect, screen_x as f32, screen_y as f32);
                if !(left || right || top || bottom) {
                    return;
                }
                let scale = (left || right) && (top || bottom);
                gesture.set_state(gtk::EventSequenceState::Claimed);
                this.0.edit_scale.set(1.0);
                this.0.edit_drag.set(Some(EditDrag {
                    crop,
                    start_screen_x: screen_x,
                    start_screen_y: screen_y,
                    scale,
                    anchor_x: if left {
                        f64::from(rect.x() + rect.width())
                    } else {
                        f64::from(rect.x())
                    },
                    anchor_y: if top {
                        f64::from(rect.y() + rect.height())
                    } else {
                        f64::from(rect.y())
                    },
                    start_width: f64::from(rect.width()),
                    start_height: f64::from(rect.height()),
                    left,
                    right,
                    top,
                    bottom,
                }));
            }
        });
        edit_drag.connect_drag_update({
            let this = self.clone();
            move |_, dx, dy| {
                let Some(drag) = this.0.edit_drag.get() else {
                    return;
                };
                if drag.scale {
                    let x = drag.start_screen_x + dx;
                    let y = drag.start_screen_y + dy;
                    let scale = corner_scale(drag, x, y);
                    this.0.edit_scale.set(scale);
                    this.0.canvas.set_preview_scale(scale as f32);
                    return;
                }
                let Some((x, y)) = this
                    .0
                    .canvas
                    .pixel_at(drag.start_screen_x + dx, drag.start_screen_y + dy)
                else {
                    return;
                };
                let mut crop = drag.crop;
                if drag.left {
                    let right = crop.x + crop.width;
                    crop.x = x.min(right.saturating_sub(1));
                    crop.width = right - crop.x;
                }
                if drag.right {
                    crop.width = x.saturating_sub(crop.x).clamp(1, crop.image_width - crop.x);
                }
                if drag.top {
                    let bottom = crop.y + crop.height;
                    crop.y = y.min(bottom.saturating_sub(1));
                    crop.height = bottom - crop.y;
                }
                if drag.bottom {
                    crop.height = y
                        .saturating_sub(crop.y)
                        .clamp(1, crop.image_height - crop.y);
                }
                this.0.edit_crop.replace(Some(crop));
                this.0.canvas.set_crop_overlay(Some(crop));
            }
        });
        edit_drag.connect_drag_end({
            let this = self.clone();
            move |_, _, _| {
                let Some(drag) = this.0.edit_drag.take() else {
                    return;
                };
                if drag.scale {
                    this.0.canvas.set_preview_scale(1.0);
                    let scale = this.0.edit_scale.replace(1.0);
                    let width = (f64::from(drag.crop.image_width) * scale)
                        .round()
                        .clamp(1.0, f64::from(u32::MAX)) as u32;
                    let height = (f64::from(drag.crop.image_height) * scale)
                        .round()
                        .clamp(1.0, f64::from(u32::MAX)) as u32;
                    this.apply(Operation::Scale {
                        width,
                        height,
                        resampling: this.0.settings.scale_resampling(),
                    });
                    this.0.edit_button.set_active(false);
                    return;
                }
                let Some(crop) = *this.0.edit_crop.borrow() else {
                    return;
                };
                this.apply(Operation::Crop {
                    x: crop.x,
                    y: crop.y,
                    width: crop.width,
                    height: crop.height,
                });
                this.0.edit_button.set_active(false);
            }
        });
        self.0.canvas.add_controller(edit_drag);

        let pencil = gtk::GestureDrag::new();
        pencil.set_button(1);
        pencil.connect_drag_begin({
            let this = self.clone();
            move |_, x, y| {
                if !this.0.pencil_active.get() {
                    return;
                }
                this.0.pencil_start.set((x, y));
                let Some((x, y)) = this.0.canvas.pixel_at(x, y) else {
                    return;
                };
                this.0.pencil_points.replace(vec![BrushPoint {
                    x: x as f32 + 0.5,
                    y: y as f32 + 0.5,
                    pressure: 1.0,
                }]);
                this.preview_pencil_stroke();
            }
        });
        pencil.connect_drag_update({
            let this = self.clone();
            move |_, offset_x, offset_y| {
                if !this.0.pencil_active.get() {
                    return;
                }
                let (start_x, start_y) = this.0.pencil_start.get();
                let Some((x, y)) = this
                    .0
                    .canvas
                    .pixel_at(start_x + offset_x, start_y + offset_y)
                else {
                    return;
                };
                this.0.pencil_points.borrow_mut().push(BrushPoint {
                    x: x as f32 + 0.5,
                    y: y as f32 + 0.5,
                    pressure: 1.0,
                });
                this.preview_pencil_stroke();
            }
        });
        pencil.connect_drag_end({
            let this = self.clone();
            move |_, _, _| {
                if !this.0.pencil_active.get() {
                    return;
                }
                let points = this.0.pencil_points.take();
                if !points.is_empty() {
                    this.apply(Operation::Pencil(Stroke {
                        points,
                        color: this.0.pencil_color.get(),
                        width: 1.0,
                        opacity: 1.0,
                        hardness: 1.0,
                    }));
                }
            }
        });
        self.0.canvas.add_controller(pencil);

        let sampler = gtk::GestureClick::new();
        sampler.set_button(3);
        sampler.connect_pressed({
            let this = self.clone();
            move |gesture, _, x, y| {
                if !this.0.pencil_active.get() {
                    return;
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let pixel = this.0.canvas.pixel_at(x, y).and_then(|(x, y)| {
                    this.0
                        .rendered
                        .borrow()
                        .as_ref()
                        .and_then(|image| crate::tools::pencil::sample(image, x, y))
                });
                if let Some(color) = pixel {
                    this.0.pencil_color.set(color);
                    this.0.color_button.set_rgba(&u8_to_rgba(color));
                    this.0.toasts.add_toast(adw::Toast::new(&format!(
                        "Sampled #{:02X}{:02X}{:02X}{:02X} · rgba({}, {}, {}, {})",
                        color[0],
                        color[1],
                        color[2],
                        color[3],
                        color[0],
                        color[1],
                        color[2],
                        color[3]
                    )));
                }
            }
        });
        self.0.canvas.add_controller(sampler);

        let pan = gtk::GestureDrag::new();
        pan.set_button(2);
        let pan_start = Rc::new(Cell::new((0.0, 0.0)));
        pan.connect_drag_begin({
            let pan_start = pan_start.clone();
            let horizontal = self.0.scrolled.hadjustment();
            let vertical = self.0.scrolled.vadjustment();
            move |_, _, _| pan_start.set((horizontal.value(), vertical.value()))
        });
        pan.connect_drag_update({
            let horizontal = self.0.scrolled.hadjustment();
            let vertical = self.0.scrolled.vadjustment();
            move |_, x, y| {
                let (start_x, start_y) = pan_start.get();
                horizontal.set_value(start_x - x);
                vertical.set_value(start_y - y);
            }
        });
        self.0.canvas.add_controller(pan);
    }

    fn install_comparison_pencil_gestures(&self, canvas: &ImageCanvas) {
        let pencil = gtk::GestureDrag::new();
        pencil.set_button(1);
        pencil.connect_drag_begin({
            let this = self.clone();
            let canvas = canvas.clone();
            move |_, x, y| {
                if !this.0.pencil_active.get() || this.0.compare_rendered.borrow().is_none() {
                    return;
                }
                this.0.pencil_start.set((x, y));
                let Some((x, y)) = canvas.pixel_at(x, y) else {
                    return;
                };
                this.0.pencil_points.replace(vec![BrushPoint {
                    x: x as f32 + 0.5,
                    y: y as f32 + 0.5,
                    pressure: 1.0,
                }]);
                this.preview_comparison_pencil_stroke(&canvas);
            }
        });
        pencil.connect_drag_update({
            let this = self.clone();
            let canvas = canvas.clone();
            move |_, offset_x, offset_y| {
                if !this.0.pencil_active.get() {
                    return;
                }
                let (start_x, start_y) = this.0.pencil_start.get();
                let Some((x, y)) = canvas.pixel_at(start_x + offset_x, start_y + offset_y) else {
                    return;
                };
                this.0.pencil_points.borrow_mut().push(BrushPoint {
                    x: x as f32 + 0.5,
                    y: y as f32 + 0.5,
                    pressure: 1.0,
                });
                this.preview_comparison_pencil_stroke(&canvas);
            }
        });
        pencil.connect_drag_end({
            let this = self.clone();
            let canvas = canvas.clone();
            move |_, _, _| {
                if !this.0.pencil_active.get() {
                    return;
                }
                if !this.0.pencil_points.borrow().is_empty() {
                    this.commit_comparison_pencil_stroke(&canvas);
                }
                this.0.pencil_points.take();
            }
        });
        canvas.add_controller(pencil);

        let sampler = gtk::GestureClick::new();
        sampler.set_button(3);
        sampler.connect_pressed({
            let this = self.clone();
            let canvas = canvas.clone();
            move |gesture, _, x, y| {
                if !this.0.pencil_active.get() {
                    return;
                }
                let pixel = canvas.pixel_at(x, y).and_then(|(x, y)| {
                    this.0
                        .compare_rendered
                        .borrow()
                        .as_ref()
                        .and_then(|image| crate::tools::pencil::sample(image, x, y))
                });
                let Some(color) = pixel else {
                    return;
                };
                gesture.set_state(gtk::EventSequenceState::Claimed);
                this.0.pencil_color.set(color);
                this.0.color_button.set_rgba(&u8_to_rgba(color));
            }
        });
        canvas.add_controller(sampler);
    }

    fn install_state_persistence(&self) {
        self.0.window.connect_close_request({
            let this = self.clone();
            let settings = self.0.settings.clone();
            move |window| {
                settings.set_window_size(window.width(), window.height());
                settings.set_maximized(window.is_maximized());
                if this.0.close_approved.get()
                    || !this
                        .0
                        .document
                        .borrow()
                        .as_ref()
                        .is_some_and(Document::is_dirty)
                {
                    return glib::Propagation::Proceed;
                }
                let this_for_discard = this.clone();
                this.confirm_discard("Discard unsaved edits?", move || {
                    this_for_discard.0.close_approved.set(true);
                    this_for_discard.0.window.close();
                });
                glib::Propagation::Stop
            }
        });
    }

    fn confirm_discard(&self, heading: &str, on_discard: impl Fn() + 'static) {
        let dialog = adw::AlertDialog::builder()
            .heading(heading)
            .body("This cannot be undone.")
            .close_response("cancel")
            .default_response("cancel")
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("discard", "Discard");
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.connect_response(Some("discard"), move |_, _| on_discard());
        dialog.present(Some(&self.0.window));
    }
}

fn build_header(title: &adw::WindowTitle) -> HeaderWidgets {
    let header = adw::HeaderBar::builder().title_widget(title).build();
    let animation_controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    animation_controls.add_css_class("linked");
    animation_controls.set_visible(false);
    let previous_frame = button(
        "media-skip-backward-symbolic",
        "Previous Frame",
        "win.previous-frame",
    );
    let play = button(
        "media-playback-pause-symbolic",
        "Stop animation",
        "win.play-pause",
    );
    let next_frame = button(
        "media-skip-forward-symbolic",
        "Next Frame",
        "win.next-frame",
    );
    animation_controls.append(&previous_frame);
    animation_controls.append(&play);
    animation_controls.append(&next_frame);
    let previous = button("go-previous-symbolic", "Previous Image", "win.previous");
    let next = button("go-next-symbolic", "Next Image", "win.next");
    let pencil_button = toggle_button("xsi-edit-symbolic", "Toggle Pencil");
    let lens_button = toggle_button("edit-find-symbolic", "Toggle 4× Lens");
    let edit_button = toggle_button("edit-cut-symbolic", "Toggle Edit Mode");
    let color_button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    color_button.set_rgba(&u8_to_rgba([0, 0, 0, 255]));
    color_button.set_tooltip_text(Some("Pencil color"));
    header.pack_start(&animation_controls);
    header.pack_start(&previous);
    header.pack_start(&next);
    header.pack_end(&menu_button());
    header.pack_end(&button("media-floppy-symbolic", "Save As", "win.save-as"));
    header.pack_end(&edit_button);
    header.pack_end(&color_button);
    header.pack_end(&pencil_button);
    header.pack_end(&lens_button);
    header.pack_end(&button(
        "view-dual-symbolic",
        "Compare Images",
        "win.compare",
    ));
    HeaderWidgets {
        header,
        animation_controls,
        animation_play_button: play,
        pencil_button,
        lens_button,
        color_button,
        edit_button,
    }
}

fn button(icon: &str, tooltip: &str, action: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .action_name(action)
        .build()
}

fn toggle_button(icon: &str, tooltip: &str) -> gtk::ToggleButton {
    gtk::ToggleButton::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .build()
}

fn menu_button() -> gtk::MenuButton {
    let menu = gio::Menu::new();
    menu.append(Some("Open…"), Some("win.open"));
    menu.append(Some("Save"), Some("win.save"));
    menu.append(Some("Save As…"), Some("win.save-as"));
    menu.append(Some("Image Properties"), Some("win.properties"));
    menu.append(Some("Preferences"), Some("win.preferences"));
    menu.append(Some("Keyboard Shortcuts"), Some("win.shortcuts"));
    menu.append(Some("About Image Viewer"), Some("win.about"));
    gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Main Menu")
        .menu_model(&menu)
        .build()
}

fn lens_size_index(diameter: f32) -> u32 {
    if diameter < 230.0 {
        0
    } else if diameter < 340.0 {
        1
    } else {
        2
    }
}

fn u8_to_rgba(color: [u8; 4]) -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::new(
        f32::from(color[0]) / 255.0,
        f32::from(color[1]) / 255.0,
        f32::from(color[2]) / 255.0,
        f32::from(color[3]) / 255.0,
    )
}

fn rgba_to_u8(color: gtk::gdk::RGBA) -> [u8; 4] {
    [
        (color.red() * 255.0).round() as u8,
        (color.green() * 255.0).round() as u8,
        (color.blue() * 255.0).round() as u8,
        (color.alpha() * 255.0).round() as u8,
    ]
}

fn spin(minimum: f64, maximum: f64, value: f64) -> gtk::SpinButton {
    let adjustment = gtk::Adjustment::new(value, minimum, maximum, 1.0, 10.0, 0.0);
    gtk::SpinButton::builder()
        .adjustment(&adjustment)
        .numeric(true)
        .build()
}

fn export_options(path: &Path, settings: &Settings) -> Option<ExportOptions> {
    match path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => Some(ExportOptions::Png(PngOptions {
            compression: settings.png_compression(),
            preserve_metadata: settings.preserve_metadata(),
            convert_to_srgb: false,
        })),
        Some("jpg" | "jpeg") => Some(ExportOptions::Jpeg(JpegOptions {
            quality: settings.jpeg_quality(),
            background: settings.jpeg_background(),
            preserve_metadata: settings.preserve_metadata(),
        })),
        _ => None,
    }
}

fn texture_from_rgba(image: &image::RgbaImage) -> Result<gtk::gdk::Texture, String> {
    let width = i32::try_from(image.width()).map_err(|_| "Image width is too large".to_owned())?;
    let height =
        i32::try_from(image.height()).map_err(|_| "Image height is too large".to_owned())?;
    let stride = usize::try_from(u64::from(image.width()) * 4)
        .map_err(|_| "Image stride is too large".to_owned())?;
    let bytes = glib::Bytes::from_owned(image.as_raw().clone());
    Ok(gtk::gdk::MemoryTexture::new(
        width,
        height,
        gtk::gdk::MemoryFormat::R8g8b8a8,
        &bytes,
        stride,
    )
    .upcast())
}

fn rgba_from_texture(texture: &gtk::gdk::Texture) -> Option<image::RgbaImage> {
    let width = u32::try_from(texture.width()).ok()?;
    let height = u32::try_from(texture.height()).ok()?;
    let mut downloader = gtk::gdk::TextureDownloader::new(texture);
    downloader.set_format(gtk::gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let row_bytes = usize::try_from(u64::from(width).checked_mul(4)?).ok()?;
    let expected_bytes = stride.checked_mul(usize::try_from(height).ok()?)?;
    if stride < row_bytes || bytes.len() < expected_bytes {
        return None;
    }
    let mut pixels = Vec::with_capacity(row_bytes.checked_mul(usize::try_from(height).ok()?)?);
    for row in bytes.as_ref().chunks_exact(stride).take(height as usize) {
        pixels.extend_from_slice(&row[..row_bytes]);
    }
    image::RgbaImage::from_raw(width, height, pixels)
}

fn sync_adjustment(source: &gtk::Adjustment, target: &gtk::Adjustment) {
    let source_range = (source.upper() - source.page_size()).max(0.0);
    let target_range = (target.upper() - target.page_size()).max(0.0);
    let normalized = if source_range <= f64::EPSILON {
        0.0
    } else {
        source.value() / source_range
    };
    target.set_value((normalized * target_range).clamp(0.0, target_range));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_frame_uses_directional_resize_cursors() {
        let rect = gtk::graphene::Rect::new(20.0, 30.0, 100.0, 80.0);

        assert_eq!(edit_resize_cursor(rect, 20.0, 30.0), "nwse-resize");
        assert_eq!(edit_resize_cursor(rect, 120.0, 30.0), "nesw-resize");
        assert_eq!(edit_resize_cursor(rect, 70.0, 30.0), "ns-resize");
        assert_eq!(edit_resize_cursor(rect, 20.0, 70.0), "ew-resize");
        assert_eq!(edit_resize_cursor(rect, 20.0, 10.0), "default");
    }

    #[test]
    fn corner_drag_preserves_aspect_ratio_scale() {
        let drag = EditDrag {
            crop: CropOverlay {
                x: 0,
                y: 0,
                width: 200,
                height: 100,
                image_width: 200,
                image_height: 100,
            },
            start_screen_x: 0.0,
            start_screen_y: 0.0,
            scale: true,
            anchor_x: 200.0,
            anchor_y: 100.0,
            start_width: 200.0,
            start_height: 100.0,
            left: true,
            right: false,
            top: true,
            bottom: false,
        };

        assert_eq!(corner_scale(drag, -100.0, -20.0), 1.5);
        assert_eq!(corner_scale(drag, 160.0, 80.0), 0.2);
    }

    #[test]
    fn downloaded_comparison_texture_keeps_rgba_pixels() {
        let image = image::RgbaImage::from_raw(1, 1, vec![12, 34, 56, 78]).unwrap();
        let texture = texture_from_rgba(&image).unwrap();

        assert_eq!(rgba_from_texture(&texture), Some(image));
    }
}
