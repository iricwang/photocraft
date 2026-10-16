//! Structural role maintenance. Called only by structural edits, never by brush/move frames.
use crate::{Result, defold_cmds::err, defold_project_model::new_id};
use photocraft_doc::{
    Document, Layer, LayerContent, LayerId,
    defold::{AppearanceSource, LayerBinding, LayerRole},
};
use std::collections::{BTreeMap, BTreeSet};

fn binding(role: LayerRole) -> Result<LayerBinding> {
    Ok(LayerBinding {
        layer_id: new_id().map_err(err)?,
        role,
        node_id: if role == LayerRole::GuiNode { Some(new_id().map_err(err)?) } else { None },
        asset_id: if role == LayerRole::GuiNode { Some(new_id().map_err(err)?) } else { None },
        gui_node_id: None,
        appearance: (role == LayerRole::GuiNode).then_some(AppearanceSource::SelfLayer),
        artwork_fingerprint: None,
    })
}

/// Attach identities to a legacy/native import without changing its pixels or tree.
/// Imported artwork is explicit, while an ordinary sibling added to the node group is structural.
pub(crate) fn initialize(doc: &mut Document) -> Result<()> {
    let Some(design) = doc.defold.as_ref() else { return Ok(()) };
    let Some(project) = design.project.as_ref() else { return Ok(()) };
    let mut layers = project.layers.clone();
    for node in &design.nodes {
        if doc.layer(LayerId(node.layer)).is_none() {
            continue;
        }
        let imported = !layers.contains_key(&node.layer);
        if imported {
            layers.insert(node.layer, binding(LayerRole::GuiNode)?);
        }
        if node.artwork != node.layer && doc.layer(LayerId(node.artwork)).is_some() && !layers.contains_key(&node.artwork) {
            layers.insert(node.artwork, binding(LayerRole::ArtworkRoot)?);
        }
        let root = layers.get(&node.artwork).map(|b| b.layer_id.clone());
        if let Some(b) = layers.get_mut(&node.layer) {
            b.gui_node_id = Some(node.node.clone());
            if imported && node.artwork != node.layer {
                b.appearance = root.map(|root_layer_id| AppearanceSource::Subtree { root_layer_id });
            }
        }
    }
    // One-time legacy migration: AI provenance identifies old appearance siblings.
    // Once explicit roles exist, new operations never infer a role from names or history.
    for id in doc.ai_generations.iter().flat_map(|g| &g.versions).filter_map(|v| v.layer) {
        if !layers.contains_key(&id.0) && crate::defold_design::node_for_layer(doc, id).is_some() {
            layers.insert(id.0, binding(LayerRole::ArtworkLayer)?);
        }
    }
    if let Some(project) = doc.defold.as_mut().and_then(|d| d.project.as_mut()) {
        project.layers = layers;
    }
    structure_changed(doc)
}

/// Assign defaults from the actual insertion parent. Existing identities survive moves/Undo.
/// This function performs metadata work only: no rendering, resource reads, hashing or writes.
pub(crate) fn structure_changed(doc: &mut Document) -> Result<()> {
    let Some(project) = doc.defold.as_ref().and_then(|d| d.project.as_ref()) else { return Ok(()) };
    let mut bindings = project.layers.clone();
    let mut live = BTreeSet::new();
    fn visit(layers: &[Layer], inherited: Option<LayerRole>, bindings: &mut BTreeMap<u64, LayerBinding>, live: &mut BTreeSet<u64>, depth: usize) -> Result<()> {
        if depth > 128 {
            return Err(err("Layer nesting limit"));
        }
        for layer in layers {
            live.insert(layer.id.0);
            let in_artwork = matches!(inherited, Some(LayerRole::ArtworkRoot | LayerRole::ArtworkLayer));
            if let std::collections::btree_map::Entry::Vacant(entry) = bindings.entry(layer.id.0) {
                let role = if inherited == Some(LayerRole::DesignOnly) {
                    LayerRole::DesignOnly
                } else if in_artwork || layer.clipped || matches!(layer.content, LayerContent::Adjustment(_)) {
                    LayerRole::ArtworkLayer
                } else {
                    LayerRole::GuiNode
                };
                entry.insert(binding(role)?);
            }
            let role = bindings.get(&layer.id.0).map(|b| b.role).ok_or_else(|| err("Missing layer role"))?;
            if in_artwork && role == LayerRole::GuiNode {
                return Err(err("Move GUI nodes into the node structure, not inside an artwork container"));
            }
            if let LayerContent::Group(group) = &layer.content {
                visit(&group.children, Some(role), bindings, live, depth + 1)?;
            }
        }
        Ok(())
    }
    visit(&doc.layers, None, &mut bindings, &mut live, 0)?;
    bindings.retain(|id, _| live.contains(id));
    if let Some(project) = doc.defold.as_mut().and_then(|d| d.project.as_mut()) {
        project.layers = bindings;
    }
    crate::defold_design::reconcile(doc)?;
    Ok(())
}

