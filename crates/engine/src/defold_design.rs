//! GUI assets as ordinary PhotoCraft documents. Only explicit publication touches Defold.
use crate::{Result, Session, commands::CommandSpec, defold_cmds::err};
use photocraft_algo::transform::{Homography, Interp};
use photocraft_defold_format::{Node, Scene};
use photocraft_doc::{
    defold::{Design, NodeLink},
    *,
};
use photocraft_geom::Point;
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path, sync::Arc};

fn string<'a>(p: &'a Value, key: &str) -> Result<&'a str> {
    p.get(key).and_then(Value::as_str).ok_or_else(|| err(format!("Missing {key}")))
}
fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}
pub fn linked(s: &Session) -> bool {
    s.active().is_some_and(|st| st.doc.defold.is_some())
}
fn enabled(s: &Session) -> std::result::Result<(), String> {
    if linked(s) { Ok(()) } else { Err("Open a linked GUI design".into()) }
}
pub fn node_for_layer(doc: &Document, id: LayerId) -> Option<&NodeLink> {
    let design = doc.defold.as_ref()?;
    let path = doc.path_of(id)?;
    (1..=path.len()).rev().find_map(|len| {
        let layer = doc.layer_at(path.get(..len)?)?;
        design.nodes.iter().find(|n| n.layer == layer.id.0)
    })
}
pub fn selected(s: &Session) -> Option<&NodeLink> {
    let st = s.active()?;
    node_for_layer(&st.doc, st.active_layer?)
}
pub fn scene(s: &Session) -> Result<Scene> {
    let doc = &s.active().ok_or_else(|| err("No document"))?.doc;
    let link = doc.defold.as_ref().ok_or_else(|| err("Not a GUI design"))?;
    let mut scene = Scene::parse(link.source.clone()).map_err(err)?;
    for b in &link.nodes {
        if let Some(LayerContent::Text(t)) = doc.layer(LayerId(b.artwork)).map(|l| &l.content)
            && scene.nodes.iter().any(|n| n.id == b.node && n.kind == "TYPE_TEXT" && n.text != t.text)
        {
            scene = scene.edit(&b.node, "text", &json!(t.text)).map_err(err)?;
        }
    }
    Ok(scene)
}
fn matches_nodes(a: &Scene, b: &Scene) -> bool {
    a.nodes.len() == b.nodes.len()
        && a.nodes.iter().zip(&b.nodes).all(|(a_node, b_node)| {
            a_node.id == b_node.id
                && a_node.parent == b_node.parent
                && a_node.kind == b_node.kind
                && photocraft_defold_format::properties(&a_node.kind).iter().all(|p| {
                    let left = a.property(&a_node.id, &p.path).ok();
                    let right = b.property(&b_node.id, &p.path).ok();
                    match (left.as_ref().and_then(Value::as_f64), right.as_ref().and_then(Value::as_f64)) {
                        // Defold stores GUI vectors as protobuf floats. Compare their
                        // representable values, not the decimal spelling of the file.
                        (Some(x), Some(y)) => (x - y).abs() < 1e-5 || ((x as f32).is_finite() && x as f32 == y as f32),
                        _ => left == right,
                    }
                })
        })
}
pub fn resource_choices(s: &Session, key: &str) -> Result<Vec<String>> {
    let scene = scene(s)?;
    let root = &s.active().and_then(|s| s.doc.defold.as_ref()).ok_or_else(|| err("No GUI"))?.root;
    let collection = match key {
        "font" => "fonts",
        "texture" => "textures",
        "layer" => "layers",
        _ => "materials",
    };
    let mut choices = vec![String::new()];
    for f in scene.proto.fields.iter().filter(|f| f.name == collection) {
        let name = scene.proto.text(&f.children, "name");
        if key != "texture" {
            choices.push(name);
            continue;
        }
        let path = scene.proto.text(&f.children, "texture");
        let Ok(path) = crate::defold_cmds::resource(root, &path) else { continue };
        let Ok(bytes) = crate::file_cmds::read_file(&path.to_string_lossy()) else { continue };
        let Ok(raw) = String::from_utf8(bytes) else { continue };
        let Ok(atlas) = photocraft_defold_format::TextProto::parse(raw) else { continue };
        for image in atlas.fields.iter().filter(|f| f.name == "images") {
            let path = atlas.text(&image.children, "image");
            if let Some(stem) = Path::new(&path).file_stem() {
                choices.push(format!("{name}/{}", stem.to_string_lossy()));
            }
        }
    }
    choices.extend(scene.nodes.iter().map(|n| scene.property(&n.id, key).unwrap_or_default()).filter_map(|v| v.as_str().map(str::to_string)));
    choices.sort();
    choices.dedup();
    Ok(choices)
}
fn open_resource(s: &mut Session, p: &Value) -> Result<Value> {
    let key = string(p, "property")?;
    let scene = scene(s)?;
    let node = selected(s).ok_or_else(|| err("Select a node"))?;
    let value = scene.property(&node.node, key).map_err(err)?;
    let value = value.as_str().ok_or_else(|| err("No resource"))?;
    let collection = match key {
        "texture" => "textures",
        "font" => "fonts",
        "material" => "materials",
        _ => return Err(err("Not a resource property")),
    };
    let alias = if key == "texture" { value.split('/').next().unwrap_or_default() } else { value };
    let field = scene
        .proto
        .fields
        .iter()
        .find(|f| f.name == collection && scene.proto.text(&f.children, "name") == alias)
        .ok_or_else(|| err("Resource is not assigned"))?;
    let path = scene.proto.text(&field.children, key);
    let root = &s.active().and_then(|s| s.doc.defold.as_ref()).ok_or_else(|| err("No GUI"))?.root;
    let gui = resource_path(root, &s.active().and_then(|s| s.doc.defold.as_ref()).ok_or_else(|| err("No GUI"))?.gui)?;
    crate::defold_live::request(root, &json!({"op":"openResource","path":gui,"resource":path}))?;
    Ok(Value::Null)
}
fn pivot(n: &Node) -> [f64; 2] {
    match n.pivot.as_str() {
        "PIVOT_N" => [0.5, 1.0],
        "PIVOT_NE" => [1.0, 1.0],
        "PIVOT_E" => [1.0, 0.5],
        "PIVOT_SE" => [1.0, 0.0],
        "PIVOT_S" => [0.5, 0.0],
        "PIVOT_SW" => [0.0, 0.0],
        "PIVOT_W" => [0.0, 0.5],
        "PIVOT_NW" => [0.0, 1.0],
        _ => [0.5, 0.5],
    }
}
fn local(n: &Node) -> Affine {
    Affine::translate(n.position[0], n.position[1])
        .mul(&Affine::rotate(n.rotation[2].to_radians()))
        .mul(&Affine { m: [n.scale[0], 0.0, 0.0, n.scale[1], 0.0, 0.0] })
}
fn world(scene: &Scene, id: &str) -> Result<Affine> {
    let mut result = Affine::IDENTITY;
    let mut current = id;
    for _ in 0..128 {
        if current.is_empty() {
            return Ok(result);
        }
        let n = scene.nodes.iter().find(|n| n.id == current).ok_or_else(|| err("Missing GUI parent"))?;
        result = local(n).mul(&result);
        current = &n.parent;
    }
    Err(err("GUI hierarchy exceeds 128 levels"))
}
fn screen(height: u32) -> Affine {
    Affine { m: [1.0, 0.0, 0.0, -1.0, 0.0, height as f64] }
}
fn document_screen(doc: &Document) -> Affine {
    let [x, y] = doc.defold.as_ref().map_or([0, 0], |d| d.canvas_origin);
    Affine { m: [1.0, 0.0, 0.0, -1.0, -f64::from(x), f64::from(doc.size.height) + f64::from(y)] }
}
fn transform(layer: &mut Layer, a: Affine) -> Result<()> {
    let [a0, b, c, d, e, f] = a.m;
    crate::transform_cmds::transform_layer(None, Locks::default(), layer, &Homography([a0, c, e, b, d, f, 0.0, 0.0, 1.0]), Some(a), Interp::Bilinear)?;
    // Text's cache is normally refreshed by the text tool; imports/property edits use the same renderer.
    fn text_cache(l: &mut Layer) {
        match &mut l.content {
            LayerContent::Text(t) => {
                photocraft_text::shared().lock().unwrap_or_else(std::sync::PoisonError::into_inner).render_layer(t, 72.0, PixelFormat::RGBA8);
            }
            LayerContent::Group(g) => {
                for c in &mut g.children {
                    text_cache(c);
                }
            }
            _ => {}
        }
    }
    text_cache(layer);
    Ok(())
}
fn own_layer(doc: &Document, binding: &NodeLink) -> Result<Layer> {
    let mut layer = doc.layer(LayerId(binding.layer)).ok_or_else(|| err("Node layer was deleted"))?.clone();
    let nodes = &doc.defold.as_ref().ok_or_else(|| err("Missing link"))?.nodes;
    fn strip(l: &mut Layer, nodes: &[NodeLink]) {
        if let LayerContent::Group(g) = &mut l.content {
            g.children.retain(|c| !nodes.iter().any(|n| n.layer == c.id.0));
            for c in &mut g.children {
                strip(c, nodes);
            }
        }
    }
    strip(&mut layer, nodes);
    if let Some(path) = doc.path_of(LayerId(binding.layer)) {
        for depth in (1..path.len()).rev() {
            if let Some(parent) = path.get(..depth).and_then(|p| doc.layer_at(p)) {
                let mut parent = parent.clone();
                if let LayerContent::Group(group) = &mut parent.content {
                    group.children = vec![layer];
                    layer = parent;
                } else {
                    return Err(err("Node ancestor is not a group"));
                }
            }
        }
    }
    Ok(layer)
}
fn rendered(doc: &Document, layer: Layer) -> Result<photocraft_compose::Buffer> {
    let mut isolated = doc.clone();
    isolated.layers = vec![layer];
    let l = isolated.layers.first().ok_or_else(|| err("Missing artwork"))?;
    // Canvas dimensions do not bound an individual GUI asset. Keep off-canvas pixels
    // for publication, including nodes that will be moved into view at runtime.
    // Version 1 retains its old hashing rules until migration has checked dirty artwork.
    let canvas = if doc.defold.as_ref().is_some_and(|d| d.version >= 2) {
        fn margin(l: &Layer, depth: usize) -> Result<i32> {
            if depth > 128 {
                return Err(err("Layer nesting limit"));
            }
            let mut child_margin = 0;
            if let LayerContent::Group(g) = &l.content {
                for c in &g.children {
                    child_margin = child_margin.max(margin(c, depth + 1)?);
                }
            }
            Ok(photocraft_compose::effects::margin(l).saturating_add(child_margin).min(16384))
        }
        photocraft_compose::layer_bounds(l, doc.bounds()).union(&doc.bounds()).inflate(margin(l, 0)?)
    } else {
        doc.bounds()
    };
    let mut bounds = photocraft_compose::composite_bounds(l, canvas).unwrap_or(canvas);
    if bounds.is_empty() {
        bounds = Rect::new(0, 0, 1, 1);
    }
    if bounds.width() > 8192 || bounds.height() > 8192 || u64::from(bounds.width()) * u64::from(bounds.height()) > 16 * 1024 * 1024 {
        return Err(err("Node artwork exceeds 8192px / 16 megapixels"));
    }
    Ok(photocraft_compose::render(&isolated, bounds))
}
fn appearance(doc: &Document, binding: &NodeLink) -> Result<String> {
    let rendered = rendered(doc, own_layer(doc, binding)?)?;
    let mut h = blake3::Hasher::new();
    for v in [rendered.rect.x0, rendered.rect.y0, rendered.rect.x1, rendered.rect.y1] {
        h.update(&v.to_le_bytes());
    }
    for pixel in rendered.px {
        for value in pixel {
            h.update(&value.to_le_bytes());
        }
    }
    Ok(h.finalize().to_hex().to_string())
}
fn native_path(gui: &str) -> String {
    format!("{gui}.pcraft")
}

