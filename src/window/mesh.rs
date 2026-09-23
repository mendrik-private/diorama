use std::sync::Arc;

use gtk::{gio, glib, prelude::*};

use crate::{
    canvas::{MeshGrid, MeshOverlay},
    document::{CancellationToken, Operation, Point},
    i18n::gettext,
};

use super::{ViewerWindow, texture_from_rgba};

#[derive(Clone)]
struct Snapshot {
    source: Vec<Point>,
    target: Vec<Point>,
    warped: bool,
}

#[derive(Clone, Copy)]
struct Drag {
    handle: usize,
    remembered: bool,
}

/// A session owns its immutable base. Previews only replace the canvas texture;
/// document pixels are changed exactly once when the session is confirmed.
pub(super) struct Session {
    source_image: Arc<image::RgbaImage>,
    source: Vec<Point>,
    target: Vec<Point>,
    pub(super) warped: bool,
    pub(super) grid: Option<MeshGrid>,
    drag: Option<Drag>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    preview: Option<Arc<image::RgbaImage>>,
    cancellation: Option<CancellationToken>,
    confirm_pending: bool,
    flattened_annotations: Vec<crate::document::AnnotationId>,
}

impl Session {
    fn new(
        image: image::RgbaImage,
        flattened_annotations: Vec<crate::document::AnnotationId>,
    ) -> Self {
        Self {
            source_image: Arc::new(image),
            source: Vec::new(),
            target: Vec::new(),
            warped: false,
            grid: None,
            drag: None,
            undo: Vec::new(),
            redo: Vec::new(),
            preview: None,
            cancellation: None,
            confirm_pending: false,
            flattened_annotations,
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            source: self.source.clone(),
            target: self.target.clone(),
            warped: self.warped,
        }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.source = snapshot.source;
        self.target = snapshot.target;
        self.warped = snapshot.warped;
        self.drag = None;
        self.preview = None;
        self.confirm_pending = false;
    }

    fn remember(&mut self) {
        self.undo.push(self.snapshot());
        self.redo.clear();
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.source_image.width(), self.source_image.height())
    }

    fn can_warp(&self) -> bool {
        let (width, height) = self.dimensions();
        !self.warped
            && crate::tools::mesh::valid_deformation(width, height, &self.source, &self.source)
    }

    fn can_apply(&self) -> bool {
        let (width, height) = self.dimensions();
        self.warped
            && crate::tools::mesh::valid_deformation(width, height, &self.source, &self.target)
    }

    fn overlay(&self, grid_size: u32, grid_offset_x: i32, grid_offset_y: i32) -> MeshOverlay {
        MeshOverlay {
            grid: self.grid,
            grid_size,
            grid_offset_x,
            grid_offset_y,
            points: self.target.clone(),
            active_handle: self.drag.map(|drag| drag.handle),
        }
    }

    fn clear_nodes(&mut self) -> bool {
        if self.source.is_empty() {
            return false;
        }
        self.remember();
        if let Some(cancellation) = self.cancellation.take() {
            cancellation.cancel();
        }
        self.source.clear();
        self.target.clear();
        self.warped = false;
        self.drag = None;
        self.preview = None;
        self.confirm_pending = false;
        true
    }
}

impl ViewerWindow {
    pub(super) fn start_mesh_session(&self) -> bool {
        if !self.rendered_is_current() {
            self.0.toasts.add_toast(libadwaita::Toast::new(&gettext(
                "Wait for the image to finish rendering",
            )));
            return false;
        }
        let Some(image) = self.0.rendered.borrow().as_ref().cloned() else {
            return false;
        };
        let flattened_annotations =
            self.0
                .document
                .borrow()
                .as_ref()
                .map_or_else(Vec::new, |doc| {
                    doc.annotations()
                        .into_iter()
                        .map(|annotation| annotation.id)
                        .collect()
                });
        self.0
            .mesh
            .replace(Some(Session::new(image, flattened_annotations)));
        self.refresh_mesh_overlay();
        self.0.canvas.grab_focus();
        true
    }

    pub(super) fn finish_mesh_session(&self, restore_texture: bool) {
        let Some(mut session) = self.0.mesh.borrow_mut().take() else {
            return;
        };
        if let Some(cancellation) = session.cancellation.take() {
            cancellation.cancel();
        }
        self.invalidate_mesh_preview();
        self.0.canvas.set_mesh_overlay(None);
        self.0.mesh_controls.set_visible(false);
        self.sync_mesh_controls();
        if restore_texture {
            self.restore_rendered_canvas_texture();
        }
    }

