use std::cell::{Cell, RefCell};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::canvas::{Background, CropOverlay, ImageCanvas, MiniMap, ZoomFilter};
use crate::compare::{SplitOrientation, choose_split};
use crate::document::{
    Annotation, AnnotationEdit, AnnotationId, Axis, BrushPoint, CancellationToken, Document,
    HIGHLIGHT_STROKE_WIDTH, MEASUREMENT_STROKE_WIDTH, Operation, PencilGeometry, Point, Rect,
    Resampling, Rotation, Shape, Stroke, StrokePath, StrokeStyle,
};
use crate::export::{ExportOptions, JpegOptions, PngOptions};
use crate::i18n::gettext;
use crate::image::{
    AnimationFrame, DecodeLimits, decode_animation, decode_headless, decode_memory, load_preview,
};
use crate::navigation::{DirectorySequence, find_matching_file};
#[cfg(test)]
use crate::settings::ColorFormat;
use crate::settings::{Settings, ZoomMode};
use crate::tools::annotation::highlight::highlight_stroke_width;
use crate::tools::crop::CropBounds;
use adw::prelude::{
    ActionRowExt, AdwApplicationWindowExt, AdwDialogExt, AlertDialogExt, BreakpointBinExt,
    ComboRowExt, PreferencesDialogExt, PreferencesGroupExt, PreferencesPageExt,
};
use gio::prelude::*;
use gtk::prelude::*;
use libadwaita as adw;

mod annotation;
mod color;
mod file_state;
mod presentation;
mod scale;
mod tool;
mod zoom;

use annotation::AnnotationDrag;
use color::{color_format_at, color_format_index, format_color, rgba_to_u8, u8_to_rgba};
use file_state::{
    PendingDirectoryChanges, export_context_matches, files_equal, first_existing_folder,
    is_directory, is_regular_file, merge_directory_change, source_revision_changed,
};
#[cfg(test)]
use presentation::relative_modified_time;
use presentation::{compare_metadata, folder_path, image_subtitle};
use scale::{
    ScaleUnit, dimensions_from_percent, resampling_label, scale_unit, scaled_dimensions,
    scaled_width_for_height,
};
use tool::{Tool, palette_visible, pencil_drag_available, resting_tool};
use zoom::{
    ZoomAlignment, aligned_hard_fit_zoom, aligned_hard_zoom, anchored_adjustment_value,
    centered_adjustment_value, comparison_zoom, fit_on_load, panel_fit_zoom,
    sanitized_render_scale, scale_preview_zoom, stepped_hard_zoom, usable_panel_size,
    zoom_rect_target,
};

#[derive(Clone)]
pub struct ViewerWindow(Rc<WindowState>);

struct HeaderWidgets {
    header: adw::HeaderBar,
    save_as_button: gtk::Button,
    animation_controls: gtk::Box,
    animation_play_button: gtk::Button,
    scale_button: gtk::ToggleButton,
    measurement_button: gtk::ToggleButton,
    highlight_button: gtk::ToggleButton,
    arrow_button: gtk::ToggleButton,
    text_button: gtk::ToggleButton,
    color_picker_button: gtk::ToggleButton,
    pencil_button: gtk::ToggleButton,
    lens_button: gtk::ToggleButton,
    color_button: gtk::ColorDialogButton,
    pencil_size: gtk::SpinButton,
    pencil_controls: gtk::Box,
}

#[derive(Clone)]
struct ExportSnapshot {
    document: Document,
    operations: Arc<[Operation]>,
    source_file: Option<gio::File>,
    load_generation: u64,
}

#[derive(Clone, Copy)]
enum RegionDrag {
    Marking(SelectionDrag),
    Resizing {
        crop: CropOverlay,
        start_screen: (f64, f64),
        left: bool,
        right: bool,
        top: bool,
        bottom: bool,
    },
}

#[derive(Clone, Copy)]
struct SelectionDrag {
    start: (u32, u32),
    current: (u32, u32),
    start_screen: (f64, f64),
    image_dimensions: (u32, u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PencilZoomKey {
    In,
    Out,
}

struct PencilDrag {
    canvas: ImageCanvas,
    start_screen: (f64, f64),
    mode: PencilDragMode,
    origin: BrushPoint,
    line_start: BrushPoint,
    current: BrushPoint,
    freehand_points: Vec<crate::tools::pencil::TimedBrushPoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PencilDragMode {
    Freehand,
    Line,
    Rectangle,
    Circle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyboardTool {
    Highlight,
    Arrow,
    Measure,
    Text,
    Select,
    PickColor,
    Pencil,
}

fn pencil_drag_mode(modifiers: gtk::gdk::ModifierType) -> PencilDragMode {
    if modifiers.contains(gtk::gdk::ModifierType::ALT_MASK) {
        PencilDragMode::Circle
    } else if modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
        PencilDragMode::Rectangle
    } else if modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
        PencilDragMode::Line
    } else {
        PencilDragMode::Freehand
    }
}

fn pencil_line_start(
    mode: PencilDragMode,
    line_anchor: Option<BrushPoint>,
    origin: BrushPoint,
) -> BrushPoint {
    if mode == PencilDragMode::Line {
        line_anchor.unwrap_or(origin)
    } else {
        origin
    }
}

fn pencil_drag_points(drag: &PencilDrag) -> Vec<BrushPoint> {
    match drag.mode {
        PencilDragMode::Freehand => {
            crate::tools::pencil::adaptive_smooth(&drag.freehand_points, 0.3, 1.5, 0.8, 12)
        }
        PencilDragMode::Line => {
            crate::tools::pencil::shape_points(crate::tools::pencil::PencilShape::Line {
                start: drag.line_start,
                end: drag.current,
            })
        }
        PencilDragMode::Rectangle => {
            crate::tools::pencil::shape_points(crate::tools::pencil::PencilShape::Rectangle {
                start: drag.origin,
                end: drag.current,
            })
        }
        PencilDragMode::Circle => {
            crate::tools::pencil::shape_points(crate::tools::pencil::PencilShape::Circle {
                center: drag.origin,
                edge: drag.current,
            })
        }
    }
}

fn pencil_drag_path(mode: PencilDragMode) -> StrokePath {
    match mode {
        PencilDragMode::Freehand => StrokePath::Smooth,
        PencilDragMode::Line | PencilDragMode::Rectangle => StrokePath::Linear,
        PencilDragMode::Circle => StrokePath::Circle,
    }
}

fn pencil_drag_should_preview(mode: PencilDragMode, sampled_pixels: usize) -> bool {
    mode != PencilDragMode::Freehand || sampled_pixels > 1
}

fn pencil_geometry(mode: PencilDragMode, points: &[BrushPoint]) -> Option<PencilGeometry> {
    let first = points.first()?;
    Some(match mode {
        PencilDragMode::Freehand => PencilGeometry::Freehand(points.to_vec()),
        PencilDragMode::Line => {
            let end = points.last()?;
            PencilGeometry::Line(vec![
                Point {
                    x: first.x,
                    y: first.y,
                },
                Point { x: end.x, y: end.y },
            ])
        }
        PencilDragMode::Rectangle => {
            let end = points.get(2).or_else(|| points.last())?;
            PencilGeometry::Rectangle(Rect::from_points(
                Point {
                    x: first.x,
                    y: first.y,
                },
                Point { x: end.x, y: end.y },
            ))
        }
        PencilDragMode::Circle => {
            let edge = points.get(1)?;
            let center = Point {
                x: first.x,
                y: first.y,
            };
            let radius = center.distance(Point {
                x: edge.x,
                y: edge.y,
            });
            PencilGeometry::Ellipse(Rect {
                x: center.x - radius,
                y: center.y - radius,
                width: radius * 2.0,
                height: radius * 2.0,
            })
        }
    })
}

fn pencil_event_time(gesture: &gtk::GestureDrag) -> u32 {
    gesture.current_event().map_or(0, |event| event.time())
}

#[derive(Clone, Copy)]
struct ZoomGestureAnchor {
    start_zoom: f64,
    content_x: f64,
    content_y: f64,
    horizontal_value: f64,
    vertical_value: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalePreviewView {
    Footprint,
    ActualSize,
    Fit,
}

#[derive(Debug, Clone, Copy)]
struct ScaleViewState {
    zoom: f64,
    horizontal: f64,
    vertical: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompareLensSource {
    Primary,
    Comparison,
}

fn region_edge_hit(rect: gtk::graphene::Rect, x: f32, y: f32) -> (bool, bool, bool, bool) {
    const EDGE: f32 = 12.0;
    let within_vertical_span = y >= rect.y() - EDGE && y <= rect.y() + rect.height() + EDGE;
    let within_horizontal_span = x >= rect.x() - EDGE && x <= rect.x() + rect.width() + EDGE;
    let left = within_vertical_span && (x - rect.x()).abs() <= EDGE;
    let right = within_vertical_span && (x - (rect.x() + rect.width())).abs() <= EDGE;
    let top = within_horizontal_span && (y - rect.y()).abs() <= EDGE;
    let bottom = within_horizontal_span && (y - (rect.y() + rect.height())).abs() <= EDGE;
    (left, right, top, bottom)
}

fn region_resize_cursor(rect: gtk::graphene::Rect, x: f32, y: f32) -> &'static str {
    let (left, right, top, bottom) = region_edge_hit(rect, x, y);
    match (left, right, top, bottom) {
        (true, _, true, _) | (_, true, _, true) => "nwse-resize",
        (_, true, true, _) | (true, _, _, true) => "nesw-resize",
        (true, _, _, _) | (_, true, _, _) => "ew-resize",
        (_, _, true, _) | (_, _, _, true) => "ns-resize",
        _ => "default",
    }
}

fn pencil_can_activate(has_image: bool) -> bool {
    has_image
}

fn image_navigation_direction(
    key: gtk::gdk::Key,
    modifiers: gtk::gdk::ModifierType,
    contextual_mode_active: bool,
) -> Option<bool> {
    if contextual_mode_active
        || modifiers.intersects(
            gtk::gdk::ModifierType::SHIFT_MASK
                | gtk::gdk::ModifierType::CONTROL_MASK
                | gtk::gdk::ModifierType::ALT_MASK
                | gtk::gdk::ModifierType::SUPER_MASK
                | gtk::gdk::ModifierType::HYPER_MASK
                | gtk::gdk::ModifierType::META_MASK,
        )
    {
        return None;
    }
    match key {
        gtk::gdk::Key::Left => Some(false),
        gtk::gdk::Key::Right => Some(true),
        _ => None,
    }
}

fn resize_region(
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

fn boundary_overlay(
    start: (u32, u32),
    current: (u32, u32),
    image_dimensions: (u32, u32),
) -> CropOverlay {
    CropOverlay {
        x: start.0.min(current.0),
        y: start.1.min(current.1),
        width: start.0.abs_diff(current.0),
        height: start.1.abs_diff(current.1),
        image_width: image_dimensions.0,
        image_height: image_dimensions.1,
    }
}

fn pencil_zoom_key(
    key: gtk::gdk::Key,
    modifiers: gtk::gdk::ModifierType,
    pencil_active: bool,
) -> Option<PencilZoomKey> {
    if !pencil_active
        || modifiers.intersects(
            gtk::gdk::ModifierType::ALT_MASK
                | gtk::gdk::ModifierType::SUPER_MASK
                | gtk::gdk::ModifierType::HYPER_MASK
                | gtk::gdk::ModifierType::META_MASK,
        )
    {
        return None;
    }
    match key {
        gtk::gdk::Key::plus | gtk::gdk::Key::equal | gtk::gdk::Key::KP_Add => {
            Some(PencilZoomKey::In)
        }
        gtk::gdk::Key::minus | gtk::gdk::Key::KP_Subtract => Some(PencilZoomKey::Out),
        _ => None,
    }
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

fn image_property_row(title: &str, value: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).build();
    let value_label = gtk::Label::builder()
        .label(value)
        .tooltip_text(value)
        .selectable(true)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .max_width_chars(48)
        .halign(gtk::Align::End)
        .valign(gtk::Align::Center)
        .build();
    value_label.add_css_class("dim-label");
    row.add_suffix(&value_label);
    row
}

struct WindowState {
    window: adw::ApplicationWindow,
    canvas: ImageCanvas,
    scrolled: gtk::ScrolledWindow,
    canvas_overlay: gtk::Overlay,
    content_stack: gtk::Stack,
    error_page: adw::StatusPage,
    view_only_banner: adw::Banner,
    title: adw::WindowTitle,
    toasts: adw::ToastOverlay,
    settings: Settings,
    current_file: RefCell<Option<gio::File>>,
    sequence: RefCell<Option<DirectorySequence>>,
    explicit_navigation: Cell<bool>,
    cancellable: RefCell<Option<gio::Cancellable>>,
    render_cancellation: RefCell<Option<CancellationToken>>,
    load_generation: Cell<u64>,
    render_generation: Cell<u64>,
    document: RefCell<Option<Document>>,
    rendered: RefCell<Option<image::RgbaImage>>,
    editable_decode_pending: Cell<bool>,
    pending_scale_activation: Cell<bool>,
    source_modified: RefCell<Option<SystemTime>>,
    external_source_conflict: Cell<bool>,
    subtitle_ready: Cell<bool>,
    close_approved: Cell<bool>,
    tool: Cell<Tool>,
    return_tool: Cell<Option<Tool>>,
    updating_tool: Cell<bool>,
    selected_annotation: Cell<Option<AnnotationId>>,
    nudge_annotation: Cell<Option<AnnotationId>>,
    annotation_drag: RefCell<Option<AnnotationDrag>>,
    annotation_preview: RefCell<Option<Annotation>>,
    annotation_preview_queue: RefCell<annotation::PreviewQueue<Annotation>>,
    text_editor: RefCell<Option<InlineTextEditor>>,
    pencil_points: RefCell<Vec<BrushPoint>>,
    pencil_path: Cell<StrokePath>,
    pencil_drag: RefCell<Option<PencilDrag>>,
    pencil_line_anchor: Cell<Option<BrushPoint>>,
    pencil_line_annotation: Cell<Option<AnnotationId>>,
    keyboard_tool_cursor: Cell<Option<(u32, u32)>>,
    keyboard_tool_anchor: Cell<Option<(u32, u32)>>,
    pencil_color: Cell<[u8; 4]>,
    pencil_antialiasing: Cell<bool>,
    line_width: Cell<f64>,
    pencil_size: gtk::SpinButton,
    pencil_controls: gtk::Box,
    measurement_button: gtk::ToggleButton,
    highlight_button: gtk::ToggleButton,
    arrow_button: gtk::ToggleButton,
    text_button: gtk::ToggleButton,
    region_selection: Cell<Option<CropOverlay>>,
    region_drag: Cell<Option<RegionDrag>>,
    region_controls: gtk::Box,
    color_picker_button: gtk::ToggleButton,
    pencil_button: gtk::ToggleButton,
    lens_button: gtk::ToggleButton,
    color_button: gtk::ColorDialogButton,
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
    pending_fit: Cell<Option<bool>>,
    zoom_mode: Cell<ZoomMode>,
    fit_tick_scheduled: Cell<bool>,
    zoom_controls: gtk::Box,
    zoom_label: gtk::MenuButton,
    render_scale: Cell<f64>,
    scale_button: gtk::ToggleButton,
    scale_controls: gtk::Box,
    scale_slider: gtk::Scale,
    scale_value_label: gtk::Label,
    scale_spinner: adw::Spinner,
    scale_width: gtk::SpinButton,
    scale_height: gtk::SpinButton,
    scale_lock: gtk::ToggleButton,
    scale_unit: gtk::DropDown,
    scale_algorithm_label: gtk::Label,
    scale_original_button: gtk::Button,
    scale_source: RefCell<Option<Arc<image::RgbaImage>>>,
    scale_preview: RefCell<Option<Arc<image::RgbaImage>>>,
    scale_source_view: Cell<Option<ScaleViewState>>,
    scale_preview_view: Cell<ScalePreviewView>,
    scale_preview_zoom_before_original: Cell<f64>,
    scale_showing_original: Cell<bool>,
    scale_committing: Cell<bool>,
    scale_updating_controls: Cell<bool>,
    scale_resampling: Cell<Resampling>,
    scale_preview_generation: Cell<u64>,
    scale_preview_cancellation: RefCell<Option<CancellationToken>>,
    minimap: MiniMap,
}

struct InlineTextEditor {
    widget: gtk::Text,
    anchor: Point,
    font_size: f32,
    _accelerator_suppression: Option<crate::application::AcceleratorSuppression>,
}

impl ViewerWindow {
    pub fn new(application: &adw::Application, file: Option<gio::File>) -> Self {
        let files = file.into_iter().collect::<Vec<_>>();
        Self::new_with_files(application, &files)
    }

    pub fn new_with_files(application: &adw::Application, files: &[gio::File]) -> Self {
        add_development_icon_search_path();
        let initial_file = files.first().cloned();
        let initial_sequence = if files.len() > 1 {
            DirectorySequence::from_files(files)
        } else {
            None
        };
        let explicit_navigation = initial_sequence.is_some();
        let settings = Settings::default();
        let scale_resampling = settings.scale_resampling();
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
        let region_controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        region_controls.add_css_class("toolbar");
        region_controls.add_css_class("osd");
        region_controls.set_visible(false);
        region_controls.set_halign(gtk::Align::Start);
        region_controls.set_valign(gtk::Align::End);
        region_controls.set_margin_start(26);
        region_controls.set_margin_bottom(26);
        region_controls.append(&button(
            "zoom-in-symbolic",
            "Zoom to Selected Region",
            "win.selection-zoom",
        ));
        region_controls.append(&button(
            "edit-cut-symbolic",
            "Crop to Selected Region",
            "win.selection-crop",
        ));
        region_controls.append(&button(
            "edit-copy-symbolic",
            "Copy Selected Region",
            "win.selection-copy",
        ));
        canvas_overlay.add_overlay(&region_controls);
        let zoom_controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        zoom_controls.add_css_class("toolbar");
        zoom_controls.add_css_class("osd");
        zoom_controls.set_halign(gtk::Align::End);
        zoom_controls.set_valign(gtk::Align::End);
        zoom_controls.set_margin_end(26);
        zoom_controls.set_margin_bottom(26);
        zoom_controls.append(&button("zoom-out-symbolic", "Zoom Out", "win.zoom-out"));
        let zoom_label = gtk::MenuButton::builder()
            .label("100%")
            .tooltip_text(gettext("Zoom presets (0: Fit; 1–9: 100%–900%)"))
            .build();
        zoom_label.set_margin_start(8);
        zoom_label.set_margin_end(8);
        let zoom_menu = gio::Menu::new();
        zoom_menu.append(Some(&gettext("Fit to Window (0)")), Some("win.fit"));
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
        let scale_controls = gtk::Box::new(gtk::Orientation::Vertical, 0);
        scale_controls.set_visible(false);
        scale_controls.set_halign(gtk::Align::Fill);
        scale_controls.set_valign(gtk::Align::End);
        scale_controls.set_hexpand(true);
        scale_controls.set_margin_start(26);
        scale_controls.set_margin_end(26);
        scale_controls.set_margin_bottom(26);
        let scale_surface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        scale_surface.add_css_class("toolbar");
        scale_surface.add_css_class("osd");
        scale_surface.set_hexpand(true);
        let scale_content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        scale_content.set_hexpand(true);
        scale_content.set_margin_start(12);
        scale_content.set_margin_end(12);
        scale_content.set_margin_top(12);
        scale_content.set_margin_bottom(12);
        let scale_control_row = adw::WrapBox::builder()
            .align(0.5)
            .child_spacing(6)
            .line_spacing(6)
            .build();
        scale_control_row.set_hexpand(true);
        let scale_width = spin(1.0, 2.0, 1.0);
        scale_width.set_width_chars(6);
        scale_width.set_tooltip_text(Some(&gettext("Output width in pixels")));
        let scale_height = spin(1.0, 2.0, 1.0);
        scale_height.set_width_chars(6);
        scale_height.set_tooltip_text(Some(&gettext("Output height in pixels")));
        let scale_lock = gtk::ToggleButton::builder()
            .label(gettext("Aspect"))
            .tooltip_text(gettext("Lock the source aspect ratio"))
            .active(true)
            .build();
        let scale_units = [gettext("Pixels"), gettext("Percent")];
        let scale_unit = gtk::DropDown::from_strings(
            &scale_units.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        scale_unit.set_tooltip_text(Some(&gettext("Slider unit")));
        let scale_algorithm_label = gtk::Label::new(Some(
            &gettext("{method} · Properties")
                .replace("{method}", &gettext(resampling_label(scale_resampling))),
        ));
        scale_algorithm_label.add_css_class("dim-label");
        scale_algorithm_label.set_tooltip_text(Some(&gettext("Scaling method from Properties")));
        let scale_original_button = gtk::Button::with_label(&gettext("Hold Original"));
        scale_original_button.set_tooltip_text(Some(&gettext(
            "Press and hold to show the unscaled source image",
        )));
        let scale_actual_button = gtk::Button::builder()
            .label(gettext("Actual Pixels"))
            .tooltip_text(gettext("Show each scaled output pixel at its actual size"))
            .action_name("win.scale-actual-size")
            .build();
        let scale_fit_button = gtk::Button::builder()
            .label(gettext("Fit Preview"))
            .tooltip_text(gettext("Fit the complete scaled preview in the window"))
            .action_name("win.scale-fit")
            .build();
        let scale_dimensions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let scale_width_label = gtk::Label::new(Some("W"));
        scale_dimensions.append(&scale_width_label);
        scale_dimensions.append(&scale_width);
        scale_dimensions.append(&gtk::Label::new(Some("× H")));
        scale_dimensions.append(&scale_height);
        scale_dimensions.append(&scale_lock);
        scale_algorithm_label.set_margin_start(8);
        scale_algorithm_label.set_margin_end(8);
        scale_algorithm_label.set_margin_top(6);
        scale_algorithm_label.set_margin_bottom(6);
        let scale_view_controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        scale_view_controls.add_css_class("linked");
        scale_view_controls.append(&scale_original_button);
        scale_view_controls.append(&scale_actual_button);
        scale_view_controls.append(&scale_fit_button);
        let scale_cancel_button =
            button("window-close-symbolic", "Cancel Scale", "win.cancel-scale");
        scale_control_row.append(&scale_cancel_button);
        scale_control_row.append(&scale_dimensions);
        scale_control_row.append(&scale_unit);
        scale_control_row.append(&scale_algorithm_label);
        scale_control_row.append(&scale_view_controls);
        let scale_apply_button =
            button("object-select-symbolic", "Apply Scale", "win.confirm-scale");
        scale_control_row.append(&scale_apply_button);
        let scale_slider_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        scale_slider_row.set_hexpand(true);
        let scale_value_label = gtk::Label::new(Some("1 × 1 → 1 × 1 (100%)"));
        let scale_spinner = adw::Spinner::new();
        scale_spinner.set_visible(false);
        scale_spinner.set_tooltip_text(Some(&gettext("Generating scale preview")));
        let scale_slider = gtk::Scale::with_range(gtk::Orientation::Horizontal, 1.0, 2.0, 1.0);
        scale_slider.set_hexpand(true);
        scale_slider.set_draw_value(false);
        scale_slider.set_tooltip_text(Some(&gettext("Scaled width in pixels")));
        scale_slider_row.append(&scale_value_label);
        scale_slider_row.append(&scale_spinner);
        scale_slider_row.append(&scale_slider);
        scale_content.append(&scale_control_row);
        scale_content.append(&scale_slider_row);
        scale_surface.append(&scale_content);
        scale_controls.append(&scale_surface);
        canvas_overlay.add_overlay(&scale_controls);
        let minimap = MiniMap::default();
        minimap.set_size_request(160, 120);
        minimap.set_halign(gtk::Align::Start);
        minimap.set_valign(gtk::Align::Start);
        minimap.set_margin_start(20);
        minimap.set_margin_top(20);
        minimap.set_tooltip_text(Some(&gettext("Image overview — click to pan")));
        minimap.set_visible(false);
        canvas_overlay.add_overlay(&minimap);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&canvas_overlay));

        let open_button = gtk::Button::builder()
            .label(gettext("Open Image"))
            .action_name("win.open")
            .halign(gtk::Align::Center)
            .build();
        open_button.add_css_class("suggested-action");
        open_button.add_css_class("pill");
        let empty_page = adw::StatusPage::builder()
            .icon_name("image-x-generic-symbolic")
            .title(gettext("Open an Image"))
            .description(gettext("Choose an image to view, compare, or edit."))
            .child(&open_button)
            .build();
        let loading_spinner = adw::Spinner::new();
        loading_spinner.set_halign(gtk::Align::Center);
        let loading_page = adw::StatusPage::builder()
            .title(gettext("Loading Image"))
            .description(gettext("Preparing the image for viewing."))
            .child(&loading_spinner)
            .build();
        let retry_button = gtk::Button::builder()
            .label(gettext("Open Another Image"))
            .action_name("win.open")
            .halign(gtk::Align::Center)
            .build();
        retry_button.add_css_class("suggested-action");
        retry_button.add_css_class("pill");
        let error_page = adw::StatusPage::builder()
            .icon_name("dialog-error-symbolic")
            .title(gettext("Could Not Open Image"))
            .description(gettext("The image could not be loaded."))
            .child(&retry_button)
            .build();
        let content_stack = gtk::Stack::builder()
            .hexpand(true)
            .vexpand(true)
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        content_stack.add_named(&empty_page, Some("empty"));
        content_stack.add_named(&loading_page, Some("loading"));
        content_stack.add_named(&error_page, Some("error"));
        content_stack.add_named(&toasts, Some("viewer"));
        content_stack.set_visible_child_name("empty");

        let title = adw::WindowTitle::builder()
            .title("Diorama")
            .subtitle(gettext("Open an image to begin"))
            .build();
        let header_widgets = build_header(&title);
        header_widgets
            .pencil_size
            .set_value(f64::from(settings.pencil_size()));
        header_widgets.pencil_controls.set_visible(false);
        header_widgets.pencil_controls.set_halign(gtk::Align::Start);
        header_widgets.pencil_controls.set_valign(gtk::Align::End);
        header_widgets.pencil_controls.set_margin_start(26);
        header_widgets.pencil_controls.set_margin_bottom(26);
        canvas_overlay.add_overlay(&header_widgets.pencil_controls);
        let view_only_banner = adw::Banner::builder()
            .title(gettext(
                "This image can be viewed, but its decoder does not support editing.",
            ))
            .revealed(false)
            .build();
        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header_widgets.header);
        toolbar_view.add_top_bar(&view_only_banner);
        toolbar_view.set_content(Some(&content_stack));
        let (width, height) = settings.window_size();
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title("Diorama")
            .default_width(width)
            .default_height(height)
            .content(&toolbar_view)
            .build();
        let compact = adw::Breakpoint::new(adw::BreakpointCondition::new_or(
            adw::BreakpointCondition::parse("max-width: 500px")
                .expect("valid narrow-window breakpoint"),
            adw::BreakpointCondition::parse("max-height: 400px")
                .expect("valid short-window breakpoint"),
        ));
        compact.add_setter(
            &header_widgets.save_as_button,
            "visible",
            Some(&false.to_value()),
        );
        compact.add_setter(&zoom_controls, "visible", Some(&false.to_value()));
        compact.add_setter(&title, "subtitle", Some(&"".to_value()));
        compact.add_setter(
            &empty_page,
            "icon-name",
            Some(&Option::<String>::None.to_value()),
        );
        compact.add_setter(
            &error_page,
            "icon-name",
            Some(&Option::<String>::None.to_value()),
        );
        window.add_breakpoint(compact);
        if settings.maximized() {
            window.maximize();
        }

        let lens_diameter = settings.compare_lens_size();
        let lens_magnification = settings.compare_lens_magnification();
        let pencil_antialiasing = settings.pencil_antialiasing();
        let line_width = f64::from(settings.pencil_size());
        let zoom_mode = settings.last_zoom_mode();
        let this = Self(Rc::new(WindowState {
            window,
            canvas,
            scrolled,
            canvas_overlay,
            content_stack,
            error_page,
            view_only_banner,
            title,
            toasts,
            settings,
            current_file: RefCell::new(None),
            sequence: RefCell::new(initial_sequence),
            explicit_navigation: Cell::new(explicit_navigation),
            cancellable: RefCell::new(None),
            render_cancellation: RefCell::new(None),
            load_generation: Cell::new(0),
            render_generation: Cell::new(0),
            document: RefCell::new(None),
            rendered: RefCell::new(None),
            editable_decode_pending: Cell::new(false),
            pending_scale_activation: Cell::new(false),
            source_modified: RefCell::new(None),
            external_source_conflict: Cell::new(false),
            subtitle_ready: Cell::new(false),
            close_approved: Cell::new(false),
            tool: Cell::new(Tool::None),
            return_tool: Cell::new(None),
            updating_tool: Cell::new(false),
            selected_annotation: Cell::new(None),
            nudge_annotation: Cell::new(None),
            annotation_drag: RefCell::new(None),
            annotation_preview: RefCell::new(None),
            annotation_preview_queue: RefCell::new(annotation::PreviewQueue::default()),
            text_editor: RefCell::new(None),
            pencil_points: RefCell::new(Vec::new()),
            pencil_path: Cell::new(StrokePath::Smooth),
            pencil_drag: RefCell::new(None),
            pencil_line_anchor: Cell::new(None),
            pencil_line_annotation: Cell::new(None),
            keyboard_tool_cursor: Cell::new(None),
            keyboard_tool_anchor: Cell::new(None),
            pencil_color: Cell::new(crate::tools::annotation::DEFAULT_ANNOTATION_COLOR),
            pencil_antialiasing: Cell::new(pencil_antialiasing),
            line_width: Cell::new(line_width),
            pencil_size: header_widgets.pencil_size,
            pencil_controls: header_widgets.pencil_controls,
            measurement_button: header_widgets.measurement_button,
            highlight_button: header_widgets.highlight_button,
            arrow_button: header_widgets.arrow_button,
            text_button: header_widgets.text_button,
            region_selection: Cell::new(None),
            region_drag: Cell::new(None),
            region_controls,
            color_picker_button: header_widgets.color_picker_button,
            pencil_button: header_widgets.pencil_button,
            lens_button: header_widgets.lens_button,
            color_button: header_widgets.color_button,
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
            pending_fit: Cell::new(None),
            zoom_mode: Cell::new(zoom_mode),
            fit_tick_scheduled: Cell::new(false),
            zoom_controls,
            zoom_label,
            render_scale: Cell::new(1.0),
            scale_button: header_widgets.scale_button,
            scale_controls,
            scale_slider,
            scale_value_label,
            scale_spinner,
            scale_width,
            scale_height,
            scale_lock,
            scale_unit,
            scale_algorithm_label,
            scale_original_button,
            scale_source: RefCell::new(None),
            scale_preview: RefCell::new(None),
            scale_source_view: Cell::new(None),
            scale_preview_view: Cell::new(ScalePreviewView::Footprint),
            scale_preview_zoom_before_original: Cell::new(1.0),
            scale_showing_original: Cell::new(false),
            scale_committing: Cell::new(false),
            scale_updating_controls: Cell::new(false),
            scale_resampling: Cell::new(scale_resampling),
            scale_preview_generation: Cell::new(0),
            scale_preview_cancellation: RefCell::new(None),
            minimap,
            export_cancellation: RefCell::new(None),
            export_generation: Cell::new(0),
            export_lock: Arc::new(Mutex::new(())),
            deletion_running: Cell::new(false),
        }));
        this.install_actions();
        this.update_action_states();
        this.install_navigation_keys();
        this.install_tool_controls();
        this.install_scale_controls();
        this.install_gestures();
        this.install_annotation_controls();
        this.install_render_scale_tracking();
        this.install_minimap();
        this.connect_single_image_lens();
        this.install_state_persistence();
        this.install_subtitle_clock();
        if let Some(file) = initial_file {
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
        self.load_with_fit(file, true);
    }

    fn load_preserving_zoom(&self, file: gio::File) {
        self.load_with_fit(file, false);
    }

    fn load_from_new_scope(&self, file: gio::File) {
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
                this.load_from_new_scope(file.clone());
            });
            return;
        }
        self.0.explicit_navigation.set(false);
        self.0.sequence.replace(None);
        self.load(file);
    }

