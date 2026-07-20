use std::cell::{Cell, RefCell};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use adw::prelude::{AdwDialogExt, AlertDialogExt, ComboRowExt, PreferencesGroupExt};
use gio::prelude::*;
use gtk::prelude::*;
use libadwaita as adw;
use object_detector::{DetectorType, ModelScale, ObjectDetector};
use palette::FromColor as _;

use crate::canvas::{Background, CropOverlay, ImageCanvas, MiniMap, ZoomFilter};
use crate::compare::{SplitOrientation, choose_split};
use crate::document::{
    BrushPoint, CancellationToken, Document, Operation, Resampling, Rotation, Stroke,
};
use crate::export::{ExportOptions, JpegOptions, PngOptions};
use crate::image::{
    AnimationFrame, DecodeLimits, decode_animation, decode_headless, decode_memory, load_preview,
};
use crate::navigation::{DirectorySequence, find_matching_file};
use crate::settings::{ColorFormat, Settings};

#[derive(Clone)]
pub struct ViewerWindow(Rc<WindowState>);

struct HeaderWidgets {
    header: adw::HeaderBar,
    animation_controls: gtk::Box,
    animation_play_button: gtk::Button,
    scale_button: gtk::ToggleButton,
    measurement_button: gtk::ToggleButton,
    selection_button: gtk::ToggleButton,
    color_picker_button: gtk::ToggleButton,
    pencil_button: gtk::ToggleButton,
    lens_button: gtk::ToggleButton,
    color_button: gtk::ColorDialogButton,
    edit_button: gtk::ToggleButton,
}

#[derive(Clone)]
struct ExportSnapshot {
    document: Document,
    operations: Arc<[Operation]>,
    source_file: Option<gio::File>,
    load_generation: u64,
}

#[derive(Clone, Copy)]
struct EditDrag {
    crop: CropOverlay,
    start_screen_x: f64,
    start_screen_y: f64,
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
}

#[derive(Clone, Copy)]
struct SelectionDrag {
    start: (u32, u32),
    current: (u32, u32),
    start_screen: (f64, f64),
    image_dimensions: (u32, u32),
}

#[derive(Clone, Copy)]
struct MeasurementDrag {
    start: (u32, u32),
    current: (u32, u32),
    start_screen: (f64, f64),
    image_dimensions: (u32, u32),
}

#[derive(Clone, Copy)]
struct ZoomGestureAnchor {
    start_zoom: f64,
    content_x: f64,
    content_y: f64,
    horizontal_value: f64,
    vertical_value: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompareLensSource {
    Primary,
    Comparison,
}

#[derive(Default)]
struct PendingDirectoryChanges {
    refresh_navigation: bool,
    current_changed: bool,
    current_removed: bool,
    current_renamed_to: Option<gio::File>,
}

fn merge_directory_change(
    pending: &mut PendingDirectoryChanges,
    current: &gio::File,
    file: &gio::File,
    other_file: Option<&gio::File>,
    event: gio::FileMonitorEvent,
) {
    let source_is_current = file.equal(current);
    let source_is_parent = current.parent().is_some_and(|parent| file.equal(&parent));
    let target_is_current = other_file.is_some_and(|target| target.equal(current));
    pending.refresh_navigation = true;
    match event {
        gio::FileMonitorEvent::Changed | gio::FileMonitorEvent::ChangesDoneHint => {
            pending.current_changed |= source_is_current;
        }
        gio::FileMonitorEvent::AttributeChanged => {}
        gio::FileMonitorEvent::Created | gio::FileMonitorEvent::MovedIn => {
            pending.current_changed |= source_is_current || target_is_current;
        }
        gio::FileMonitorEvent::Deleted => {
            pending.current_removed |= source_is_current || source_is_parent;
        }
        gio::FileMonitorEvent::MovedOut => {
            if source_is_current && let Some(target) = other_file {
                pending.current_renamed_to = Some(target.clone());
            }
            pending.current_removed |= source_is_current || source_is_parent;
        }
        gio::FileMonitorEvent::Moved | gio::FileMonitorEvent::Renamed => {
            if source_is_current {
                pending.current_renamed_to = other_file.cloned();
                pending.current_removed = true;
            } else if source_is_parent {
                pending.current_renamed_to = other_file.and_then(|target_parent| {
                    current
                        .basename()
                        .map(|basename| target_parent.child(basename))
                });
                pending.current_removed = true;
            }
            pending.current_changed |= target_is_current;
        }
        gio::FileMonitorEvent::PreUnmount | gio::FileMonitorEvent::Unmounted => {
            pending.current_removed = true;
        }
        _ => {}
    }
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

fn pencil_can_activate(edit_active: bool, has_image: bool) -> bool {
    !edit_active && has_image
}

fn files_equal(left: &Option<gio::File>, right: &Option<gio::File>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.equal(right),
        (None, None) => true,
        _ => false,
    }
}

fn is_regular_file(file: &gio::File) -> bool {
    file.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
        == gio::FileType::Regular
}

fn is_directory(file: &gio::File) -> bool {
    file.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
        == gio::FileType::Directory
}

fn first_existing_folder(candidates: impl IntoIterator<Item = gio::File>) -> Option<gio::File> {
    candidates.into_iter().find(is_directory)
}

fn source_revision_changed(
    previous: Option<SystemTime>,
    current: Option<SystemTime>,
    is_local: bool,
) -> bool {
    if is_local { previous != current } else { true }
}

fn export_context_matches(
    current_load_generation: u64,
    exported_load_generation: u64,
    current_file: &Option<gio::File>,
    exported_file: &Option<gio::File>,
) -> bool {
    current_load_generation == exported_load_generation && files_equal(current_file, exported_file)
}

fn resize_crop(
    mut crop: CropOverlay,
    x: u32,
    y: u32,
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
) -> CropOverlay {
    if left {
        let right = crop.x + crop.width;
        crop.x = x.min(right.saturating_sub(1));
        crop.width = right - crop.x;
    }
    if right {
        crop.width = x.saturating_sub(crop.x).clamp(1, crop.image_width - crop.x);
    }
    if top {
        let bottom = crop.y + crop.height;
        crop.y = y.min(bottom.saturating_sub(1));
        crop.height = bottom - crop.y;
    }
    if bottom {
        crop.height = y
            .saturating_sub(crop.y)
            .clamp(1, crop.image_height - crop.y);
    }
    crop
}

fn selection_overlay(drag: SelectionDrag) -> Option<CropOverlay> {
    let bounds = crate::tools::selection::bounds_between(
        drag.start,
        drag.current,
        drag.image_dimensions.0,
        drag.image_dimensions.1,
    )
    .ok()?;
    Some(CropOverlay {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
        image_width: drag.image_dimensions.0,
        image_height: drag.image_dimensions.1,
    })
}

fn measurement_overlay(drag: MeasurementDrag) -> CropOverlay {
    CropOverlay {
        x: drag.start.0.min(drag.current.0),
        y: drag.start.1.min(drag.current.1),
        width: drag.start.0.abs_diff(drag.current.0),
        height: drag.start.1.abs_diff(drag.current.1),
        image_width: drag.image_dimensions.0,
        image_height: drag.image_dimensions.1,
    }
}

fn measurement_clipboard_text(measurement: CropOverlay) -> String {
    format!(
        "x:{},y:{},width:{},height:{}",
        measurement.x, measurement.y, measurement.width, measurement.height
    )
}

fn scaled_dimensions(width: u32, height: u32, target_width: u32) -> (u32, u32) {
    let width = width.max(1);
    let height = height.max(1);
    let target_width = target_width.max(1);
    let target_height = (u64::from(height) * u64::from(target_width) / u64::from(width))
        .max(1)
        .min(u64::from(u32::MAX)) as u32;
    (target_width, target_height)
}

fn folder_path(file: &gio::File) -> String {
    let Some(folder) = file.parent() else {
        return file.uri().to_string();
    };
    folder.path().map_or_else(
        || folder.uri().to_string(),
        |path| path.display().to_string(),
    )
}

fn compare_metadata(file: &gio::File, width: u32, height: u32) -> String {
    format!("{} · {width} × {height}", folder_path(file))
}

fn compare_metadata_label(file: &gio::File, width: u32, height: u32, xalign: f32) -> gtk::Label {
    let details = compare_metadata(file, width, height);
    gtk::Label::builder()
        .label(&details)
        .tooltip_text(&details)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .selectable(true)
        .xalign(xalign)
        .hexpand(true)
        .margin_start(8)
        .margin_end(8)
        .build()
}

fn relative_modified_time(modified: SystemTime, now: SystemTime) -> String {
    let (elapsed, is_past) = match now.duration_since(modified) {
        Ok(elapsed) => (elapsed, true),
        Err(error) => (error.duration(), false),
    };
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        return "just now".to_owned();
    }

    let (value, unit) = if seconds < 60 * 60 {
        (seconds / 60, "minute")
    } else if seconds < 24 * 60 * 60 {
        (seconds / (60 * 60), "hour")
    } else if seconds < 30 * 24 * 60 * 60 {
        (seconds / (24 * 60 * 60), "day")
    } else if seconds < 365 * 24 * 60 * 60 {
        (seconds / (30 * 24 * 60 * 60), "month")
    } else {
        (seconds / (365 * 24 * 60 * 60), "year")
    };
    let unit = if value == 1 {
        unit.to_owned()
    } else {
        format!("{unit}s")
    };
    if is_past {
        format!("{value} {unit} ago")
    } else {
        format!("in {value} {unit}")
    }
}

fn image_subtitle(
    dimensions: (u32, u32),
    zoom: f64,
    modified: Option<SystemTime>,
    now: SystemTime,
) -> String {
    let details = format!("{} × {} · {:.0}%", dimensions.0, dimensions.1, zoom * 100.0);
    match modified {
        Some(modified) => format!("{details} · {}", relative_modified_time(modified, now)),
        None => details,
    }
}

fn panel_fit_zoom(size: (i32, i32), dimensions: (i32, i32)) -> f64 {
    (f64::from(size.0.max(1)) / f64::from(dimensions.0.max(1)))
        .min(f64::from(size.1.max(1)) / f64::from(dimensions.1.max(1)))
}

fn usable_panel_size(size: (i32, i32)) -> bool {
    size.0 > 1 && size.1 > 1
}

fn comparison_zoom(primary_zoom: f64, fit_zooms: (f64, f64)) -> f64 {
    primary_zoom * fit_zooms.1 / fit_zooms.0.max(0.01)
}

fn scale_preview_zoom(source_width: u32, target_width: u32, source_zoom: f64) -> f64 {
    source_zoom * f64::from(source_width.max(1)) / f64::from(target_width.max(1))
}

fn anchored_adjustment_value(value: f64, content_position: f64, factor: f64) -> f64 {
    let viewport_position = content_position - value;
    content_position * factor - viewport_position
}

fn render_scale_preview(
    image: &image::RgbaImage,
    target_width: u32,
    resampling: Resampling,
) -> crate::error::Result<image::RgbaImage> {
    let (width, height) = scaled_dimensions(image.width(), image.height(), target_width);
    crate::tools::scale::resize(
        image,
        width,
        height,
        resampling,
        &CancellationToken::default(),
    )
}