    fn invalidate_mesh_preview(&self) {
        self.0
            .mesh_preview_generation
            .set(self.0.mesh_preview_generation.get().wrapping_add(1));
    }

    pub(super) fn refresh_mesh_overlay(&self) {
        let overlay = self.0.mesh.borrow().as_ref().map(|session| {
            session.overlay(
                self.0.settings.mesh_grid_size(),
                self.0.settings.mesh_grid_offset_x(),
                self.0.settings.mesh_grid_offset_y(),
            )
        });
        self.0.canvas.set_mesh_overlay(overlay);
        self.sync_mesh_controls();
    }

    fn sync_mesh_controls(&self) {
        let (grid, warped, can_warp, can_apply, has_nodes) =
            self.0
                .mesh
                .borrow()
                .as_ref()
                .map_or((None, false, false, false, false), |session| {
                    (
                        session.grid,
                        session.warped,
                        session.can_warp(),
                        session.can_apply(),
                        !session.source.is_empty(),
                    )
                });
        let updating_tool = self.0.updating_tool.replace(true);
        self.0
            .square_grid_button
            .set_active(grid == Some(MeshGrid::Square));
        self.0
            .dimetric_grid_button
            .set_active(grid == Some(MeshGrid::Dimetric));
        self.0
            .isometric_grid_button
            .set_active(grid == Some(MeshGrid::Isometric));
        self.0.warp_mesh_button.set_active(warped);
        self.0.warp_mesh_button.set_sensitive(can_warp);
        self.0.clear_mesh_button.set_sensitive(has_nodes);
        self.0.apply_mesh_button.set_sensitive(can_apply);
        self.0.updating_tool.set(updating_tool);
        self.set_action_enabled("clear-mesh", has_nodes);
        self.set_action_enabled("apply-mesh", can_apply);
    }

    pub(super) fn set_mesh_grid(&self, grid: MeshGrid) {
        {
            let mut sessions = self.0.mesh.borrow_mut();
            let Some(session) = sessions.as_mut() else {
                return;
            };
            session.grid = (session.grid != Some(grid)).then_some(grid);
        }
        self.refresh_mesh_overlay();
        self.0.canvas.grab_focus();
    }

    fn mesh_point_at(&self, x: f64, y: f64) -> Option<Point> {
        let point = self.0.canvas.image_point_at(x, y)?;
        let (width, height) = self.0.mesh.borrow().as_ref().map(Session::dimensions)?;
        let maximum_x = width.checked_sub(1)? as f32;
        let maximum_y = height.checked_sub(1)? as f32;
        Some(Point {
            x: point.x.round().clamp(0.0, maximum_x),
            y: point.y.round().clamp(0.0, maximum_y),
        })
    }

    pub(super) fn mesh_click(&self, x: f64, y: f64) {
        let Some(point) = self.mesh_point_at(x, y) else {
            return;
        };
        let added = {
            let mut sessions = self.0.mesh.borrow_mut();
            let Some(session) = sessions.as_mut().filter(|session| !session.warped) else {
                return;
            };
            let mut source = session.source.clone();
            source.push(point);
            let (width, height) = session.dimensions();
            if !crate::tools::mesh::valid_points(width, height, &source) {
                return;
            }
            session.remember();
            session.source = source;
            session.target.push(point);
            true
        };
        if added {
            self.refresh_mesh_overlay();
            self.update_action_states();
        }
    }

    pub(super) fn begin_mesh_warp(&self) {
        let began = {
            let mut sessions = self.0.mesh.borrow_mut();
            let Some(session) = sessions.as_mut().filter(|session| session.can_warp()) else {
                return;
            };
            session.remember();
            session.warped = true;
            true
        };
        if began {
            self.refresh_mesh_overlay();
            self.schedule_mesh_preview();
            self.update_action_states();
            self.0.canvas.grab_focus();
        }
    }

    pub(super) fn clear_mesh_nodes(&self) {
        let cleared = {
            let mut sessions = self.0.mesh.borrow_mut();
            let Some(session) = sessions.as_mut() else {
                return;
            };
            session.clear_nodes()
        };
        if cleared {
            self.invalidate_mesh_preview();
            self.restore_rendered_canvas_texture();
            self.refresh_mesh_overlay();
            self.update_action_states();
            self.0.canvas.grab_focus();
        }
    }

