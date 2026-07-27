//! The editor application: layout, input routing, and the glue between the
//! document, the viewport, and the panels.

use std::time::{Duration, Instant};

use egui::{Color32, Rect, Sense, Stroke};
use engine_core::formatter::SetComponentField;
use engine_core::scene::RenderItem;
use glam::{Mat4, Vec2, Vec3};
use serde_json::Value;

use crate::camera::OrbitCamera;
use crate::doc::SceneDoc;
use crate::gizmo::{self, Drag};
use crate::inspector::{self, InspectorState};
use crate::pick;
use crate::viewport::{grid_items, ViewportRenderer};
use crate::EditorOptions;

pub struct EditorApp {
    options: EditorOptions,
    doc: SceneDoc,
    camera: OrbitCamera,
    viewport: Option<ViewportRenderer>,
    grid: Vec<RenderItem>,
    schema: Value,
    selected: Option<String>,
    filter: String,
    inspector_state: InspectorState,
    drag: Option<Drag>,
    hot_axis: Option<usize>,
    started: Instant,
    screenshot_sent: bool,
}

impl EditorApp {
    pub fn new(options: EditorOptions) -> Self {
        let doc = SceneDoc::open(options.scene.clone());
        Self {
            options,
            doc,
            camera: OrbitCamera::default(),
            viewport: None,
            grid: grid_items(),
            schema: engine_core::schema::component_schema(),
            selected: None,
            filter: String::new(),
            inspector_state: InspectorState::default(),
            drag: None,
            hot_axis: None,
            started: Instant::now(),
            screenshot_sent: false,
        }
    }

    fn read_only(&self) -> bool {
        self.options.watch_only
    }

    /// `Transform.position` of an entity as the file states it (absent
    /// position on a present Transform is the documented [0,0,0]).
    fn file_position(&self, entity: &str) -> Option<Vec3> {
        let raw = self.doc.raw.as_ref()?;
        let components = raw["entities"]
            .as_array()?
            .iter()
            .find(|e| e["name"] == entity)?["components"]
            .as_array()?;
        let transform = components.iter().find(|c| c["type"] == "Transform")?;
        let p = &transform["position"];
        Some(match p.as_array() {
            Some(a) if a.len() == 3 => Vec3::new(
                a[0].as_f64().unwrap_or(0.0) as f32,
                a[1].as_f64().unwrap_or(0.0) as f32,
                a[2].as_f64().unwrap_or(0.0) as f32,
            ),
            _ => Vec3::ZERO,
        })
    }

    fn commit(&mut self, edit: SetComponentField) {
        if self.read_only() {
            return;
        }
        self.doc.apply(&edit);
        self.inspector_state.clear();
    }

    /// The draw list for this frame: scene items (with gesture preview and
    /// selection highlight) plus the grid overlay.
    fn frame_items(&self) -> Vec<RenderItem> {
        let mut items: Vec<RenderItem> = self.doc.items.clone();

        if let Some(drag) = &self.drag {
            for item in items.iter_mut().filter(|i| i.entity == drag.entity) {
                item.model = Mat4::from_translation(drag.delta) * item.model;
            }
        }

        if let Some(selected) = &self.selected {
            for item in items.iter_mut().filter(|i| &i.entity == selected) {
                item.material.emissive =
                    (item.material.emissive + Vec3::new(0.22, 0.19, 0.04)).min(Vec3::ONE);
            }
        }

        items.extend(self.grid.iter().cloned());
        items
    }

    fn viewport_ui(&mut self, ui: &mut egui::Ui, render_state: &egui_wgpu::RenderState) {
        let size = ui.available_size();
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());

