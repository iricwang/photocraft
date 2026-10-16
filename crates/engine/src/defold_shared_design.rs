//! Native shared artwork: immutable owner references and disposable consumer previews.
use crate::{Result, defold_cmds::err, defold_design, defold_project_model::new_id};
use photocraft_doc::{
    defold::{AppearanceSource, ArtworkSourceNode, AssetId, LayerBinding, LayerRole, SharedAppearanceRef},
    *,
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn shared_for_layer(doc: &Document, id: LayerId) -> Option<&SharedAppearanceRef> {
    let carrier = defold_design::node_for_layer(doc, id)?;
    match &doc.defold.as_ref()?.project.as_ref()?.layers.get(&carrier.layer)?.appearance {
        Some(AppearanceSource::Shared { source }) => Some(source),
        _ => None,
    }
}
pub(crate) fn require_editable_artwork(doc: &Document, id: LayerId) -> Result<()> {
    if shared_for_layer(doc, id).is_some() {
        return Err(err("Shared artwork is read-only here; open its source or make it independent first"));
    }
    Ok(())
}
fn neutral(layer: &Layer) -> bool {
    layer.visible
        && layer.opacity == 1.0
        && layer.fill_opacity == 1.0
        && !layer.clipped
        && layer.mask.is_none()
        && layer.vector_mask.is_none()
        && layer.effects == Layer::group("", vec![]).effects
        && matches!(layer.blend, BlendMode::Normal | BlendMode::PassThrough)
        && layer.excluded_channels == 0
        && layer.blend_if == Default::default()
        && layer.advanced == Default::default()
}
fn carrier<'a>(doc: &'a Document, node: &ArtworkSourceNode) -> Result<&'a defold::NodeLink> {
    let design = doc.defold.as_ref().ok_or_else(|| err("Shared source has no GUI document"))?;
    let project = design.project.as_ref().ok_or_else(|| err("Shared source has no project identity"))?;
    if project.scene_id != node.scene_id {
        return Err(err("Shared source scene identity changed"));
    }
    let (id, role) = project
        .layers
        .iter()
        .find(|(_, role)| role.layer_id == node.root_layer_id && role.node_id.as_ref() == Some(&node.node_id))
        .ok_or_else(|| err("Shared source carrier was deleted"))?;
    if role.role != LayerRole::GuiNode || matches!(role.appearance, Some(AppearanceSource::Shared { .. })) {
        return Err(err("A shared appearance must refer directly to an editable owner node"));
    }
    design.nodes.iter().find(|link| link.layer == *id).ok_or_else(|| err("Shared source node link is missing"))
}
/// Extract the complete editable subtree into canonical node-local Y-down coordinates.
/// The caller supplies one verified immutable owner package; no file or host access occurs here.
fn transform_unlocked(layer: &mut Layer, affine: Affine) -> Result<()> {
    fn collect(layer: &mut Layer, locks: &mut BTreeMap<u64, Locks>) {
        locks.insert(layer.id.0, std::mem::take(&mut layer.locks));
        if let Some(children) = layer.children_mut() {
            for child in children {
                collect(child, locks);
            }
        }
    }
    fn restore(layer: &mut Layer, locks: &BTreeMap<u64, Locks>) {
        if let Some(original) = locks.get(&layer.id.0) {
            layer.locks = *original;
        }
        if let Some(children) = layer.children_mut() {
            for child in children {
                restore(child, locks);
            }
        }
    }
    let mut locks = BTreeMap::new();
    collect(layer, &mut locks);
    let result = defold_design::transform(layer, affine);
    restore(layer, &locks);
    result
}
pub(crate) fn extract_shared_artwork(doc: &Document, node: &ArtworkSourceNode) -> Result<Layer> {
    let link = carrier(doc, node)?;
    let scene = defold_design::document_scene(doc)?;
    let authored = scene.nodes.iter().find(|node| node.id == link.node).ok_or_else(|| err("Shared source GUI node is missing"))?;
    let instance_neutral = |layer: &Layer| layer.visible && layer.opacity == 1.0 && layer.fill_opacity == 1.0;
    if !authored.enabled
        || !authored.visible
        || authored.alpha != 1.0
        || authored.color != [1.0; 4]
        || doc.layer(LayerId(link.layer)).is_none_or(|layer| !instance_neutral(layer))
        || doc.layer(LayerId(link.artwork)).is_none_or(|layer| !instance_neutral(layer))
    {
        return Err(err(format!(
            "Shared owner {} / {} has instance visibility, opacity or tint that cannot be separated from its artwork yet; restore the owner instance style or make its consumers independent",
            doc.defold.as_ref().map(|d| d.gui.as_str()).unwrap_or_default(),
            link.node
        )));
    }
    let path = doc.path_of(LayerId(link.layer)).ok_or_else(|| err("Shared source layer is missing"))?;
    for depth in 1..path.len() {
        if path.get(..depth).and_then(|p| doc.layer_at(p)).is_none_or(|layer| !neutral(layer)) {
            return Err(err("Shared artwork cannot depend on masks, effects, opacity or blending from ancestors"));
        }
    }
    let own = doc.layer(LayerId(link.layer)).ok_or_else(|| err("Source node missing"))?;
    if own.clipped
        || !matches!(own.blend, BlendMode::Normal | BlendMode::PassThrough)
        || own.excluded_channels != 0
        || own.blend_if != Default::default()
        || own.advanced != Default::default()
    {
        return Err(err("Shared node artwork cannot depend on another node's backdrop"));
    }
    let mut layer = defold_design::own_layer(doc, link)?;
    fn canvas_dependencies(layer: &Layer) -> bool {
        let fill = match &layer.content {
            LayerContent::Fill(fill) => Some(fill),
            LayerContent::Shape(shape) => shape.fill.as_ref(),
            _ => None,
        };
        fill.is_some_and(|fill| matches!(fill, Fill::Pattern { .. } | Fill::Gradient { align: false, .. }))
            || layer.effects.items.iter().any(|effect| match effect {
                Effect::PatternOverlay { .. } => true,
                Effect::Stroke(stroke) => matches!(stroke.paint, FxPaint::Pattern { .. }),
                Effect::OuterGlow(glow) | Effect::InnerGlow(glow) => matches!(glow.paint, FxPaint::Pattern { .. }),
                _ => false,
            })
            || layer.children().is_some_and(|children| children.iter().any(canvas_dependencies))
    }
    if canvas_dependencies(&layer) {
        return Err(err(
            "Shared artwork cannot carry document pattern libraries or canvas-aligned fills; keep this node local or rasterize its appearance first",
        ));
    }

    fn freeze_light(layer: &mut Layer, light: &GlobalLight) {
        for effect in &mut layer.effects.items {
            match effect {
                Effect::DropShadow(shadow) | Effect::InnerShadow(shadow) if shadow.use_global_light => {
                    shadow.use_global_light = false;
                    shadow.angle = light.angle;
                }
                Effect::BevelEmboss(bevel) if bevel.use_global_light => {
                    bevel.use_global_light = false;
                    bevel.angle = light.angle;
                    bevel.altitude = light.altitude;
                }
                _ => {}
            }
        }
        if let Some(children) = layer.children_mut() {
            for child in children {
                freeze_light(child, light);
            }
        }
    }
    freeze_light(&mut layer, &doc.global_light);
    let world = defold_design::world(&scene, &link.node)?;
    let flip = Affine { m: [1.0, 0.0, 0.0, -1.0, 0.0, 0.0] };
    transform_unlocked(
        &mut layer,
        flip.mul(&world.inverse().ok_or_else(|| err("Shared source has zero scale"))?)
            .mul(&defold_design::document_screen(doc).inverse().ok_or_else(|| err("Invalid shared canvas transform"))?),
    )?;
    Ok(layer)
}
/// Instantiate canonical native content in a consumer without flattening masks/text/AI layers.
pub(crate) fn instantiate_shared_artwork(doc: &Document, node: &str, canonical: &Layer) -> Result<Layer> {
    let scene = defold_design::document_scene(doc)?;
    let flip = Affine { m: [1.0, 0.0, 0.0, -1.0, 0.0, 0.0] };
    let mut layer = canonical.duplicate();
    transform_unlocked(&mut layer, defold_design::document_screen(doc).mul(&defold_design::world(&scene, node)?).mul(&flip))?;
    Ok(layer)
}
fn binding(role: LayerRole) -> Result<LayerBinding> {
    Ok(LayerBinding { layer_id: new_id().map_err(err)?, role, node_id: None, asset_id: None, gui_node_id: None, appearance: None, artwork_fingerprint: None })
}
fn index_artwork(layer: &Layer, root: bool, bindings: &mut BTreeMap<u64, LayerBinding>) -> Result<()> {
    bindings.insert(layer.id.0, binding(if root { LayerRole::ArtworkRoot } else { LayerRole::ArtworkLayer })?);
    if let Some(children) = layer.children() {
        for child in children {
            index_artwork(child, false, bindings)?;
        }
    }
    Ok(())
}
fn keep_structural_children(layer: &Layer, gui: &BTreeSet<u64>, out: &mut Vec<Layer>) {
    if let Some(children) = layer.children() {
        for child in children {
            if gui.contains(&child.id.0) {
                out.push(child.clone());
            } else {
                keep_structural_children(child, gui, out);
            }
        }
    }
}
fn replace_proxy(doc: &mut Document, carrier_id: LayerId, mut artwork: Layer, readonly: bool) -> Result<()> {
    let design = doc.defold.as_ref().ok_or_else(|| err("Missing GUI"))?;
    let gui: BTreeSet<_> = design.nodes.iter().map(|n| n.layer).collect();
    let old = doc.layer(carrier_id).ok_or_else(|| err("Consumer node was deleted"))?;
    let mut children = vec![];
    keep_structural_children(old, &gui, &mut children);
    if readonly {
        fn unlock(layer: &mut Layer) {
            layer.locks = Locks::default();
            if let Some(children) = layer.children_mut() {
                for child in children {
                    unlock(child);
                }
            }
        }
        unlock(&mut artwork);
        artwork.name = "Shared artwork (read-only)".into();
        artwork.locks.pixels = true;
    }
    let previous_root = design
        .nodes
        .iter()
        .find(|n| n.layer == carrier_id.0)
        .and_then(|n| design.project.as_ref()?.layers.get(&n.artwork))
        .filter(|b| b.role == LayerRole::ArtworkRoot)
        .map(|b| b.layer_id.clone());
    let artwork_id = artwork.id;
    let mut bindings = BTreeMap::new();
    index_artwork(&artwork, true, &mut bindings)?;
    if readonly
        && let Some(stable) = previous_root
        && let Some(role) = bindings.get_mut(&artwork_id.0)
    {
        role.layer_id = stable;
    }
    children.insert(0, artwork);
    let mut wrapper = Layer::group(old.name.clone(), children);
    wrapper.id = old.id;
    wrapper.visible = old.visible;
    wrapper.label = old.label;
    *doc.layer_mut(carrier_id).ok_or_else(|| err("Consumer node was deleted"))? = wrapper;
    let design = doc.defold.as_mut().ok_or_else(|| err("Missing GUI"))?;
    let link = design.nodes.iter_mut().find(|n| n.layer == carrier_id.0).ok_or_else(|| err("Consumer node link is missing"))?;
    link.artwork = artwork_id.0;
    link.appearance.clear();
    design.project.as_mut().ok_or_else(|| err("Missing project"))?.layers.extend(bindings);
    Ok(())
}
/// Preview caches are excluded from the persisted creative source and dependency capture.
/// The carrier, authored GUI properties and child GUI nodes remain intact.
pub(crate) fn strip_shared_caches(doc: &mut Document) -> Result<()> {
    let Some(project) = doc.defold.as_ref().and_then(|d| d.project.as_ref()) else { return Ok(()) };
    let ids: Vec<_> =
        project.layers.iter().filter_map(|(id, b)| matches!(b.appearance, Some(AppearanceSource::Shared { .. })).then_some(LayerId(*id))).collect();
    for id in ids {
        replace_proxy(doc, id, Layer::group("Shared artwork", vec![]), true)?;
    }
    crate::defold_design_reconcile::structure_changed(doc)
}
pub(crate) fn hydrate_shared_document(doc: &mut Document, sources: &BTreeMap<AssetId, (Document, SharedAppearanceRef)>) -> Result<()> {
    let Some(project) = doc.defold.as_ref().and_then(|d| d.project.as_ref()) else { return Ok(()) };
    let refs: Vec<_> = project
        .layers
        .iter()
        .filter_map(|(id, b)| match &b.appearance {
            Some(AppearanceSource::Shared { source }) => Some((LayerId(*id), source.clone())),
            _ => None,
        })
        .collect();
    for (id, reference) in refs {
        let (owner, verified) = sources.get(&reference.asset_id).ok_or_else(|| err("Pinned shared source is missing"))?;
        if verified != &reference {
            return Err(err("Shared source version does not match its pinned reference"));
        }
        let owner_project = owner.defold.as_ref().and_then(|d| d.project.as_ref()).ok_or_else(|| err("Shared owner has no project identity"))?;
        if owner_project.package_id != reference.package_id
            || owner_project.project_id != doc.defold.as_ref().and_then(|d| d.project.as_ref()).ok_or_else(|| err("Missing consumer project"))?.project_id
        {
            return Err(err("Shared source belongs to another project or package"));
        }
        let node = doc.defold.as_ref().and_then(|d| d.nodes.iter().find(|n| n.layer == id.0)).ok_or_else(|| err("Consumer node link missing"))?.node.clone();
        if owner.mode != doc.mode || owner.icc_profile != doc.icc_profile {
            return Err(err("Shared native artwork requires the same color mode and profile as its owner"));
        }
        let canonical = extract_shared_artwork(owner, &reference.source_node)?;
        let artwork = instantiate_shared_artwork(doc, &node, &canonical)?;
        replace_proxy(doc, id, artwork, true)?;
    }
    crate::defold_design_reconcile::structure_changed(doc)
}

