//! Defold is a document attachment, never a replacement toolbar/canvas/layer panel.
use crate::{PhotocraftApp, widgets};
use photocraft_defold_format::{PropertyKind, Scene};
use serde_json::{Value, json};

pub fn panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    if app.session.active_gui().is_some() {
        crate::defold_ui::properties(app, ui);
    } else if !properties(app, ui) {
        ui.label(tl!("Select a Defold GUI node to edit its properties"));
    }
}

pub fn sync_panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let Some((doc_id, link)) = app.session.active().and_then(|st| st.doc.defold.as_ref().map(|link| (st.doc.id.0, link.clone()))) else {
        ui.label(tl!("Open a linked Defold GUI to synchronize"));
        return;
    };
    let status_id = egui::Id::new(("defold-sync-status", doc_id));
    let width = ui.available_width();
    if ui.add_enabled_ui(app.session.is_enabled("defold.syncDesign"), |ui| widgets::primary_button(ui, tl!("Sync to Defold"), width)).inner.clicked() {
        let status = match app.run("defold.syncDesign", json!({})) {
            Ok(result) if result["editorSaveRequired"] == true => tl!("Synced to Defold; save the GUI in Defold").to_owned(),
            Ok(_) => tl!("GUI and artwork synchronized").to_owned(),
            Err(error) => error,
        };
        app.ui.status.clone_from(&status);
        ui.ctx().data_mut(|data| data.insert_temp(status_id, status));
    }
    if let Some(status) = ui.ctx().data(|data| data.get_temp::<String>(status_id)) {
        ui.add(egui::Label::new(status).wrap());
    } else {
        ui.add(egui::Label::new(tl!("Edits stay local until you sync")).wrap());
    }
    ui.add_space(8.0);
    let resource = std::path::Path::new(&link.gui).strip_prefix(&link.root).unwrap_or_else(|_| std::path::Path::new(&link.gui));
    ui.strong(tl!("GUI resource"));
    ui.add(egui::Label::new(format!("/{}", resource.to_string_lossy().replace('\\', "/"))).wrap()).on_hover_text(&link.gui);
    ui.collapsing(tl!("Defold Project"), |ui| { ui.add(egui::Label::new(&link.root).wrap()); });
    ui.add_space(8.0);
    ui.separator();
    ui.strong(tl!("GUI nodes"));
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(app.session.is_enabled("defold.addDesignNode"), egui::Button::new(tl!("Add child node"))).clicked() {
            let _ = app.run("defold.addDesignNode", json!({"child":true}));
        }
        if ui.add_enabled(app.session.is_enabled("defold.addDesignNode"), egui::Button::new(tl!("Add GUI node"))).clicked() {
            let _ = app.run("defold.addDesignNode", json!({"child":false}));
        }
    });
    if ui.button(tl!("Defold GUI Properties")).clicked() {
        let _ = app.run("window.panel.defoldProperties", json!({}));
    }
}

