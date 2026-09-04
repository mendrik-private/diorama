use gtk::prelude::*;

use super::{ViewerWindow, texture_from_owned_rgba, texture_from_rgba};
use crate::canvas::{AnnotationOverlay, SelectionHandles};
use crate::document::{
    Annotation, AnnotationEdit, AnnotationId, Axis, HIGHLIGHT_STROKE_WIDTH,
    MEASUREMENT_STROKE_WIDTH, Operation, PencilGeometry, Point, Rect, Shape, StrokeStyle,
};
use crate::tools::annotation::edit::{handle_drag, moved, rotated_text};
use crate::tools::annotation::hit::{HitKind, cursor_for_hit, hit_test};
use crate::tools::annotation::render_annotation_preview;
use crate::window::tool::Tool;

#[derive(Debug)]
pub(super) struct PreviewQueue<T> {
    pending: Option<T>,
    scheduled: bool,
}

impl<T> Default for PreviewQueue<T> {
    fn default() -> Self {
        Self {
            pending: None,
            scheduled: false,
        }
    }
}

impl<T: PartialEq> PreviewQueue<T> {
    fn push(&mut self, preview: T, displayed: Option<&T>) -> bool {
        if self.pending.as_ref() == Some(&preview) {
            return false;
        }
        if !self.scheduled && displayed == Some(&preview) {
            return false;
        }
        self.pending = Some(preview);
        if self.scheduled {
            false
        } else {
            self.scheduled = true;
            true
        }
    }

    fn take(&mut self) -> Option<T> {
        self.scheduled = false;
        self.pending.take()
    }

    fn clear_pending(&mut self) {
        self.pending = None;
    }
}

#[derive(Debug, Clone)]
pub(super) enum AnnotationDrag {
    Create {
        tool: Tool,
        id: AnnotationId,
        start: Point,
    },
    Move {
        original: Annotation,
        start: Point,
    },
    Handle {
        kind: crate::tools::annotation::hit::HandleKind,
        original: Annotation,
        start: Point,
    },
    Rotate {
        original: Annotation,
        center: Point,
        start_angle: f32,
    },
}

impl AnnotationDrag {
    fn start(&self) -> Point {
        match self {
            Self::Create { start, .. } | Self::Move { start, .. } | Self::Handle { start, .. } => {
                *start
            }
            Self::Rotate {
                center,
                start_angle,
                ..
            } => Point {
                x: center.x + start_angle.cos(),
                y: center.y + start_angle.sin(),
            },
        }
    }
}

