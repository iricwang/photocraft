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
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

fn string<'a>(p: &'a Value, key: &str) -> Result<&'a str> {
    p.get(key).and_then(Value::as_str).ok_or_else(|| err(format!("Missing {key}")))
}
fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}
fn retain_generated_texture(
    images: &mut BTreeMap<String, Vec<u8>>,
    resources: &BTreeMap<String, Vec<u8>>,
    folder: &str,
    alias: &str,
    texture: &str,
    registry: &crate::defold_assets::Registry,
) -> Result<()> {
    let Some(stem) = texture.strip_prefix(alias).and_then(|s| s.strip_prefix('/')) else { return Ok(()) };
    let path = format!("{folder}/images/{stem}.png");
    if images.contains_key(&path) {
        return Ok(());
    }
    let bytes = resources.get(&path).cloned().ok_or_else(|| err(format!("Missing generated texture {path}; restore the image before syncing")))?;
    let expected = registry
        .files
        .get(&path)
        .or_else(|| registry.retired.get(&path))
        .map(String::as_str)
        .or_else(|| stem.strip_prefix("img_").filter(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit())));
    if expected.is_none_or(|value| hash(&bytes) != value) {
        return Err(err(format!("Generated texture was modified externally: {path}")));
    }
    images.insert(path, bytes);
    Ok(())
}
#[cfg(test)]
use std::sync::Arc;

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
/// Insert native artwork into its captured GUI owner. A pixel layer can itself carry a
/// node; promote that carrier once, preserving its pixels and identity inside the group.
/// Callers perform this inside their existing `Session::edit` transaction.
pub(crate) fn insert_artwork(doc: &mut Document, target: LayerId, artwork: Layer) -> Result<LayerId> {
    crate::defold_shared_design::require_editable_artwork(doc, target)?;
    let Some(binding) = node_for_layer(doc, target).cloned() else {
        return Err(err("The target GUI node was deleted"));
    };
    let owner = ensure_node_group(doc, &binding)?;
    let container = crate::defold_design_reconcile::artwork_container(doc, owner, LayerId(binding.artwork))?;
    let layer = doc.layer_mut(container).ok_or_else(|| err("Node layer missing"))?;
    let LayerContent::Group(group) = &mut layer.content else {
        return Err(err("Invalid node artwork container"));
    };
    let id = artwork.id;
    group.children.push(artwork);
    crate::defold_design_reconcile::structure_changed(doc)?;
    Ok(id)
}

