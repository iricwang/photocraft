//! Defold adapters for PhotoCraft's existing document, Layers and Properties surfaces.
use super::*;
use crate::{icons, theme::Tokens};
use photocraft_engine::defold_cmds::DocumentFocus;

pub(super) fn mixed_tabs(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let mut tabs: Vec<_> =
        app.session.documents().iter().map(|d| (DocumentFocus::Image(d.open_id), format!("{}{}", d.doc.name, if d.is_dirty() { " *" } else { "" }))).collect();
    tabs.extend(
        app.session
            .gui
            .documents
            .iter()
            .map(|d| (DocumentFocus::Gui(d.id), format!("{}{}", crate::file_open::display_name(&d.path), if d.dirty() { " *" } else { "" }))),
    );
    let focus = app.session.focus();
    let mut selected = tabs.iter().position(|(f, _)| *f == focus).unwrap_or(0);
    let names: Vec<_> = tabs.iter().map(|(_, n)| n.as_str()).collect();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::hover());
    let result = crate::tab_strip::pro_tabs(ui, ui.id().with("workspace-documents"), rect, rect.right(), &names, &mut selected, false, false);
    if result.clicked
        && let Some((f, _)) = tabs.get(selected)
    {
        app.session.restore_focus(f);
        app.ui.defold.zoom = 1.0;
        app.ui.defold.pan = [0.0; 2];
    }
    if let Some(crate::tab_strip::TabContextAction::Close(i)) = result.context
        && let Some((f, _)) = tabs.get(i)
    {
        app.session.restore_focus(f);
        let _ = crate::menus::invoke(app, ui.ctx(), "file.close", json!({}));
    }
}
pub fn options(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::top("options_bar").exact_size(36.0).frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(8, 0))).show(ui, |ui| {
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                if ui.button(tl!("Defold Project…")).clicked() {
                    app.ui.defold.project_open = true;
                    if let Some(d) = app.session.active_gui() {
                        app.ui.defold.project_root = d.root.clone();
                    }
                }
                let live = app.session.active_gui().and_then(|d| d.live.as_ref());
                if let Some(live) = live {
                    ui.colored_label(
                        if live.error.is_some() { t.text } else { t.accent },
                        if live.error.is_some() { tl!("Sync paused") } else { tl!("Live · Defold") },
                    );
                    if ui.button(tl!("Refresh")).clicked() {
                        let _ = app.run("defold.refreshLive", json!({}));
                    }
                } else if ui.button(tl!("Connect Defold")).clicked() {
                    let _ = app.run("defold.connectLive", json!({}));
                }
                if ui.button(tl!("Save")).clicked() {
                    if app.session.active_gui().is_some_and(|d| d.live.is_some()) {
                        app.ui.defold.save_project = true;
                    } else {
                        let _ = app.run("defold.saveGui", json!({}));
                    }
                }
                for (label, cmd) in [("Undo", "edit.undo"), ("Redo", "edit.redo")] {
                    if ui.add_enabled(app.session.is_enabled(cmd), egui::Button::new(tl!(label))).clicked() {
                        let _ = app.run(cmd, json!({}));
                    }
                }
                if ui.button(tl!("Fit on Screen")).clicked() {
                    app.ui.defold.zoom = 1.0;
                    app.ui.defold.pan = [0.0; 2];
                }
                if ui.button(tl!("Reload textures")).clicked()
                    && let Some(id) = app.session.gui.active
                {
                    clear_preview(ui.ctx(), id);
                    #[cfg(not(target_arch = "wasm32"))]
                    ui.ctx().data_mut(|d| d.remove::<EngineFrame>(egui::Id::new(("defold-engine-frame", id))));
                }
            });
        });
    });
}
pub fn toolbar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    crate::direction::start_panel("toolbar").resizable(false).exact_size(40.0).frame(egui::Frame::NONE.fill(t.chrome).inner_margin(5)).show(ui, |ui| {
        for (icon, tip, kind) in [("move", "Move", None), ("type", "Text", Some("TYPE_TEXT")), ("rectangle-horizontal", "Rectangle", Some("TYPE_BOX"))] {
            if icons::button(ui, icon, 30.0, kind.is_none(), tl!(tip)).clicked() {
                if let Some(kind) = kind {
                    let _ = app.run("defold.addNode", json!({"kind":kind}));
                } else {
                    app.ui.defold.transform = false;
                }
            }
        }
        if icons::button(ui, "scan", 30.0, app.ui.defold.transform, tl!("Free Transform")).clicked() {
            app.ui.defold.transform = !app.ui.defold.transform;
        }
        if icons::button(ui, "folder", 30.0, false, tl!("Defold Project")).clicked() {
            app.ui.defold.project_open = true;
        }
    });
}
pub fn layers(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let Some(d) = app.session.active_gui() else { return };
    let nodes = d.scene.nodes.clone();
    let selected = d.selected.clone();
    let gui = d.id;
    let mut children: std::collections::HashMap<&str, Vec<&Node>> = std::collections::HashMap::new();
    for n in &nodes {
        children.entry(&n.parent).or_default().push(n);
    }
    let mut rows = Vec::new();
    let mut stack: Vec<_> = children.get("").into_iter().flatten().map(|n| (*n, 0usize)).collect();
    while let Some((n, depth)) = stack.pop() {
        rows.push((n, depth));
        if depth < 128
            && !app.ui.defold.collapsed.contains(&format!("{gui}:{}", n.id))
            && let Some(kids) = children.get(n.id.as_str())
        {
            stack.extend(kids.iter().map(|n| (*n, depth + 1)));
        }
    }
    if let Some(n) = nodes.iter().find(|n| Some(&n.id) == selected.as_ref()) {
        ui.horizontal(|ui| {
            ui.label(tl!("Opacity"));
            if let Some(alpha) = number_editor(ui, n.alpha * 100.0, 0) {
                let _ = app.run("defold.setNodeProperty", json!({"property":"alpha","value":alpha.clamp(0.0,100.0)/100.0}));
            }
        });
    }
    let t = Tokens::get(ui.ctx());
    for (n, depth) in rows {
        let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click_and_drag());
        if selected.as_deref() == Some(&n.id) {
            ui.painter().rect_filled(rect, 0.0, t.row_selected);
        } else if response.hovered() {
            ui.painter().rect_filled(rect, 0.0, t.hover);
        }
        let mut x = rect.left() + 3.0;
        let eye = egui::Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new(24.0, 30.0));
        icons::paint(ui, eye, if n.visible { "eye" } else { "eye-off" }, 14.0, t.icon);
        if ui.interact(eye, ui.id().with(("gui-eye", &n.id)), Sense::click()).clicked() {
            let _ = app.run("defold.setNodeProperty", json!({"node":n.id,"property":"visible","value":!n.visible}));
        }
        x += 26.0 + depth as f32 * 14.0;
        if children.contains_key(n.id.as_str()) {
            let key = format!("{gui}:{}", n.id);
            let folded = app.ui.defold.collapsed.contains(&key);
            let r = egui::Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new(14.0, 30.0));
            icons::paint(ui, r, if folded { "chevron-right" } else { "chevron-down" }, 10.0, t.icon);
            if ui.interact(r, ui.id().with(("gui-fold", &n.id)), Sense::click()).clicked() {
                if folded {
                    app.ui.defold.collapsed.remove(&key);
                } else {
                    app.ui.defold.collapsed.insert(key);
                }
            }
        }
        x += 14.0;
        let icon = if n.kind == "TYPE_TEXT" {
            "type"
        } else if children.contains_key(n.id.as_str()) {
            "folder"
        } else {
            "image"
        };
        icons::paint(ui, egui::Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new(24.0, 30.0)), icon, 16.0, t.icon);
        x += 28.0;
        let (_, right) = crate::layer_row_ui::layout(rect, x, &[]);
        let name_rect = egui::Rect::from_min_max(Pos2::new(x, rect.top()), Pos2::new(right.max(x), rect.bottom()));
        if let Some((id, draft)) = &mut app.ui.defold.rename
            && *id == n.id
        {
            let r = ui.put(name_rect, egui::TextEdit::singleline(draft));
            r.request_focus();
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let name = draft.clone();
                let _ = app.run("defold.renameNode", json!({"node":n.id,"name":name}));
                app.ui.defold.rename = None;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                app.ui.defold.rename = None;
            }
        }
        if app.ui.defold.rename.as_ref().is_none_or(|(id, _)| *id != n.id) {
            let g = crate::tab_strip::elided(ui, &n.id, egui::FontId::proportional(12.0), t.text, name_rect.width());
            ui.painter().galley(Pos2::new(name_rect.left(), name_rect.center().y - g.size().y / 2.0), g, t.text);
        }
        if response.clicked() {
            let _ = app.run("defold.selectNode", json!({"node":n.id}));
        }
        if response.double_clicked() {
            app.ui.defold.rename = Some((n.id.clone(), n.id.clone()));
        }
        if response.drag_started() {
            response.dnd_set_drag_payload((gui, n.id.clone()));
        }
        if let Some(payload) = response.dnd_release_payload::<(u64, String)>()
            && payload.0 == gui
            && payload.1 != n.id
        {
            let child = ui.input(|i| i.modifiers.shift);
            let params = if child { json!({"node":payload.1,"parent":n.id}) } else { json!({"node":payload.1,"parent":n.parent,"before":n.id}) };
            let _ = app.run("defold.moveNode", params);
        }
        response.context_menu(|ui| {
            if ui.button(tl!("Add child")).clicked() {
                let _ = app.run("defold.addNode", json!({"parent":n.id}));
                ui.close();
            }
            if ui.button(tl!("Add sibling below")).clicked() {
                let _ = app.run("defold.addNode", json!({"parent":n.parent,"after":n.id}));
                ui.close();
            }
            if ui.button(tl!("Rename Layer…")).clicked() {
                app.ui.defold.rename = Some((n.id.clone(), n.id.clone()));
                ui.close();
            }
            if ui.button(tl!("Delete layer")).clicked() {
                let _ = app.run("defold.deleteNode", json!({"node":n.id}));
                ui.close();
            }
            if ui.button(tl!("Edit / generate node image")).clicked() {
                let _ = app.run("defold.openNodeSource", json!({"node":n.id}));
                app.ui.ai.visible = true;
                ui.close();
            }
        });
    }
    ui.separator();
    ui.horizontal(|ui| {
        if icons::button(ui, "plus", 24.0, false, tl!("Create a new layer")).clicked() {
            let parent = nodes.iter().find(|n| Some(&n.id) == selected.as_ref()).map(|n| n.parent.clone()).unwrap_or_default();
            let _ = app.run("defold.addNode", json!({"parent":parent,"after":selected}));
        }
        if icons::button(ui, "folder-plus", 24.0, false, tl!("Add child node")).clicked() {
            let _ = app.run("defold.addNode", json!({"parent":selected.unwrap_or_default()}));
        }
        if icons::button(ui, "trash", 24.0, false, tl!("Delete layer")).clicked() {
            let _ = app.run("defold.deleteNode", json!({}));
        }
    });
}
pub fn properties(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let Some(d) = app.session.active_gui() else { return };
    let Some(n) = d.selected_node().cloned() else {
        ui.label(tl!("No properties"));
        return;
    };
    ui.label(format!("{} · {}", n.id, n.kind.trim_start_matches("TYPE_")));
    let mut props: Vec<(String, Value, bool)> = Vec::new();
    if let Some(live) = &d.live {
        if let Some(r) = live.snapshot.nodes.iter().find(|r| r.id == n.id) {
            props.extend(r.properties.iter().map(|(k, v)| {
                (k.clone(), if k == "rotation" { photocraft_engine::defold_live::rotation_degrees(v) } else { v.clone() }, r.writable.contains(k))
            }));
        }
        if let Some(e) = &live.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
    } else {
        for p in photocraft_defold_format::properties(&n.kind) {
            if let Ok(v) = d.scene.property(&n.id, &p.path) {
                props.push((p.path, v, n.editable));
            }
        }
    }
    props.sort_by_key(|(key, _, _)| match key.as_str() {
        "id" => 0,
        "type" => 1,
        "position" => 2,
        "rotation" => 3,
        "scale" => 4,
        "manual_size" | "size" => 5,
        "size_mode" => 6,
        "enabled" => 7,
        "visible" => 8,
        "texture" => 9,
        "text" => 10,
        "font" => 11,
        "material" => 12,
        "color" => 13,
        "alpha" => 14,
        _ => 15,
    });
    let gui = d.id;
    let online = d.live.is_some();
    let schema = photocraft_defold_format::properties(&n.kind);
    for (key, value, writable) in props {
        let mut next = None;
        ui.push_id((&n.id, &key), |ui| {
            ui.add_enabled_ui(writable, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(&key);
                    let field = match key.as_str() {
                        "manual_size" => "size",
                        "x_anchor" => "xanchor",
                        "y_anchor" => "yanchor",
                        k => k,
                    };
                    let choices = schema.iter().find(|p| p.path == field).and_then(|p| match &p.kind {
                        photocraft_defold_format::PropertyKind::Enum(c) => Some(*c),
                        _ => None,
                    });
                    if let Some(choices) = choices {
                        egui::ComboBox::from_id_salt("enum").selected_text(value.as_str().unwrap_or_default()).width(ui.available_width().max(70.0)).show_ui(
                            ui,
                            |ui| {
                                for choice in choices {
                                    let choice = if online { choice.replace('_', "-").to_lowercase() } else { choice.to_string() };
                                    if ui.selectable_label(value.as_str() == Some(&choice), &choice).clicked() {
                                        next = Some(json!(choice));
                                    }
                                }
                            },
                        );
                        return;
                    }
                    match &value {
                        Value::Bool(v) => {
                            let mut v = *v;
                            if ui.checkbox(&mut v, "").changed() {
                                next = Some(json!(v));
                            }
                        }
                        Value::Number(v) => {
                            if let Some(v) = number_editor(ui, v.as_f64().unwrap_or_default(), 0) {
                                next = Some(json!(v));
                            }
                        }
                        Value::Array(a) if a.iter().all(Value::is_number) => {
                            let mut a = a.clone();
                            let mut changed = false;
                            for (i, v) in a.iter_mut().enumerate() {
                                ui.label(["X", "Y", "Z", "W"].get(i).copied().unwrap_or(""));
                                if let Some(f) = number_editor(ui, v.as_f64().unwrap_or_default(), i) {
                                    *v = json!(f);
                                    changed = true;
                                }
                            }
                            if changed {
                                next = Some(json!(a));
                            }
                        }
                        _ => {
                            let raw = value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string());
                            let id = ui.id().with((gui, "property-draft"));
                            let mut draft =
                                ui.ctx().data(|d| d.get_temp::<(String, String)>(id)).filter(|(base, _)| base == &raw).map_or_else(|| raw.clone(), |(_, s)| s);
                            let r = ui.add(egui::TextEdit::singleline(&mut draft).desired_width(ui.available_width().max(50.0)));
                            if r.lost_focus() && draft != raw {
                                next = Some(if value.is_string() { json!(draft.clone()) } else { serde_json::from_str(&draft).unwrap_or(value.clone()) });
                            }
                            ui.ctx().data_mut(|d| d.insert_temp(id, (raw, draft)));
                        }
                    }
                });
            });
        });
        if let Some(value) = next {
            if key == "id" {
                let _ = app.run("defold.renameNode", json!({"node":n.id,"name":value}));
            } else {
                let _ = app.run("defold.setNodeProperty", json!({"node":n.id,"property":key,"value":value}));
            }
        }
    }
    ui.separator();
    if ui.button(tl!("Edit / generate node image")).clicked() {
        let _ = app.run("defold.openNodeSource", json!({"node":n.id}));
        app.ui.ai.visible = true;
    }
    if ui.button(tl!("Show resource in folder")).clicked() {
        let _ = app.run("defold.revealNodeResource", json!({}));
    }
    if ui.button(tl!("Open resource in Defold")).clicked() {
        let _ = app.run("defold.openNodeResourceInEditor", json!({}));
    }
}
fn number_editor(ui: &mut egui::Ui, base: f64, component: usize) -> Option<f64> {
    let id = ui.id().with(("number-draft", component));
    let mut value = ui.ctx().data(|d| d.get_temp::<(f64, f64)>(id)).filter(|(old, _)| *old == base).map_or(base, |(_, v)| v);
    let response = ui.add(egui::DragValue::new(&mut value).speed(0.1));
    let commit = response.drag_stopped() || (response.changed() && !response.dragged());
    ui.ctx().data_mut(|d| d.insert_temp(id, (base, value)));
    (commit && value != base).then_some(value)
}
pub(super) fn canvas_input(
    app: &mut PhotocraftApp,
    ui: &mut egui::Ui,
    response: &egui::Response,
    _board: egui::Rect,
    zoom: f32,
    hits: &[(Node, [Pos2; 4])],
    nodes: &[Node],
) {
    if app.ui.defold.save_project || app.ui.defold.project_open || app.ui.defold.close.is_some() {
        return;
    }
    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            app.ui.defold.zoom = (app.ui.defold.zoom.max(0.1) * (scroll * 0.002).exp()).clamp(0.1, 20.0);
        }
    }
    if response.dragged() && ui.input(|i| i.key_down(egui::Key::Space)) {
        let delta = ui.input(|i| i.pointer.delta());
        app.ui.defold.pan[0] += delta.x;
        app.ui.defold.pan[1] += delta.y;
        app.ui.defold.drag = None;
        return;
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.ui.defold.drag = None;
        app.ui.defold.transform = false;
        return;
    }
    if (response.clicked() || response.drag_started())
        && let Some(p) = ui.input(|i| i.pointer.press_origin().or(i.pointer.interact_pos()))
    {
        let selected = app.session.active_gui().and_then(|d| d.selected.as_deref());
        let handle = if app.ui.defold.transform {
            hits.iter()
                .find(|(n, _)| Some(n.id.as_str()) == selected)
                .and_then(|(n, points)| points.iter().position(|v| v.distance(p) <= 10.0).map(|i| (n, Some(i))))
        } else {
            None
        };
        let hit = handle.or_else(|| hits.iter().rev().find(|(_, points)| inside(*points, p)).map(|(n, _)| (n, None)));
        if let Some((n, corner)) = hit {
            let _ = app.run("defold.selectNode", json!({"node":n.id}));
            if response.drag_started() {
                app.ui.defold.drag = Some((n.id.clone(), n.position, p));
                app.ui.defold.drag_resize = corner;
            }
        }
    }
    if response.drag_stopped()
        && let Some((id, _, start)) = app.ui.defold.drag.take()
        && let Some(end) = ui.input(|i| i.pointer.interact_pos())
        && let Some(mut n) = nodes.iter().find(|n| n.id == id).cloned()
        && let Some(delta) = parent_delta(nodes, &id, [(end.x - start.x) as f64 / zoom as f64, -(end.y - start.y) as f64 / zoom as f64])
        && transform_node(&mut n, delta, app.ui.defold.drag_resize)
    {
        let mut values = json!({"position.x":n.position[0],"position.y":n.position[1]});
        if app.ui.defold.drag_resize.is_some() {
            values["size.x"] = json!(n.size[0]);
            values["size.y"] = json!(n.size[1]);
            values["size_mode"] = json!("SIZE_MODE_MANUAL");
        }
        let _ = app.run("defold.setNodeProperties", json!({"node":id,"values":values}));
    }
    if !ui.ctx().egui_wants_keyboard_input() {
        if ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
            let _ = app.run("defold.deleteNode", json!({}));
        }
        let step = if ui.input(|i| i.modifiers.shift) { 10.0 } else { 1.0 };
        let mut delta = [0.0; 2];
        for (key, axis, sign) in
            [(egui::Key::ArrowLeft, 0, -1.0), (egui::Key::ArrowRight, 0, 1.0), (egui::Key::ArrowUp, 1, 1.0), (egui::Key::ArrowDown, 1, -1.0)]
        {
            if ui.input(|i| i.key_pressed(key))
                && let Some(v) = delta.get_mut(axis)
            {
                *v += sign * step;
            }
        }
        if delta != [0.0; 2]
            && let Some(n) = app.session.active_gui().and_then(|d| d.selected_node()).cloned()
        {
            let _ = app.run("defold.setNodeProperties", json!({"values":{"position.x":n.position[0]+delta[0],"position.y":n.position[1]+delta[1]}}));
        }
    }
}
pub(super) fn preview_drag(app: &PhotocraftApp, ctx: &egui::Context, zoom: f32, nodes: &mut [Node]) {
    if let Some((id, _, start)) = &app.ui.defold.drag
        && let Some(end) = ctx.input(|i| i.pointer.interact_pos())
        && let Some(delta) = parent_delta(nodes, id, [(end.x - start.x) as f64 / zoom as f64, -(end.y - start.y) as f64 / zoom as f64])
        && let Some(n) = nodes.iter_mut().find(|n| n.id == *id)
    {
        transform_node(n, delta, app.ui.defold.drag_resize);
    }
}
fn transform_node(n: &mut Node, delta: [f64; 2], corner: Option<usize>) -> bool {
    let Some(corner) = corner else {
        n.position[0] += delta[0];
        n.position[1] += delta[1];
        return true;
    };
    if n.scale[0].abs() < 1e-9 || n.scale[1].abs() < 1e-9 {
        return false;
    }
    let (sin, cos) = n.rotation[2].to_radians().sin_cos();
    let local = [(cos * delta[0] + sin * delta[1]) / n.scale[0], (-sin * delta[0] + cos * delta[1]) / n.scale[1]];
    let positive = [corner == 1 || corner == 2, corner >= 2];
    let pivot = super::pivot(&n.pivot);
    let mut shift = [0.0; 2];
    for axis in 0..2 {
        let sign = if positive[axis] { 1.0 } else { -1.0 };
        let size = (n.size[axis] + sign * local[axis]).max(1.0);
        let delta = (size - n.size[axis]) * sign;
        n.size[axis] = size;
        shift[axis] = delta * if positive[axis] { pivot[axis] } else { 1.0 - pivot[axis] };
    }
    n.position[0] += cos * shift[0] * n.scale[0] - sin * shift[1] * n.scale[1];
    n.position[1] += sin * shift[0] * n.scale[0] + cos * shift[1] * n.scale[1];
    true
}