// Defold's default layout uses the project display, never a root-node heuristic.
fn linked_design_size(doc: &Document) -> Result<Size> {
    let link = doc.defold.as_ref().ok_or_else(|| err("No GUI link"))?;
    crate::defold_cmds::project_size(&link.root)
}

/// Restore a native GUI source using the Defold project reference frame.
/// Moving the coordinate origin must move pixels, masks and geometry together without resampling.
pub fn prepare_open(doc: &mut Document) -> Result<Vec<String>> {
    let Some(link) = &doc.defold else {
        return Ok(vec![]);
    };
    if !(1..=3).contains(&link.version) {
        return Err(err("Unsupported GUI design version"));
    }
    let size = match linked_design_size(doc) {
        Ok(size) => size,
        Err(e) => return Ok(vec![format!("Cannot verify the GUI canvas against game.project; keeping the saved source: {e}")]),
    };
    fit_canvas(doc, size)
}
// Keep the reference frame fixed. The UI fits/displays scene_bounds independently.
fn fit_canvas(doc: &mut Document, reference: Size) -> Result<Vec<String>> {
    let (_, mut warnings) = scene_bounds(doc)?;
    if normalize_frame(doc, reference, [0, 0])? {
        warnings.push(format!(
            "GUI reference frame: {} x {}. Off-frame artwork is preserved in the scene; source node coordinates are unchanged.",
            reference.width, reference.height
        ));
    }
    Ok(warnings)
}