    fn load_with_fit(&self, file: gio::File, fit: bool) {
        let fit_on_load = fit_on_load(fit, self.0.zoom_mode.get());
        self.clear_region_selection();
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
                this.load_with_fit(file.clone(), fit);
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
        self.0.document.replace(None);
        self.0.rendered.replace(None);
        self.set_tool(Tool::None);
        self.select_annotation(None);
        self.exit_compare();
        self.0
            .directory_refresh_generation
            .set(self.0.directory_refresh_generation.get().wrapping_add(1));
        self.0
            .pending_directory_changes
            .replace(PendingDirectoryChanges::default());
        self.0.directory_refresh_scheduled.set(false);
        if !self.0.explicit_navigation.get() {
            self.0.sequence.replace(None);
        }
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
        self.0.editable_decode_pending.set(true);
        self.0.pending_scale_activation.set(false);
        self.0.external_source_conflict.set(false);
        self.0.canvas.clear_annotation_previews();
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
        self.0.title.set_subtitle(&gettext("Loading…"));
        self.0.view_only_banner.set_revealed(false);
        self.0.content_stack.set_visible_child_name("loading");
        self.update_action_states();

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
                    state.content_stack.set_visible_child_name("viewer");
                    ViewerWindow(state.clone()).update_action_states();
                    if let Some(fill) = fit_on_load {
                        ViewerWindow(state.clone()).fit(fill);
                    }
                    ViewerWindow(state.clone()).update_subtitle();
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
                                ViewerWindow(state.clone()).finish_editable_decode(false);
                                state.toasts.add_toast(adw::Toast::new(&gettext(
                                    "This image can be viewed but could not be read for editing",
                                )));
                                return;
                            }
                        }
                    };
                    if state.load_generation.get() != generation || cancellable.is_cancelled() {
                        return;
                    }
                    let editable_available = match editable {
                        Ok(Ok(mut source)) => {
                            source.metadata = preview.metadata.clone();
                            let document = Document::new(source);
                            state
                                .rendered
                                .replace(Some(document.source().pixels.as_ref().clone()));
                            state.document.replace(Some(document));
                            true
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(%error, "Editable decode unavailable");
                            false
                        }
                        Err(_) => {
                            tracing::warn!("Editable decode worker panicked");
                            state.toasts.add_toast(adw::Toast::new(&gettext(
                                "Could not prepare image for editing",
                            )));
                            false
                        }
                    };
                    ViewerWindow(state.clone()).finish_editable_decode(editable_available);
                    if let Some(compare_file) = state.pending_comparison.borrow_mut().take() {
                        ViewerWindow(state.clone()).load_comparison(compare_file);
                    }
                    ViewerWindow(state).rebuild_navigation(file);
                }
                Err(error) => {
                    ViewerWindow(state.clone()).finish_editable_decode(false);
                    state.pending_comparison.borrow_mut().take();
                    state.title.set_subtitle(&gettext("Could not open image"));
                    state.error_page.set_description(Some(&gettext(
                        "Check that the file is a supported image and try again.",
                    )));
                    state.content_stack.set_visible_child_name("error");
                    tracing::warn!(%error, "Could not open image");
                    ViewerWindow(state).update_action_states();
                }
            }
        });
    }

    fn install_actions(&self) {
        self.add_action("open", {
            let this = self.clone();
            move || {
                let mut builder = gtk::FileDialog::builder()
                    .title(gettext("Open Image"))
                    .modal(true);
                if let Some(folder) = this.preferred_initial_folder() {
                    builder = builder.initial_folder(&folder);
                }
                let dialog = builder.build();
                let parent = this.0.window.clone();
                let this = this.clone();
                glib::spawn_future_local(async move {
                    if let Ok(file) = dialog.open_future(Some(&parent)).await {
                        this.load_from_new_scope(file);
                    }
                });
            }
        });
        self.add_action("open-with", {
            let this = self.clone();
            move || this.open_with()
        });
        self.add_action("close", {
            let window = self.0.window.clone();
            move || window.close()
        });
        self.add_action("copy-image", {
            let this = self.clone();
            move || this.copy_current_selection_or_image_to_clipboard()
        });
        self.add_action("zoom-in", {
            let this = self.clone();
            move || this.step_zoom(true)
        });
        self.add_action("zoom-out", {
            let this = self.clone();
            move || this.step_zoom(false)
        });
        self.add_action("actual-size", {
            let this = self.clone();
            move || this.set_zoom_centered(1.0)
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
            self.add_action(name, move || this.set_zoom_centered(zoom));
        }
        self.add_action("fit", {
            let this = self.clone();
            move || {
                if this.0.tool.get() == Tool::Scale {
                    this.0.scale_preview_view.set(ScalePreviewView::Fit);
                    this.fit(false);
                } else {
                    this.set_fit_mode(false);
                }
            }
        });
        self.add_action("fill", {
            let this = self.clone();
            move || this.set_fit_mode(true)
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
                this.realign_zoom();
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
                this.0.nudge_annotation.set(None);
                let changed = this
                    .0
                    .document
                    .borrow_mut()
                    .as_mut()
                    .is_some_and(Document::undo);
                if changed {
                    this.render_document();
                }
                this.update_action_states();
            }
        });
        self.add_action("redo", {
            let this = self.clone();
            move || {
                this.0.nudge_annotation.set(None);
                let changed = this
                    .0
                    .document
                    .borrow_mut()
                    .as_mut()
                    .is_some_and(Document::redo);
                if changed {
                    this.render_document();
                }
                this.update_action_states();
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
        self.add_action("measure", {
            let this = self.clone();
            move || this.toggle_tool(Tool::Measure)
        });
        self.add_action("highlight", {
            let this = self.clone();
            move || this.toggle_tool(Tool::Highlight)
        });
        self.add_action("arrow", {
            let this = self.clone();
            move || this.toggle_tool(Tool::Arrow)
        });
        self.add_action("text", {
            let this = self.clone();
            move || this.toggle_tool(Tool::Text)
        });
        self.add_action("scale-preview", {
            let this = self.clone();
            move || this.toggle_tool(Tool::Scale)
        });
        self.add_action("confirm-scale", {
            let this = self.clone();
            move || this.confirm_scale_preview()
        });
        self.add_action("cancel-scale", {
            let this = self.clone();
            move || {
                if this.0.tool.get() == Tool::Scale {
                    this.set_tool(Tool::None);
                }
            }
        });
        self.add_action("scale-actual-size", {
            let this = self.clone();
            move || {
                if this.0.tool.get() != Tool::Scale {
                    return;
                }
                this.0.scale_preview_view.set(ScalePreviewView::ActualSize);
                this.set_zoom(1.0);
            }
        });
        self.add_action("scale-fit", {
            let this = self.clone();
            move || {
                if this.0.tool.get() != Tool::Scale {
                    return;
                }
                this.0.scale_preview_view.set(ScalePreviewView::Fit);
                this.fit(false);
            }
        });
        self.add_action("crop-content", {
            let this = self.clone();
            move || this.crop_to_content()
        });
        self.add_action("scale", {
            let this = self.clone();
            move || this.set_tool(Tool::Scale)
        });
        self.add_action("palette", {
            let this = self.clone();
            move || this.show_palette_dialog()
        });
        self.add_action("pencil", {
            let this = self.clone();
            move || this.toggle_tool(Tool::Pencil)
        });
        self.add_action("pick-color", {
            let this = self.clone();
            move || this.toggle_tool(Tool::PickColor)
        });
        let tool_action = gio::SimpleAction::new_stateful(
            "tool",
            Some(&String::static_variant_type()),
            &Tool::None.name().to_variant(),
        );
        tool_action.connect_activate({
            let this = self.clone();
            move |_, parameter| {
                if let Some(tool) = parameter
                    .and_then(glib::Variant::str)
                    .and_then(Tool::from_name)
                {
                    this.set_tool(tool);
                }
            }
        });
        self.0.window.add_action(&tool_action);
        self.add_action("cancel-tool", {
            let this = self.clone();
            move || {
                if this.0.tool.get() == Tool::Select
                    && (this.0.region_drag.get().is_some()
                        || this.0.region_selection.get().is_some())
                {
                    this.clear_region_selection();
                    return;
                }
                if this.close_text_editor() || this.cancel_annotation_drag() {
                    return;
                }
                if this.0.selected_annotation.get().is_some() {
                    this.select_annotation(None);
                    return;
                }
                if this.0.tool.get() == Tool::PickColor
                    && let Some(return_tool) = this.0.return_tool.get()
                {
                    this.set_tool(return_tool);
                    return;
                }
                if matches!(this.0.tool.get(), Tool::None | Tool::Select) {
                    return;
                }
                this.set_tool(Tool::None);
                this.0.pencil_points.borrow_mut().clear();
                this.0
                    .toasts
                    .add_toast(adw::Toast::new(&gettext("Active tool cancelled")));
            }
        });
        self.add_action("properties", {
            let this = self.clone();
            move || this.show_properties()
        });
        self.add_action("preferences", {
            let this = self.clone();
            move || this.show_preferences()
        });
        self.add_action("shortcuts", {
            let this = self.clone();
            move || this.show_shortcuts()
        });
        self.add_action("about", {
            let window = self.0.window.clone();
            move || {
                let dialog = adw::AboutDialog::builder()
                    .application_name("Diorama")
                    .application_icon(crate::APP_ID)
                    .version(env!("CARGO_PKG_VERSION"))
                    .developer_name(gettext("Diorama contributors"))
                    .license_type(gtk::License::Gpl30)
                    .website("https://github.com/mendrik-private/diorama")
                    .issue_url("https://github.com/mendrik-private/diorama/issues")
                    .build();
                dialog.add_legal_section(
                    "Excalifont",
                    Some("Copyright 2024 Excalidraw"),
                    gtk::License::Custom,
                    Some(include_str!("../../data/fonts/OFL-Excalifont.txt")),
                );
                dialog.present(Some(&window));
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
        self.add_action("select", {
            let this = self.clone();
            move || this.toggle_tool(Tool::Select)
        });
        self.add_action("selection-zoom", {
            let this = self.clone();
            move || {
                this.zoom_selected_region();
            }
        });
        self.add_action("selection-crop", {
            let this = self.clone();
            move || this.crop_selected_region()
        });
        self.add_action("selection-copy", {
            let this = self.clone();
            move || this.copy_selected_region()
        });
    }

    fn install_navigation_keys(&self) {
        let pencil_zoom = gtk::EventControllerKey::new();
        pencil_zoom.set_propagation_phase(gtk::PropagationPhase::Capture);
        pencil_zoom.connect_key_pressed({
            let this = self.clone();
            move |_, key, _, modifiers| {
                let Some(direction) =
                    pencil_zoom_key(key, modifiers, this.0.tool.get() == Tool::Pencil)
                else {
                    return glib::Propagation::Proceed;
                };
                this.step_zoom(direction == PencilZoomKey::In);
                glib::Propagation::Stop
            }
        });
        self.0.window.add_controller(pencil_zoom);

        let annotation_keys = gtk::EventControllerKey::new();
        annotation_keys.set_propagation_phase(gtk::PropagationPhase::Bubble);
        annotation_keys.connect_key_pressed({
            let this = self.clone();
            move |_, key, _, modifiers| {
                if modifiers.intersects(
                    gtk::gdk::ModifierType::CONTROL_MASK
                        | gtk::gdk::ModifierType::ALT_MASK
                        | gtk::gdk::ModifierType::SUPER_MASK
                        | gtk::gdk::ModifierType::HYPER_MASK
                        | gtk::gdk::ModifierType::META_MASK,
                ) || gtk::prelude::GtkWindowExt::focus(&this.0.window).is_some_and(|focus| {
                    focus.is::<gtk::Text>()
                        || focus.is::<gtk::Entry>()
                        || focus.is::<gtk::TextView>()
                        || focus.is::<gtk::SpinButton>()
                }) {
                    return glib::Propagation::Proceed;
                }
                if this.handle_annotation_key(key, modifiers) {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        });
        self.0.window.add_controller(annotation_keys);

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let this = self.clone();
            move |_, key, _, modifiers| {
                if modifiers.intersects(
                    gtk::gdk::ModifierType::CONTROL_MASK
                        | gtk::gdk::ModifierType::ALT_MASK
                        | gtk::gdk::ModifierType::SUPER_MASK
                        | gtk::gdk::ModifierType::HYPER_MASK
                        | gtk::gdk::ModifierType::META_MASK,
                ) {
                    return glib::Propagation::Proceed;
                }
                if this.handle_annotation_key(key, modifiers) {
                    return glib::Propagation::Stop;
                }
                if this.0.tool.get() == Tool::Scale
                    && (key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter)
                {
                    this.confirm_scale_preview();
                    return glib::Propagation::Stop;
                }
                if matches!(
                    key,
                    gtk::gdk::Key::space | gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter
                ) && this.zoom_selected_region()
                {
                    return glib::Propagation::Stop;
                }
                if this.active_keyboard_tool().is_some() {
                    let step = if modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
                        10
                    } else {
                        1
                    };
                    let moved = match key {
                        gtk::gdk::Key::Left => this.move_keyboard_tool_cursor(-step, 0),
                        gtk::gdk::Key::Right => this.move_keyboard_tool_cursor(step, 0),
                        gtk::gdk::Key::Up => this.move_keyboard_tool_cursor(0, -step),
                        gtk::gdk::Key::Down => this.move_keyboard_tool_cursor(0, step),
                        _ => false,
                    };
                    if moved {
                        return glib::Propagation::Stop;
                    }
                    if matches!(
                        key,
                        gtk::gdk::Key::space | gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter
                    ) {
                        this.activate_keyboard_tool();
                        return glib::Propagation::Stop;
                    }
                }
                glib::Propagation::Proceed
            }
        });
        keys.connect_key_released({
            let this = self.clone();
            move |_, key, _, _| {
                if key == gtk::gdk::Key::Control_L || key == gtk::gdk::Key::Control_R {
                    this.abort_pencil_line();
                }
            }
        });
        self.0.canvas.add_controller(keys);

        let navigation = gtk::EventControllerKey::new();
        navigation.connect_key_pressed({
            let this = self.clone();
            move |_, key, _, modifiers| {
                let contextual_mode_active =
                    this.active_keyboard_tool().is_some() || this.0.tool.get() == Tool::Scale;
                let Some(forward) =
                    image_navigation_direction(key, modifiers, contextual_mode_active)
                else {
                    return glib::Propagation::Proceed;
                };
                this.navigate(forward);
                glib::Propagation::Stop
            }
        });
        self.0.window.add_controller(navigation);
    }

    fn active_keyboard_tool(&self) -> Option<KeyboardTool> {
        match self.0.tool.get() {
            Tool::Highlight => Some(KeyboardTool::Highlight),
            Tool::Arrow => Some(KeyboardTool::Arrow),
            Tool::Measure => Some(KeyboardTool::Measure),
            Tool::Text => Some(KeyboardTool::Text),
            Tool::Select => Some(KeyboardTool::Select),
            Tool::PickColor => Some(KeyboardTool::PickColor),
            Tool::Pencil => Some(KeyboardTool::Pencil),
            Tool::None | Tool::Scale => None,
        }
    }

    fn keyboard_tool_dimensions(&self, tool: KeyboardTool) -> Option<(u32, u32)> {
        let image = self.0.rendered.borrow();
        let (width, height) = image.as_ref().map(image::GenericImageView::dimensions)?;
        Some(if tool == KeyboardTool::Measure {
            (width, height)
        } else {
            (width.saturating_sub(1), height.saturating_sub(1))
        })
    }

    fn prepare_keyboard_tool(&self, active: bool) {
        self.0.keyboard_tool_cursor.set(None);
        self.0.keyboard_tool_anchor.set(None);
        self.0.canvas.set_marker(None);
        self.0.canvas.set_measurement_cursor(None);
        if !active {
            return;
        }
        let Some(tool) = self.active_keyboard_tool() else {
            return;
        };
        if self.keyboard_tool_dimensions(tool).is_none() {
            return;
        }
        self.0.canvas.grab_focus();
        self.0.canvas.announce(
            &gettext(
                "Use the arrow keys to move, Shift with an arrow to move faster, and Space or Enter to act.",
            ),
            gtk::AccessibleAnnouncementPriority::Medium,
        );
    }

    fn move_keyboard_tool_cursor(&self, dx: i32, dy: i32) -> bool {
        let Some(tool) = self.active_keyboard_tool() else {
            return false;
        };
        let Some((max_x, max_y)) = self.keyboard_tool_dimensions(tool) else {
            return false;
        };
        let (x, y) = self
            .0
            .keyboard_tool_cursor
            .get()
            .unwrap_or((max_x / 2, max_y / 2));
        let x = x.saturating_add_signed(dx).min(max_x);
        let y = y.saturating_add_signed(dy).min(max_y);
        self.0.keyboard_tool_cursor.set(Some((x, y)));
        self.update_keyboard_tool_cursor(true);
        true
    }

    fn update_keyboard_tool_cursor(&self, announce: bool) {
        let Some(tool) = self.active_keyboard_tool() else {
            return;
        };
        let Some((x, y)) = self.0.keyboard_tool_cursor.get() else {
            return;
        };
        let Some(image) = self.0.rendered.borrow().as_ref().cloned() else {
            return;
        };
        let (width, height) = image.dimensions();
        if tool == KeyboardTool::Measure {
            self.0.canvas.set_marker(None);
            self.0.canvas.set_measurement_cursor(Some((
                x as f32 / width.max(1) as f32,
                y as f32 / height.max(1) as f32,
            )));
        } else {
            self.0.canvas.set_measurement_cursor(None);
            self.0.canvas.set_marker(Some((
                (x as f32 + 0.5) / width.max(1) as f32,
                (y as f32 + 0.5) / height.max(1) as f32,
            )));
        }
        if let Some(start) = self.0.keyboard_tool_anchor.get() {
            let dimensions = (width, height);
            match tool {
                KeyboardTool::Measure => {}
                KeyboardTool::Select => {
                    self.0
                        .canvas
                        .set_crop_overlay(selection_overlay(SelectionDrag {
                            start,
                            current: (x, y),
                            start_screen: (0.0, 0.0),
                            image_dimensions: dimensions,
                        }));
                }
                KeyboardTool::Highlight
                | KeyboardTool::Arrow
                | KeyboardTool::Text
                | KeyboardTool::PickColor
                | KeyboardTool::Pencil => {}
            }
        }
        if announce {
            self.0.canvas.announce(
                &gettext("Column {column}, row {row}")
                    .replace("{column}", &(x + 1).to_string())
                    .replace("{row}", &(y + 1).to_string()),
                gtk::AccessibleAnnouncementPriority::Medium,
            );
        }
    }

    fn activate_keyboard_tool(&self) {
        let Some(tool) = self.active_keyboard_tool() else {
            return;
        };
        let Some(image_dimensions) = self
            .0
            .rendered
            .borrow()
            .as_ref()
            .map(image::GenericImageView::dimensions)
        else {
            return;
        };
        let current = if let Some(current) = self.0.keyboard_tool_cursor.get() {
            current
        } else {
            let Some((max_x, max_y)) = self.keyboard_tool_dimensions(tool) else {
                return;
            };
            let current = (max_x / 2, max_y / 2);
            self.0.keyboard_tool_cursor.set(Some(current));
            current
        };
        self.update_keyboard_tool_cursor(false);
        match tool {
            KeyboardTool::PickColor => {
                if let Some(color) = self
                    .0
                    .rendered
                    .borrow()
                    .as_ref()
                    .and_then(|image| crate::tools::pencil::sample(image, current.0, current.1))
                {
                    self.copy_color_to_clipboard(color);
                }
            }
            KeyboardTool::Pencil => self.commit_editable_pencil_stroke(
                &[BrushPoint {
                    x: current.0 as f32 + 0.5,
                    y: current.1 as f32 + 0.5,
                    pressure: 1.0,
                }],
                PencilDragMode::Freehand,
            ),
            KeyboardTool::Highlight | KeyboardTool::Arrow | KeyboardTool::Text => {
                let id = {
                    let mut document = self.0.document.borrow_mut();
                    let Some(document) = document.as_mut() else {
                        return;
                    };
                    document.allocate_annotation_id()
                };
                let color = self.0.pencil_color.get();
                let stroke_width = self.current_annotation_stroke_width();
                let current = crate::document::Point {
                    x: current.0 as f32 + 0.5,
                    y: current.1 as f32 + 0.5,
                };
                let shape = match tool {
                    KeyboardTool::Highlight => {
                        let width = image_dimensions.0.min(64) as f32;
                        let height = image_dimensions.1.min(40) as f32;
                        let x =
                            (current.x - width / 2.0).clamp(0.0, image_dimensions.0 as f32 - width);
                        let y = (current.y - height / 2.0)
                            .clamp(0.0, image_dimensions.1 as f32 - height);
                        Shape::Highlight {
                            rect: crate::document::Rect {
                                x,
                                y,
                                width,
                                height,
                            },
                            seed: id.0 ^ 0xD10A_AA73_9E37_79B9,
                            style: StrokeStyle {
                                color,
                                width: HIGHLIGHT_STROKE_WIDTH,
                            },
                        }
                    }
                    KeyboardTool::Arrow => {
                        let length = image_dimensions.0.saturating_sub(1).min(80) as f32;
                        let start_x = (current.x - length / 2.0)
                            .clamp(0.5, image_dimensions.0 as f32 - 0.5 - length);
                        let start = crate::document::Point {
                            x: start_x,
                            y: current.y,
                        };
                        let end = crate::document::Point {
                            x: start_x + length,
                            y: current.y,
                        };
                        Shape::Arrow {
                            start,
                            end,
                            control: start.midpoint(end),
                            style: StrokeStyle {
                                color,
                                width: stroke_width,
                            },
                        }
                    }
                    KeyboardTool::Text => {
                        self.open_text_editor(None, id, current, 0.0, String::new());
                        return;
                    }
                    _ => unreachable!(),
                };
                self.apply(Operation::Annotate(AnnotationEdit::Create(Annotation {
                    id,
                    shape,
                })));
                self.select_annotation(Some(id));
            }
            KeyboardTool::Measure | KeyboardTool::Select => {
                let Some(start) = self.0.keyboard_tool_anchor.replace(Some(current)) else {
                    self.0.canvas.announce(
                        &gettext(
                            "Start point set. Move to the end point and press Space or Enter.",
                        ),
                        gtk::AccessibleAnnouncementPriority::Medium,
                    );
                    return;
                };
                self.0.keyboard_tool_anchor.set(None);
                match tool {
                    KeyboardTool::Measure => {
                        let id = {
                            let mut document = self.0.document.borrow_mut();
                            let Some(document) = document.as_mut() else {
                                return;
                            };
                            document.allocate_annotation_id()
                        };
                        let horizontal = start.0.abs_diff(current.0) >= start.1.abs_diff(current.1);
                        let shape = if horizontal {
                            Shape::Measurement {
                                axis: Axis::Horizontal,
                                from: start.0.min(current.0) as f32,
                                to: start.0.max(current.0) as f32,
                                at: start.1 as f32,
                                style: StrokeStyle {
                                    color: self.0.pencil_color.get(),
                                    width: MEASUREMENT_STROKE_WIDTH,
                                },
                                label_size: self.0.settings.annotation_text_size() as f32,
                            }
                        } else {
                            Shape::Measurement {
                                axis: Axis::Vertical,
                                from: start.1.min(current.1) as f32,
                                to: start.1.max(current.1) as f32,
                                at: start.0 as f32,
                                style: StrokeStyle {
                                    color: self.0.pencil_color.get(),
                                    width: MEASUREMENT_STROKE_WIDTH,
                                },
                                label_size: self.0.settings.annotation_text_size() as f32,
                            }
                        };
                        self.apply(Operation::Annotate(AnnotationEdit::Create(Annotation {
                            id,
                            shape,
                        })));
                        self.select_annotation(Some(id));
                    }
                    KeyboardTool::Select => {
                        let selection = selection_overlay(SelectionDrag {
                            start,
                            current,
                            start_screen: (0.0, 0.0),
                            image_dimensions,
                        });
                        self.set_region_selection(selection);
                        if let Some(selection) = selection {
                            self.0.canvas.announce(
                                &gettext(
                                    "Region selected, {width} by {height} pixels. Choose zoom, crop, or copy.",
                                )
                                .replace("{width}", &selection.width.to_string())
                                .replace("{height}", &selection.height.to_string()),
                                gtk::AccessibleAnnouncementPriority::Medium,
                            );
                        }
                    }
                    KeyboardTool::Highlight
                    | KeyboardTool::Arrow
                    | KeyboardTool::Text
                    | KeyboardTool::PickColor
                    | KeyboardTool::Pencil => unreachable!(),
                }
            }
        }
    }

    fn add_action(&self, name: &str, callback: impl Fn() + 'static) {
        let action = gio::SimpleAction::new(name, None);
        action.connect_activate(move |_, _| callback());
        self.0.window.add_action(&action);
    }

    fn set_action_enabled(&self, name: &str, enabled: bool) {
        if let Some(action) = self
            .0
            .window
            .lookup_action(name)
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
        {
            action.set_enabled(enabled);
        }
    }

    fn current_annotation_stroke_width(&self) -> f32 {
        self.0.pencil_size.value() as f32
    }

    fn update_action_states(&self) {
        let has_image = self.0.canvas.texture().is_some();
        let has_file = self.0.current_file.borrow().is_some();
        let document = self.0.document.borrow();
        let editable = document.is_some() && self.0.rendered.borrow().is_some();
        let has_neighbors = self
            .0
            .sequence
            .borrow()
            .as_ref()
            .is_some_and(|sequence| sequence.len() > 1);

        for action in [
            "copy-image",
            "zoom-in",
            "zoom-out",
            "actual-size",
            "zoom-25",
            "zoom-50",
            "zoom-75",
            "zoom-100",
            "zoom-200",
            "zoom-300",
            "zoom-400",
            "zoom-500",
            "zoom-600",
            "zoom-700",
            "zoom-800",
            "zoom-900",
            "fit",
            "fill",
            "toggle-filter",
            "compare",
            "lens",
            "properties",
        ] {
            self.set_action_enabled(action, has_image);
        }
        self.set_action_enabled("open-with", has_file && has_image);
        self.set_action_enabled("delete-file", has_file && has_image);
        self.set_action_enabled("previous", has_image && has_neighbors);
        self.set_action_enabled("next", has_image && has_neighbors);

        for action in [
            "save-as",
            "rotate-clockwise",
            "rotate-counterclockwise",
            "flip-horizontal",
            "flip-vertical",
            "scale-preview",
            "crop-content",
            "scale",
            "palette",
            "pencil",
            "pick-color",
            "select",
            "tool",
        ] {
            self.set_action_enabled(action, editable);
        }
        let region_selected = editable && self.0.region_selection.get().is_some();
        for action in ["selection-zoom", "selection-crop", "selection-copy"] {
            self.set_action_enabled(action, region_selected);
        }
        let vector_annotations_available = editable && self.0.compare_canvas.borrow().is_none();
        for action in ["measure", "highlight", "arrow", "text"] {
            self.set_action_enabled(action, vector_annotations_available);
        }
        for button in [
            &self.0.measurement_button,
            &self.0.highlight_button,
            &self.0.arrow_button,
            &self.0.text_button,
        ] {
            button.set_sensitive(vector_annotations_available);
        }
        self.set_action_enabled("save", document.as_ref().is_some_and(Document::is_dirty));
        self.set_action_enabled("undo", document.as_ref().is_some_and(Document::can_undo));
        self.set_action_enabled("redo", document.as_ref().is_some_and(Document::can_redo));
        let has_animation = self.0.animation_frames.borrow().len() > 1;
        self.set_action_enabled("play-pause", has_animation);
        self.set_action_enabled("previous-frame", has_animation);
        self.set_action_enabled("next-frame", has_animation);
    }

    fn apply(&self, operation: Operation) {
        self.0.nudge_annotation.set(None);
        {
            let mut document = self.0.document.borrow_mut();
            let Some(document) = document.as_mut() else {
                self.0
                    .toasts
                    .add_toast(adw::Toast::new(&gettext("Open an editable image first")));
                return;
            };
            document.apply(operation);
        }
        self.update_action_states();
        self.render_document();
    }

    fn toggle_tool(&self, tool: Tool) {
        self.set_tool(if self.0.tool.get() == tool {
            Tool::None
        } else {
            tool
        });
    }

    fn tool_button_toggled(&self, button: &gtk::ToggleButton, tool: Tool) {
        if self.0.updating_tool.get() {
            return;
        }
        let requested = if button.is_active() { tool } else { Tool::None };
        self.set_tool(requested);
        if self.0.tool.get() != requested {
            self.0.updating_tool.set(true);
            button.set_active(self.0.tool.get() == tool);
            self.0.updating_tool.set(false);
        }
    }

    fn set_tool(&self, tool: Tool) {
        let editable = self.0.document.borrow().is_some() && self.0.rendered.borrow().is_some();
        let tool = resting_tool(tool, editable);
        let previous = self.0.tool.get();
        if previous == tool {
            return;
        }
        if tool.is_vector_annotation() && self.0.compare_canvas.borrow().is_some() {
            return;
        }
        if tool != Tool::None && self.0.rendered.borrow().is_none() {
            self.0
                .toasts
                .add_toast(adw::Toast::new(&gettext("Open an editable image first")));
            return;
        }

        self.0.updating_tool.set(true);
        self.0.nudge_annotation.set(None);
        self.cancel_annotation_drag();
        self.close_text_editor();
        match previous {
            Tool::Pencil => self.set_pencil_active(false),
            Tool::PickColor => self.set_color_picker_active(false),
            Tool::Select => self.set_selection_active(false),
            Tool::Scale => self.set_scale_preview_active(false),
            Tool::Measure => {
                self.0.canvas.set_measurement_cursor(None);
                self.prepare_keyboard_tool(false);
            }
            _ => {}
        }

        if tool == Tool::PickColor {
            self.0
                .return_tool
                .set(previous.is_annotation().then_some(previous));
        } else {
            self.0.return_tool.set(None);
        }
        self.0.tool.set(tool);
        let keeps_annotation_selection =
            tool.is_annotation() || tool == Tool::PickColor && self.0.return_tool.get().is_some();
        if !keeps_annotation_selection {
            self.select_annotation(None);
        }
        for (button, button_tool) in [
            (&self.0.pencil_button, Tool::Pencil),
            (&self.0.highlight_button, Tool::Highlight),
            (&self.0.arrow_button, Tool::Arrow),
            (&self.0.measurement_button, Tool::Measure),
            (&self.0.text_button, Tool::Text),
            (&self.0.color_picker_button, Tool::PickColor),
            (&self.0.scale_button, Tool::Scale),
        ] {
            button.set_active(tool == button_tool);
        }

        match tool {
            Tool::Pencil => self.set_pencil_active(true),
            Tool::PickColor => self.set_color_picker_active(true),
            Tool::Select => self.set_selection_active(true),
            Tool::Scale => self.set_scale_preview_active(true),
            Tool::Measure => {
                self.0.canvas.set_cursor_from_name(Some("none"));
                self.prepare_keyboard_tool(true);
            }
            Tool::Highlight | Tool::Arrow | Tool::Text => {
                self.0
                    .canvas
                    .set_cursor_from_name((!self.0.lens_active.get()).then_some("crosshair"));
                self.prepare_keyboard_tool(true);
            }
            Tool::None => {
                self.0.canvas.set_cursor_from_name(None);
                self.prepare_keyboard_tool(false);
            }
        }

        let highlight_width = self
            .0
            .rendered
            .borrow()
            .as_ref()
            .map(image::GenericImageView::dimensions)
            .map_or(HIGHLIGHT_STROKE_WIDTH, highlight_stroke_width);
        let (lower, upper, value, tooltip, size_sensitive) = match tool {
            Tool::Text => (
                6.0,
                512.0,
                f64::from(self.0.settings.annotation_text_size()),
                gettext("Text size in image pixels"),
                true,
            ),
            Tool::Highlight => (
                f64::from(highlight_width),
                f64::from(highlight_width),
                f64::from(highlight_width),
                gettext("Automatic highlight width for this image"),
                false,
            ),
            Tool::Arrow | Tool::Pencil => (
                1.0,
                128.0,
                self.0.line_width.get(),
                gettext("Stroke width in image pixels"),
                true,
            ),
            Tool::Measure => (
                f64::from(MEASUREMENT_STROKE_WIDTH),
                f64::from(MEASUREMENT_STROKE_WIDTH),
                f64::from(MEASUREMENT_STROKE_WIDTH),
                gettext("Fixed native 1-pixel measurement line"),
                false,
            ),
            _ => (
                1.0,
                128.0,
                self.0.line_width.get(),
                gettext("Width in image pixels"),
                true,
            ),
        };
        self.0.pencil_size.set_range(lower, upper);
        self.0.pencil_size.set_value(value);
        self.0.pencil_size.set_sensitive(size_sensitive);
        self.0.pencil_size.set_tooltip_text(Some(&tooltip));
        self.0
            .pencil_controls
            .set_visible(palette_visible(tool, self.0.return_tool.get()));
        if let Some(action) = self
            .0
            .window
            .lookup_action("tool")
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
        {
            action.set_state(&tool.name().to_variant());
        }
        self.0.canvas.set_accessible_label(&if tool == Tool::None {
            gettext("Image canvas")
        } else {
            gettext("Image canvas, {tool} tool active").replace("{tool}", tool.name())
        });
        self.0.updating_tool.set(false);
    }

    fn install_tool_controls(&self) {
        self.0.measurement_button.connect_toggled({
            let this = self.clone();
            move |button| this.tool_button_toggled(button, Tool::Measure)
        });
        self.0.highlight_button.connect_toggled({
            let this = self.clone();
            move |button| this.tool_button_toggled(button, Tool::Highlight)
        });
        self.0.arrow_button.connect_toggled({
            let this = self.clone();
            move |button| this.tool_button_toggled(button, Tool::Arrow)
        });
        self.0.text_button.connect_toggled({
            let this = self.clone();
            move |button| this.tool_button_toggled(button, Tool::Text)
        });
        self.0.color_picker_button.connect_toggled({
            let this = self.clone();
            move |button| this.tool_button_toggled(button, Tool::PickColor)
        });
        self.0.pencil_button.connect_toggled({
            let this = self.clone();
            move |button| this.tool_button_toggled(button, Tool::Pencil)
        });
        self.0.lens_button.connect_toggled({
            let this = self.clone();
            move |button| this.set_single_image_lens_active(button.is_active())
        });
        self.0.color_button.connect_rgba_notify({
            let this = self.clone();
            move |button| {
                let color = rgba_to_u8(button.rgba());
                this.0.pencil_color.set(color);
                if !this.0.updating_tool.get() {
                    this.update_selected_annotation_style(Some(color), None);
                }
            }
        });
        self.0.pencil_size.connect_value_changed({
            let this = self.clone();
            move |spinner| {
                if this.0.updating_tool.get() {
                    return;
                }
                match this.0.tool.get() {
                    Tool::Text => this
                        .0
                        .settings
                        .set_annotation_text_size(spinner.value().round() as u16),
                    Tool::Highlight | Tool::Measure => return,
                    _ => {
                        this.0.line_width.set(spinner.value());
                        this.0
                            .settings
                            .set_pencil_size(spinner.value().round() as u8);
                    }
                }
                this.update_selected_annotation_style(None, Some(spinner.value() as f32));
            }
        });
    }

    fn install_scale_controls(&self) {
        self.0.scale_button.connect_toggled({
            let this = self.clone();
            move |button| this.tool_button_toggled(button, Tool::Scale)
        });
        self.0.scale_width.connect_value_changed({
            let this = self.clone();
            move |_| this.scale_dimension_changed(true)
        });
        self.0.scale_height.connect_value_changed({
            let this = self.clone();
            move |_| this.scale_dimension_changed(false)
        });
        self.0.scale_lock.connect_toggled({
            let this = self.clone();
            move |button| {
                if button.is_active() {
                    this.scale_dimension_changed(true);
                } else {
                    this.refresh_scale_controls();
                }
            }
        });
        self.0.scale_unit.connect_selected_notify({
            let this = self.clone();
            move |_| this.refresh_scale_controls()
        });
        self.0.scale_slider.connect_value_changed({
            let this = self.clone();
            move |slider| this.scale_slider_changed(slider.value())
        });
        let original = gtk::GestureClick::new();
        original.set_button(1);
        original.connect_pressed({
            let this = self.clone();
            move |_, _, _, _| this.set_scale_original_visible(true)
        });
        original.connect_released({
            let this = self.clone();
            move |_, _, _, _| this.set_scale_original_visible(false)
        });
        original.connect_cancel({
            let this = self.clone();
            move |_, _| this.set_scale_original_visible(false)
        });
        self.0.scale_original_button.add_controller(original);
    }

    fn finish_editable_decode(&self, editable_available: bool) {
        self.0.editable_decode_pending.set(false);
        self.0
            .view_only_banner
            .set_revealed(self.0.canvas.texture().is_some() && !editable_available);
        self.update_action_states();
        if self.0.pending_scale_activation.replace(false) {
            if editable_available && self.0.tool.get() == Tool::Scale {
                self.set_scale_preview_active(true);
            } else {
                self.0.scale_button.set_active(false);
            }
            return;
        }
        if editable_available && self.0.tool.get() == Tool::None {
            self.set_tool(Tool::Select);
        }
    }

    fn set_scale_preview_active(&self, active: bool) {
        if active {
            let image = self.0.rendered.borrow().clone();
            let Some(image) = image else {
                if self.0.editable_decode_pending.get() {
                    self.0.pending_scale_activation.set(true);
                    self.0
                        .toasts
                        .add_toast(adw::Toast::new(&gettext("Preparing image for scaling…")));
                    return;
                }
                self.0.scale_button.set_active(false);
                self.0
                    .toasts
                    .add_toast(adw::Toast::new(&gettext("Open an editable image first")));
                return;
            };
            self.0.pending_scale_activation.set(false);
            let (width, height) = image.dimensions();
            let horizontal = self.0.scrolled.hadjustment();
            let vertical = self.0.scrolled.vadjustment();
            self.0.scale_source.replace(Some(Arc::new(image)));
            self.0.scale_preview.borrow_mut().take();
            self.0.scale_source_view.set(Some(ScaleViewState {
                zoom: self.0.canvas.zoom(),
                horizontal: horizontal.value(),
                vertical: vertical.value(),
            }));
            self.0.scale_preview_view.set(ScalePreviewView::Footprint);
            self.0.scale_showing_original.set(false);
            self.0.scale_committing.set(false);
            self.0.scale_updating_controls.set(true);
            self.configure_scale_ranges(width, height);
            self.0.scale_width.set_value(f64::from(width));
            self.0.scale_height.set_value(f64::from(height));
            self.0.scale_updating_controls.set(false);
            self.refresh_scale_controls();
            self.0.scale_controls.set_visible(true);
            self.0.zoom_controls.set_visible(false);
            return;
        }
        self.0.pending_scale_activation.set(false);
        self.0
            .scale_preview_generation
            .set(self.0.scale_preview_generation.get().wrapping_add(1));
        if let Some(cancellation) = self.0.scale_preview_cancellation.borrow_mut().take() {
            cancellation.cancel();
        }
        self.0.scale_spinner.set_visible(false);
        self.0.scale_controls.set_visible(false);
        self.0.pending_fit.set(None);
        self.0.zoom_controls.set_visible(true);
        self.0.scale_showing_original.set(false);
        self.0.scale_preview.borrow_mut().take();
        let source = self.0.scale_source.borrow_mut().take();
        let source_view = self.0.scale_source_view.take();
        if !self.0.scale_committing.replace(false)
            && let Some(image) = source
            && let Ok(texture) = texture_from_rgba(&image)
        {
            self.0.canvas.set_texture(Some(&texture));
            if let Some(view) = source_view {
                self.set_scale_preview_zoom(view.zoom);
                let horizontal = self.0.scrolled.hadjustment();
                let vertical = self.0.scrolled.vadjustment();
                glib::idle_add_local_once(move || {
                    horizontal.set_value(view.horizontal);
                    vertical.set_value(view.vertical);
                });
            }
            self.update_minimap();
            self.update_subtitle();
        }
    }

    fn configure_scale_ranges(&self, source_width: u32, source_height: u32) {
        let factor = if self.0.scale_resampling.get() == Resampling::SeamCarving {
            1
        } else {
            2
        };
        self.0
            .scale_width
            .set_range(1.0, f64::from(source_width.saturating_mul(factor)));
        self.0
            .scale_height
            .set_range(1.0, f64::from(source_height.saturating_mul(factor)));
        self.0
            .scale_algorithm_label
            .set_label(&gettext("{method} · Properties").replace(
                "{method}",
                &gettext(resampling_label(self.0.scale_resampling.get())),
            ));
    }

    fn scale_dimension_changed(&self, width_changed: bool) {
        if self.0.scale_updating_controls.get() {
            return;
        }
        let Some(source) = self.0.scale_source.borrow().clone() else {
            return;
        };
        self.0.scale_updating_controls.set(true);
        if self.0.scale_lock.is_active() {
            if width_changed {
                let (_, height) = scaled_dimensions(
                    source.width(),
                    source.height(),
                    self.0.scale_width.value().round() as u32,
                );
                self.0.scale_height.set_value(f64::from(height));
            } else {
                let width = scaled_width_for_height(
                    source.width(),
                    source.height(),
                    self.0.scale_height.value().round() as u32,
                );
                self.0.scale_width.set_value(f64::from(width));
            }
        }
        self.0.scale_updating_controls.set(false);
        self.refresh_scale_controls();
    }

    fn scale_slider_changed(&self, value: f64) {
        if self.0.scale_updating_controls.get() {
            return;
        }
        let Some(source) = self.0.scale_source.borrow().clone() else {
            return;
        };
        self.0.scale_updating_controls.set(true);
        match scale_unit(self.0.scale_unit.selected()) {
            ScaleUnit::Pixels => {
                let width = value.round().max(1.0) as u32;
                self.0.scale_width.set_value(f64::from(width));
                if self.0.scale_lock.is_active() {
                    let (_, height) = scaled_dimensions(source.width(), source.height(), width);
                    self.0.scale_height.set_value(f64::from(height));
                }
            }
            ScaleUnit::Percent => {
                let (width, height) =
                    dimensions_from_percent(source.width(), source.height(), value);
                self.0.scale_width.set_value(f64::from(width));
                self.0.scale_height.set_value(f64::from(height));
            }
        }
        self.0.scale_updating_controls.set(false);
        self.refresh_scale_controls();
    }

    fn refresh_scale_controls(&self) {
        let Some(source) = self.0.scale_source.borrow().clone() else {
            return;
        };
        let width = self.0.scale_width.value().round() as u32;
        let height = self.0.scale_height.value().round() as u32;
        let percent = f64::from(width) * 100.0 / f64::from(source.width().max(1));
        self.0.scale_updating_controls.set(true);
        match scale_unit(self.0.scale_unit.selected()) {
            ScaleUnit::Pixels => {
                self.0
                    .scale_slider
                    .set_range(1.0, self.0.scale_width.adjustment().upper());
                self.0.scale_slider.set_value(f64::from(width));
                self.0
                    .scale_slider
                    .set_tooltip_text(Some(&gettext("Output width in pixels")));
            }
            ScaleUnit::Percent => {
                let maximum = if self.0.scale_resampling.get() == Resampling::SeamCarving {
                    100.0
                } else {
                    200.0
                };
                self.0.scale_slider.set_range(1.0, maximum);
                self.0.scale_slider.set_value(percent.clamp(1.0, maximum));
                self.0
                    .scale_slider
                    .set_tooltip_text(Some(&gettext("Output size as a percentage")));
            }
        }
        self.0.scale_updating_controls.set(false);
        let scale_summary = if self.0.scale_lock.is_active() {
            format!("{percent:.0}%")
        } else {
            let height_percent = f64::from(height) * 100.0 / f64::from(source.height().max(1));
            format!("{percent:.0}% × {height_percent:.0}%")
        };
        self.0.scale_value_label.set_label(&format!(
            "{} × {} → {width} × {height} ({scale_summary})",
            source.width(),
            source.height()
        ));
        self.schedule_scale_preview(width, height);
    }

    fn refresh_scale_method(&self) {
        self.0
            .scale_algorithm_label
            .set_label(&gettext("{method} · Properties").replace(
                "{method}",
                &gettext(resampling_label(self.0.scale_resampling.get())),
            ));
        let Some(source) = self.0.scale_source.borrow().clone() else {
            return;
        };
        self.0.scale_updating_controls.set(true);
        self.configure_scale_ranges(source.width(), source.height());
        self.0.scale_updating_controls.set(false);
        self.scale_dimension_changed(true);
    }

    fn schedule_scale_preview(&self, target_width: u32, target_height: u32) {
        let Some(source) = self.0.scale_source.borrow().clone() else {
            self.0.scale_spinner.set_visible(false);
            return;
        };
        let generation = self.0.scale_preview_generation.get().wrapping_add(1);
        self.0.scale_preview_generation.set(generation);
        if let Some(cancellation) = self.0.scale_preview_cancellation.borrow_mut().take() {
            cancellation.cancel();
        }
        if (target_width, target_height) == source.dimensions() {
            self.0.scale_spinner.set_visible(false);
            self.display_scale_preview(source);
            return;
        }
        self.0.scale_spinner.set_visible(true);
        self.0.scale_original_button.set_sensitive(false);
        let resampling = self.0.scale_resampling.get();
        let cancellation = CancellationToken::default();
        self.0
            .scale_preview_cancellation
            .replace(Some(cancellation.clone()));
        let weak = Rc::downgrade(&self.0);
        glib::timeout_add_local_once(Duration::from_millis(50), move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            if state.scale_preview_generation.get() != generation {
                return;
            }
            let source = source.clone();
            let cancellation = cancellation.clone();
            let weak = Rc::downgrade(&state);
            glib::spawn_future_local(async move {
                let preview = gio::spawn_blocking(move || {
                    crate::tools::scale::resize(
                        source.as_ref(),
                        target_width,
                        target_height,
                        resampling,
                        &cancellation,
                    )
                })
                .await;
                let Some(state) = weak.upgrade() else {
                    return;
                };
                if state.scale_preview_generation.get() != generation {
                    return;
                }
                state.scale_preview_cancellation.borrow_mut().take();
                state.scale_spinner.set_visible(false);
                match preview {
                    Ok(Ok(preview)) => {
                        ViewerWindow(state).display_scale_preview(Arc::new(preview));
                    }
                    Ok(Err(error)) => state.toasts.add_toast(adw::Toast::new(&error.to_string())),
                    Err(_) => state
                        .toasts
                        .add_toast(adw::Toast::new(&gettext("Scale preview worker failed"))),
                }
            });
        });
    }

    fn display_scale_preview(&self, preview: Arc<image::RgbaImage>) {
        self.0.scale_spinner.set_visible(false);
        self.0.scale_preview.replace(Some(preview.clone()));
        self.0.scale_original_button.set_sensitive(true);
        if self.0.scale_showing_original.get() {
            return;
        }
        match texture_from_rgba(&preview) {
            Ok(texture) => {
                self.0.canvas.set_texture(Some(&texture));
                self.apply_scale_preview_view(preview.width());
            }
            Err(error) => self.0.toasts.add_toast(adw::Toast::new(&error)),
        }
    }

    fn apply_scale_preview_view(&self, target_width: u32) {
        match self.0.scale_preview_view.get() {
            ScalePreviewView::Footprint => {
                if let Some(source) = self.0.scale_source.borrow().as_ref()
                    && let Some(view) = self.0.scale_source_view.get()
                {
                    self.set_scale_preview_zoom(scale_preview_zoom(
                        source.width(),
                        target_width,
                        view.zoom,
                    ));
                }
            }
            ScalePreviewView::ActualSize => self.set_scale_preview_zoom(1.0),
            ScalePreviewView::Fit => self.fit(false),
        }
    }

    fn set_scale_original_visible(&self, visible: bool) {
        if self.0.tool.get() != Tool::Scale
            || self.0.scale_showing_original.replace(visible) == visible
        {
            return;
        }
        if visible {
            let Some(source) = self.0.scale_source.borrow().clone() else {
                return;
            };
            let width = self.0.scale_width.value().round() as u32;
            let preview_zoom = self.0.canvas.zoom();
            self.0.scale_preview_zoom_before_original.set(preview_zoom);
            if let Ok(texture) = texture_from_rgba(&source) {
                self.0.canvas.set_texture(Some(&texture));
                let zoom =
                    preview_zoom * f64::from(width.max(1)) / f64::from(source.width().max(1));
                if self.0.scale_preview_view.get() == ScalePreviewView::Fit {
                    self.set_scale_preview_fit_zoom(zoom);
                } else {
                    self.set_scale_preview_zoom(zoom);
                }
            }
            return;
        }
        let dimensions = (
            self.0.scale_width.value().round() as u32,
            self.0.scale_height.value().round() as u32,
        );
        let preview = self
            .0
            .scale_preview
            .borrow()
            .as_ref()
            .filter(|preview| preview.dimensions() == dimensions)
            .cloned();
        if let Some(preview) = preview
            && let Ok(texture) = texture_from_rgba(&preview)
        {
            self.0.canvas.set_texture(Some(&texture));
            if self.0.scale_preview_view.get() == ScalePreviewView::Fit {
                self.fit(false);
            } else {
                self.set_scale_preview_zoom(self.0.scale_preview_zoom_before_original.get());
            }
        } else {
            self.schedule_scale_preview(dimensions.0, dimensions.1);
        }
    }

    fn confirm_scale_preview(&self) {
        let Some(source) = self.0.scale_source.borrow().clone() else {
            return;
        };
        let width = self.0.scale_width.value().round() as u32;
        let height = self.0.scale_height.value().round() as u32;
        let resampling = self.0.scale_resampling.get();
        if resampling == Resampling::SeamCarving
            && (width > source.width() || height > source.height())
        {
            self.0.toasts.add_toast(adw::Toast::new(&gettext(
                "Seam carving currently supports shrinking only",
            )));
            return;
        }
        if width > source.width() || height > source.height() {
            self.0.toasts.add_toast(adw::Toast::new(&gettext(
                "Scaling up may reduce perceived image quality",
            )));
        }
        self.set_scale_original_visible(false);
        self.0.scale_committing.set(true);
        self.set_tool(Tool::None);
        self.apply(Operation::Scale {
            width,
            height,
            resampling,
        });
    }

    fn set_scale_preview_zoom(&self, zoom: f64) {
        self.0.canvas.set_zoom(zoom);
        self.update_scale_preview_zoom();
    }

    fn set_scale_preview_fit_zoom(&self, zoom: f64) {
        self.0.canvas.set_fit_zoom(zoom);
        self.update_scale_preview_zoom();
    }

    fn update_scale_preview_zoom(&self) {
        self.0
            .zoom_label
            .set_label(&format!("{:.0}%", self.0.canvas.zoom() * 100.0));
        self.update_subtitle();
        self.update_minimap();
    }

    fn set_pencil_active(&self, active: bool) {
        if active && !pencil_can_activate(self.0.rendered.borrow().is_some()) {
            self.0.pencil_button.set_active(false);
            if self.0.rendered.borrow().is_none() {
                self.0
                    .toasts
                    .add_toast(adw::Toast::new(&gettext("Open an editable image first")));
            }
            return;
        }
        if !active {
            self.abort_pencil_drag();
        }
        self.0.pencil_controls.set_visible(active);
        self.0.canvas.set_accessible_label(&if active {
            gettext("Image canvas, Pencil tool active")
        } else {
            gettext("Image canvas")
        });
        self.prepare_keyboard_tool(active);
    }

    fn set_color_picker_active(&self, active: bool) {
        if active && self.0.rendered.borrow().is_none() {
            self.0.color_picker_button.set_active(false);
            self.0
                .toasts
                .add_toast(adw::Toast::new(&gettext("Open an editable image first")));
            return;
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
        self.0.canvas.set_accessible_label(&if active {
            gettext("Image canvas, Color Picker tool active")
        } else {
            gettext("Image canvas")
        });
        self.prepare_keyboard_tool(active);
    }

    fn apply_picked_color(&self, color: [u8; 4]) -> String {
        self.abort_pencil_drag();
        self.0.pencil_color.set(color);
        self.0.color_button.set_rgba(&u8_to_rgba(color));
        format_color(color, self.0.settings.color_picker_format())
    }

    fn copy_color_to_clipboard(&self, color: [u8; 4]) {
        let return_tool = self.0.return_tool.get();
        let value = self.apply_picked_color(color);
        self.0.window.clipboard().set_text(&value);
        self.0.toasts.add_toast(adw::Toast::new(
            &gettext("Copied {value}").replace("{value}", &value),
        ));
        if let Some(tool) = return_tool {
            self.set_tool(tool);
        }
    }

    fn copy_current_image_to_clipboard(&self) {
        let Some(texture) = self.0.canvas.texture() else {
            self.0
                .toasts
                .add_toast(adw::Toast::new(&gettext("Open an image first")));
            return;
        };
        self.0.window.clipboard().set_texture(&texture);
        self.0.toasts.add_toast(adw::Toast::new(
            &gettext("Copied {width} × {height} image")
                .replace("{width}", &texture.width().to_string())
                .replace("{height}", &texture.height().to_string()),
        ));
    }

    fn copy_current_selection_or_image_to_clipboard(&self) {
        if self.0.region_selection.get().is_some() {
            self.copy_selected_region();
        } else {
            self.copy_current_image_to_clipboard();
        }
    }

    fn open_with(&self) {
        let Some(file) = self.0.current_file.borrow().clone() else {
            self.0
                .toasts
                .add_toast(adw::Toast::new(&gettext("Open an image first")));
            return;
        };
        let launcher = open_with_launcher(&file);
        let parent = self.0.window.clone();
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            if let Err(error) = launcher.launch_future(Some(&parent)).await
                && !open_with_was_cancelled(&error)
                && let Some(state) = weak.upgrade()
            {
                state.toasts.add_toast(adw::Toast::new(
                    &gettext("Could not open image with another app: {error}")
                        .replace("{error}", &error.to_string()),
                ));
            }
        });
    }

    fn set_selection_active(&self, active: bool) {
        if active && self.0.rendered.borrow().is_none() {
            self.0
                .toasts
                .add_toast(adw::Toast::new(&gettext("Open an editable image first")));
            return;
        }
        self.0.region_controls.set_visible(active);
        if !active {
            self.clear_region_selection();
        }
        self.0
            .canvas
            .set_cursor_from_name((active && !self.0.lens_active.get()).then_some("crosshair"));
        self.0.canvas.set_accessible_label(&if active {
            gettext("Image canvas, Select Region tool active")
        } else {
            gettext("Image canvas")
        });
        self.prepare_keyboard_tool(active);
    }

    fn set_region_selection(&self, selection: Option<CropOverlay>) {
        self.0.region_selection.set(selection);
        self.0.canvas.set_crop_overlay(selection);
        let enabled = selection.is_some();
        for action in ["selection-zoom", "selection-crop", "selection-copy"] {
            self.set_action_enabled(action, enabled);
        }
    }

    fn clear_region_selection(&self) {
        self.0.region_drag.set(None);
        self.set_region_selection(None);
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

    fn preview_pencil_stroke(&self) {
        self.0.canvas.set_pencil_overlay(
            &self.0.pencil_points.borrow(),
            self.0.pencil_path.get(),
            self.0.pencil_color.get(),
            self.0.pencil_size.value().round() as f32,
        );
    }

    fn preview_comparison_pencil_stroke(&self, canvas: &ImageCanvas) {
        canvas.set_pencil_overlay(
            &self.0.pencil_points.borrow(),
            self.0.pencil_path.get(),
            self.0.pencil_color.get(),
            self.0.pencil_size.value().round() as f32,
        );
    }

    fn paint_pencil_preview(
        &self,
        canvas: &ImageCanvas,
        image: &image::RgbaImage,
        points: &[BrushPoint],
        path: StrokePath,
    ) -> Option<image::RgbaImage> {
        let stroke = self.pencil_stroke(points, path);
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

    fn pencil_stroke(&self, points: &[BrushPoint], path: StrokePath) -> Stroke {
        Stroke {
            points: points.to_vec(),
            path,
            color: self.0.pencil_color.get(),
            width: self.0.pencil_size.value().round() as f32,
            anti_aliasing: self.0.pencil_antialiasing.get(),
            opacity: 1.0,
            hardness: 1.0,
        }
    }

    fn commit_comparison_pencil_stroke(
        &self,
        canvas: &ImageCanvas,
        points: &[BrushPoint],
        path: StrokePath,
    ) {
        self.0.pencil_line_annotation.set(None);
        let Some(image) = self.0.compare_rendered.borrow().clone() else {
            canvas.clear_pencil_overlay();
            return;
        };
        if let Some(preview) = self.paint_pencil_preview(canvas, &image, points, path) {
            self.0.compare_rendered.replace(Some(preview));
        }
        canvas.clear_pencil_overlay();
    }

    fn commit_editable_pencil_stroke(&self, points: &[BrushPoint], mode: PencilDragMode) {
        let Some(geometry) = pencil_geometry(mode, points) else {
            self.0.canvas.clear_pencil_overlay();
            return;
        };
        let line_annotation = self.0.pencil_line_annotation.get().and_then(|id| {
            self.0
                .document
                .borrow()
                .as_ref()?
                .annotations()
                .into_iter()
                .find(|annotation| annotation.id == id)
                .map(|annotation| (id, annotation))
        });
        if mode == PencilDragMode::Line
            && let Some((id, mut annotation)) = line_annotation
            && let PencilGeometry::Line(segment) = &geometry
            && let Some(end) = segment.last().copied()
            && let Shape::Pencil {
                geometry: PencilGeometry::Line(vertices),
                ..
            } = &mut annotation.shape
        {
            if vertices.last().copied() != Some(end) {
                vertices.push(end);
                self.0.canvas.clear_pencil_overlay();
                self.commit_annotation_preview(&annotation);
                self.apply(Operation::Annotate(AnnotationEdit::Set(annotation)));
            } else {
                self.0.canvas.clear_pencil_overlay();
            }
            self.select_annotation(Some(id));
            return;
        }
        let id = {
            let mut document = self.0.document.borrow_mut();
            let Some(document) = document.as_mut() else {
                self.0.canvas.clear_pencil_overlay();
                return;
            };
            document.allocate_annotation_id()
        };
        let annotation = Annotation {
            id,
            shape: Shape::Pencil {
                geometry,
                style: StrokeStyle {
                    color: self.0.pencil_color.get(),
                    width: self.0.pencil_size.value().round() as f32,
                },
                anti_aliasing: self.0.pencil_antialiasing.get(),
            },
        };
        self.0.canvas.clear_pencil_overlay();
        self.commit_annotation_preview(&annotation);
        self.apply(Operation::Annotate(AnnotationEdit::Create(annotation)));
        self.select_annotation(Some(id));
        if mode == PencilDragMode::Line {
            self.0.pencil_line_annotation.set(Some(id));
        } else {
            self.0.pencil_line_annotation.set(None);
        }
    }

    fn pencil_line_chain_end(&self) -> Option<BrushPoint> {
        let id = self.0.pencil_line_annotation.get()?;
        let point = self
            .0
            .document
            .borrow()
            .as_ref()?
            .annotations()
            .into_iter()
            .find_map(|annotation| match annotation {
                Annotation {
                    id: annotation_id,
                    shape:
                        Shape::Pencil {
                            geometry: PencilGeometry::Line(points),
                            ..
                        },
                } if annotation_id == id => points.last().copied(),
                _ => None,
            })?;
        Some(BrushPoint {
            x: point.x,
            y: point.y,
            pressure: 1.0,
        })
    }

    fn begin_pencil_drag(
        &self,
        canvas: &ImageCanvas,
        screen_x: f64,
        screen_y: f64,
        modifiers: gtk::gdk::ModifierType,
        timestamp_ms: u32,
    ) {
        let Some(origin) = canvas
            .pixel_at(screen_x, screen_y)
            .map(|(x, y)| BrushPoint {
                x: x as f32 + 0.5,
                y: y as f32 + 0.5,
                pressure: 1.0,
            })
        else {
            return;
        };
        let mode = pencil_drag_mode(modifiers);
        let line_start = pencil_line_start(
            mode,
            self.pencil_line_chain_end()
                .or_else(|| self.0.pencil_line_anchor.get()),
            origin,
        );
        self.0.pencil_drag.replace(Some(PencilDrag {
            canvas: canvas.clone(),
            start_screen: (screen_x, screen_y),
            mode,
            origin,
            line_start,
            current: origin,
            freehand_points: vec![crate::tools::pencil::TimedBrushPoint {
                point: origin,
                timestamp_ms,
            }],
        }));
        self.update_pencil_drag(canvas, screen_x, screen_y, timestamp_ms);
    }

    fn update_pencil_drag(
        &self,
        canvas: &ImageCanvas,
        screen_x: f64,
        screen_y: f64,
        timestamp_ms: u32,
    ) {
        let Some(current) = canvas
            .pixel_at(screen_x, screen_y)
            .map(|(x, y)| BrushPoint {
                x: x as f32 + 0.5,
                y: y as f32 + 0.5,
                pressure: 1.0,
            })
        else {
            return;
        };
        let (points, path, should_preview) = {
            let mut pencil_drag = self.0.pencil_drag.borrow_mut();
            let Some(drag) = pencil_drag.as_mut() else {
                return;
            };
            if drag.canvas != *canvas {
                return;
            }
            drag.current = current;
            if drag.mode == PencilDragMode::Freehand
                && drag.freehand_points.last().map(|sample| sample.point) != Some(current)
            {
                drag.freehand_points
                    .push(crate::tools::pencil::TimedBrushPoint {
                        point: current,
                        timestamp_ms,
                    });
            }
            (
                pencil_drag_points(drag),
                pencil_drag_path(drag.mode),
                pencil_drag_should_preview(drag.mode, drag.freehand_points.len()),
            )
        };
        self.0.pencil_points.replace(points);
        self.0.pencil_path.set(path);
        if !should_preview {
            canvas.clear_pencil_overlay();
        } else if canvas == &self.0.canvas {
            self.preview_pencil_stroke();
        } else {
            self.preview_comparison_pencil_stroke(canvas);
        }
    }

    fn finish_pencil_drag(
        &self,
        canvas: &ImageCanvas,
        screen_x: f64,
        screen_y: f64,
        timestamp_ms: u32,
    ) -> Option<(Vec<BrushPoint>, StrokePath, PencilDragMode)> {
        self.update_pencil_drag(canvas, screen_x, screen_y, timestamp_ms);
        let drag = self.0.pencil_drag.take()?;
        if drag.canvas != *canvas {
            self.0.pencil_drag.replace(Some(drag));
            return None;
        }
        if drag.mode == PencilDragMode::Line {
            self.0.pencil_line_anchor.set(Some(drag.current));
        } else {
            self.0.pencil_line_anchor.set(None);
        }
        let points = self.0.pencil_points.take();
        let path = self.0.pencil_path.replace(StrokePath::Smooth);
        Some((points, path, drag.mode))
    }

    fn abort_pencil_drag(&self) {
        self.0.pencil_line_anchor.set(None);
        self.0.pencil_line_annotation.set(None);
        self.0.pencil_points.borrow_mut().clear();
        self.0.pencil_path.set(StrokePath::Smooth);
        let Some(drag) = self.0.pencil_drag.take() else {
            return;
        };
        drag.canvas.clear_pencil_overlay();
    }

    fn abort_pencil_line(&self) {
        if self
            .0
            .pencil_drag
            .borrow()
            .as_ref()
            .is_some_and(|drag| drag.mode == PencilDragMode::Line)
        {
            self.abort_pencil_drag();
        } else {
            self.0.pencil_line_anchor.set(None);
            self.0.pencil_line_annotation.set(None);
        }
    }

    fn crop_selected_region(&self) {
        if self.0.tool.get() != Tool::Select {
            return;
        }
        let Some(crop) = self.0.region_selection.get() else {
            return;
        };
        self.apply(Operation::Crop {
            x: crop.x,
            y: crop.y,
            width: crop.width,
            height: crop.height,
        });
        self.clear_region_selection();
    }

    fn render_document(&self) {
        let Some(document) = self.0.document.borrow().clone() else {
            return;
        };
        self.render_candidate(document);
    }

    fn cancel_document_render(&self) {
        if let Some(previous) = self.0.render_cancellation.borrow_mut().take() {
            previous.cancel();
        }
        self.0
            .render_generation
            .set(self.0.render_generation.get().wrapping_add(1));
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
                            state.canvas.finish_annotation_render();
                            state.rendered.replace(Some(rendered.pixels));
                            let window = ViewerWindow(state.clone());
                            window.refresh_annotation_selection();
                            window.update_minimap();
                            window.update_subtitle();
                            window.update_action_states();
                        }
                        Err(error) => state.toasts.add_toast(adw::Toast::new(&error)),
                    }
                }
                Ok((_, Err(crate::error::AppError::Cancelled))) => {}
                Ok((_, Err(error))) => state.toasts.add_toast(adw::Toast::new(&error.to_string())),
                Err(_) => state
                    .toasts
                    .add_toast(adw::Toast::new(&gettext("Image processing worker failed"))),
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
        let Some(file) = self.0.current_file.borrow().clone() else {
            return;
        };
        let dimensions = (texture.width() as u32, texture.height() as u32);
        let modified = *self.0.source_modified.borrow();
        self.0.title.set_subtitle(&image_subtitle(
            &folder_path(&file),
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
                .add_toast(adw::Toast::new(&gettext("Open an editable image first")));
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
                self.0.toasts.add_toast(adw::Toast::new(&gettext(
                    "The file changed externally; use Save As to avoid overwriting it",
                )));
                return;
            }
            self.export_document(snapshot, path);
            return;
        }

        let mut builder = gtk::FileDialog::builder()
            .title(gettext("Save Image"))
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
                    this.0.toasts.add_toast(adw::Toast::new(&gettext(
                        "This location does not support atomic export",
                    )));
                }
            }
        });
    }

    fn export_document(&self, snapshot: ExportSnapshot, path: PathBuf) {
        let Some(options) = export_options(&path, &self.0.settings) else {
            self.0.toasts.add_toast(adw::Toast::new(&gettext(
                "Choose a file name ending in .png, .jpg, or .jpeg",
            )));
            return;
        };
        self.export_document_with_options(snapshot, path, options, false);
    }

    fn show_export_options(&self, snapshot: ExportSnapshot, path: PathBuf) {
        let Some(defaults) = export_options(&path, &self.0.settings) else {
            self.0.toasts.add_toast(adw::Toast::new(&gettext(
                "Choose a file name ending in .png, .jpg, or .jpeg",
            )));
            return;
        };
        let dialog = adw::Dialog::builder()
            .title(gettext("Export Options"))
            .content_width(420)
            .build();
        let header = adw::HeaderBar::new();
        let cancel = gtk::Button::with_label(&gettext("Cancel"));
        let export = gtk::Button::with_label(&gettext("Export"));
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
        let preserve = gtk::CheckButton::with_label(&gettext(
            "Preserve compatible metadata and color profile",
        ));
        preserve.set_active(self.0.settings.preserve_metadata());
        content.append(&preserve);
        let background_labels = [gettext("White"), gettext("Gray"), gettext("Black")];
        let jpeg_background = gtk::DropDown::from_strings(
            &background_labels
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        let convert_srgb = gtk::CheckButton::with_label(&gettext("Convert color profile to sRGB"));
        let control: gtk::Widget = match &defaults {
            ExportOptions::Png(options) => {
                let compression =
                    gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 9.0, 1.0);
                compression.set_value(f64::from(options.compression));
                compression.set_digits(0);
                compression.set_hexpand(true);
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                row.append(&gtk::Label::new(Some(&gettext("Compression"))));
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
                row.append(&gtk::Label::new(Some(&gettext("Quality"))));
                row.append(&quality);
                jpeg_background.set_selected(match options.background {
                    [128, 128, 128] => 1,
                    [0, 0, 0] => 2,
                    _ => 0,
                });
                let background_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                background_row.append(&gtk::Label::new(Some(&gettext("Transparency background"))));
                background_row.append(&jpeg_background);
                content.append(&background_row);
                row.upcast()
            }
        };
        content.append(&control);
        if matches!(defaults, ExportOptions::Jpeg(_)) {
            content.append(&gtk::Label::new(Some(&gettext(
                "Transparent pixels are composited onto the saved JPEG background.",
            ))));
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
                .title(gettext("Exporting image…"))
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
                crate::export::export(&rendered, &worker_path, &options, &worker_cancellation)?;
                Ok::<_, crate::error::AppError>(
                    std::fs::metadata(&worker_path)
                        .ok()
                        .and_then(|metadata| metadata.modified().ok()),
                )
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
                Ok(Ok(modified)) => {
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
                        if state.explicit_navigation.get()
                            && let Some(source) = source_file.as_ref()
                            && let Some(sequence) = state.sequence.borrow_mut().as_mut()
                        {
                            sequence.replace_file(source, target.clone());
                        }
                        state.current_file.replace(Some(target.clone()));
                        ViewerWindow(state.clone()).rebuild_navigation(target);
                    }
                    state.source_modified.replace(modified);
                    state.external_source_conflict.set(false);
                    let has_newer_edits = state
                        .document
                        .borrow()
                        .as_ref()
                        .is_some_and(Document::is_dirty);
                    let message = if has_newer_edits {
                        gettext("Image exported; newer edits remain unsaved")
                    } else {
                        gettext("Image saved")
                    };
                    state.toasts.add_toast(adw::Toast::new(&message));
                    ViewerWindow(state.clone()).update_title();
                }
                Ok(Err(error)) => state.toasts.add_toast(adw::Toast::new(&error.to_string())),
                Err(_) => state
                    .toasts
                    .add_toast(adw::Toast::new(&gettext("Export worker failed"))),
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
                .add_toast(adw::Toast::new(&gettext("Open an editable image first")));
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
                    state.toasts.add_toast(adw::Toast::new(&gettext(
                        "The background could not be identified with enough confidence",
                    )));
                    return;
                }
                Ok(Err(error)) => {
                    state.toasts.add_toast(adw::Toast::new(&error.to_string()));
                    return;
                }
                Err(_) => {
                    state
                        .toasts
                        .add_toast(adw::Toast::new(&gettext("Content detection worker failed")));
                    return;
                }
            };
            let dialog = adw::AlertDialog::builder()
                .heading(gettext("Crop to detected content?"))
                .body(
                    gettext("Detected bounds: x {x}, y {y}, {width} × {height} pixels")
                        .replace("{x}", &bounds.x.to_string())
                        .replace("{y}", &bounds.y.to_string())
                        .replace("{width}", &bounds.width.to_string())
                        .replace("{height}", &bounds.height.to_string()),
                )
                .close_response("cancel")
                .default_response("apply")
                .build();
            dialog.add_response("cancel", &gettext("Cancel"));
            dialog.add_response("apply", &gettext("Apply"));
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

    fn show_palette_dialog(&self) {
        if self.0.rendered.borrow().is_none() {
            self.0
                .toasts
                .add_toast(adw::Toast::new(&gettext("Open an editable image first")));
            return;
        }
        let dialog = adw::Dialog::builder()
            .title(gettext("Reduce Palette"))
            .content_width(420)
            .build();
        let header = adw::HeaderBar::new();
        let cancel = gtk::Button::with_label(&gettext("Cancel"));
        let apply = gtk::Button::with_label(&gettext("Apply"));
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
        count_row.append(&gtk::Label::new(Some(&gettext("Colors"))));
        count_row.append(&colors);
        count_row.append(&count);
        let dithering = gtk::CheckButton::with_label(&gettext("Dithering"));
        let accents =
            gtk::CheckButton::with_label(&gettext("Preserve accents and isolated colors"));
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
        let dialog = adw::PreferencesDialog::builder()
            .title(gettext("Preferences"))
            .search_enabled(false)
            .build();
        let page = adw::PreferencesPage::builder()
            .title(gettext("Preferences"))
            .build();
        let viewing_group = adw::PreferencesGroup::builder()
            .title(gettext("Viewing"))
            .build();
        let filter = adw::SwitchRow::builder()
            .title(gettext("Hard zoom"))
            .subtitle(gettext(
                "Keep pixel edges sharp with nearest-neighbor rendering",
            ))
            .active(self.0.canvas.filter() == ZoomFilter::Hard)
            .build();
        viewing_group.add(&filter);
        let background_labels = [
            gettext("Checkerboard"),
            gettext("Auto"),
            gettext("White"),
            gettext("Gray"),
            gettext("Black"),
        ];
        let background_model = gtk::StringList::new(
            &background_labels
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        let background = adw::ComboRow::builder()
            .title(gettext("Transparency background"))
            .model(&background_model)
            .selected(match self.0.canvas.background() {
                Background::Checkerboard => 0,
                Background::Auto => 1,
                Background::White => 2,
                Background::Gray => 3,
                Background::Black => 4,
            })
            .build();
        viewing_group.add(&background);
        let lens_size_labels = [gettext("Small"), gettext("Medium"), gettext("Large")];
        let lens_size_model = gtk::StringList::new(
            &lens_size_labels
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        let lens_size = adw::ComboRow::builder()
            .title(gettext("Lens size"))
            .subtitle(gettext("Diameter of the pixel-inspection lens"))
            .model(&lens_size_model)
            .selected(lens_size_index(self.0.lens_diameter.get()))
            .build();
        viewing_group.add(&lens_size);
        let resampling_labels = [
            gettext("Nearest"),
            gettext("Linear"),
            gettext("Bicubic"),
            gettext("Seam carving"),
        ];
        let resampling_model = gtk::StringList::new(
            &resampling_labels
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        let resampling = adw::ComboRow::builder()
            .title(gettext("Scaling method"))
            .model(&resampling_model)
            .selected(match self.0.scale_resampling.get() {
                Resampling::Nearest => 0,
                Resampling::Linear => 1,
                Resampling::Bicubic => 2,
                Resampling::SeamCarving => 3,
            })
            .build();
        viewing_group.add(&resampling);
        let anti_aliasing = adw::SwitchRow::builder()
            .title(gettext("Anti-aliasing"))
            .subtitle(gettext("Smooth the edges of pencil strokes and circles"))
            .active(self.0.pencil_antialiasing.get())
            .build();
        let color_format_labels = [
            gettext("Hex"),
            gettext("RGB(A)"),
            gettext("OKLab"),
            gettext("HSL"),
        ];
        let color_format_model = gtk::StringList::new(
            &color_format_labels
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        let color_format = adw::ComboRow::builder()
            .title(gettext("Copied color format"))
            .subtitle(gettext("Format used by the Color Picker tool"))
            .model(&color_format_model)
            .selected(color_format_index(self.0.settings.color_picker_format()))
            .build();
        let drawing_group = adw::PreferencesGroup::builder()
            .title(gettext("Drawing"))
            .build();
        drawing_group.add(&anti_aliasing);
        drawing_group.add(&color_format);

        page.add(&viewing_group);
        page.add(&drawing_group);
        dialog.add(&page);

        filter.connect_active_notify({
            let this = self.clone();
            move |row| {
                let filter = if row.is_active() {
                    ZoomFilter::Hard
                } else {
                    ZoomFilter::Soft
                };
                this.0.canvas.set_filter(filter);
                if let Some(canvas) = this.0.compare_canvas.borrow().as_ref() {
                    canvas.set_filter(filter);
                }
                this.0.settings.set_zoom_filter(filter);
                this.realign_zoom();
            }
        });
        background.connect_selected_notify({
            let this = self.clone();
            move |row| {
                let background = match row.selected() {
                    1 => Background::Auto,
                    2 => Background::White,
                    3 => Background::Gray,
                    4 => Background::Black,
                    _ => Background::Checkerboard,
                };
                this.0.canvas.set_background(background);
                if let Some(canvas) = this.0.compare_canvas.borrow().as_ref() {
                    canvas.set_background(background);
                }
                this.0.settings.set_background(background);
            }
        });
        lens_size.connect_selected_notify({
            let this = self.clone();
            move |row| {
                let diameter = match row.selected() {
                    1 => 280.0,
                    2 => 400.0,
                    _ => 180.0,
                };
                this.0.lens_diameter.set(diameter);
                this.0.settings.set_compare_lens_size(diameter);
            }
        });
        resampling.connect_selected_notify({
            let this = self.clone();
            move |row| {
                let resampling = match row.selected() {
                    0 => Resampling::Nearest,
                    1 => Resampling::Linear,
                    3 => Resampling::SeamCarving,
                    _ => Resampling::Bicubic,
                };
                this.0.scale_resampling.set(resampling);
                this.0.settings.set_scale_resampling(resampling);
                this.refresh_scale_method();
            }
        });
        anti_aliasing.connect_active_notify({
            let this = self.clone();
            move |row| {
                this.0.pencil_antialiasing.set(row.is_active());
                this.0.settings.set_pencil_antialiasing(row.is_active());
            }
        });
        color_format.connect_selected_notify({
            let settings = self.0.settings.clone();
            move |row| settings.set_color_picker_format(color_format_at(row.selected()))
        });
        dialog.present(Some(&self.0.window));
    }

    fn show_properties(&self) {
        if self.0.canvas.texture().is_none() {
            return;
        }
        let dialog = adw::PreferencesDialog::builder()
            .title(gettext("Image Properties"))
            .search_enabled(false)
            .build();
        let page = adw::PreferencesPage::builder()
            .title(gettext("Image Properties"))
            .build();
        let image_group = adw::PreferencesGroup::builder()
            .title(gettext("Image"))
            .build();
        let document = self.0.document.borrow().clone();
        let current_file = self.0.current_file.borrow().clone();
        let dimensions = self
            .0
            .rendered
            .borrow()
            .as_ref()
            .map(image::GenericImageView::dimensions)
            .or_else(|| {
                self.0
                    .canvas
                    .texture()
                    .map(|texture| (texture.width() as u32, texture.height() as u32))
            })
            .unwrap_or((0, 0));
        let location = current_file.as_ref().map_or_else(
            || {
                document
                    .as_ref()
                    .and_then(|document| document.source().path.as_ref())
                    .map_or_else(
                        || "Unavailable".to_owned(),
                        |path| path.display().to_string(),
                    )
            },
            |file| {
                file.path()
                    .map_or_else(|| file.uri().to_string(), |path| path.display().to_string())
            },
        );
        let metadata = document
            .as_ref()
            .map(|document| &document.source().metadata);
        let format = metadata
            .and_then(|metadata| metadata.mime_type.as_deref())
            .unwrap_or("Unknown");
        let metadata_summary = metadata.map_or_else(
            || "EXIF: Unknown · XMP: Unknown · ICC profile: Unknown".to_owned(),
            |metadata| {
                format!(
                    "EXIF: {} · XMP: {} · ICC profile: {}",
                    if metadata.exif.is_some() { "Yes" } else { "No" },
                    if metadata.xmp.is_some() { "Yes" } else { "No" },
                    if metadata.icc.is_some() { "Yes" } else { "No" },
                )
            },
        );
        let dimensions = format!("{} × {}", dimensions.0, dimensions.1);
        for row in [
            image_property_row(&gettext("Dimensions"), &dimensions),
            image_property_row(&gettext("Location"), &location),
            image_property_row(&gettext("Format"), format),
            image_property_row(&gettext("Metadata"), &metadata_summary),
        ] {
            image_group.add(&row);
        }
        page.add(&image_group);
        dialog.add(&page);
        dialog.present(Some(&self.0.window));
    }

    fn show_shortcuts(&self) {
        let dialog = adw::ShortcutsDialog::new();
        for (title, shortcuts) in [
            (
                gettext("General"),
                vec![
                    (gettext("Open"), "<Control>o"),
                    (gettext("Copy Image or Selection"), "<Control>c"),
                    (gettext("Save"), "<Control>s"),
                    (gettext("Save As"), "<Control><Shift>s"),
                    (gettext("Close"), "<Control>w"),
                    (gettext("Preferences"), "<Control>comma"),
                ],
            ),
            (
                gettext("Viewing"),
                vec![
                    (gettext("Zoom In"), "plus"),
                    (gettext("Zoom Out"), "minus"),
                    (gettext("Fit to Window"), "0"),
                    (gettext("Zoom 100%–900%"), "1–9"),
                    (gettext("Toggle Soft/Hard Zoom"), "x"),
                    (gettext("Magnifying Lens"), "l"),
                    (gettext("Previous Image"), "Left"),
                    (gettext("Next Image"), "Right"),
                    (gettext("Delete Image"), "Delete"),
                ],
            ),
            (
                gettext("Editing"),
                vec![
                    (gettext("Undo"), "<Control>z"),
                    (gettext("Redo"), "<Control><Shift>z"),
                    (gettext("Rotate Clockwise"), "r"),
                    (gettext("Rotate Counterclockwise"), "<Shift>r"),
                    (gettext("Flip Horizontally"), "h"),
                    (gettext("Flip Vertically"), "v"),
                    (gettext("Select Region"), "c"),
                    (gettext("Highlight"), "o"),
                    (gettext("Arrow"), "a"),
                    (gettext("Measure"), "m"),
                    (gettext("Text"), "t"),
                    (gettext("Scale"), "s"),
                    (gettext("Zoom Selected Region or Apply Scale"), "Return"),
                    (gettext("Move Active Tool"), "Left Right Up Down"),
                    (gettext("Set Tool Point"), "space"),
                    (gettext("Clear Selection or Cancel Scale"), "Escape"),
                    (gettext("Pencil"), "p"),
                    (gettext("Exit Active Tool"), "Escape"),
                ],
            ),
        ] {
            let section = adw::ShortcutsSection::new(Some(&title));
            for (item_title, accelerator) in shortcuts {
                section.add(adw::ShortcutsItem::new(&item_title, accelerator));
            }
            dialog.add(section);
        }
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
            let window = ViewerWindow(state.clone());
            window.sync_animation_play_button();
            window.update_action_states();
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
            .set_tooltip_text(Some(&if paused {
                gettext("Play animation")
            } else {
                gettext("Stop animation")
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
        if self.0.explicit_navigation.get() {
            if let Some(monitor) = self.0.directory_monitor.borrow_mut().take() {
                monitor.cancel();
            }
            self.prefetch_neighbors();
            self.monitor_directory();
            self.update_action_states();
            return;
        }
        self.0.sequence.replace(None);
        if let Some(monitor) = self.0.directory_monitor.borrow_mut().take() {
            monitor.cancel();
        }
        self.prefetch_neighbors();
        self.refresh_navigation(file, true);
    }

    fn refresh_navigation(&self, file: gio::File, restart_monitor: bool) {
        if self.0.explicit_navigation.get() {
            self.prefetch_neighbors();
            if restart_monitor {
                self.monitor_directory();
            }
            return;
        }
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
                    this.update_action_states();
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
            if self.0.explicit_navigation.get()
                && let Some(sequence) = self.0.sequence.borrow_mut().as_mut()
            {
                sequence.replace_file(&current, target.clone());
            }
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
            self.0.toasts.add_toast(adw::Toast::new(&gettext(
                "Image location updated after an external move",
            )));
            self.rebuild_navigation(target);
            return;
        }

        if pending.current_removed && !is_regular_file(&current) {
            let dirty = self
                .0
                .document
                .borrow()
                .as_ref()
                .is_some_and(Document::is_dirty);
            let replacement = if dirty {
                None
            } else if self.0.explicit_navigation.get() {
                self.0
                    .sequence
                    .borrow_mut()
                    .as_mut()
                    .and_then(|sequence| sequence.remove_file(&current))
            } else {
                self.0.sequence.borrow().as_ref().and_then(|sequence| {
                    sequence
                        .replacements_after_current_removed()
                        .find(is_regular_file)
                })
            };
            if !dirty && let Some(replacement) = replacement {
                self.0
                    .pending_comparison
                    .replace(self.0.compare_file.borrow().clone());
                self.load(replacement);
                self.0.toasts.add_toast(adw::Toast::new(&gettext(
                    "The previous image was moved or deleted",
                )));
                return;
            }
            if !self.0.explicit_navigation.get() {
                self.0.sequence.replace(None);
            }
            self.prefetch_neighbors();
            self.0.external_source_conflict.set(true);
            self.0.title.set_subtitle(&gettext("File moved or deleted"));
            let message = if dirty {
                gettext(
                    "The source file was moved or deleted; unsaved edits are still available via Save As",
                )
            } else {
                gettext("The current file was moved or deleted")
            };
            self.0.toasts.add_toast(adw::Toast::new(&message));
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
                    &gettext(
                        "The source file changed externally; unsaved edits were kept and Save As is required",
                    ),
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
                    state.toasts.add_toast(adw::Toast::new(&gettext(
                        "Image reloaded after an external update",
                    )));
                }
                Err(error) => {
                    tracing::warn!(%error, "Could not reload externally updated image");
                    state.toasts.add_toast(adw::Toast::new(
                        &gettext("Could not reload the updated image: {error}")
                            .replace("{error}", &error.to_string()),
                    ));
                }
            }
        });
    }

    fn choose_comparison(&self) {
        if self.0.canvas.texture().is_none() {
            self.0.toasts.add_toast(adw::Toast::new(&gettext(
                "Open the first image before comparing",
            )));
            return;
        }
        let mut builder = gtk::FileDialog::builder()
            .title(gettext("Choose Comparison Image"))
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
        if self.0.tool.get().is_vector_annotation() {
            self.set_tool(Tool::None);
        }
        let Some(primary) = self.0.canvas.texture() else {
            return;
        };
        let compare_canvas = ImageCanvas::default();
        compare_canvas.set_texture(Some(&preview.texture));
        compare_canvas.set_filter(self.0.canvas.filter());
        compare_canvas.set_background(self.0.canvas.background());
        compare_canvas.set_render_scale(self.0.render_scale.get());
        compare_canvas.set_zoom(self.0.canvas.zoom());
        compare_canvas.set_halign(gtk::Align::Center);
        compare_canvas.set_valign(gtk::Align::Center);
        compare_canvas.set_accessible_label(&gettext("Comparison image B"));
        self.0
            .canvas
            .set_accessible_label(&gettext("Primary image A"));
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
        paned.connect_position_notify({
            let primary = self.0.canvas.clone();
            let comparison = compare_canvas.clone();
            move |_| {
                primary.queue_draw();
                comparison.queue_draw();
            }
        });
        let narrow_compare = adw::Breakpoint::new(
            adw::BreakpointCondition::parse("max-width: 600px").expect("valid compare breakpoint"),
        );
        narrow_compare.add_setter(
            &paned,
            "orientation",
            Some(&gtk::Orientation::Vertical.to_value()),
        );
        let compare_bin = adw::BreakpointBin::builder().child(&paned).build();
        compare_bin.add_breakpoint(narrow_compare);

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
            .tooltip_text(gettext("Synchronize Pan and Zoom"))
            .active(true)
            .build();
        let close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text(gettext("Exit Compare Mode"))
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
        root.append(&compare_bin);
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
        self.update_action_states();
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
        let cursor = if self.0.tool.get() == Tool::PickColor {
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
                    this.step_zoom(dy < 0.0);
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
        self.apply_fit_zoom_with_alignment(fit_zooms.0, Some(ZoomAlignment::Contain));
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
        let cursor = if self.0.lens_active.get() || self.0.tool.get() == Tool::Measure {
            Some("none")
        } else if matches!(self.0.tool.get(), Tool::Select | Tool::PickColor) {
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
        self.0.canvas.set_accessible_label(&gettext("Image canvas"));
        self.update_action_states();
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
                state.toasts.add_toast(adw::Toast::new(&gettext(
                    "The comparison image was moved or deleted",
                )));
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
                this.0.canvas.queue_draw_if_device_phase_changed();
                if let Some(comparison) = this.0.compare_canvas.borrow().as_ref() {
                    comparison.queue_draw_if_device_phase_changed();
                }
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
        self.0
            .lens_button
            .set_active(!self.0.lens_button.is_active());
    }

    fn set_single_image_lens_active(&self, active: bool) {
        if self.0.canvas.texture().is_none() {
            if active {
                self.0.lens_button.set_active(false);
                self.0.toasts.add_toast(adw::Toast::new(&gettext(
                    "Open an image before using the lens",
                )));
            }
            return;
        }
        self.0.lens_active.set(active);
        self.0.canvas.set_cursor_from_name(if active {
            Some("none")
        } else {
            match self.0.tool.get() {
                Tool::Measure => Some("none"),
                Tool::None => None,
                _ => Some("crosshair"),
            }
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
                    || matches!(this.0.tool.get(), Tool::Measure | Tool::Select)
                {
                    source.clear_lens();
                    target.clear_lens();
                    let cursor = if source == this.0.canvas && this.0.tool.get() == Tool::Measure {
                        Some("none")
                    } else if source == this.0.canvas && this.0.tool.get() == Tool::Select {
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

    fn zoom_selected_region(&self) -> bool {
        let Some(selection) = self.0.region_selection.get() else {
            return false;
        };
        self.zoom_to_rect(selection);
        true
    }

    fn zoom_to_rect(&self, selection: CropOverlay) {
        let horizontal = self.0.scrolled.hadjustment();
        let vertical = self.0.scrolled.vadjustment();
        let adjustment_viewport = (horizontal.page_size(), vertical.page_size());
        let viewport = if zoom_rect_target(adjustment_viewport, selection).is_some() {
            adjustment_viewport
        } else {
            (
                f64::from(self.0.scrolled.width()),
                f64::from(self.0.scrolled.height()),
            )
        };
        let Some(target_zoom) = zoom_rect_target(viewport, selection) else {
            return;
        };
        let generation = self.0.load_generation.get();
        self.0.zoom_mode.set(ZoomMode::Manual);
        self.0.settings.set_last_zoom_mode(ZoomMode::Manual);
        self.apply_fit_zoom_with_alignment(target_zoom, None);
        let this = self.clone();
        glib::idle_add_local_once(move || {
            this.center_selection_native_point(selection, generation);
        });
        let weak = Rc::downgrade(&self.0);
        let frames = Cell::new(0);
        self.0.window.add_tick_callback(move |_, _| {
            let Some(state) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let this = ViewerWindow(state);
            if !this.center_selection_native_point(selection, generation) {
                return glib::ControlFlow::Break;
            }
            frames.set(frames.get() + 1);
            if frames.get() >= 2 {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }

    fn center_selection_native_point(&self, selection: CropOverlay, generation: u64) -> bool {
        if self.0.load_generation.get() != generation
            || self.0.region_selection.get() != Some(selection)
            || self.0.canvas.texture().is_none_or(|texture| {
                texture.width() as u32 != selection.image_width
                    || texture.height() as u32 != selection.image_height
            })
        {
            return false;
        }
        let native_center = Point {
            x: selection.x as f32 + selection.width as f32 / 2.0,
            y: selection.y as f32 + selection.height as f32 / 2.0,
        };
        let Some(center) = self.0.canvas.widget_point_for_image(native_center) else {
            return false;
        };
        let horizontal = self.0.scrolled.hadjustment();
        let vertical = self.0.scrolled.vadjustment();
        horizontal.set_value(f64::from(center.x()) - horizontal.page_size() / 2.0);
        vertical.set_value(f64::from(center.y()) - vertical.page_size() / 2.0);
        true
    }

    fn copy_selected_region(&self) {
        let Some(selection) = self.0.region_selection.get() else {
            return;
        };
        if selection.width == 0 || selection.height == 0 {
            return;
        }
        let Some(texture) = self.0.canvas.texture() else {
            return;
        };
        if texture.width() as u32 != selection.image_width
            || texture.height() as u32 != selection.image_height
        {
            return;
        }
        let Some(image) = rgba_from_texture(&texture) else {
            self.0.toasts.add_toast(adw::Toast::new(&gettext(
                "Could not copy the selected image area",
            )));
            return;
        };
        let bounds = CropBounds {
            x: selection.x,
            y: selection.y,
            width: selection.width,
            height: selection.height,
        };
        match crate::tools::selection::crop(&image, bounds) {
            Ok(fragment) => self.copy_image_to_clipboard(
                &fragment,
                &gettext("Copied {width} × {height} selection")
                    .replace("{width}", &bounds.width.to_string())
                    .replace("{height}", &bounds.height.to_string()),
            ),
            Err(error) => self.0.toasts.add_toast(adw::Toast::new(&error.to_string())),
        }
    }

    fn step_zoom(&self, zoom_in: bool) {
        if self.0.tool.get() == Tool::Pencil {
            self.abort_pencil_drag();
        }
        let zoom = if self.0.canvas.filter() == ZoomFilter::Hard {
            stepped_hard_zoom(self.0.canvas.zoom(), self.0.render_scale.get(), zoom_in)
        } else {
            self.0.canvas.zoom() * if zoom_in { 1.25 } else { 0.8 }
        };
        self.set_zoom_centered(zoom);
    }

    fn set_zoom_centered(&self, zoom: f64) {
        self.set_zoom(zoom);
        let horizontal = self.0.scrolled.hadjustment();
        let vertical = self.0.scrolled.vadjustment();
        glib::idle_add_local_once(move || {
            for adjustment in [horizontal, vertical] {
                adjustment.set_value(centered_adjustment_value(
                    adjustment.lower(),
                    adjustment.upper(),
                    adjustment.page_size(),
                ));
            }
        });
    }

    fn set_zoom(&self, zoom: f64) {
        self.set_zoom_with_alignment(zoom, Some(ZoomAlignment::Nearest));
    }

    fn set_zoom_with_alignment(&self, zoom: f64, alignment: Option<ZoomAlignment>) {
        self.0.zoom_mode.set(ZoomMode::Manual);
        self.0.settings.set_last_zoom_mode(ZoomMode::Manual);
        self.apply_zoom_with_alignment(zoom, alignment);
    }

    fn apply_zoom_with_alignment(&self, zoom: f64, alignment: Option<ZoomAlignment>) {
        self.apply_zoom_with_bounds(zoom, alignment, false);
    }

    fn apply_fit_zoom_with_alignment(&self, zoom: f64, alignment: Option<ZoomAlignment>) {
        self.apply_zoom_with_bounds(zoom, alignment, true);
    }

    fn apply_zoom_with_bounds(
        &self,
        zoom: f64,
        alignment: Option<ZoomAlignment>,
        fit_bounds: bool,
    ) {
        self.0.pending_fit.set(None);
        let zoom = if self.0.canvas.filter() == ZoomFilter::Hard {
            alignment.map_or(zoom, |alignment| {
                if fit_bounds {
                    aligned_hard_fit_zoom(zoom, self.0.render_scale.get(), alignment)
                } else {
                    aligned_hard_zoom(zoom, self.0.render_scale.get(), alignment)
                }
            })
        } else {
            zoom
        };
        if fit_bounds {
            self.0.canvas.set_fit_zoom(zoom);
        } else {
            self.0.canvas.set_zoom(zoom);
        }
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
            let zoom = if compare.filter() == ZoomFilter::Hard {
                alignment.map_or(zoom, |alignment| {
                    if fit_bounds {
                        aligned_hard_fit_zoom(zoom, self.0.render_scale.get(), alignment)
                    } else {
                        aligned_hard_zoom(zoom, self.0.render_scale.get(), alignment)
                    }
                })
            } else {
                zoom
            };
            if fit_bounds {
                compare.set_fit_zoom(zoom);
            } else {
                compare.set_zoom(zoom);
            }
        }
        self.update_minimap();
    }

    fn realign_zoom(&self) {
        match self.0.zoom_mode.get() {
            ZoomMode::Fit if self.0.canvas.texture().is_some() => self.fit(false),
            ZoomMode::Fill if self.0.canvas.texture().is_some() => self.fit(true),
            ZoomMode::Fit | ZoomMode::Fill | ZoomMode::Manual => {
                self.apply_zoom_with_alignment(self.0.canvas.zoom(), Some(ZoomAlignment::Nearest))
            }
        }
    }

    fn install_render_scale_tracking(&self) {
        self.0.window.connect_realize({
            let this = self.clone();
            move |window| {
                let Some(surface) = window.surface() else {
                    return;
                };
                this.update_render_scale(surface.scale());
                let weak = Rc::downgrade(&this.0);
                surface.connect_scale_notify(move |surface| {
                    if let Some(state) = weak.upgrade() {
                        ViewerWindow(state).update_render_scale(surface.scale());
                    }
                });
            }
        });
    }

    fn update_render_scale(&self, scale: f64) {
        let scale = sanitized_render_scale(scale);
        self.0.render_scale.set(scale);
        self.0.canvas.set_render_scale(scale);
        if let Some(compare) = self.0.compare_canvas.borrow().as_ref() {
            compare.set_render_scale(scale);
        }
        self.realign_zoom();
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
            adjustment.connect_value_changed(move |_| {
                this.0.canvas.queue_draw_if_device_phase_changed();
                this.update_minimap();
            });
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
        let new_zoom = if self.0.canvas.filter() == ZoomFilter::Hard {
            stepped_hard_zoom(old_zoom, self.0.render_scale.get(), factor > 1.0)
        } else {
            old_zoom * factor
        }
        .clamp(0.01, 64.0);
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
        if next.is_none() && self.0.sequence.borrow().is_none() {
            let Some(current) = self.0.current_file.borrow().clone() else {
                return;
            };
            let fallback = self.0.settings.folder_sort();
            let generation = self.0.directory_refresh_generation.get().wrapping_add(1);
            self.0.directory_refresh_generation.set(generation);
            let weak = Rc::downgrade(&self.0);
            glib::spawn_future_local(async move {
                let sequence =
                    gio::spawn_blocking(move || DirectorySequence::build(&current, fallback)).await;
                let Some(state) = weak.upgrade() else {
                    return;
                };
                if state.directory_refresh_generation.get() != generation {
                    return;
                }
                match sequence {
                    Ok(Ok(sequence)) => {
                        state.sequence.replace(Some(sequence));
                        ViewerWindow(state.clone()).update_action_states();
                        ViewerWindow(state).navigate(forward);
                    }
                    Ok(Err(error)) => tracing::debug!(%error, "Directory navigation unavailable"),
                    Err(_) => tracing::warn!("Directory navigation worker panicked"),
                }
            });
            return;
        }
        if let Some((next_sequence, file)) = next {
            if !is_regular_file(&file) {
                if self.0.explicit_navigation.get() {
                    if let Some(sequence) = self.0.sequence.borrow_mut().as_mut() {
                        sequence.remove_file(&file);
                    }
                    self.prefetch_neighbors();
                    self.0.toasts.add_toast(adw::Toast::new(&gettext(
                        "That image was moved or deleted; it was removed from the opened files",
                    )));
                    self.navigate(forward);
                    return;
                }
                if let Some(current) = self.0.current_file.borrow().clone() {
                    self.refresh_navigation(current, false);
                }
                self.0.toasts.add_toast(adw::Toast::new(&gettext(
                    "That image was moved or deleted; the folder was refreshed",
                )));
                return;
            }
            self.0.sequence.replace(Some(next_sequence));
            let Some(compare_file) = self.0.compare_file.borrow().clone() else {
                self.load_preserving_zoom(file);
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
                ViewerWindow(state).load_preserving_zoom(file);
            });
        }
    }

    fn confirm_delete_current_file(&self) {
        if self.0.export_cancellation.borrow().is_some() {
            self.0.toasts.add_toast(adw::Toast::new(&gettext(
                "Wait for the current export to finish",
            )));
            return;
        }
        if self.0.deletion_running.get() {
            self.0.toasts.add_toast(adw::Toast::new(&gettext(
                "Image deletion is already in progress",
            )));
            return;
        }
        let Some(file) = self.0.current_file.borrow().clone() else {
            self.0
                .toasts
                .add_toast(adw::Toast::new(&gettext("No image is open")));
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
            gettext(
                "“{name}” and its unsaved edits will be permanently deleted. This cannot be undone.",
            )
            .replace("{name}", &name)
        } else {
            gettext("“{name}” will be permanently deleted. This cannot be undone.")
                .replace("{name}", &name)
        };
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Delete this image?"))
            .body(body)
            .close_response("cancel")
            .default_response("cancel")
            .build();
        dialog.add_response("cancel", &gettext("Cancel"));
        dialog.add_response("delete", &gettext("Delete"));
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
        let explicit_navigation = self.0.explicit_navigation.get();
        let fallback = self.0.settings.folder_sort();
        if let Some(monitor) = self.0.directory_monitor.borrow_mut().take() {
            monitor.cancel();
        }
        let generation = self.0.load_generation.get();
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let replacement = if explicit_navigation || known_replacement.is_some() {
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
                state.toasts.add_toast(adw::Toast::new(
                    &gettext("Could not delete image: {error}")
                        .replace("{error}", &error.to_string()),
                ));
                this.monitor_directory();
                return;
            }
            if state.load_generation.get() != generation
                || !files_equal(&state.current_file.borrow(), &Some(file.clone()))
            {
                state.deletion_running.set(false);
                this.monitor_directory();
                return;
            }
            if let Some(document) = state.document.borrow_mut().as_mut() {
                document.restore_original();
            }
            let replacement = if state.explicit_navigation.get() {
                state
                    .sequence
                    .borrow_mut()
                    .as_mut()
                    .and_then(|sequence| sequence.remove_file(&file))
            } else {
                replacement
            };
            state.deletion_running.set(false);
            state
                .toasts
                .add_toast(adw::Toast::new(&gettext("Image deleted")));
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

    fn set_fit_mode(&self, fill: bool) {
        let mode = if fill { ZoomMode::Fill } else { ZoomMode::Fit };
        self.0.zoom_mode.set(mode);
        self.0.settings.set_last_zoom_mode(mode);
        self.fit(fill);
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
        let zoom = if fill {
            horizontal.max(vertical)
        } else {
            horizontal.min(vertical)
        };
        let alignment = if fill {
            ZoomAlignment::Cover
        } else {
            ZoomAlignment::Contain
        };
        if !fill
            && self.0.tool.get() == Tool::Scale
            && self.0.scale_preview_view.get() == ScalePreviewView::Fit
        {
            self.0.pending_fit.set(None);
            let zoom = if self.0.canvas.filter() == ZoomFilter::Hard {
                aligned_hard_fit_zoom(zoom, self.0.render_scale.get(), alignment)
            } else {
                zoom
            };
            self.set_scale_preview_fit_zoom(zoom);
        } else {
            self.apply_fit_zoom_with_alignment(zoom, Some(alignment));
        }
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
                this.set_zoom_with_alignment(target_zoom, None);
                let horizontal = this.0.scrolled.hadjustment();
                let vertical = this.0.scrolled.vadjustment();
                horizontal.set_value(horizontal_target);
                vertical.set_value(vertical_target);
            }
        });
        zoom.connect_end({
            let this = self.clone();
            let zoom_anchor = zoom_anchor.clone();
            let zoom_adjustment_target = zoom_adjustment_target.clone();
            move |_, _| {
                if let Some(anchor) = zoom_anchor.take() {
                    let target_zoom = if this.0.canvas.filter() == ZoomFilter::Hard {
                        aligned_hard_zoom(
                            this.0.canvas.zoom(),
                            this.0.render_scale.get(),
                            ZoomAlignment::Nearest,
                        )
                    } else {
                        this.0.canvas.zoom()
                    };
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
                    this.set_zoom(target_zoom);
                    this.0.scrolled.hadjustment().set_value(horizontal_target);
                    this.0.scrolled.vadjustment().set_value(vertical_target);
                }
                zoom_adjustment_target.set(None);
            }
        });
        zoom.connect_cancel({
            let zoom_anchor = zoom_anchor.clone();
            let zoom_adjustment_target = zoom_adjustment_target.clone();
            move |_, _| {
                zoom_anchor.set(None);
                zoom_adjustment_target.set(None);
            }
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

        let color_picker = gtk::GestureClick::new();
        color_picker.set_button(1);
        color_picker.connect_pressed({
            let this = self.clone();
            move |gesture, _, x, y| {
                if this.0.tool.get() != Tool::PickColor {
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

        let region_cursor = gtk::EventControllerMotion::new();
        region_cursor.connect_motion({
            let this = self.clone();
            move |_, x, y| {
                if this.0.lens_active.get() {
                    this.0.canvas.set_cursor_from_name(Some("none"));
                    return;
                }
                if this.0.tool.get() != Tool::Select {
                    return;
                }
                let cursor = this
                    .0
                    .region_selection
                    .get()
                    .and_then(|crop| this.0.canvas.crop_display_bounds(crop))
                    .map_or("crosshair", |rect| {
                        region_resize_cursor(rect, x as f32, y as f32)
                    });
                this.0.canvas.set_cursor_from_name(Some(cursor));
            }
        });
        region_cursor.connect_leave({
            let this = self.clone();
            move |_| {
                let cursor = if this.0.lens_active.get() || this.0.tool.get() == Tool::Measure {
                    Some("none")
                } else if matches!(this.0.tool.get(), Tool::Select | Tool::PickColor) {
                    Some("crosshair")
                } else {
                    None
                };
                this.0.canvas.set_cursor_from_name(cursor)
            }
        });
        self.0.canvas.add_controller(region_cursor);

        let region_drag = gtk::GestureDrag::new();
        region_drag.set_button(1);
        region_drag.connect_drag_begin({
            let this = self.clone();
            move |gesture, x, y| {
                if this.0.tool.get() != Tool::Select {
                    return;
                }
                if let Some(crop) = this.0.region_selection.get()
                    && let Some(rect) = this.0.canvas.crop_display_bounds(crop)
                {
                    let (left, right, top, bottom) = region_edge_hit(rect, x as f32, y as f32);
                    if left || right || top || bottom {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        this.0.region_drag.set(Some(RegionDrag::Resizing {
                            crop,
                            start_screen: (x, y),
                            left,
                            right,
                            top,
                            bottom,
                        }));
                        return;
                    }
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
                this.0.canvas.grab_focus();
                let marking = SelectionDrag {
                    start,
                    current: start,
                    start_screen: (x, y),
                    image_dimensions,
                };
                this.set_region_selection(None);
                this.0.region_drag.set(Some(RegionDrag::Marking(marking)));
            }
        });
        region_drag.connect_drag_update({
            let this = self.clone();
            move |_, dx, dy| {
                let Some(drag) = this.0.region_drag.get() else {
                    return;
                };
                match drag {
                    RegionDrag::Marking(mut marking) => {
                        let Some(current) = this.0.canvas.clamped_pixel_boundary_at(
                            marking.start_screen.0 + dx,
                            marking.start_screen.1 + dy,
                        ) else {
                            return;
                        };
                        marking.current = current;
                        let selection = boundary_overlay(
                            marking.start,
                            marking.current,
                            marking.image_dimensions,
                        );
                        this.set_region_selection(
                            (selection.width > 0 && selection.height > 0).then_some(selection),
                        );
                        this.0.region_drag.set(Some(RegionDrag::Marking(marking)));
                    }
                    RegionDrag::Resizing {
                        crop,
                        start_screen,
                        left,
                        right,
                        top,
                        bottom,
                    } => {
                        let Some((x, y)) = this
                            .0
                            .canvas
                            .clamped_pixel_boundary_at(start_screen.0 + dx, start_screen.1 + dy)
                        else {
                            return;
                        };
                        let crop = resize_region(crop, x, y, left, right, top, bottom);
                        this.set_region_selection(Some(crop));
                    }
                }
            }
        });
        region_drag.connect_drag_end({
            let this = self.clone();
            move |_, dx, dy| {
                let Some(drag) = this.0.region_drag.take() else {
                    return;
                };
                match drag {
                    RegionDrag::Marking(mut marking) => {
                        if let Some(current) = this
                            .0
                            .canvas
                            .clamped_pixel_boundary_at(
                                marking.start_screen.0 + dx,
                                marking.start_screen.1 + dy,
                            )
                        {
                            marking.current = current;
                        }
                        let selection = boundary_overlay(
                            marking.start,
                            marking.current,
                            marking.image_dimensions,
                        );
                        let selection =
                            (selection.width > 0 && selection.height > 0).then_some(selection);
                        this.set_region_selection(selection);
                        if let Some(selection) = selection {
                            this.0.canvas.announce(
                                &gettext(
                                    "Region selected, {width} by {height} pixels. Choose zoom, crop, or copy.",
                                )
                                .replace("{width}", &selection.width.to_string())
                                .replace("{height}", &selection.height.to_string()),
                                gtk::AccessibleAnnouncementPriority::Medium,
                            );
                        }
                    }
                    RegionDrag::Resizing {
                        crop,
                        start_screen,
                        left,
                        right,
                        top,
                        bottom,
                    } => {
                        if let Some((x, y)) = this
                            .0
                            .canvas
                            .clamped_pixel_boundary_at(start_screen.0 + dx, start_screen.1 + dy)
                        {
                            let crop = resize_region(crop, x, y, left, right, top, bottom);
                            this.set_region_selection(Some(crop));
                        }
                    }
                }
            }
        });
        region_drag.connect_cancel({
            let this = self.clone();
            move |_, _| {
                let Some(drag) = this.0.region_drag.take() else {
                    return;
                };
                match drag {
                    RegionDrag::Marking(_) => this.set_region_selection(None),
                    RegionDrag::Resizing { crop, .. } => {
                        this.set_region_selection(Some(crop));
                    }
                }
            }
        });
        self.0.canvas.add_controller(region_drag);

        let pencil = gtk::GestureDrag::new();
        pencil.set_button(1);
        pencil.connect_drag_begin({
            let this = self.clone();
            move |gesture, x, y| {
                if this.0.tool.get() != Tool::Pencil {
                    return;
                }
                if !pencil_drag_available(this.annotation_hit_at(x, y)) {
                    return;
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);
                this.begin_pencil_drag(
                    &this.0.canvas,
                    x,
                    y,
                    gesture.current_event_state(),
                    pencil_event_time(gesture),
                );
            }
        });
        pencil.connect_drag_update({
            let this = self.clone();
            move |gesture, offset_x, offset_y| {
                if this.0.tool.get() != Tool::Pencil {
                    return;
                }
                let Some(drag) = this.0.pencil_drag.borrow().as_ref().map(|drag| {
                    (
                        drag.start_screen.0 + offset_x,
                        drag.start_screen.1 + offset_y,
                    )
                }) else {
                    return;
                };
                this.update_pencil_drag(&this.0.canvas, drag.0, drag.1, pencil_event_time(gesture));
            }
        });
        pencil.connect_drag_end({
            let this = self.clone();
            move |gesture, offset_x, offset_y| {
                if this.0.tool.get() != Tool::Pencil {
                    return;
                }
                let Some(drag) = this.0.pencil_drag.borrow().as_ref().map(|drag| {
                    (
                        drag.start_screen.0 + offset_x,
                        drag.start_screen.1 + offset_y,
                    )
                }) else {
                    return;
                };
                let Some((points, _path, mode)) = this.finish_pencil_drag(
                    &this.0.canvas,
                    drag.0,
                    drag.1,
                    pencil_event_time(gesture),
                ) else {
                    return;
                };
                if !points.is_empty() {
                    this.commit_editable_pencil_stroke(&points, mode);
                }
            }
        });
        self.0.canvas.add_controller(pencil);

        let sampler = gtk::GestureClick::new();
        sampler.set_button(3);
        sampler.connect_pressed({
            let this = self.clone();
            move |gesture, _, x, y| {
                if this.0.tool.get() != Tool::Pencil {
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
                    this.apply_picked_color(color);
                    let color_value = format!(
                        "#{:02X}{:02X}{:02X}{:02X} · rgba({}, {}, {}, {})",
                        color[0],
                        color[1],
                        color[2],
                        color[3],
                        color[0],
                        color[1],
                        color[2],
                        color[3]
                    );
                    this.0.toasts.add_toast(adw::Toast::new(
                        &gettext("Sampled {color}").replace("{color}", &color_value),
                    ));
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
            move |gesture, x, y| {
                if this.0.tool.get() != Tool::Pencil || this.0.compare_rendered.borrow().is_none() {
                    return;
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);
                this.begin_pencil_drag(
                    &canvas,
                    x,
                    y,
                    gesture.current_event_state(),
                    pencil_event_time(gesture),
                );
            }
        });
        pencil.connect_drag_update({
            let this = self.clone();
            let canvas = canvas.clone();
            move |gesture, offset_x, offset_y| {
                if this.0.tool.get() != Tool::Pencil {
                    return;
                }
                let Some(drag) = this.0.pencil_drag.borrow().as_ref().map(|drag| {
                    (
                        drag.start_screen.0 + offset_x,
                        drag.start_screen.1 + offset_y,
                    )
                }) else {
                    return;
                };
                this.update_pencil_drag(&canvas, drag.0, drag.1, pencil_event_time(gesture));
            }
        });
        pencil.connect_drag_end({
            let this = self.clone();
            let canvas = canvas.clone();
            move |gesture, offset_x, offset_y| {
                if this.0.tool.get() != Tool::Pencil {
                    return;
                }
                let Some(drag) = this.0.pencil_drag.borrow().as_ref().map(|drag| {
                    (
                        drag.start_screen.0 + offset_x,
                        drag.start_screen.1 + offset_y,
                    )
                }) else {
                    return;
                };
                let Some((points, path, _mode)) =
                    this.finish_pencil_drag(&canvas, drag.0, drag.1, pencil_event_time(gesture))
                else {
                    return;
                };
                if !points.is_empty() {
                    this.commit_comparison_pencil_stroke(&canvas, &points, path);
                }
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
                if this.0.tool.get() != Tool::Pencil {
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
                this.apply_picked_color(color);
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
                if this.0.tool.get() != Tool::PickColor {
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
            .heading(gettext(heading))
            .body(gettext("This cannot be undone."))
            .close_response("cancel")
            .default_response("cancel")
            .build();
        dialog.add_response("cancel", &gettext("Cancel"));
        dialog.add_response("discard", &gettext("Discard"));
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
    let color_picker_button = toggle_button(
        "color-select-symbolic",
        "Pick Color — click a pixel to copy its value",
    );
    let measurement_button = toggle_button(
        "ruler-measure-symbolic",
        "Measure — drag an axis-aligned measurement line",
    );
    let pencil_button = toggle_button("pencil-symbolic", "Pencil");
    let highlight_button = toggle_button("highlight-symbolic", "Highlight");
    let arrow_button = toggle_button("arrow-symbolic", "Arrow");
    let text_button = toggle_button("text-symbolic", "Text");
    let lens_button = toggle_button("edit-find-symbolic", "Toggle 4× Lens");
    let color_button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    color_button.set_rgba(&u8_to_rgba(
        crate::tools::annotation::DEFAULT_ANNOTATION_COLOR,
    ));
    color_button.set_tooltip_text(Some(&gettext("Annotation color")));
    let pencil_size = spin(1.0, 128.0, 1.0);
    pencil_size.set_width_chars(2);
    pencil_size.set_max_width_chars(3);
    pencil_size.set_tooltip_text(Some(&gettext("Width in image pixels")));
    let pencil_controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    pencil_controls.add_css_class("toolbar");
    pencil_controls.add_css_class("osd");
    pencil_controls.append(&pencil_button);
    pencil_controls.append(&highlight_button);
    pencil_controls.append(&arrow_button);
    pencil_controls.append(&measurement_button);
    pencil_controls.append(&text_button);
    pencil_controls.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    pencil_controls.append(&color_button);
    pencil_controls.append(&color_picker_button);
    pencil_controls.append(&lens_button);
    pencil_controls.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    pencil_controls.append(&pencil_size);
    header.pack_start(&animation_controls);
    header.pack_start(&previous);
    header.pack_start(&next);
    header.pack_end(&menu_button());
    let save_as_button = button("media-floppy-symbolic", "Save As", "win.save-as");
    header.pack_end(&save_as_button);
    HeaderWidgets {
        header,
        save_as_button,
        animation_controls,
        animation_play_button: play,
        scale_button,
        measurement_button,
        highlight_button,
        arrow_button,
        text_button,
        color_picker_button,
        pencil_button,
        lens_button,
        color_button,
        pencil_size,
        pencil_controls,
    }
}

fn add_development_icon_search_path() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/data/icons"));
    let theme = gtk::IconTheme::for_display(&display);
    if root.is_dir() && !theme.search_path().iter().any(|path| path == root) {
        theme.add_search_path(root);
    }
}

fn button(icon: &str, tooltip: &str, action: &str) -> gtk::Button {
    let label = gettext(tooltip);
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(&label)
        .action_name(action)
        .build();
    button.update_property(&[gtk::accessible::Property::Label(&label)]);
    button
}

fn toggle_button(icon: &str, tooltip: &str) -> gtk::ToggleButton {
    let label = gettext(tooltip);
    let button = gtk::ToggleButton::builder()
        .icon_name(icon)
        .tooltip_text(&label)
        .build();
    button.update_property(&[gtk::accessible::Property::Label(&label)]);
    button
}

fn menu_button() -> gtk::MenuButton {
    let menu = main_menu();
    let label = gettext("Main Menu");
    let button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text(&label)
        .menu_model(&menu)
        .build();
    button.update_property(&[gtk::accessible::Property::Label(&label)]);
    button
}

fn menu_item(menu: &gio::Menu, label: &str, action: &str) {
    menu.append(Some(&gettext(label)), Some(action));
}

fn menu_submenu(menu: &gio::Menu, label: &str, submenu: &gio::Menu) {
    menu.append_submenu(Some(&gettext(label)), submenu);
}

fn main_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    menu_item(&menu, "Open…", "win.open");
    menu_item(&menu, "Open With…", "win.open-with");
    menu_item(&menu, "Copy Image or Selection", "win.copy-image");
    menu_item(&menu, "Save", "win.save");
    menu_item(&menu, "Save As…", "win.save-as");
    menu_item(&menu, "Compare Images…", "win.compare");
    let edit_menu = gio::Menu::new();
    menu_item(&edit_menu, "Pencil", "win.pencil");
    menu_item(&edit_menu, "Highlight", "win.highlight");
    menu_item(&edit_menu, "Arrow", "win.arrow");
    menu_item(&edit_menu, "Measure", "win.measure");
    menu_item(&edit_menu, "Text", "win.text");
    menu_item(&edit_menu, "Select Region", "win.select");
    menu_item(
        &edit_menu,
        "Rotate Counterclockwise",
        "win.rotate-counterclockwise",
    );
    menu_item(&edit_menu, "Rotate Clockwise", "win.rotate-clockwise");
    menu_item(&edit_menu, "Flip Horizontally", "win.flip-horizontal");
    menu_item(&edit_menu, "Flip Vertically", "win.flip-vertical");
    menu_item(&edit_menu, "Scale", "win.scale-preview");
    menu_submenu(&menu, "Edit", &edit_menu);
    menu_item(&menu, "Magnifying Lens", "win.lens");
    menu_item(&menu, "Image Properties", "win.properties");
    menu_item(&menu, "Preferences", "win.preferences");
    menu_item(&menu, "Keyboard Shortcuts", "win.shortcuts");
    menu_item(&menu, "About Diorama", "win.about");
    menu
}

fn open_with_launcher(file: &gio::File) -> gtk::FileLauncher {
    let launcher = gtk::FileLauncher::new(Some(file));
    launcher.set_always_ask(true);
    launcher.set_writable(true);
    launcher
}

fn open_with_was_cancelled(error: &glib::Error) -> bool {
    error.matches(gtk::DialogError::Cancelled)
        || error.matches(gtk::DialogError::Dismissed)
        || error.matches(gio::IOErrorEnum::Cancelled)
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
    texture_from_owned_rgba(image.clone())
}

fn texture_from_owned_rgba(image: image::RgbaImage) -> Result<gtk::gdk::Texture, String> {
    let width = i32::try_from(image.width()).map_err(|_| "Image width is too large".to_owned())?;
    let height =
        i32::try_from(image.height()).map_err(|_| "Image height is too large".to_owned())?;
    let stride = usize::try_from(u64::from(image.width()) * 4)
        .map_err(|_| "Image stride is too large".to_owned())?;
    let bytes = glib::Bytes::from_owned(image.into_raw());
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
    fn region_handles_use_directional_resize_cursors() {
        let rect = gtk::graphene::Rect::new(20.0, 30.0, 100.0, 80.0);

        assert_eq!(region_resize_cursor(rect, 20.0, 30.0), "nwse-resize");
        assert_eq!(region_resize_cursor(rect, 120.0, 30.0), "nesw-resize");
        assert_eq!(region_resize_cursor(rect, 70.0, 30.0), "ns-resize");
        assert_eq!(region_resize_cursor(rect, 20.0, 70.0), "ew-resize");
        assert_eq!(region_resize_cursor(rect, 20.0, 10.0), "default");
    }

    #[test]
    fn unmodified_horizontal_arrows_navigate_images() {
        assert_eq!(
            image_navigation_direction(gtk::gdk::Key::Left, gtk::gdk::ModifierType::empty(), false),
            Some(false)
        );
        assert_eq!(
            image_navigation_direction(
                gtk::gdk::Key::Right,
                gtk::gdk::ModifierType::LOCK_MASK,
                false
            ),
            Some(true)
        );
        assert_eq!(
            image_navigation_direction(
                gtk::gdk::Key::Left,
                gtk::gdk::ModifierType::SHIFT_MASK,
                false
            ),
            None
        );
        assert_eq!(
            image_navigation_direction(gtk::gdk::Key::Right, gtk::gdk::ModifierType::empty(), true),
            None
        );
        assert_eq!(
            image_navigation_direction(gtk::gdk::Key::Up, gtk::gdk::ModifierType::empty(), false),
            None
        );
    }

    #[test]
    fn pencil_mode_claims_zoom_keys_without_treating_control_as_a_line_modifier() {
        assert_eq!(
            pencil_zoom_key(
                gtk::gdk::Key::plus,
                gtk::gdk::ModifierType::SHIFT_MASK,
                true,
            ),
            Some(PencilZoomKey::In)
        );
        assert_eq!(
            pencil_zoom_key(
                gtk::gdk::Key::equal,
                gtk::gdk::ModifierType::CONTROL_MASK,
                true,
            ),
            Some(PencilZoomKey::In)
        );
        assert_eq!(
            pencil_zoom_key(
                gtk::gdk::Key::KP_Subtract,
                gtk::gdk::ModifierType::empty(),
                true,
            ),
            Some(PencilZoomKey::Out)
        );
        assert_eq!(
            pencil_zoom_key(gtk::gdk::Key::minus, gtk::gdk::ModifierType::ALT_MASK, true),
            None
        );
        assert_eq!(
            pencil_zoom_key(gtk::gdk::Key::plus, gtk::gdk::ModifierType::empty(), false),
            None
        );
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
    fn image_subtitle_includes_folder_and_places_modified_time_after_zoom() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        let modified = now - Duration::from_secs(3 * 60);

        assert_eq!(
            image_subtitle(
                "/images/comparison",
                (1920, 1080),
                1.25,
                Some(modified),
                now
            ),
            "/images/comparison · 1920 × 1080 · 125% · 3 minutes ago"
        );
        assert_eq!(
            image_subtitle("/images/comparison", (640, 480), 0.5, None, now),
            "/images/comparison · 640 × 480 · 50%"
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
    fn navigation_reapplies_the_selected_fit_mode() {
        assert_eq!(fit_on_load(false, ZoomMode::Fit), Some(false));
        assert_eq!(fit_on_load(false, ZoomMode::Fill), Some(true));
        assert_eq!(fit_on_load(false, ZoomMode::Manual), None);
        assert_eq!(fit_on_load(true, ZoomMode::Manual), Some(false));
    }

    #[test]
    fn hard_zoom_aligns_to_the_integer_render_grid() {
        let render_scale = 2.0;

        assert_eq!(
            aligned_hard_zoom(1.0, render_scale, ZoomAlignment::Nearest),
            1.0
        );
        assert_eq!(
            aligned_hard_zoom(1.24, render_scale, ZoomAlignment::Nearest),
            1.0
        );
        assert_eq!(
            aligned_hard_zoom(1.26, render_scale, ZoomAlignment::Nearest),
            1.5
        );
        assert_eq!(
            aligned_hard_zoom(1.4, render_scale, ZoomAlignment::Contain),
            1.0
        );
        assert_eq!(
            aligned_hard_zoom(1.4, render_scale, ZoomAlignment::Cover),
            1.5
        );
        assert_eq!(
            aligned_hard_zoom(0.75, render_scale, ZoomAlignment::Nearest),
            1.0
        );
        assert_eq!(
            aligned_hard_zoom(1.0, f64::NAN, ZoomAlignment::Nearest),
            1.0
        );
        assert_eq!(sanitized_render_scale(1.666_667), 1.666_667);
    }

    #[test]
    fn hard_fit_can_downscale_an_oversized_image() {
        assert_eq!(aligned_hard_fit_zoom(0.2, 1.0, ZoomAlignment::Contain), 0.2);
        assert_eq!(aligned_hard_fit_zoom(0.4, 2.0, ZoomAlignment::Contain), 0.4);
    }

    #[test]
    fn hard_zoom_steps_by_whole_render_pixels_above_actual_size() {
        let render_scale = 2.0;

        assert_eq!(stepped_hard_zoom(1.0, render_scale, true), 1.5);
        assert_eq!(stepped_hard_zoom(1.5, render_scale, true), 2.0);
        assert_eq!(stepped_hard_zoom(2.0, render_scale, false), 1.5);
        assert_eq!(stepped_hard_zoom(1.5, render_scale, false), 1.0);
        assert_eq!(stepped_hard_zoom(1.0, render_scale, false), 0.5);
        assert_eq!(stepped_hard_zoom(0.8, render_scale, false), 0.5);
    }

    #[test]
    fn compare_layout_rejects_placeholder_allocations() {
        assert!(!usable_panel_size((1, 600)));
        assert!(!usable_panel_size((800, 1)));
        assert!(usable_panel_size((800, 600)));
    }

    #[test]
    fn pencil_requires_an_editable_image() {
        assert!(!pencil_can_activate(false));
        assert!(pencil_can_activate(true));
    }

    #[test]
    fn pencil_drag_modifiers_select_shapes_and_reuse_only_line_anchors() {
        let origin = BrushPoint {
            x: 2.5,
            y: 3.5,
            pressure: 1.0,
        };
        let anchor = BrushPoint {
            x: 8.5,
            y: 9.5,
            pressure: 1.0,
        };

        assert_eq!(
            pencil_drag_mode(gtk::gdk::ModifierType::CONTROL_MASK),
            PencilDragMode::Line
        );
        assert_eq!(
            pencil_drag_mode(gtk::gdk::ModifierType::SHIFT_MASK),
            PencilDragMode::Rectangle
        );
        assert_eq!(
            pencil_drag_mode(gtk::gdk::ModifierType::ALT_MASK),
            PencilDragMode::Circle
        );
        assert_eq!(
            pencil_drag_mode(
                gtk::gdk::ModifierType::CONTROL_MASK
                    | gtk::gdk::ModifierType::SHIFT_MASK
                    | gtk::gdk::ModifierType::ALT_MASK
            ),
            PencilDragMode::Circle
        );
        assert_eq!(
            pencil_line_start(PencilDragMode::Line, Some(anchor), origin),
            anchor
        );
        assert_eq!(
            pencil_line_start(PencilDragMode::Rectangle, Some(anchor), origin),
            origin
        );
        assert_eq!(
            pencil_drag_path(PencilDragMode::Freehand),
            StrokePath::Smooth
        );
        assert_eq!(pencil_drag_path(PencilDragMode::Line), StrokePath::Linear);
        assert_eq!(
            pencil_drag_path(PencilDragMode::Rectangle),
            StrokePath::Linear
        );
        assert_eq!(pencil_drag_path(PencilDragMode::Circle), StrokePath::Circle);
    }

    #[test]
    fn pencil_drag_modes_create_editable_geometry() {
        let start = BrushPoint {
            x: 10.5,
            y: 20.5,
            pressure: 0.5,
        };
        let end = BrushPoint {
            x: 30.5,
            y: 40.5,
            pressure: 1.0,
        };
        assert!(matches!(
            pencil_geometry(PencilDragMode::Freehand, &[start, end]),
            Some(PencilGeometry::Freehand(points)) if points == [start, end]
        ));
        assert!(matches!(
            pencil_geometry(PencilDragMode::Line, &[start, end]),
            Some(PencilGeometry::Line(points)) if points.len() == 2
        ));
        assert!(matches!(
            pencil_geometry(PencilDragMode::Rectangle, &[start, start, end]),
            Some(PencilGeometry::Rectangle(_))
        ));
        assert!(matches!(
            pencil_geometry(PencilDragMode::Circle, &[start, end]),
            Some(PencilGeometry::Ellipse(Rect { width, height, .. }))
                if (width - height).abs() <= f32::EPSILON
        ));
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn circle_drag_does_not_fall_back_to_buffered_freehand_points() {
        adw::init().expect("GTK display initialization");
        let origin = BrushPoint {
            x: 10.5,
            y: 12.5,
            pressure: 1.0,
        };
        let current = BrushPoint {
            x: 18.5,
            y: 12.5,
            pressure: 1.0,
        };
        let hidden_freehand_point = BrushPoint {
            x: 14.5,
            y: 16.5,
            pressure: 1.0,
        };
        let drag = PencilDrag {
            canvas: ImageCanvas::default(),
            start_screen: (0.0, 0.0),
            mode: PencilDragMode::Circle,
            origin,
            line_start: origin,
            current,
            freehand_points: [origin, hidden_freehand_point, current]
                .into_iter()
                .enumerate()
                .map(|(index, point)| crate::tools::pencil::TimedBrushPoint {
                    point,
                    timestamp_ms: index as u32,
                })
                .collect(),
        };

        let points = pencil_drag_points(&drag);

        assert_eq!(
            points,
            crate::tools::pencil::shape_points(crate::tools::pencil::PencilShape::Circle {
                center: origin,
                edge: current,
            })
        );
        assert!(!points.contains(&hidden_freehand_point));
        assert_eq!(pencil_drag_path(drag.mode), StrokePath::Circle);
    }

    #[test]
    fn freehand_preview_waits_until_the_pointer_enters_another_source_pixel() {
        assert!(!pencil_drag_should_preview(PencilDragMode::Freehand, 1));
        assert!(pencil_drag_should_preview(PencilDragMode::Freehand, 2));
        assert!(pencil_drag_should_preview(PencilDragMode::Line, 1));
        assert!(pencil_drag_should_preview(PencilDragMode::Rectangle, 1));
        assert!(pencil_drag_should_preview(PencilDragMode::Circle, 1));
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
    fn edit_menu_contains_tools_and_transforms_directly() {
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
        assert_eq!(
            (0..edit_menu.n_items())
                .map(|index| string_attribute(&edit_menu, index, "label"))
                .collect::<Vec<_>>(),
            [
                "Pencil".to_owned(),
                "Highlight".to_owned(),
                "Arrow".to_owned(),
                "Measure".to_owned(),
                "Text".to_owned(),
                "Select Region".to_owned(),
                "Rotate Counterclockwise".to_owned(),
                "Rotate Clockwise".to_owned(),
                "Flip Horizontally".to_owned(),
                "Flip Vertically".to_owned(),
                "Scale".to_owned(),
            ]
        );
        for (index, action) in [
            (0, "win.pencil"),
            (1, "win.highlight"),
            (2, "win.arrow"),
            (3, "win.measure"),
            (4, "win.text"),
            (5, "win.select"),
            (6, "win.rotate-counterclockwise"),
            (7, "win.rotate-clockwise"),
            (8, "win.flip-horizontal"),
            (9, "win.flip-vertical"),
            (10, "win.scale-preview"),
        ] {
            assert_eq!(string_attribute(&edit_menu, index, "action"), action);
        }
        assert!(
            (0..edit_menu.n_items()).all(|index| edit_menu.item_link(index, "submenu").is_none())
        );
    }

    #[test]
    fn main_menu_separates_image_properties_from_preferences() {
        let menu: gio::MenuModel = main_menu().upcast();
        let string_attribute = |index, name| {
            menu.item_attribute_value(index, name, None)
                .and_then(|value| value.get::<String>())
        };
        let entries = (0..menu.n_items())
            .filter_map(|index| {
                Some((
                    string_attribute(index, "label")?,
                    string_attribute(index, "action")?,
                ))
            })
            .collect::<Vec<_>>();
        assert!(entries.contains(&("Image Properties".to_owned(), "win.properties".to_owned())));
        assert!(entries.contains(&("Preferences".to_owned(), "win.preferences".to_owned())));
    }

    #[test]
    fn main_menu_delegates_open_with_to_a_window_action() {
        let menu: gio::MenuModel = main_menu().upcast();
        let entries = (0..menu.n_items())
            .filter_map(|index| {
                let label = menu
                    .item_attribute_value(index, "label", None)?
                    .get::<String>()?;
                let action = menu
                    .item_attribute_value(index, "action", None)?
                    .get::<String>()?;
                Some((label, action))
            })
            .collect::<Vec<_>>();

        assert!(entries.contains(&("Open With…".to_owned(), "win.open-with".to_owned())));
    }

    #[test]
    fn cancelling_the_system_app_chooser_is_not_an_error() {
        for error in [
            glib::Error::new(gtk::DialogError::Cancelled, "cancelled"),
            glib::Error::new(gtk::DialogError::Dismissed, "dismissed"),
            glib::Error::new(gio::IOErrorEnum::Cancelled, "cancelled"),
        ] {
            assert!(open_with_was_cancelled(&error));
        }
        assert!(!open_with_was_cancelled(&glib::Error::new(
            gtk::DialogError::Failed,
            "failed"
        )));
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn open_with_action_uses_an_always_ask_writable_launcher() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.OpenWithTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let file = gio::File::for_path("/images/example.png");

        let launcher = open_with_launcher(&file);

        assert!(window.0.window.lookup_action("open-with").is_some());
        assert!(launcher.must_always_ask());
        assert!(launcher.is_writable());
        assert_eq!(launcher.file(), Some(file));
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
    fn corner_drag_resizes_both_region_boundaries() {
        let crop = CropOverlay {
            x: 10,
            y: 20,
            width: 50,
            height: 60,
            image_width: 100,
            image_height: 100,
        };

        let resized = resize_region(crop, 20, 30, true, false, true, false);

        assert_eq!((resized.x, resized.y), (20, 30));
        assert_eq!((resized.width, resized.height), (40, 50));
    }

    #[test]
    fn keyboard_region_marking_builds_an_inclusive_rectangle() {
        let crop = selection_overlay(SelectionDrag {
            start: (7, 5),
            current: (2, 1),
            start_screen: (0.0, 0.0),
            image_dimensions: (10, 8),
        })
        .unwrap();

        assert_eq!((crop.x, crop.y, crop.width, crop.height), (2, 1, 6, 5));
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

        let preview = crate::tools::scale::resize(
            &image,
            4,
            2,
            Resampling::Nearest,
            &CancellationToken::default(),
        )
        .unwrap();

        assert_eq!(preview.dimensions(), (4, 2));
        assert_eq!(preview.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(preview.get_pixel(3, 0).0, [0, 0, 255, 255]);
    }

    #[test]
    fn scale_dimensions_support_locked_height_and_percentage_input() {
        assert_eq!(scaled_width_for_height(800, 600, 300), 400);
        assert_eq!(scaled_dimensions(8, 6, 2), (2, 2));
        assert_eq!(dimensions_from_percent(800, 600, 50.0), (400, 300));
        assert_eq!(dimensions_from_percent(1, 1, 1.0), (1, 1));
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
    fn centered_zoom_places_the_content_midpoint_at_the_viewport_midpoint() {
        assert_eq!(centered_adjustment_value(0.0, 1_600.0, 800.0), 400.0);
        assert_eq!(centered_adjustment_value(20.0, 1_620.0, 800.0), 420.0);
        assert_eq!(centered_adjustment_value(0.0, 600.0, 800.0), 0.0);
    }

    #[test]
    fn region_selection_uses_grid_boundaries_in_both_drag_directions() {
        let forward = boundary_overlay((12, 9), (52, 39), (100, 80));
        let reverse = boundary_overlay((52, 39), (12, 9), (100, 80));

        assert_eq!(
            (
                forward.x,
                forward.y,
                forward.width,
                forward.height,
                forward.image_width,
                forward.image_height,
            ),
            (
                reverse.x,
                reverse.y,
                reverse.width,
                reverse.height,
                reverse.image_width,
                reverse.image_height,
            )
        );
        assert_eq!(
            (forward.x, forward.y, forward.width, forward.height),
            (12, 9, 40, 30)
        );
    }

    #[test]
    fn selected_region_fits_completely_and_ignores_degenerate_bounds() {
        let selection = CropOverlay {
            x: 10,
            y: 20,
            width: 200,
            height: 100,
            image_width: 1000,
            image_height: 800,
        };

        assert_eq!(zoom_rect_target((800.0, 600.0), selection), Some(4.0));
        assert_eq!(
            zoom_rect_target(
                (800.0, 600.0),
                CropOverlay {
                    width: 0,
                    ..selection
                }
            ),
            None
        );
        assert_eq!(zoom_rect_target((1.0, 600.0), selection), None);
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn annotation_palette_icons_resolve_without_installing_the_app() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.AnnotationIconTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let _window = ViewerWindow::new(&application, None);
        let display = gtk::gdk::Display::default().expect("graphical display");
        let theme = gtk::IconTheme::for_display(&display);

        for icon in [
            "pencil-symbolic",
            "highlight-symbolic",
            "arrow-symbolic",
            "ruler-measure-symbolic",
            "text-symbolic",
        ] {
            assert!(theme.has_icon(icon), "missing annotation icon {icon}");
        }
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn tool_state_clears_selection_and_eyedropper_escape_returns_to_its_tool() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.ToolStateTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let image = image::RgbaImage::from_pixel(16, 16, image::Rgba([1, 2, 3, 255]));
        let texture = texture_from_rgba(&image).expect("image texture");
        let mut document = Document::new(crate::document::ImageSource {
            pixels: Arc::new(image.clone()),
            path: None,
            metadata: crate::document::Metadata::default(),
        });
        let annotation = Annotation {
            id: document.allocate_annotation_id(),
            shape: Shape::Arrow {
                start: crate::document::Point { x: 2.0, y: 2.0 },
                end: crate::document::Point { x: 12.0, y: 12.0 },
                control: crate::document::Point { x: 7.0, y: 7.0 },
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 3.0,
                },
            },
        };
        document.apply(Operation::Annotate(AnnotationEdit::Create(
            annotation.clone(),
        )));
        window.0.canvas.set_texture(Some(&texture));
        window.0.rendered.replace(Some(image));
        window.0.document.replace(Some(document));

        window.set_tool(Tool::Arrow);
        window.select_annotation(Some(annotation.id));
        window.set_tool(Tool::None);
        assert_eq!(window.0.tool.get(), Tool::Select);
        assert_eq!(window.0.selected_annotation.get(), None);

        window.set_tool(Tool::Arrow);
        window.select_annotation(Some(annotation.id));
        window.set_tool(Tool::PickColor);
        assert_eq!(window.0.return_tool.get(), Some(Tool::Arrow));
        assert_eq!(window.0.selected_annotation.get(), Some(annotation.id));
        window.select_annotation(None);
        gio::prelude::ActionGroupExt::activate_action(&window.0.window, "cancel-tool", None);
        assert_eq!(window.0.tool.get(), Tool::Arrow);

        window.set_tool(Tool::Measure);
        assert_eq!(window.0.pencil_size.value(), 1.0);
        assert!(!window.0.pencil_size.is_sensitive());
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn select_is_the_resting_tool_and_escape_cannot_deactivate_it() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.SelectRestingToolTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let image = image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255]));
        window.0.rendered.replace(Some(image.clone()));
        window
            .0
            .document
            .replace(Some(Document::new(crate::document::ImageSource {
                pixels: Arc::new(image),
                path: None,
                metadata: crate::document::Metadata::default(),
            })));

        window.set_tool(Tool::Select);
        gio::prelude::ActionGroupExt::activate_action(&window.0.window, "cancel-tool", None);
        assert_eq!(window.0.tool.get(), Tool::Select);

        window.set_tool(Tool::Pencil);
        gio::prelude::ActionGroupExt::activate_action(&window.0.window, "cancel-tool", None);
        assert_eq!(window.0.tool.get(), Tool::Select);

        window.toggle_tool(Tool::Select);
        assert_eq!(window.0.tool.get(), Tool::Select);
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn keyboard_creation_matches_the_pencil_highlight_and_arrow_spec() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.KeyboardAnnotationTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let image = image::RgbaImage::from_pixel(100, 80, image::Rgba([1, 2, 3, 255]));
        let texture = texture_from_rgba(&image).expect("image texture");
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

        window.set_tool(Tool::Highlight);
        window.0.pencil_size.set_value(14.0);
        assert_eq!(window.0.pencil_size.value(), 1.0);
        assert!(!window.0.pencil_size.is_sensitive());
        window.0.keyboard_tool_cursor.set(Some((50, 40)));
        window.activate_keyboard_tool();
        let annotations = window.0.document.borrow().as_ref().unwrap().annotations();
        assert!(matches!(
            annotations[0].shape,
            Shape::Highlight {
                rect: crate::document::Rect {
                    width: 64.0,
                    height: 40.0,
                    ..
                },
                ..
            }
        ));
        let Shape::Highlight { style, .. } = &annotations[0].shape else {
            unreachable!()
        };
        assert_eq!(style.width, 1.0);

        window.select_annotation(None);
        window.set_tool(Tool::Arrow);
        window.0.keyboard_tool_cursor.set(Some((50, 40)));
        window.activate_keyboard_tool();
        let annotations = window.0.document.borrow().as_ref().unwrap().annotations();
        let Shape::Arrow { start, end, .. } = annotations[1].shape else {
            panic!("keyboard Arrow did not create an arrow")
        };
        assert_eq!(end.x - start.x, 80.0);
        assert_eq!(end.y, start.y);

        window.select_annotation(None);
        window.set_tool(Tool::Pencil);
        window.0.keyboard_tool_cursor.set(Some((12, 14)));
        window.activate_keyboard_tool();
        let document = window.0.document.borrow();
        let document = document.as_ref().expect("document");
        let annotations = document.annotations();
        assert!(matches!(
            &annotations[2].shape,
            Shape::Pencil {
                geometry: PencilGeometry::Freehand(points),
                ..
            } if points == &[BrushPoint {
                x: 12.5,
                y: 14.5,
                pressure: 1.0,
            }]
        ));
        assert!(
            document
                .operations()
                .iter()
                .all(|operation| matches!(operation, Operation::Annotate(_)))
        );
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn inline_text_editor_commits_renderable_text_and_delete_removes_it() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.InlineTextTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let image = image::RgbaImage::from_pixel(160, 100, image::Rgba([255, 255, 255, 255]));
        let texture = texture_from_rgba(&image).expect("image texture");
        let mut document = Document::new(crate::document::ImageSource {
            pixels: Arc::new(image.clone()),
            path: None,
            metadata: crate::document::Metadata::default(),
        });
        let id = document.allocate_annotation_id();
        window.0.canvas.set_texture(Some(&texture));
        window.0.rendered.replace(Some(image));
        window.0.document.replace(Some(document));
        window.set_tool(Tool::Text);
        application.set_accels_for_action("win.text", &["t"]);

        window.open_text_editor(None, id, Point { x: 24.5, y: 60.5 }, 0.0, String::new());
        assert!(application.accels_for_action("win.text").is_empty());
        let editor = window
            .0
            .text_editor
            .borrow()
            .as_ref()
            .expect("inline text editor")
            .widget
            .clone();
        assert_eq!(
            editor.parent(),
            Some(window.0.canvas_overlay.clone().upcast())
        );
        let sample = "AV A  VA";
        editor.set_text(sample);
        window.0.window.present();
        while glib::MainContext::default().iteration(false) {}
        let assert_caret_matches_preview = || {
            let caret_origin = editor.compute_cursor_extents(0).0.x();
            let rendered_font_size = window
                .0
                .text_editor
                .borrow()
                .as_ref()
                .expect("inline text editor")
                .font_size
                * window.0.canvas.image_scale();
            for position in 0..=sample.len() {
                let actual = editor.compute_cursor_extents(position).0.x() - caret_origin;
                let expected = crate::tools::annotation::font::text_advance(
                    &sample[..position],
                    rendered_font_size,
                );
                assert!(
                    (actual - expected).abs() <= 1.0,
                    "caret at {position} is {actual}, rendered prefix advance is {expected}"
                );
            }
        };
        assert_caret_matches_preview();

        window.0.canvas.set_zoom(0.1);
        while glib::MainContext::default().iteration(false) {}
        window.position_text_editor();
        while glib::MainContext::default().iteration(false) {}
        assert_caret_matches_preview();

        editor.set_text("tomato");
        assert_eq!(window.0.tool.get(), Tool::Text);
        editor.emit_activate();
        assert_eq!(application.accels_for_action("win.text"), ["t"]);

        let annotations = window.0.document.borrow().as_ref().unwrap().annotations();
        assert!(matches!(
            &annotations[0].shape,
            Shape::Text { text, .. } if text == "tomato"
        ));
        assert!(
            window.handle_annotation_key(gtk::gdk::Key::Delete, gtk::gdk::ModifierType::empty())
        );
        assert!(
            window
                .0
                .document
                .borrow()
                .as_ref()
                .unwrap()
                .annotations()
                .is_empty()
        );
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn nudge_coalescing_never_absorbs_a_separate_style_edit() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.NudgeHistoryTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let image = image::RgbaImage::from_pixel(32, 32, image::Rgba([1, 2, 3, 255]));
        let texture = texture_from_rgba(&image).expect("image texture");
        let mut document = Document::new(crate::document::ImageSource {
            pixels: Arc::new(image.clone()),
            path: None,
            metadata: crate::document::Metadata::default(),
        });
        let id = document.allocate_annotation_id();
        document.apply(Operation::Annotate(AnnotationEdit::Create(Annotation {
            id,
            shape: Shape::Highlight {
                rect: crate::document::Rect {
                    x: 4.0,
                    y: 4.0,
                    width: 12.0,
                    height: 8.0,
                },
                seed: 1,
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 3.0,
                },
            },
        })));
        window.0.canvas.set_texture(Some(&texture));
        window.0.rendered.replace(Some(image));
        window.0.document.replace(Some(document));
        window.set_tool(Tool::Highlight);
        window.select_annotation(Some(id));

        assert!(
            window.handle_annotation_key(gtk::gdk::Key::Right, gtk::gdk::ModifierType::empty())
        );
        assert!(
            window.handle_annotation_key(gtk::gdk::Key::Right, gtk::gdk::ModifierType::empty())
        );
        assert_eq!(
            window
                .0
                .document
                .borrow()
                .as_ref()
                .unwrap()
                .operations()
                .len(),
            2
        );

        window.update_selected_annotation_style(Some([0, 0, 255, 255]), None);
        assert_eq!(
            window
                .0
                .document
                .borrow()
                .as_ref()
                .unwrap()
                .operations()
                .len(),
            3
        );
        assert!(
            window.handle_annotation_key(gtk::gdk::Key::Right, gtk::gdk::ModifierType::empty())
        );
        assert_eq!(
            window
                .0
                .document
                .borrow()
                .as_ref()
                .unwrap()
                .operations()
                .len(),
            4
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
    fn selection_tool_shows_region_actions_and_keeps_the_selection_after_copy() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.SelectionClipboardTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255]));
        let texture = texture_from_rgba(&image).expect("image texture");
        window.0.canvas.set_texture(Some(&texture));
        window.0.rendered.replace(Some(image));
        window.set_tool(Tool::Select);
        let selection = CropOverlay {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            image_width: 2,
            image_height: 2,
        };
        window.set_region_selection(Some(selection));

        let actions = window.0.region_controls.observe_children();
        assert_eq!(window.0.tool.get(), Tool::Select);
        assert!(window.0.region_controls.property::<bool>("visible"));
        assert_eq!(actions.n_items(), 3);
        assert_eq!(
            (0..actions.n_items())
                .map(|index| {
                    actions
                        .item(index)
                        .expect("region control")
                        .downcast::<gtk::Button>()
                        .expect("region button")
                        .action_name()
                        .expect("region action")
                        .to_string()
                })
                .collect::<Vec<_>>(),
            [
                "win.selection-zoom",
                "win.selection-crop",
                "win.selection-copy"
            ]
        );

        window.copy_selected_region();
        let copied = glib::MainContext::default()
            .block_on(window.0.window.clipboard().read_texture_future())
            .expect("clipboard read")
            .expect("clipboard texture");
        assert_eq!(
            rgba_from_texture(&copied),
            Some(image::RgbaImage::from_pixel(
                1,
                1,
                image::Rgba([1, 2, 3, 255])
            ))
        );
        assert_eq!(window.0.tool.get(), Tool::Select);
        assert_eq!(window.0.region_selection.get(), Some(selection));
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn copy_image_action_places_the_complete_canvas_texture_on_the_clipboard() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.ImageClipboardTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let image = image::RgbaImage::from_fn(2, 2, |x, y| {
            image::Rgba([x as u8, y as u8, (x + y) as u8, 255])
        });
        let texture = texture_from_rgba(&image).unwrap();
        window.0.canvas.set_texture(Some(&texture));
        window.update_action_states();

        assert!(window.0.window.lookup_action("copy-image").is_some());
        gio::prelude::ActionGroupExt::activate_action(&window.0.window, "copy-image", None);
        let copied = glib::MainContext::default()
            .block_on(window.0.window.clipboard().read_texture_future())
            .expect("clipboard read")
            .expect("clipboard texture");

        assert_eq!(rgba_from_texture(&copied), Some(image));
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
    fn pencil_color_sampling_forgets_any_pending_line_origin() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.PencilSamplingStateTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        window.0.lens_active.set(true);
        window.0.pencil_line_anchor.set(Some(BrushPoint {
            x: 5.5,
            y: 7.5,
            pressure: 1.0,
        }));

        window.apply_picked_color([12, 34, 56, 255]);

        assert_eq!(window.0.pencil_line_anchor.get(), None);
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn annotation_palette_is_contextual_and_pencil_settings_define_each_stroke() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.PencilControlsTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let controls = window
            .0
            .pencil_button
            .parent()
            .expect("pencil control group");
        assert_eq!(controls.downcast_ref::<gtk::Box>().unwrap().spacing(), 0);

        assert_eq!(window.0.color_button.parent().as_ref(), Some(&controls));
        assert_eq!(
            window.0.color_picker_button.parent().as_ref(),
            Some(&controls)
        );
        assert_eq!(window.0.lens_button.parent().as_ref(), Some(&controls));
        assert_eq!(window.0.pencil_size.parent().as_ref(), Some(&controls));
        assert!(controls.has_css_class("toolbar"));
        assert!(controls.has_css_class("osd"));
        assert!(window.0.zoom_controls.has_css_class("toolbar"));
        assert!(window.0.zoom_controls.has_css_class("osd"));

        assert!(controls.ancestor(adw::HeaderBar::static_type()).is_none());
        assert!(controls.ancestor(gtk::Overlay::static_type()).is_some());
        assert!(!controls.is_visible());

        window.0.pencil_size.set_value(7.0);
        window.0.pencil_antialiasing.set(true);
        let stroke = window.pencil_stroke(
            &[BrushPoint {
                x: 0.5,
                y: 0.5,
                pressure: 1.0,
            }],
            StrokePath::Smooth,
        );
        assert_eq!(stroke.width, 7.0);
        assert!(stroke.anti_aliasing);
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn every_pencil_drag_mode_commits_an_editable_annotation_node() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.PencilCommitTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let pixels = image::RgbaImage::from_pixel(64, 64, image::Rgba([0, 0, 0, 0]));
        let texture = texture_from_rgba(&pixels).unwrap();
        window.0.canvas.set_texture(Some(&texture));
        window
            .0
            .document
            .replace(Some(Document::new(crate::document::ImageSource {
                pixels: Arc::new(pixels.clone()),
                path: None,
                metadata: crate::document::Metadata::default(),
            })));
        window.0.rendered.replace(Some(pixels));

        let start = BrushPoint {
            x: 8.5,
            y: 8.5,
            pressure: 0.5,
        };
        let end = BrushPoint {
            x: 24.5,
            y: 20.5,
            pressure: 1.0,
        };
        for mode in [
            PencilDragMode::Freehand,
            PencilDragMode::Line,
            PencilDragMode::Rectangle,
            PencilDragMode::Circle,
        ] {
            let points = if mode == PencilDragMode::Rectangle {
                crate::tools::pencil::shape_points(crate::tools::pencil::PencilShape::Rectangle {
                    start,
                    end,
                })
            } else {
                vec![start, end]
            };
            window.commit_editable_pencil_stroke(&points, mode);
        }

        let mut document = window.0.document.borrow_mut();
        let document = document.as_mut().expect("document");
        assert_eq!(document.operations().len(), 4);
        assert!(
            document.operations().iter().all(|operation| matches!(
                operation,
                Operation::Annotate(AnnotationEdit::Create(_))
            ))
        );
        let annotations = document.annotations();
        assert!(matches!(
            annotations[0].shape,
            Shape::Pencil {
                geometry: PencilGeometry::Freehand(_),
                ..
            }
        ));
        assert!(matches!(
            annotations[1].shape,
            Shape::Pencil {
                geometry: PencilGeometry::Line(_),
                ..
            }
        ));
        assert!(matches!(
            annotations[2].shape,
            Shape::Pencil {
                geometry: PencilGeometry::Rectangle(_),
                ..
            }
        ));
        assert!(matches!(
            annotations[3].shape,
            Shape::Pencil {
                geometry: PencilGeometry::Ellipse(_),
                ..
            }
        ));
        assert!(document.undo());
        assert_eq!(document.annotations().len(), 3);
        assert!(document.redo());
        assert_eq!(document.annotations().len(), 4);
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn ctrl_line_chain_is_one_annotation_with_every_vertex_handle() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.PencilLineChainTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let image = image::RgbaImage::from_pixel(64, 64, image::Rgba([0, 0, 0, 0]));
        window.0.rendered.replace(Some(image.clone()));
        window
            .0
            .document
            .replace(Some(Document::new(crate::document::ImageSource {
                pixels: Arc::new(image),
                path: None,
                metadata: crate::document::Metadata::default(),
            })));
        let point = |x, y| BrushPoint {
            x,
            y,
            pressure: 1.0,
        };

        window.commit_editable_pencil_stroke(
            &[point(4.5, 4.5), point(20.5, 12.5)],
            PencilDragMode::Line,
        );
        window.commit_editable_pencil_stroke(
            &[point(20.5, 12.5), point(36.5, 28.5)],
            PencilDragMode::Line,
        );

        let document = window.0.document.borrow();
        let annotations = document.as_ref().expect("document").annotations();
        assert_eq!(
            annotations.len(),
            1,
            "a line chain must be one editable node"
        );
        assert_eq!(
            crate::tools::annotation::hit::handles(&annotations[0]).len(),
            3,
            "every line vertex must remain repositionable"
        );
        assert!(matches!(
            &annotations[0].shape,
            Shape::Pencil {
                geometry: PencilGeometry::Line(points),
                ..
            } if points == &[
                Point { x: 4.5, y: 4.5 },
                Point { x: 20.5, y: 12.5 },
                Point { x: 36.5, y: 28.5 },
            ]
        ));
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn preferences_apply_immediately_and_image_properties_are_read_only() {
        use adw::prelude::{
            ActionRowExt as _, AdwApplicationWindowExt as _, PreferencesRowExt as _,
        };

        fn row_with_title(widget: &gtk::Widget, title: &str) -> Option<adw::PreferencesRow> {
            if let Ok(row) = widget.clone().downcast::<adw::PreferencesRow>()
                && row.title() == title
            {
                return Some(row);
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if let Some(row) = row_with_title(&current, title) {
                    return Some(row);
                }
                child = current.next_sibling();
            }
            None
        }

        fn label_with_text(widget: &gtk::Widget, text: &str) -> Option<gtk::Label> {
            if let Ok(label) = widget.clone().downcast::<gtk::Label>()
                && label.text() == text
            {
                return Some(label);
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if let Some(label) = label_with_text(&current, text) {
                    return Some(label);
                }
                child = current.next_sibling();
            }
            None
        }

        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.UnifiedPropertiesTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let pixels = image::RgbaImage::from_pixel(2, 1, image::Rgba([0, 0, 0, 255]));
        window
            .0
            .document
            .replace(Some(Document::new(crate::document::ImageSource {
                pixels: Arc::new(pixels.clone()),
                path: Some(PathBuf::from("/images/current.png")),
                metadata: crate::document::Metadata {
                    mime_type: Some("image/png".to_owned()),
                    ..crate::document::Metadata::default()
                },
            })));
        let texture = texture_from_rgba(&pixels).expect("image texture");
        window.0.canvas.set_texture(Some(&texture));
        window.0.rendered.replace(Some(pixels));
        window
            .0
            .current_file
            .replace(Some(gio::File::for_path("/images/current.png")));
        window.present();

        window.show_preferences();

        let dialog = window
            .0
            .window
            .dialogs()
            .item(0)
            .expect("preferences dialog")
            .downcast::<adw::Dialog>()
            .expect("dialog");
        let dialog_widget = dialog.clone().upcast::<gtk::Widget>();
        let anti_aliasing = row_with_title(&dialog_widget, "Anti-aliasing")
            .expect("anti-aliasing row")
            .downcast::<adw::SwitchRow>()
            .expect("anti-aliasing switch");
        assert!(row_with_title(&dialog_widget, "Hard zoom").is_some());
        assert!(row_with_title(&dialog_widget, "Copied color format").is_some());
        assert!(row_with_title(&dialog_widget, "Dimensions").is_none());
        let initial = anti_aliasing.is_active();

        anti_aliasing.set_active(!initial);
        assert_eq!(window.0.pencil_antialiasing.get(), !initial);
        dialog.close();
        while glib::MainContext::default().iteration(false) {}

        window.show_properties();
        let properties = window
            .0
            .window
            .dialogs()
            .item(0)
            .expect("image properties dialog")
            .downcast::<adw::Dialog>()
            .expect("dialog");
        let properties_widget = properties.upcast::<gtk::Widget>();
        let dimensions = row_with_title(&properties_widget, "Dimensions")
            .expect("dimensions row")
            .downcast::<adw::ActionRow>()
            .expect("dimensions action row");
        assert!(
            dimensions
                .subtitle()
                .is_none_or(|subtitle| subtitle.is_empty())
        );
        let dimensions_value = label_with_text(&dimensions.upcast::<gtk::Widget>(), "2 × 1")
            .expect("dimensions value");
        assert!(dimensions_value.has_css_class("dim-label"));
        assert_eq!(dimensions_value.halign(), gtk::Align::End);
        assert_eq!(
            dimensions_value.ellipsize(),
            gtk::pango::EllipsizeMode::Middle
        );
        assert!(dimensions_value.is_selectable());
        assert_eq!(dimensions_value.tooltip_text().as_deref(), Some("2 × 1"));
        let location = row_with_title(&properties_widget, "Location")
            .expect("location row")
            .downcast::<adw::ActionRow>()
            .expect("location action row");
        assert!(
            location
                .subtitle()
                .is_none_or(|subtitle| subtitle.is_empty())
        );
        let location_value =
            label_with_text(&location.upcast::<gtk::Widget>(), "/images/current.png")
                .expect("location value");
        assert_eq!(location_value.halign(), gtk::Align::End);
        assert_eq!(
            location_value.tooltip_text().as_deref(),
            Some("/images/current.png")
        );
        assert!(row_with_title(&properties_widget, "Format").is_some());
        assert!(row_with_title(&properties_widget, "Metadata").is_some());
        assert!(row_with_title(&properties_widget, "Hard zoom").is_none());
        window.0.pencil_antialiasing.set(initial);
        window.0.settings.set_pencil_antialiasing(initial);
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn annotation_tools_share_the_canvas_palette_and_have_window_actions() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.EditMenuTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);

        assert_eq!(
            window.0.measurement_button.parent().as_ref(),
            Some(&window.0.pencil_controls.clone().upcast::<gtk::Widget>())
        );
        assert_eq!(
            window.0.highlight_button.parent().as_ref(),
            Some(&window.0.pencil_controls.clone().upcast::<gtk::Widget>())
        );
        assert_eq!(
            window.0.arrow_button.parent().as_ref(),
            Some(&window.0.pencil_controls.clone().upcast::<gtk::Widget>())
        );
        assert_eq!(
            window.0.text_button.parent().as_ref(),
            Some(&window.0.pencil_controls.clone().upcast::<gtk::Widget>())
        );
        assert!(window.0.scale_button.parent().is_none());
        assert!(window.0.window.lookup_action("measure").is_some());
        assert!(window.0.window.lookup_action("highlight").is_some());
        assert!(window.0.window.lookup_action("arrow").is_some());
        assert!(window.0.window.lookup_action("text").is_some());
        assert!(window.0.window.lookup_action("select").is_some());
        assert!(window.0.window.lookup_action("scale-preview").is_some());
        assert!(
            window
                .0
                .lens_button
                .ancestor(adw::HeaderBar::static_type())
                .is_none()
        );
        let region_children = window.0.region_controls.observe_children();
        assert_eq!(region_children.n_items(), 3);
        assert_eq!(
            (0..region_children.n_items())
                .map(|index| {
                    region_children
                        .item(index)
                        .expect("region control")
                        .downcast::<gtk::Button>()
                        .expect("region button")
                        .action_name()
                        .expect("region button action")
                        .to_string()
                })
                .collect::<Vec<_>>(),
            [
                "win.selection-zoom",
                "win.selection-crop",
                "win.selection-copy"
            ]
        );

        let image = image::RgbaImage::from_pixel(8, 6, image::Rgba([1, 2, 3, 255]));
        let texture = texture_from_rgba(&image).unwrap();
        window.0.canvas.set_texture(Some(&texture));
        window.0.rendered.replace(Some(image));
        window.set_tool(Tool::Select);

        assert!(window.0.region_selection.get().is_none());
        assert!(window.0.region_controls.property::<bool>("visible"));
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn empty_and_loaded_states_drive_content_and_action_availability() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.ContentStateTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let enabled = |name: &str| {
            window
                .0
                .window
                .lookup_action(name)
                .expect("window action")
                .is_enabled()
        };

        assert_eq!(
            window.0.content_stack.visible_child_name().as_deref(),
            Some("empty")
        );
        assert!(enabled("open"));
        assert!(enabled("preferences"));
        assert!(!enabled("copy-image"));
        assert!(!enabled("select"));
        assert!(!enabled("properties"));

        let pixels = image::RgbaImage::from_pixel(8, 6, image::Rgba([1, 2, 3, 255]));
        let texture = texture_from_rgba(&pixels).expect("image texture");
        window.0.canvas.set_texture(Some(&texture));
        window
            .0
            .document
            .replace(Some(Document::new(crate::document::ImageSource {
                pixels: Arc::new(pixels.clone()),
                path: None,
                metadata: crate::document::Metadata::default(),
            })));
        window.0.rendered.replace(Some(pixels));
        window.0.content_stack.set_visible_child_name("viewer");
        window.update_action_states();

        assert!(enabled("copy-image"));
        assert!(enabled("select"));
        assert!(enabled("properties"));
        assert!(!enabled("save"));
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn keyboard_region_cursor_selects_source_pixels_without_pointer_input() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.KeyboardCropTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let pixels = image::RgbaImage::from_pixel(8, 6, image::Rgba([1, 2, 3, 255]));
        let texture = texture_from_rgba(&pixels).expect("image texture");
        window.0.canvas.set_texture(Some(&texture));
        window.0.rendered.replace(Some(pixels));

        window.set_tool(Tool::Select);
        assert_eq!(window.0.keyboard_tool_cursor.get(), None);
        assert!(window.move_keyboard_tool_cursor(0, 0));
        let start = window
            .0
            .keyboard_tool_cursor
            .get()
            .expect("keyboard tool cursor");
        window.activate_keyboard_tool();
        assert_eq!(window.0.keyboard_tool_anchor.get(), Some(start));
        assert!(window.move_keyboard_tool_cursor(2, 1));
        window.activate_keyboard_tool();

        let selection = window
            .0
            .region_selection
            .get()
            .expect("keyboard region selection");
        assert_eq!(selection.width, 3);
        assert_eq!(selection.height, 2);
        assert!(
            window
                .0
                .window
                .lookup_action("selection-crop")
                .expect("crop region action")
                .is_enabled()
        );
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn arrow_tool_keeps_the_selected_line_thickness() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.ArrowThicknessTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        window.0.rendered.replace(Some(image::RgbaImage::new(1, 1)));
        let original_width = window.0.settings.pencil_size();
        let selected_width = if original_width == 11 { 12.0 } else { 11.0 };
        window.set_tool(Tool::Pencil);
        window.0.pencil_size.set_value(selected_width);
        window.set_tool(Tool::Highlight);

        window.set_tool(Tool::Arrow);

        assert_eq!(window.0.pencil_size.value(), selected_width);
        assert_eq!(
            window.current_annotation_stroke_width(),
            selected_width as f32
        );
        window.0.settings.set_pencil_size(original_width);
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn scale_controls_use_the_available_overlay_width() {
        fn button_with_label(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
            if let Ok(button) = widget.clone().downcast::<gtk::Button>()
                && button.label().as_deref() == Some(label)
            {
                return Some(button);
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if let Some(button) = button_with_label(&current, label) {
                    return Some(button);
                }
                child = current.next_sibling();
            }
            None
        }

        fn button_with_action(widget: &gtk::Widget, action: &str) -> Option<gtk::Button> {
            if let Ok(button) = widget.clone().downcast::<gtk::Button>()
                && button.action_name().as_deref() == Some(action)
            {
                return Some(button);
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if let Some(button) = button_with_action(&current, action) {
                    return Some(button);
                }
                child = current.next_sibling();
            }
            None
        }

        fn css_class_count(widget: &gtk::Widget, class: &str) -> usize {
            let mut count = usize::from(widget.has_css_class(class));
            let mut child = widget.first_child();
            while let Some(current) = child {
                count += css_class_count(&current, class);
                child = current.next_sibling();
            }
            count
        }

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
        let scale_surface = window
            .0
            .scale_controls
            .first_child()
            .expect("shared scale surface")
            .downcast::<gtk::Box>()
            .expect("scale surface box");
        assert!(scale_surface.has_css_class("osd"));
        let scale_content = scale_surface
            .first_child()
            .expect("padded scale content")
            .downcast::<gtk::Box>()
            .expect("scale content box");
        for margin in [
            scale_content.margin_start(),
            scale_content.margin_end(),
            scale_content.margin_top(),
            scale_content.margin_bottom(),
        ] {
            assert_eq!(margin, 12);
        }
        for control in [
            window.0.scale_width.clone().upcast::<gtk::Widget>(),
            window.0.scale_height.clone().upcast(),
            window.0.scale_lock.clone().upcast(),
            window.0.scale_unit.clone().upcast(),
            window.0.scale_algorithm_label.clone().upcast(),
            window.0.scale_original_button.clone().upcast(),
        ] {
            assert!(
                !control
                    .parent()
                    .expect("scale control group")
                    .has_css_class("osd"),
                "{control:?} should not have its own OSD background"
            );
        }
        let scale_controls: gtk::Widget = window.0.scale_controls.clone().upcast();
        assert_eq!(css_class_count(&scale_controls, "osd"), 1);
        for action in ["win.cancel-scale", "win.confirm-scale"] {
            let button = button_with_action(&scale_controls, action).expect("scale action button");
            assert!(!button.has_css_class("osd"));
        }
        assert_eq!(
            window.0.scale_original_button.label().as_deref(),
            Some("Hold Original")
        );
        assert!(button_with_label(&scale_controls, "Actual Pixels").is_some());
        assert!(button_with_label(&scale_controls, "Fit Preview").is_some());
        assert!(window.0.scale_slider.hexpands());
        assert_eq!(window.0.scale_slider.width_request(), -1);
        let scale_slider_row = window
            .0
            .scale_slider
            .parent()
            .expect("slider surface")
            .downcast::<gtk::Box>()
            .expect("slider surface box");
        assert!(!scale_slider_row.has_css_class("osd"));
        let scale_control_row = scale_slider_row
            .parent()
            .expect("scale content")
            .first_child()
            .expect("scale control row")
            .downcast::<adw::WrapBox>()
            .expect("responsive scale control row");
        assert_eq!(scale_control_row.align(), 0.5);

        window.0.scale_controls.set_visible(true);
        window.0.canvas_overlay.allocate(1000, 600, -1, None);
        assert_eq!(window.0.scale_controls.width(), 948);
        assert!(scale_slider_row.width() >= 900);
        assert!(window.0.scale_slider.width() > 700);
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn scale_action_enables_after_the_editable_decode() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.ScaleActivationTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let image = image::RgbaImage::from_pixel(8, 6, image::Rgba([1, 2, 3, 255]));
        let texture = texture_from_rgba(&image).unwrap();
        window.0.canvas.set_texture(Some(&texture));
        window.0.editable_decode_pending.set(true);
        window.update_action_states();

        gio::prelude::ActionGroupExt::activate_action(&window.0.window, "scale-preview", None);

        assert!(!window.0.scale_button.is_active());
        assert!(!window.0.scale_controls.get_visible());

        window
            .0
            .document
            .replace(Some(Document::new(crate::document::ImageSource {
                pixels: Arc::new(image.clone()),
                path: None,
                metadata: crate::document::Metadata::default(),
            })));
        window.0.rendered.replace(Some(image));
        window.finish_editable_decode(true);
        gio::prelude::ActionGroupExt::activate_action(&window.0.window, "scale-preview", None);

        assert!(window.0.scale_button.is_active());
        assert!(!window.0.pending_scale_activation.get());
        assert!(window.0.scale_controls.get_visible());
        assert_eq!(window.0.scale_width.value(), 8.0);
        assert_eq!(window.0.scale_height.value(), 6.0);
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn fit_action_stays_locked_to_each_scale_preview_size() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.ScaleFitTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let image = image::RgbaImage::from_pixel(8, 6, image::Rgba([1, 2, 3, 255]));
        let texture = texture_from_rgba(&image).unwrap();
        window.0.canvas.set_texture(Some(&texture));
        window.0.rendered.replace(Some(image));
        window.update_action_states();
        window.0.scale_button.set_active(true);
        window.0.scrolled.allocate(640, 480, -1, None);

        gio::prelude::ActionGroupExt::activate_action(&window.0.window, "fit", None);
        assert_eq!(window.0.scale_preview_view.get(), ScalePreviewView::Fit);

        let preview = Arc::new(image::RgbaImage::from_pixel(
            4,
            3,
            image::Rgba([4, 5, 6, 255]),
        ));
        window.display_scale_preview(preview);
        let viewport = (window.0.scrolled.width(), window.0.scrolled.height());
        assert!(usable_panel_size(viewport));
        assert!(window.0.canvas.zoom() > 64.0);
        assert_eq!(
            window.0.canvas.zoom(),
            aligned_hard_zoom(
                panel_fit_zoom(viewport, (4, 3)),
                window.0.render_scale.get(),
                ZoomAlignment::Contain,
            )
        );
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn scale_controls_keep_dimensions_units_and_properties_method_in_sync() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.ScaleDraftTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let image = image::RgbaImage::from_pixel(8, 6, image::Rgba([1, 2, 3, 255]));
        let texture = texture_from_rgba(&image).unwrap();
        window.0.canvas.set_texture(Some(&texture));
        window.0.rendered.replace(Some(image));
        window.0.scale_resampling.set(Resampling::Nearest);

        window.0.scale_button.set_active(true);

        assert_eq!(window.0.scale_width.value(), 8.0);
        assert_eq!(window.0.scale_height.value(), 6.0);
        assert_eq!(
            window.0.scale_algorithm_label.label(),
            "Nearest · Properties"
        );
        assert_eq!(window.0.scale_value_label.label(), "8 × 6 → 8 × 6 (100%)");
        assert!(window.0.scale_original_button.is_sensitive());
        assert!(!window.0.scale_spinner.get_visible());

        window.0.scale_width.set_value(4.0);
        assert_eq!(window.0.scale_height.value(), 3.0);
        assert!(window.0.scale_spinner.get_visible());

        window.0.scale_unit.set_selected(1);
        window.0.scale_slider.set_value(25.0);
        assert_eq!(window.0.scale_width.value(), 2.0);
        assert_eq!(window.0.scale_height.value(), 2.0);
        assert_eq!(window.0.scale_value_label.label(), "8 × 6 → 2 × 2 (25%)");
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while window
            .0
            .scale_preview
            .borrow()
            .as_ref()
            .is_none_or(|preview| preview.dimensions() != (2, 2))
            && std::time::Instant::now() < deadline
        {
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            window
                .0
                .scale_preview
                .borrow()
                .as_ref()
                .expect("live scale preview")
                .dimensions(),
            (2, 2)
        );
        assert!(!window.0.scale_spinner.get_visible());

        window.0.scale_resampling.set(Resampling::Linear);
        window.refresh_scale_method();
        assert_eq!(
            window.0.scale_algorithm_label.label(),
            "Linear · Properties"
        );

        let preview = Arc::new(image::RgbaImage::from_pixel(
            2,
            2,
            image::Rgba([4, 5, 6, 255]),
        ));
        window.display_scale_preview(preview);
        assert_eq!(window.0.canvas.texture().unwrap().width(), 2);
        window.set_scale_original_visible(true);
        assert_eq!(window.0.canvas.texture().unwrap().width(), 8);
        window.set_scale_original_visible(false);
        assert_eq!(window.0.canvas.texture().unwrap().width(), 2);

        window.0.scale_button.set_active(false);
        assert_eq!(window.0.canvas.texture().unwrap().width(), 8);
        assert_eq!(window.0.canvas.texture().unwrap().height(), 6);
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn multiple_opened_files_seed_one_explicit_navigation_sequence() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.MultiFileOpenTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let files = ["third.png", "first.png", "second.png"].map(gio::File::for_path);
        let window = ViewerWindow::new_with_files(&application, &files);

        assert_eq!(application.windows().len(), 1);
        assert!(window.0.explicit_navigation.get());
        assert!(files_equal(
            &window.0.current_file.borrow(),
            &Some(files[0].clone())
        ));
        let sequence = window.0.sequence.borrow();
        let sequence = sequence.as_ref().expect("explicit navigation sequence");
        assert_eq!(sequence.len(), 3);
        assert_eq!(sequence.current().uri(), files[0].uri());
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn new_window_fit_waits_for_the_real_viewport_and_downscales_large_images() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.InitialFitTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        window.0.window.set_default_size(800, 600);
        let image = image::RgbaImage::from_pixel(1_600, 1_200, image::Rgba([1, 2, 3, 255]));
        let texture = texture_from_rgba(&image).unwrap();
        window.0.canvas.set_texture(Some(&texture));
        window.0.content_stack.set_visible_child_name("viewer");

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
        let requested_zoom = panel_fit_zoom(viewport, (1_600, 1_200));
        assert!(requested_zoom * window.0.render_scale.get() < 1.0);
        assert_eq!(
            window.0.canvas.zoom(),
            aligned_hard_fit_zoom(
                requested_zoom,
                window.0.render_scale.get(),
                ZoomAlignment::Contain,
            )
        );
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn hard_zoom_keeps_one_hundred_percent_at_fractional_gnome_scaling() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.PhysicalZoomTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);

        window.update_render_scale(2.0);
        window.set_zoom(1.0);

        assert_eq!(window.0.render_scale.get(), 2.0);
        assert_eq!(window.0.canvas.zoom(), 1.0);
        assert_eq!(window.0.zoom_label.label().as_deref(), Some("100%"));
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn selected_region_remains_available_after_copy_and_zoom() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.ZoomRectangleTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        let image = image::RgbaImage::from_pixel(100, 80, image::Rgba([1, 2, 3, 255]));
        let texture = texture_from_rgba(&image).unwrap();
        window.0.canvas.set_texture(Some(&texture));
        window.0.scrolled.allocate(800, 600, -1, None);
        let selection = CropOverlay {
            x: 40,
            y: 30,
            width: 20,
            height: 10,
            image_width: 100,
            image_height: 80,
        };
        let expected_zoom = zoom_rect_target(
            (
                f64::from(window.0.scrolled.width()),
                f64::from(window.0.scrolled.height()),
            ),
            selection,
        )
        .unwrap();

        let initial_zoom = window.0.canvas.zoom();
        window.set_region_selection(Some(selection));
        window.copy_current_selection_or_image_to_clipboard();
        let copied = glib::MainContext::default()
            .block_on(window.0.window.clipboard().read_texture_future())
            .expect("clipboard read")
            .expect("clipboard texture");
        assert_eq!(
            rgba_from_texture(&copied),
            Some(image::RgbaImage::from_pixel(
                20,
                10,
                image::Rgba([1, 2, 3, 255])
            ))
        );
        assert_eq!(window.0.canvas.zoom(), initial_zoom);
        assert_eq!(window.0.region_selection.get(), Some(selection));

        assert!(window.zoom_selected_region());
        assert_eq!(window.0.region_selection.get(), Some(selection));
        window.0.scrolled.allocate(800, 600, -1, None);
        let context = glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }

        assert_eq!(window.0.canvas.zoom(), expected_zoom);
        let selected = window.0.canvas.crop_display_bounds(selection).unwrap();
        let horizontal = window.0.scrolled.hadjustment();
        let vertical = window.0.scrolled.vadjustment();
        assert!(
            (horizontal.value() + horizontal.page_size() / 2.0
                - f64::from(selected.x() + selected.width() / 2.0))
            .abs()
                < 1.0
        );
        assert!(
            (vertical.value() + vertical.page_size() / 2.0
                - f64::from(selected.y() + selected.height() / 2.0))
            .abs()
                < 1.0
        );
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn zoom_to_large_selected_region_fits_and_centers_in_the_actual_viewport() {
        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik.Diorama.LargeSelectionZoomTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(gio::Cancellable::NONE)
            .expect("application registration");
        let window = ViewerWindow::new(&application, None);
        window.0.window.set_default_size(820, 620);
        let image = image::RgbaImage::new(1_600, 1_200);
        let texture = texture_from_rgba(&image).unwrap();
        window.0.canvas.set_texture(Some(&texture));
        window.0.content_stack.set_visible_child_name("viewer");
        window.present();
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while (window.0.scrolled.hadjustment().page_size() <= 1.0
            || window.0.scrolled.vadjustment().page_size() <= 1.0)
            && std::time::Instant::now() < deadline
        {
            context.iteration(false);
            std::thread::yield_now();
        }
        let selection = CropOverlay {
            x: 100,
            y: 100,
            width: 1_200,
            height: 900,
            image_width: 1_600,
            image_height: 1_200,
        };
        window.set_region_selection(Some(selection));

        assert!(window.zoom_selected_region());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while std::time::Instant::now() < deadline {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::yield_now();
        }

        let horizontal = window.0.scrolled.hadjustment();
        let vertical = window.0.scrolled.vadjustment();
        let expected_zoom = (horizontal.page_size() / f64::from(selection.width))
            .min(vertical.page_size() / f64::from(selection.height));
        assert!(expected_zoom < 1.0, "the regression requires downscaling");
        assert!(
            (window.0.canvas.zoom() - expected_zoom).abs() < 0.01,
            "applied zoom {} did not match viewport-fit zoom {expected_zoom}; page={}x{}",
            window.0.canvas.zoom(),
            horizontal.page_size(),
            vertical.page_size(),
        );
        let selected = window.0.canvas.crop_display_bounds(selection).unwrap();
        assert!(f64::from(selected.width()) <= horizontal.page_size() + 1.0);
        assert!(f64::from(selected.height()) <= vertical.page_size() + 1.0);
        let native_center = Point {
            x: selection.x as f32 + selection.width as f32 / 2.0,
            y: selection.y as f32 + selection.height as f32 / 2.0,
        };
        let canvas_center = window
            .0
            .canvas
            .widget_point_for_image(native_center)
            .expect("native selection center");
        let viewport = window.0.canvas.parent().expect("scroll viewport");
        let visible_center = window
            .0
            .canvas
            .compute_point(&viewport, &canvas_center)
            .expect("selection center in viewport coordinates");
        assert!(
            (visible_center.x() - viewport.width() as f32 / 2.0).abs() < 1.0,
            "native selection center x={} did not land at viewport center {}",
            visible_center.x(),
            viewport.width() as f32 / 2.0,
        );
        assert!(
            (visible_center.y() - viewport.height() as f32 / 2.0).abs() < 1.0,
            "native selection center y={} did not land at viewport center {}",
            visible_center.y(),
            viewport.height() as f32 / 2.0,
        );
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
        window.0.rendered.replace(Some(image.clone()));
        window
            .0
            .document
            .replace(Some(Document::new(crate::document::ImageSource {
                pixels: Arc::new(image),
                path: None,
                metadata: crate::document::Metadata::default(),
            })));
        window.update_action_states();
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
            assert!(window.0.highlight_button.is_sensitive());
            window.set_tool(Tool::Highlight);
            window.enter_compare(comparison.clone(), preview.clone());
            assert_eq!(window.0.tool.get(), Tool::Select);
            assert!(
                !window
                    .0
                    .window
                    .lookup_action("highlight")
                    .expect("highlight action")
                    .is_enabled()
            );
            assert!(!window.0.highlight_button.is_sensitive());
            assert_eq!(window.0.compare_controllers.borrow().len(), 8);
            assert_eq!(window.0.compare_adjustment_handlers.borrow().len(), 4);

            window.exit_compare();
            assert!(window.0.highlight_button.is_sensitive());

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
                window.0.region_controls.parent(),
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
            assert!(window.0.canvas.cursor().is_some());
            assert!(window.0.compare_controllers.borrow().is_empty());
            assert!(window.0.compare_adjustment_handlers.borrow().is_empty());
        }
    }
}