        // ── Camera input ─────────────────────────────────────────────
        let modifiers = ui.input(|i| i.modifiers);
        if response.dragged_by(egui::PointerButton::Secondary) {
            let d = response.drag_delta();
            if modifiers.shift {
                self.camera.pan(Vec2::new(d.x, d.y));
            } else {
                self.camera.orbit(Vec2::new(d.x, d.y));
            }
        }
        if response.dragged_by(egui::PointerButton::Middle) {
            let d = response.drag_delta();
            self.camera.pan(Vec2::new(d.x, d.y));
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.camera.zoom(scroll * 8.0);
            }
        }

        // All interaction math runs in egui points; the texture itself is
        // rendered at physical resolution.
        let logical = Vec2::new(rect.width(), rect.height());
        let view_projection = engine_render::scene_renderer::view_projection(
            &self.camera.component(),
            self.camera.model(),
            logical.x / logical.y.max(1.0),
        );

        let pointer = response
            .hover_pos()
            .or_else(|| response.interact_pointer_pos())
            .map(|p| Vec2::new(p.x - rect.min.x, p.y - rect.min.y));

        // ── Gizmo interaction (before picking, so a grab wins) ───────
        let gizmo_origin = self
            .selected
            .as_ref()
            .and_then(|s| self.file_position(s))
            .map(|p| match &self.drag {
                Some(drag) => drag.preview_position(),
                None => p,
            });

        self.hot_axis = None;
        if let (Some(origin), Some(pointer), false) =
            (gizmo_origin, pointer, self.read_only())
        {
            let arm = gizmo::arm_length(origin, self.camera.eye());

            if let Some(drag) = &mut self.drag {
                let axis = gizmo::AXES[drag.axis].0;
                let base = drag.start_position;
                let t = gizmo::axis_parameter(view_projection, logical, base, axis, pointer);
                drag.delta = axis * (t - drag.grab_t);
                self.hot_axis = Some(drag.axis);

                let released = ui.input(|i| !i.pointer.primary_down());
                if released {
                    let drag = self.drag.take().expect("checked above");
                    let position = drag.preview_position();
                    let rounded = |v: f32| (v * 1000.0).round() / 1000.0;
                    self.commit(SetComponentField {
                        entity: drag.entity,
                        component: "Transform".into(),
                        field: "position".into(),
                        value: Value::Array(
                            [position.x, position.y, position.z]
                                .into_iter()
                                .map(|v| inspector::number_from_f32(rounded(v)))
                                .collect(),
                        ),
                    });
                }
            } else {
                self.hot_axis =
                    gizmo::hit_axis(view_projection, logical, origin, arm, pointer);
                let grab = self.hot_axis.is_some()
                    && response.drag_started_by(egui::PointerButton::Primary);
                if grab {
                    let axis_index = self.hot_axis.expect("checked above");
                    let axis = gizmo::AXES[axis_index].0;
                    self.drag = Some(Drag {
                        entity: self.selected.clone().expect("gizmo needs a selection"),
                        axis: axis_index,
                        grab_t: gizmo::axis_parameter(
                            view_projection,
                            logical,
                            origin,
                            axis,
                            pointer,
                        ),
                        start_position: origin,
                        delta: Vec3::ZERO,
                    });
                }
            }
        } else {
            self.drag = None;
        }

        // ── Click-to-select (when the gizmo did not take the click) ──
        if response.clicked() && self.drag.is_none() && self.hot_axis.is_none() {
            if let Some(pointer) = pointer {
                let (origin, direction) = pick::ray_through(view_projection, logical, pointer);
                self.selected = pick::pick(&self.doc.items, origin, direction)
                    .map(str::to_string);
            }
        }

        // ── Render the scene into the offscreen texture ──────────────
        let items = self.frame_items();
        let camera_eye = self.camera.eye();
        let lights = self.doc.lights;
        let viewport = self
            .viewport
            .get_or_insert_with(|| ViewportRenderer::new(render_state));
        let ppp = ui.ctx().pixels_per_point();
        let texture_id = viewport.paint(
            render_state,
            (logical.x * ppp) as u32,
            (logical.y * ppp) as u32,
            &items,
            view_projection,
            camera_eye,
            lights,
        );
        ui.painter().image(
            texture_id,
            rect,
            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );

        // ── Agent-verification capture (`--self-screenshot`) ─────────
        // Reads back the viewport's own texture rather than the window
        // surface: the window path depends on OS visibility and, on the
        // pinned egui commit, never delivers the capture on macOS. The
        // viewport texture is the part that answers "did the scene update"
        // and it is entirely ours.
        if let Some(path) = self.options.screenshot.clone() {
            if !self.screenshot_sent
                && self.started.elapsed()
                    >= Duration::from_millis(self.options.screenshot_after_ms)
            {
                self.screenshot_sent = true;
                let saved = viewport.read_back(render_state).and_then(
                    |(width, height, pixels)| {
                        image::RgbaImage::from_raw(width, height, pixels)
                            .map(|i| i.save(&path))
                    },
                );
                match saved {
                    Some(Ok(())) => {
                        eprintln!("[self-screenshot] wrote {}", path.display());
                        std::process::exit(0);
                    }
                    _ => {
                        eprintln!(
                            "{}",
                            engine_core::EngineError::new(
                                engine_core::codes::PNG_WRITE_FAILED,
                                format!(
                                    "could not write screenshot to {}",
                                    path.display()
                                ),
                            )
                            .to_json()
                        );
                        std::process::exit(2);
                    }
                }
            }
        }

        // ── Gizmo overlay drawing ────────────────────────────────────
        if let Some(origin) = gizmo_origin {
            if !self.read_only() {
                let arm = gizmo::arm_length(origin, self.camera.eye());
                let painter = ui.painter_at(rect);
                let to_screen =
                    |p: Vec2| egui::pos2(rect.min.x + p.x, rect.min.y + p.y);
                if let Some(root) = gizmo::project(view_projection, logical, origin) {
                    for (i, (axis, color)) in gizmo::AXES.iter().enumerate() {
                        let Some(tip) =
                            gizmo::project(view_projection, logical, origin + *axis * arm)
                        else {
                            continue;
                        };
                        let hot = self.hot_axis == Some(i);
                        let stroke = Stroke::new(
                            if hot { 4.0 } else { 2.5 },
                            if hot { Color32::WHITE } else { *color },
                        );
                        painter.line_segment([to_screen(root), to_screen(tip)], stroke);
                        painter.circle_filled(
                            to_screen(tip),
                            if hot { 6.0 } else { 4.5 },
                            stroke.color,
                        );
                    }
                }
            }
        }

        // Empty-scene / invalid-scene message over the viewport.
        if self.doc.items.is_empty() {
            let message = if self.doc.is_valid() {
                "scene has no meshes to draw"
            } else {
                "scene does not validate — see the validation panel"
            };
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                message,
                egui::FontId::proportional(16.0),
                Color32::from_gray(200),
            );
        }
    }

    fn hierarchy_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("filter:");
            ui.text_edit_singleline(&mut self.filter);
        });
        ui.separator();

        let names: Vec<String> = self
            .doc
            .raw
            .as_ref()
            .and_then(|r| r["entities"].as_array())
            .map(|entities| {
                entities
                    .iter()
                    .filter_map(|e| e["name"].as_str())
                    .filter(|n| {
                        self.filter.is_empty()
                            || n.to_lowercase().contains(&self.filter.to_lowercase())
                    })
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        egui::ScrollArea::vertical().show(ui, |ui| {
            for name in names {
                let selected = self.selected.as_deref() == Some(name.as_str());
                if ui.selectable_label(selected, &name).clicked() {
                    self.selected = if selected { None } else { Some(name) };
                }
            }
        });
    }

    fn validation_ui(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
            for diagnostic in &self.doc.diagnostics {
                let warning = diagnostic.is_warning();
                let color = if warning {
                    Color32::from_rgb(220, 180, 60)
                } else {
                    Color32::from_rgb(235, 90, 90)
                };
                let context = diagnostic.context();
                let line = context
                    .and_then(|c| c.line)
                    .map(|l| format!("line {l}"))
                    .unwrap_or_default();
                let entity = context.and_then(|c| c.entity.clone());

                let text = format!(
                    "[{}] {} — {}",
                    diagnostic.error, line, diagnostic.message
                );
                let response = ui.colored_label(color, text);
                if let Some(entity) = entity {
                    // Click-to-navigate: an error that names an entity
                    // selects it.
                    if response.interact(Sense::click()).clicked() {
                        self.selected = Some(entity);
                    }
                }
            }
        });
    }

    fn status_ui(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(&self.doc.display);
            if let Some(age) = self.doc.reload_age() {
                ui.separator();
                ui.label(format!("reloaded {age:.1}s ago"));
            }
            if self.read_only() {
                ui.separator();
                ui.colored_label(Color32::from_rgb(220, 180, 60), "READ-ONLY (--watch)");
            }
            if let Some(notice) = &self.doc.notice {
                ui.separator();
                ui.label(notice);
            }
        });
    }
}