/// Bounded camera/display extent, never the document's design resolution.
pub fn scene_bounds(doc: &Document) -> Result<(Rect, Vec<String>)> {
    let link = doc.defold.as_ref().ok_or_else(|| err("No GUI link"))?;
    let mut bounds = doc.bounds();
    let mut warnings = Vec::new();
    let mut layers = link.nodes.iter().filter(|b| doc.layer(LayerId(b.layer)).is_some()).map(|b| own_layer(doc, b)).collect::<Result<Vec<_>>>()?;
    layers.extend(doc.layers.iter().filter(|l| !link.nodes.iter().any(|b| b.layer == l.id.0)).cloned());
    for mut layer in layers {
        fn visible(l: &mut Layer, depth: usize) -> Result<bool> {
            if depth > 128 {
                return Err(err("Layer nesting limit"));
            }
            if !l.visible || l.opacity <= 0.0 {
                return Ok(false);
            }
            if let LayerContent::Group(g) = &mut l.content {
                let mut children = Vec::new();
                for mut c in std::mem::take(&mut g.children) {
                    if visible(&mut c, depth + 1)? {
                        children.push(c);
                    }
                }
                g.children = children;
            }
            Ok(true)
        }
        if !visible(&mut layer, 0)? {
            continue;
        }
        let envelope = photocraft_compose::layer_bounds(&layer, doc.bounds()).union(&doc.bounds());
        let Some(r) = photocraft_compose::composite_bounds(&layer, envelope) else { continue };
        if r.is_empty() {
            continue;
        }
        let next = bounds.union(&r);
        if next.width() > 16384 || next.height() > 16384 || u64::from(next.width()) * u64::from(next.height()) > 64 * 1024 * 1024 {
            warnings.push(format!("{} is too far outside the GUI reference frame to fit safely; its pixels remain preserved off canvas.", layer.name));
            continue;
        }
        bounds = next;
    }
    Ok((bounds, warnings))
}
#[cfg(test)]
fn normalize_canvas(doc: &mut Document, size: Size) -> Result<bool> {
    normalize_frame(doc, size, [0, 0])
}
fn normalize_frame(doc: &mut Document, size: Size, origin: [i32; 2]) -> Result<bool> {
    let link = doc.defold.as_ref().ok_or_else(|| err("No GUI link"))?;
    if !(1..=3).contains(&link.version) {
        return Err(err("Unsupported GUI design version"));
    }
    if link.version == 3 && doc.size == size && link.canvas_origin == origin {
        return Ok(false);
    }
    let bindings = link.nodes.clone();
    let clean = bindings
        .iter()
        .map(|b| Ok(doc.layer(LayerId(b.layer)).is_some() && !b.appearance.is_empty() && appearance(doc, b)? == b.appearance))
        .collect::<Result<Vec<_>>>()?;
    let dx = link.canvas_origin[0].checked_sub(origin[0]).ok_or_else(|| err("Invalid GUI canvas origin"))?;
    let dy = i32::try_from(size.height)
        .ok()
        .zip(i32::try_from(doc.size.height).ok())
        .and_then(|(next, old)| next.checked_sub(old))
        .and_then(|delta| delta.checked_add(origin[1]))
        .and_then(|delta| delta.checked_sub(link.canvas_origin[1]))
        .ok_or_else(|| err("Invalid GUI canvas height"))?;
    crate::image_cmds::translate_doc(doc, "defold.openDesign", dx, dy)?;
    for group in &mut doc.ai_generations {
        if group.canvas != doc.size {
            continue;
        }
        let area = group.area.translate(dx, dy);
        // History outside the new viewport stays available for version selection,
        // while in-viewport generations can continue at the corrected coordinates.
        if area.intersect(&Rect::from_size(size)) != area {
            continue;
        }
        if let Some(selection) = &group.selection {
            let bounds = selection.content_bounds();
            if bounds.x0.checked_add(dx).is_none()
                || bounds.x1.checked_add(dx).is_none()
                || bounds.y0.checked_add(dy).is_none()
                || bounds.y1.checked_add(dy).is_none()
            {
                return Err(err("Generation selection exceeds coordinate range"));
            }
            group.selection = Some(photocraft_algo::resample::translate_surface(selection, dx, dy));
        }
        group.area = area;
        group.frame = group.frame.map(|f| f.translate(dx, dy));
        group.canvas = size;
    }
    doc.size = size;
    if let Some(link) = &mut doc.defold {
        link.version = 3;
        link.canvas_origin = origin;
    }
    for (binding, pristine) in bindings.iter().zip(clean) {
        let stamp = if pristine { appearance(doc, binding)? } else { String::new() };
        if let Some(link) = &mut doc.defold
            && let Some(node) = link.nodes.iter_mut().find(|n| n.layer == binding.layer)
        {
            node.appearance = stamp;
        }
    }
    Ok(true)
}
fn font(scene: &Scene, id: &str, root: &str) -> (String, f32) {
    let alias = scene.property(id, "font").ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let path = scene
        .proto
        .fields
        .iter()
        .find(|f| f.name == "fonts" && scene.proto.text(&f.children, "name") == alias)
        .map(|f| scene.proto.text(&f.children, "font"))
        .unwrap_or_default();
    let read = |path: &str| crate::defold_cmds::resource(root, path).and_then(|p| crate::file_cmds::read_file(&p.to_string_lossy()));
    let raw = read(&path).ok().and_then(|b| String::from_utf8(b).ok()).and_then(|s| photocraft_defold_format::TextProto::parse(s).ok());
    let Some(proto) = raw else { return ("Arial".into(), 14.0) };
    let size = proto.scalar(&proto.fields, "size", "14").parse::<f32>().ok().filter(|n| n.is_finite()).unwrap_or(14.0).clamp(0.1, 1296.0);
    let family = read(&proto.text(&proto.fields, "font"))
        .ok()
        .filter(|b| b.len() < 32 * 1024 * 1024)
        .and_then(|b| photocraft_text::shared().lock().unwrap_or_else(std::sync::PoisonError::into_inner).fonts.register_font_data(b).into_iter().next())
        .unwrap_or_else(|| "Arial".into());
    (family, size)
}
fn text_style(t: &mut TextLayer, scene: &Scene, id: &str) {
    let number = |k: &str, default: f64| scene.property(id, k).ok().and_then(|v| v.as_f64()).unwrap_or(default) as f32;
    t.runs = vec![text::TextRun {
        len: t.text.len(),
        style: text::CharStyle {
            font_family: t.font_family.clone(),
            size_pt: t.size_pt,
            color: t.color,
            tracking: number("text_tracking", 0.0) * 1000.0,
            leading_pt: Some(t.size_pt * number("text_leading", 1.0)),
            ..Default::default()
        },
    }];
    t.paragraphs.clear();
    if scene.property(id, "line_break").ok() == Some(json!(true)) {
        t.shape = text::TextShape::Box { x: 0.0, y: 0.0, width: number("size.x", 1.0).max(1.0), height: number("size.y", 1.0).max(1.0) };
    } else {
        t.shape = text::TextShape::Point;
    }
}
fn alpha(scene: &Scene, id: &str) -> Result<f32> {
    let mut id = id;
    let mut alpha = 1.0;
    for _ in 0..128 {
        let Some(n) = scene.nodes.iter().find(|n| n.id == id) else { return Ok(alpha) };
        alpha *= n.alpha as f32;
        if scene.property(id, "inherit_alpha").map_err(err)? != true || n.parent.is_empty() {
            return Ok(alpha);
        }
        id = &n.parent;
    }
    Err(err("GUI alpha hierarchy limit"))
}
fn update(s: &mut Session, p: &Value) -> Result<Value> {
    let binding = selected(s).cloned().ok_or_else(|| err("Select a linked node layer"))?;
    let values = p.get("values").and_then(Value::as_object).ok_or_else(|| err("Expected property values"))?.clone();
    if values.len() > 64 {
        return Err(err("Too many properties"));
    }
    let old = scene(s)?;
    let clean: Vec<u64> = {
        let doc = &s.active().ok_or_else(|| err("No document"))?.doc;
        doc.defold
            .as_ref()
            .map(|d| d.nodes.iter().filter(|n| appearance(doc, n).is_ok_and(|h| h == n.appearance)).map(|n| n.layer).collect())
            .unwrap_or_default()
    };
    let mut next = old.clone();
    let mut id = binding.node.clone();
    for (key, value) in &values {
        if key == "id" {
            let name = value.as_str().ok_or_else(|| err("Expected node ID"))?;
            next = next.rename_node(&id, name).map_err(err)?;
            id = name.into();
        } else {
            next = next.edit(&id, key, value).map_err(err)?;
        }
    }
    let before = old.nodes.iter().find(|n| n.id == binding.node).ok_or_else(|| err("Node missing"))?;
    let after = next.nodes.iter().find(|n| n.id == id).ok_or_else(|| err("Node missing"))?.clone();
    let to_screen = document_screen(&s.active().ok_or_else(|| err("No document"))?.doc);
    let from_screen = to_screen.inverse().ok_or_else(|| err("Invalid GUI canvas transform"))?;
    let font_change =
        values.contains_key("font").then(|| font(&next, &id, &s.active().and_then(|s| s.doc.defold.as_ref()).map(|d| d.root.clone()).unwrap_or_default()));
    let replacement = if before.texture != after.texture && !after.texture.is_empty() {
        let link = s.active().and_then(|s| s.doc.defold.as_ref()).ok_or_else(|| err("No link"))?;
        let mut temp = Session::new();
        temp.execute("defold.openGui", json!({"path":link.gui}))?;
        temp.active_gui_mut()?.scene = next.clone();
        let imported = photocraft_io::import("node.png", &crate::defold_cmds::preview_png(&temp, &id)?).map_err(err)?;
        let w = after.size[0].max(1.0);
        let height = after.size[1].max(1.0);
        if w > 8192.0 || height > 8192.0 || w * height > 16.0 * 1024.0 * 1024.0 {
            return Err(err("Texture dimensions exceed pixel budget"));
        }
        let p = pivot(&after);
        let mut art = imported.document.layers.into_iter().next().ok_or_else(|| err("Empty texture"))?;
        transform(
            &mut art,
            Affine {
                m: [
                    w / imported.document.size.width.max(1) as f64,
                    0.0,
                    0.0,
                    height / imported.document.size.height.max(1) as f64,
                    -p[0] * w,
                    -(1.0 - p[1]) * height,
                ],
            },
        )?;
        transform(&mut art, to_screen.mul(&world(&next, &id)?).mul(&Affine { m: [1.0, 0.0, 0.0, -1.0, 0.0, 0.0] }))?;
        Some(art.content)
    } else {
        None
    };
    let a = to_screen
        .mul(&world(&next, &id)?)
        .mul(&world(&old, &binding.node)?.inverse().ok_or_else(|| err("Zero-scale node cannot be transformed"))?)
        .mul(&from_screen);
    let size_changed = before.size != after.size || before.pivot != after.pivot;
    let before = before.clone();
    let result_id = id.clone();
    s.edit("Defold node properties", move |doc, _| {
        let layer = doc.layer_mut(LayerId(binding.layer)).ok_or_else(|| err("Node layer missing"))?;
        transform(layer, a)?;
        if values.contains_key("id") {
            layer.name = id.clone();
        }
        if values.contains_key("enabled") {
            layer.visible = after.enabled;
        }
        if size_changed && before.size[0] > 0.0 && before.size[1] > 0.0 {
            let p0 = pivot(&before);
            let p1 = pivot(&after);
            let resize = Affine {
                m: [after.size[0] / before.size[0], 0.0, 0.0, after.size[1] / before.size[1], (p0[0] - p1[0]) * after.size[0], (p0[1] - p1[1]) * after.size[1]],
            };
            let w = to_screen.mul(&world(&next, &id)?);
            if let Some(art) = doc.layer_mut(LayerId(binding.artwork)) {
                transform(art, w.mul(&resize).mul(&w.inverse().ok_or_else(|| err("Zero scale"))?))?;
            }
        }
        if let Some(art) = doc.layer_mut(LayerId(binding.artwork)) {
            if let Some(content) = replacement {
                art.content = content;
            }
            if values.contains_key("visible") {
                art.visible = after.visible;
            }
            if let LayerContent::Text(t) = &mut art.content {
                if values.contains_key("text") {
                    t.text = after.text.clone();
                }
                if before.color != after.color {
                    t.color = Color::rgb(after.color[0] as f32, after.color[1] as f32, after.color[2] as f32);
                }
                if let Some((family, size)) = font_change {
                    t.font_family = family;
                    t.size_pt = size;
                }
                if values.keys().any(|k| matches!(k.as_str(), "text_leading" | "text_tracking" | "line_break" | "font") || k.starts_with("color.")) {
                    text_style(t, &next, &id);
                }
                photocraft_text::shared().lock().unwrap_or_else(std::sync::PoisonError::into_inner).render_layer(t, 72.0, PixelFormat::RGBA8);
            } else if before.color != after.color
                && let Some(surface) = art.surface_mut()
            {
                let bounds = surface.content_bounds();
                if u64::from(bounds.width()) * u64::from(bounds.height()) > 16 * 1024 * 1024 {
                    return Err(err("Color edit exceeds node pixel budget"));
                }
                let mut pixels = surface.read_region(bounds);
                let channels = surface.channels();
                for pixel in pixels.chunks_mut(channels) {
                    for (i, v) in pixel.iter_mut().enumerate().take(3) {
                        let old = before.color.get(i).copied().unwrap_or(1.0);
                        let new = after.color.get(i).copied().unwrap_or(1.0);
                        *v = if old.abs() > 1e-6 { *v * (new / old) as f32 } else { new as f32 };
                    }
                }
                surface.write_region(bounds, &pixels);
            }
        }
        if let Some(link) = &mut doc.defold {
            link.source = next.proto.source.clone();
            if let Some(b) = link.nodes.iter_mut().find(|b| b.layer == binding.layer) {
                b.node = id.clone();
            }
        }
        // Property-only changes remain authored GUI properties, not a forced rasterization.
        let bindings = doc.defold.as_ref().map(|d| d.nodes.clone()).unwrap_or_default();
        for b in bindings {
            let old_id = if b.layer == binding.layer { binding.node.as_str() } else { b.node.as_str() };
            let previous = alpha(&old, old_id)?;
            let current = alpha(&next, &b.node)?;
            if (previous - current).abs() > f32::EPSILON
                && let Some(art) = doc.layer_mut(LayerId(b.artwork))
            {
                art.opacity = if previous > f32::EPSILON { (art.opacity * current / previous).clamp(0.0, 1.0) } else { current };
            }
            let stamp = appearance(doc, &b)?;
            if let Some(link) = &mut doc.defold
                && let Some(n) = link.nodes.iter_mut().find(|n| n.layer == b.layer)
            {
                // Only reset the changed subtree if its artwork was still pristine before this edit.
                if clean.contains(&b.layer) {
                    n.appearance = stamp;
                    n.text = next.nodes.iter().find(|node| node.id == b.node && node.kind == "TYPE_TEXT").map(|node| node.text.clone());
                }
            }
        }
        Ok(())
    })?;
    Ok(json!({"node":result_id}))
}
fn add_node(s: &mut Session, p: &Value) -> Result<Value> {
    let old = scene(s)?;
    let parent = if p["child"] == true { selected(s).map(|n| n.node.clone()).unwrap_or_default() } else { String::new() };
    let id = (1..=8192).map(|i| format!("node_{i}")).find(|id| !old.nodes.iter().any(|n| n.id == *id)).ok_or_else(|| err("GUI node limit"))?;
    let kind = p.get("kind").and_then(Value::as_str).unwrap_or("TYPE_BOX");
    let next = old.new_node(&id, kind, &parent, None).map_err(err)?;
    let art = Layer::new("Artwork", LayerContent::Raster(Surface::new(PixelFormat::RGBA8)));
    let art_id = art.id;
    let group = Layer::new(&id, LayerContent::Group(Group { children: vec![art], expanded: true, artboard: None }));
    let link = NodeLink { layer: group.id.0, artwork: art_id.0, node: id.clone(), appearance: String::new(), text: None };
    s.edit("Add Defold node", move |doc, active| {
        let parent_layer = doc.defold.as_ref().and_then(|d| d.nodes.iter().find(|n| n.node == parent)).map(|n| LayerId(n.layer));
        if let Some(parent) = parent_layer {
            let layer = doc.layer_mut(parent).ok_or_else(|| err("Parent missing"))?;
            let LayerContent::Group(g) = &mut layer.content else {
                return Err(err("Parent must be a layer group"));
            };
            g.children.push(group);
        } else {
            doc.layers.push(group);
        }
        let d = doc.defold.as_mut().ok_or_else(|| err("Missing GUI link"))?;
        d.source = next.proto.source;
        d.nodes.push(link);
        *active = Some(art_id);
        Ok(())
    })?;
    Ok(json!({"node":id}))
}
fn open(s: &mut Session, p: &Value) -> Result<Value> {
    let mut source = Session::new();
    source.execute("defold.openGui", json!({"path":string(p,"path")?}))?;
    let d = source.active_gui().ok_or_else(|| err("GUI open failed"))?;
    let gui = d.path.clone();
    if let Some(i) = s.documents().iter().position(|st| st.doc.defold.as_ref().is_some_and(|d| d.gui == gui)) {
        s.set_active(i);
        return Ok(json!({"document":i,"restored":true}));
    }
    let raw = crate::file_cmds::read_file(&gui)?;
    let disk_hash = hash(&raw);
    let saved = native_path(&gui);
    if Path::new(&saved).is_file() {
        let mut doc = photocraft_format::load_from_bytes(&crate::file_cmds::read_file(&saved)?).map_err(err)?;
        let link = doc.defold.as_ref().ok_or_else(|| err("Saved design has no GUI link"))?;
        if !(1..=3).contains(&link.version) || link.gui != gui {
            return Err(err("Saved design has an incompatible GUI link"));
        }
        let mut warnings = Vec::new();
        if link.disk_hash != disk_hash {
            let unchanged = if let Some(saved) = &link.remote {
                let expected: crate::defold_live::Snapshot = serde_json::from_str(saved).map_err(err)?;
                crate::defold_live::request(&link.root, &json!({"op":"snapshot","path":resource_path(&link.root,&gui)?}))
                    .is_ok_and(|current| crate::defold_live::same_nodes(&current.nodes, &expected.nodes))
            } else {
                false
            };
            if unchanged {
                if let Some(link) = &mut doc.defold {
                    link.disk_hash = disk_hash;
                }
            } else {
                warnings.push(
                    "GUI changed outside PhotoCraft. Your editable design is preserved; synchronization will stop until the conflict is reconciled."
                        .to_string(),
                );
            }
        }
        warnings.extend(prepare_open(&mut doc)?);
        let i = s.add_document(doc, Some(saved));
        return Ok(json!({"document":i,"restored":true,"warnings":warnings}));
    }
    let mut remote = None;
    let mut warnings = Vec::new();
    if Path::new(&d.root).join(".internal/editor.port").is_file() {
        if let Err(e) = source.execute("defold.connectLive", json!({})) {
            warnings.push(format!("Opening saved GUI: {e}"));
        }
        remote = source.active_gui().and_then(|d| d.live.as_ref()).map(|l| serde_json::to_string(&l.snapshot)).transpose().map_err(err)?;
    }
    let d = source.active_gui().ok_or_else(|| err("GUI closed"))?;
    let scene = d.scene.clone();
    // An authored design board can have a different orientation from the runtime viewport.
    let mut doc = Document::new(
        Path::new(&gui).file_name().unwrap_or_default().to_string_lossy(),
        Size::new(d.width as u32, d.height as u32),
        ColorMode::Rgb,
        SampleType::U8,
    );
    doc.defold =
        Some(Design { version: 3, root: d.root.clone(), gui, source: scene.proto.source.clone(), disk_hash, nodes: vec![], remote, canvas_origin: [0, 0] });
    let mut groups = HashMap::new();
    let mut pixel_budget = 0_u64;
    for n in &scene.nodes {
        let p = pivot(n);
        let mut width = n.size[0].max(1.0);
        let mut height = n.size[1].max(1.0);
        if width > 8192.0
            || height > 8192.0
            || width * height > 16.0 * 1024.0 * 1024.0
            || pixel_budget.saturating_add((width * height) as u64) > 64 * 1024 * 1024
        {
            return Err(err("GUI import exceeds artwork pixel budget"));
        }
        let mut artwork = if n.kind == "TYPE_TEXT" {
            let (family, size) = font(&scene, &n.id, &d.root);
            let mut t = TextLayer {
                text: n.text.clone(),
                font_family: family,
                size_pt: size,
                color: Color::rgb(n.color[0] as f32, n.color[1] as f32, n.color[2] as f32),
                ..Default::default()
            };
            text_style(&mut t, &scene, &n.id);
            t.transform = Affine::translate(-p[0] * width, -(1.0 - p[1]) * height + size as f64);
            Layer::new("Text", LayerContent::Text(t))
        } else {
            let mut layer = if !n.texture.is_empty() {
                match crate::defold_cmds::preview_png(&source, &n.id).and_then(|b| photocraft_io::import("node.png", &b).map_err(err)) {
                    Ok(imported) => {
                        if n.size[0] <= 0.0 {
                            width = imported.document.size.width as f64;
                        }
                        if n.size[1] <= 0.0 {
                            height = imported.document.size.height as f64;
                        }
                        if width > 8192.0
                            || height > 8192.0
                            || width * height > 16.0 * 1024.0 * 1024.0
                            || pixel_budget.saturating_add((width * height) as u64) > 64 * 1024 * 1024
                        {
                            return Err(err("GUI texture exceeds artwork pixel budget"));
                        }
                        let mut layer = imported.document.layers.into_iter().next().ok_or_else(|| err("Texture contains no pixels"))?;
                        transform(
                            &mut layer,
                            Affine {
                                m: [
                                    width / imported.document.size.width.max(1) as f64,
                                    0.0,
                                    0.0,
                                    height / imported.document.size.height.max(1) as f64,
                                    -p[0] * width,
                                    -(1.0 - p[1]) * height,
                                ],
                            },
                        )?;
                        layer
                    }
                    Err(e) => {
                        warnings.push(format!("{}: {e}", n.id));
                        Layer::new("Artwork", LayerContent::Raster(Surface::new(PixelFormat::RGBA8)))
                    }
                }
            } else {
                if width > 8192.0 || height > 8192.0 {
                    return Err(err("GUI node dimensions exceed 8192"));
                }
                let mut surface = Surface::new(PixelFormat::RGBA8);
                if n.kind == "TYPE_BOX" {
                    surface.fill_rect(
                        Rect::new((-p[0] * width) as i32, (-(1.0 - p[1]) * height) as i32, ((1.0 - p[0]) * width) as i32, (p[1] * height) as i32),
                        &[n.color[0] as f32, n.color[1] as f32, n.color[2] as f32, n.color[3] as f32],
                    );
                } else {
                    warnings.push(format!("{}: {} is retained; its native preview is not raster artwork", n.id, n.kind));
                }
                Layer::new("Artwork", LayerContent::Raster(surface))
            };
            layer.name = "Artwork".into();
            layer
        };
        pixel_budget = pixel_budget.saturating_add((width * height) as u64);
        if pixel_budget > 64 * 1024 * 1024 {
            return Err(err("GUI import exceeds 64 megapixels of artwork"));
        }
        transform(&mut artwork, screen(doc.size.height).mul(&world(&scene, &n.id)?).mul(&Affine { m: [1.0, 0.0, 0.0, -1.0, 0.0, 0.0] }))?;
        artwork.opacity = alpha(&scene, &n.id)?;
        artwork.visible = n.visible;
        let artwork_id = artwork.id.0;
        let mut group = Layer::new(&n.id, LayerContent::Group(Group { children: vec![artwork], expanded: true, artboard: None }));
        group.visible = n.enabled;
        let binding = NodeLink {
            layer: group.id.0,
            artwork: artwork_id,
            node: n.id.clone(),
            appearance: String::new(),
            text: (n.kind == "TYPE_TEXT").then(|| n.text.clone()),
        };
        if let Some(link) = &mut doc.defold {
            link.nodes.push(binding);
        }
        groups.insert(n.id.clone(), group);
    }
    fn assemble(id: &str, scene: &Scene, groups: &mut HashMap<String, Layer>, depth: usize) -> Result<Layer> {
        if depth > 128 {
            return Err(err("GUI nesting limit"));
        }
        let mut layer = groups.remove(id).ok_or_else(|| err("Duplicate/cyclic GUI node"))?;
        if let LayerContent::Group(g) = &mut layer.content {
            for child in scene.nodes.iter().filter(|n| n.parent == id) {
                g.children.push(assemble(&child.id, scene, groups, depth + 1)?);
            }
        }
        Ok(layer)
    }
    for n in scene.nodes.iter().filter(|n| n.parent.is_empty()) {
        doc.layers.push(assemble(&n.id, &scene, &mut groups, 0)?);
    }
    if !groups.is_empty() {
        return Err(err("Unresolved GUI hierarchy"));
    }
    let reference = doc.size;
    warnings.extend(fit_canvas(&mut doc, reference)?);
    let bindings = doc.defold.as_ref().map(|d| d.nodes.clone()).unwrap_or_default();
    let stamps = bindings.iter().map(|b| appearance(&doc, b)).collect::<Result<Vec<_>>>()?;
    if let Some(link) = &mut doc.defold {
        for (n, stamp) in link.nodes.iter_mut().zip(stamps) {
            n.appearance = stamp;
        }
    }
    let active = doc
        .defold
        .as_ref()
        .and_then(|d| d.nodes.iter().find(|n| doc.layer(LayerId(n.artwork)).is_some_and(|l| l.visible && l.opacity > 0.0)).or_else(|| d.nodes.first()))
        .map(|n| LayerId(n.artwork));
    let i = s.add_document(doc, Some(saved));
    if let Some(st) = s.active_mut() {
        st.active_layer = active;
        st.selected_layers = active.into_iter().collect();
        st.saved_revision = 0;
    }
    Ok(json!({"document":i,"warnings":warnings}))
}