#[derive(Default)]
struct SharedIndex {
    document: std::sync::Weak<Document>,
    carriers: std::sync::Arc<Vec<LayerId>>,
}
std::thread_local! { static SHARED_INDEX: std::cell::RefCell<SharedIndex> = std::cell::RefCell::new(SharedIndex::default()); }
fn shared_carriers(document: &std::sync::Arc<Document>) -> std::sync::Arc<Vec<LayerId>> {
    SHARED_INDEX.with(|cached| {
        let mut cached = cached.borrow_mut();
        if cached.document.upgrade().is_none_or(|previous| !std::sync::Arc::ptr_eq(&previous, document)) {
            cached.document = std::sync::Arc::downgrade(document);
            cached.carriers = document
                .defold
                .as_ref()
                .and_then(|d| d.project.as_ref())
                .into_iter()
                .flat_map(|p| p.layers.iter())
                .filter_map(|(id, b)| matches!(b.appearance, Some(AppearanceSource::Shared { .. })).then_some(LayerId(*id)))
                .collect::<Vec<_>>()
                .into();
        }
        cached.carriers.clone()
    })
}

/// Dispatch-time protection for the selected/explicit affected subtree only. No renders or I/O.
pub(crate) fn command_precondition(session: &crate::Session, command: &str, params: &serde_json::Value) -> Result<()> {
    let Some(state) = session.active() else { return Ok(()) };
    if state.doc.defold.as_ref().and_then(|d| d.project.as_ref()).is_none() {
        return Ok(());
    }
    let Some(spec) = crate::commands::find(command) else { return Ok(()) };
    if (!spec.journal && !command.starts_with("ai."))
        || command.starts_with("file.")
        || command.starts_with("select.")
        || command.starts_with("view.")
        || command.starts_with("tool.")
        || command.starts_with("document.")
        || command.starts_with("layer.select")
        || matches!(command, "edit.undo" | "edit.redo" | "edit.copy" | "edit.copyMerged")
        || command.starts_with("defold.") && command != "defold.setDesignProperties" && command != "defold.addDesignNode"
    {
        return Ok(());
    }
    // Menu/tool enablement runs many times per frame. Reuse this immutable snapshot's
    // role index; Weak identity also invalidates correctly after Arc::make_mut.
    let shared = shared_carriers(&state.doc);
    if shared.is_empty() {
        return Ok(());
    }
    let failure = || err("Shared artwork is read-only here; open its source or make it independent first");
    if command.starts_with("image.") || matches!(command, "layer.flattenImage" | "layer.mergeVisible") {
        return Err(failure());
    }
    let mut targets = state.selected_layers();
    for key in ["layer", "parent", "targetLayer", "destination"] {
        if let Some(id) = params.get(key).and_then(serde_json::Value::as_u64) {
            targets.push(LayerId(id));
        }
    }
    for key in ["layers", "targets"] {
        targets.extend(params.get(key).and_then(serde_json::Value::as_array).into_iter().flatten().filter_map(serde_json::Value::as_u64).map(LayerId));
    }
    let structure = matches!(
        command,
        "layer.renameLayer"
            | "layer.delete"
            | "layer.duplicate"
            | "layer.hideLayers"
            | "layer.showLayers"
            | "layer.new.layer"
            | "layer.new.group"
            | "defold.addDesignNode"
    ) || command.starts_with("layer.arrange.");
    let movement = command == "layer.translate" && params.get("target").and_then(serde_json::Value::as_str).is_none_or(|target| target == "pixels");
    for target in targets {
        let Some(path) = state.doc.path_of(target) else { continue };
        for carrier in shared.iter() {
            let Some(shared_path) = state.doc.path_of(*carrier) else { continue };
            if path.starts_with(&shared_path) {
                if command == "defold.setDesignProperties" {
                    let allowed = [
                        "id",
                        "position.x",
                        "position.y",
                        "position.z",
                        "rotation.x",
                        "rotation.y",
                        "rotation.z",
                        "scale.x",
                        "scale.y",
                        "scale.z",
                        "enabled",
                        "visible",
                        "layer",
                    ];
                    if params
                        .get("values")
                        .and_then(serde_json::Value::as_object)
                        .is_some_and(|values| values.keys().all(|key| allowed.contains(&key.as_str())))
                    {
                        continue;
                    }
                }
                if target != *carrier {
                    return Err(failure());
                }
                let presentation = command == "layer.setProps"
                    && params
                        .as_object()
                        .is_some_and(|values| values.keys().all(|key| ["layer", "name", "visible", "label", "coalesce"].contains(&key.as_str())));
                if !structure && !movement && !presentation {
                    return Err(failure());
                }
            } else if shared_path.starts_with(&path) && !structure && !movement {
                return Err(failure());
            }
        }
    }
    Ok(())
}