impl eframe::App for EditorApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.doc.poll() {
            // External edit won; stale buffers must not fight it.
            self.inspector_state.clear();
        }
        // Keep polling and keep the reload age fresh.
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let render_state = frame
            .wgpu_render_state()
            .expect("the editor requires the wgpu backend")
            .clone();

        egui::Panel::bottom("status").show(ui, |ui| self.status_ui(ui));

        if !self.doc.diagnostics.is_empty() {
            egui::Panel::bottom("validation")
                .resizable(true)
                .show(ui, |ui| {
                    ui.strong(format!(
                        "validation — {} diagnostic(s)",
                        self.doc.diagnostics.len()
                    ));
                    self.validation_ui(ui);
                });
        }

        egui::Panel::left("hierarchy")
            .default_size(200.0)
            .show(ui, |ui| {
                ui.heading("entities");
                self.hierarchy_ui(ui);
            });

        egui::Panel::right("inspector")
            .default_size(300.0)
            .show(ui, |ui| {
                ui.heading("inspector");
                match self.selected.clone() {
                    Some(entity) => {
                        let raw = self.doc.raw.clone();
                        let read_only = self.read_only();
                        if let Some(raw) = raw {
                            let commits = inspector::ui(
                                ui,
                                &mut self.inspector_state,
                                &self.schema,
                                &raw,
                                &entity,
                                read_only,
                            );
                            for edit in commits {
                                self.commit(edit);
                            }
                        } else {
                            ui.label("the file does not parse; fix it in a text editor");
                        }
                    }
                    None => {
                        ui.label("select an entity in the hierarchy or the viewport");
                    }
                }
            });

        egui::CentralPanel::default().show(ui, |ui| {
            self.viewport_ui(ui, &render_state);
        });
    }
}