impl ViewerWindow {
    pub(super) fn install_annotation_controls(&self) {
        for adjustment in [self.0.scrolled.hadjustment(), self.0.scrolled.vadjustment()] {
            adjustment.connect_value_changed({
                let this = self.clone();
                move |_| this.position_text_editor()
            });
        }

        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        drag.connect_drag_begin({
            let this = self.clone();
            move |gesture, x, y| {
                let tool = this.0.tool.get();
                if !tool.is_annotation() {
                    return;
                }
                if this.close_text_editor() {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    return;
                }
                let Some(point) = this.0.canvas.image_point_at(x, y) else {
                    return;
                };
                let annotations = this
                    .0
                    .document
                    .borrow()
                    .as_ref()
                    .map_or_else(Vec::new, crate::document::Document::annotations);
                let tolerance = 8.0 / this.0.canvas.image_scale().max(0.01);
                if let Some(hit) = hit_test(
                    &annotations,
                    this.0.selected_annotation.get(),
                    point,
                    tolerance,
                ) {
                    let Some(original) = annotations
                        .into_iter()
                        .find(|annotation| annotation.id == hit.id)
                    else {
                        return;
                    };
                    let measurement = matches!(&original.shape, Shape::Measurement { .. });
                    this.select_annotation(Some(original.id));
                    let state = match hit.kind {
                        HitKind::Body => AnnotationDrag::Move {
                            original,
                            start: point,
                        },
                        HitKind::Handle(kind) => AnnotationDrag::Handle {
                            kind,
                            original,
                            start: point,
                        },
                        HitKind::Rotate => {
                            let center = text_midpoint(&original);
                            AnnotationDrag::Rotate {
                                original,
                                center,
                                start_angle: (point.y - center.y).atan2(point.x - center.x),
                            }
                        }
                    };
                    this.0.annotation_drag.replace(Some(state));
                    if measurement {
                        this.show_measurement_drag_base(Some(hit.id));
                    } else {
                        this.show_render_excluding(hit.id);
                    }
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                } else if tool.is_vector_annotation() {
                    let id = {
                        let mut document = this.0.document.borrow_mut();
                        let Some(document) = document.as_mut() else {
                            return;
                        };
                        document.allocate_annotation_id()
                    };
                    this.select_annotation(None);
                    this.0.annotation_drag.replace(Some(AnnotationDrag::Create {
                        tool,
                        id,
                        start: point,
                    }));
                    if tool == Tool::Measure {
                        this.show_measurement_drag_base(None);
                    }
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                }
            }
        });
        drag.connect_drag_update({
            let this = self.clone();
            move |gesture, offset_x, offset_y| {
                let Some((origin_x, origin_y)) = gesture.start_point() else {
                    return;
                };
                let Some(pointer) = this
                    .0
                    .canvas
                    .image_point_at(origin_x + offset_x, origin_y + offset_y)
                else {
                    return;
                };
                let modifiers = gesture.current_event_state();
                let Some(state) = this.0.annotation_drag.borrow().clone() else {
                    return;
                };
                if let Some(annotation) = this.annotation_for_drag(&state, pointer, modifiers) {
                    this.queue_annotation_preview(annotation);
                }
            }
        });
        drag.connect_drag_end({
            let this = self.clone();
            move |gesture, offset_x, offset_y| {
                let Some(state) = this.0.annotation_drag.take() else {
                    return;
                };
                let pointer = gesture
                    .start_point()
                    .and_then(|(x, y)| this.0.canvas.image_point_at(x + offset_x, y + offset_y));
                this.0.annotation_preview_queue.borrow_mut().clear_pending();
                let Some(pointer) = pointer else {
                    this.discard_annotation_preview();
                    this.render_document();
                    return;
                };
                let short = {
                    let start = state.start();
                    (pointer.x - start.x).abs() < 4.0 && (pointer.y - start.y).abs() < 4.0
                };
                let final_annotation =
                    this.annotation_for_drag(&state, pointer, gesture.current_event_state());
                match state {
                    AnnotationDrag::Create {
                        tool: Tool::Text,
                        id,
                        start,
                    } => {
                        let angle = if short {
                            0.0
                        } else {
                            (pointer.y - start.y).atan2(pointer.x - start.x)
                        };
                        let anchor = Point {
                            x: start.x.floor() + 0.5,
                            y: start.y.floor() + 0.5,
                        };
                        this.discard_annotation_preview();
                        this.open_text_editor(None, id, anchor, angle, String::new());
                    }
                    AnnotationDrag::Create { .. } if short => {
                        this.discard_annotation_preview();
                        this.render_document();
                    }
                    AnnotationDrag::Create { id, .. } => {
                        let Some(annotation) = final_annotation else {
                            this.discard_annotation_preview();
                            return;
                        };
                        this.commit_annotation_preview(&annotation);
                        this.apply(Operation::Annotate(AnnotationEdit::Create(annotation)));
                        this.select_annotation(Some(id));
                    }
                    _ => {
                        let Some(annotation) = final_annotation else {
                            this.discard_annotation_preview();
                            this.render_document();
                            return;
                        };
                        let id = annotation.id;
                        this.commit_annotation_preview(&annotation);
                        this.apply(Operation::Annotate(AnnotationEdit::Set(annotation)));
                        this.select_annotation(Some(id));
                    }
                }
            }
        });
        self.0.canvas.add_controller(drag);

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion({
            let this = self.clone();
            move |_, x, y| this.update_annotation_hover(x, y)
        });
        motion.connect_leave({
            let this = self.clone();
            move |_| {
                this.0.canvas.set_measurement_cursor(None);
                if let Some(selected) = this.selected_annotation() {
                    this.0
                        .canvas
                        .set_annotation_selection(Some(SelectionHandles {
                            annotation: selected,
                            hot: None,
                        }));
                }
            }
        });
        self.0.canvas.add_controller(motion);

        let double_click = gtk::GestureClick::new();
        double_click.set_button(1);
        double_click.connect_pressed({
            let this = self.clone();
            move |gesture, presses, x, y| {
                if presses != 2 || !this.0.tool.get().is_annotation() {
                    return;
                }
                let Some(point) = this.0.canvas.image_point_at(x, y) else {
                    return;
                };
                let annotations = this
                    .0
                    .document
                    .borrow()
                    .as_ref()
                    .map_or_else(Vec::new, crate::document::Document::annotations);
                let tolerance = 8.0 / this.0.canvas.image_scale().max(0.01);
                let Some(hit) = hit_test(
                    &annotations,
                    this.0.selected_annotation.get(),
                    point,
                    tolerance,
                ) else {
                    return;
                };
                let Some(mut annotation) = annotations
                    .into_iter()
                    .find(|annotation| annotation.id == hit.id)
                else {
                    return;
                };
                if reset_arrow_control(&mut annotation, hit.kind) {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    let id = annotation.id;
                    this.apply(Operation::Annotate(AnnotationEdit::Set(annotation)));
                    this.select_annotation(Some(id));
                    return;
                }
                let Shape::Text {
                    anchor,
                    angle,
                    text,
                    ..
                } = &annotation.shape
                else {
                    return;
                };
                gesture.set_state(gtk::EventSequenceState::Claimed);
                this.select_annotation(Some(annotation.id));
                this.open_text_editor(
                    Some(annotation.clone()),
                    annotation.id,
                    *anchor,
                    *angle,
                    text.clone(),
                );
            }
        });
        self.0.canvas.add_controller(double_click);
    }