/// A native move of a shared preview changes its GUI placement, never its canonical pixels.
pub(crate) fn translated(doc: &mut Document, roots: &[LayerId], dx: i32, dy: i32) -> Result<()> {
    let Some(design) = doc.defold.as_ref() else { return Ok(()) };
    let Some(project) = design.project.as_ref() else { return Ok(()) };
    for id in roots {
        if shared_for_layer(doc, *id).is_some() && defold_design::node_for_layer(doc, *id).is_some_and(|node| node.layer != id.0) {
            return Err(err("Move the GUI node carrier, not its shared artwork preview"));
        }
    }
    let root_paths: Vec<_> = roots.iter().filter_map(|id| doc.path_of(*id)).collect();
    let affected: Vec<_> = design
        .nodes
        .iter()
        .filter(|node| project.layers.get(&node.layer).is_some_and(|role| matches!(role.appearance, Some(AppearanceSource::Shared { .. }))))
        .filter(|node| doc.path_of(LayerId(node.layer)).is_some_and(|path| root_paths.iter().any(|root| path.starts_with(root))))
        .map(|node| node.node.clone())
        .collect();
    if affected.is_empty() {
        return Ok(());
    }
    let mut scene = defold_design::document_scene(doc)?;
    let original = scene.clone();
    for id in &affected {
        let node = original.nodes.iter().find(|n| &n.id == id).ok_or_else(|| err("Shared GUI node is missing"))?;
        let mut ancestor = node.parent.as_str();
        let mut inherited = false;
        for _ in 0..128 {
            if ancestor.is_empty() {
                break;
            }
            if affected.iter().any(|id| id == ancestor) {
                inherited = true;
                break;
            }
            ancestor = original.nodes.iter().find(|n| n.id == ancestor).ok_or_else(|| err("Shared GUI parent missing"))?.parent.as_str();
        }
        if inherited {
            continue;
        }
        let parent = if node.parent.is_empty() { Affine::IDENTITY } else { defold_design::world(&original, &node.parent)? };
        let inverse = parent.inverse().ok_or_else(|| err("Shared GUI parent has zero scale"))?;
        let origin = inverse.apply(photocraft_geom::Point::new(0.0, 0.0));
        let shifted = inverse.apply(photocraft_geom::Point::new(f64::from(dx), -f64::from(dy)));
        scene = scene
            .edit(id, "position.x", &serde_json::json!(node.position[0] + shifted.x - origin.x))
            .map_err(err)?
            .edit(id, "position.y", &serde_json::json!(node.position[1] + shifted.y - origin.y))
            .map_err(err)?;
    }
    doc.defold.as_mut().ok_or_else(|| err("Missing GUI"))?.source = scene.proto.source;
    Ok(())
}

