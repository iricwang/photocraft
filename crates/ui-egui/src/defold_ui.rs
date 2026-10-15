//! Thin native GUI workspace. Image editing and AI remain in their existing panels.
use crate::PhotocraftApp;
use egui::{Color32, Pos2, Sense, Stroke, Vec2};
use photocraft_defold_format::Node;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GuiUiState {
    #[serde(skip)]
    pub prompt_gui: Option<u64>,
    #[serde(skip)]
    pub prompt_drafts: std::collections::BTreeMap<u64, String>,
    #[serde(skip)]
    pub close: Option<(u64, String, Value)>,
    #[serde(skip)]
    pub resource: Option<String>,
    #[serde(default = "unit_zoom")]
    pub zoom: f32,
    #[serde(default)]
    pub pan: [f32; 2],
    #[serde(default)]
    pub project_root: String,
    #[serde(skip)]
    pub project_files: Vec<String>,
    #[serde(skip)]
    pub project_open: bool,
    #[serde(skip)]
    pub collapsed: std::collections::BTreeSet<String>,
    #[serde(skip)]
    pub rename: Option<(String, String)>,
    #[serde(skip)]
    pub drag: Option<(String, [f64; 3], egui::Pos2)>,
    #[serde(skip)]
    pub transform: bool,
    #[serde(skip)]
    pub last_poll: f64,
    #[serde(skip)]
    pub drag_resize: Option<usize>,
    #[serde(skip)]
    pub save_project: bool,
}

fn unit_zoom() -> f32 {
    1.0
}
#[path = "defold_panels.rs"]
mod panels;
use panels::{canvas_input, mixed_tabs};
pub use panels::{layers, menu, options, poll, project_window, properties, toolbar};