    fn annotation_for_drag(
        &self,
        state: &AnnotationDrag,
        pointer: Point,
        modifiers: gtk::gdk::ModifierType,
    ) -> Option<Annotation> {
        let color = self.0.pencil_color.get();
        let stroke_width = self.current_annotation_stroke_width();
        let text_size = self.0.settings.annotation_text_size() as f32;
        Some(match state {
            AnnotationDrag::Create { tool, id, start } => Annotation {
                id: *id,
                shape: match tool {
                    Tool::Highlight => Shape::Highlight {
                        rect: highlight_creation_rect(*start, pointer),
                        seed: id.0 ^ 0xD10A_AA73_9E37_79B9,
                        style: StrokeStyle {
                            color,
                            width: HIGHLIGHT_STROKE_WIDTH,
                        },
                    },
                    Tool::Arrow => Shape::Arrow {
                        start: *start,
                        end: pointer,
                        control: start.midpoint(pointer),
                        style: StrokeStyle {
                            color,
                            width: stroke_width,
                        },
                    },
                    Tool::Measure => {
                        let horizontal = (pointer.x - start.x).abs() >= (pointer.y - start.y).abs();
                        let (from, to, at, axis) = if horizontal {
                            (
                                start.x.round().min(pointer.x.round()),
                                start.x.round().max(pointer.x.round()),
                                start.y.round(),
                                Axis::Horizontal,
                            )
                        } else {
                            (
                                start.y.round().min(pointer.y.round()),
                                start.y.round().max(pointer.y.round()),
                                start.x.round(),
                                Axis::Vertical,
                            )
                        };
                        Shape::Measurement {
                            axis,
                            from,
                            to,
                            at,
                            style: StrokeStyle {
                                color,
                                width: MEASUREMENT_STROKE_WIDTH,
                            },
                            label_size: text_size,
                        }
                    }
                    Tool::Text => Shape::Text {
                        anchor: *start,
                        angle: (pointer.y - start.y).atan2(pointer.x - start.x),
                        font_size: text_size,
                        bend: 0.0,
                        text: "Text".to_owned(),
                        color,
                    },
                    _ => return None,
                },
            },
            AnnotationDrag::Move {
                original, start, ..
            } => moved(
                original,
                Point {
                    x: pointer.x - start.x,
                    y: pointer.y - start.y,
                },
                matches!(original.shape, Shape::Measurement { .. }),
            ),
            AnnotationDrag::Handle { original, kind, .. } => handle_drag(
                original,
                *kind,
                pointer,
                modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK),
            ),
            AnnotationDrag::Rotate {
                original,
                center,
                start_angle,
                ..
            } => rotated_text(
                original,
                (pointer.y - center.y).atan2(pointer.x - center.x) - start_angle,
                modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK),
            ),
        })
    }

    fn queue_annotation_preview(&self, annotation: Annotation) {
        let displayed = self.0.annotation_preview.borrow();
        let should_schedule = self
            .0
            .annotation_preview_queue
            .borrow_mut()
            .push(annotation, displayed.as_ref());
        drop(displayed);
        if !should_schedule {
            return;
        }
        let this = self.clone();
        self.0.window.add_tick_callback(move |_, _| {
            if let Some(annotation) = this.0.annotation_preview_queue.borrow_mut().take() {
                this.preview_annotation_now(annotation);
            }
            glib::ControlFlow::Break
        });
    }

    fn preview_annotation_now(&self, annotation: Annotation) {
        if self.0.annotation_preview.borrow().as_ref() == Some(&annotation) {
            return;
        }
        let Some(dimensions) = self
            .0
            .rendered
            .borrow()
            .as_ref()
            .map(image::GenericImageView::dimensions)
        else {
            return;
        };
        let annotations = self
            .0
            .document
            .borrow()
            .as_ref()
            .map_or_else(Vec::new, crate::document::Document::annotations);
        if let Ok(Some(overlay)) = render_annotation_preview(
            dimensions,
            &annotation,
            &annotations,
            &crate::document::CancellationToken::default(),
        ) {
            let bounds = overlay.bounds;
            let Ok(texture) = texture_from_owned_rgba(overlay.pixels) else {
                return;
            };
            self.0
                .canvas
                .set_annotation_preview(Some(AnnotationOverlay { texture, bounds }));
            self.0.annotation_preview.replace(Some(annotation.clone()));
            self.0
                .canvas
                .set_annotation_selection(Some(SelectionHandles {
                    annotation,
                    hot: None,
                }));
        }
    }

    fn discard_annotation_preview(&self) {
        self.0.annotation_preview_queue.borrow_mut().clear_pending();
        self.0.annotation_preview.take();
        self.0.canvas.set_annotation_preview(None);
    }

    pub(super) fn commit_annotation_preview(&self, annotation: &Annotation) {
        self.preview_annotation_now(annotation.clone());
        let has_exact_preview = self.0.annotation_preview.borrow().as_ref() == Some(annotation);
        self.0.annotation_preview.take();
        if has_exact_preview {
            self.0.canvas.commit_annotation_preview();
        } else {
            self.0.canvas.set_annotation_preview(None);
        }
    }

    fn show_render_excluding(&self, id: AnnotationId) {
        self.cancel_document_render();
        let rendered = self.0.document.borrow().as_ref().and_then(|document| {
            document
                .render_excluding(id, &crate::document::CancellationToken::default())
                .ok()
        });
        if let Some(rendered) = rendered
            && let Ok(texture) = texture_from_rgba(&rendered.pixels)
        {
            self.0.canvas.set_texture(Some(&texture));
            self.0.canvas.finish_annotation_render();
        }
    }

    fn show_measurement_drag_base(&self, excluded: Option<AnnotationId>) {
        self.cancel_document_render();
        let rendered = self.0.document.borrow().as_ref().and_then(|document| {
            document
                .render_measurement_drag_base(
                    excluded,
                    &crate::document::CancellationToken::default(),
                )
                .ok()
        });
        if let Some(rendered) = rendered
            && let Ok(texture) = texture_from_rgba(&rendered.pixels)
        {
            self.0.canvas.set_texture(Some(&texture));
            self.0.canvas.finish_annotation_render();
        }
    }

    pub(super) fn select_annotation(&self, id: Option<AnnotationId>) {
        self.0.nudge_annotation.set(None);
        self.0.selected_annotation.set(id);
        self.refresh_annotation_selection();
        if let Some(annotation) = self.selected_annotation() {
            let kind = match annotation.shape {
                Shape::Pencil {
                    geometry: PencilGeometry::Freehand(_),
                    ..
                } => crate::i18n::gettext("Pencil drawing"),
                Shape::Pencil {
                    geometry: PencilGeometry::Line(_),
                    ..
                } => crate::i18n::gettext("Pencil line"),
                Shape::Pencil {
                    geometry: PencilGeometry::Rectangle(_),
                    ..
                } => crate::i18n::gettext("Pencil rectangle"),
                Shape::Pencil {
                    geometry: PencilGeometry::Ellipse(_),
                    ..
                } => crate::i18n::gettext("Pencil ellipse"),
                Shape::Highlight { .. } => crate::i18n::gettext("Highlight"),
                Shape::Arrow { .. } => crate::i18n::gettext("Arrow"),
                Shape::Measurement { .. } => crate::i18n::gettext("Measurement"),
                Shape::Text { .. } => crate::i18n::gettext("Text"),
            };
            self.0.canvas.announce(
                &crate::i18n::gettext("{kind} selected").replace("{kind}", &kind),
                gtk::AccessibleAnnouncementPriority::Medium,
            );
        }
    }

    pub(super) fn refresh_annotation_selection(&self) {
        let selected = self.selected_annotation();
        if selected.is_none() {
            self.0.selected_annotation.set(None);
        }
        self.0
            .canvas
            .set_annotation_selection(selected.map(|annotation| SelectionHandles {
                annotation,
                hot: None,
            }));
    }

    fn selected_annotation(&self) -> Option<Annotation> {
        let id = self.0.selected_annotation.get()?;
        self.0
            .document
            .borrow()
            .as_ref()?
            .annotations()
            .into_iter()
            .find(|annotation| annotation.id == id)
    }

    pub(super) fn update_selected_annotation_style(
        &self,
        color: Option<[u8; 4]>,
        size: Option<f32>,
    ) {
        let Some(mut annotation) = self.selected_annotation() else {
            return;
        };
        match &mut annotation.shape {
            Shape::Pencil { style, .. } => {
                if let Some(color) = color {
                    style.color = color;
                }
                if let Some(size) = size {
                    style.width = size;
                }
            }
            Shape::Highlight { style, .. } => {
                if let Some(color) = color {
                    style.color = color;
                }
                style.width = HIGHLIGHT_STROKE_WIDTH;
            }
            Shape::Arrow { style, .. } => {
                if let Some(color) = color {
                    style.color = color;
                }
                if let Some(size) = size {
                    style.width = size;
                }
            }
            Shape::Measurement { style, .. } => {
                if let Some(color) = color {
                    style.color = color;
                }
                style.width = MEASUREMENT_STROKE_WIDTH;
            }
            Shape::Text {
                color: text_color,
                font_size,
                ..
            } => {
                if let Some(color) = color {
                    *text_color = color;
                }
                if let Some(size) = size {
                    *font_size = size;
                }
            }
        }
        self.apply(Operation::Annotate(AnnotationEdit::Set(annotation)));
    }

    fn update_annotation_hover(&self, x: f64, y: f64) {
        if !self.0.tool.get().is_annotation() {
            return;
        }
        if self.0.tool.get() == Tool::Measure {
            self.0
                .canvas
                .set_measurement_cursor(self.0.canvas.snapped_normalized_at(x, y));
        } else {
            self.0.canvas.set_measurement_cursor(None);
        }
        // The drag callback owns the preview and selection while drawing. Hit-testing the
        // unchanged document on every raw motion event only wastes the frame budget and can
        // briefly replace the live selection handles with their pre-drag geometry.
        if self.0.annotation_drag.borrow().is_some() {
            return;
        }
        let Some(point) = self.0.canvas.image_point_at(x, y) else {
            return;
        };
        let annotations = self
            .0
            .document
            .borrow()
            .as_ref()
            .map_or_else(Vec::new, crate::document::Document::annotations);
        let hit = hit_test(
            &annotations,
            self.0.selected_annotation.get(),
            point,
            8.0 / self.0.canvas.image_scale().max(0.01),
        );
        if let Some(selected) = self.selected_annotation() {
            self.0
                .canvas
                .set_annotation_selection(Some(SelectionHandles {
                    annotation: selected,
                    hot: hit.and_then(|hit| match hit.kind {
                        HitKind::Handle(kind) => Some(kind),
                        _ => None,
                    }),
                }));
        }
        let cursor = if hit.is_none() && self.0.tool.get() == Tool::Measure {
            "none"
        } else {
            cursor_for_hit(hit, self.0.annotation_drag.borrow().is_some()).name()
        };
        self.0.canvas.set_cursor_from_name(Some(cursor));
    }

    pub(super) fn annotation_hit_at(&self, x: f64, y: f64) -> bool {
        let Some(point) = self.0.canvas.image_point_at(x, y) else {
            return false;
        };
        let annotations = self
            .0
            .document
            .borrow()
            .as_ref()
            .map_or_else(Vec::new, crate::document::Document::annotations);
        hit_test(
            &annotations,
            self.0.selected_annotation.get(),
            point,
            8.0 / self.0.canvas.image_scale().max(0.01),
        )
        .is_some()
    }

    pub(super) fn cancel_annotation_drag(&self) -> bool {
        if self.0.annotation_drag.take().is_some() {
            self.discard_annotation_preview();
            self.render_document();
            true
        } else {
            false
        }
    }

    pub(super) fn close_text_editor(&self) -> bool {
        if !self.remove_text_editor(false) {
            return false;
        }
        self.render_document();
        self.0.canvas.grab_focus();
        true
    }

    pub(super) fn open_text_editor(
        &self,
        original: Option<Annotation>,
        id: AnnotationId,
        anchor: Point,
        angle: f32,
        initial_text: String,
    ) {
        self.close_text_editor();
        let editor = gtk::Text::builder()
            .placeholder_text(crate::i18n::gettext("Annotation text"))
            .max_length(256)
            .activates_default(false)
            .truncate_multiline(true)
            .css_classes(["annotation-inline-editor"])
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .build();
        editor.set_text(&initial_text);
        editor.set_tooltip_text(Some(&crate::i18n::gettext(
            "Type annotation text; press Enter to commit or Escape to cancel",
        )));
        let provider = gtk::CssProvider::new();
        provider.load_from_string(
            ".annotation-inline-editor {
                color: transparent;
                caret-color: @accent_color;
                background: transparent;
                border: none;
                outline: none;
                box-shadow: none;
                padding: 0;
            }",
        );
        gtk::style_context_add_provider_for_display(
            &editor.display(),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let editing = original.is_some();
        let base = original.unwrap_or_else(|| Annotation {
            id,
            shape: Shape::Text {
                anchor,
                angle,
                font_size: self.0.settings.annotation_text_size() as f32,
                bend: 0.0,
                text: String::new(),
                color: self.0.pencil_color.get(),
            },
        });
        if editing {
            self.show_render_excluding(id);
        }
        editor.connect_changed({
            let this = self.clone();
            let base = base.clone();
            move |editor| {
                let mut annotation = base.clone();
                if let Shape::Text { text, .. } = &mut annotation.shape {
                    *text = editor.text().chars().take(256).collect();
                }
                this.preview_annotation_now(annotation);
            }
        });
        editor.connect_activate({
            let this = self.clone();
            let base = base.clone();
            move |editor| {
                let text = editor.text();
                if text.is_empty() {
                    this.close_text_editor();
                    return;
                }
                let mut annotation = base.clone();
                if let Shape::Text { text: value, .. } = &mut annotation.shape {
                    *value = text.chars().take(256).collect();
                }
                this.preview_annotation_now(annotation.clone());
                this.remove_text_editor(true);
                this.apply(Operation::Annotate(if editing {
                    AnnotationEdit::Set(annotation.clone())
                } else {
                    AnnotationEdit::Create(annotation.clone())
                }));
                this.select_annotation(Some(annotation.id));
                this.0.canvas.grab_focus();
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let this = self.clone();
            move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    this.close_text_editor();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        });
        editor.add_controller(keys);
        let font_size = match &base.shape {
            Shape::Text { font_size, .. } => *font_size,
            _ => unreachable!("text editor base must be text"),
        };
        self.0.canvas_overlay.add_overlay(&editor);
        self.0.text_editor.replace(Some(super::InlineTextEditor {
            widget: editor.clone(),
            anchor,
            font_size,
            _accelerator_suppression: self
                .0
                .window
                .application()
                .map(|application| crate::application::suppress_accelerators(&application)),
        }));
        self.position_text_editor();
        let mut annotation = base;
        if let Shape::Text { text, .. } = &mut annotation.shape {
            *text = initial_text;
        }
        self.preview_annotation_now(annotation);
        editor.grab_focus();
    }

    fn remove_text_editor(&self, commit_preview: bool) -> bool {
        let Some(editor) = self.0.text_editor.borrow_mut().take() else {
            return false;
        };
        self.0.canvas_overlay.remove_overlay(&editor.widget);
        self.0.annotation_preview_queue.borrow_mut().clear_pending();
        self.0.annotation_preview.take();
        if commit_preview {
            self.0.canvas.commit_annotation_preview();
        } else {
            self.0.canvas.set_annotation_preview(None);
        }
        true
    }

    pub(super) fn position_text_editor(&self) {
        let Some((widget, anchor, font_size)) = self
            .0
            .text_editor
            .borrow()
            .as_ref()
            .map(|editor| (editor.widget.clone(), editor.anchor, editor.font_size))
        else {
            return;
        };
        let Some(canvas_point) = self.0.canvas.widget_point_for_image(anchor) else {
            widget.set_visible(false);
            return;
        };
        let Some(point) = self
            .0
            .canvas
            .compute_point(&self.0.canvas_overlay, &canvas_point)
        else {
            widget.set_visible(false);
            return;
        };
        let rendered_font_size = font_size * self.0.canvas.image_scale();
        widget.set_margin_start(point.x().round().max(0.0) as i32);
        widget.set_margin_top((point.y() - rendered_font_size).round().max(0.0) as i32);
        widget.set_width_request(
            (self.0.canvas_overlay.width() as f32 - point.x())
                .clamp(96.0, 640.0)
                .round() as i32,
        );
        let attributes = gtk::pango::AttrList::new();
        // The canvas renderer places raw Excalifont glyph advances. Give the
        // invisible native editor the same layout so its caret stays on the
        // visible preview, including across whitespace.
        attributes.insert(gtk::pango::AttrString::new_family("Excalifont"));
        attributes.insert(gtk::pango::AttrFontFeatures::new(
            "kern=0,liga=0,clig=0,calt=0",
        ));
        attributes.insert(gtk::pango::AttrInt::new_fallback(false));
        attributes.insert(gtk::pango::AttrSize::new_size_absolute(
            (rendered_font_size.max(f32::EPSILON) * gtk::pango::SCALE as f32).round() as i32,
        ));
        widget.set_attributes(Some(&attributes));
        widget.set_visible(true);
    }

    pub(super) fn handle_annotation_key(
        &self,
        key: gtk::gdk::Key,
        modifiers: gtk::gdk::ModifierType,
    ) -> bool {
        if matches!(
            key,
            gtk::gdk::Key::Delete | gtk::gdk::Key::KP_Delete | gtk::gdk::Key::BackSpace
        ) && let Some(id) = self.0.selected_annotation.get()
        {
            self.apply(Operation::Annotate(AnnotationEdit::Delete(id)));
            self.select_annotation(None);
            self.0.canvas.grab_focus();
            return true;
        }
        if matches!(key, gtk::gdk::Key::Delete | gtk::gdk::Key::KP_Delete)
            && self.0.tool.get() == Tool::None
        {
            gtk::prelude::WidgetExt::activate_action(&self.0.window, "delete-file", None).ok();
            return true;
        }
        let delta = match key {
            gtk::gdk::Key::Left => Some(Point { x: -1.0, y: 0.0 }),
            gtk::gdk::Key::Right => Some(Point { x: 1.0, y: 0.0 }),
            gtk::gdk::Key::Up => Some(Point { x: 0.0, y: -1.0 }),
            gtk::gdk::Key::Down => Some(Point { x: 0.0, y: 1.0 }),
            _ => None,
        };
        if let Some(mut delta) = delta
            && let Some(original) = self.selected_annotation()
        {
            if modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
                delta.x *= 10.0;
                delta.y *= 10.0;
            }
            let changed = moved(
                &original,
                delta,
                matches!(original.shape, Shape::Measurement { .. }),
            );
            let id = changed.id;
            let amended = self.0.nudge_annotation.get() == Some(id)
                && self
                    .0
                    .document
                    .borrow_mut()
                    .as_mut()
                    .is_some_and(|document| document.amend_annotation(changed.clone()));
            if amended {
                self.update_action_states();
                self.render_document();
            } else {
                self.apply(Operation::Annotate(AnnotationEdit::Set(changed)));
                self.0.nudge_annotation.set(Some(id));
            }
            return true;
        }
        if matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter)
            && let Some(annotation) = self.selected_annotation()
            && let Shape::Text {
                anchor,
                angle,
                text,
                ..
            } = &annotation.shape
        {
            self.open_text_editor(
                Some(annotation.clone()),
                annotation.id,
                *anchor,
                *angle,
                text.clone(),
            );
            return true;
        }
        false
    }
}