/// A self-layer node becomes a group once when a second artwork layer is inserted.
/// Its node, asset and stable carrier identity follow the group; the pixel gains an artwork identity.
pub(crate) fn promoted(doc: &mut Document, original: LayerId, carrier: LayerId) -> Result<()> {
    let Some(project) = doc.defold.as_mut().and_then(|d| d.project.as_mut()) else { return Ok(()) };
    let Some(mut node) = project.layers.remove(&original.0) else { return Ok(()) };
    let artwork = binding(LayerRole::ArtworkRoot)?;
    node.appearance = Some(AppearanceSource::Subtree { root_layer_id: artwork.layer_id.clone() });
    project.layers.insert(original.0, artwork);
    project.layers.insert(carrier.0, node);
    Ok(())
}

/// Keep an explicit artwork root around all versions. A pixel root is wrapped at insertion,
/// in the caller's native Undo transaction, rather than mutating the tree at publication.
pub(crate) fn artwork_container(doc: &mut Document, carrier: LayerId, original: LayerId) -> Result<LayerId> {
    if doc.defold.as_ref().and_then(|d| d.project.as_ref()).is_none() {
        return Ok(carrier);
    }
    let root_id = doc.defold.as_ref().and_then(|d| d.project.as_ref()).and_then(|p| p.layers.get(&carrier.0)).and_then(|b| match &b.appearance {
        Some(AppearanceSource::Subtree { root_layer_id }) => Some(root_layer_id.clone()),
        _ => None,
    });
    let root = root_id
        .and_then(|stable| doc.defold.as_ref()?.project.as_ref()?.layers.iter().find(|(_, b)| b.layer_id == stable).map(|(id, _)| LayerId(*id)))
        .unwrap_or(original);
    if doc.layer(root).is_some_and(|l| matches!(l.content, LayerContent::Group(_))) {
        return Ok(root);
    }
    let layer = doc.layer_mut(root).ok_or_else(|| err("Node artwork is missing"))?;
    let wrapper = Layer::group("Artwork", Vec::new());
    let id = wrapper.id;
    let old = std::mem::replace(layer, wrapper);
    if let LayerContent::Group(group) = &mut layer.content {
        group.children.push(old);
    }
    let project = doc.defold.as_mut().and_then(|d| d.project.as_mut()).ok_or_else(|| err("Project link missing"))?;
    let root_binding = binding(LayerRole::ArtworkRoot)?;
    if let Some(b) = project.layers.get_mut(&root.0) {
        b.role = LayerRole::ArtworkLayer;
    }
    if let Some(node) = project.layers.get_mut(&carrier.0) {
        node.appearance = Some(AppearanceSource::Subtree { root_layer_id: root_binding.layer_id.clone() });
    }
    project.layers.insert(id.0, root_binding);
    Ok(id)
}

