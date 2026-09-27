use gtk::prelude::*;

use super::{ViewerWindow, texture_from_owned_rgba, texture_from_rgba};
use crate::canvas::{AnnotationOverlay, SelectionHandles};
use crate::document::{
    Annotation, AnnotationEdit, AnnotationId, Axis, MEASUREMENT_STROKE_WIDTH, Operation,
    PencilGeometry, Point, Rect, Shape, StrokeStyle,
};
use crate::tools::annotation::edit::{handle_drag, moved, rotated};
use crate::tools::annotation::hit::{HitKind, cursor_for_hit, handles, hit_test};
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
        pointer_offset: Point,
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

        let click = gtk::GestureClick::new();
        click.set_button(1);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        click.connect_pressed({
            let this = self.clone();
            move |_, _, _, _| this.restore_canvas_focus_from_stroke_width()
        });
        self.0.canvas.add_controller(click);

        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        drag.connect_drag_begin({
            let this = self.clone();
            move |gesture, x, y| {
                let tool = this.0.tool.get();
                if !annotation_editing_active(tool) {
                    return;
                }
                // Ctrl is the pencil line gesture. Leave node hits unclaimed so
                // the pencil controller can start or close a connected segment.
                if tool == Tool::Pencil
                    && super::pencil_drag_mode(gesture.current_event_state())
                        == super::PencilDragMode::Line
                {
                    return;
                }
                if this.close_text_editor() {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    return;
                }
                let Some(point) = this.annotation_point_at(x, y) else {
                    return;
                };
                let annotations = this
                    .0
                    .document
                    .borrow()
                    .as_ref()
                    .map_or_else(Vec::new, crate::document::Document::annotations);
                let editable = editable_annotations(&annotations, tool);
                let tolerance = 8.0 / this.0.canvas.image_scale().max(0.01);
                if let Some(hit) = hit_test(
                    &editable,
                    selected_if_editable(&editable, this.0.selected_annotation.get()),
                    point,
                    tolerance,
                ) {
                    let Some(original) = editable
                        .into_iter()
                        .find(|annotation| annotation.id == hit.id)
                    else {
                        return;
                    };
                    let measurement = matches!(&original.shape, Shape::Measurement { .. });
                    let image = matches!(&original.shape, Shape::Image { .. });
                    if image && tool == Tool::Select {
                        // A region selection and an object selection have
                        // different Delete/Copy meanings. Selecting an image
                        // therefore ends any pending cutout interaction.
                        this.clear_region_selection();
                    }
                    this.select_annotation(Some(original.id));
                    let state = match hit.kind {
                        HitKind::Body => AnnotationDrag::Move {
                            original,
                            start: point,
                        },
                        HitKind::Handle(kind) => {
                            let handle = handles(&original)
                                .into_iter()
                                .find_map(|(candidate, handle)| {
                                    (candidate == kind).then_some(handle)
                                })
                                .unwrap_or(point);
                            AnnotationDrag::Handle {
                                kind,
                                original,
                                start: point,
                                pointer_offset: Point {
                                    x: point.x - handle.x,
                                    y: point.y - handle.y,
                                },
                            }
                        }
                        HitKind::Rotate => {
                            let center = rotation_center(&original);
                            AnnotationDrag::Rotate {
                                original,
                                center,
                                start_angle: (point.y - center.y).atan2(point.x - center.x),
                            }
                        }
                    };
                    this.0.annotation_drag.replace(Some(state));
                    this.0.annotation_drag_screen_start.set(Some((x, y)));
                    if image {
                        if !this.rendered_is_current() {
                            this.cancel_annotation_drag();
                            return;
                        }
                        this.cancel_document_render();
                        // Image previews render the full scene to preserve
                        // their original stacking position.
                    } else if measurement {
                        this.show_measurement_drag_base(Some(hit.id));
                    } else {
                        this.show_render_excluding(hit.id);
                    }
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                } else if tool == Tool::Select {
                    this.select_annotation(None);
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
                    this.0.annotation_drag_screen_start.set(Some((x, y)));
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
                if !annotation_editing_active(this.0.tool.get()) {
                    return;
                }
                let Some((origin_x, origin_y)) = this
                    .0
                    .annotation_drag_screen_start
                    .get()
                    .or_else(|| gesture.start_point())
                else {
                    return;
                };
                let Some(pointer) =
                    this.annotation_point_at(origin_x + offset_x, origin_y + offset_y)
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
                if !annotation_editing_active(this.0.tool.get()) {
                    this.cancel_annotation_drag();
                    return;
                }
                let Some(state) = this.0.annotation_drag.take() else {
                    return;
                };
                let pointer = this
                    .0
                    .annotation_drag_screen_start
                    .take()
                    .or_else(|| gesture.start_point())
                    .and_then(|(x, y)| this.annotation_point_at(x + offset_x, y + offset_y));
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
                // A released gesture with no screen displacement must be a
                // true no-op. Reconstructing an affine resize or rotation at
                // zero delta can otherwise introduce tiny float drift.
                let final_annotation = if offset_x == 0.0 && offset_y == 0.0 {
                    match &state {
                        AnnotationDrag::Move { original, .. }
                        | AnnotationDrag::Handle { original, .. }
                        | AnnotationDrag::Rotate { original, .. } => Some(original.clone()),
                        AnnotationDrag::Create { .. } => {
                            this.annotation_for_drag(&state, pointer, gesture.current_event_state())
                        }
                    }
                } else {
                    this.annotation_for_drag(&state, pointer, gesture.current_event_state())
                };
                let unchanged = final_annotation
                    .as_ref()
                    .is_some_and(|annotation| match &state {
                        AnnotationDrag::Move { original, .. }
                        | AnnotationDrag::Handle { original, .. }
                        | AnnotationDrag::Rotate { original, .. } => annotation == original,
                        AnnotationDrag::Create { .. } => false,
                    });
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
                    _ if unchanged => {
                        this.discard_annotation_preview();
                        this.render_document();
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
        drag.connect_cancel({
            let this = self.clone();
            move |_, _| {
                this.cancel_annotation_drag();
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
                if presses != 2 || !annotation_editing_active(this.0.tool.get()) {
                    return;
                }
                let Some(point) = this.annotation_point_at(x, y) else {
                    return;
                };
                let annotations = this
                    .0
                    .document
                    .borrow()
                    .as_ref()
                    .map_or_else(Vec::new, crate::document::Document::annotations);
                let editable = editable_annotations(&annotations, this.0.tool.get());
                let tolerance = 8.0 / this.0.canvas.image_scale().max(0.01);
                let Some(hit) = hit_test(
                    &editable,
                    selected_if_editable(&editable, this.0.selected_annotation.get()),
                    point,
                    tolerance,
                ) else {
                    return;
                };
                let Some(mut annotation) = editable
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
                        angle: 0.0,
                        seed: id.0 ^ 0xD10A_AA73_9E37_79B9,
                        style: StrokeStyle {
                            color,
                            width: stroke_width,
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
            AnnotationDrag::Handle {
                original,
                kind,
                pointer_offset,
                ..
            } => {
                let mut changed = handle_drag(
                    original,
                    *kind,
                    Point {
                        x: pointer.x - pointer_offset.x,
                        y: pointer.y - pointer_offset.y,
                    },
                    modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK),
                );
                if changed != *original
                    && let Shape::Image { resampling, .. } = &mut changed.shape
                {
                    *resampling = self.0.settings.pasted_image_resampling();
                }
                changed
            }
            AnnotationDrag::Rotate {
                original,
                center,
                start_angle,
                ..
            } => rotated(
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
        // Images may sit below later annotations. A separate overlay would
        // always be painted on top, so preview the complete scene to retain
        // document stacking while their affine frame changes. Linked lines
        // need the same treatment because one node can update another line.
        let full_scene_preview = matches!(annotation.shape, Shape::Image { .. })
            || matches!(
                &annotation.shape,
                Shape::Pencil {
                    geometry: PencilGeometry::Line(_),
                    ..
                }
            ) && self.0.document.borrow().as_ref().is_some_and(|document| {
                document.line_links().iter().any(|link| {
                    link.first.annotation == annotation.id
                        || link.second.annotation == annotation.id
                })
            });
        if full_scene_preview && let Some(mut preview) = self.0.document.borrow().as_ref().cloned()
        {
            preview.apply(Operation::Annotate(AnnotationEdit::Set(annotation.clone())));
            if let Ok(rendered) = preview.render(&crate::document::CancellationToken::default())
                && let Ok(texture) = texture_from_rgba(&rendered.pixels)
            {
                self.0.canvas.clear_annotation_previews();
                self.0.canvas.set_texture(Some(&texture));
                self.0.annotation_preview.replace(Some(annotation.clone()));
                self.0
                    .canvas
                    .set_annotation_selection(Some(SelectionHandles {
                        annotation,
                        hot: None,
                    }));
                return;
            }
        }
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
                Shape::Image { .. } => crate::i18n::gettext("Image"),
                Shape::Pencil {
                    geometry: PencilGeometry::Freehand(_),
                    ..
                } => crate::i18n::gettext("Pencil drawing"),
                Shape::Pencil {
                    geometry: PencilGeometry::Line(_),
                    ..
                } => crate::i18n::gettext("Pencil line"),
                Shape::Pencil {
                    geometry: PencilGeometry::Rectangle(_) | PencilGeometry::RotatedRectangle(_),
                    ..
                } => crate::i18n::gettext("Pencil rectangle"),
                Shape::Pencil {
                    geometry: PencilGeometry::Ellipse(_) | PencilGeometry::RotatedEllipse(_),
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

    pub(super) fn selected_annotation(&self) -> Option<Annotation> {
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
            Shape::Image { .. } => return,
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
                if let Some(size) = size {
                    style.width = size;
                }
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
        if !annotation_editing_active(self.0.tool.get()) {
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
        let Some(point) = self.annotation_point_at(x, y) else {
            return;
        };
        let annotations = self
            .0
            .document
            .borrow()
            .as_ref()
            .map_or_else(Vec::new, crate::document::Document::annotations);
        let editable = editable_annotations(&annotations, self.0.tool.get());
        let hit = hit_test(
            &editable,
            selected_if_editable(&editable, self.0.selected_annotation.get()),
            point,
            8.0 / self.0.canvas.image_scale().max(0.01),
        );
        if self.0.tool.get() == Tool::Select && hit.is_none() {
            if let Some(selected) = self.selected_annotation() {
                self.0
                    .canvas
                    .set_annotation_selection(Some(SelectionHandles {
                        annotation: selected,
                        hot: None,
                    }));
            }
            return;
        }
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
        let Some(point) = self.annotation_point_at(x, y) else {
            return false;
        };
        let annotations = self
            .0
            .document
            .borrow()
            .as_ref()
            .map_or_else(Vec::new, crate::document::Document::annotations);
        let editable = editable_annotations(&annotations, self.0.tool.get());
        hit_test(
            &editable,
            selected_if_editable(&editable, self.0.selected_annotation.get()),
            point,
            8.0 / self.0.canvas.image_scale().max(0.01),
        )
        .is_some()
    }

    fn annotation_point_at(&self, x: f64, y: f64) -> Option<Point> {
        if self.0.tool.get() == Tool::Select {
            self.0.canvas.unclamped_image_point_at(x, y)
        } else {
            self.0.canvas.image_point_at(x, y)
        }
    }

    pub(super) fn cancel_annotation_drag(&self) -> bool {
        if self.0.annotation_drag.take().is_some() {
            self.0.annotation_drag_screen_start.set(None);
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
        // Match the canvas shaper so the caret follows the visible preview.
        attributes.insert(gtk::pango::AttrString::new_family("Excalifont"));
        attributes.insert(gtk::pango::AttrFontFeatures::new(
            crate::tools::annotation::font::FONT_FEATURES,
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
        if matches!(key, gtk::gdk::Key::Delete | gtk::gdk::Key::KP_Delete)
            && self.0.region_selection.get().is_some()
        {
            self.fill_selected_region_with_background();
            // Never fall through to deleting the source file.
            return true;
        }
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
        if matches!(key, gtk::gdk::Key::Delete | gtk::gdk::Key::KP_Delete) {
            gio::prelude::ActionGroupExt::activate_action(&self.0.window, "delete-file", None);
            return true;
        }
        let delta = match key {
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

fn annotation_editing_active(tool: Tool) -> bool {
    tool.is_annotation() || tool == Tool::Select
}

fn annotation_is_editable(annotations: &[Annotation], id: AnnotationId, tool: Tool) -> bool {
    let Some(annotation) = annotations.iter().find(|annotation| annotation.id == id) else {
        return false;
    };
    match tool {
        // The Select tool continues to own pixel-region selection. It only
        // delegates persistent clipboard images to the annotation controller.
        Tool::Select => matches!(annotation.shape, Shape::Image { .. }),
        // Drawing tools retain their existing object-editing behavior while
        // avoiding accidental image moves when a user switches back to draw.
        tool if tool.is_annotation() => !matches!(annotation.shape, Shape::Image { .. }),
        _ => false,
    }
}

fn editable_annotations(annotations: &[Annotation], tool: Tool) -> Vec<Annotation> {
    annotations
        .iter()
        .filter(|annotation| annotation_is_editable(annotations, annotation.id, tool))
        .cloned()
        .collect()
}

fn selected_if_editable(
    annotations: &[Annotation],
    selected: Option<AnnotationId>,
) -> Option<AnnotationId> {
    selected.filter(|id| annotations.iter().any(|annotation| annotation.id == *id))
}

fn rotation_center(annotation: &Annotation) -> Point {
    if let Shape::Image { corners, .. } = &annotation.shape {
        return corners[0].midpoint(corners[2]);
    }
    if let Shape::Arrow { start, end, .. } = &annotation.shape {
        return start.midpoint(*end);
    }
    if let Shape::Pencil { geometry, .. } = &annotation.shape {
        return crate::tools::annotation::pencil::geometry_bounds(geometry).center();
    }
    if let Shape::Highlight { rect, .. } = &annotation.shape {
        return rect.center();
    }
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
    let radius_x = (pointer.x - start.x).abs().max(MINIMUM_SIZE / 2.0);
    let radius_y = (pointer.y - start.y).abs().max(MINIMUM_SIZE / 2.0);
    Rect {
        x: start.x - radius_x,
        y: start.y - radius_y,
        width: radius_x * 2.0,
        height: radius_y * 2.0,
    }
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
    #[ignore = "requires a graphical display"]
    fn pasted_images_are_reselectable_transformable_and_cancel_cleanly() {
        use crate::document::{Document, ImageSource, Metadata};
        use libadwaita as adw;
        use std::{
            sync::Arc,
            time::{Duration, Instant},
        };

        fn canvas_pixels(window: &ViewerWindow) -> image::RgbaImage {
            let texture = window.0.canvas.texture().expect("canvas texture");
            let bytes = texture.save_to_png_bytes();
            image::load_from_memory(bytes.as_ref())
                .expect("decode canvas texture")
                .to_rgba8()
        }

        fn wait_for_render(window: &ViewerWindow, context: &glib::MainContext) {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !window.rendered_is_current() && Instant::now() < deadline {
                context.iteration(false);
                std::thread::yield_now();
            }
            assert!(window.rendered_is_current(), "document render completed");
        }

        fn widget(window: &ViewerWindow, point: Point) -> (f64, f64) {
            let point = window
                .0
                .canvas
                .widget_point_for_image(point)
                .expect("image point maps to the presented canvas");
            (f64::from(point.x()), f64::from(point.y()))
        }

        fn image_annotation(window: &ViewerWindow, id: AnnotationId) -> Annotation {
            window
                .0
                .document
                .borrow()
                .as_ref()
                .expect("document")
                .annotations()
                .into_iter()
                .find(|annotation| annotation.id == id)
                .expect("pasted image annotation")
        }

        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik_private.Diorama.PastedImageObjectGestureTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application.register(gio::Cancellable::NONE).unwrap();
        let window = ViewerWindow::new(&application, None);
        let context = glib::MainContext::default();
        let base = image::RgbaImage::from_pixel(64, 64, image::Rgba([245, 245, 245, 255]));
        window.0.document.replace(Some(Document::new(ImageSource {
            pixels: Arc::new(base.clone()),
            path: None,
            metadata: Metadata::default(),
        })));
        window.0.rendered.replace(Some(base.clone()));
        window
            .0
            .rendered_generation
            .set(window.0.render_generation.get());
        window
            .0
            .canvas
            .set_texture(Some(&texture_from_rgba(&base).expect("base texture")));
        window.0.content_stack.set_visible_child_name("viewer");
        window.present();
        while context.pending() {
            context.iteration(false);
        }
        window.0.canvas.allocate(64, 64, -1, None);
        window.set_tool(Tool::Select);

        window.paste_rgba_image(image::RgbaImage::from_fn(16, 12, |x, y| {
            image::Rgba([220, x as u8 * 8, y as u8 * 12, 255])
        }));
        wait_for_render(&window, &context);
        let a = window.selected_annotation().expect("first pasted image");
        let (a_id, a_pixels) = match &a.shape {
            Shape::Image { pixels, .. } => (a.id, Arc::clone(pixels)),
            _ => unreachable!(),
        };

        window.paste_rgba_image(image::RgbaImage::from_fn(16, 12, |x, _y| {
            image::Rgba([x as u8 * 10, 80, 235, 160])
        }));
        wait_for_render(&window, &context);
        let b = window.selected_annotation().expect("second pasted image");
        let b_id = b.id;
        assert_eq!(
            window
                .0
                .document
                .borrow()
                .as_ref()
                .unwrap()
                .annotations()
                .len(),
            2,
            "two independent pasted-image objects survive"
        );

        // A sits below B. Image previews must render the full document, not
        // a top-most overlay, so B remains above a proposed edit of A.
        let proposed_a = moved(&a, Point { x: 3.0, y: -2.0 }, false);
        let mut expected_document = window.0.document.borrow().as_ref().unwrap().clone();
        expected_document.apply(Operation::Annotate(AnnotationEdit::Set(proposed_a.clone())));
        let expected_preview = expected_document
            .render(&crate::document::CancellationToken::default())
            .expect("stacked preview render")
            .pixels;
        window.preview_annotation_now(proposed_a);
        assert_eq!(canvas_pixels(&window), expected_preview);
        window.discard_annotation_preview();
        window.restore_rendered_canvas_texture();

        // Move B away so all subsequent pointer gestures can target A.
        window.apply(Operation::Annotate(AnnotationEdit::Set(moved(
            &b,
            Point { x: 22.0, y: -14.0 },
            false,
        ))));
        wait_for_render(&window, &context);

        let body = widget(&window, rotation_center(&a));
        let drags: Vec<_> = (0..window.0.canvas.observe_controllers().n_items())
            .filter_map(|index| window.0.canvas.observe_controllers().item(index))
            .filter_map(|controller| controller.downcast::<gtk::GestureDrag>().ok())
            .collect();
        window.select_annotation(None);
        let annotation_drag = drags
            .into_iter()
            .find(|candidate| {
                candidate.emit_by_name::<()>("drag-begin", &[&body.0, &body.1]);
                let matched = window.0.annotation_drag.borrow().is_some();
                if matched {
                    candidate
                        .emit_by_name::<()>("cancel", &[&Option::<gtk::gdk::EventSequence>::None]);
                    wait_for_render(&window, &context);
                }
                matched
            })
            .expect("annotation drag controller identified by its real begin callback");
        window.select_annotation(None);

        // Selecting an object clears an existing pixel-region selection. A
        // click with no motion only changes selection, never history.
        window.set_region_selection(Some(crate::canvas::CropOverlay {
            x: 1,
            y: 1,
            width: 6,
            height: 5,
            image_width: 64,
            image_height: 64,
        }));
        let history_before_click = window
            .0
            .document
            .borrow()
            .as_ref()
            .unwrap()
            .operations()
            .len();
        annotation_drag.emit_by_name::<()>("drag-begin", &[&body.0, &body.1]);
        assert!(
            matches!(
                window.0.annotation_drag.borrow().as_ref(),
                Some(AnnotationDrag::Move { original, .. }) if original.id == a_id
            ),
            "body drag state: {:?}",
            window.0.annotation_drag.borrow()
        );
        assert_eq!(window.0.annotation_drag_screen_start.get(), Some(body));
        annotation_drag.emit_by_name::<()>("drag-end", &[&0.0_f64, &0.0_f64]);
        wait_for_render(&window, &context);
        assert_eq!(window.0.selected_annotation.get(), Some(a_id));
        assert_eq!(window.0.region_selection.get(), None);
        assert_eq!(
            window
                .0
                .document
                .borrow()
                .as_ref()
                .unwrap()
                .operations()
                .len(),
            history_before_click,
            "a stationary selection click is not an annotation edit"
        );

        let before_rotation = image_annotation(&window, a_id);
        let Shape::Image { corners, .. } = &before_rotation.shape else {
            unreachable!()
        };
        let center = corners[0].midpoint(corners[2]);
        let rotate_start = Point {
            x: corners[0].x - 9.0,
            y: corners[0].y - 9.0,
        };
        let vector = Point {
            x: rotate_start.x - center.x,
            y: rotate_start.y - center.y,
        };
        let rotate_end = Point {
            x: center.x - vector.y,
            y: center.y + vector.x,
        };
        let rotate_start_widget = widget(&window, rotate_start);
        let rotate_end_widget = widget(&window, rotate_end);
        annotation_drag.emit_by_name::<()>(
            "drag-begin",
            &[&rotate_start_widget.0, &rotate_start_widget.1],
        );
        assert!(matches!(
            window.0.annotation_drag.borrow().as_ref(),
            Some(AnnotationDrag::Rotate { original, .. }) if original.id == a_id
        ));
        annotation_drag.emit_by_name::<()>(
            "drag-update",
            &[
                &(rotate_end_widget.0 - rotate_start_widget.0),
                &(rotate_end_widget.1 - rotate_start_widget.1),
            ],
        );
        annotation_drag.emit_by_name::<()>(
            "drag-end",
            &[
                &(rotate_end_widget.0 - rotate_start_widget.0),
                &(rotate_end_widget.1 - rotate_start_widget.1),
            ],
        );
        wait_for_render(&window, &context);
        let rotated_a = image_annotation(&window, a_id);
        let Shape::Image {
            pixels: rotated_pixels,
            corners: rotated_corners,
            ..
        } = &rotated_a.shape
        else {
            unreachable!()
        };
        assert_ne!(
            rotated_corners, corners,
            "rotation changes the affine frame"
        );
        assert!(
            Arc::ptr_eq(rotated_pixels, &a_pixels),
            "rotation retains source pixels"
        );

        let resize_start = widget(&window, rotated_corners[2]);
        let resize_end = widget(
            &window,
            Point {
                x: rotated_corners[2].x + 5.0,
                y: rotated_corners[2].y + 3.0,
            },
        );
        annotation_drag.emit_by_name::<()>("drag-begin", &[&resize_start.0, &resize_start.1]);
        assert!(matches!(
            window.0.annotation_drag.borrow().as_ref(),
            Some(AnnotationDrag::Handle { original, .. }) if original.id == a_id
        ));
        annotation_drag.emit_by_name::<()>(
            "drag-update",
            &[
                &(resize_end.0 - resize_start.0),
                &(resize_end.1 - resize_start.1),
            ],
        );
        annotation_drag.emit_by_name::<()>(
            "drag-end",
            &[
                &(resize_end.0 - resize_start.0),
                &(resize_end.1 - resize_start.1),
            ],
        );
        wait_for_render(&window, &context);
        let resized_a = image_annotation(&window, a_id);
        let Shape::Image {
            pixels: resized_pixels,
            corners: resized_corners,
            ..
        } = &resized_a.shape
        else {
            unreachable!()
        };
        assert_ne!(
            resized_corners, rotated_corners,
            "handle drag resizes the frame"
        );
        assert!(
            Arc::ptr_eq(resized_pixels, &a_pixels),
            "resize retains source pixels"
        );

        let history_before_handle_click = window
            .0
            .document
            .borrow()
            .as_ref()
            .unwrap()
            .operations()
            .len();
        let handle = widget(&window, resized_corners[2]);
        annotation_drag.emit_by_name::<()>("drag-begin", &[&handle.0, &handle.1]);
        annotation_drag.emit_by_name::<()>("drag-end", &[&0.0_f64, &0.0_f64]);
        wait_for_render(&window, &context);
        assert_eq!(image_annotation(&window, a_id), resized_a);
        assert_eq!(
            window
                .0
                .document
                .borrow()
                .as_ref()
                .unwrap()
                .operations()
                .len(),
            history_before_handle_click,
            "a stationary off-center handle click is also a no-op"
        );

        gio::prelude::ActionGroupExt::activate_action(&window.0.window, "undo", None);
        wait_for_render(&window, &context);
        assert_eq!(image_annotation(&window, a_id), rotated_a);
        gio::prelude::ActionGroupExt::activate_action(&window.0.window, "redo", None);
        wait_for_render(&window, &context);
        assert_eq!(image_annotation(&window, a_id), resized_a);

        let history_before_cancel = window
            .0
            .document
            .borrow()
            .as_ref()
            .unwrap()
            .operations()
            .len();
        let move_start = widget(&window, rotation_center(&resized_a));
        window.select_annotation(None);
        annotation_drag.emit_by_name::<()>("drag-begin", &[&move_start.0, &move_start.1]);
        annotation_drag.emit_by_name::<()>("drag-update", &[&4.0_f64, &2.0_f64]);
        annotation_drag.emit_by_name::<()>("cancel", &[&Option::<gtk::gdk::EventSequence>::None]);
        wait_for_render(&window, &context);
        let after_cancel = image_annotation(&window, a_id);
        assert_eq!(after_cancel, resized_a, "cancel restores image geometry");
        let Shape::Image { pixels, .. } = &after_cancel.shape else {
            unreachable!()
        };
        assert!(
            Arc::ptr_eq(pixels, &a_pixels),
            "cancel retains original source pixels"
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
            history_before_cancel,
            "cancel does not write annotation history"
        );

        window.set_region_selection(Some(crate::canvas::CropOverlay {
            x: 2,
            y: 2,
            width: 4,
            height: 4,
            image_width: 64,
            image_height: 64,
        }));
        window.select_annotation(None);
        let body = widget(&window, rotation_center(&resized_a));
        annotation_drag.emit_by_name::<()>("drag-begin", &[&body.0, &body.1]);
        annotation_drag.emit_by_name::<()>("drag-end", &[&0.0_f64, &0.0_f64]);
        wait_for_render(&window, &context);
        assert_eq!(window.0.region_selection.get(), None);
        assert!(
            window.handle_annotation_key(gtk::gdk::Key::Delete, gtk::gdk::ModifierType::empty())
        );
        wait_for_render(&window, &context);
        assert!(
            window
                .0
                .document
                .borrow()
                .as_ref()
                .unwrap()
                .annotations()
                .iter()
                .all(|annotation| annotation.id != a_id),
            "Delete targets the selected image after it replaces a region selection"
        );
        assert!(
            window
                .0
                .document
                .borrow()
                .as_ref()
                .unwrap()
                .annotations()
                .iter()
                .any(|annotation| annotation.id == b_id),
            "deleting A keeps the independently pasted B object"
        );
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn ctrl_drag_from_an_existing_node_starts_a_linked_branch() {
        use crate::document::{BrushPoint, Document, ImageSource, Metadata};
        use crate::window::PencilDragMode;
        use libadwaita as adw;
        use std::sync::Arc;
        adw::init().unwrap();
        let application = adw::Application::builder()
            .application_id("io.github.mendrik_private.Diorama.CtrlNodeBranchTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application.register(gio::Cancellable::NONE).unwrap();
        let window = ViewerWindow::new(&application, None);
        let image = image::RgbaImage::from_pixel(64, 64, image::Rgba([0; 4]));
        window
            .0
            .canvas
            .set_texture(Some(&texture_from_rgba(&image).unwrap()));
        window.0.canvas.allocate(64, 64, -1, None);
        window.0.rendered.replace(Some(image.clone()));
        window.0.document.replace(Some(Document::new(ImageSource {
            pixels: Arc::new(image),
            path: None,
            metadata: Metadata::default(),
        })));
        let point = |x, y| BrushPoint {
            x,
            y,
            pressure: 1.0,
        };
        window.commit_editable_pencil_stroke(
            &[point(12.5, 12.5), point(48.5, 12.5)],
            PencilDragMode::Line,
        );
        let original = window.0.document.borrow().as_ref().unwrap().annotations()[0].clone();
        // The current endpoint and empty canvas still continue the active chain.
        for pointer in [Point { x: 48.5, y: 12.5 }, Point { x: 32.5, y: 40.5 }] {
            let screen = window.0.canvas.widget_point_for_image(pointer).unwrap();
            window.begin_pencil_drag(
                &window.0.canvas,
                1,
                screen.x().into(),
                screen.y().into(),
                gtk::gdk::ModifierType::CONTROL_MASK,
                0,
            );
            assert_eq!(
                window.0.pencil_drag.borrow().as_ref().unwrap().line_start,
                point(48.5, 12.5)
            );
            assert_eq!(window.0.pencil_line_annotation.get(), Some(original.id));
        }
        let start = window
            .0
            .canvas
            .widget_point_for_image(Point { x: 12.5, y: 12.5 })
            .unwrap();
        window.begin_pencil_drag(
            &window.0.canvas,
            1,
            start.x().into(),
            start.y().into(),
            gtk::gdk::ModifierType::CONTROL_MASK,
            0,
        );
        let drag = window.0.pencil_drag.borrow();
        assert_eq!(drag.as_ref().unwrap().line_start, point(12.5, 12.5));
        drop(drag);
        let end = window
            .0
            .canvas
            .widget_point_for_image(Point { x: 12.5, y: 48.5 })
            .unwrap();
        let (points, _, mode) = window
            .finish_pencil_drag(&window.0.canvas, end.x().into(), end.y().into(), 1)
            .unwrap();
        window.commit_editable_pencil_stroke(&points, mode);
        let document = window.0.document.borrow();
        let document = document.as_ref().unwrap();
        assert_eq!(document.annotations().len(), 2);
        assert_eq!(document.annotations()[0], original);
        assert_eq!(document.line_links().len(), 1);
        let Shape::Pencil {
            geometry: PencilGeometry::Line(vertices),
            ..
        } = &document.annotations()[1].shape
        else {
            panic!("expected branch")
        };
        assert_eq!(
            vertices,
            &[Point { x: 12.5, y: 12.5 }, Point { x: 12.5, y: 48.5 }]
        );
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
    fn arrow_rotation_center_is_its_chord_midpoint() {
        let annotation = Annotation {
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
        assert_eq!(rotation_center(&annotation), Point { x: 6.0, y: 6.0 });
    }

    #[test]
    fn highlight_drags_keep_the_press_point_at_the_center_in_every_direction() {
        let start = Point { x: 50.0, y: 60.0 };
        for dx in [-20.0, 0.0, 20.0] {
            for dy in [-10.0, 0.0, 10.0] {
                let rect = highlight_creation_rect(
                    start,
                    Point {
                        x: start.x + dx,
                        y: start.y + dy,
                    },
                );
                assert_eq!(rect.center(), start);
                assert_eq!(rect.width, if dx == 0.0 { 4.0 } else { 40.0 });
                assert_eq!(rect.height, if dy == 0.0 { 4.0 } else { 20.0 });
            }
        }
    }

    #[test]
    fn narrow_highlight_drags_still_create_a_four_pixel_minor_axis() {
        assert_eq!(
            highlight_creation_rect(Point { x: 10.0, y: 10.0 }, Point { x: 20.0, y: 11.0 }),
            Rect {
                x: 0.0,
                y: 8.0,
                width: 20.0,
                height: 4.0,
            }
        );
        assert_eq!(
            highlight_creation_rect(Point { x: 10.0, y: 10.0 }, Point { x: 9.0, y: 0.0 }),
            Rect {
                x: 8.0,
                y: 0.0,
                width: 4.0,
                height: 20.0,
            }
        );
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn linked_line_preview_returns_to_the_original_render_and_cancels_cleanly() {
        use crate::window::adw;
        use std::{
            sync::Arc,
            time::{Duration, Instant},
        };

        fn line(id: u64, points: &[(f32, f32)], color: [u8; 4]) -> Annotation {
            Annotation {
                id: AnnotationId(id),
                shape: Shape::Pencil {
                    geometry: PencilGeometry::Line(
                        points.iter().map(|&(x, y)| Point { x, y }).collect(),
                    ),
                    style: StrokeStyle { color, width: 3.0 },
                    anti_aliasing: true,
                },
            }
        }

        fn canvas_pixels(window: &ViewerWindow) -> image::RgbaImage {
            let texture = window.0.canvas.texture().expect("canvas texture");
            let bytes = texture.save_to_png_bytes();
            image::load_from_memory(bytes.as_ref())
                .expect("decode texture PNG")
                .to_rgba8()
        }

        adw::init().expect("GTK display initialization");
        let application = adw::Application::builder()
            .application_id("io.github.mendrik_private.Diorama.LinkedPreviewTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application.register(gio::Cancellable::NONE).unwrap();
        let window = ViewerWindow::new(&application, None);
        let source = image::RgbaImage::from_pixel(64, 64, image::Rgba([240, 240, 240, 255]));
        let original = line(1, &[(12.5, 20.5), (48.5, 20.5)], [220, 40, 40, 255]);
        let linked = line(2, &[(12.5, 20.5), (12.5, 50.5)], [40, 70, 220, 255]);
        let mut document = crate::document::Document::new(crate::document::ImageSource {
            pixels: Arc::new(source),
            path: None,
            metadata: crate::document::Metadata::default(),
        });
        document.apply(Operation::Annotate(AnnotationEdit::Create(
            original.clone(),
        )));
        document.apply(Operation::Annotate(AnnotationEdit::CreateLinked {
            annotation: linked,
            links: vec![crate::document::LineLink {
                first: crate::document::LineVertex {
                    annotation: AnnotationId(2),
                    index: 0,
                },
                second: crate::document::LineVertex {
                    annotation: AnnotationId(1),
                    index: 0,
                },
            }],
        }));
        let baseline = document
            .render(&crate::document::CancellationToken::default())
            .expect("baseline render")
            .pixels;
        let mut moved = original.clone();
        let Shape::Pencil {
            geometry: PencilGeometry::Line(points),
            ..
        } = &mut moved.shape
        else {
            unreachable!();
        };
        points[0] = Point { x: 30.5, y: 34.5 };
        let mut expected_document = document.clone();
        expected_document.apply(Operation::Annotate(AnnotationEdit::Set(moved.clone())));
        let expected_preview = expected_document
            .render(&crate::document::CancellationToken::default())
            .expect("linked preview render")
            .pixels;

        window.0.document.replace(Some(document));
        window.0.rendered.replace(Some(baseline.clone()));
        window
            .0
            .rendered_generation
            .set(window.0.render_generation.get());
        window.0.canvas.set_texture(Some(
            &texture_from_rgba(&baseline).expect("baseline texture"),
        ));
        window.0.canvas.allocate(64, 64, -1, None);

        window.preview_annotation_now(moved.clone());
        assert_eq!(canvas_pixels(&window), expected_preview);

        window.preview_annotation_now(original.clone());
        assert_eq!(canvas_pixels(&window), baseline);

        window.preview_annotation_now(moved);
        window.0.annotation_drag.replace(Some(AnnotationDrag::Move {
            original: original.clone(),
            start: Point { x: 12.5, y: 20.5 },
        }));
        assert!(window.cancel_annotation_drag());
        let expected_generation = window.0.render_generation.get();
        let context = glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_secs(3);
        while window.0.rendered_generation.get() != expected_generation && Instant::now() < deadline
        {
            context.iteration(false);
        }
        assert_eq!(window.0.rendered_generation.get(), expected_generation);
        assert_eq!(canvas_pixels(&window), baseline);
        assert_eq!(
            window.0.document.borrow().as_ref().unwrap().annotations(),
            vec![
                original,
                line(2, &[(12.5, 20.5), (12.5, 50.5)], [40, 70, 220, 255])
            ]
        );
    }
}