fn text_midpoint(annotation: &Annotation) -> Point {
    if let Shape::Text {
        anchor,
        angle,
        font_size,
        text,
        ..
    } = &annotation.shape
    {
        let advance = crate::tools::annotation::font::text_advance(text, *font_size);
        Point {
            x: anchor.x + advance * angle.cos() / 2.0,
            y: anchor.y + advance * angle.sin() / 2.0,
        }
    } else {
        Point::default()
    }
}

fn reset_arrow_control(annotation: &mut Annotation, hit: HitKind) -> bool {
    let Shape::Arrow {
        start,
        end,
        control,
        ..
    } = &mut annotation.shape
    else {
        return false;
    };
    if hit != HitKind::Handle(crate::tools::annotation::hit::HandleKind::Control) {
        return false;
    }
    *control = start.midpoint(*end);
    true
}

fn highlight_creation_rect(start: Point, pointer: Point) -> Rect {
    const MINIMUM_SIZE: f32 = 4.0;
    let endpoint = |origin: f32, value: f32| {
        if value >= origin {
            origin + (value - origin).max(MINIMUM_SIZE)
        } else {
            origin - (origin - value).max(MINIMUM_SIZE)
        }
    };
    Rect::from_points(
        start,
        Point {
            x: endpoint(start.x, pointer.x),
            y: endpoint(start.y, pointer.y),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_queue_coalesces_motion_events_and_keeps_the_latest_state() {
        let mut queue = PreviewQueue::default();

        assert!(queue.push(1, None));
        assert!(!queue.push(2, None));
        assert!(!queue.push(1, None));
        assert_eq!(queue.take(), Some(1));
        assert!(!queue.push(1, Some(&1)));
        assert!(queue.push(3, Some(&1)));
    }

    #[test]
    fn double_clicking_an_arrow_control_resets_it_to_the_midpoint() {
        let mut annotation = Annotation {
            id: AnnotationId(1),
            shape: Shape::Arrow {
                start: Point { x: 2.0, y: 4.0 },
                end: Point { x: 10.0, y: 8.0 },
                control: Point { x: 20.0, y: 20.0 },
                style: StrokeStyle {
                    color: [255, 0, 0, 255],
                    width: 3.0,
                },
            },
        };
        assert!(reset_arrow_control(
            &mut annotation,
            HitKind::Handle(crate::tools::annotation::hit::HandleKind::Control)
        ));
        let Shape::Arrow { control, .. } = annotation.shape else {
            unreachable!()
        };
        assert_eq!(control, Point { x: 6.0, y: 6.0 });
    }

    #[test]
    fn narrow_highlight_drags_still_create_a_four_pixel_minor_axis() {
        assert_eq!(
            highlight_creation_rect(Point { x: 10.0, y: 10.0 }, Point { x: 20.0, y: 11.0 }),
            Rect {
                x: 10.0,
                y: 10.0,
                width: 10.0,
                height: 4.0,
            }
        );
        assert_eq!(
            highlight_creation_rect(Point { x: 10.0, y: 10.0 }, Point { x: 9.0, y: 0.0 }),
            Rect {
                x: 6.0,
                y: 0.0,
                width: 4.0,
                height: 10.0,
            }
        );
    }
}