fn number(ui: &mut egui::Ui, key: &str, base: f64, width: f32, range: std::ops::RangeInclusive<f32>) -> Option<Value> {
    let id = ui.id().with(key);
    let mut value = ui.ctx().data(|d| d.get_temp::<(f64, f32)>(id)).filter(|(v, _)| *v == base).map_or(base as f32, |(_, v)| v);
    let r = widgets::value_field_prec(ui, &mut value, range, "", width.max(30.0), 2);
    ui.ctx().data_mut(|d| d.insert_temp(id, (base, value)));
    ((r.drag_stopped() || (r.changed() && !r.dragged())) && (value as f64 - base).abs() > 1e-7).then(|| json!(value))
}
fn string(ui: &mut egui::Ui, key: &str, base: &str, multiline: bool) -> Option<Value> {
    let id = ui.id().with(key);
    let mut draft = ui.ctx().data(|d| d.get_temp::<(String, String)>(id)).filter(|(b, _)| b == base).map_or_else(|| base.to_string(), |(_, v)| v);
    let r = if multiline {
        ui.add(egui::TextEdit::multiline(&mut draft).desired_rows(3).desired_width(ui.available_width()))
    } else {
        ui.add(egui::TextEdit::singleline(&mut draft).desired_width(ui.available_width()))
    };
    ui.ctx().data_mut(|d| d.insert_temp(id, (base.to_string(), draft.clone())));
    (r.lost_focus() && draft != base).then(|| json!(draft))
}
fn value(scene: &Scene, node: &str, key: &str) -> Value {
    scene.property(node, key).unwrap_or(Value::Null)
}
fn enum_label(value: &str) -> String {
    let short = ["BLEND_MODE_", "PIVOT_", "XANCHOR_", "YANCHOR_", "ADJUST_MODE_", "SIZE_MODE_", "CLIPPING_MODE_", "PIEBOUNDS_"]
        .iter()
        .find_map(|prefix| value.strip_prefix(prefix))
        .unwrap_or(value);
    if short.len() <= 2 {
        return short.into();
    }
    short
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or(String::new(), |c| format!("{}{}", c, chars.as_str().to_lowercase()))
        })
        .collect::<Vec<_>>()
        .join(" ")
}
fn color(ui: &mut egui::Ui, scene: &Scene, node: &str, key: &str, out: &mut serde_json::Map<String, Value>) {
    let mut rgb: [u8; 3] = std::array::from_fn(|i| {
        let component = ["x", "y", "z"].get(i).copied().unwrap_or("x");
        (value(scene, node, &format!("{key}.{component}")).as_f64().unwrap_or(0.0) * 255.0).round().clamp(0.0, 255.0) as u8
    });
    let hex = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.allocate_ui(egui::vec2((ui.available_width() - 35.0).max(40.0), 24.0), |ui| {
            if let Some(v) = string(ui, key, &hex, false)
                && let Some(v) = v.as_str().and_then(|s| u32::from_str_radix(s.trim_start_matches('#'), 16).ok())
            {
                rgb = [((v >> 16) & 255) as u8, ((v >> 8) & 255) as u8, (v & 255) as u8];
                changed = true;
            }
        });
        changed |= ui.color_edit_button_srgb(&mut rgb).changed();
    });
    if changed {
        for (k, c) in ["x", "y", "z"].into_iter().zip(rgb) {
            out.insert(format!("{key}.{k}"), json!(c as f64 / 255.0));
        }
    }
}
pub fn properties(app: &mut PhotocraftApp, ui: &mut egui::Ui) -> bool {
    let Some(binding) = photocraft_engine::defold_design::selected(&app.session).cloned() else { return false };
    let Ok(scene) = photocraft_engine::defold_design::scene(&app.session) else { return false };
    let Some(node) = scene.nodes.iter().find(|n| n.id == binding.node) else { return false };
    let mut updates = serde_json::Map::new();
    let schema = photocraft_defold_format::properties(&node.kind);
    let mut rows = vec![
        ("id", tl!("Id")),
        ("position", tl!("Position")),
        ("rotation", tl!("Rotation")),
        ("scale", tl!("Scale")),
        ("size", tl!("Size")),
        ("enabled", tl!("Enabled")),
        ("visible", tl!("Visible")),
    ];
    if node.kind == "TYPE_TEXT" {
        rows.extend([("text", tl!("Text")), ("line_break", tl!("Line break")), ("font", tl!("Font"))]);
    }
    if matches!(node.kind.as_str(), "TYPE_BOX" | "TYPE_PIE") {
        rows.push(("texture", tl!("Texture")));
    }
    rows.extend([("material", tl!("Material")), ("color", tl!("Color")), ("alpha", tl!("Opacity")), ("inherit_alpha", tl!("Inherit alpha"))]);
    if node.kind == "TYPE_TEXT" {
        rows.extend([
            ("text_leading", tl!("Text leading")),
            ("text_tracking", tl!("Text tracking")),
            ("outline", tl!("Outline")),
            ("outline_alpha", tl!("Outline opacity")),
            ("shadow", tl!("Shadow")),
            ("shadow_alpha", tl!("Shadow opacity")),
        ]);
    }
    rows.extend([
        ("layer", tl!("Layer")),
        ("blend_mode", tl!("Blend Mode")),
        ("pivot", tl!("Pivot")),
        ("xanchor", tl!("X anchor")),
        ("yanchor", tl!("Y anchor")),
        ("adjust_mode", tl!("Adjust mode")),
    ]);
    for (key, label) in [
        ("size_mode", tl!("Size mode")),
        ("slice9", tl!("Nine slice")),
        ("clipping_mode", tl!("Clipping mode")),
        ("clipping_visible", tl!("Clipping visible")),
        ("clipping_inverted", tl!("Clipping inverted")),
        ("outerBounds", tl!("Outer bounds")),
        ("innerRadius", tl!("Inner radius")),
        ("perimeterVertices", tl!("Perimeter vertices")),
        ("pieFillAngle", tl!("Fill angle")),
        ("particlefx", tl!("Particle FX")),
        ("template", tl!("Template")),
    ] {
        if schema.iter().any(|p| p.path == key || p.path.starts_with(&format!("{key}."))) {
            rows.push((key, label));
        }
    }
    ui.push_id(("defold-native-properties", binding.layer), |ui| {
        egui::Grid::new("fields").num_columns(2).spacing(egui::vec2(8.0, 6.0)).show(ui, |ui| {
            for (key, label) in rows {
                ui.label(label);
                ui.push_id(key, |ui| {
                    let width = (ui.available_width()).max(120.0);
                    ui.allocate_ui(egui::vec2(width, 22.0), |ui| {
                        if matches!(key, "position" | "rotation" | "scale" | "size" | "slice9") {
                            ui.horizontal(|ui| {
                                let components = if key == "slice9" { "xyzw" } else { "xyz" };
                                let w = ((width - 18.0 * components.len() as f32) / components.len() as f32 - 4.0).max(28.0);
                                for c in components.chars() {
                                    ui.label(c.to_ascii_uppercase().to_string());
                                    let path = format!("{key}.{c}");
                                    let min = if key == "size" || key == "slice9" { 0.0 } else { -1_000_000.0 };
                                    if let Some(v) = number(ui, &path, value(&scene, &node.id, &path).as_f64().unwrap_or(0.0), w, min..=1_000_000.0) {
                                        updates.insert(path, v);
                                    }
                                }
                            });
                            return;
                        }
                        if matches!(key, "color" | "outline" | "shadow") {
                            color(ui, &scene, &node.id, key, &mut updates);
                            return;
                        }
                        if key == "id" {
                            if let Some(v) = string(ui, key, &node.id, false) {
                                updates.insert(key.into(), v);
                            }
                            return;
                        }
                        let v = value(&scene, &node.id, key);
                        if let Some(p) = schema.iter().find(|p| p.path == key)
                            && let PropertyKind::Enum(choices) = &p.kind
                        {
                            egui::ComboBox::from_id_salt(key).width(width).selected_text(enum_label(v.as_str().unwrap_or_default())).show_ui(ui, |ui| {
                                for choice in *choices {
                                    if ui.selectable_label(v.as_str() == Some(*choice), enum_label(choice)).clicked() {
                                        updates.insert(key.into(), json!(choice));
                                    }
                                }
                            });
                            return;
                        }
                        if matches!(key, "font" | "material" | "texture" | "layer") {
                            ui.horizontal(|ui| {
                                egui::ComboBox::from_id_salt(key).width((width - 30.0).max(60.0)).selected_text(v.as_str().unwrap_or_default()).show_ui(
                                    ui,
                                    |ui| {
                                        for choice in photocraft_engine::defold_design::resource_choices(&app.session, key).unwrap_or_default() {
                                            if ui.selectable_label(v.as_str() == Some(choice.as_str()), if choice.is_empty() { "—" } else { &choice }).clicked()
                                            {
                                                updates.insert(key.into(), json!(choice));
                                            }
                                        }
                                    },
                                );
                                if key != "layer" && ui.small_button("↗").on_hover_text(tl!("Open resource in Defold")).clicked() {
                                    let _ = app.run("defold.openDesignResource", json!({"property":key}));
                                }
                            });
                            return;
                        }
                        match v {
                            Value::Bool(mut b) => {
                                if ui.checkbox(&mut b, "").changed() {
                                    updates.insert(key.into(), json!(b));
                                }
                            }
                            Value::Number(n) => {
                                let base = n.as_f64().unwrap_or(0.0);
                                ui.horizontal(|ui| {
                                    if let Some(v) = number(
                                        ui,
                                        key,
                                        base,
                                        if key.ends_with("alpha") { 54.0 } else { width },
                                        if key.ends_with("alpha") { 0.0..=1.0 } else { -1_000_000.0..=1_000_000.0 },
                                    ) {
                                        updates.insert(key.into(), v);
                                    }
                                    if key.ends_with("alpha") {
                                        let mut alpha = base as f32;
                                        if widgets::slider(ui, &mut alpha, 0.0..=1.0, None).changed() {
                                            updates.insert(key.into(), json!(alpha));
                                        }
                                    }
                                });
                            }
                            _ => {
                                if let Some(v) = string(ui, key, v.as_str().unwrap_or_default(), key == "text") {
                                    updates.insert(key.into(), v);
                                }
                            }
                        }
                    });
                });
                ui.end_row();
            }
        });
    });
    if !updates.is_empty() {
        let _ = app.run("defold.setDesignProperties", json!({"values":updates}));
    }
    true
}