pub fn tabs(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    if app.session.gui.documents.is_empty() {
        return;
    }
    mixed_tabs(app, ui);
    if let Some(source) = app.session.active().and_then(|st| st.doc.design_source_id.clone()) {
        let bindings: Vec<_> = app
            .session
            .gui
            .documents
            .iter()
            .flat_map(|d| {
                d.bindings
                    .bindings
                    .iter()
                    .filter(|b| b.design_source_id == source)
                    .map(|b| (d.id, b.id.clone(), b.node.clone(), b.target.clone(), b.source_layer))
            })
            .collect();
        for (gui, binding, node, target, layer) in bindings {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("Node: {node} → {target}"));
                let source = layer.map_or_else(
                    || tl!("Use whole canvas").to_string(),
                    |id| {
                        app.session
                            .active()
                            .and_then(|st| st.doc.layer(photocraft_doc::LayerId(id)))
                            .map_or_else(|| format!("Missing layer {id}"), |l| l.name.clone())
                    },
                );
                ui.label(format!("{}: {source}", tl!("Layer")));
                if ui.button(tl!("Link selected layer")).clicked() {
                    let _ = app.run("defold.bindSourceLayer", json!({"binding":binding}));
                }
                if ui.button(tl!("Use whole canvas")).clicked() {
                    let _ = app.run("defold.bindSourceLayer", json!({"binding":binding,"composite":true}));
                }
                if ui.button(tl!("Return to GUI")).clicked() {
                    let _ = app.run("defold.activateGui", json!({"id":gui}));
                }
                if ui
                    .button(tl!("Publish to Defold"))
                    .on_hover_text(
                        "Save the editable source and replace its bound PNG using the linked layer or visible canvas. All nodes sharing that PNG will update.",
                    )
                    .clicked()
                {
                    if app.ai_runtime.running() {
                        app.ui.status = "Wait for image generation to finish before publishing".into();
                        app.ui.status_error = true;
                    } else {
                        let _ = app.run("defold.publishNodeSource", json!({"binding":binding}));
                        clear_preview(ui.ctx(), gui);
                    }
                }
            });
        }
    }
    ui.separator();
}
#[derive(Clone, Default)]
struct Preview {
    signature: String,
    textures: std::collections::HashMap<String, egui::TextureHandle>,
    errors: Vec<String>,
}
fn clear_preview(ctx: &egui::Context, id: u64) {
    ctx.data_mut(|d| d.remove::<Preview>(egui::Id::new(("defold-preview", id))));
}
fn preview(app: &PhotocraftApp, ctx: &egui::Context, id: u64, nodes: &[Node]) -> Preview {
    let key = egui::Id::new(("defold-preview", id));
    let signature = nodes.iter().map(|n| format!("{}:{};", n.id, n.texture)).collect::<String>();
    if let Some(p) = ctx.data(|d| d.get_temp::<Preview>(key))
        && p.signature == signature
    {
        return p;
    }
    let mut p = Preview { signature, ..Default::default() };
    let mut decoded_pixels = 0_u64;
    for n in nodes.iter().filter(|n| !n.texture.is_empty()).take(256) {
        let loaded = photocraft_engine::defold_cmds::preview_png(&app.session, &n.id).map_err(|e| e.to_string()).and_then(|b| {
            photocraft_codecs::decode_with(
                &b,
                &photocraft_codecs::DecodeOptions {
                    limits: photocraft_codecs::Limits { max_width: 8192, max_height: 8192, max_pixels: 16 * 1024 * 1024, max_alloc: 128 * 1024 * 1024 },
                    ..Default::default()
                },
            )
            .map_err(|e| e.to_string())
        });
        match loaded {
            Ok(image) => {
                decoded_pixels += u64::from(image.width()) * u64::from(image.height());
                if decoded_pixels > 16 * 1024 * 1024 {
                    p.errors.push("Preview texture budget reached; remaining resources are preserved".into());
                    break;
                }
                let image = image.convert(photocraft_codecs::ChannelLayout::Rgba, photocraft_codecs::SampleType::U8);
                let limit = ctx.input(|i| i.max_texture_side).min(8192) as u32;
                if image.width() > limit || image.height() > limit {
                    p.errors.push(format!("{}: preview too large", n.id));
                    continue;
                }
                let pixels = egui::ColorImage::from_rgba_unmultiplied([image.width() as usize, image.height() as usize], image.data());
                p.textures.insert(n.id.clone(), ctx.load_texture(format!("gui-{id}-{}", n.id), pixels, egui::TextureOptions::LINEAR));
            }
            Err(e) => p.errors.push(format!("{}: {e}", n.id)),
        }
    }
    ctx.data_mut(|d| d.insert_temp(key, p.clone()));
    p
}
pub fn document(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let Some(d) = app.session.active_gui() else { return };
    let (id, mut nodes, selected, width, height) = (d.id, d.scene.nodes.clone(), d.selected.clone(), d.width, d.height);
    if app.ui.defold.zoom <= 0.0 {
        app.ui.defold.zoom = 1.0;
    }
    let original_nodes = nodes.clone();
    let online = app.session.active_gui().is_some_and(|d| d.live.is_some());
    let preview = if online { Preview::default() } else { preview(app, ui.ctx(), id, &nodes) };
    let engine_frame = panels::engine_preview(app, ui.ctx());
    let mut hit_nodes = Vec::new();
    let available = Vec2::new(ui.available_width().max(1.0), (ui.available_height() - 45.0).max(1.0));
    let (rect, response) = ui.allocate_exact_size(available, Sense::click_and_drag());
    app.last_canvas_rect = rect;
    app.drop_canvas_rect = None;
    let zoom = (rect.width() / width).min(rect.height() / height).max(0.001) * app.ui.defold.zoom.max(0.1);
    let frame = egui::Rect::from_center_size(rect.center() + Vec2::new(app.ui.defold.pan[0], app.ui.defold.pan[1]), Vec2::new(width, height) * zoom);
    let (board, zoom) = framed_board(&original_nodes, width, height, frame, engine_frame.as_ref().map(|t| t.size()));
    panels::preview_drag(app, ui.ctx(), zoom, &mut nodes);
    let by_id: std::collections::HashMap<_, _> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let t = crate::theme::Tokens::get(ui.ctx());
    let painter = ui.painter().with_clip_rect(frame.intersect(rect));
    painter.rect_filled(frame, 0.0, t.card);
    painter.rect_stroke(board, 0.0, Stroke::new(1.0, t.card_border), egui::StrokeKind::Inside);
    let exact = engine_frame.is_some() && app.ui.defold.drag.is_none();
    if exact && let Some(texture) = &engine_frame {
        painter.image(texture.id(), frame, egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
    }
    for n in &nodes {
        if !n.visible || !n.enabled {
            continue;
        }
        let texture = preview.textures.get(&n.id);
        let size = if n.size[0] <= 0.0 || n.size[1] <= 0.0 {
            texture.map_or([100.0, 40.0], |t| [t.size()[0] as f64, t.size()[1] as f64])
        } else {
            [n.size[0], n.size[1]]
        };
        let pivot = pivot(&n.pivot);
        let points = [
            [-pivot[0] * size[0], -pivot[1] * size[1]],
            [(1.0 - pivot[0]) * size[0], -pivot[1] * size[1]],
            [(1.0 - pivot[0]) * size[0], (1.0 - pivot[1]) * size[1]],
            [-pivot[0] * size[0], (1.0 - pivot[1]) * size[1]],
        ]
        .map(|p| {
            let p = world(n, &by_id, p);
            Pos2::new(board.left() + p[0] as f32 * zoom, board.bottom() - p[1] as f32 * zoom)
        });
        let color = Color32::from_rgba_unmultiplied(
            (n.color[0].clamp(0.0, 1.0) * 255.0) as u8,
            (n.color[1].clamp(0.0, 1.0) * 255.0) as u8,
            (n.color[2].clamp(0.0, 1.0) * 255.0) as u8,
            (n.alpha.clamp(0.0, 1.0) * 255.0) as u8,
        );
        if exact {
        } else if n.kind == "TYPE_TEXT" {
            let center = points.iter().fold(Vec2::ZERO, |s, p| s + p.to_vec2()) / 4.0;
            painter.text(center.to_pos2(), egui::Align2::CENTER_CENTER, &n.text, egui::FontId::proportional((20.0 * zoom).max(8.0)), color);
        } else if let Some(texture) = texture {
            let mut mesh = egui::Mesh::with_texture(texture.id());
            for (pos, uv) in points.into_iter().zip([Pos2::new(0.0, 1.0), Pos2::new(1.0, 1.0), Pos2::new(1.0, 0.0), Pos2::new(0.0, 0.0)]) {
                mesh.vertices.push(egui::epaint::Vertex { pos, uv, color });
            }
            mesh.indices.extend([0, 1, 2, 0, 2, 3]);
            painter.add(egui::Shape::mesh(mesh));
        } else {
            painter.add(egui::Shape::convex_polygon(points.to_vec(), color, Stroke::NONE));
        }
        if selected.as_deref() == Some(&n.id) {
            painter.add(egui::Shape::closed_line(points.to_vec(), Stroke::new(2.0, t.accent)));
            if app.ui.defold.transform {
                for p in points {
                    painter.rect_filled(egui::Rect::from_center_size(p, Vec2::splat(7.0)), 0.0, t.accent);
                }
            }
        }
        hit_nodes.push((n.clone(), points));
    }
    canvas_input(app, ui, &response, board, zoom, &hit_nodes, &original_nodes);
    if online && !exact {
        ui.label(tl!("Approximate preview · waiting for Defold"));
    }
    if !preview.errors.is_empty() {
        ui.label(format!("{} resource preview warnings", preview.errors.len()));
    }
}
// Defold previews frame the union of the display rectangle and scene bounds,
// including authored nodes outside the display, with a one-pixel edge margin.
fn framed_board(nodes: &[Node], width: f32, height: f32, frame: egui::Rect, pixels: Option<[usize; 2]>) -> (egui::Rect, f32) {
    let by: std::collections::HashMap<_, _> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut min = [0.0_f64; 2];
    let mut max = [width as f64, height as f64];
    for n in nodes.iter().filter(|n| n.visible && n.enabled) {
        let pivot = pivot(&n.pivot);
        let size = [n.size[0].max(1.0), n.size[1].max(1.0)];
        for corner in [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]] {
            let p = world(n, &by, [(corner[0] - pivot[0]) * size[0], (corner[1] - pivot[1]) * size[1]]);
            for axis in 0..2 {
                if p[axis].is_finite() {
                    min[axis] = min[axis].min(p[axis]);
                    max[axis] = max[axis].max(p[axis]);
                }
            }
        }
    }
    let span = [(max[0] - min[0]).max(1.0), (max[1] - min[1]).max(1.0)];
    let mut zoom = (frame.width() as f64 / span[0]).min(frame.height() as f64 / span[1]);
    if let Some([w, h]) = pixels {
        zoom /= 1.0 + 2.0 / (w.min(h).max(1) as f64);
    }
    let zoom = zoom as f32;
    let left = frame.center().x - ((min[0] + max[0]) * 0.5) as f32 * zoom;
    let bottom = frame.center().y + ((min[1] + max[1]) * 0.5) as f32 * zoom;
    (egui::Rect::from_min_max(Pos2::new(left, bottom - height * zoom), Pos2::new(left + width * zoom, bottom)), zoom)
}
fn pivot(s: &str) -> [f64; 2] {
    match s {
        "PIVOT_NW" => [0.0, 1.0],
        "PIVOT_N" => [0.5, 1.0],
        "PIVOT_NE" => [1.0, 1.0],
        "PIVOT_W" => [0.0, 0.5],
        "PIVOT_E" => [1.0, 0.5],
        "PIVOT_SW" => [0.0, 0.0],
        "PIVOT_S" => [0.5, 0.0],
        "PIVOT_SE" => [1.0, 0.0],
        _ => [0.5, 0.5],
    }
}
fn world<'a>(mut n: &'a Node, nodes: &std::collections::HashMap<&str, &'a Node>, mut p: [f64; 2]) -> [f64; 2] {
    for _ in 0..256 {
        let (sin, cos) = n.rotation[2].to_radians().sin_cos();
        let x = p[0] * n.scale[0];
        let y = p[1] * n.scale[1];
        p = [cos * x - sin * y + n.position[0], sin * x + cos * y + n.position[1]];
        let Some(parent) = nodes.get(n.parent.as_str()).copied() else {
            break;
        };
        n = parent;
    }
    p
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "defold_ui_tests.rs"]
mod tests;
pub fn intercept_close(app: &mut PhotocraftApp, cmd: &str, params: &Value) -> bool {
    let active = app.session.gui.active;
    let all = matches!(cmd, "file.exit" | "file.closeAll");
    let others = cmd == "file.closeOthers";
    let one = cmd == "file.close" && params.get("document").is_none();
    if !all && !others && !one {
        return false;
    }
    let target =
        app.session.gui.documents.iter().find(|d| d.dirty() && (all || (others && Some(d.id) != active) || (one && Some(d.id) == active))).map(|d| d.id);
    if let Some(id) = target {
        app.ui.defold.close = Some((id, cmd.into(), params.clone()));
        true
    } else {
        false
    }
}
pub fn close_prompt(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some((id, cmd, params)) = app.ui.defold.close.clone() else {
        return;
    };
    let Some(d) = app.session.gui.documents.iter().find(|d| d.id == id) else {
        app.ui.defold.close = None;
        return;
    };
    let name = crate::file_open::display_name(&d.path);
    let mut action = None;
    egui::Modal::new(egui::Id::new("gui-close-prompt")).show(ctx, |ui| {
        ui.label(format!("Save changes to {name}?"));
        ui.horizontal(|ui| {
            for label in ["Save", "Discard", "Cancel"] {
                if ui.button(tl!(label)).clicked() {
                    action = Some(label);
                }
            }
        });
    });
    match action {
        Some("Cancel") => app.ui.defold.close = None,
        Some(answer) => {
            let focus = app.session.focus();
            let _ = app.run("defold.activateGui", json!({"id":id}));
            let done = if answer == "Save" {
                app.run("defold.saveGui", json!({})).is_ok()
            } else {
                app.run("defold.closeGui", json!({"id":id,"discard":true})).is_ok()
            };
            app.session.restore_focus(&focus);
            if done {
                app.ui.defold.close = None;
                // A discarded target is already closed; never close the next tab by accident.
                if cmd != "file.close" || answer == "Save" {
                    let _ = crate::menus::invoke(app, ctx, &cmd, params);
                }
            }
        }
        None => {}
    }
}