struct WindowState {
    window: adw::ApplicationWindow,
    canvas: ImageCanvas,
    scrolled: gtk::ScrolledWindow,
    canvas_overlay: gtk::Overlay,
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
    source_modified: RefCell<Option<SystemTime>>,
    external_source_conflict: Cell<bool>,
    subtitle_ready: Cell<bool>,
    close_approved: Cell<bool>,
    pencil_active: Cell<bool>,
    pencil_points: RefCell<Vec<BrushPoint>>,
    pencil_start: Cell<(f64, f64)>,
    pencil_color: Cell<[u8; 4]>,
    measurement_button: gtk::ToggleButton,
    measurement_drag: Cell<Option<MeasurementDrag>>,
    selection_button: gtk::ToggleButton,
    selection_drag: Cell<Option<SelectionDrag>>,
    color_picker_button: gtk::ToggleButton,
    object_detector: Arc<Mutex<Option<ObjectDetector>>>,
    object_detection_running: Cell<bool>,
    mask_flash_generation: Cell<u64>,
    pencil_button: gtk::ToggleButton,
    lens_button: gtk::ToggleButton,
    color_button: gtk::ColorDialogButton,
    edit_button: gtk::ToggleButton,
    edit_crop: RefCell<Option<CropOverlay>>,
    edit_drag: Cell<Option<EditDrag>>,
    compare_canvas: RefCell<Option<ImageCanvas>>,
    compare_fit_zooms: Cell<Option<(f64, f64)>>,
    compare_rendered: RefCell<Option<image::RgbaImage>>,
    compare_file: RefCell<Option<gio::File>>,
    pending_comparison: RefCell<Option<gio::File>>,
    navigation_generation: Cell<u64>,
    compare_lens_source: Cell<Option<CompareLensSource>>,
    compare_scrolled: RefCell<Option<gtk::ScrolledWindow>>,
    compare_paned: RefCell<Option<gtk::Paned>>,
    compare_controllers: RefCell<Vec<(ImageCanvas, gtk::EventController)>>,
    compare_adjustment_handlers: RefCell<Vec<(gtk::Adjustment, glib::SignalHandlerId)>>,
    compare_locked: Cell<bool>,
    syncing_compare: Cell<bool>,
    lens_diameter: Cell<f32>,
    lens_magnification: Cell<f32>,
    lens_active: Cell<bool>,
    preview_cache: RefCell<lru::LruCache<String, crate::image::LoadedPreview>>,
    directory_monitor: RefCell<Option<gio::FileMonitor>>,
    pending_directory_changes: RefCell<PendingDirectoryChanges>,
    directory_refresh_scheduled: Cell<bool>,
    directory_refresh_generation: Cell<u64>,
    comparison_monitor: RefCell<Option<gio::FileMonitor>>,
    comparison_refresh_scheduled: Cell<bool>,
    comparison_renamed_to: RefCell<Option<gio::File>>,
    comparison_generation: Cell<u64>,
    comparison_cancellable: RefCell<Option<gio::Cancellable>>,
    external_reload_generation: Cell<u64>,
    prefetch_cancellables: RefCell<Vec<gio::Cancellable>>,
    animation_cancellable: RefCell<Option<gio::Cancellable>>,
    animation_frames: RefCell<Vec<AnimationFrame>>,
    animation_index: Cell<usize>,
    animation_paused: Cell<bool>,
    animation_controls: gtk::Box,
    animation_play_button: gtk::Button,
    export_cancellation: RefCell<Option<CancellationToken>>,
    export_generation: Cell<u64>,
    export_lock: Arc<Mutex<()>>,
    deletion_running: Cell<bool>,
    transform_controls: gtk::Box,
    pending_fit: Cell<Option<bool>>,
    fit_tick_scheduled: Cell<bool>,
    zoom_controls: gtk::Box,
    zoom_label: gtk::MenuButton,
    scale_button: gtk::ToggleButton,
    scale_controls: gtk::Box,
    scale_slider: gtk::Scale,
    scale_value_label: gtk::Label,
    scale_source: RefCell<Option<Arc<image::RgbaImage>>>,
    scale_source_zoom: Cell<f64>,
    scale_resampling: Cell<Resampling>,
    scale_preview_generation: Cell<u64>,
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
        transforms.append(&button(
            "window-close-symbolic",
            "Abort Crop",
            "win.cancel-tool",
        ));
        transforms.append(&button(
            "object-select-symbolic",
            "Apply Crop",
            "win.confirm-crop",
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
        let scale_controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        scale_controls.add_css_class("linked");
        scale_controls.set_visible(false);
        scale_controls.set_halign(gtk::Align::Fill);
        scale_controls.set_valign(gtk::Align::End);
        scale_controls.set_hexpand(true);
        scale_controls.set_margin_start(26);
        scale_controls.set_margin_end(26);
        scale_controls.set_margin_bottom(26);
        let scale_slider = gtk::Scale::with_range(gtk::Orientation::Horizontal, 1.0, 2.0, 1.0);
        scale_slider.set_hexpand(true);
        scale_slider.set_draw_value(false);
        scale_slider.set_tooltip_text(Some("Scaled width in pixels"));
        let scale_value_label = gtk::Label::new(Some("1 px"));
        scale_controls.append(&button(
            "window-close-symbolic",
            "Cancel Scale",
            "win.cancel-scale",
        ));
        scale_controls.append(&scale_value_label);
        scale_controls.append(&scale_slider);
        scale_controls.append(&button(
            "object-select-symbolic",
            "Apply Scale",
            "win.confirm-scale",
        ));
        canvas_overlay.add_overlay(&scale_controls);
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
            .title("Diorama")
            .subtitle("Open an image to begin")
            .build();
        let header_widgets = build_header(&title);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header_widgets.header);
        content.append(&toasts);
        let (width, height) = settings.window_size();
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title("Diorama")
            .default_width(width)
            .default_height(height)
            .content(&content)
            .build();
        if settings.maximized() {
            window.maximize();
        }

        let lens_diameter = settings.compare_lens_size();
        let lens_magnification = settings.compare_lens_magnification();
        let scale_resampling = settings.scale_resampling();
        let this = Self(Rc::new(WindowState {
            window,
            canvas,
            scrolled,
            canvas_overlay,
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
            external_source_conflict: Cell::new(false),
            subtitle_ready: Cell::new(false),
            close_approved: Cell::new(false),
            pencil_active: Cell::new(false),
            pencil_points: RefCell::new(Vec::new()),
            pencil_start: Cell::new((0.0, 0.0)),
            pencil_color: Cell::new([0, 0, 0, 255]),
            measurement_button: header_widgets.measurement_button,
            measurement_drag: Cell::new(None),
            selection_button: header_widgets.selection_button,
            selection_drag: Cell::new(None),
            color_picker_button: header_widgets.color_picker_button,
            object_detector: Arc::new(Mutex::new(None)),
            object_detection_running: Cell::new(false),
            mask_flash_generation: Cell::new(0),
            pencil_button: header_widgets.pencil_button,
            lens_button: header_widgets.lens_button,
            color_button: header_widgets.color_button,
            edit_button: header_widgets.edit_button,
            edit_crop: RefCell::new(None),
            edit_drag: Cell::new(None),
            compare_canvas: RefCell::new(None),
            compare_fit_zooms: Cell::new(None),
            compare_rendered: RefCell::new(None),
            compare_file: RefCell::new(None),
            pending_comparison: RefCell::new(None),
            navigation_generation: Cell::new(0),
            compare_lens_source: Cell::new(None),
            compare_scrolled: RefCell::new(None),
            compare_paned: RefCell::new(None),
            compare_controllers: RefCell::new(Vec::new()),
            compare_adjustment_handlers: RefCell::new(Vec::new()),
            compare_locked: Cell::new(true),
            syncing_compare: Cell::new(false),
            lens_diameter: Cell::new(lens_diameter),
            lens_magnification: Cell::new(lens_magnification),
            lens_active: Cell::new(false),
            preview_cache: RefCell::new(lru::LruCache::new(
                NonZeroUsize::new(3).expect("three is non-zero"),
            )),
            directory_monitor: RefCell::new(None),
            pending_directory_changes: RefCell::new(PendingDirectoryChanges::default()),
            directory_refresh_scheduled: Cell::new(false),
            directory_refresh_generation: Cell::new(0),
            comparison_monitor: RefCell::new(None),
            comparison_refresh_scheduled: Cell::new(false),
            comparison_renamed_to: RefCell::new(None),
            comparison_generation: Cell::new(0),
            comparison_cancellable: RefCell::new(None),
            external_reload_generation: Cell::new(0),
            prefetch_cancellables: RefCell::new(Vec::new()),
            animation_cancellable: RefCell::new(None),
            animation_frames: RefCell::new(Vec::new()),
            animation_index: Cell::new(0),
            animation_paused: Cell::new(false),
            animation_controls: header_widgets.animation_controls,
            animation_play_button: header_widgets.animation_play_button,
            transform_controls: transforms,
            pending_fit: Cell::new(None),
            fit_tick_scheduled: Cell::new(false),
            zoom_controls,
            zoom_label,
            scale_button: header_widgets.scale_button,
            scale_controls,
            scale_slider,
            scale_value_label,
            scale_source: RefCell::new(None),
            scale_source_zoom: Cell::new(1.0),
            scale_resampling: Cell::new(scale_resampling),
            scale_preview_generation: Cell::new(0),
            minimap,
            export_cancellation: RefCell::new(None),
            export_generation: Cell::new(0),
            export_lock: Arc::new(Mutex::new(())),
            deletion_running: Cell::new(false),
        }));
        this.install_actions();
        this.install_tool_controls();
        this.install_scale_controls();
        this.install_gestures();
        this.install_minimap();
        this.connect_single_image_lens();
        this.install_state_persistence();
        this.install_subtitle_clock();
        if let Some(file) = file {
            this.load(file);
        }
        this
    }

    pub fn present(&self) {
        self.0.window.present();
    }

    fn preferred_initial_folder(&self) -> Option<gio::File> {
        let mut candidates = Vec::with_capacity(3);
        if let Some(parent) = self
            .0
            .current_file
            .borrow()
            .as_ref()
            .and_then(gio::File::parent)
        {
            candidates.push(parent);
        }
        if let Some(folder) = self.0.settings.last_open_folder() {
            candidates.push(folder);
        }
        candidates.push(gio::File::for_path(glib::home_dir()));
        first_existing_folder(candidates)
    }

