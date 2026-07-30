//! The editor application: layout, input routing, and the glue between the
//! document, the viewport, and the panels.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use egui::{Color32, Rect, Sense, Stroke};
use engine_core::components::Transform;
use engine_core::formatter::SetComponentField;
use engine_core::scene::RenderItem;
use engine_core::EngineError;
use glam::{Vec2, Vec3};
use serde_json::{json, Value};

use crate::camera::OrbitCamera;
use crate::doc::SceneDoc;
use crate::gizmo::{self, Drag, GizmoMode};
use crate::import::{self, ImportedAsset};
use crate::inspector::{self, InspectorState};
use crate::pick;
use crate::viewport::{grid_items, ViewportRenderer};
use crate::EditorOptions;

/// A drag-and-drop import running on its worker thread.
struct PendingImport {
    /// The dropped file's name, for the status bar.
    label: String,
    receiver: mpsc::Receiver<Result<ImportedAsset, EngineError>>,
}

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
    mode: GizmoMode,
    drag: Option<Drag>,
    hot_axis: Option<usize>,
    imports: Vec<PendingImport>,
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
            mode: GizmoMode::Translate,
            drag: None,
            hot_axis: None,
            imports: Vec::new(),
            started: Instant::now(),
            screenshot_sent: false,
        }
    }

    fn read_only(&self) -> bool {
        self.options.watch_only
    }

    /// The entity's `Transform` as the file states it (fields absent on a
    /// present Transform take their documented identity defaults).
    fn file_transform(&self, entity: &str) -> Option<Transform> {
        let raw = self.doc.raw.as_ref()?;
        let components = raw["entities"]
            .as_array()?
            .iter()
            .find(|e| e["name"] == entity)?["components"]
            .as_array()?;
        let transform = components.iter().find(|c| c["type"] == "Transform")?;
        let vec3 = |value: &Value, default: Vec3| match value.as_array() {
            Some(a) if a.len() == 3 => Vec3::new(
                a[0].as_f64().unwrap_or(f64::from(default.x)) as f32,
                a[1].as_f64().unwrap_or(f64::from(default.y)) as f32,
                a[2].as_f64().unwrap_or(f64::from(default.z)) as f32,
            ),
            _ => default,
        };
        Some(Transform {
            position: vec3(&transform["position"], Vec3::ZERO),
            rotation: vec3(&transform["rotation"], Vec3::ZERO),
            scale: vec3(&transform["scale"], Vec3::ONE),
        })
    }

    fn commit(&mut self, edit: SetComponentField) {
        if self.read_only() {
            return;
        }
        self.doc.apply(&edit);
        self.inspector_state.clear();
    }

    /// Commit a structure edit (add/remove component) through the doc,
    /// dropping stale staged widget state like any other commit.
    fn commit_structure(&mut self, edit: impl FnOnce(&mut crate::doc::SceneDoc)) {
        if self.read_only() {
            return;
        }
        edit(&mut self.doc);
        self.inspector_state.clear();
    }

    /// Splice a new entity carrying a `builtin:` primitive at the origin —
    /// the same Transform + Mesh shape a drag-and-drop import writes, named
    /// after the primitive and deduplicated by the document.
    fn add_primitive(&mut self, primitive: &str) {
        let added = self.doc.add_entity(
            primitive,
            vec![
                (
                    "Transform".into(),
                    vec![("position".into(), json!([0.0, 0.0, 0.0]))],
                ),
                (
                    "Mesh".into(),
                    vec![(
                        "asset".into(),
                        Value::String(format!("builtin:{primitive}")),
                    )],
                ),
            ],
        );
        if added.is_some() {
            self.selected = added;
        }
    }

    /// Route dropped files into import workers and harvest finished ones
    /// into entity splices. Conversion (a Blender run for `.blend`) happens
    /// off-thread; the file write stays on the UI thread with the other
    /// commits.
    fn handle_drops(&mut self, ui: &egui::Ui) {
        let dropped = ui.input(|i| i.raw.dropped_files.clone());
        for file in dropped {
            let Some(path) = file.path else { continue };
            if self.read_only() {
                self.doc.notice = Some("read-only (--watch): drop ignored".into());
                continue;
            }
            if !import::supported(&path) {
                self.doc.notice = Some(format!(
                    "cannot import {} — drop a .blend, .gltf, or .glb file",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ));
                continue;
            }
            let label = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            let scene = self.doc.path.clone();
            let (sender, receiver) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = sender.send(import::import(&scene, &path));
            });
            self.imports.push(PendingImport { label, receiver });
        }

        let mut finished = Vec::new();
        self.imports.retain(|pending| match pending.receiver.try_recv() {
            Ok(result) => {
                finished.push((pending.label.clone(), result));
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => {
                finished.push((
                    pending.label.clone(),
                    Err(EngineError::new(
                        engine_core::codes::IMPORT_FAILED,
                        "the import worker died",
                    )),
                ));
                false
            }
        });

        for (label, result) in finished {
            match result {
                Ok(asset) => {
                    let added = self.doc.add_entity(
                        &asset.base_name,
                        vec![
                            (
                                "Transform".into(),
                                vec![("position".into(), json!([0.0, 0.0, 0.0]))],
                            ),
                            (
                                "Mesh".into(),
                                vec![("asset".into(), Value::String(asset.asset))],
                            ),
                        ],
                    );
                    if added.is_some() {
                        self.selected = added;
                    }
                }
                Err(e) => {
                    self.doc.notice =
                        Some(format!("import of {label} failed — {}", e.message));
                }
            }
        }
    }

    /// The draw list for this frame: scene items (with gesture preview and
    /// selection highlight) plus the grid overlay.
    fn frame_items(&self) -> Vec<RenderItem> {
        let mut items: Vec<RenderItem> = self.doc.items.clone();

        if let Some(drag) = &self.drag {
            // Same construction as `Scene::render_items`, so the preview is
            // exactly what committing the drag would render.
            let model = drag.preview_transform().matrix();
            for item in items.iter_mut().filter(|i| i.entity == drag.entity) {
                item.model = model;
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

        // ── Gizmo mode keys (W move / R rotate / S scale) ────────────
        // Skipped while a text field owns the keyboard or a drag is live.
        if !ui.ctx().egui_wants_keyboard_input() && self.drag.is_none() {
            ui.input(|i| {
                if i.key_pressed(egui::Key::W) {
                    self.mode = GizmoMode::Translate;
                }
                if i.key_pressed(egui::Key::R) {
                    self.mode = GizmoMode::Rotate;
                }
                if i.key_pressed(egui::Key::S) {
                    self.mode = GizmoMode::Scale;
                }
            });
        }

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
        let file_transform = self
            .selected
            .as_ref()
            .and_then(|s| self.file_transform(s));
        let gizmo_origin = file_transform.map(|t| match &self.drag {
            Some(drag) => drag.preview_transform().position,
            None => t.position,
        });

        self.hot_axis = None;
        if let (Some(start), Some(origin), Some(pointer), false) =
            (file_transform, gizmo_origin, pointer, self.read_only())
        {
            let arm = gizmo::arm_length(origin, self.camera.eye());

            if let Some(drag) = &mut self.drag {
                let axis = gizmo::AXES[drag.axis].0;
                let pivot = drag.start.position;
                match drag.mode {
                    GizmoMode::Rotate => {
                        if let Some(angle) = gizmo::ring_angle(
                            view_projection,
                            logical,
                            pivot,
                            axis,
                            pointer,
                        ) {
                            drag.delta = gizmo::angle_delta(angle, drag.grab_t);
                        }
                    }
                    GizmoMode::Translate | GizmoMode::Scale => {
                        let t = gizmo::axis_parameter(
                            view_projection,
                            logical,
                            pivot,
                            axis,
                            pointer,
                        );
                        drag.delta = t - drag.grab_t;
                    }
                }
                self.hot_axis = Some(drag.axis);

                let released = ui.input(|i| !i.pointer.primary_down());
                if released {
                    let drag = self.drag.take().expect("checked above");
                    let value = drag.value();
                    let rounded = |v: f32| (v * 1000.0).round() / 1000.0;
                    self.commit(SetComponentField {
                        entity: drag.entity,
                        component: "Transform".into(),
                        field: drag.mode.field().into(),
                        value: Value::Array(
                            [value.x, value.y, value.z]
                                .into_iter()
                                .map(|v| inspector::number_from_f32(rounded(v)))
                                .collect(),
                        ),
                    });
                }
            } else {
                self.hot_axis = match self.mode {
                    GizmoMode::Rotate => {
                        gizmo::hit_ring(view_projection, logical, origin, arm, pointer)
                    }
                    GizmoMode::Translate | GizmoMode::Scale => {
                        gizmo::hit_axis(view_projection, logical, origin, arm, pointer)
                    }
                };
                let grab = self.hot_axis.is_some()
                    && response.drag_started_by(egui::PointerButton::Primary);
                if grab {
                    let axis_index = self.hot_axis.expect("checked above");
                    let axis = gizmo::AXES[axis_index].0;
                    let grab_t = match self.mode {
                        GizmoMode::Rotate => gizmo::ring_angle(
                            view_projection,
                            logical,
                            origin,
                            axis,
                            pointer,
                        )
                        .unwrap_or(0.0),
                        GizmoMode::Translate | GizmoMode::Scale => gizmo::axis_parameter(
                            view_projection,
                            logical,
                            origin,
                            axis,
                            pointer,
                        ),
                    };
                    self.drag = Some(Drag {
                        entity: self.selected.clone().expect("gizmo needs a selection"),
                        mode: self.mode,
                        axis: axis_index,
                        grab_t,
                        start,
                        delta: 0.0,
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
        // Cloned rather than copied: `ResolvedLights` carries a point-light Vec
        // as of M17, so it is no longer `Copy`.
        let lights = self.doc.lights.clone();
        let environment = self.doc.environment;
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
            environment,
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
                let stroke_for = |hot: bool, color: Color32| {
                    Stroke::new(
                        if hot { 4.0 } else { 2.5 },
                        if hot { Color32::WHITE } else { color },
                    )
                };
                match self.mode {
                    GizmoMode::Rotate => {
                        for (i, (axis, color)) in gizmo::AXES.iter().enumerate() {
                            let stroke = stroke_for(self.hot_axis == Some(i), *color);
                            let points: Vec<Option<Vec2>> =
                                gizmo::ring_points(origin, *axis, arm)
                                    .into_iter()
                                    .map(|p| gizmo::project(view_projection, logical, p))
                                    .collect();
                            for pair in points.windows(2) {
                                if let (Some(a), Some(b)) = (pair[0], pair[1]) {
                                    painter.line_segment(
                                        [to_screen(a), to_screen(b)],
                                        stroke,
                                    );
                                }
                            }
                        }
                    }
                    GizmoMode::Translate | GizmoMode::Scale => {
                        if let Some(root) =
                            gizmo::project(view_projection, logical, origin)
                        {
                            for (i, (axis, color)) in gizmo::AXES.iter().enumerate() {
                                let Some(tip) = gizmo::project(
                                    view_projection,
                                    logical,
                                    origin + *axis * arm,
                                ) else {
                                    continue;
                                };
                                let hot = self.hot_axis == Some(i);
                                let stroke = stroke_for(hot, *color);
                                painter.line_segment(
                                    [to_screen(root), to_screen(tip)],
                                    stroke,
                                );
                                let tip_size = if hot { 6.0 } else { 4.5 };
                                if self.mode == GizmoMode::Scale {
                                    // Square tips: the visual tell that this
                                    // gizmo scales rather than moves.
                                    painter.rect_filled(
                                        Rect::from_center_size(
                                            to_screen(tip),
                                            egui::vec2(tip_size * 2.0, tip_size * 2.0),
                                        ),
                                        0.0,
                                        stroke.color,
                                    );
                                } else {
                                    painter.circle_filled(
                                        to_screen(tip),
                                        tip_size,
                                        stroke.color,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        // ── Mode hint (top-left corner of the viewport) ──────────────
        if !self.read_only() {
            let painter = ui.painter_at(rect);
            let mut cursor = rect.min + egui::vec2(8.0, 8.0);
            for (key, label, mode) in [
                ("W", "move", GizmoMode::Translate),
                ("R", "rotate", GizmoMode::Rotate),
                ("S", "scale", GizmoMode::Scale),
            ] {
                let active = self.mode == mode;
                let drawn = painter.text(
                    cursor,
                    egui::Align2::LEFT_TOP,
                    format!("{key} {label}"),
                    egui::FontId::proportional(13.0),
                    if active {
                        Color32::WHITE
                    } else {
                        Color32::from_gray(120)
                    },
                );
                cursor.x = drawn.max.x + 14.0;
            }
        }

        // Drop-target feedback while a file hovers over the window.
        if !self.read_only() && ui.input(|i| !i.raw.hovered_files.is_empty()) {
            ui.painter()
                .rect_filled(rect, 0.0, Color32::from_black_alpha(110));
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "drop to import (.blend, .gltf, .glb)",
                egui::FontId::proportional(18.0),
                Color32::WHITE,
            );
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
            for pending in &self.imports {
                ui.separator();
                ui.label(format!("importing {}…", pending.label));
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

        self.handle_drops(ui);

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
                ui.horizontal(|ui| {
                    ui.heading("entities");
                    // The menu is generated from the builtin list, so a new
                    // primitive in engine-core appears here for free.
                    if !self.read_only() {
                        let mut chosen = None;
                        ui.menu_button("+ add", |ui| {
                            for asset in engine_core::mesh::BuiltinMesh::ASSETS {
                                let name = asset
                                    .strip_prefix(engine_core::mesh::BuiltinMesh::PREFIX)
                                    .expect("ASSETS entries carry the prefix");
                                if ui.button(name).clicked() {
                                    chosen = Some(name.to_string());
                                    ui.close();
                                }
                            }
                        });
                        if let Some(primitive) = chosen {
                            self.add_primitive(&primitive);
                        }
                    }
                });
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
                                match edit {
                                    inspector::InspectorEdit::Set(edit) => self.commit(edit),
                                    inspector::InspectorEdit::AddComponent(component) => {
                                        self.commit_structure(|doc| {
                                            doc.add_component(&entity, &component)
                                        });
                                    }
                                    inspector::InspectorEdit::RemoveComponent(component) => {
                                        self.commit_structure(|doc| {
                                            doc.remove_component(&entity, &component)
                                        });
                                    }
                                }
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