    pub(super) fn mesh_begin_drag(&self, x: f64, y: f64) -> bool {
        let Some(point) = self.mesh_point_at(x, y) else {
            return false;
        };
        let found = {
            let mut sessions = self.0.mesh.borrow_mut();
            let Some(session) = sessions.as_mut().filter(|session| session.warped) else {
                return false;
            };
            let radius = 10.0 / self.0.canvas.image_scale().max(0.01);
            let Some((handle, _)) = session
                .target
                .iter()
                .enumerate()
                .map(|(index, handle)| (index, handle.distance(point)))
                .filter(|(_, distance)| *distance <= radius)
                .min_by(|(_, a), (_, b)| a.total_cmp(b))
            else {
                return false;
            };
            session.drag = Some(Drag {
                handle,
                remembered: false,
            });
            true
        };
        if found {
            self.refresh_mesh_overlay();
        }
        found
    }

    pub(super) fn mesh_drag_to(&self, x: f64, y: f64) {
        let Some(point) = self.mesh_point_at(x, y) else {
            return;
        };
        let changed = {
            let mut sessions = self.0.mesh.borrow_mut();
            let Some(session) = sessions.as_mut() else {
                return;
            };
            let Some(drag) = session.drag else {
                return;
            };
            if session.target[drag.handle] == point {
                return;
            }
            let mut target = session.target.clone();
            target[drag.handle] = point;
            let (width, height) = session.dimensions();
            if !crate::tools::mesh::valid_deformation(width, height, &session.source, &target) {
                return;
            }
            if !drag.remembered {
                session.remember();
                session
                    .drag
                    .as_mut()
                    .expect("drag remains active")
                    .remembered = true;
            }
            session.target = target;
            session.confirm_pending = false;
            true
        };
        if changed {
            self.refresh_mesh_overlay();
            self.schedule_mesh_preview();
        }
    }

    pub(super) fn mesh_end_drag(&self) {
        if let Some(session) = self.0.mesh.borrow_mut().as_mut() {
            session.drag = None;
        }
        self.refresh_mesh_overlay();
        self.update_action_states();
    }