    fn load(&self, file: gio::File) {
        self.0
            .navigation_generation
            .set(self.0.navigation_generation.get().wrapping_add(1));
        self.0
            .external_reload_generation
            .set(self.0.external_reload_generation.get().wrapping_add(1));
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
        if let Some(previous) = self.0.render_cancellation.borrow_mut().take() {
            previous.cancel();
        }
        self.0
            .render_generation
            .set(self.0.render_generation.get().wrapping_add(1));
        self.0.animation_frames.borrow_mut().clear();
        self.0.animation_controls.set_visible(false);
        self.0.measurement_button.set_active(false);
        self.0.selection_button.set_active(false);
        self.0.color_picker_button.set_active(false);
        self.clear_mask_flash();
        self.0.scale_button.set_active(false);
        self.exit_compare();
        self.0
            .directory_refresh_generation
            .set(self.0.directory_refresh_generation.get().wrapping_add(1));
        self.0
            .pending_directory_changes
            .replace(PendingDirectoryChanges::default());
        self.0.directory_refresh_scheduled.set(false);
        self.0.sequence.replace(None);
        if let Some(monitor) = self.0.directory_monitor.borrow_mut().take() {
            monitor.cancel();
        }
        self.prefetch_neighbors();
        let cancellable = gio::Cancellable::new();
        self.0.cancellable.replace(Some(cancellable.clone()));
        let generation = self.0.load_generation.get().wrapping_add(1);
        self.0.load_generation.set(generation);
        self.0.current_file.replace(Some(file.clone()));
        self.monitor_directory();
        if let Some(parent) = file.parent().filter(is_directory) {
            self.0.settings.set_last_open_folder(&parent);
        }
        self.0.document.replace(None);
        self.0.rendered.replace(None);
        self.0.external_source_conflict.set(false);
        self.0.canvas.set_texture(None);
        self.0.subtitle_ready.set(false);
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
                    state.subtitle_ready.set(true);
                    state.canvas.set_texture(Some(&preview.texture));
                    ViewerWindow(state.clone()).fit(false);
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
                    if let Some(compare_file) = state.pending_comparison.borrow_mut().take() {
                        ViewerWindow(state.clone()).load_comparison(compare_file);
                    }
                    ViewerWindow(state).rebuild_navigation(file);
                }
                Err(error) => {
                    state.pending_comparison.borrow_mut().take();
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
                if let Some(folder) = this.preferred_initial_folder() {
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
        self.add_action("delete-file", {
            let this = self.clone();
            move || this.confirm_delete_current_file()
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
        self.add_action("measure", {
            let this = self.clone();
            move || {
                this.0
                    .measurement_button
                    .set_active(!this.0.measurement_button.is_active())
            }
        });
        self.add_action("confirm-crop", {
            let this = self.clone();
            move || this.confirm_crop()
        });
        self.add_action("scale-preview", {
            let this = self.clone();
            move || {
                this.0
                    .scale_button
                    .set_active(!this.0.scale_button.is_active())
            }
        });
        self.add_action("confirm-scale", {
            let this = self.clone();
            move || this.confirm_scale_preview()
        });
        self.add_action("cancel-scale", {
            let this = self.clone();
            move || this.0.scale_button.set_active(false)
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
                if this.0.measurement_button.is_active() {
                    this.0.measurement_button.set_active(false);
                    return;
                }
                if this.0.selection_button.is_active() {
                    this.0.selection_button.set_active(false);
                    return;
                }
                if this.0.color_picker_button.is_active() {
                    this.0.color_picker_button.set_active(false);
                    return;
                }
                if this.0.edit_button.is_active() {
                    this.0.edit_button.set_active(false);
                    return;
                }
                if this.0.scale_button.is_active() {
                    this.0.scale_button.set_active(false);
                    return;
                }
                this.0.pencil_button.set_active(false);
                this.0.pencil_points.borrow_mut().clear();
                this.0.canvas.set_accessible_label("Image canvas");
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
                    .application_name("Diorama")
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
            let this = self.clone();
            move || {
                this.0
                    .selection_button
                    .set_active(!this.0.selection_button.is_active())
            }
        });
    }

    fn add_action(&self, name: &str, callback: impl Fn() + 'static) {
        let action = gio::SimpleAction::new(name, None);
        action.connect_activate(move |_, _| callback());
        self.0.window.add_action(&action);
    }

    fn apply(&self, operation: Operation) {
        {
            let mut document = self.0.document.borrow_mut();
            let Some(document) = document.as_mut() else {
                self.0
                    .toasts
                    .add_toast(adw::Toast::new("Open an editable image first"));
                return;
            };
            document.apply(operation);
        }
        self.render_document();
    }

    fn install_tool_controls(&self) {
        self.0.measurement_button.connect_toggled({
            let this = self.clone();
            move |button| this.set_measurement_active(button.is_active())
        });
        self.0.selection_button.connect_toggled({
            let this = self.clone();
            move |button| this.set_selection_active(button.is_active())
        });
        self.0.color_picker_button.connect_toggled({
            let this = self.clone();
            move |button| this.set_color_picker_active(button.is_active())
        });
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

    fn install_scale_controls(&self) {
        self.0.scale_button.connect_toggled({
            let this = self.clone();
            move |button| this.set_scale_preview_active(button.is_active())
        });
        self.0.scale_slider.connect_value_changed({
            let this = self.clone();
            move |slider| {
                let width = slider.value().round() as u32;
                this.0.scale_value_label.set_label(&format!("{width} px"));
                this.schedule_scale_preview(width);
            }
        });
    }

    fn set_scale_preview_active(&self, active: bool) {
        if active {
            self.0.measurement_button.set_active(false);
            self.0.selection_button.set_active(false);
            self.0.color_picker_button.set_active(false);
            let Some(image) = self.0.rendered.borrow().clone() else {
                self.0.scale_button.set_active(false);
                self.0
                    .toasts
                    .add_toast(adw::Toast::new("Open an editable image first"));
                return;
            };
            self.0.edit_button.set_active(false);
            let width = image.width().max(1);
            self.0.scale_source.replace(Some(Arc::new(image)));
            self.0.scale_source_zoom.set(self.0.canvas.zoom());
            self.0
                .scale_slider
                .set_range(1.0, f64::from(width.saturating_mul(2)));
            self.0.scale_slider.set_value(f64::from(width));
            self.0.scale_value_label.set_label(&format!("{width} px"));
            self.0.scale_controls.set_visible(true);
            self.0.zoom_controls.set_visible(false);
            return;
        }
        self.0
            .scale_preview_generation
            .set(self.0.scale_preview_generation.get().wrapping_add(1));
        self.0.scale_controls.set_visible(false);
        self.0
            .zoom_controls
            .set_visible(!self.0.edit_button.is_active());
        self.set_scale_preview_zoom(self.0.scale_source_zoom.get());
        if let Some(image) = self.0.scale_source.borrow_mut().take()
            && let Ok(texture) = texture_from_rgba(&image)
        {
            self.0.canvas.set_texture(Some(&texture));
            self.update_minimap();
            self.update_subtitle();
        }
    }

    fn schedule_scale_preview(&self, target_width: u32) {
        let Some(source) = self.0.scale_source.borrow().clone() else {
            return;
        };
        let generation = self.0.scale_preview_generation.get().wrapping_add(1);
        self.0.scale_preview_generation.set(generation);
        let preview_zoom =
            scale_preview_zoom(source.width(), target_width, self.0.scale_source_zoom.get());
        if target_width == source.width() {
            if let Ok(texture) = texture_from_rgba(&source) {
                self.0.canvas.set_texture(Some(&texture));
                self.set_scale_preview_zoom(preview_zoom);
            }
            return;
        }
        let resampling = self.0.scale_resampling.get();
        let weak = Rc::downgrade(&self.0);
        glib::timeout_add_local_once(Duration::from_millis(50), move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            if state.scale_preview_generation.get() != generation {
                return;
            }
            let source = source.clone();
            let weak = Rc::downgrade(&state);
            glib::spawn_future_local(async move {
                let preview = gio::spawn_blocking(move || {
                    render_scale_preview(source.as_ref(), target_width, resampling)
                })
                .await;
                let Some(state) = weak.upgrade() else {
                    return;
                };
                if state.scale_preview_generation.get() != generation {
                    return;
                }
                match preview {
                    Ok(Ok(preview)) => match texture_from_rgba(&preview) {
                        Ok(texture) => {
                            state.canvas.set_texture(Some(&texture));
                            ViewerWindow(state).set_scale_preview_zoom(preview_zoom);
                        }
                        Err(error) => state.toasts.add_toast(adw::Toast::new(&error)),
                    },
                    Ok(Err(error)) => state.toasts.add_toast(adw::Toast::new(&error.to_string())),
                    Err(_) => state
                        .toasts
                        .add_toast(adw::Toast::new("Scale preview worker failed")),
                }
            });
        });
    }

    fn confirm_scale_preview(&self) {
        let Some(source) = self.0.scale_source.borrow().clone() else {
            return;
        };
        let width = self.0.scale_slider.value().round() as u32;
        let (_, height) = scaled_dimensions(source.width(), source.height(), width);
        let preview_zoom =
            scale_preview_zoom(source.width(), width, self.0.scale_source_zoom.get());
        self.0.scale_button.set_active(false);
        self.set_zoom(preview_zoom);
        self.apply(Operation::Scale {
            width,
            height,
            resampling: self.0.scale_resampling.get(),
        });
    }

    fn set_scale_preview_zoom(&self, zoom: f64) {
        self.0.canvas.set_zoom(zoom);
        self.0
            .zoom_label
            .set_label(&format!("{:.0}%", self.0.canvas.zoom() * 100.0));
        self.update_subtitle();
        self.update_minimap();
    }

    fn set_pencil_active(&self, active: bool) {
        if active {
            self.0.measurement_button.set_active(false);
            self.0.selection_button.set_active(false);
            self.0.color_picker_button.set_active(false);
        }
        if active
            && !pencil_can_activate(
                self.0.edit_button.is_active(),
                self.0.rendered.borrow().is_some(),
            )
        {
            self.0.pencil_button.set_active(false);
            if self.0.rendered.borrow().is_none() {
                self.0
                    .toasts
                    .add_toast(adw::Toast::new("Open an editable image first"));
            }
            return;
        }
        self.0.pencil_active.set(active);
        self.0.canvas.set_accessible_label(if active {
            "Image canvas, Pencil tool active"
        } else {
            "Image canvas"
        });
    }

    fn set_color_picker_active(&self, active: bool) {
        if active && self.0.rendered.borrow().is_none() {
            self.0.color_picker_button.set_active(false);
            self.0
                .toasts
                .add_toast(adw::Toast::new("Open an editable image first"));
            return;
        }
        if active {
            self.0.measurement_button.set_active(false);
            self.0.selection_button.set_active(false);
            self.0.scale_button.set_active(false);
            self.0.edit_button.set_active(false);
            self.0.pencil_button.set_active(false);
        }
        let cursor = if self.0.lens_active.get() || self.0.compare_canvas.borrow().is_some() {
            Some("none")
        } else {
            active.then_some("crosshair")
        };
        self.0.canvas.set_cursor_from_name(cursor);
        if let Some(canvas) = self.0.compare_canvas.borrow().as_ref() {
            canvas.set_cursor_from_name(cursor);
        }
        self.0.canvas.set_accessible_label(if active {
            "Image canvas, Color Picker tool active"
        } else {
            "Image canvas"
        });
    }

    fn apply_picked_color(&self, color: [u8; 4]) -> String {
        self.0.pencil_color.set(color);
        self.0.color_button.set_rgba(&u8_to_rgba(color));
        format_color(color, self.0.settings.color_picker_format())
    }

    fn copy_color_to_clipboard(&self, color: [u8; 4]) {
        let value = self.apply_picked_color(color);
        self.0.window.clipboard().set_text(&value);
        self.0
            .toasts
            .add_toast(adw::Toast::new(&format!("Copied {value}")));
    }

    fn copy_measurement_to_clipboard(&self, measurement: CropOverlay) {
        let text = measurement_clipboard_text(measurement);
        self.0.window.clipboard().set_text(&text);
        self.0
            .toasts
            .add_toast(adw::Toast::new(&format!("Copied {text}")));
    }

    fn set_selection_active(&self, active: bool) {
        if active && self.0.rendered.borrow().is_none() {
            self.0.selection_button.set_active(false);
            self.0
                .toasts
                .add_toast(adw::Toast::new("Open an editable image first"));
            return;
        }
        if active {
            self.0.measurement_button.set_active(false);
            self.0.color_picker_button.set_active(false);
            self.0.scale_button.set_active(false);
            self.0.edit_button.set_active(false);
            self.0.pencil_button.set_active(false);
            self.0.lens_button.set_active(false);
            self.0.canvas.clear_lens();
            if let Some(canvas) = self.0.compare_canvas.borrow().as_ref() {
                canvas.clear_lens();
                canvas.set_cursor_from_name(None);
            }
        } else {
            self.0.selection_drag.set(None);
            self.0.canvas.set_crop_overlay(None);
        }
        self.0
            .canvas
            .set_cursor_from_name(active.then_some("crosshair"));
        self.0.canvas.set_accessible_label(if active {
            "Image canvas, Select and Copy tool active"
        } else {
            "Image canvas"
        });
    }

    fn set_measurement_active(&self, active: bool) {
        if active && self.0.rendered.borrow().is_none() {
            self.0.measurement_button.set_active(false);
            self.0
                .toasts
                .add_toast(adw::Toast::new("Open an editable image first"));
            return;
        }
        if active {
            self.0.selection_button.set_active(false);
            self.0.color_picker_button.set_active(false);
            self.0.scale_button.set_active(false);
            self.0.edit_button.set_active(false);
            self.0.pencil_button.set_active(false);
            self.0.lens_button.set_active(false);
            self.0.canvas.clear_lens();
            if let Some(canvas) = self.0.compare_canvas.borrow().as_ref() {
                canvas.clear_lens();
                canvas.set_cursor_from_name(None);
            }
        } else {
            self.0.measurement_drag.set(None);
            self.0.canvas.set_measurement_cursor(None);
            self.0.canvas.set_measurement_overlay(None);
        }
        self.0.canvas.set_cursor_from_name(active.then_some("none"));
        self.0.canvas.set_accessible_label(if active {
            "Image canvas, Measuring tool active"
        } else {
            "Image canvas"
        });
    }

    fn complete_selection(&self, drag: SelectionDrag) {
        if drag.start == drag.current {
            let Some(image) = self.0.rendered.borrow().clone() else {
                return;
            };
            self.detect_and_copy_object(image, drag.start);
            return;
        }
        let image = self.0.rendered.borrow();
        let Some(image) = image.as_ref() else {
            return;
        };
        let result = crate::tools::selection::bounds_between(
            drag.start,
            drag.current,
            image.width(),
            image.height(),
        )
        .and_then(|bounds| {
            crate::tools::selection::crop(image, bounds).map(|selected| (bounds, selected))
        });
        match result {
            Ok((bounds, selected)) => self.copy_image_to_clipboard(
                &selected,
                &format!("Copied {} × {} selection", bounds.width, bounds.height),
            ),
            Err(error) => self.0.toasts.add_toast(adw::Toast::new(&error.to_string())),
        }
    }

    fn detect_and_copy_object(&self, image: image::RgbaImage, point: (u32, u32)) {
        if self.0.object_detection_running.replace(true) {
            self.0
                .toasts
                .add_toast(adw::Toast::new("Object detection is already running"));
            return;
        }
        let first_use = self
            .0
            .object_detector
            .lock()
            .map_or(true, |detector| detector.is_none());
        self.0.toasts.add_toast(adw::Toast::new(if first_use {
            "Preparing object detector — first use downloads the Medium model"
        } else {
            "Detecting object…"
        }));
        let detector = Arc::clone(&self.0.object_detector);
        let generation = self.0.load_generation.get();
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || {
                let mut detector = detector
                    .lock()
                    .map_err(|error| crate::error::AppError::AiInference(error.to_string()))?;
                if detector.is_none() {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| crate::error::AppError::AiInference(error.to_string()))?;
                    let loaded = runtime
                        .block_on(
                            ObjectDetector::from_hf(DetectorType::PromptFree)
                                .scale(ModelScale::Medium)
                                .include_mask(true)
                                .build(),
                        )
                        .map_err(|error| crate::error::AppError::AiInference(error.to_string()))?;
                    *detector = Some(loaded);
                }
                let detector = detector
                    .as_ref()
                    .ok_or(crate::error::AppError::AiModelUnavailable)?;
                crate::ai::select_object_at(detector, image, point.0, point.1)
            })
            .await;
            let Some(state) = weak.upgrade() else {
                return;
            };
            state.object_detection_running.set(false);
            if state.load_generation.get() != generation {
                return;
            }
            let this = ViewerWindow(state.clone());
            match result {
                Ok(Ok(Some(selected))) => {
                    this.flash_object_mask(&selected);
                    this.copy_image_to_clipboard(
                        &selected.image,
                        &format!(
                            "Copied {} object ({:.0}% confidence)",
                            selected.tag,
                            selected.score * 100.0
                        ),
                    );
                }
                Ok(Ok(None)) => state
                    .toasts
                    .add_toast(adw::Toast::new("No detected object contains that pixel")),
                Ok(Err(error)) => state.toasts.add_toast(adw::Toast::new(&format!(
                    "Object detection failed: {error}"
                ))),
                Err(_) => state
                    .toasts
                    .add_toast(adw::Toast::new("Object detection worker failed")),
            }
        });
    }

    fn copy_image_to_clipboard(&self, image: &image::RgbaImage, message: &str) {
        match texture_from_rgba(image) {
            Ok(texture) => {
                self.0.window.clipboard().set_texture(&texture);
                self.0.toasts.add_toast(adw::Toast::new(message));
            }
            Err(error) => self.0.toasts.add_toast(adw::Toast::new(&error)),
        }
    }

    fn flash_object_mask(&self, selected: &crate::ai::SelectedObject) {
        let Ok(texture) = texture_from_rgba(&selected.flash) else {
            return;
        };
        let generation = self.0.mask_flash_generation.get().wrapping_add(1);
        self.0.mask_flash_generation.set(generation);
        self.0.canvas.set_mask_flash(
            Some(&texture),
            CropOverlay {
                x: selected.bounds.x,
                y: selected.bounds.y,
                width: selected.bounds.width,
                height: selected.bounds.height,
                image_width: selected.image_dimensions.0,
                image_height: selected.image_dimensions.1,
            },
        );
        let weak = Rc::downgrade(&self.0);
        glib::timeout_add_local_once(Duration::from_millis(350), move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            if state.mask_flash_generation.get() == generation {
                state.canvas.clear_mask_flash();
            }
        });
    }

    fn clear_mask_flash(&self) {
        self.0
            .mask_flash_generation
            .set(self.0.mask_flash_generation.get().wrapping_add(1));
        self.0.canvas.clear_mask_flash();
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
            canvas.update_lens_texture(&texture);
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
        if active {
            self.0.measurement_button.set_active(false);
            self.0.selection_button.set_active(false);
            self.0.color_picker_button.set_active(false);
            self.0.scale_button.set_active(false);
            self.0.pencil_button.set_active(false);
        }
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
        self.0.scale_button.set_sensitive(!active);
        self.0.measurement_button.set_sensitive(!active);
        self.0.selection_button.set_sensitive(!active);
        self.0.color_picker_button.set_sensitive(!active);
        self.0.pencil_button.set_sensitive(!active);
        if !active {
            self.0.canvas.set_preview_scale(1.0);
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

    fn confirm_crop(&self) {
        if !self.0.edit_button.is_active() {
            return;
        }
        let Some(crop) = *self.0.edit_crop.borrow() else {
            return;
        };
        self.apply(Operation::Crop {
            x: crop.x,
            y: crop.y,
            width: crop.width,
            height: crop.height,
        });
        self.0.edit_button.set_active(false);
    }

    fn render_document(&self) {
        let Some(document) = self.0.document.borrow().clone() else {
            return;
        };
        self.render_candidate(document);
    }

    fn render_candidate(&self, document: Document) {
        if let Some(previous) = self.0.render_cancellation.borrow_mut().take() {
            previous.cancel();
        }
        let cancellation = CancellationToken::default();
        self.0
            .render_cancellation
            .replace(Some(cancellation.clone()));
        let generation = self.0.render_generation.get().wrapping_add(1);
        self.0.render_generation.set(generation);
        self.update_title();

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
                    let matches_live_document = state
                        .document
                        .borrow_mut()
                        .as_mut()
                        .is_some_and(|live| live.adopt_render_cache(&document));
                    if !matches_live_document {
                        return;
                    }
                    match texture_from_rgba(&rendered.pixels) {
                        Ok(texture) => {
                            state
                                .canvas
                                .set_auto_background_from_image(&rendered.pixels);
                            state.canvas.set_texture(Some(&texture));
                            state.rendered.replace(Some(rendered.pixels));
                            ViewerWindow(state.clone()).update_minimap();
                            ViewerWindow(state.clone()).update_subtitle();
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
        self.update_subtitle();
    }

    fn update_subtitle(&self) {
        if !self.0.subtitle_ready.get() {
            return;
        }
        let Some(texture) = self.0.canvas.texture() else {
            return;
        };
        let dimensions = (texture.width() as u32, texture.height() as u32);
        let modified = *self.0.source_modified.borrow();
        self.0.title.set_subtitle(&image_subtitle(
            dimensions,
            self.0.canvas.zoom(),
            modified,
            SystemTime::now(),
        ));
    }

    fn install_subtitle_clock(&self) {
        let weak = Rc::downgrade(&self.0);
        glib::timeout_add_local(Duration::from_secs(30), move || {
            let Some(state) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            ViewerWindow(state).update_subtitle();
            glib::ControlFlow::Continue
        });
    }

    fn save(&self, force_dialog: bool) {
        let Some(document) = self.0.document.borrow().clone() else {
            self.0
                .toasts
                .add_toast(adw::Toast::new("Open an editable image first"));
            return;
        };
        let current = self.0.current_file.borrow().clone();
        let snapshot = ExportSnapshot {
            operations: document.operations().into(),
            document,
            source_file: current.clone(),
            load_generation: self.0.load_generation.get(),
        };
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
            self.export_document(snapshot, path);
            return;
        }

        let mut builder = gtk::FileDialog::builder()
            .title("Save Image")
            .initial_name("image.png")
            .modal(true);
        if let Some(folder) = self.preferred_initial_folder() {
            builder = builder.initial_folder(&folder);
        }
        let dialog = builder.build();
        let parent = self.0.window.clone();
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(file) = dialog.save_future(Some(&parent)).await {
                if let Some(path) = file.path() {
                    this.show_export_options(snapshot, path);
                } else {
                    this.0.toasts.add_toast(adw::Toast::new(
                        "This location does not support atomic export",
                    ));
                }
            }
        });
    }

    fn export_document(&self, snapshot: ExportSnapshot, path: PathBuf) {
        let Some(options) = export_options(&path, &self.0.settings) else {
            self.0.toasts.add_toast(adw::Toast::new(
                "Choose a file name ending in .png, .jpg, or .jpeg",
            ));
            return;
        };
        self.export_document_with_options(snapshot, path, options, false);
    }

    fn show_export_options(&self, snapshot: ExportSnapshot, path: PathBuf) {
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
            this.export_document_with_options(snapshot.clone(), path.clone(), options, true);
            export_dialog.close();
        });
        dialog.present(Some(&self.0.window));
    }

    fn export_document_with_options(
        &self,
        snapshot: ExportSnapshot,
        path: PathBuf,
        options: ExportOptions,
        replace_current_file: bool,
    ) {
        if let Some(previous) = self.0.export_cancellation.borrow_mut().take() {
            previous.cancel();
        }
        let cancellation = CancellationToken::default();
        self.0
            .export_cancellation
            .replace(Some(cancellation.clone()));
        let generation = self.0.export_generation.get().wrapping_add(1);
        self.0.export_generation.set(generation);
        let worker_cancellation = cancellation.clone();
        let worker_path = path.clone();
        let export_lock = self.0.export_lock.clone();
        let ExportSnapshot {
            document,
            operations,
            source_file,
            load_generation,
        } = snapshot;
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
                let _export_guard = export_lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                worker_cancellation.check()?;
                let rendered = document.render(&worker_cancellation)?;
                crate::export::export(&rendered, &worker_path, &options, &worker_cancellation)
            })
            .await;
            let Some(state) = weak.upgrade() else {
                return;
            };
            if state.export_generation.get() != generation {
                return;
            }
            state.export_cancellation.borrow_mut().take();
            if !export_context_matches(
                state.load_generation.get(),
                load_generation,
                &state.current_file.borrow(),
                &source_file,
            ) {
                return;
            }
            match result {
                Ok(Ok(())) => {
                    if let Some(document) = state.document.borrow_mut().as_mut() {
                        document.mark_saved_at(operations);
                        if replace_current_file {
                            document.set_path(Some(path.clone()));
                        }
                    }
                    if replace_current_file {
                        let target = gio::File::for_path(&path);
                        if let Some(parent) = target.parent() {
                            state.settings.set_last_open_folder(&parent);
                        }
                        state.current_file.replace(Some(target.clone()));
                        ViewerWindow(state.clone()).rebuild_navigation(target);
                    }
                    state.source_modified.replace(
                        std::fs::metadata(&path)
                            .ok()
                            .and_then(|metadata| metadata.modified().ok()),
                    );
                    state.external_source_conflict.set(false);
                    let has_newer_edits = state
                        .document
                        .borrow()
                        .as_ref()
                        .is_some_and(Document::is_dirty);
                    state.toasts.add_toast(adw::Toast::new(if has_newer_edits {
                        "Image exported; newer edits remain unsaved"
                    } else {
                        "Image saved"
                    }));
                    ViewerWindow(state.clone()).update_title();
                }
                Ok(Err(error)) => state.toasts.add_toast(adw::Toast::new(&error.to_string())),
                Err(_) => state
                    .toasts
                    .add_toast(adw::Toast::new("Export worker failed")),
            }
        });
    }

    fn source_changed(&self, path: &Path) -> bool {
        if self.0.external_source_conflict.get() {
            return true;
        }
        let current = std::fs::metadata(path)
            .ok()
            .and_then(|metadata| metadata.modified().ok());
        source_revision_changed(*self.0.source_modified.borrow(), current, true)
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
                "Auto",
                "White",
                "Gray",
                "Black",
            ]))
            .selected(match self.0.canvas.background() {
                Background::Checkerboard => 0,
                Background::Auto => 1,
                Background::White => 2,
                Background::Gray => 3,
                Background::Black => 4,
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
            .selected(match self.0.scale_resampling.get() {
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
                1 => Background::Auto,
                2 => Background::White,
                3 => Background::Gray,
                4 => Background::Black,
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
            let scale_resampling = match resampling.selected() {
                0 => Resampling::Nearest,
                1 => Resampling::Linear,
                3 => Resampling::SeamCarving,
                _ => Resampling::Bicubic,
            };
            this.0.scale_resampling.set(scale_resampling);
            this.0.settings.set_scale_resampling(scale_resampling);
            if this.0.scale_button.is_active() {
                this.schedule_scale_preview(this.0.scale_slider.value().round() as u32);
            }
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
                    ("Delete Image", "Delete"),
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
                    ("Apply Crop", "Return"),
                    ("Cancel Crop", "Escape"),
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
        let color_format = adw::ComboRow::builder()
            .title("Copied color format")
            .subtitle("Format used by the Color Picker tool")
            .model(&gtk::StringList::new(&["Hex", "RGB(A)", "OKLab", "HSL"]))
            .selected(color_format_index(self.0.settings.color_picker_format()))
            .build();
        color_format.connect_selected_notify({
            let settings = self.0.settings.clone();
            move |row| settings.set_color_picker_format(color_format_at(row.selected()))
        });
        let dialog = adw::AlertDialog::builder()
            .heading("Image Properties")
            .body(body)
            .extra_child(&color_format)
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
                    let window = ViewerWindow(state.clone());
                    window.update_minimap();
                    window.update_subtitle();
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
        self.update_subtitle();
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

    fn rebuild_navigation(&self, file: gio::File) {
        self.0.sequence.replace(None);
        if let Some(monitor) = self.0.directory_monitor.borrow_mut().take() {
            monitor.cancel();
        }
        self.prefetch_neighbors();
        self.refresh_navigation(file, true);
    }

    fn refresh_navigation(&self, file: gio::File, restart_monitor: bool) {
        let fallback = self.0.settings.folder_sort();
        let expected_file = file.clone();
        let generation = self.0.directory_refresh_generation.get().wrapping_add(1);
        self.0.directory_refresh_generation.set(generation);
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let sequence =
                gio::spawn_blocking(move || DirectorySequence::build(&file, fallback)).await;
            let Some(state) = weak.upgrade() else {
                return;
            };
            if state.directory_refresh_generation.get() != generation
                || !files_equal(&state.current_file.borrow(), &Some(expected_file))
            {
                return;
            }
            match sequence {
                Ok(Ok(sequence)) => {
                    state.sequence.replace(Some(sequence));
                    let this = ViewerWindow(state.clone());
                    this.prefetch_neighbors();
                }
                Ok(Err(error)) => {
                    tracing::debug!(%error, "Directory navigation unavailable")
                }
                Err(_) => tracing::warn!("Directory navigation worker panicked"),
            }
            if restart_monitor {
                ViewerWindow(state).monitor_directory();
            }
        });
    }

    fn monitor_directory(&self) {
        if let Some(monitor) = self.0.directory_monitor.borrow_mut().take() {
            monitor.cancel();
        }
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
            move |_, file, other_file, event| {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                ViewerWindow(state).queue_directory_change(file, other_file, event);
            }
        });
        self.0.directory_monitor.replace(Some(monitor));
    }

    fn queue_directory_change(
        &self,
        file: &gio::File,
        other_file: Option<&gio::File>,
        event: gio::FileMonitorEvent,
    ) {
        let Some(current) = self.0.current_file.borrow().clone() else {
            return;
        };
        {
            let mut pending = self.0.pending_directory_changes.borrow_mut();
            merge_directory_change(&mut pending, &current, file, other_file, event);
        }
        if self.0.directory_refresh_scheduled.replace(true) {
            return;
        }
        let generation = self.0.directory_refresh_generation.get();
        let weak = Rc::downgrade(&self.0);
        glib::timeout_add_local_once(Duration::from_millis(250), move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            if state.directory_refresh_generation.get() != generation
                || !state.directory_refresh_scheduled.replace(false)
            {
                return;
            }
            ViewerWindow(state).process_directory_changes();
        });
    }

    fn process_directory_changes(&self) {
        let pending = self
            .0
            .pending_directory_changes
            .replace(PendingDirectoryChanges::default());
        let Some(current) = self.0.current_file.borrow().clone() else {
            return;
        };

        if let Some(target) = pending.current_renamed_to.filter(is_regular_file) {
            self.0.current_file.replace(Some(target.clone()));
            if let Some(document) = self.0.document.borrow_mut().as_mut() {
                document.set_path(target.path());
            }
            self.0.source_modified.replace(
                target
                    .path()
                    .and_then(|path| std::fs::metadata(path).ok())
                    .and_then(|metadata| metadata.modified().ok()),
            );
            if let Some(parent) = target.parent().filter(is_directory) {
                self.0.settings.set_last_open_folder(&parent);
            }
            self.update_title();
            self.0.toasts.add_toast(adw::Toast::new(
                "Image location updated after an external move",
            ));
            self.rebuild_navigation(target);
            return;
        }

        if pending.current_removed && !is_regular_file(&current) {
            let replacement = self.0.sequence.borrow().as_ref().and_then(|sequence| {
                sequence
                    .replacements_after_current_removed()
                    .find(is_regular_file)
            });
            let dirty = self
                .0
                .document
                .borrow()
                .as_ref()
                .is_some_and(Document::is_dirty);
            if !dirty && let Some(replacement) = replacement {
                self.0
                    .pending_comparison
                    .replace(self.0.compare_file.borrow().clone());
                self.load(replacement);
                self.0
                    .toasts
                    .add_toast(adw::Toast::new("The previous image was moved or deleted"));
                return;
            }
            self.0.sequence.replace(None);
            self.prefetch_neighbors();
            self.0.external_source_conflict.set(true);
            self.0.title.set_subtitle("File moved or deleted");
            self.0.toasts.add_toast(adw::Toast::new(if dirty {
                "The source file was moved or deleted; unsaved edits are still available via Save As"
            } else {
                "The current file was moved or deleted"
            }));
            return;
        }

        if pending.current_changed && is_regular_file(&current) {
            let current_modified = current
                .path()
                .and_then(|path| std::fs::metadata(path).ok())
                .and_then(|metadata| metadata.modified().ok());
            let changed = source_revision_changed(
                *self.0.source_modified.borrow(),
                current_modified,
                current.path().is_some(),
            );
            let dirty = self
                .0
                .document
                .borrow()
                .as_ref()
                .is_some_and(Document::is_dirty);
            if dirty {
                self.0.external_source_conflict.set(true);
                self.0.toasts.add_toast(adw::Toast::new(
                    "The source file changed externally; unsaved edits were kept and Save As is required",
                ));
            } else if changed {
                self.reload_current_after_external_update(current);
                return;
            }
        }

        if pending.refresh_navigation {
            self.refresh_navigation(current, false);
        }
    }

    fn reload_current_after_external_update(&self, file: gio::File) {
        let generation = self.0.external_reload_generation.get().wrapping_add(1);
        self.0.external_reload_generation.set(generation);
        let load_generation = self.0.load_generation.get();
        let cancellable = gio::Cancellable::new();
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let preview = load_preview(&file, DecodeLimits::default(), &cancellable).await;
            let Some(state) = weak.upgrade() else {
                return;
            };
            if state.external_reload_generation.get() != generation
                || state.load_generation.get() != load_generation
                || !files_equal(&state.current_file.borrow(), &Some(file.clone()))
            {
                return;
            }
            match preview {
                Ok(preview) => {
                    state
                        .preview_cache
                        .borrow_mut()
                        .put(file.uri().to_string(), preview);
                    state
                        .pending_comparison
                        .replace(state.compare_file.borrow().clone());
                    let this = ViewerWindow(state.clone());
                    this.load(file);
                    state
                        .toasts
                        .add_toast(adw::Toast::new("Image reloaded after an external update"));
                }
                Err(error) => {
                    tracing::warn!(%error, "Could not reload externally updated image");
                    state.toasts.add_toast(adw::Toast::new(&format!(
                        "Could not reload the updated image: {error}"
                    )));
                }
            }
        });
    }

    fn choose_comparison(&self) {
        if self.0.canvas.texture().is_none() {
            self.0
                .toasts
                .add_toast(adw::Toast::new("Open the first image before comparing"));
            return;
        }
        let mut builder = gtk::FileDialog::builder()
            .title("Choose Comparison Image")
            .modal(true);
        if let Some(folder) = self.preferred_initial_folder() {
            builder = builder.initial_folder(&folder);
        }
        let dialog = builder.build();
        let parent = self.0.window.clone();
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(file) = dialog.open_future(Some(&parent)).await {
                this.load_comparison(file);
            }
        });
    }

    fn load_comparison(&self, file: gio::File) {
        if let Some(previous) = self.0.comparison_cancellable.borrow_mut().take() {
            previous.cancel();
        }
        let comparison_generation = self.0.comparison_generation.get().wrapping_add(1);
        self.0.comparison_generation.set(comparison_generation);
        let cancellable = gio::Cancellable::new();
        self.0
            .comparison_cancellable
            .replace(Some(cancellable.clone()));
        let primary_generation = self.0.load_generation.get();
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let preview = load_preview(&file, DecodeLimits::default(), &cancellable).await;
            let Some(state) = weak.upgrade() else {
                return;
            };
            if state.load_generation.get() != primary_generation
                || state.comparison_generation.get() != comparison_generation
            {
                return;
            }
            state.comparison_cancellable.borrow_mut().take();
            match preview {
                Ok(preview) => ViewerWindow(state).enter_compare(file, preview),
                Err(error) => state.toasts.add_toast(adw::Toast::new(&error.to_string())),
            }
        });
    }

    fn enter_compare(&self, file: gio::File, preview: crate::image::LoadedPreview) {
        self.exit_compare();
        let Some(primary) = self.0.canvas.texture() else {
            return;
        };
        let compare_canvas = ImageCanvas::default();
        compare_canvas.set_texture(Some(&preview.texture));
        compare_canvas.set_filter(self.0.canvas.filter());
        compare_canvas.set_background(self.0.canvas.background());
        compare_canvas.set_zoom(self.0.canvas.zoom());
        compare_canvas.set_halign(gtk::Align::Center);
        compare_canvas.set_valign(gtk::Align::Center);
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

        self.0.canvas_overlay.set_child(None::<&gtk::Widget>);
        self.0.toasts.set_child(None::<&gtk::Widget>);
        paned.set_start_child(Some(&self.0.scrolled));
        paned.set_end_child(Some(&compare_scrolled));
        let toolbar = gtk::CenterBox::builder()
            .orientation(gtk::Orientation::Horizontal)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
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
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        controls.append(&lock);
        controls.append(&close);
        toolbar.set_center_widget(Some(&controls));
        if let Some(primary_file) = self.0.current_file.borrow().as_ref() {
            toolbar.set_start_widget(Some(&compare_metadata_label(
                primary_file,
                primary.width() as u32,
                primary.height() as u32,
                0.0,
            )));
        }
        toolbar.set_end_widget(Some(&compare_metadata_label(
            &file,
            preview.width,
            preview.height,
            1.0,
        )));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&toolbar);
        root.append(&paned);
        self.0.toasts.set_child(Some(&root));
        self.0.compare_canvas.replace(Some(compare_canvas.clone()));
        self.0.compare_file.replace(Some(file));
        self.0
            .compare_scrolled
            .replace(Some(compare_scrolled.clone()));
        self.0.compare_paned.replace(Some(paned.clone()));
        self.0.compare_locked.set(true);
        compare_canvas.set_zoom(self.0.canvas.zoom());
        let compare_rendered = rgba_from_texture(&preview.texture);
        if let Some(image) = compare_rendered.as_ref() {
            compare_canvas.set_auto_background_from_image(image);
        }
        self.0.compare_rendered.replace(compare_rendered);
        self.monitor_comparison_file();

        lock.connect_toggled({
            let this = self.clone();
            move |button| this.0.compare_locked.set(button.is_active())
        });
        close.connect_clicked({
            let this = self.clone();
            move |_| this.exit_compare()
        });
        self.connect_compare_adjustments(&compare_scrolled);
        let cursor = if self.0.color_picker_button.is_active() {
            "crosshair"
        } else {
            "none"
        };
        self.0.canvas.set_cursor_from_name(Some(cursor));
        compare_canvas.set_cursor_from_name(Some(cursor));
        self.connect_lens(&self.0.canvas, &compare_canvas, CompareLensSource::Primary);
        self.connect_lens(
            &compare_canvas,
            &self.0.canvas,
            CompareLensSource::Comparison,
        );
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
        compare_canvas.add_controller(scroll.clone());
        self.0
            .compare_controllers
            .borrow_mut()
            .push((compare_canvas.clone(), scroll.upcast()));
        let this = self.clone();
        self.0.window.add_tick_callback(move |_, _| {
            if this.layout_compare_panels() {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }

    fn layout_compare_panels(&self) -> bool {
        let Some(paned) = self.0.compare_paned.borrow().clone() else {
            return true;
        };
        let paned_size = (paned.width(), paned.height());
        if !usable_panel_size(paned_size) {
            return false;
        }
        let available = if paned.orientation() == gtk::Orientation::Horizontal {
            paned_size.0
        } else {
            paned_size.1
        };
        let centered_position = available / 2;
        if paned.position() != centered_position {
            paned.set_position(centered_position);
            return false;
        }
        self.fit_compare_panels()
    }

    fn fit_compare_panels(&self) -> bool {
        let Some(comparison) = self.0.compare_canvas.borrow().clone() else {
            return true;
        };
        let Some(primary_texture) = self.0.canvas.texture() else {
            return true;
        };
        let Some(comparison_texture) = comparison.texture() else {
            return true;
        };
        let Some(comparison_scrolled) = self.0.compare_scrolled.borrow().clone() else {
            return true;
        };
        let primary_size = (self.0.scrolled.width(), self.0.scrolled.height());
        let comparison_size = (comparison_scrolled.width(), comparison_scrolled.height());
        if !usable_panel_size(primary_size) || !usable_panel_size(comparison_size) {
            return false;
        }
        let fit_zooms = (
            panel_fit_zoom(
                primary_size,
                (primary_texture.width(), primary_texture.height()),
            ),
            panel_fit_zoom(
                comparison_size,
                (comparison_texture.width(), comparison_texture.height()),
            ),
        );
        self.0.compare_fit_zooms.set(Some(fit_zooms));
        self.set_zoom(fit_zooms.0);
        true
    }

    fn exit_compare(&self) {
        self.0
            .comparison_generation
            .set(self.0.comparison_generation.get().wrapping_add(1));
        if let Some(monitor) = self.0.comparison_monitor.borrow_mut().take() {
            monitor.cancel();
        }
        if let Some(cancellable) = self.0.comparison_cancellable.borrow_mut().take() {
            cancellable.cancel();
        }
        self.0.comparison_refresh_scheduled.set(false);
        self.0.comparison_renamed_to.replace(None);
        self.0.compare_lens_source.set(None);
        self.0.compare_locked.set(false);
        self.0.syncing_compare.set(false);
        for (canvas, controller) in self.0.compare_controllers.borrow_mut().drain(..) {
            canvas.remove_controller(&controller);
        }
        for (adjustment, handler) in self.0.compare_adjustment_handlers.borrow_mut().drain(..) {
            adjustment.disconnect(handler);
        }
        self.0.canvas.clear_lens();
        let cursor = if self.0.lens_active.get() || self.0.measurement_button.is_active() {
            Some("none")
        } else if self.0.selection_button.is_active() || self.0.color_picker_button.is_active() {
            Some("crosshair")
        } else {
            None
        };
        self.0.canvas.set_cursor_from_name(cursor);
        self.0.canvas.set_marker(None);
        if let Some(canvas) = self.0.compare_canvas.borrow().as_ref() {
            canvas.clear_lens();
            canvas.set_marker(None);
        }
        if let Some(paned) = self.0.compare_paned.borrow_mut().take() {
            self.0.toasts.set_child(None::<&gtk::Widget>);
            paned.set_start_child(None::<&gtk::Widget>);
            paned.set_end_child(None::<&gtk::Widget>);
            self.0.canvas_overlay.set_child(Some(&self.0.scrolled));
            self.0.toasts.set_child(Some(&self.0.canvas_overlay));
        }
        self.0.compare_scrolled.replace(None);
        self.0.compare_canvas.replace(None);
        self.0.compare_fit_zooms.set(None);
        self.0.compare_rendered.replace(None);
        self.0.compare_file.replace(None);
        self.0.canvas.set_tooltip_text(Some("Image canvas"));
        self.0.canvas.set_accessible_label("Image canvas");
        let this = self.clone();
        glib::idle_add_local_once(move || this.update_minimap());
    }

    fn monitor_comparison_file(&self) {
        if let Some(monitor) = self.0.comparison_monitor.borrow_mut().take() {
            monitor.cancel();
        }
        let Some(file) = self.0.compare_file.borrow().clone() else {
            return;
        };
        let Ok(monitor) =
            file.monitor_file(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        else {
            return;
        };
        monitor.connect_changed({
            let weak = Rc::downgrade(&self.0);
            move |_, changed_file, other_file, event| {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                let Some(comparison) = state.compare_file.borrow().clone() else {
                    return;
                };
                if matches!(event, gio::FileMonitorEvent::AttributeChanged) {
                    return;
                }
                if changed_file.equal(&comparison)
                    && matches!(
                        event,
                        gio::FileMonitorEvent::Moved
                            | gio::FileMonitorEvent::Renamed
                            | gio::FileMonitorEvent::MovedOut
                    )
                    && let Some(target) = other_file
                {
                    state.comparison_renamed_to.replace(Some(target.clone()));
                }
                let this = ViewerWindow(state);
                this.queue_comparison_refresh();
            }
        });
        self.0.comparison_monitor.replace(Some(monitor));
    }

    fn queue_comparison_refresh(&self) {
        if self.0.comparison_refresh_scheduled.replace(true) {
            return;
        }
        let generation = self.0.comparison_generation.get();
        let weak = Rc::downgrade(&self.0);
        glib::timeout_add_local_once(Duration::from_millis(250), move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            if state.comparison_generation.get() != generation
                || !state.comparison_refresh_scheduled.replace(false)
            {
                return;
            }
            let current = state.compare_file.borrow().clone();
            let renamed = state.comparison_renamed_to.borrow_mut().take();
            let candidate = renamed
                .filter(is_regular_file)
                .or_else(|| current.filter(is_regular_file));
            let this = ViewerWindow(state.clone());
            if let Some(candidate) = candidate {
                this.load_comparison(candidate);
            } else {
                this.exit_compare();
                state
                    .toasts
                    .add_toast(adw::Toast::new("The comparison image was moved or deleted"));
            }
        });
    }

    fn connect_compare_adjustments(&self, compare: &gtk::ScrolledWindow) {
        for (source, target) in [
            (self.0.scrolled.hadjustment(), compare.hadjustment()),
            (self.0.scrolled.vadjustment(), compare.vadjustment()),
            (compare.hadjustment(), self.0.scrolled.hadjustment()),
            (compare.vadjustment(), self.0.scrolled.vadjustment()),
        ] {
            let this = self.clone();
            let handler = source.connect_value_changed(move |source| {
                if !this.0.compare_locked.get() || this.0.syncing_compare.replace(true) {
                    return;
                }
                sync_adjustment(source, &target);
                this.0.syncing_compare.set(false);
            });
            self.0
                .compare_adjustment_handlers
                .borrow_mut()
                .push((source, handler));
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
        if active {
            self.0.measurement_button.set_active(false);
            self.0.selection_button.set_active(false);
        }
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
        self.0.canvas.set_cursor_from_name(if active {
            Some("none")
        } else {
            self.0
                .color_picker_button
                .is_active()
                .then_some("crosshair")
        });
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
                    true,
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
        source_id: CompareLensSource,
    ) {
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion({
            let this = self.clone();
            let source = source.clone();
            let target = target.clone();
            move |_, x, y| {
                if this.0.compare_canvas.borrow().is_none()
                    || this.0.edit_button.is_active()
                    || this.0.measurement_button.is_active()
                    || this.0.selection_button.is_active()
                {
                    source.clear_lens();
                    target.clear_lens();
                    let cursor = if source == this.0.canvas && this.0.measurement_button.is_active()
                    {
                        Some("none")
                    } else if source == this.0.canvas && this.0.selection_button.is_active() {
                        Some("crosshair")
                    } else {
                        None
                    };
                    source.set_cursor_from_name(cursor);
                    return;
                }
                this.0.compare_lens_source.set(Some(source_id));
                source.set_cursor_from_name(Some("none"));
                let Some(source_texture) = source.texture() else {
                    source.clear_lens();
                    target.clear_lens();
                    return;
                };
                let Some(target_texture) = target.texture() else {
                    source.clear_lens();
                    target.clear_lens();
                    return;
                };
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
                    true,
                );
                target.set_lens(
                    &target_texture,
                    normalized_x,
                    normalized_y,
                    this.0.lens_diameter.get(),
                    magnification,
                    false,
                );
            }
        });
        motion.connect_leave({
            let this = self.clone();
            let source = source.clone();
            let target = target.clone();
            move |_| {
                if this.0.compare_lens_source.get() == Some(source_id) {
                    this.0.compare_lens_source.set(None);
                    source.clear_lens();
                    target.clear_lens();
                }
            }
        });
        source.add_controller(motion.clone());
        self.0
            .compare_controllers
            .borrow_mut()
            .push((source.clone(), motion.upcast()));

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.connect_scroll({
            let this = self.clone();
            move |controller, _, dy| {
                if this.0.compare_canvas.borrow().is_none() {
                    return glib::Propagation::Proceed;
                }
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
        source.add_controller(scroll.clone());
        self.0
            .compare_controllers
            .borrow_mut()
            .push((source.clone(), scroll.upcast()));
    }

    fn set_zoom(&self, zoom: f64) {
        self.0.pending_fit.set(None);
        self.0.canvas.set_zoom(zoom);
        self.0
            .zoom_label
            .set_label(&format!("{:.0}%", self.0.canvas.zoom() * 100.0));
        self.update_subtitle();
        self.0.settings.set_last_zoom(self.0.canvas.zoom());
        if self.0.compare_locked.get()
            && let Some(compare) = self.0.compare_canvas.borrow().as_ref()
        {
            let zoom = self
                .0
                .compare_fit_zooms
                .get()
                .map_or(self.0.canvas.zoom(), |fit_zooms| {
                    comparison_zoom(self.0.canvas.zoom(), fit_zooms)
                });
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
            move |_, _| {
                this.update_minimap();
                this.apply_pending_fit();
            }
        });
        self.0.scrolled.connect_notify_local(Some("height"), {
            let this = self.clone();
            move |_, _| {
                this.update_minimap();
                this.apply_pending_fit();
            }
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
        let horizontal_target =
            anchored_adjustment_value(horizontal.value(), content_x, applied_factor);
        let vertical_target =
            anchored_adjustment_value(vertical.value(), content_y, applied_factor);
        self.set_zoom(new_zoom);
        glib::idle_add_local_once(move || {
            horizontal.set_value(horizontal_target);
            vertical.set_value(vertical_target);
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
        let next = self.0.sequence.borrow().as_ref().and_then(|sequence| {
            let mut next_sequence = sequence.clone();
            let file = if forward {
                next_sequence.next_image().cloned()
            } else {
                next_sequence.previous().cloned()
            }?;
            Some((next_sequence, file))
        });
        if let Some((next_sequence, file)) = next {
            if !is_regular_file(&file) {
                if let Some(current) = self.0.current_file.borrow().clone() {
                    self.refresh_navigation(current, false);
                }
                self.0.toasts.add_toast(adw::Toast::new(
                    "That image was moved or deleted; the folder was refreshed",
                ));
                return;
            }
            self.0.sequence.replace(Some(next_sequence));
            let Some(compare_file) = self.0.compare_file.borrow().clone() else {
                self.load(file);
                return;
            };
            let generation = self.0.navigation_generation.get().wrapping_add(1);
            self.0.navigation_generation.set(generation);
            let weak = Rc::downgrade(&self.0);
            let target_for_match = file.clone();
            let comparison_for_match = compare_file.clone();
            glib::spawn_future_local(async move {
                let matching_file = gio::spawn_blocking(move || {
                    find_matching_file(&comparison_for_match, &target_for_match)
                        .ok()
                        .flatten()
                })
                .await;
                let Some(state) = weak.upgrade() else {
                    return;
                };
                if state.navigation_generation.get() != generation {
                    return;
                }
                let comparison = matching_file.unwrap_or(Some(compare_file));
                state.pending_comparison.replace(comparison);
                ViewerWindow(state).load(file);
            });
        }
    }

    fn confirm_delete_current_file(&self) {
        if self.0.export_cancellation.borrow().is_some() {
            self.0
                .toasts
                .add_toast(adw::Toast::new("Wait for the current export to finish"));
            return;
        }
        if self.0.deletion_running.get() {
            self.0
                .toasts
                .add_toast(adw::Toast::new("Image deletion is already in progress"));
            return;
        }
        let Some(file) = self.0.current_file.borrow().clone() else {
            self.0.toasts.add_toast(adw::Toast::new("No image is open"));
            return;
        };
        let name = file.basename().map_or_else(
            || file.uri().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let has_unsaved_edits = self
            .0
            .document
            .borrow()
            .as_ref()
            .is_some_and(Document::is_dirty);
        let body = if has_unsaved_edits {
            format!(
                "“{name}” and its unsaved edits will be permanently deleted. This cannot be undone."
            )
        } else {
            format!("“{name}” will be permanently deleted. This cannot be undone.")
        };
        let dialog = adw::AlertDialog::builder()
            .heading("Delete this image?")
            .body(body)
            .close_response("cancel")
            .default_response("cancel")
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("delete", "Delete");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        let this = self.clone();
        dialog.connect_response(Some("delete"), move |_, _| {
            this.delete_current_file(file.clone());
        });
        dialog.present(Some(&self.0.window));
    }

    fn delete_current_file(&self, file: gio::File) {
        if self.0.deletion_running.replace(true) {
            return;
        }
        let known_replacement = self
            .0
            .sequence
            .borrow()
            .as_ref()
            .and_then(DirectorySequence::replacement_after_current_removed);
        let fallback = self.0.settings.folder_sort();
        if let Some(monitor) = self.0.directory_monitor.borrow_mut().take() {
            monitor.cancel();
        }
        let generation = self.0.load_generation.get();
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let replacement = if known_replacement.is_some() {
                known_replacement
            } else {
                let sequence_file = file.clone();
                gio::spawn_blocking(move || {
                    DirectorySequence::build(&sequence_file, fallback)
                        .ok()
                        .and_then(|sequence| sequence.replacement_after_current_removed())
                })
                .await
                .ok()
                .flatten()
            };
            let result = file.delete_future(glib::Priority::DEFAULT).await;
            let Some(state) = weak.upgrade() else {
                return;
            };
            let this = ViewerWindow(state.clone());
            if let Err(error) = result {
                state.deletion_running.set(false);
                state
                    .toasts
                    .add_toast(adw::Toast::new(&format!("Could not delete image: {error}")));
                this.monitor_directory();
                return;
            }
            if state.load_generation.get() != generation
                || !files_equal(&state.current_file.borrow(), &Some(file))
            {
                state.deletion_running.set(false);
                this.monitor_directory();
                return;
            }
            if let Some(document) = state.document.borrow_mut().as_mut() {
                document.restore_original();
            }
            state.deletion_running.set(false);
            state.toasts.add_toast(adw::Toast::new("Image deleted"));
            if let Some(replacement) = replacement {
                this.load(replacement);
            } else {
                state.close_approved.set(true);
                state.window.close();
            }
        });
    }

    fn fit(&self, fill: bool) {
        self.0.pending_fit.set(Some(fill));
        if !self.apply_pending_fit() && !self.0.fit_tick_scheduled.replace(true) {
            let this = self.clone();
            self.0.window.add_tick_callback(move |_, _| {
                if this.apply_pending_fit() {
                    this.0.fit_tick_scheduled.set(false);
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
                }
            });
        }
    }

    fn apply_pending_fit(&self) -> bool {
        let Some(fill) = self.0.pending_fit.get() else {
            return true;
        };
        let Some(texture) = self.0.canvas.texture() else {
            return false;
        };
        let viewport = (self.0.scrolled.width(), self.0.scrolled.height());
        if !usable_panel_size(viewport) {
            return false;
        }
        let width = f64::from(viewport.0);
        let height = f64::from(viewport.1);
        let horizontal = width / f64::from(texture.width());
        let vertical = height / f64::from(texture.height());
        self.set_zoom(if fill {
            horizontal.max(vertical)
        } else {
            horizontal.min(vertical)
        });
        true
    }

    fn install_gestures(&self) {
        let zoom = gtk::GestureZoom::new();
        let zoom_anchor = Rc::new(Cell::new(None::<ZoomGestureAnchor>));
        let zoom_adjustment_target = Rc::new(Cell::new(None::<(f64, f64)>));
        self.0.scrolled.hadjustment().connect_changed({
            let zoom_adjustment_target = zoom_adjustment_target.clone();
            move |adjustment| {
                if let Some((target, _)) = zoom_adjustment_target.get() {
                    adjustment.set_value(target);
                }
            }
        });
        self.0.scrolled.vadjustment().connect_changed({
            let zoom_adjustment_target = zoom_adjustment_target.clone();
            move |adjustment| {
                if let Some((_, target)) = zoom_adjustment_target.get() {
                    adjustment.set_value(target);
                }
            }
        });
        zoom.connect_begin({
            let this = self.clone();
            let zoom_anchor = zoom_anchor.clone();
            move |gesture, _| {
                let horizontal = this.0.scrolled.hadjustment();
                let vertical = this.0.scrolled.vadjustment();
                let (content_x, content_y) = gesture.bounding_box_center().unwrap_or((
                    horizontal.value() + horizontal.page_size() / 2.0,
                    vertical.value() + vertical.page_size() / 2.0,
                ));
                zoom_anchor.set(Some(ZoomGestureAnchor {
                    start_zoom: this.0.canvas.zoom(),
                    content_x,
                    content_y,
                    horizontal_value: horizontal.value(),
                    vertical_value: vertical.value(),
                }));
            }
        });
        zoom.connect_scale_changed({
            let this = self.clone();
            let zoom_anchor = zoom_anchor.clone();
            let zoom_adjustment_target = zoom_adjustment_target.clone();
            move |_, scale| {
                let Some(anchor) = zoom_anchor.get() else {
                    return;
                };
                let target_zoom = (anchor.start_zoom * scale).clamp(0.01, 64.0);
                let applied_factor = target_zoom / anchor.start_zoom;
                let horizontal_target = anchored_adjustment_value(
                    anchor.horizontal_value,
                    anchor.content_x,
                    applied_factor,
                );
                let vertical_target = anchored_adjustment_value(
                    anchor.vertical_value,
                    anchor.content_y,
                    applied_factor,
                );
                zoom_adjustment_target.set(Some((horizontal_target, vertical_target)));
                this.set_zoom(target_zoom);
                let horizontal = this.0.scrolled.hadjustment();
                let vertical = this.0.scrolled.vadjustment();
                horizontal.set_value(horizontal_target);
                vertical.set_value(vertical_target);
            }
        });
        zoom.connect_end({
            let zoom_adjustment_target = zoom_adjustment_target.clone();
            move |_, _| zoom_adjustment_target.set(None)
        });
        zoom.connect_cancel({
            let zoom_adjustment_target = zoom_adjustment_target.clone();
            move |_, _| zoom_adjustment_target.set(None)
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

        let measurement_motion = gtk::EventControllerMotion::new();
        measurement_motion.connect_motion({
            let this = self.clone();
            move |_, x, y| {
                if !this.0.measurement_button.is_active() {
                    return;
                }
                this.0
                    .canvas
                    .set_measurement_cursor(this.0.canvas.snapped_normalized_at(x, y));
                this.0.canvas.set_cursor_from_name(Some("none"));
            }
        });
        measurement_motion.connect_leave({
            let this = self.clone();
            move |_| {
                if this.0.measurement_button.is_active() {
                    this.0.canvas.set_measurement_cursor(None);
                }
            }
        });
        self.0.canvas.add_controller(measurement_motion);

        let measurement = gtk::GestureDrag::new();
        measurement.set_button(1);
        measurement.connect_drag_begin({
            let this = self.clone();
            move |gesture, x, y| {
                if !this.0.measurement_button.is_active() {
                    return;
                }
                let Some(start) = this.0.canvas.pixel_boundary_at(x, y) else {
                    return;
                };
                let Some(image_dimensions) = this
                    .0
                    .rendered
                    .borrow()
                    .as_ref()
                    .map(image::GenericImageView::dimensions)
                else {
                    return;
                };
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let drag = MeasurementDrag {
                    start,
                    current: start,
                    start_screen: (x, y),
                    image_dimensions,
                };
                this.0.measurement_drag.set(Some(drag));
                this.0
                    .canvas
                    .set_measurement_overlay(Some(measurement_overlay(drag)));
            }
        });
        measurement.connect_drag_update({
            let this = self.clone();
            move |_, offset_x, offset_y| {
                let Some(mut drag) = this.0.measurement_drag.get() else {
                    return;
                };
                let Some(current) = this.0.canvas.pixel_boundary_at(
                    drag.start_screen.0 + offset_x,
                    drag.start_screen.1 + offset_y,
                ) else {
                    return;
                };
                drag.current = current;
                this.0.measurement_drag.set(Some(drag));
                this.0
                    .canvas
                    .set_measurement_overlay(Some(measurement_overlay(drag)));
            }
        });
        measurement.connect_drag_end({
            let this = self.clone();
            move |_, offset_x, offset_y| {
                let Some(mut drag) = this.0.measurement_drag.take() else {
                    return;
                };
                if let Some(current) = this.0.canvas.pixel_boundary_at(
                    drag.start_screen.0 + offset_x,
                    drag.start_screen.1 + offset_y,
                ) {
                    drag.current = current;
                }
                let measurement = measurement_overlay(drag);
                this.0.canvas.set_measurement_overlay(Some(measurement));
                this.copy_measurement_to_clipboard(measurement);
            }
        });
        self.0.canvas.add_controller(measurement);

        let selection = gtk::GestureDrag::new();
        selection.set_button(1);
        selection.connect_drag_begin({
            let this = self.clone();
            move |gesture, x, y| {
                if !this.0.selection_button.is_active() {
                    return;
                }
                let Some(start) = this.0.canvas.pixel_at(x, y) else {
                    return;
                };
                let Some(image_dimensions) = this
                    .0
                    .rendered
                    .borrow()
                    .as_ref()
                    .map(image::GenericImageView::dimensions)
                else {
                    return;
                };
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let drag = SelectionDrag {
                    start,
                    current: start,
                    start_screen: (x, y),
                    image_dimensions,
                };
                this.0.selection_drag.set(Some(drag));
                this.0.canvas.set_crop_overlay(selection_overlay(drag));
            }
        });
        selection.connect_drag_update({
            let this = self.clone();
            move |_, offset_x, offset_y| {
                let Some(mut drag) = this.0.selection_drag.get() else {
                    return;
                };
                let Some(current) = this.0.canvas.pixel_at(
                    drag.start_screen.0 + offset_x,
                    drag.start_screen.1 + offset_y,
                ) else {
                    return;
                };
                drag.current = current;
                this.0.selection_drag.set(Some(drag));
                this.0.canvas.set_crop_overlay(selection_overlay(drag));
            }
        });
        selection.connect_drag_end({
            let this = self.clone();
            move |_, offset_x, offset_y| {
                let Some(mut drag) = this.0.selection_drag.take() else {
                    return;
                };
                if let Some(current) = this.0.canvas.pixel_at(
                    drag.start_screen.0 + offset_x,
                    drag.start_screen.1 + offset_y,
                ) {
                    drag.current = current;
                }
                this.0.canvas.set_crop_overlay(None);
                this.complete_selection(drag);
            }
        });
        self.0.canvas.add_controller(selection);

        let color_picker = gtk::GestureClick::new();
        color_picker.set_button(1);
        color_picker.connect_pressed({
            let this = self.clone();
            move |gesture, _, x, y| {
                if !this.0.color_picker_button.is_active() {
                    return;
                }
                let color = this.0.canvas.pixel_at(x, y).and_then(|(x, y)| {
                    this.0
                        .rendered
                        .borrow()
                        .as_ref()
                        .and_then(|image| crate::tools::pencil::sample(image, x, y))
                });
                let Some(color) = color else {
                    return;
                };
                gesture.set_state(gtk::EventSequenceState::Claimed);
                this.copy_color_to_clipboard(color);
            }
        });
        self.0.canvas.add_controller(color_picker);

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
                let cursor = if this.0.lens_active.get() || this.0.measurement_button.is_active() {
                    Some("none")
                } else if this.0.selection_button.is_active()
                    || this.0.color_picker_button.is_active()
                {
                    Some("crosshair")
                } else {
                    None
                };
                this.0.canvas.set_cursor_from_name(cursor)
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
                gesture.set_state(gtk::EventSequenceState::Claimed);
                this.0.edit_drag.set(Some(EditDrag {
                    crop,
                    start_screen_x: screen_x,
                    start_screen_y: screen_y,
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
                let Some((x, y)) = this
                    .0
                    .canvas
                    .pixel_at(drag.start_screen_x + dx, drag.start_screen_y + dy)
                else {
                    return;
                };
                let crop = resize_crop(
                    drag.crop,
                    x,
                    y,
                    drag.left,
                    drag.right,
                    drag.top,
                    drag.bottom,
                );
                this.0.edit_crop.replace(Some(crop));
                this.0.canvas.set_crop_overlay(Some(crop));
            }
        });
        edit_drag.connect_drag_end({
            let this = self.clone();
            move |_, _, _| {
                this.0.edit_drag.take();
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
        canvas.add_controller(pencil.clone());
        self.0
            .compare_controllers
            .borrow_mut()
            .push((canvas.clone(), pencil.upcast()));

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
        canvas.add_controller(sampler.clone());
        self.0
            .compare_controllers
            .borrow_mut()
            .push((canvas.clone(), sampler.upcast()));

        let color_picker = gtk::GestureClick::new();
        color_picker.set_button(1);
        color_picker.connect_pressed({
            let this = self.clone();
            let canvas = canvas.clone();
            move |gesture, _, x, y| {
                if !this.0.color_picker_button.is_active() {
                    return;
                }
                let color = canvas.pixel_at(x, y).and_then(|(x, y)| {
                    this.0
                        .compare_rendered
                        .borrow()
                        .as_ref()
                        .and_then(|image| crate::tools::pencil::sample(image, x, y))
                });
                let Some(color) = color else {
                    return;
                };
                gesture.set_state(gtk::EventSequenceState::Claimed);
                this.copy_color_to_clipboard(color);
            }
        });
        canvas.add_controller(color_picker.clone());
        self.0
            .compare_controllers
            .borrow_mut()
            .push((canvas.clone(), color_picker.upcast()));
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
    let scale_button = toggle_button("view-fullscreen-symbolic", "Scale image");
    let selection_button = toggle_button(
        "edit-select-all-symbolic",
        "Select and Copy — drag a rectangle or click an object",
    );
    let color_picker_button = toggle_button(
        "color-select-symbolic",
        "Pick Color — click a pixel to copy its value",
    );
    let measurement_button = toggle_button(
        "ruler-measure-symbolic",
        "Measure pixels — click or drag a rectangle",
    );
    let pencil_button = toggle_button("xsi-edit-symbolic", "Toggle Pencil");
    let lens_button = toggle_button("edit-find-symbolic", "Toggle 4× Lens");
    let edit_button = toggle_button(
        "edit-cut-symbolic",
        "Crop image — Enter to apply, Escape to cancel",
    );
    let color_button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    color_button.set_rgba(&u8_to_rgba([0, 0, 0, 255]));
    color_button.set_tooltip_text(Some("Pencil color"));
    header.pack_start(&animation_controls);
    header.pack_start(&previous);
    header.pack_start(&next);
    header.pack_end(&menu_button());
    header.pack_end(&button("media-floppy-symbolic", "Save As", "win.save-as"));
    header.pack_end(&selection_button);
    header.pack_end(&color_picker_button);
    header.pack_end(&color_button);
    header.pack_end(&pencil_button);
    header.pack_end(&lens_button);
    HeaderWidgets {
        header,
        animation_controls,
        animation_play_button: play,
        scale_button,
        measurement_button,
        selection_button,
        color_picker_button,
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
    let menu = main_menu();
    gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Main Menu")
        .menu_model(&menu)
        .build()
}

fn main_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    menu.append(Some("Open…"), Some("win.open"));
    menu.append(Some("Save"), Some("win.save"));
    menu.append(Some("Save As…"), Some("win.save-as"));
    menu.append(Some("Compare Images…"), Some("win.compare"));
    let edit_menu = gio::Menu::new();
    edit_menu.append(Some("Measure"), Some("win.measure"));
    edit_menu.append(Some("Crop"), Some("win.crop"));
    edit_menu.append(Some("Scale"), Some("win.scale-preview"));
    menu.append_submenu(Some("Edit"), &edit_menu);
    menu.append(Some("Image Properties"), Some("win.properties"));
    menu.append(Some("Preferences"), Some("win.preferences"));
    menu.append(Some("Keyboard Shortcuts"), Some("win.shortcuts"));
    menu.append(Some("About Diorama"), Some("win.about"));
    menu
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

fn color_format_index(format: ColorFormat) -> u32 {
    match format {
        ColorFormat::Hex => 0,
        ColorFormat::Rgb => 1,
        ColorFormat::Oklab => 2,
        ColorFormat::Hsl => 3,
    }
}

fn color_format_at(index: u32) -> ColorFormat {
    match index {
        1 => ColorFormat::Rgb,
        2 => ColorFormat::Oklab,
        3 => ColorFormat::Hsl,
        _ => ColorFormat::Hex,
    }
}

fn format_decimal(value: f32, precision: usize) -> String {
    let threshold = 0.5 * 10.0_f32.powi(-(precision as i32));
    let value = if value.abs() < threshold { 0.0 } else { value };
    let formatted = format!("{value:.precision$}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn format_color(color: [u8; 4], format: ColorFormat) -> String {
    let [red, green, blue, alpha] = color;
    match format {
        ColorFormat::Hex if alpha == u8::MAX => format!("#{red:02X}{green:02X}{blue:02X}"),
        ColorFormat::Hex => format!("#{red:02X}{green:02X}{blue:02X}{alpha:02X}"),
        ColorFormat::Rgb if alpha == u8::MAX => format!("rgb({red}, {green}, {blue})"),
        ColorFormat::Rgb => format!(
            "rgba({red}, {green}, {blue}, {})",
            format_decimal(f32::from(alpha) / 255.0, 3)
        ),
        ColorFormat::Oklab => {
            let srgb = palette::Srgb::new(
                f32::from(red) / 255.0,
                f32::from(green) / 255.0,
                f32::from(blue) / 255.0,
            );
            let oklab = palette::Oklab::from_color(srgb.into_linear());
            let components = format!(
                "{}% {} {}",
                format_decimal(oklab.l * 100.0, 2),
                format_decimal(oklab.a, 4),
                format_decimal(oklab.b, 4)
            );
            if alpha == u8::MAX {
                format!("oklab({components})")
            } else {
                format!(
                    "oklab({components} / {})",
                    format_decimal(f32::from(alpha) / 255.0, 3)
                )
            }
        }
        ColorFormat::Hsl => {
            let srgb = palette::Srgb::new(
                f32::from(red) / 255.0,
                f32::from(green) / 255.0,
                f32::from(blue) / 255.0,
            );
            let hsl = palette::Hsl::from_color(srgb);
            let components = format!(
                "{} {}% {}%",
                format_decimal(hsl.hue.into_positive_degrees(), 1),
                format_decimal(hsl.saturation * 100.0, 1),
                format_decimal(hsl.lightness * 100.0, 1)
            );
            if alpha == u8::MAX {
                format!("hsl({components})")
            } else {
                format!(
                    "hsl({components} / {})",
                    format_decimal(f32::from(alpha) / 255.0, 3)
                )
            }
        }
    }
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
    fn folder_path_uses_the_file_parent() {
        let file = gio::File::for_path("/images/comparison/frame.png");

        assert_eq!(folder_path(&file), "/images/comparison");
    }

    #[test]
    fn initial_folder_selection_skips_a_deleted_folder() {
        let deleted = tempfile::tempdir().expect("temporary deleted folder");
        let deleted_path = deleted.path().to_path_buf();
        drop(deleted);
        let fallback = tempfile::tempdir().expect("temporary fallback folder");

        let selected = first_existing_folder([
            gio::File::for_path(deleted_path),
            gio::File::for_path(fallback.path()),
        ])
        .expect("existing fallback");

        assert_eq!(selected.path().as_deref(), Some(fallback.path()));
    }

    #[test]
    fn missing_or_replaced_source_counts_as_an_external_change() {
        let original = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let replacement = SystemTime::UNIX_EPOCH + Duration::from_secs(200);

        assert!(!source_revision_changed(
            Some(original),
            Some(original),
            true
        ));
        assert!(source_revision_changed(Some(original), None, true));
        assert!(source_revision_changed(
            Some(original),
            Some(replacement),
            true
        ));
        assert!(source_revision_changed(None, None, false));
    }

    #[test]
    fn directory_events_distinguish_updates_replacements_and_moves() {
        let current = gio::File::for_path("/images/current.png");
        let temporary = gio::File::for_path("/images/.current.png.tmp");
        let renamed = gio::File::for_path("/images/renamed.png");

        let mut changed = PendingDirectoryChanges::default();
        merge_directory_change(
            &mut changed,
            &current,
            &current,
            None,
            gio::FileMonitorEvent::ChangesDoneHint,
        );
        assert!(changed.current_changed);
        assert!(!changed.current_removed);

        let mut replaced = PendingDirectoryChanges::default();
        merge_directory_change(
            &mut replaced,
            &current,
            &temporary,
            Some(&current),
            gio::FileMonitorEvent::Renamed,
        );
        assert!(replaced.current_changed);
        assert!(!replaced.current_removed);

        let mut moved = PendingDirectoryChanges::default();
        merge_directory_change(
            &mut moved,
            &current,
            &current,
            Some(&renamed),
            gio::FileMonitorEvent::Renamed,
        );
        assert!(moved.current_removed);
        assert!(
            moved
                .current_renamed_to
                .as_ref()
                .is_some_and(|target| target.equal(&renamed))
        );
    }

    #[test]
    fn deleting_the_containing_folder_marks_the_current_file_removed() {
        let current = gio::File::for_path("/images/current.png");
        let folder = gio::File::for_path("/images");
        let mut pending = PendingDirectoryChanges::default();

        merge_directory_change(
            &mut pending,
            &current,
            &folder,
            None,
            gio::FileMonitorEvent::Deleted,
        );

        assert!(pending.current_removed);
    }

    #[test]
    fn compare_metadata_includes_folder_and_resolution() {
        let file = gio::File::for_path("/images/comparison/frame.png");

        assert_eq!(
            compare_metadata(&file, 1920, 1080),
            "/images/comparison · 1920 × 1080"
        );
    }

    #[test]
    fn modified_time_is_formatted_relative_to_now() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000_000);

        assert_eq!(
            relative_modified_time(now - Duration::from_secs(30), now),
            "just now"
        );
        assert_eq!(
            relative_modified_time(now - Duration::from_secs(60), now),
            "1 minute ago"
        );
        assert_eq!(
            relative_modified_time(now - Duration::from_secs(3 * 60), now),
            "3 minutes ago"
        );
        assert_eq!(
            relative_modified_time(now - Duration::from_secs(2 * 60 * 60), now),
            "2 hours ago"
        );
        assert_eq!(
            relative_modified_time(now - Duration::from_secs(2 * 24 * 60 * 60), now),
            "2 days ago"
        );
        assert_eq!(
            relative_modified_time(now - Duration::from_secs(60 * 24 * 60 * 60), now),
            "2 months ago"
        );
        assert_eq!(
            relative_modified_time(now - Duration::from_secs(2 * 365 * 24 * 60 * 60), now),
            "2 years ago"
        );
        assert_eq!(
            relative_modified_time(now + Duration::from_secs(3 * 60), now),
            "in 3 minutes"
        );
    }

    #[test]
    fn image_subtitle_places_modified_time_after_zoom() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        let modified = now - Duration::from_secs(3 * 60);

        assert_eq!(
            image_subtitle((1920, 1080), 1.25, Some(modified), now),
            "1920 × 1080 · 125% · 3 minutes ago"
        );
        assert_eq!(
            image_subtitle((640, 480), 0.5, None, now),
            "640 × 480 · 50%"
        );
    }

    #[test]
    fn compare_zoom_keeps_each_image_at_its_relative_fit_level() {
        let primary_fit = panel_fit_zoom((800, 600), (1600, 900));
        let comparison_fit = panel_fit_zoom((400, 300), (400, 200));

        assert_eq!(primary_fit, 0.5);
        assert_eq!(comparison_fit, 1.0);
        assert_eq!(
            comparison_zoom(primary_fit, (primary_fit, comparison_fit)),
            1.0
        );
        assert_eq!(
            comparison_zoom(primary_fit * 2.0, (primary_fit, comparison_fit)),
            2.0
        );
    }

    #[test]
    fn compare_layout_rejects_placeholder_allocations() {
        assert!(!usable_panel_size((1, 600)));
        assert!(!usable_panel_size((800, 1)));
        assert!(usable_panel_size((800, 600)));
    }

    #[test]
    fn pencil_is_unavailable_while_crop_is_active() {
        assert!(!pencil_can_activate(true, true));
        assert!(!pencil_can_activate(false, false));
        assert!(pencil_can_activate(false, true));
    }

    #[test]
    fn color_picker_formats_opaque_and_transparent_colors() {
        let opaque = [0x12, 0x34, 0x56, 0xff];
        let transparent = [0x12, 0x34, 0x56, 0x78];

        assert_eq!(format_color(opaque, ColorFormat::Hex), "#123456");
        assert_eq!(format_color(transparent, ColorFormat::Hex), "#12345678");
        assert_eq!(format_color(opaque, ColorFormat::Rgb), "rgb(18, 52, 86)");
        assert_eq!(
            format_color(transparent, ColorFormat::Rgb),
            "rgba(18, 52, 86, 0.471)"
        );
        assert_eq!(
            format_color([255, 0, 0, 255], ColorFormat::Oklab),
            "oklab(62.8% 0.2249 0.1258)"
        );
        assert_eq!(
            format_color([255, 0, 0, 128], ColorFormat::Hsl),
            "hsl(0 100% 50% / 0.502)"
        );
    }

    #[test]
    fn color_property_indices_round_trip_with_hex_as_the_fallback() {
        for (index, format) in [
            (0, ColorFormat::Hex),
            (1, ColorFormat::Rgb),
            (2, ColorFormat::Oklab),
            (3, ColorFormat::Hsl),
        ] {
            assert_eq!(color_format_index(format), index);
            assert_eq!(color_format_at(index), format);
        }
        assert_eq!(color_format_at(u32::MAX), ColorFormat::Hex);
    }

    #[test]
    fn edit_menu_contains_the_moved_header_tools() {
        let menu: gio::MenuModel = main_menu().upcast();
        let string_attribute = |model: &gio::MenuModel, index, name| {
            model
                .item_attribute_value(index, name, None)
                .and_then(|value| value.get::<String>())
                .expect("string menu attribute")
        };
        let edit_index = (0..menu.n_items())
            .find(|index| string_attribute(&menu, *index, "label") == "Edit")
            .expect("Edit submenu");
        let edit_menu = menu
            .item_link(edit_index, "submenu")
            .expect("Edit submenu model");
        let entries = (0..edit_menu.n_items())
            .map(|index| {
                (
                    string_attribute(&edit_menu, index, "label"),
                    string_attribute(&edit_menu, index, "action"),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            entries,
            [
                ("Measure".to_owned(), "win.measure".to_owned()),
                ("Crop".to_owned(), "win.crop".to_owned()),
                ("Scale".to_owned(), "win.scale-preview".to_owned()),
            ]
        );
    }

    #[test]
    fn export_completion_only_matches_its_originating_file_generation() {
        let original = Some(gio::File::for_path("/images/original.png"));
        let replacement = Some(gio::File::for_path("/images/replacement.png"));

        assert!(export_context_matches(7, 7, &original, &original));
        assert!(!export_context_matches(8, 7, &original, &original));
        assert!(!export_context_matches(7, 7, &replacement, &original));
    }

    #[test]
    fn corner_drag_resizes_both_crop_boundaries() {
        let crop = CropOverlay {
            x: 10,
            y: 20,
            width: 50,
            height: 60,
            image_width: 100,
            image_height: 100,
        };

        let resized = resize_crop(crop, 20, 30, true, false, true, false);

        assert_eq!((resized.x, resized.y), (20, 30));
        assert_eq!((resized.width, resized.height), (40, 50));
    }

    #[test]
    fn scale_preview_resizes_pixels_to_selected_width() {
        let image = image::RgbaImage::from_fn(2, 1, |x, _| {
            if x == 0 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 255])
            }
        });

        let preview = render_scale_preview(&image, 4, Resampling::Nearest).unwrap();

        assert_eq!(preview.dimensions(), (4, 2));
        assert_eq!(preview.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(preview.get_pixel(3, 0).0, [0, 0, 255, 255]);
    }

    #[test]
    fn scale_preview_zoom_keeps_the_viewport_width_fixed() {
        let source_width = 800;
        let source_zoom = 0.75;
        let target_width = 1600;

        let preview_zoom = scale_preview_zoom(source_width, target_width, source_zoom);

        assert_eq!(
            source_zoom * f64::from(source_width),
            preview_zoom * f64::from(target_width)
        );
    }

    #[test]
    fn anchored_zoom_keeps_the_content_point_at_the_same_viewport_position() {
        let old_adjustment = 320.0;
        let content_position = 500.0;
        let factor = 1.75;

        let new_adjustment = anchored_adjustment_value(old_adjustment, content_position, factor);

        assert_eq!(content_position - old_adjustment, 180.0);
        assert_eq!(content_position * factor - new_adjustment, 180.0);
    }

    #[test]
    fn measurement_overlay_uses_grid_intersections_in_both_drag_directions() {
        let forward = measurement_overlay(MeasurementDrag {
            start: (2, 3),
            current: (8, 9),
            start_screen: (0.0, 0.0),
            image_dimensions: (16, 12),
        });
        let reverse = measurement_overlay(MeasurementDrag {
            start: (8, 9),
            current: (2, 3),
            start_screen: (0.0, 0.0),
            image_dimensions: (16, 12),
        });
        let point = measurement_overlay(MeasurementDrag {
            start: (5, 4),
            current: (5, 4),
            start_screen: (0.0, 0.0),
            image_dimensions: (16, 12),
        });

        assert_eq!(
            (forward.x, forward.y, forward.width, forward.height),
            (2, 3, 6, 6)
        );
        assert_eq!(
            (reverse.x, reverse.y, reverse.width, reverse.height),
            (2, 3, 6, 6)
        );
        assert_eq!(
            measurement_clipboard_text(forward),
            "x:2,y:3,width:6,height:6"
        );
        assert_eq!(
            measurement_clipboard_text(reverse),
            "x:2,y:3,width:6,height:6"
        );
        assert_eq!(
            measurement_clipboard_text(point),
            "x:5,y:4,width:0,height:0"
        );
        assert_eq!(forward.x as f32 / forward.image_width as f32, 2.0 / 16.0);
        assert_eq!(
            (forward.x + forward.width) as f32 / forward.image_width as f32,
            8.0 / 16.0
        );
    }

    #[test]
    fn downloaded_comparison_texture_keeps_rgba_pixels() {
        let image = image::RgbaImage::from_raw(1, 1, vec![12, 34, 56, 78]).unwrap();
        let texture = texture_from_rgba(&image).unwrap();

        assert_eq!(rgba_from_texture(&texture), Some(image));
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn completed_measurement_is_written_to_the_clipboard() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.MeasurementClipboardTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let measurement = CropOverlay {
            x: 2,
            y: 3,
            width: 6,
            height: 7,
            image_width: 16,
            image_height: 12,
        };

        window.copy_measurement_to_clipboard(measurement);
        let copied = glib::MainContext::default()
            .block_on(window.0.window.clipboard().read_text_future())
            .expect("clipboard read")
            .expect("clipboard text");

        assert_eq!(copied, "x:2,y:3,width:6,height:7");
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn color_picker_updates_the_pencil_color_and_works_with_the_lens() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.ColorPickerButtonTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let display = gtk::gdk::Display::default().expect("graphical display");

        assert_eq!(
            window.0.color_picker_button.icon_name().as_deref(),
            Some("color-select-symbolic")
        );
        assert!(gtk::IconTheme::for_display(&display).has_icon("color-select-symbolic"));
        assert!(window.0.color_picker_button.parent().is_some());

        let image = image::RgbaImage::from_pixel(2, 1, image::Rgba([12, 34, 56, 78]));
        let texture = texture_from_rgba(&image).unwrap();
        window.0.canvas.set_texture(Some(&texture));
        window.0.rendered.replace(Some(image));

        window.0.lens_button.set_active(true);
        window.0.color_picker_button.set_active(true);
        assert!(window.0.lens_active.get());
        assert!(window.0.lens_button.is_active());
        assert!(window.0.color_picker_button.is_active());

        window.0.lens_button.set_active(false);
        assert!(window.0.color_picker_button.is_active());
        window.0.lens_button.set_active(true);
        assert!(window.0.color_picker_button.is_active());

        let picked = [12, 34, 56, 78];
        window.apply_picked_color(picked);
        assert_eq!(window.0.pencil_color.get(), picked);
        assert_eq!(rgba_to_u8(window.0.color_button.rgba()), picked);
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn edit_tools_are_removed_from_the_header_and_have_window_actions() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.EditMenuTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);

        assert!(window.0.measurement_button.parent().is_none());
        assert!(window.0.edit_button.parent().is_none());
        assert!(window.0.scale_button.parent().is_none());
        assert!(window.0.window.lookup_action("measure").is_some());
        assert!(window.0.window.lookup_action("crop").is_some());
        assert!(window.0.window.lookup_action("scale-preview").is_some());
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn scale_controls_use_the_available_overlay_width() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.ScaleLayoutTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);

        assert_eq!(window.0.scale_controls.halign(), gtk::Align::Fill);
        assert!(window.0.scale_controls.hexpands());
        assert_eq!(window.0.scale_controls.margin_start(), 26);
        assert_eq!(window.0.scale_controls.margin_end(), 26);
        assert!(window.0.scale_slider.hexpands());
        assert_eq!(window.0.scale_slider.width_request(), -1);

        window.0.scale_controls.set_visible(true);
        window.0.canvas_overlay.allocate(1000, 600, -1, None);
        assert_eq!(window.0.scale_controls.width(), 948);
        assert!(window.0.scale_slider.width() > 260);
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn new_window_fit_waits_for_the_real_viewport_size() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.InitialFitTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let image = image::RgbaImage::from_pixel(200, 100, image::Rgba([1, 2, 3, 255]));
        let texture = texture_from_rgba(&image).unwrap();
        window.0.canvas.set_texture(Some(&texture));

        assert!(!usable_panel_size((
            window.0.scrolled.width(),
            window.0.scrolled.height()
        )));
        window.fit(false);
        window.present();

        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while window.0.pending_fit.get().is_some() && std::time::Instant::now() < deadline {
            context.iteration(false);
            std::thread::yield_now();
        }

        let viewport = (window.0.scrolled.width(), window.0.scrolled.height());
        assert!(usable_panel_size(viewport));
        assert_eq!(window.0.canvas.zoom(), panel_fit_zoom(viewport, (200, 100)));
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn compare_mode_round_trip_restores_overlay_and_disconnects_session_state() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.CompareLifecycleTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let image = image::RgbaImage::from_pixel(2, 1, image::Rgba([1, 2, 3, 255]));
        let texture = texture_from_rgba(&image).unwrap();
        window.0.canvas.set_texture(Some(&texture));
        window
            .0
            .current_file
            .replace(Some(gio::File::for_path("/images/primary.png")));
        let preview = crate::image::LoadedPreview {
            texture,
            width: 2,
            height: 1,
            metadata: crate::document::Metadata::default(),
            animation_delay: None,
        };
        let comparison = gio::File::for_path("/images/comparison.png");

        for _ in 0..2 {
            window.enter_compare(comparison.clone(), preview.clone());
            assert_eq!(window.0.compare_controllers.borrow().len(), 8);
            assert_eq!(window.0.compare_adjustment_handlers.borrow().len(), 4);

            window.exit_compare();

            assert_eq!(
                window.0.toasts.child(),
                Some(window.0.canvas_overlay.clone().upcast())
            );
            assert_eq!(
                window.0.canvas_overlay.child(),
                Some(window.0.scrolled.clone().upcast())
            );
            assert_eq!(
                window.0.zoom_controls.parent(),
                Some(window.0.canvas_overlay.clone().upcast())
            );
            assert_eq!(
                window.0.transform_controls.parent(),
                Some(window.0.canvas_overlay.clone().upcast())
            );
            assert_eq!(
                window.0.scale_controls.parent(),
                Some(window.0.canvas_overlay.clone().upcast())
            );
            assert_eq!(
                window.0.minimap.parent(),
                Some(window.0.canvas_overlay.clone().upcast())
            );
            assert!(window.0.canvas.cursor().is_none());
            assert!(window.0.compare_controllers.borrow().is_empty());
            assert!(window.0.compare_adjustment_handlers.borrow().is_empty());
        }
    }
}