fn active_capture(session: &crate::Session) -> Result<(u64, u64, Document, crate::defold_project_client::ClientBinding, LayerId)> {
    let state = session.active().ok_or(crate::EngineError::NoDocument)?;
    let link = defold_design::selected(session).ok_or_else(|| err("Select a GUI node"))?;
    let binding = session.project_client.bindings.get(&state.open_id).cloned().ok_or_else(|| err("Connect the GUI to its project host first"))?;
    Ok((state.open_id, state.revision, state.doc.as_ref().clone(), binding, LayerId(link.layer)))
}
fn read_owner(binding: &crate::defold_project_client::ClientBinding, reference: &SharedAppearanceRef, root: &str) -> Result<Document> {
    use crate::defold_project_model::*;
    let package = binding
        .store
        .read_source(&ReadSourceRequest {
            context: binding.context.clone(),
            package_id: reference.package_id.clone(),
            source_revision: Some(reference.source_revision.clone()),
        })
        .map_err(err)?;
    if package.envelope.package_id != reference.package_id || package.envelope.source_revision != reference.source_revision {
        return Err(err("Pinned source version changed"));
    }
    package.envelope.verify_payload(&package.payload, &ProjectLimits::default()).map_err(err)?;
    let mut doc = photocraft_format::load_from_bytes(&package.payload.native).map_err(err)?;
    crate::defold_project_dependencies::restore(&mut doc, &package, root).map_err(err)?;
    let project = doc.defold.as_ref().and_then(|d| d.project.as_ref()).ok_or_else(|| err("Shared source has no project"))?;
    if project.project_id != binding.context.project_id || project.package_id != reference.package_id {
        return Err(err("Shared source belongs to another project"));
    }
    let link = carrier(&doc, &reference.source_node)?;
    if project.layers.get(&link.layer).and_then(|b| b.asset_id.as_ref()) != Some(&reference.asset_id) {
        return Err(err("Shared source asset identity changed"));
    }
    Ok(doc)
}
fn record_reference(record: &crate::defold_publish_plan::AssetRecord) -> Result<SharedAppearanceRef> {
    use crate::defold_project_model::*;
    if record.ownership != AssetOwnership::Managed || record.state != AssetState::Active || record.recipe.as_ref().is_none_or(|r| r.cross_node_composition) {
        return Err(err("Publish owned editable artwork before sharing it"));
    }
    let source = record.source.as_ref().ok_or_else(|| err("The asset has no editable source"))?;
    Ok(SharedAppearanceRef {
        asset_id: record.asset_id.clone(),
        package_id: source.package_id.clone(),
        source_revision: source.source_revision.clone(),
        source_node: record.source_node.clone().ok_or_else(|| err("The asset has no canonical native carrier; republish its owner first"))?,
    })
}
/// Sharing identifies an already published owner. It never creates a second source or registry entry.
pub fn share(session: &mut crate::Session) -> Result<serde_json::Value> {
    let (opening, _revision, doc, binding, id) = active_capture(session)?;
    let role = doc.defold.as_ref().and_then(|d| d.project.as_ref()).and_then(|p| p.layers.get(&id.0)).ok_or_else(|| err("Node has no stable identity"))?;
    let asset = role.asset_id.clone().ok_or_else(|| err("Node has no asset identity"))?;
    require_editable_artwork(&doc, id)?;
    if session.active().is_none_or(|state| state.saved_revision != state.revision) {
        return Err(err("Save this creative source and publish its artwork before sharing"));
    }
    crate::jobs::run(
        session,
        "Inspect shared artwork source",
        false,
        move |job| {
            job.check()?;
            let registry = binding.store.read_asset_registry(&binding.context).map_err(err)?;
            let record = registry.assets.get(&asset).ok_or_else(|| err("Publish this node's artwork before sharing it"))?;
            let reference = record_reference(record)?;
            if Some(&reference.source_revision) != binding.source_revision.as_ref() || reference.package_id != binding.scene.package_id {
                return Err(err("Publish the saved owner version before sharing it"));
            }
            let selected = carrier(&doc, &reference.source_node)?.node.clone();
            extract_shared_artwork(&doc, &reference.source_node)?;
            // A pre-recipe registry may contain placeholder crop values. Verify the
            // actual frozen source instead of treating any six numbers as canonical.
            let mut verification = doc.clone();
            for node in &mut verification.defold.as_mut().ok_or_else(|| err("Missing GUI"))?.nodes {
                node.appearance.clear();
            }
            let planned = defold_design::plan_publication(
                verification,
                defold_design::PublicationInputs {
                    registry: Default::default(),
                    resources: Default::default(),
                    shared_atlas: false,
                    retained_textures: vec![],
                    managed_aliases: vec![],
                    shared_owners: std::collections::BTreeSet::from([asset.as_str().into()]),
                    force_bake: Default::default(),
                },
            )?;
            let baked =
                planned.registry.assets.iter().find(|asset| asset.node == selected).ok_or_else(|| err("This node has no publishable creative artwork"))?;
            let bytes = planned.files.iter().find(|(path, _)| path == &baked.image).ok_or_else(|| err("The canonical artwork output is missing"))?;
            if planned.recipes.get(&selected) != record.recipe.as_ref() || crate::defold_project_model::BlobDescriptor::of_bytes(&bytes.1) != record.output {
                return Err(err("Republish this saved owner to establish its exact artwork and crop before sharing"));
            }
            Ok(serde_json::json!({"openId":opening,"sharedAssetId":asset,"sharedSource":reference}))
        },
        |_, result| Ok(result),
    )
}
pub fn attach(session: &mut crate::Session, params: &serde_json::Value) -> Result<serde_json::Value> {
    let asset = AssetId::parse(params.get("assetId").and_then(serde_json::Value::as_str).ok_or_else(|| err("Choose a shared artwork asset"))?).map_err(err)?;
    let (opening, revision, mut doc, binding, id) = active_capture(session)?;
    require_editable_artwork(&doc, id)?;
    crate::jobs::run(
        session,
        "Attach shared artwork",
        true,
        move |job| {
            job.check()?;
            let registry = binding.store.read_asset_registry(&binding.context).map_err(err)?;
            let record = registry.assets.get(&asset).ok_or_else(|| err("Shared asset is missing"))?;
            let reference = record_reference(record)?;
            if reference.package_id == binding.scene.package_id {
                return Err(err("Choose an artwork owner in another GUI; local layers already share one source"));
            }
            let design = doc.defold.as_mut().ok_or_else(|| err("Missing GUI"))?;
            let role = design.project.as_mut().and_then(|p| p.layers.get_mut(&id.0)).ok_or_else(|| err("Node identity is missing"))?;
            role.appearance = Some(AppearanceSource::Shared { source: reference.clone() });
            role.asset_id = Some(asset.clone());
            role.artwork_fingerprint = None;
            design.nodes.iter_mut().find(|n| n.layer == id.0).ok_or_else(|| err("Node link is missing"))?.asset_id = asset.as_str().into();
            crate::defold_project_client::hydrate_shared_sources(&mut doc, &binding)?;
            Ok((doc, asset))
        },
        move |session, (doc, asset)| {
            let state = session.active().ok_or(crate::EngineError::NoDocument)?;
            if state.open_id != opening || state.revision != revision {
                return Err(err("The consumer changed while its source was loading"));
            }
            session.edit("Attach shared artwork", |current, active| {
                *current = doc;
                *active = Some(id);
                Ok(())
            })?;
            Ok(serde_json::json!({"openId":opening,"attachedAssetId":asset}))
        },
    )
}
pub fn independent(session: &mut crate::Session) -> Result<serde_json::Value> {
    let (opening, revision, mut doc, binding, id) = active_capture(session)?;
    let reference = shared_for_layer(&doc, id).cloned().ok_or_else(|| err("Select a shared artwork node"))?;
    crate::jobs::run(
        session,
        "Make shared artwork independent",
        true,
        move |job| {
            job.check()?;
            let root = doc.defold.as_ref().ok_or_else(|| err("Missing GUI"))?.root.clone();
            let owner = read_owner(&binding, &reference, &root)?;
            let canonical = extract_shared_artwork(&owner, &reference.source_node)?;
            let node = doc.defold.as_ref().and_then(|d| d.nodes.iter().find(|n| n.layer == id.0)).ok_or_else(|| err("Node disappeared"))?.node.clone();
            let mut artwork = instantiate_shared_artwork(&doc, &node, &canonical)?;
            fn ids(old: &Layer, new: &Layer, map: &mut BTreeMap<LayerId, LayerId>) {
                map.insert(old.id, new.id);
                if let (Some(a), Some(b)) = (old.children(), new.children()) {
                    for (a, b) in a.iter().zip(b) {
                        ids(a, b, map);
                    }
                }
            }
            let mut copied = BTreeMap::new();
            ids(&canonical, &artwork, &mut copied);
            let mut next = doc.ai_generations.iter().map(|g| g.id).max().unwrap_or(0);
            for group in &owner.ai_generations {
                let mut group = group.clone();
                group.versions.retain_mut(|v| {
                    v.layer = v.layer.and_then(|id| copied.get(&id).copied());
                    v.layer.is_some()
                });
                if group.versions.is_empty() {
                    continue;
                }
                if doc.ai_generations.len() >= photocraft_doc::ai::MAX_GROUPS {
                    return Err(err("The independent source exceeds the AI provenance limit"));
                }
                next = next.checked_add(1).ok_or_else(|| err("AI generation identity overflow"))?;
                group.id = next;
                doc.ai_generations.push(group);
            }
            let original_locks = artwork.locks;
            let root_id = artwork.id;
            artwork.name = "Artwork".into();
            replace_proxy(&mut doc, id, artwork, false)?;
            if let Some(layer) = doc.layer_mut(root_id) {
                layer.name = "Artwork".into();
                layer.locks = original_locks;
            }
            let design = doc.defold.as_mut().ok_or_else(|| err("Missing GUI"))?;
            let project = design.project.as_mut().ok_or_else(|| err("Missing project"))?;
            let root = project.layers.get(&root_id.0).ok_or_else(|| err("Missing artwork identity"))?.layer_id.clone();
            let asset: AssetId = new_id().map_err(err)?;
            let role = project.layers.get_mut(&id.0).ok_or_else(|| err("Missing node identity"))?;
            role.appearance = Some(AppearanceSource::Subtree { root_layer_id: root });
            role.asset_id = Some(asset.clone());
            role.artwork_fingerprint = None;
            let link = design.nodes.iter_mut().find(|n| n.layer == id.0).ok_or_else(|| err("Missing node link"))?;
            link.asset_id = asset.as_str().into();
            link.appearance.clear();
            crate::defold_design_reconcile::structure_changed(&mut doc)?;
            Ok((doc, root_id, asset))
        },
        move |session, (doc, root_id, asset)| {
            let state = session.active().ok_or(crate::EngineError::NoDocument)?;
            if state.open_id != opening || state.revision != revision {
                return Err(err("The consumer changed while its source was loading"));
            }
            session.edit("Make artwork independent", |current, active| {
                *current = doc;
                *active = Some(root_id);
                Ok(())
            })?;
            Ok(serde_json::json!({"openId":opening,"independentAssetId":asset}))
        },
    )
}
#[cfg(not(target_arch = "wasm32"))]
pub fn open_source(session: &mut crate::Session) -> Result<serde_json::Value> {
    use crate::defold_project_model::*;
    let (_, _, doc, binding, id) = active_capture(session)?;
    let reference = shared_for_layer(&doc, id).cloned().ok_or_else(|| err("Select a shared artwork node"))?;
    let root = doc.defold.as_ref().ok_or_else(|| err("Missing GUI"))?.root.clone();
    crate::jobs::run(
        session,
        "Open shared artwork source",
        false,
        move |job| {
            job.check()?;
            let identity = binding
                .store
                .scene_identity(&SceneIdentityRequest { context: binding.context.clone(), scene_id: reference.source_node.scene_id.clone() })
                .map_err(err)?;
            if identity.package_id != reference.package_id {
                return Err(err("Shared owner identity changed"));
            }
            let gui = std::path::Path::new(&root).join(identity.path.as_str().trim_start_matches('/')).to_string_lossy().into_owned();
            let (owner, owner_binding) =
                crate::defold_project_client::load_saved_document_with_binding(&root, &gui)?.ok_or_else(|| err("The owner has no saved creative source"))?;
            Ok((owner, owner_binding, gui, reference.source_node))
        },
        |session, (doc, binding, path, node)| {
            let index = if let Some(index) = session.documents().iter().position(|state| {
                state
                    .doc
                    .defold
                    .as_ref()
                    .and_then(|d| d.project.as_ref())
                    .is_some_and(|p| p.project_id == binding.context.project_id && p.package_id == binding.scene.package_id)
            }) {
                session.set_active(index);
                index
            } else {
                let index = session.add_document(doc, Some(path));
                let opening = session.active().ok_or(crate::EngineError::NoDocument)?.open_id;
                session.project_client.acknowledged_openings.insert(opening);
                session.project_client.bindings.insert(opening, binding);
                index
            };
            let id = {
                let state = session.active().ok_or(crate::EngineError::NoDocument)?;
                carrier(&state.doc, &node)?.layer
            };
            session.select_layer(LayerId(id))?;
            Ok(serde_json::json!({"sourceOpened":true,"document":index}))
        },
    )
}
#[cfg(target_arch = "wasm32")]
pub fn open_source(_: &mut crate::Session) -> Result<serde_json::Value> {
    Err(err("Shared artwork source editing requires the desktop application"))
}
fn enabled(session: &crate::Session) -> std::result::Result<(), String> {
    crate::defold_project_cmds::linked_idle(session)?;
    let state = session.active().ok_or_else(|| "Open a linked GUI".to_string())?;
    if session.project_client.bindings.contains_key(&state.open_id) && defold_design::selected(session).is_some() {
        Ok(())
    } else {
        Err("Select a connected GUI node".into())
    }
}
pub fn specs() -> Vec<crate::commands::CommandSpec> {
    vec![
        crate::commands::CommandSpec {
            id: "defold.shareNodeArtwork",
            label: "Share node artwork",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled,
            run: |s, _| share(s),
            journal: false,
        },
        crate::commands::CommandSpec {
            id: "defold.attachSharedArtwork",
            label: "Attach shared artwork",
            menu: &[],
            shortcut: None,
            params: "{assetId:string}",
            enabled,
            run: attach,
            journal: true,
        },
        crate::commands::CommandSpec {
            id: "defold.openSharedArtworkSource",
            label: "Open shared artwork source",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled,
            run: |s, _| open_source(s),
            journal: false,
        },
        crate::commands::CommandSpec {
            id: "defold.makeArtworkIndependent",
            label: "Make artwork independent",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled,
            run: |s, _| independent(s),
            journal: true,
        },
    ]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;
    fn document(scene_byte: u8) -> Document {
        let mut doc = Document::new("Native shared fixture", Size::new(64, 64), ColorMode::Rgb, photocraft_color::SampleType::U8);
        let mut art = Layer::raster("Paint and masks", doc.pixel_format());
        art.surface_mut().unwrap().fill_rect(Rect::new(25, 20, 40, 35), &[1.0, 0.4, 0.0, 1.0]);
        let art_id = art.id;
        let carrier = Layer::group("Button", vec![art]);
        let id = carrier.id;
        doc.layers.push(carrier);
        doc.defold = Some(defold::Design {
            version: 3,
            root: "X:/fixture".into(),
            gui: "X:/fixture/main.gui".into(),
            source: "nodes{id:\"button\" type:TYPE_BOX position{x:32 y:32} size{x:15 y:15}}".into(),
            disk_hash: String::new(),
            nodes: vec![defold::NodeLink {
                asset_id: String::new(),
                layer: id.0,
                artwork: art_id.0,
                node: "button".into(),
                appearance: String::new(),
                text: None,
            }],
            remote: None,
            canvas_origin: [0, 0],
            project: Some(defold::ProjectLink {
                schema_version: 1,
                project_id: defold::ProjectId::from_bytes([1; 16]),
                scene_id: defold::SceneId::from_bytes([scene_byte; 16]),
                package_id: defold::PackageId::from_bytes([scene_byte; 16]),
                gui_path: "/main.gui".into(),
                imported_gui_source: None,
                imported_disk_hash: None,
                layers: BTreeMap::new(),
                legacy_aliases: BTreeMap::new(),
            }),
        });
        crate::defold_design_reconcile::initialize(&mut doc).unwrap();
        doc
    }
    fn source_reference(owner: &Document) -> SharedAppearanceRef {
        let design = owner.defold.as_ref().unwrap();
        let project = design.project.as_ref().unwrap();
        let role = &project.layers[&design.nodes[0].layer];
        SharedAppearanceRef {
            asset_id: role.asset_id.clone().unwrap(),
            package_id: project.package_id.clone(),
            source_revision: "pinned-source".into(),
            source_node: ArtworkSourceNode { scene_id: project.scene_id.clone(), node_id: role.node_id.clone().unwrap(), root_layer_id: role.layer_id.clone() },
        }
    }
    fn attach_ref(consumer: &mut Document, reference: &SharedAppearanceRef) {
        let design = consumer.defold.as_mut().unwrap();
        let node = &mut design.nodes[0];
        node.asset_id = reference.asset_id.as_str().into();
        let role = design.project.as_mut().unwrap().layers.get_mut(&node.layer).unwrap();
        role.appearance = Some(AppearanceSource::Shared { source: reference.clone() });
        role.asset_id = Some(reference.asset_id.clone());
    }
    #[test]
    fn shared_artwork_cache_stripping_and_rehydration_preserve_native_source_and_pixels() {
        let owner = document(2);
        let reference = source_reference(&owner);
        let original = owner.clone();
        let mut consumer = document(3);
        attach_ref(&mut consumer, &reference);
        let resolved = BTreeMap::from([(reference.asset_id.clone(), (owner.clone(), reference.clone()))]);
        hydrate_shared_document(&mut consumer, &resolved).unwrap();
        assert_eq!(owner, original);
        let before = photocraft_compose::render(&consumer, consumer.bounds());
        assert!(before.px.iter().any(|px| px[3] > 0.9));
        let design = consumer.defold.as_ref().unwrap();
        let proxy = design.nodes[0].artwork;
        let stable = design.project.as_ref().unwrap().layers[&proxy].layer_id.clone();
        strip_shared_caches(&mut consumer).unwrap();
        let design = consumer.defold.as_ref().unwrap();
        let proxy = design.nodes[0].artwork;
        assert_eq!(design.project.as_ref().unwrap().layers[&proxy].layer_id, stable);
        assert!(consumer.layer(LayerId(proxy)).unwrap().children().unwrap().is_empty());
        assert!(photocraft_compose::render(&consumer, consumer.bounds()).px.iter().all(|px| px[3] == 0.0));
        let bytes = photocraft_format::save_to_bytes(&consumer, &Default::default()).unwrap();
        let mut restored = photocraft_format::load_from_bytes(&bytes).unwrap();
        hydrate_shared_document(&mut restored, &resolved).unwrap();
        assert_eq!(photocraft_compose::render(&restored, restored.bounds()).px, before.px);
    }
    #[test]
    fn shared_artwork_command_guard_protects_explicit_proxy_targets_but_allows_carrier_move() {
        let owner = document(2);
        let reference = source_reference(&owner);
        let mut consumer = document(3);
        attach_ref(&mut consumer, &reference);
        hydrate_shared_document(&mut consumer, &BTreeMap::from([(reference.asset_id.clone(), (owner, reference))])).unwrap();
        let link = consumer.defold.as_ref().unwrap().nodes[0].clone();
        let mut session = crate::Session::new();
        session.add_document(consumer, None);
        session.select_layer(LayerId(link.layer)).unwrap();
        assert!(command_precondition(&session, "layer.translate", &json!({"dx":2,"dy":3})).is_ok());
        assert!(command_precondition(&session, "paint.stroke", &json!({"layer":link.artwork})).is_err());
        assert!(command_precondition(&session, "layer.setProps", &json!({"layer":link.artwork,"locks":{"all":false}})).is_err());
        session.select_layer(LayerId(link.artwork)).unwrap();
        assert!(command_precondition(&session, "layer.delete", &json!({})).is_err());
        assert!(command_precondition(&session, "defold.setDesignProperties", &json!({"values":{"position.x":20}})).is_ok());
        assert!(command_precondition(&session, "defold.setDesignProperties", &json!({"values":{"alpha":0.5}})).is_err());
        assert!(require_editable_artwork(&session.active().unwrap().doc, LayerId(link.artwork)).is_err());
    }
    #[test]
    fn shared_artwork_owner_instance_style_is_not_baked_into_other_consumers() {
        let mut owner = document(2);
        let reference = source_reference(&owner);
        let layer = owner.defold.as_ref().unwrap().nodes[0].layer;
        owner.layer_mut(LayerId(layer)).unwrap().visible = false;
        assert!(extract_shared_artwork(&owner, &reference.source_node).unwrap_err().to_string().contains("instance visibility"));
        owner.layer_mut(LayerId(layer)).unwrap().visible = true;
        let scene = defold_design::document_scene(&owner).unwrap().edit("button", "alpha", &json!(0.5)).unwrap();
        owner.defold.as_mut().unwrap().source = scene.proto.source;
        assert!(extract_shared_artwork(&owner, &reference.source_node).unwrap_err().to_string().contains("instance visibility"));
    }
    #[test]
    fn shared_artwork_rejects_document_pattern_dependencies_before_copying() {
        let mut owner = document(2);
        let reference = source_reference(&owner);
        let artwork = owner.defold.as_ref().unwrap().nodes[0].artwork;
        owner.layer_mut(LayerId(artwork)).unwrap().content =
            LayerContent::Fill(Fill::Pattern { name: "source-library".into(), id: "pattern-id".into(), scale: 1.0, angle: 0.0, link: true, phase: (0.0, 0.0) });
        assert!(extract_shared_artwork(&owner, &reference.source_node).unwrap_err().to_string().contains("pattern libraries"));
    }
    #[test]
    fn shared_artwork_rejects_ancestor_backdrop_and_mismatched_pins_without_mutating_owner() {
        let mut owner = document(2);
        let reference = source_reference(&owner);
        let mut parent = Layer::group("Backdrop", std::mem::take(&mut owner.layers));
        parent.opacity = 0.5;
        owner.layers.push(parent);
        assert!(extract_shared_artwork(&owner, &reference.source_node).unwrap_err().to_string().contains("ancestors"));
        let owner = document(2);
        let reference = source_reference(&owner);
        let mut consumer = document(3);
        attach_ref(&mut consumer, &reference);
        let before = consumer.clone();
        let mut wrong = reference.clone();
        wrong.source_revision = "newer-source".into();
        assert!(hydrate_shared_document(&mut consumer, &BTreeMap::from([(reference.asset_id, (owner, wrong))])).is_err());
        assert_eq!(consumer, before);
    }
}