/// Copy a GUI subtree with fresh identities but the same internal roles and authored fields.
/// Plain pasted artwork is classified at its destination by `structure_changed` instead.
pub(crate) fn copy_roles(doc: &mut Document, source: &Document, original: &Layer, copy: &Layer) -> Result<()> {
    let Some(source_design) = source.defold.as_ref() else { return Ok(()) };
    let Some(source_project) = source_design.project.as_ref() else { return Ok(()) };
    if source_project.layers.get(&original.id.0).is_none_or(|b| b.role != LayerRole::GuiNode) || doc.defold.as_ref().and_then(|d| d.project.as_ref()).is_none()
    {
        return Ok(());
    }
    fn pairs(a: &Layer, b: &Layer, ids: &mut BTreeMap<u64, u64>) {
        ids.insert(a.id.0, b.id.0);
        if let (LayerContent::Group(a), LayerContent::Group(b)) = (&a.content, &b.content) {
            for (a, b) in a.children.iter().zip(&b.children) {
                pairs(a, b, ids);
            }
        }
    }
    let mut ids = BTreeMap::new();
    pairs(original, copy, &mut ids);
    let mut layers = BTreeMap::new();
    let mut stable = BTreeMap::new();
    for (old, new) in &ids {
        if let Some(previous) = source_project.layers.get(old) {
            let fresh = binding(previous.role)?;
            stable.insert(previous.layer_id.clone(), fresh.layer_id.clone());
            layers.insert(*new, fresh);
        }
    }
    for (old, new) in &ids {
        if let (Some(previous), Some(fresh)) = (source_project.layers.get(old), layers.get_mut(new)) {
            if let Some(AppearanceSource::Shared { source }) = &previous.appearance {
                fresh.asset_id = Some(source.asset_id.clone());
            }
            fresh.appearance = match &previous.appearance {
                Some(AppearanceSource::Subtree { root_layer_id }) => {
                    stable.get(root_layer_id).cloned().map(|root_layer_id| AppearanceSource::Subtree { root_layer_id })
                }
                other => other.clone(),
            };
        }
    }
    let source_scene = photocraft_defold_format::Scene::parse(source_design.source.clone()).map_err(err)?;
    let target = doc.defold.as_mut().ok_or_else(|| err("Missing GUI link"))?;
    for node in &source_design.nodes {
        let Some(layer) = ids.get(&node.layer) else { continue };
        let fresh = layers.get_mut(layer).ok_or_else(|| err("Missing copied node identity"))?;
        let suffix = fresh.node_id.as_ref().ok_or_else(|| err("Missing copied node identity"))?.as_str().replace('-', "");
        let id = format!("{}__{}", crate::defold_assets::name(&node.node), &suffix[..8]);
        let field = source_scene
            .proto
            .fields
            .iter()
            .find(|f| f.name == "nodes" && source_scene.proto.text(&f.children, "id") == node.node)
            .ok_or_else(|| err("Missing copied GUI node"))?;
        let mut raw = source_scene.proto.source.get(field.span.clone()).ok_or_else(|| err("Invalid node source range"))?.to_string();
        let mut edits = Vec::new();
        for field_value in field.children.iter().filter(|f| matches!(f.name.as_str(), "id" | "parent")) {
            let value = if field_value.name == "id" { id.as_str() } else { "" };
            edits.push((field_value.value.start - field.span.start..field_value.value.end - field.span.start, serde_json::to_string(value).map_err(err)?));
        }
        edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        for (range, value) in edits {
            raw.replace_range(range, &value);
        }
        target.source.push('\n');
        target.source.push_str(&raw);
        let mut copied = node.clone();
        copied.layer = *layer;
        copied.artwork = *ids.get(&node.artwork).unwrap_or(layer);
        copied.node = id.clone();
        copied.asset_id = fresh.asset_id.as_ref().ok_or_else(|| err("Missing copied asset identity"))?.as_str().to_string();
        copied.appearance.clear();
        fresh.gui_node_id = Some(id);
        target.nodes.push(copied);
    }
    if let Some(project) = target.project.as_mut() {
        project.layers.extend(layers);
    }
    Ok(())
}