fn node_kind(scene: &Scene, id: &str, kind: &str) -> Result<Scene> {
    let f = scene.proto.fields.iter().find(|f| f.name == "nodes" && scene.proto.text(&f.children, "id") == id).ok_or_else(|| err("Missing node"))?;
    let mut text = scene.proto.source.clone();
    if let Some(t) = f.children.iter().find(|f| f.name == "type") {
        text.replace_range(t.value.clone(), kind);
    } else {
        text.insert_str(f.value.end.saturating_sub(1), &format!("\ntype: {kind}\n"));
    }
    Scene::parse(text).map_err(err)
}
fn resource_path(root: &str, path: &str) -> Result<String> {
    Ok(format!("/{}", Path::new(path).strip_prefix(root).map_err(err)?.to_string_lossy().replace('\\', "/")))
}
fn output_file(root: &str, relative: &str, bytes: &[u8]) -> Result<()> {
    if relative.contains(['\\', ':']) || relative.split('/').any(|s| s == ".." || s == ".") {
        return Err(err("Invalid generated resource path"));
    }
    let path = Path::new(root).join(relative.trim_start_matches('/'));
    #[cfg(not(target_arch = "wasm32"))]
    {
        let parent = path.parent().ok_or_else(|| err("Invalid generated folder"))?;
        let existing = parent.ancestors().find(|p| p.exists()).ok_or_else(|| err("Missing project"))?;
        let canonical_root = Path::new(root).canonicalize().map_err(err)?;
        if !existing.canonicalize().map_err(err)?.starts_with(&canonical_root) {
            return Err(err("Generated folder leaves the project"));
        }
        std::fs::create_dir_all(parent).map_err(err)?;
        if !parent.canonicalize().map_err(err)?.starts_with(canonical_root) {
            return Err(err("Generated folder leaves the project"));
        }
    }
    if path.is_file() {
        if crate::file_cmds::read_file(&path.to_string_lossy())? == bytes {
            return Ok(());
        }
        return Err(err("Generated resource changed externally; choose a new publication"));
    }
    crate::file_cmds::write_file(&path.to_string_lossy(), bytes)
}
fn reconcile(doc: &mut Document) -> Result<Scene> {
    let mut design = doc.defold.clone().ok_or_else(|| err("Missing GUI link"))?;
    let mut scene = Scene::parse(design.source.clone()).map_err(err)?;
    let mut entries = Vec::new();
    // Imported artwork and AI versions edit a node's own appearance. Other
    // native layers represent nodes, including ordinary layers inside groups.
    let mut artwork: std::collections::HashSet<u64> = design.nodes.iter().filter(|n| n.artwork != n.layer).map(|n| n.artwork).collect();
    artwork.extend(doc.ai_generations.iter().flat_map(|g| &g.versions).filter_map(|v| v.layer.map(|id| id.0)));
    for node in &design.nodes {
        artwork.remove(&node.layer);
    }
    fn visit(layers: &[Layer], parent: u64, artwork: &std::collections::HashSet<u64>, entries: &mut Vec<(u64, u64, String)>, depth: usize) -> Result<()> {
        if depth > 128 {
            return Err(err("Layer nesting limit"));
        }
        for l in layers {
            if parent == 0 || !artwork.contains(&l.id.0) {
                entries.push((l.id.0, parent, l.name.clone()));
                if entries.len() > 8192 {
                    return Err(err("GUI node limit"));
                }
                if let LayerContent::Group(g) = &l.content {
                    visit(&g.children, l.id.0, artwork, entries, depth + 1)?;
                }
            }
        }
        Ok(())
    }
    visit(&doc.layers, 0, &artwork, &mut entries, 0)?;
    for (layer, parent, _) in &entries {
        if design.nodes.iter().any(|n| n.layer == *layer) {
            continue;
        }
        let id = format!("pc_node_{layer}");
        let parent = design.nodes.iter().find(|n| n.layer == *parent).map(|n| n.node.as_str()).unwrap_or("");
        scene = scene.new_node(&id, "TYPE_BOX", parent, None).map_err(err)?;
        design.nodes.push(NodeLink { layer: *layer, artwork: *layer, node: id, appearance: String::new(), text: None });
    }
    for (layer, parent, name) in &entries {
        let binding = design.nodes.iter().find(|n| n.layer == *layer).ok_or_else(|| err("Unlinked node"))?.clone();
        let parent = design.nodes.iter().find(|n| n.layer == *parent).map(|n| n.node.clone()).unwrap_or_default();
        let old = scene.nodes.iter().find(|n| n.id == binding.node).ok_or_else(|| err("Node missing"))?;
        if old.parent != parent {
            scene = scene.move_node(&binding.node, &parent, None).map_err(err)?;
        }
        if binding.appearance.is_empty() {
            continue;
        }
        if *name != binding.node {
            scene = scene.rename_node(&binding.node, name).map_err(err)?;
            if let Some(b) = design.nodes.iter_mut().find(|n| n.layer == *layer) {
                b.node = name.clone();
            }
        }
    }
    for b in design.nodes.clone() {
        if doc.layer(LayerId(b.layer)).is_none() && scene.nodes.iter().any(|n| n.id == b.node) {
            scene = scene.remove_node(&b.node).map_err(err)?;
        }
    }
    design.nodes.retain(|n| doc.layer(LayerId(n.layer)).is_some());
    // The native tree is bottom-to-top, as is GUI draw order.
    let mut ordered_parents = std::collections::HashSet::new();
    for (_, parent, _) in &entries {
        if !ordered_parents.insert(*parent) {
            continue;
        }
        let siblings: Vec<_> = entries
            .iter()
            .filter(|(_, p, _)| p == parent)
            .filter_map(|(id, _, _)| design.nodes.iter().find(|n| n.layer == *id).map(|n| n.node.clone()))
            .collect();
        let parent = design.nodes.iter().find(|n| n.layer == *parent).map(|n| n.node.as_str()).unwrap_or("");
        if scene.nodes.iter().filter(|n| n.parent == parent).map(|n| n.id.as_str()).eq(siblings.iter().map(String::as_str)) {
            continue;
        }
        for node in siblings {
            scene = scene.move_node(&node, parent, None).map_err(err)?;
        }
    }
    design.source = scene.proto.source.clone();
    doc.defold = Some(design);
    Ok(scene)
}
fn live_nodes(scene: &Scene) -> Result<Value> {
    let mut nodes = Vec::new();
    for n in &scene.nodes {
        let mut values = serde_json::Map::new();
        for p in photocraft_defold_format::properties(&n.kind) {
            let v = scene.property(&n.id, &p.path).map_err(err)?;
            let key = p.path.split('.').next().unwrap_or_default();
            let key = match key {
                "size" => "manual_size",
                "xanchor" => "x_anchor",
                "yanchor" => "y_anchor",
                x => x,
            };
            if let Some((_, component)) = p.path.split_once('.') {
                let i = match component {
                    "x" => 0,
                    "y" => 1,
                    "z" => 2,
                    _ => 3,
                };
                let a = values.entry(key).or_insert_with(|| json!([]));
                if let Some(a) = a.as_array_mut() {
                    while a.len() <= i {
                        a.push(Value::Null);
                    }
                    if let Some(slot) = a.get_mut(i) {
                        *slot = v;
                    }
                }
            } else {
                let v = if matches!(p.kind, photocraft_defold_format::PropertyKind::Enum(_)) {
                    json!(v.as_str().unwrap_or_default().replace('_', "-").to_lowercase())
                } else {
                    v
                };
                values.insert(key.into(), v);
            }
        }
        nodes.push(json!({"id":n.id,"parent":n.parent,"kind":format!("gui-node-type-{}",n.kind.trim_start_matches("TYPE_").to_lowercase()),"values":values}));
    }
    Ok(json!(nodes))
}
fn sync(s: &mut Session, _: &Value) -> Result<Value> {
    let st = s.active().ok_or_else(|| err("No document"))?;
    if st.floating.is_some() || s.job_on(st.doc.id).is_some() {
        return Err(err("Finish the current edit or generation before syncing"));
    }
    let revision = st.revision;
    let mut doc = (*st.doc).clone();
    let size = linked_design_size(&doc)?;
    let canvas_warnings = fit_canvas(&mut doc, size)?;
    let original = doc.defold.clone().ok_or_else(|| err("Not linked"))?;
    let target = crate::defold_cmds::resource(&original.root, &resource_path(&original.root, &original.gui)?)?;
    let disk = crate::file_cmds::read_file(&target.to_string_lossy())?;
    let expected = s.gui.design_stamps.get(&original.gui).cloned().unwrap_or((original.disk_hash.clone(), original.remote.clone()));
    let online = Path::new(&original.root).join(".internal/editor.port").is_file();
    let path = resource_path(&original.root, &original.gui)?;
    let remote = if online {
        let expected_remote: crate::defold_live::Snapshot = if let Some(baseline) = expected.1.as_deref() {
            serde_json::from_str(baseline).map_err(err)?
        } else {
            if hash(&disk) != expected.0 {
                return Err(err("GUI changed on disk before connecting to Defold"));
            }
            let mut probe = Session::new();
            probe.execute("defold.openGui", json!({"path":original.gui}))?;
            let disk_scene = probe.active_gui().ok_or_else(|| err("Missing GUI"))?.scene.clone();
            probe.execute("defold.connectLive", json!({}))?;
            let live = probe.active_gui().ok_or_else(|| err("Missing GUI"))?;
            if !matches_nodes(&disk_scene, &live.scene) {
                return Err(err("Defold has unsaved GUI changes; synchronization stopped"));
            }
            live.live.as_ref().ok_or_else(|| err("Missing editor snapshot"))?.snapshot.clone()
        };
        let current = crate::defold_live::request(&original.root, &json!({"op":"snapshot","path":path}))?;
        if !crate::defold_live::same_nodes(&current.nodes, &expected_remote.nodes) {
            return Err(err("GUI changed in Defold; sync stopped without overwriting it"));
        }
        Some(current)
    } else {
        if hash(&disk) != expected.0 {
            return Err(err("GUI changed on disk; sync stopped without overwriting it"));
        }
        None
    };
    let authored = reconcile(&mut doc)?;
    if authored.proto.fields.iter().any(|f| matches!(f.name.as_str(), "layouts")) || authored.nodes.iter().any(|n| n.kind == "TYPE_TEMPLATE" || !n.editable) {
        return Err(err("Template instances and layout overrides require reconciliation before publishing this design"));
    }
    let links = doc.defold.as_ref().ok_or_else(|| err("Missing link"))?.nodes.clone();
    let mut export_doc = doc.clone();
    crate::color_cmds::convert_document(&mut export_doc, photocraft_cms::Builtin::Srgb.profile(), photocraft_cms::Intent::RelativeColorimetric, true)?;
    let mut output = authored.clone();
    let draft = photocraft_format::save_to_bytes(&doc, &Default::default()).map_err(err)?;
    let folder = format!(
        "/photocraft_generated/{}/{}",
        hash(original.gui.as_bytes()).chars().take(12).collect::<String>(),
        hash(&draft).chars().take(16).collect::<String>()
    );
    let mut files = Vec::new();
    let mut textures = Vec::new();
    let mut new_world = HashMap::new();
    let mut total_pixels = 0_u64;
    for b in &links {
        let node = authored.nodes.iter().find(|n| n.id == b.node).ok_or_else(|| err("Missing authored node"))?.clone();
        let group = doc.layer(LayerId(b.layer)).ok_or_else(|| err("Missing layer"))?;
        output = output.edit(&b.node, "enabled", &json!(group.visible)).map_err(err)?;
        let w = world(&authored, &b.node)?;
        new_world.insert(b.node.clone(), w);
        if !b.appearance.is_empty() && appearance(&doc, b)? == b.appearance {
            continue;
        }
        // A normal native text edit remains a Defold text node (gui.set_text stays valid).
        if node.kind == "TYPE_TEXT"
            && let Some(base) = &b.text
            && let Some(LayerContent::Text(t)) = doc.layer(LayerId(b.artwork)).map(|l| &l.content)
        {
            let text = t.text.clone();
            let mut restored = doc.clone();
            if let Some(layer) = restored.layer_mut(LayerId(b.artwork))
                && let LayerContent::Text(t) = &mut layer.content
            {
                t.text = base.clone();
                if t.runs.len() == 1
                    && let Some(run) = t.runs.first_mut()
                {
                    run.len = base.len();
                }
                transform(layer, Affine::IDENTITY)?;
            }
            if appearance(&restored, b)? == b.appearance {
                output = output.edit(&b.node, "text", &json!(text)).map_err(err)?;
                continue;
            }
        }
        let own = own_layer(&export_doc, b)?;
        let buffer = rendered(&export_doc, own)?;
        total_pixels += u64::from(buffer.rect.width()) * u64::from(buffer.rect.height());
        if total_pixels > 64 * 1024 * 1024 {
            return Err(err("Sync exceeds 64 megapixels of changed artwork"));
        }
        let mut surface = Surface::new(PixelFormat::RGBA32F);
        surface.write_region(buffer.rect, &buffer.px.iter().flatten().copied().collect::<Vec<_>>());
        let mut local_art = Layer::new("Export", LayerContent::Raster(surface));
        // Convert screen coordinates to node-local pixel coordinates (Y down).
        let flip = Affine { m: [1.0, 0.0, 0.0, -1.0, 0.0, 0.0] };
        transform(
            &mut local_art,
            flip.mul(&w.inverse().ok_or_else(|| err("Cannot publish a zero-scale node"))?)
                .mul(&document_screen(&doc).inverse().ok_or_else(|| err("Invalid GUI canvas transform"))?),
        )?;
        // Local artwork routinely straddles negative X/Y around its pivot. Do not clip it
        // against the document's positive screen rectangle a second time.
        let pixels = local_art.surface().ok_or_else(|| err("Missing flattened pixels"))?;
        let mut r = pixels.content_bounds();
        if r.is_empty() {
            r = Rect::new(0, 0, 1, 1);
        }
        if r.width() > 8192 || r.height() > 8192 || u64::from(r.width()) * u64::from(r.height()) > 16 * 1024 * 1024 {
            return Err(err("Local artwork exceeds pixel budget"));
        }
        let mut local_buffer = photocraft_compose::Buffer::transparent(r);
        pixels.read_rgba_into(r, &mut local_buffer.px);
        let center = Point::new((r.x0 as f64 + r.x1 as f64) * 0.5, -(r.y0 as f64 + r.y1 as f64) * 0.5);
        new_world.insert(b.node.clone(), w.mul(&Affine::translate(center.x, center.y)));
        let pixels = local_buffer.to_rgba8();
        let image = photocraft_codecs::Image::from_u8(r.width(), r.height(), photocraft_codecs::ChannelLayout::Rgba, pixels.pixels).map_err(err)?;
        let png = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).map_err(err)?;
        let stem = format!("node_{}", b.layer);
        let alias = format!("pc_{}", b.layer);
        let png_path = format!("{folder}/{stem}.png");
        let atlas_path = format!("{folder}/{stem}.atlas");
        files.push((png_path.clone(), png));
        files.push((atlas_path.clone(), format!("images {{ image: {} }}\nextrude_borders: 2\n", serde_json::to_string(&png_path).map_err(err)?).into_bytes()));
        textures.push(json!({"name":alias,"texture":atlas_path}));
        output = node_kind(&output, &b.node, "TYPE_BOX")?;
        output = output.edit(&b.node, "inherit_alpha", &json!(false)).map_err(err)?;
        for (k, v) in [
            ("texture", json!(format!("{alias}/{stem}"))),
            ("size.x", json!(r.width())),
            ("size.y", json!(r.height())),
            ("size_mode", json!("SIZE_MODE_MANUAL")),
            ("pivot", json!("PIVOT_CENTER")),
            ("alpha", json!(1.0)),
            ("color.x", json!(1.0)),
            ("color.y", json!(1.0)),
            ("color.z", json!(1.0)),
            ("color.w", json!(1.0)),
        ] {
            output = output.edit(&b.node, k, &v).map_err(err)?;
        }
        for k in ["slice9.x", "slice9.y", "slice9.z", "slice9.w"] {
            output = output.edit(&b.node, k, &json!(0)).map_err(err)?;
        }
        let _ = node;
    }
    // Cropping node artwork shifts its origin; preserve descendants' world positions.
    for n in &authored.nodes {
        let w = new_world.get(&n.id).copied().ok_or_else(|| err("Missing transform"))?;
        let parent = if n.parent.is_empty() { Affine::IDENTITY } else { *new_world.get(&n.parent).ok_or_else(|| err("Missing parent transform"))? };
        let original_parent = if n.parent.is_empty() { Affine::IDENTITY } else { world(&authored, &n.parent)? };
        if w == world(&authored, &n.id)? && parent == original_parent {
            // Preserve authored values exactly when no crop shifted either origin.
            // Multiplying parent/inverse matrices otherwise introduces rounding drift.
            continue;
        }
        let local = parent.inverse().ok_or_else(|| err("Zero-scale parent"))?.mul(&w);
        let p = local.apply(Point::new(0.0, 0.0));
        output = output.edit(&n.id, "position.x", &json!(p.x)).map_err(err)?;
        output = output.edit(&n.id, "position.y", &json!(p.y)).map_err(err)?;
    }
    let aliases: Vec<_> = textures.iter().filter_map(|v| v["name"].as_str()).collect();
    let mut raw = output.proto.source.clone();
    for f in output.proto.fields.iter().rev().filter(|f| f.name == "textures" && aliases.contains(&output.proto.text(&f.children, "name").as_str())) {
        raw.replace_range(f.span.clone(), "");
    }
    for t in &textures {
        raw.push_str(&format!("\ntextures {{ name: {} texture: {} }}\n", t["name"], t["texture"]));
    }
    output = Scene::parse(raw).map_err(err)?;
    // Save the editable source before any asset or GUI publication.
    let source_path = native_path(&original.gui);
    crate::file_cmds::write_file(&source_path, &draft)?;
    for (path, bytes) in &files {
        output_file(&original.root, path, bytes)?;
    }
    let next_remote = if let Some(remote) = remote {
        let snap = crate::defold_live::request(
            &original.root,
            &json!({"op":"publishDesign","path":path,"expected":remote.nodes,"directory":folder,"textures":textures,"nodes":live_nodes(&output)?}),
        )?;
        Some(serde_json::to_string(&snap).map_err(err)?)
    } else {
        if hash(&crate::file_cmds::read_file(&original.gui)?) != expected.0 {
            return Err(err("GUI changed during export"));
        }
        crate::file_cmds::write_file(&original.gui, output.proto.source.as_bytes())?;
        None
    };
    let disk_hash = if online { hash(&disk) } else { hash(output.proto.source.as_bytes()) };
    if let Some(link) = &mut doc.defold {
        link.source = authored.proto.source.clone();
        link.disk_hash = disk_hash.clone();
        link.remote = next_remote.clone();
    }
    let native = photocraft_format::save_to_bytes(&doc, &Default::default()).map_err(err)?;
    s.gui.design_stamps.insert(original.gui.clone(), (disk_hash, next_remote));
    crate::file_cmds::write_file(&source_path, &native)?;
    if let Some(st) = s.active_mut() {
        if st.revision != revision {
            return Err(err("Document changed during publication"));
        }
        st.doc = Arc::new(doc);
        st.saved_to(source_path);
    }
    Ok(json!({"synced":true,"online":online,"assets":files.len()/2,"nodes":output.nodes.len(),"editorSaveRequired":online,"warnings":canvas_warnings}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "defold.openDesign",
            label: "Open linked GUI design",
            menu: &[],
            shortcut: None,
            params: r#"{"path":"absolute GUI path"}"#,
            enabled: crate::file_cmds::native,
            run: open,
            journal: false,
        },
        CommandSpec {
            id: "defold.setDesignProperties",
            label: "Defold node properties",
            menu: &[],
            shortcut: None,
            params: r#"{"values":{property:value}}"#,
            enabled,
            run: update,
            journal: true,
        },
        CommandSpec {
            id: "defold.addDesignNode",
            label: "Add linked GUI node",
            menu: &[],
            shortcut: None,
            params: r#"{"child":bool,"kind":string?}"#,
            enabled,
            run: add_node,
            journal: true,
        },
        CommandSpec { id: "defold.syncDesign", label: "Sync design to Defold", menu: &[], shortcut: None, params: "{}", enabled, run: sync, journal: false },
        CommandSpec {
            id: "defold.openDesignResource",
            label: "Open linked resource in Defold",
            menu: &[],
            shortcut: None,
            params: r#"{"property":"texture|font|material"}"#,
            enabled,
            run: open_resource,
            journal: false,
        },
    ]
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "defold_design_tests.rs"]
mod tests;