    fn schedule_mesh_preview(&self) {
        let (image, source, target, cancellation, generation) = {
            let mut sessions = self.0.mesh.borrow_mut();
            let Some(session) = sessions.as_mut().filter(|session| session.can_apply()) else {
                return;
            };
            if let Some(cancellation) = session.cancellation.take() {
                cancellation.cancel();
            }
            let cancellation = CancellationToken::default();
            session.cancellation = Some(cancellation.clone());
            session.preview = None;
            let generation = self.0.mesh_preview_generation.get().wrapping_add(1);
            self.0.mesh_preview_generation.set(generation);
            (
                session.source_image.clone(),
                session.source.clone(),
                session.target.clone(),
                cancellation,
                generation,
            )
        };
        let weak = std::rc::Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || {
                crate::tools::mesh::warp(&image, &source, &target, &cancellation)
            })
            .await;
            let Some(state) = weak.upgrade() else {
                return;
            };
            let window = ViewerWindow(state);
            let image = match result {
                Ok(Ok(image)) => image,
                Ok(Err(crate::error::AppError::Cancelled)) => return,
                Err(_) => {
                    tracing::warn!("Mesh preview worker panicked");
                    window.mesh_preview_failed(generation);
                    return;
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, "Could not preview mesh deformation");
                    window.mesh_preview_failed(generation);
                    return;
                }
            };
            let should_confirm = {
                let mut sessions = window.0.mesh.borrow_mut();
                let Some(session) = sessions.as_mut() else {
                    return;
                };
                if window.0.mesh_preview_generation.get() != generation
                    || window.0.tool.get() != super::Tool::MeshPoints
                    || !session.can_apply()
                {
                    return;
                }
                session.preview = Some(Arc::new(image.clone()));
                session.cancellation.take();
                session.confirm_pending
            };
            if let Ok(texture) = texture_from_rgba(&image) {
                window.0.canvas.set_texture(Some(&texture));
            }
            if should_confirm {
                window.confirm_mesh();
            }
        });
    }

    fn mesh_preview_failed(&self, generation: u64) {
        if self.0.mesh_preview_generation.get() != generation {
            return;
        }
        if let Some(session) = self.0.mesh.borrow_mut().as_mut() {
            session.cancellation.take();
            session.confirm_pending = false;
        }
        self.restore_rendered_canvas_texture();
        self.0.toasts.add_toast(libadwaita::Toast::new(&gettext(
            "Could not preview the mesh deformation",
        )));
        self.sync_mesh_controls();
    }

    pub(super) fn confirm_mesh(&self) {
        let operation = {
            let mut sessions = self.0.mesh.borrow_mut();
            let Some(session) = sessions.as_mut().filter(|session| session.can_apply()) else {
                return;
            };
            if session.preview.is_none() || session.cancellation.is_some() {
                session.confirm_pending = true;
                None
            } else {
                Some(Operation::SelectionEdit {
                    pixels: session.preview.clone().expect("checked preview"),
                    flattened_annotations: session.flattened_annotations.clone(),
                })
            }
        };
        let Some(operation) = operation else {
            if self
                .0
                .mesh
                .borrow()
                .as_ref()
                .is_some_and(|session| session.cancellation.is_none() && session.can_apply())
            {
                self.schedule_mesh_preview();
            }
            return;
        };
        self.set_tool(super::Tool::None);
        self.apply(operation);
    }

    fn restore_mesh_snapshot(&self, next: Snapshot) {
        let warped = {
            let mut sessions = self.0.mesh.borrow_mut();
            let Some(session) = sessions.as_mut() else {
                return;
            };
            if let Some(cancellation) = session.cancellation.take() {
                cancellation.cancel();
            }
            session.restore(next);
            session.warped
        };
        self.invalidate_mesh_preview();
        self.restore_rendered_canvas_texture();
        self.refresh_mesh_overlay();
        if warped {
            self.schedule_mesh_preview();
        }
    }

    pub(super) fn mesh_undo(&self) -> bool {
        let next = {
            let mut sessions = self.0.mesh.borrow_mut();
            let Some(session) = sessions.as_mut() else {
                return false;
            };
            let Some(previous) = session.undo.pop() else {
                return false;
            };
            session.redo.push(session.snapshot());
            previous
        };
        self.restore_mesh_snapshot(next);
        true
    }

    pub(super) fn mesh_redo(&self) -> bool {
        let next = {
            let mut sessions = self.0.mesh.borrow_mut();
            let Some(session) = sessions.as_mut() else {
                return false;
            };
            let Some(next) = session.redo.pop() else {
                return false;
            };
            session.undo.push(session.snapshot());
            next
        };
        self.restore_mesh_snapshot(next);
        true
    }

    pub(super) fn mesh_can_undo(&self) -> bool {
        self.0
            .mesh
            .borrow()
            .as_ref()
            .is_some_and(|session| !session.undo.is_empty())
    }

    pub(super) fn mesh_can_redo(&self) -> bool {
        self.0
            .mesh
            .borrow()
            .as_ref()
            .is_some_and(|session| !session.redo.is_empty())
    }

    pub(super) fn mesh_has_draft(&self) -> bool {
        self.0.mesh.borrow().as_ref().is_some_and(|session| {
            !session.source.is_empty() || !session.undo.is_empty() || !session.redo.is_empty()
        })
    }

    #[cfg(test)]
    pub(super) fn mesh_point_count(&self) -> usize {
        self.0
            .mesh
            .borrow()
            .as_ref()
            .map_or(0, |session| session.source.len())
    }

    #[cfg(test)]
    pub(super) fn mesh_is_warped(&self) -> bool {
        self.0
            .mesh
            .borrow()
            .as_ref()
            .is_some_and(|session| session.warped)
    }
}

#[cfg(test)]
mod tests {
    use super::{Point, Session};

    #[test]
    fn clear_nodes_is_undoable_and_clears_pending_confirmation() {
        let mut session = Session::new(image::RgbaImage::new(20, 20), Vec::new());
        let node = Point { x: 10.0, y: 10.0 };
        session.source.push(node);
        session.target.push(node);
        session.warped = true;
        session.confirm_pending = true;

        assert!(session.clear_nodes());
        assert!(session.source.is_empty());
        assert!(!session.warped);
        assert!(!session.confirm_pending);

        let previous = session.undo.pop().expect("clear stored a snapshot");
        session.restore(previous);
        assert_eq!(session.source, [node]);
        assert_eq!(session.target, [node]);
        assert!(session.warped);
        assert!(!session.confirm_pending);
    }
}