fn ensure_node_group(doc: &mut Document, binding: &NodeLink) -> Result<LayerId> {
    let layer = doc.layer_mut(LayerId(binding.layer)).ok_or_else(|| err("Node layer missing"))?;
    if matches!(layer.content, LayerContent::Group(_)) {
        return Ok(layer.id);
    }
    let mut group = Layer::group(layer.name.clone(), Vec::new());
    group.visible = layer.visible;
    group.clipped = layer.clipped;
    group.locks = layer.locks;
    let id = group.id;
    let mut original = std::mem::replace(layer, group);
    original.visible = true;
    original.clipped = false;
    if let LayerContent::Group(group) = &mut layer.content {
        group.children.push(original);
    }
    let link = doc.defold.as_mut().and_then(|d| d.nodes.iter_mut().find(|n| n.layer == binding.layer)).ok_or_else(|| err("Node link missing"))?;
    link.layer = id.0;
    crate::defold_design_reconcile::promoted(doc, LayerId(binding.layer), id)?;
    Ok(id)
}
pub fn scene(s: &Session) -> Result<Scene> {
    let doc = &s.active().ok_or_else(|| err("No document"))?.doc;
    document_scene(doc)
}
pub(crate) fn document_scene(doc: &Document) -> Result<Scene> {
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
#[cfg(test)]
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
        for animation in atlas.fields.iter().filter(|f| f.name == "animations") {
            let id = atlas.text(&animation.children, "id");
            if !id.is_empty() {
                choices.push(format!("{name}/{id}"));
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
pub(crate) fn world(scene: &Scene, id: &str) -> Result<Affine> {
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
pub(crate) fn document_screen(doc: &Document) -> Affine {
    let [x, y] = doc.defold.as_ref().map_or([0, 0], |d| d.canvas_origin);
    Affine { m: [1.0, 0.0, 0.0, -1.0, -f64::from(x), f64::from(doc.size.height) + f64::from(y)] }
}
pub(crate) fn transform(layer: &mut Layer, a: Affine) -> Result<()> {
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
pub(crate) fn own_layer(doc: &Document, binding: &NodeLink) -> Result<Layer> {
    let mut layer = doc.layer(LayerId(binding.layer)).ok_or_else(|| err("Node layer was deleted"))?.clone();
    let design = doc.defold.as_ref().ok_or_else(|| err("Missing link"))?;
    fn strip(l: &mut Layer, design: &Design) {
        if let LayerContent::Group(g) = &mut l.content {
            g.children.retain(|c| {
                !design.nodes.iter().any(|n| n.layer == c.id.0)
                    && !design.project.as_ref().and_then(|p| p.layers.get(&c.id.0)).is_some_and(|b| b.role == defold::LayerRole::DesignOnly)
            });
            for c in &mut g.children {
                strip(c, design);
            }
        }
    }
    strip(&mut layer, design);
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
pub(crate) fn rendered(doc: &Document, layer: Layer) -> Result<photocraft_compose::Buffer> {
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
#[cfg(test)]
std::thread_local! { static APPEARANCE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
fn appearance(doc: &Document, binding: &NodeLink) -> Result<String> {
    #[cfg(test)]
    APPEARANCE_CALLS.with(|calls| calls.set(calls.get() + 1));
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
pub(crate) fn changed_artwork_nodes(doc: &Document) -> Result<std::collections::BTreeSet<String>> {
    let design = doc.defold.as_ref().ok_or_else(|| err("Missing GUI attachment"))?;
    let mut changed = std::collections::BTreeSet::new();
    for link in &design.nodes {
        if design
            .project
            .as_ref()
            .and_then(|p| p.layers.get(&link.layer))
            .is_some_and(|binding| matches!(binding.appearance, Some(defold::AppearanceSource::Shared { .. })))
        {
            changed.insert(link.node.clone());
            continue;
        }
        if link.appearance.is_empty() || appearance(doc, link)? != link.appearance {
            changed.insert(link.node.clone());
        }
    }
    Ok(changed)
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
    if crate::peer_binding::active_for(&link.root)?.is_some() {
        crate::defold_live::request(&link.root, &json!({"op":"snapshot", "path":resource_path(&link.root, &link.gui)?}))?;
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
    let node = selected(s).map(|binding| binding.node.clone()).ok_or_else(|| err("Select a linked node layer"))?;
    let values = p.get("values").and_then(Value::as_object).ok_or_else(|| err("Expected property values"))?.clone();
    let id = s.edit("Defold node properties", |doc, _| apply_node_properties(doc, &node, &values))?;
    Ok(json!({"node": id}))
}
pub(crate) fn apply_node_properties(doc: &mut Document, node: &str, values: &serde_json::Map<String, Value>) -> Result<String> {
    let binding =
        doc.defold.as_ref().and_then(|design| design.nodes.iter().find(|link| link.node == node)).cloned().ok_or_else(|| err("Linked GUI node is missing"))?;
    let values = values.clone();
    if values.len() > 64 {
        return Err(err("Too many properties"));
    }
    let old = document_scene(doc)?;
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
    let to_screen = document_screen(doc);
    let from_screen = to_screen.inverse().ok_or_else(|| err("Invalid GUI canvas transform"))?;
    let font_change = values.contains_key("font").then(|| font(&next, &id, &doc.defold.as_ref().map(|d| d.root.clone()).unwrap_or_default()));
    let replacement = if before.texture != after.texture && !after.texture.is_empty() {
        let link = doc.defold.as_ref().ok_or_else(|| err("No link"))?;
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
    let alpha_changed = values.keys().any(|key| matches!(key.as_str(), "alpha" | "inherit_alpha" | "color.w"));
    let descendants_changed = a != Affine::IDENTITY || alpha_changed || before.enabled != after.enabled;
    let artwork_changed = descendants_changed
        || size_changed
        || replacement.is_some()
        || before.visible != after.visible
        || before.color != after.color
        || values.keys().any(|key| matches!(key.as_str(), "text" | "font" | "text_leading" | "text_tracking" | "line_break"));
    let affected: Vec<NodeLink> = if artwork_changed {
        let path = doc.path_of(LayerId(binding.layer)).ok_or_else(|| err("Node layer missing"))?;
        doc.defold
            .as_ref()
            .ok_or_else(|| err("Missing GUI link"))?
            .nodes
            .iter()
            .filter(|node| node.layer == binding.layer || (descendants_changed && doc.path_of(LayerId(node.layer)).is_some_and(|p| p.starts_with(&path))))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    // Only the changed subtree needs pristine-artwork bookkeeping. Metadata such as
    // ID, material, layer and blend mode never composites pixels while being edited.
    let clean: std::collections::HashSet<u64> =
        { affected.iter().filter(|node| appearance(doc, node).is_ok_and(|stamp| stamp == node.appearance)).map(|node| node.layer).collect() };
    let before = before.clone();
    let layer = doc.layer_mut(LayerId(binding.layer)).ok_or_else(|| err("Node layer missing"))?;
    if a != Affine::IDENTITY {
        transform(layer, a)?;
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
        if let Some(b) = link.project.as_mut().and_then(|p| p.layers.get_mut(&binding.layer)) {
            b.gui_node_id = Some(id.clone());
        }
    }
    // Property-only changes remain authored GUI properties, not a forced rasterization.
    for b in affected {
        let old_id = if b.layer == binding.layer { binding.node.as_str() } else { b.node.as_str() };
        let previous = alpha(&old, old_id)?;
        let current_id = if b.layer == binding.layer { id.as_str() } else { b.node.as_str() };
        let current = alpha(&next, current_id)?;
        if (previous - current).abs() > f32::EPSILON
            && let Some(art) = doc.layer_mut(LayerId(b.artwork))
        {
            art.opacity = if previous > f32::EPSILON { (art.opacity * current / previous).clamp(0.0, 1.0) } else { current };
        }
        // Painted nodes are already dirty; recomputing their baseline would both
        // waste a composite and risk treating painted pixels as pristine.
        if !clean.contains(&b.layer) {
            continue;
        }
        let stamp = appearance(doc, &b)?;
        if let Some(link) = &mut doc.defold
            && let Some(n) = link.nodes.iter_mut().find(|n| n.layer == b.layer)
        {
            // Only reset the changed subtree if its artwork was still pristine before this edit.
            if clean.contains(&b.layer) {
                n.appearance = stamp;
                let node_id = if b.layer == binding.layer { id.as_str() } else { b.node.as_str() };
                n.text = next.nodes.iter().find(|node| node.id == node_id && node.kind == "TYPE_TEXT").map(|node| node.text.clone());
            }
        }
    }
    Ok(id)
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
    let link = NodeLink { asset_id: String::new(), layer: group.id.0, artwork: art_id.0, node: id.clone(), appearance: String::new(), text: None };
    s.edit("Add Defold node", move |doc, active| {
        let parent_link = doc.defold.as_ref().and_then(|d| d.nodes.iter().find(|n| n.node == parent)).cloned();
        if let Some(parent) = parent_link {
            let owner = ensure_node_group(doc, &parent)?;
            let layer = doc.layer_mut(owner).ok_or_else(|| err("Parent missing"))?;
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
        crate::defold_design_reconcile::initialize(doc)?;
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
    if crate::peer_binding::active_for(&d.root)?.is_some() {
        crate::defold_live::request(&d.root, &json!({"op":"snapshot", "path":resource_path(&d.root, &gui)?}))?;
    }
    if let Some(i) = s.documents().iter().position(|st| st.doc.defold.as_ref().is_some_and(|d| d.gui == gui)) {
        s.set_active(i);
        return Ok(json!({"document":i,"restored":true}));
    }
    let raw = crate::file_cmds::read_file(&gui)?;
    let disk_hash = hash(&raw);
    let saved = native_path(&gui);
    let formal = crate::defold_project_client::load_saved_document_with_binding(&d.root, &gui)?;
    let acknowledged_source = formal.is_some();
    let (restored, restored_binding) = if let Some((document, binding)) = formal {
        (Some(document), Some(binding))
    } else if Path::new(&saved).is_file() {
        (Some(photocraft_format::load_from_bytes(&crate::file_cmds::read_file(&saved)?).map_err(err)?), None)
    } else {
        (None, None)
    };
    if let Some(mut doc) = restored {
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
        let binding = match restored_binding {
            Some(binding) => binding,
            None => crate::defold_project_client::bind_new_document(&mut doc)?,
        };
        let i = s.add_document(doc, Some(gui.clone()));
        if let Some(open_id) = s.active().map(|state| state.open_id) {
            s.project_client.bindings.insert(open_id, binding);
            if acknowledged_source {
                s.project_client.acknowledged_openings.insert(open_id);
            }
        }
        return Ok(json!({"document":i,"restored":true,"warnings":warnings}));
    }
    let mut remote = None;
    let mut warnings = Vec::new();
    if crate::peer_binding::online(&d.root) {
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
    doc.defold = Some(Design {
        version: 3,
        root: d.root.clone(),
        gui: gui.clone(),
        source: scene.proto.source.clone(),
        disk_hash,
        nodes: vec![],
        remote,
        canvas_origin: [0, 0],
        project: None,
    });
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
            asset_id: String::new(),
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
    let binding = crate::defold_project_client::bind_new_document(&mut doc)?;
    let i = s.add_document(doc, Some(gui.clone()));
    if let Some(open_id) = s.active().map(|state| state.open_id) {
        s.project_client.bindings.insert(open_id, binding);
    }
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
pub(crate) fn reconcile(doc: &mut Document) -> Result<Scene> {
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
    fn visit(
        layers: &[Layer],
        parent: u64,
        artwork: &std::collections::HashSet<u64>,
        roles: Option<&BTreeMap<u64, defold::LayerBinding>>,
        entries: &mut Vec<(u64, u64, String)>,
        depth: usize,
    ) -> Result<()> {
        if depth > 128 {
            return Err(err("Layer nesting limit"));
        }
        for l in layers {
            let structural =
                roles.map_or(parent == 0 || !artwork.contains(&l.id.0), |roles| roles.get(&l.id.0).is_some_and(|b| b.role == defold::LayerRole::GuiNode));
            if structural {
                entries.push((l.id.0, parent, l.name.clone()));
                if entries.len() > 8192 {
                    return Err(err("GUI node limit"));
                }
                if let LayerContent::Group(g) = &l.content {
                    visit(&g.children, l.id.0, artwork, roles, entries, depth + 1)?;
                }
            }
        }
        Ok(())
    }
    visit(&doc.layers, 0, &artwork, design.project.as_ref().map(|p| &p.layers), &mut entries, 0)?;
    for (layer, parent, _) in &entries {
        if design.nodes.iter().any(|n| n.layer == *layer) {
            continue;
        }
        let key: String = hash(format!("{}:layer:{layer}", resource_path(&design.root, &design.gui)?).as_bytes()).chars().take(16).collect();
        let label = entries.iter().find(|(id, _, _)| id == layer).map(|(_, _, name)| name.as_str()).unwrap_or("Node");
        let base = crate::defold_assets::name(label);
        let mut id = base.clone();
        if scene.nodes.iter().any(|node| node.id == id)
            || entries.iter().filter(|(_, _, name)| crate::defold_assets::name(name).eq_ignore_ascii_case(&base)).count() > 1
        {
            id = format!("{base}__{}", key.chars().take(8).collect::<String>());
            while scene.nodes.iter().any(|node| node.id == id) {
                id.push('_');
            }
        }
        let parent = design.nodes.iter().find(|n| n.layer == *parent).map(|n| n.node.as_str()).unwrap_or("");
        scene = scene.new_node(&id, "TYPE_BOX", parent, None).map_err(err)?;
        let asset_id = if let Some(binding) = design.project.as_mut().and_then(|p| p.layers.get_mut(layer)) {
            binding.gui_node_id = Some(id.clone());
            binding.asset_id.as_ref().map(|id| id.as_str().to_string()).unwrap_or(key)
        } else {
            key
        };
        design.nodes.push(NodeLink { asset_id, layer: *layer, artwork: *layer, node: id, appearance: String::new(), text: None });
    }
    // Display names control managed image names. Script-facing GUI IDs change only
    // through the explicit Defold ID property, never through a native layer rename.
    for (layer, parent, _) in &entries {
        let binding = design.nodes.iter().find(|n| n.layer == *layer).ok_or_else(|| err("Unlinked node"))?;
        let parent = design.nodes.iter().find(|n| n.layer == *parent).map(|n| n.node.as_str()).unwrap_or("");
        if scene.nodes.iter().find(|n| n.id == binding.node).is_some_and(|n| n.parent != parent) {
            scene = scene.move_node(&binding.node, parent, None).map_err(err)?;
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
pub(crate) fn live_nodes(scene: &Scene) -> Result<Value> {
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
/// Immutable resource inputs captured before planning. All paths are project-relative.
pub(crate) struct PublicationInputs {
    pub registry: crate::defold_assets::Registry,
    pub resources: BTreeMap<String, Vec<u8>>,
    pub shared_atlas: bool,
    pub retained_textures: Vec<String>,
    /// Formal project atlas aliases also retain their immutable captured image bytes.
    pub managed_aliases: Vec<String>,
    /// Already referenced canonical owners retain an identity even when completely erased.
    pub shared_owners: std::collections::BTreeSet<String>,
    /// Older managed assets must acquire a real crop recipe before canonical reuse.
    pub force_bake: std::collections::BTreeSet<String>,
}
pub(crate) struct PublicationPlan {
    pub document: Document,
    #[cfg(test)]
    pub authored: Scene,
    pub output: Scene,
    pub links: Vec<NodeLink>,
    #[cfg(test)]
    pub folder: String,
    pub files: Vec<(String, Vec<u8>)>,
    pub textures: Vec<Value>,
    #[cfg(test)]
    pub asset_nodes: Vec<Value>,
    pub registry: crate::defold_assets::Registry,
    pub renames: Vec<Value>,
    pub recipes: BTreeMap<String, crate::defold_project_model::ExportRecipe>,
}

/// Apply the canonical owner's crop to a consumer. Proxy render bounds never define shared geometry.
pub(crate) fn apply_shared_recipe(plan: &mut PublicationPlan, node_id: &str, recipe: &crate::defold_project_model::ExportRecipe) -> Result<()> {
    if recipe.width == 0
        || recipe.height == 0
        || recipe.cross_node_composition
        || recipe.node_local_transform != [1.0, 0.0, 0.0, 1.0, f64::from(recipe.crop_origin[0]), f64::from(recipe.crop_origin[1])]
    {
        return Err(err("Shared artwork has an unsupported canonical export recipe"));
    }
    let authored = document_scene(&plan.document)?;
    let node = authored.nodes.iter().find(|node| node.id == node_id).ok_or_else(|| err("Shared consumer node is missing"))?;
    let children: Vec<_> = plan
        .output
        .nodes
        .iter()
        .filter(|child| child.parent == node_id)
        .map(|child| Ok((child.id.clone(), world(&plan.output, &child.id)?)))
        .collect::<Result<_>>()?;
    let center =
        Point::new(f64::from(recipe.crop_origin[0]) + f64::from(recipe.width) / 2.0, -(f64::from(recipe.crop_origin[1]) + f64::from(recipe.height) / 2.0));
    let desired = world(&authored, node_id)?.mul(&Affine::translate(center.x, center.y));
    let parent = if node.parent.is_empty() { Affine::IDENTITY } else { world(&plan.output, &node.parent)? };
    let position = parent.inverse().ok_or_else(|| err("Shared consumer parent has zero scale"))?.mul(&desired).apply(Point::new(0.0, 0.0));
    plan.output = node_kind(&plan.output, node_id, "TYPE_BOX")?;
    for (key, value) in [
        ("position.x", json!(position.x)),
        ("position.y", json!(position.y)),
        ("size.x", json!(recipe.width)),
        ("size.y", json!(recipe.height)),
        ("pivot", json!("PIVOT_CENTER")),
        ("size_mode", json!("SIZE_MODE_MANUAL")),
        ("inherit_alpha", json!(false)),
        ("alpha", json!(1.0)),
        ("color.x", json!(1.0)),
        ("color.y", json!(1.0)),
        ("color.z", json!(1.0)),
        ("color.w", json!(1.0)),
        ("slice9.x", json!(0.0)),
        ("slice9.y", json!(0.0)),
        ("slice9.z", json!(0.0)),
        ("slice9.w", json!(0.0)),
    ] {
        plan.output = plan.output.edit(node_id, key, &value).map_err(err)?;
    }
    let inverse = desired.inverse().ok_or_else(|| err("Shared consumer has zero scale"))?;
    for (child, previous_world) in children {
        let p = inverse.mul(&previous_world).apply(Point::new(0.0, 0.0));
        plan.output = plan.output.edit(&child, "position.x", &json!(p.x)).map_err(err)?.edit(&child, "position.y", &json!(p.y)).map_err(err)?;
    }
    plan.recipes.insert(node_id.into(), recipe.clone());
    Ok(())
}

fn captured_image(
    resources: &BTreeMap<String, Vec<u8>>,
    registry: &crate::defold_assets::Registry,
    key: &str,
    node: &str,
    folder: &str,
    texture: &str,
) -> Result<Vec<u8>> {
    let record = registry.assets.iter().find(|asset| asset.key == key || asset.node == node);
    let resource = if let Some(record) = record {
        record.image.clone()
    } else {
        format!("{folder}/images/{}.png", texture.split_once('/').ok_or_else(|| err("Invalid managed texture"))?.1)
    };
    let bytes = resources.get(&resource).cloned().ok_or_else(|| err(format!("Missing generated texture {resource}; restore the image before syncing")))?;
    let expected = record.map(|asset| asset.hash.as_str()).or_else(|| Path::new(&resource).file_stem()?.to_str()?.strip_prefix("img_"));
    if expected.is_some_and(|value| hash(&bytes) != value) {
        return Err(err(format!("Generated texture was modified externally: {resource}")));
    }
    Ok(bytes)
}

/// Legacy local adapter only. A project host supplies the same immutable inputs from its store.
#[cfg(test)]
pub(crate) fn capture_publication_inputs(doc: &Document, remote: Option<&crate::defold_live::Snapshot>, saved_gui: Option<&[u8]>) -> Result<PublicationInputs> {
    let design = doc.defold.as_ref().ok_or_else(|| err("Not linked"))?;
    let gui = resource_path(&design.root, &design.gui)?;
    let key: String = hash(gui.as_bytes()).chars().take(16).collect();
    let name: String = Path::new(&gui)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("gui")
        .chars()
        .take(48)
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect();
    let folder = format!("/photocraft_generated/{name}_{key}");
    let mut registry = crate::defold_assets::load(&design.root, &folder)?;
    crate::defold_assets::legacy(&design.root, &folder, &mut registry)?;
    let shared_atlas = crate::defold_assets::shared_atlas(&design.root, &design.gui, &format!("{folder}/atlases/{name}.atlas"))?;
    let mut retained_textures: Vec<String> =
        remote.into_iter().flat_map(|r| &r.nodes).filter_map(|n| n.properties.get("texture").and_then(Value::as_str).map(str::to_string)).collect();
    if let Some(bytes) = saved_gui {
        retained_textures.extend(Scene::parse(String::from_utf8(bytes.to_vec()).map_err(err)?).map_err(err)?.nodes.into_iter().map(|n| n.texture));
    }
    let scene = Scene::parse(design.source.clone()).map_err(err)?;
    let mut paths: std::collections::BTreeSet<String> = registry.files.keys().chain(registry.retired.keys()).cloned().collect();
    paths.extend(registry.assets.iter().map(|a| a.image.clone()));
    let alias = format!("pc_{key}/");
    for texture in retained_textures.iter().map(String::as_str).chain(scene.nodes.iter().map(|n| n.texture.as_str())) {
        if let Some(stem) = texture.strip_prefix(&alias) {
            paths.insert(format!("{folder}/images/{stem}.png"));
        }
    }
    let mut resources = BTreeMap::new();
    for path in paths.into_iter().filter(|p| p.ends_with(".png")) {
        // Missing retired files are allowed. The pure planner rejects a missing file if still referenced.
        if let Ok(target) = crate::defold_cmds::resource(&design.root, &path)
            && let Ok(bytes) = crate::file_cmds::read_file(&target.to_string_lossy())
        {
            resources.insert(path, bytes);
        }
    }
    Ok(PublicationInputs {
        registry,
        resources,
        shared_atlas,
        retained_textures,
        managed_aliases: vec![],
        shared_owners: Default::default(),
        force_bake: Default::default(),
    })
}

/// Pure snapshot-to-publication calculation. No project reads, writes, editor requests or Session access.
pub(crate) fn plan_publication(mut doc: Document, inputs: PublicationInputs) -> Result<PublicationPlan> {
    let original = doc.defold.clone().ok_or_else(|| err("Not linked"))?;
    // Identity is project-relative, so a checkout moved to another computer reuses assets.
    // Pixel content, not document history or node placement, determines resource versions.
    let gui_resource = resource_path(&original.root, &original.gui)?;
    let gui_key: String = hash(gui_resource.as_bytes()).chars().take(16).collect();
    let gui_name: String = Path::new(&gui_resource)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("gui")
        .chars()
        .take(48)
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect();
    let folder = format!("/photocraft_generated/{gui_name}_{gui_key}");
    let atlas_alias = format!("pc_{}", gui_key);
    let previous_assets = inputs.registry;
    if let Some(design) = &mut doc.defold {
        for binding in &mut design.nodes {
            if binding.asset_id.is_empty() {
                binding.asset_id = previous_assets
                    .assets
                    .iter()
                    .find(|a| a.node == binding.node)
                    .map(|a| a.key.clone())
                    .or_else(|| previous_assets.nodes.iter().find(|(_, node)| **node == binding.node).map(|(key, _)| key.clone()))
                    .or_else(|| previous_assets.assets.iter().find(|a| binding.layer == binding.artwork && a.layer == binding.layer).map(|a| a.key.clone()))
                    .unwrap_or_else(|| hash(format!("{gui_resource}:node:{}", binding.node).as_bytes()).chars().take(16).collect());
            }
        }
    }
    let authored = reconcile(&mut doc)?;
    if authored.proto.fields.iter().any(|f| matches!(f.name.as_str(), "layouts")) || authored.nodes.iter().any(|n| n.kind == "TYPE_TEMPLATE" || !n.editable) {
        return Err(err("Template instances and layout overrides require reconciliation before publishing this design"));
    }
    let links = doc.defold.as_ref().ok_or_else(|| err("Missing link"))?.nodes.clone();
    let mut export_doc = doc.clone();
    crate::color_cmds::convert_document(&mut export_doc, photocraft_cms::Builtin::Srgb.profile(), photocraft_cms::Intent::RelativeColorimetric, true)?;
    let mut output = authored.clone();
    let mut images = BTreeMap::new();
    #[cfg(test)]
    let mut asset_nodes = Vec::new();
    let mut managed_assets = Vec::new();
    let mut files = Vec::new();
    let mut textures = Vec::new();
    let mut new_world = HashMap::new();
    let mut total_pixels = 0_u64;
    let mut recipes = BTreeMap::new();
    for b in &links {
        let node = authored.nodes.iter().find(|n| n.id == b.node).ok_or_else(|| err("Missing authored node"))?.clone();
        let group = doc.layer(LayerId(b.layer)).ok_or_else(|| err("Missing layer"))?;
        output = output.edit(&b.node, "enabled", &json!(group.visible)).map_err(err)?;
        let w = world(&authored, &b.node)?;
        new_world.insert(b.node.clone(), w);
        if doc
            .defold
            .as_ref()
            .and_then(|d| d.project.as_ref())
            .and_then(|p| p.layers.get(&b.layer))
            .is_some_and(|binding| matches!(binding.appearance, Some(defold::AppearanceSource::Shared { .. })))
        {
            // A shared preview is disposable. The typed bridge supplies the canonical
            // owner's verified recipe and bytes, so this path never bakes its cache.
            continue;
        }
        let label = crate::defold_assets::name(&group.name);
        let duplicate = links
            .iter()
            .filter_map(|other| doc.layer(LayerId(other.layer)))
            .filter(|layer| crate::defold_assets::name(&layer.name).eq_ignore_ascii_case(&label))
            .count()
            > 1;
        let named = format!("{folder}/images/{label}.png");
        let previous = previous_assets.assets.iter().find(|a| a.key == b.asset_id);
        let collision = previous_assets.assets.iter().any(|a| a.key != b.asset_id && a.image.to_lowercase() == named.to_lowercase());
        let stem = if let Some(previous) = previous.filter(|a| a.name == label) {
            Path::new(&previous.image).file_stem().and_then(|s| s.to_str()).ok_or_else(|| err("Invalid managed image name"))?.to_owned()
        } else if duplicate || collision {
            format!("{label}__{}", b.asset_id.chars().take(8).collect::<String>())
        } else {
            label.clone()
        };
        let png_path = format!("{folder}/images/{stem}.png");
        if !inputs.force_bake.contains(&b.asset_id)
            && !inputs.force_bake.contains(&format!("node:{}", b.node))
            && !b.appearance.is_empty()
            && appearance(&doc, b)? == b.appearance
        {
            if node.texture.starts_with(&format!("{atlas_alias}/")) || inputs.managed_aliases.iter().any(|alias| node.texture.starts_with(&format!("{alias}/")))
            {
                let bytes = captured_image(&inputs.resources, &previous_assets, &b.asset_id, &b.node, &folder, &node.texture)?;
                let digest = hash(&bytes);
                images.insert(png_path.clone(), bytes);
                managed_assets.push(crate::defold_assets::Asset {
                    key: b.asset_id.clone(),
                    node: b.node.clone(),
                    layer: b.layer,
                    image: png_path.clone(),
                    hash: digest,
                    name: label.clone(),
                });
                output = output.edit(&b.node, "texture", &json!(format!("{atlas_alias}/{stem}"))).map_err(err)?;
            }
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
        // Transparent RGB padding is not visible artwork. It must not turn a small
        // brush layer into a canvas-sized game texture or shift its effective pivot.
        let mut visible = Rect::new(0, 0, 0, 0);
        for (index, pixel) in local_buffer.px.iter().enumerate() {
            if pixel[3] > 0.0 {
                let x = r.x0 + (index % r.width().max(1) as usize) as i32;
                let y = r.y0 + (index / r.width().max(1) as usize) as i32;
                let point = Rect::new(x, y, x + 1, y + 1);
                visible = if visible.is_empty() { point } else { visible.union(&point) };
            }
        }
        if visible.is_empty() && inputs.shared_owners.contains(&b.asset_id) {
            r = Rect::new(0, 0, 1, 1);
            local_buffer = photocraft_compose::Buffer::transparent(r);
            visible = r;
        }
        if visible.is_empty() {
            // Empty artwork is a structural node, not a generated placeholder texture.
            // Visibility affects this node only; enabled/alpha still govern its children.
            output = output.edit(&b.node, "texture", &json!("")).map_err(err)?;
            output = output.edit(&b.node, "visible", &json!(false)).map_err(err)?;
            continue;
        }
        if visible != r {
            r = visible;
            local_buffer = photocraft_compose::Buffer::transparent(r);
            pixels.read_rgba_into(r, &mut local_buffer.px);
        }
        recipes.insert(
            b.node.clone(),
            crate::defold_project_model::ExportRecipe {
                width: r.width(),
                height: r.height(),
                crop_origin: [r.x0, r.y0],
                node_local_transform: [1.0, 0.0, 0.0, 1.0, f64::from(r.x0), f64::from(r.y0)],
                cross_node_composition: false,
            },
        );
        let center = Point::new((r.x0 as f64 + r.x1 as f64) * 0.5, -(r.y0 as f64 + r.y1 as f64) * 0.5);
        new_world.insert(b.node.clone(), w.mul(&Affine::translate(center.x, center.y)));
        let pixels = local_buffer.to_rgba8();
        let image = photocraft_codecs::Image::from_u8(r.width(), r.height(), photocraft_codecs::ChannelLayout::Rgba, pixels.pixels).map_err(err)?;
        let png = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).map_err(err)?;
        let digest = hash(&png);
        let alias = &atlas_alias;
        images.insert(png_path.clone(), png);
        #[cfg(test)]
        asset_nodes.push(json!({"node":b.node,"image":png_path,"hash":digest,"width":r.width(),"height":r.height()}));
        managed_assets.push(crate::defold_assets::Asset {
            key: b.asset_id.clone(),
            node: b.node.clone(),
            layer: b.layer,
            image: png_path.clone(),
            hash: digest,
            name: label.clone(),
        });
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
    if !images.is_empty() {
        if inputs.shared_atlas {
            for path in previous_assets.files.keys().filter(|p| p.ends_with(".png")) {
                let stem = Path::new(path).file_stem().and_then(|s| s.to_str()).ok_or_else(|| err("Invalid managed image path"))?;
                retain_generated_texture(&mut images, &inputs.resources, &folder, &atlas_alias, &format!("{atlas_alias}/{stem}"), &previous_assets)?;
            }
        }
        // A shared atlas must contain unchanged nodes' images too. During online
        // publication keep the old editor references valid until node updates
        // complete, since atlas registration precedes property assignment.
        for node in &output.nodes {
            retain_generated_texture(&mut images, &inputs.resources, &folder, &atlas_alias, &node.texture, &previous_assets)?;
        }
        for texture in &inputs.retained_textures {
            retain_generated_texture(&mut images, &inputs.resources, &folder, &atlas_alias, texture, &previous_assets)?;
        }
        let mut atlas = String::new();
        for image in images.keys() {
            atlas.push_str(&format!("images {{ image: {} }}\n", serde_json::to_string(image).map_err(err)?));
        }
        atlas.push_str("extrude_borders: 2\ninner_padding: 2\n");
        let atlas_path = format!("{folder}/atlases/{gui_name}.atlas");
        files.extend(images);
        files.push((atlas_path.clone(), atlas.into_bytes()));
        textures.push(json!({"name":atlas_alias,"texture":atlas_path}));
    }
    let mut registry = crate::defold_assets::Registry {
        version: 2,
        gui: gui_resource.clone(),
        assets: managed_assets,
        nodes: links.iter().map(|node| (node.asset_id.clone(), node.node.clone())).collect(),
        files: files.iter().map(|(path, bytes)| (path.clone(), hash(bytes))).collect(),
        retired: previous_assets.retired.clone(),
        #[cfg(test)]
        source_hash: None,
    };
    registry.retired.extend(previous_assets.files.iter().filter(|(path, _)| !registry.files.contains_key(*path)).map(|(p, h)| (p.clone(), h.clone())));
    let registry_bytes = serde_json::to_vec_pretty(&registry).map_err(err)?;
    if registry_bytes.len() > 4 * 1024 * 1024 {
        return Err(err("Asset registry exceeds limit"));
    }
    files.push((format!("{folder}/assets.json"), registry_bytes));
    let aliases = [&*atlas_alias];
    let mut raw = output.proto.source.clone();
    for f in output.proto.fields.iter().rev().filter(|f| f.name == "textures" && aliases.contains(&output.proto.text(&f.children, "name").as_str())) {
        raw.replace_range(f.span.clone(), "");
    }
    for t in &textures {
        raw.push_str(&format!("\ntextures {{ name: {} texture: {} }}\n", t["name"], t["texture"]));
    }
    output = Scene::parse(raw).map_err(err)?;
    let renames: Vec<_> = links
        .iter()
        .filter_map(|node| {
            let previous = previous_assets
                .nodes
                .get(&node.asset_id)
                .cloned()
                .or_else(|| previous_assets.assets.iter().find(|a| a.key == node.asset_id).map(|a| a.node.clone()))
                .or_else(|| original.nodes.iter().find(|old| old.layer == node.layer).map(|old| old.node.clone()))?;
            (previous != node.node).then(|| json!({"from":previous,"to":node.node}))
        })
        .collect();
    Ok(PublicationPlan {
        document: doc,
        #[cfg(test)]
        authored,
        output,
        links,
        #[cfg(test)]
        folder,
        files,
        textures,
        #[cfg(test)]
        asset_nodes,
        registry,
        renames,
        recipes,
    })
}

#[cfg(test)]
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
    let online = crate::peer_binding::online(&original.root);
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
    let inputs = capture_publication_inputs(&doc, remote.as_ref(), online.then_some(disk.as_slice()))?;
    let previous_assets = inputs.registry.clone();
    let PublicationPlan { document: planned, authored, output, links: _, folder, files, textures, asset_nodes, mut registry, renames, recipes: _ } =
        plan_publication(doc, inputs)?;
    doc = planned;
    let draft = photocraft_format::save_to_bytes(&doc, &Default::default()).map_err(err)?;
    // Save the editable source before any asset or GUI publication.
    let source_path = native_path(&original.gui);
    crate::file_cmds::write_file(&source_path, &draft)?;
    let staged = crate::defold_assets::stage(&original.root, &folder, &previous_assets, &files)?;
    let baseline_remote = remote.clone();
    let publication = (|| -> Result<Option<String>> {
        let next_remote = if let Some(remote) = remote {
            let snap = crate::defold_live::request(
                &original.root,
                &json!({"op":"publishDesign","path":path,"expected":remote.nodes,"directory":folder,"textures":textures,"renames":renames,"nodes":live_nodes(&output)?}),
            )?;
            Some(serde_json::to_string(&snap).map_err(err)?)
        } else {
            if hash(&crate::file_cmds::read_file(&original.gui)?) != expected.0 {
                return Err(err("GUI changed during export"));
            }
            crate::file_cmds::write_file(&original.gui, output.proto.source.as_bytes())?;
            None
        };
        Ok(next_remote)
    })();
    let next_remote = match publication {
        Ok(result) => result,
        Err(e) => {
            let unchanged = if let Some(baseline) = baseline_remote {
                crate::defold_live::request(&original.root, &json!({"op":"snapshot","path":path}))
                    .is_ok_and(|snapshot| crate::defold_live::same_nodes(&snapshot.nodes, &baseline.nodes))
            } else {
                crate::file_cmds::read_file(&original.gui).is_ok_and(|bytes| hash(&bytes) == expected.0)
            };
            if unchanged {
                staged.rollback();
            }
            return Err(e);
        }
    };
    // Retirement never turns a successful GUI publication into a failed save.
    let mut asset_warnings = crate::defold_assets::retire(&original.root, &mut registry);
    let current_registry = crate::defold_assets::load(&original.root, &folder)?;
    if let Err(e) = crate::defold_assets::stage(
        &original.root,
        &folder,
        &current_registry,
        &[(format!("{folder}/assets.json"), serde_json::to_vec_pretty(&registry).map_err(err)?)],
    ) {
        asset_warnings.push(e.to_string());
    }
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
    Ok(
        json!({"synced":true,"online":online,"assets":asset_nodes.len(),"nodes":output.nodes.len(),"editorSaveRequired":online,"retainedAssets":registry.retired.len(),"warnings":canvas_warnings.into_iter().chain(asset_warnings).collect::<Vec<_>>()}),
    )
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
        #[cfg(test)]
        CommandSpec {
            id: "defold.testLegacySync",
            label: "Test legacy Defold adapter",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled,
            run: sync,
            journal: false,
        },
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