#[cfg(test)]
#[test]
fn resized_rotated_node_keeps_opposite_corner_fixed() {
    let scene = photocraft_defold_format::Scene::parse(
        "nodes {id: \"a\" type: TYPE_BOX size {x:100 y:40} rotation {z:90} position {x:200 y:100} scale{x:2 y:2}}".into(),
    )
    .unwrap();
    let mut node = scene.nodes[0].clone();
    let map = std::collections::HashMap::from([("a", &node)]);
    let before = world(&node, &map, [-50.0, -20.0]);
    assert!(transform_node(&mut node, [-20.0, 40.0], Some(2)));
    assert!((node.size[0] - 120.0).abs() < 1e-8);
    assert!((node.size[1] - 50.0).abs() < 1e-8);
    let map = std::collections::HashMap::from([("a", &node)]);
    let after = world(&node, &map, [-node.size[0] / 2.0, -node.size[1] / 2.0]);
    assert!((before[0] - after[0]).abs() < 1e-8);
    assert!((before[1] - after[1]).abs() < 1e-8);
}
fn inside(points: [Pos2; 4], p: Pos2) -> bool {
    let mut sign = 0.0_f32;
    for i in 0..4 {
        let a = points[i];
        let b = points[(i + 1) % 4];
        let c = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
        if c.abs() > 0.001 {
            if sign != 0.0 && sign.signum() != c.signum() {
                return false;
            }
            sign = c;
        }
    }
    true
}
fn parent_delta(nodes: &[Node], id: &str, delta: [f64; 2]) -> Option<[f64; 2]> {
    let by: std::collections::HashMap<_, _> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let n = by.get(id)?;
    let Some(parent) = by.get(n.parent.as_str()) else { return Some(delta) };
    let o = world(parent, &by, [0.0, 0.0]);
    let x = world(parent, &by, [1.0, 0.0]);
    let y = world(parent, &by, [0.0, 1.0]);
    let (a, b, c, d) = (x[0] - o[0], y[0] - o[0], x[1] - o[1], y[1] - o[1]);
    let det = a * d - b * c;
    if det.abs() < 1e-9 {
        return None;
    }
    Some([(d * delta[0] - b * delta[1]) / det, (-c * delta[0] + a * delta[1]) / det])
}
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
struct Poll {
    id: u64,
    revision: u64,
    result: std::sync::Arc<std::sync::Mutex<Option<Result<photocraft_engine::defold_live::Snapshot, String>>>>,
}
pub fn poll(app: &mut PhotocraftApp, ctx: &egui::Context) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let key = egui::Id::new("defold-live-poll");
        if let Some(poll) = ctx.data(|d| d.get_temp::<Poll>(key)) {
            let result = poll.result.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
            if let Some(result) = result {
                ctx.data_mut(|d| d.remove::<Poll>(key));
                match result {
                    Ok(snapshot) => {
                        let _ = app.run("defold.receiveLiveSnapshot", json!({"id":poll.id,"revision":poll.revision,"snapshot":snapshot}));
                    }
                    Err(e) => {
                        app.ui.status = e;
                        app.ui.status_error = true;
                    }
                }
            }
        }
        let now = ctx.input(|i| i.time);
        let target = app
            .session
            .active_gui()
            .filter(|d| d.live.as_ref().is_some_and(|s| s.error.is_none()))
            .and_then(|d| photocraft_engine::defold_live::path(d).ok().map(|path| (d.id, d.revision, d.root.clone(), path)));
        if let Some((id, revision, root, path)) = target {
            if now - app.ui.defold.last_poll > 2.0
                && !ctx.input(|i| i.pointer.any_down())
                && !ctx.egui_wants_keyboard_input()
                && ctx.data(|d| d.get_temp::<Poll>(key)).is_none()
            {
                app.ui.defold.last_poll = now;
                let result = std::sync::Arc::new(std::sync::Mutex::new(None));
                let output = result.clone();
                let repaint = ctx.clone();
                std::thread::spawn(move || {
                    let snap = photocraft_engine::defold_live::request(&root, &json!({"op":"snapshot","path":path})).map_err(|e| e.to_string());
                    *output.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(snap);
                    repaint.request_repaint();
                });
                ctx.data_mut(|d| d.insert_temp(key, Poll { id, revision, result }));
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(2));
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (app, ctx);
}
pub fn project_window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if app.ui.defold.save_project {
        egui::Modal::new(egui::Id::new("defold-project-save")).show(ctx, |ui| {
            ui.label(tl!("Defold saves all modified project resources. Continue?"));
            ui.horizontal(|ui| {
                if ui.button(tl!("Save")).clicked() {
                    let _ = app.run("defold.saveProject", json!({"allModifiedResources":true}));
                    app.ui.defold.save_project = false;
                }
                if ui.button(tl!("Cancel")).clicked() {
                    app.ui.defold.save_project = false;
                }
            });
        });
    }
    if !app.ui.defold.project_open {
        return;
    }
    let mut open = true;
    egui::Window::new(tl!("Defold Project")).open(&mut open).default_width(540.0).show(ctx, |ui| {
        ui.label(tl!("Project folder (game.project)"));
        ui.text_edit_singleline(&mut app.ui.defold.project_root);
        if ui.button(tl!("Open")).clicked() {
            let root = app.ui.defold.project_root.clone();
            if let Ok(v) = app.run("defold.openProject", json!({"path":root})) {
                app.ui.defold.project_files =
                    v["files"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
            }
        }
        egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
            for path in app.ui.defold.project_files.clone() {
                if ui.selectable_label(false, &path).clicked() {
                    let root = app.ui.defold.project_root.clone();
                    // canonicalize() returns a verbatim Windows root (\\?\…). String
                    // concatenation leaves forward slashes in it, which Win32 rejects.
                    let file = std::path::Path::new(&root).join(path.trim_start_matches('/'));
                    if app.open_path(&file.to_string_lossy()).is_ok() {
                        app.ui.panels.layers = true;
                        app.ui.panels.properties = true;
                        app.ui.panels.dock = true;
                        app.ui.defold.zoom = 1.0;
                        // open_path already connects when a running editor is detected.
                        app.ui.defold.project_open = false;
                    }
                }
            }
        });
    });
    app.ui.defold.project_open &= open;
}
pub fn menu(app: &mut PhotocraftApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if id == "defold.projectWindow" {
        app.ui.defold.project_open = true;
        return Some(Ok(Value::Null));
    }
    app.session.active_gui()?;
    match id {
        "file.save" if app.session.active_gui().is_some_and(|d| d.live.is_some()) => {
            app.ui.defold.save_project = true;
            Some(Ok(Value::Null))
        }
        "layer.renameLayer" if p.get("name").is_none() => {
            if let Some(n) = app.session.active_gui().and_then(|d| d.selected_node()) {
                app.ui.defold.rename = Some((n.id.clone(), n.id.clone()));
            }
            Some(Ok(Value::Null))
        }
        "layer.new.layer" => Some(app.run("defold.addNode", json!({}))),
        "edit.freeTransform" => {
            app.ui.defold.transform = true;
            Some(Ok(Value::Null))
        }
        _ => None,
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
struct EngineFrame {
    revision: u64,
    texture: Option<egui::TextureHandle>,
    pending: std::sync::Arc<std::sync::Mutex<Option<Result<egui::ColorImage, String>>>>,
    finished: bool,
}
pub(super) fn engine_preview(app: &PhotocraftApp, ctx: &egui::Context) -> Option<egui::TextureHandle> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let d = app.session.active_gui()?;
        d.live.as_ref()?;
        let key = egui::Id::new(("defold-engine-frame", d.id));
        let mut frame = ctx.data(|m| m.get_temp::<EngineFrame>(key));
        if let Some(f) = &mut frame {
            let result = f.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
            if let Some(result) = result {
                f.finished = true;
                if let Ok(image) = result
                    && image.size.iter().all(|side| *side <= ctx.input(|i| i.max_texture_side))
                {
                    f.texture = Some(ctx.load_texture("Defold scene", image, egui::TextureOptions::LINEAR));
                }
            }
        }
        if frame.as_ref().is_none_or(|f| f.finished && f.revision != d.revision) {
            let root = d.root.clone();
            let path = photocraft_engine::defold_live::path(d).ok()?;
            let (w, h) = preview_dimensions(d.width, d.height, ctx.input(|i| i.max_texture_side));
            let pending = std::sync::Arc::new(std::sync::Mutex::new(None));
            let output = pending.clone();
            let repaint = ctx.clone();
            std::thread::spawn(move || {
                let image = photocraft_engine::defold_live::preview(&root, &path, w, h)
                    .map_err(|e| e.to_string())
                    .and_then(|b| {
                        photocraft_codecs::decode_with(
                            &b,
                            &photocraft_codecs::DecodeOptions {
                                limits: photocraft_codecs::Limits {
                                    max_width: 4096,
                                    max_height: 4096,
                                    max_pixels: 16 * 1024 * 1024,
                                    max_alloc: 128 * 1024 * 1024,
                                },
                                ..Default::default()
                            },
                        )
                        .map_err(|e| e.to_string())
                    })
                    .map(|im| {
                        let im = im.convert(photocraft_codecs::ChannelLayout::Rgba, photocraft_codecs::SampleType::U8);
                        egui::ColorImage::from_rgba_unmultiplied([im.width() as usize, im.height() as usize], im.data())
                    });
                *output.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(image);
                repaint.request_repaint();
            });
            frame = Some(EngineFrame { revision: d.revision, texture: None, pending, finished: false });
        }
        let texture = frame.as_ref().filter(|f| f.revision == d.revision).and_then(|f| f.texture.clone());
        if let Some(frame) = frame {
            ctx.data_mut(|m| m.insert_temp(key, frame));
        }
        texture
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (app, ctx);
        None
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn preview_dimensions(width: f32, height: f32, max_side: usize) -> (u32, u32) {
    let max = max_side.clamp(1, 4096) as f32;
    let width = if width.is_finite() { width.max(1.0) } else { 1.0 };
    let height = if height.is_finite() { height.max(1.0) } else { 1.0 };
    let scale = (max / width.max(height)).min(1.0);
    ((width * scale).floor().max(1.0) as u32, (height * scale).floor().max(1.0) as u32)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[test]
fn defold_preview_respects_gpu_texture_limits() {
    assert_eq!(preview_dimensions(2340.0, 1080.0, 2048), (2048, 945));
    assert_eq!(preview_dimensions(960.0, 640.0, 2048), (960, 640));
    assert_eq!(preview_dimensions(f32::NAN, 0.0, 0), (1, 1));
    let (w, h) = preview_dimensions(100_000.0, 100_000.0, 16_384);
    assert_eq!((w, h), (4096, 4096));
}
