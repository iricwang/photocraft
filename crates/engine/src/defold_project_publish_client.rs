//! Manual publication against an authority store. No editor or game-path I/O lives in this client.
use crate::{
    EngineError, Result, Session,
    defold_project_client::{self as client, ClientBinding},
    defold_project_model::*,
    defold_publish_plan,
};
use photocraft_doc::Document;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[path = "defold_project_publish_batch.rs"]
mod batch;
pub(crate) use batch::SharedPublicationReview;

fn engine(error: impl std::fmt::Display) -> EngineError {
    EngineError::Other(error.to_string())
}
fn optional(binding: &ClientBinding, path: &ResourcePath) -> Result<Option<ResourceContent>> {
    match binding.store.read_resource(&ResourceReadRequest { context: binding.context.clone(), path: path.clone(), expected_hash: None }) {
        Ok(content) => {
            content.version.content.verify(&content.bytes).map_err(engine)?;
            Ok(Some(content))
        }
        Err(error) if error.code == ProjectErrorCode::ResourceMissing => Ok(None),
        Err(error) => Err(engine(error)),
    }
}
fn read(binding: &ClientBinding, path: &ResourcePath, hash: Option<ContentHash>) -> Result<ResourceContent> {
    let content =
        binding.store.read_resource(&ResourceReadRequest { context: binding.context.clone(), path: path.clone(), expected_hash: hash }).map_err(engine)?;
    content.version.content.verify(&content.bytes).map_err(engine)?;
    Ok(content)
}

struct CapturedAssets {
    registry: defold_publish_plan::AssetRegistry,
    resources: BTreeMap<ResourcePath, ResourceContent>,
    native: crate::defold_design::PublicationInputs,
    folder: ResourcePath,
    atlas: ResourcePath,
    protected: Vec<crate::defold_atlas::AtlasFrame>,
}

fn capture_assets(binding: &ClientBinding, doc: &Document) -> Result<CapturedAssets> {
    let design = doc.defold.as_ref().ok_or_else(|| engine("GUI attachment is missing"))?;
    let project = design.project.as_ref().ok_or_else(|| engine("Stable GUI identities are missing"))?;
    let folder = ResourcePath::parse(&format!("/photocraft_generated/{}", project.scene_id)).map_err(engine)?;
    let atlas = ResourcePath::parse(&format!("{folder}/scene.atlas")).map_err(engine)?;
    let registry = binding.store.read_asset_registry(&binding.context).map_err(engine)?;
    let mut resources = BTreeMap::new();
    let mut protected = Vec::new();
    if let Some(content) = optional(binding, &atlas)? {
        protected = crate::defold_atlas::decode(std::str::from_utf8(&content.bytes).map_err(engine)?).map_err(engine)?;
        resources.insert(atlas.clone(), content);
    }
    let active_assets: BTreeSet<_> = project.layers.values().filter_map(|layer| layer.asset_id.clone()).collect();
    for asset in registry.assets.values().filter(|asset| asset.referencing_scenes.contains(&project.scene_id) || active_assets.contains(&asset.asset_id)) {
        resources.insert(asset.image.clone(), read(binding, &asset.image, Some(asset.output.hash.clone()))?);
    }
    for frame in &protected {
        if !resources.contains_key(&frame.image) {
            resources.insert(frame.image.clone(), read(binding, &frame.image, None)?);
        }
    }
    // Probe both readable and stable-suffix candidates so foreign files are never claimed.
    for node in &design.nodes {
        let Some(id) = project.layers.get(&node.layer).and_then(|binding| binding.asset_id.as_ref()) else { continue };
        let Some(layer) = doc.layer(photocraft_doc::LayerId(node.layer)) else { continue };
        let label = defold_publish_plan::image_label(&layer.name);
        let short: String = id.as_str().chars().filter(|c| *c != '-').take(12).collect();
        for name in [label.clone(), format!("{label}__{short}")] {
            let path = ResourcePath::parse(&format!("{folder}/images/{name}.png")).map_err(engine)?;
            if !resources.contains_key(&path)
                && let Some(content) = optional(binding, &path)?
            {
                resources.insert(path, content);
            }
        }
    }

    // Old named resources are read as migration inputs, never written or retired here.
    let gui = &project.gui_path;
    let key: String = blake3::hash(gui.as_bytes()).to_hex().chars().take(16).collect();
    let gui_name: String = std::path::Path::new(gui)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("gui")
        .chars()
        .take(48)
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect();
    let legacy_folder = format!("/photocraft_generated/{gui_name}_{key}");
    let legacy_path = ResourcePath::parse(&format!("{legacy_folder}/assets.json")).map_err(engine)?;
    let mut legacy: crate::defold_assets::Registry = match optional(binding, &legacy_path)? {
        Some(content) => serde_json::from_slice(&content.bytes).map_err(engine)?,
        None => Default::default(),
    };
    let mut native_resources = BTreeMap::new();
    for asset in &legacy.assets {
        let path = ResourcePath::parse(&asset.image).map_err(engine)?;
        let content = read(binding, &path, None)?;
        native_resources.insert(asset.image.clone(), content.bytes);
    }
    let scene = photocraft_defold_format::Scene::parse(design.source.clone()).map_err(engine)?;
    for node in &scene.nodes {
        if let Some(stem) = node.texture.strip_prefix(&format!("pc_{key}/")) {
            let path = ResourcePath::parse(&format!("{legacy_folder}/images/{stem}.png")).map_err(engine)?;
            if !native_resources.contains_key(path.as_str()) {
                native_resources.insert(path.to_string(), read(binding, &path, None)?.bytes);
            }
        }
    }
    // Adapt formal records to the native renderer's captured-byte reuse seam.
    for node in &design.nodes {
        let Some(id) = project.layers.get(&node.layer).and_then(|binding| binding.asset_id.as_ref()) else { continue };
        let Some(asset) = registry.assets.get(id) else { continue };
        let content = resources.get(&asset.image).ok_or_else(|| engine("Published image was not captured"))?;
        legacy.assets.retain(|asset| asset.key != node.asset_id && asset.node != node.node);
        legacy.assets.push(crate::defold_assets::Asset {
            key: node.asset_id.clone(),
            node: node.node.clone(),
            layer: node.layer,
            image: asset.image.to_string(),
            hash: blake3::hash(&content.bytes).to_hex().to_string(),
            name: asset.display_name.clone(),
        });
        native_resources.insert(asset.image.to_string(), content.bytes.clone());
    }
    let force_bake = design
        .nodes
        .iter()
        .filter(|node| {
            let current = project.layers.get(&node.layer).and_then(|layer| layer.asset_id.as_ref()).and_then(|id| registry.assets.get(id));
            match current {
                Some(asset) => asset.source_node.is_none() || asset.recipe.as_ref().is_none_or(|recipe| recipe.validate().is_err()),
                None => {
                    legacy.assets.iter().any(|asset| asset.key == node.asset_id || asset.node == node.node)
                        || scene.nodes.iter().any(|saved| {
                            saved.id == node.node
                                && (saved.texture.starts_with(&format!("pc_{key}/"))
                                    || saved.texture.starts_with(&format!("pc_{}/", project.scene_id.as_str().replace('-', ""))))
                        })
                }
            }
        })
        // Imported legacy links can have an empty asset_id until reconciliation.
        // Keep the node key too, so its first export cannot bypass the upgrade.
        .flat_map(|node| [node.asset_id.clone(), format!("node:{}", node.node)])
        .collect();
    let shared_owners = registry
        .assets
        .values()
        .filter(|asset| asset.referencing_scenes.len() > 1)
        .filter(|asset| asset.source_node.as_ref().is_some_and(|node| node.scene_id == project.scene_id))
        .map(|asset| asset.asset_id.as_str().to_owned())
        .collect();
    Ok(CapturedAssets {
        registry,
        resources,
        native: crate::defold_design::PublicationInputs {
            registry: legacy,
            resources: native_resources,
            shared_atlas: false,
            retained_textures: vec![],
            managed_aliases: vec![format!("pc_{}", project.scene_id.as_str().replace('-', ""))],
            shared_owners,
            force_bake,
        },
        folder,
        atlas,
        protected,
    })
}

pub(crate) struct ReviewProjection {
    pub scene: photocraft_defold_format::Scene,
    pub authored: photocraft_defold_format::Scene,
    pub painted_nodes: BTreeSet<String>,
}

/// Read-only capture followed by the same native raster planner used by publish.
/// Formal atlas/frame names remove temporary adapter names from the comparison.
pub(crate) fn review_projection(doc: &Document, binding: &ClientBinding) -> Result<ReviewProjection> {
    let captured = capture_assets(binding, doc)?;
    let mut native = crate::defold_design::plan_publication(doc.clone(), captured.native)?;
    let design = native.document.defold.as_ref().ok_or_else(|| engine("GUI attachment is missing"))?;
    let project = design.project.as_ref().ok_or_else(|| engine("Stable GUI identities are missing"))?.clone();
    for (layer, binding) in &project.layers {
        let Some(AppearanceSource::Shared { source }) = &binding.appearance else { continue };
        let record = captured.registry.assets.get(&source.asset_id).ok_or_else(|| engine("Shared artwork is not registered"))?;
        if record.source.as_ref().is_none_or(|saved| saved.package_id != source.package_id || saved.source_revision != source.source_revision)
            || record.source_node.as_ref() != Some(&source.source_node)
        {
            return Err(engine("Shared artwork has a newer owner version; publish the shared project together before reviewing this stale reference"));
        }
        let node = native.links.iter().find(|link| link.layer == *layer).ok_or_else(|| engine("Shared GUI node is missing"))?.node.clone();
        let recipe = record.recipe.as_ref().ok_or_else(|| engine("Shared artwork has no canonical geometry"))?;
        crate::defold_design::apply_shared_recipe(&mut native, &node, recipe)?;
    }
    let mut scene = native.output;
    let mut painted_nodes = crate::defold_design::changed_artwork_nodes(&native.document)?;
    let alias = format!("pc_{}", project.scene_id.as_str().replace('-', ""));
    for asset in &native.registry.assets {
        let link = native.links.iter().find(|link| link.node == asset.node).ok_or_else(|| engine("Rendered GUI node is missing"))?;
        let id = project.layers.get(&link.layer).and_then(|layer| layer.asset_id.as_ref()).ok_or_else(|| engine("Stable asset identity is missing"))?;
        let previous = captured.registry.assets.get(id);
        let frame = previous.map(|record| record.runtime_frame_id.clone()).unwrap_or_else(|| defold_publish_plan::runtime_frame_id(id));
        scene = scene.edit(&asset.node, "texture", &json!(format!("{alias}/{frame}"))).map_err(engine)?;
        // A published native asset remains editable user artwork even when its
        // bytes equal the previous output. Publication is not permission to redraw it.
        painted_nodes.insert(asset.node.clone());
    }
    for (layer, binding) in &project.layers {
        let Some(AppearanceSource::Shared { source }) = &binding.appearance else { continue };
        let record = captured.registry.assets.get(&source.asset_id).ok_or_else(|| engine("Shared artwork is missing"))?;
        let node = native.links.iter().find(|link| link.layer == *layer).ok_or_else(|| engine("Shared GUI node is missing"))?;
        scene = scene.edit(&node.node, "texture", &json!(format!("{alias}/{}", record.runtime_frame_id))).map_err(engine)?;
        painted_nodes.insert(node.node.clone());
    }
    let old_aliases: BTreeSet<_> = native.textures.iter().filter_map(|texture| texture["name"].as_str()).chain(std::iter::once(alias.as_str())).collect();
    let mut raw = scene.proto.source.clone();
    for field in
        scene.proto.fields.iter().rev().filter(|field| field.name == "textures" && old_aliases.contains(scene.proto.text(&field.children, "name").as_str()))
    {
        raw.replace_range(field.span.clone(), "");
    }
    raw.push_str(&format!(
        "\ntextures {{ name: {} texture: {} }}\n",
        serde_json::to_string(&alias).map_err(engine)?,
        serde_json::to_string(captured.atlas.as_str()).map_err(engine)?
    ));
    Ok(ReviewProjection {
        scene: photocraft_defold_format::Scene::parse(raw).map_err(engine)?,
        authored: crate::defold_design::document_scene(&native.document)?,
        painted_nodes,
    })
}

pub(crate) fn source_descends_from(binding: &ClientBinding, ancestor: &str) -> Result<bool> {
    let mut current = binding.source_revision.clone();
    let mut seen = BTreeSet::new();
    for _ in 0..128 {
        let Some(revision) = current else { return Ok(false) };
        if revision == ancestor {
            return Ok(true);
        }
        if !seen.insert(revision.clone()) {
            return Err(engine("Invalid creative source ancestry"));
        }
        let saved = binding
            .store
            .read_source(&ReadSourceRequest {
                context: binding.context.clone(),
                package_id: binding.scene.package_id.clone(),
                source_revision: Some(revision.clone()),
            })
            .map_err(engine)?;
        if saved.envelope.package_id != binding.scene.package_id || saved.envelope.source_revision != revision {
            return Err(engine("Creative source ancestry identity mismatch"));
        }
        current = saved.envelope.parent_source_revision;
    }
    Err(engine("Creative source ancestry exceeds the reconciliation limit"))
}
fn accept_baseline(binding: &mut ClientBinding, current: &SceneSnapshot, latest: Option<&PublishReceipt>) -> Result<Option<String>> {
    if current.dirty {
        return Err(engine("TARGET_DIRTY: Save or discard Defold's pending GUI edits before publication"));
    }
    let last = latest.and_then(|receipt| receipt.targets.iter().find(|target| target.scene_id == current.scene_id));
    if current.disk_hash != binding.baseline.disk_hash {
        let published_source = latest
            .filter(|receipt| receipt.phase == PublishPhase::Committed)
            .and_then(|receipt| receipt.source_revisions.iter().find(|source| source.package_id == binding.scene.package_id));
        let own_saved_source = match published_source {
            Some(source) => source_descends_from(binding, &source.source_revision)?,
            None => false,
        };
        if !own_saved_source || last.is_none_or(|target| target.disk_hash != current.disk_hash) {
            return Err(engine("TARGET_CHANGED: GUI changed since the creative-source baseline; keep the draft and reconcile before publishing"));
        }
    }
    let published = last.map(|target| target.published_revision.clone()).or_else(|| binding.baseline.published_revision.clone());
    binding.baseline = SceneBaseline {
        scene_id: current.scene_id.clone(),
        path: current.path.clone(),
        gui_source: current.source.clone(),
        disk_hash: current.disk_hash.clone(),
        scene_hash: current.scene_hash.clone(),
        published_revision: published.clone(),
    };
    Ok(published)
}

fn validate_receipt(receipt: &PublishReceipt, plan: &PublishPlan) -> Result<()> {
    check_schema(receipt.schema_version).map_err(engine)?;
    if receipt.context != plan.context
        || receipt.transaction_id != plan.transaction_id
        || receipt.plan_hash != plan.plan_hash
        || receipt.source_revisions != plan.source_revisions
    {
        return Err(engine("IDEMPOTENCY_MISMATCH: Host returned a receipt for another publication plan"));
    }
    if receipt.phase == PublishPhase::Committed {
        if receipt.targets.len() != plan.targets.len() {
            return Err(engine("SAVE_FAILED: Host did not confirm every GUI target"));
        }
        for target in &plan.targets {
            let saved = receipt
                .targets
                .iter()
                .find(|saved| saved.scene_id == target.scene_id && saved.path == target.path)
                .ok_or_else(|| engine("SAVE_FAILED: GUI target is absent from the publication receipt"))?;
            if saved.published_revision.is_empty() {
                return Err(engine("SAVE_FAILED: Saved GUI has no publication revision"));
            }
        }
    }
    Ok(())
}
fn recover_commit_error(binding: &ClientBinding, request: &PublishRequest, failure: ProjectError) -> Result<PublishReceipt> {
    let mut receipt = binding.store.publish_status(request).map_err(engine)?.ok_or_else(|| engine(&failure))?;
    if !receipt.phase.is_terminal() {
        receipt.error = Some(failure);
    }
    Ok(receipt)
}
fn transact(binding: &ClientBinding, plan: &PublishPlan, payload: &PublishPayload) -> Result<PublishReceipt> {
    let request = PublishRequest { context: binding.context.clone(), transaction_id: plan.transaction_id.clone() };
    let prepared = match binding.store.prepare_publish(plan, payload) {
        Ok(receipt) => receipt,
        Err(error) => match binding.store.publish_status(&request) {
            Ok(Some(receipt)) => receipt,
            _ => return Err(engine(error)),
        },
    };
    validate_receipt(&prepared, plan)?;
    if prepared.phase == PublishPhase::Committed {
        return Ok(prepared);
    }
    if prepared.phase != PublishPhase::Prepared {
        return Ok(prepared);
    }
    let receipt = match binding.store.commit_publish(&request) {
        Ok(receipt) => receipt,
        Err(error) => recover_commit_error(binding, &request, error)?,
    };
    validate_receipt(&receipt, plan)?;
    #[cfg(not(target_arch = "wasm32"))]
    let receipt = {
        let mut receipt = receipt;
        for _ in 0..20 {
            if !matches!(receipt.phase, PublishPhase::Applying | PublishPhase::AppliedUnsaved | PublishPhase::Saving) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
            let Some(next) = binding.store.publish_status(&request).map_err(engine)? else {
                break;
            };
            validate_receipt(&next, plan)?;
            receipt = next;
        }
        receipt
    };
    Ok(receipt)
}

struct Published {
    document: Document,
    receipt: PublishReceipt,
    assets: usize,
    nodes: usize,
    retained: Vec<RetainedResource>,
}
fn publish_captured(
    doc: &mut Document,
    binding: &mut ClientBinding,
    operation: batch::BatchRequest<'_>,
    job: &crate::jobs::JobCtx,
    openings: &[batch::OpenScene],
    related: &mut Vec<batch::RelatedAcknowledgement>,
) -> Result<Published> {
    let batch::BatchRequest { revision, transaction_id, approval, .. } = operation;
    job.check()?;
    if binding.pending_source_is_some() {
        return Err(engine("RECOVERY_REQUIRED: Confirm the pending creative-source save before reviewing or publishing related GUIs"));
    }
    let request = SceneRequest { context: binding.context.clone(), scene_id: binding.scene.scene_id.clone(), path: binding.scene.path.clone() };
    let mut latest = binding.store.latest_publication(&request).map_err(engine)?;
    if let Some(pending) = latest.as_ref().filter(|receipt| !receipt.phase.is_terminal()) {
        // A short polling window or lost reply must not permanently block the
        // next manual sync. Reconcile the original transaction before planning.
        let transaction = PublishRequest { context: binding.context.clone(), transaction_id: pending.transaction_id.clone() };
        let recovered = binding.store.publish_status(&transaction).map_err(engine)?.ok_or_else(|| engine("Publication receipt is missing"))?;
        if recovered.context != binding.context || recovered.transaction_id != pending.transaction_id || recovered.plan_hash != pending.plan_hash {
            return Err(engine("IDEMPOTENCY_MISMATCH: Publication status changed transaction identity"));
        }
        if !recovered.phase.is_terminal() {
            return Err(engine(format!(
                "RECOVERY_REQUIRED: Transaction {} is still {:?}; check publication status before retrying",
                recovered.transaction_id, recovered.phase
            )));
        }
        if binding.pending_publication.as_ref().is_some_and(|pending| pending.0 == recovered.transaction_id) {
            binding.pending_publication = None;
        }
        latest = Some(recovered);
    }
    client::acquire_or_renew(binding)?;
    if let Some(published) = batch::publish_if_shared(
        doc,
        binding,
        batch::BatchRequest { revision, transaction_id: transaction_id.clone(), approval, inspect: None },
        job,
        openings,
        related,
    )? {
        return Ok(published);
    }
    job.progress(0.15, "Save creative source");
    // The creative source is acknowledged first. A later publication failure keeps this receipt.
    let saved = client::save_captured_document(doc, binding, revision)?;
    let current = binding.store.snapshot(&request).map_err(engine)?;
    let published_revision = accept_baseline(binding, &current, latest.as_ref())?;
    job.check()?;
    job.progress(0.35, "Prepare GUI artwork");
    let captured = capture_assets(binding, doc)?;
    let native = crate::defold_design::plan_publication(doc.clone(), captured.native)?;
    let nodes = native.output.nodes.len();

    let desired_frames: BTreeSet<_> = native
        .registry
        .assets
        .iter()
        .filter_map(|asset| {
            let design = native.document.defold.as_ref()?;
            let node = design.nodes.iter().find(|node| node.node == asset.node)?;
            let id = design.project.as_ref()?.layers.get(&node.layer)?.asset_id.as_ref()?;
            Some(defold_publish_plan::runtime_frame_id(id))
        })
        .collect();
    let bridge = defold_publish_plan::bridge_publication(
        native,
        defold_publish_plan::PublicationBridgeInput {
            context: binding.context.clone(),
            client_id: binding.client_id.clone(),
            transaction_id,
            source_revisions: vec![SourceRevisionRef { package_id: binding.scene.package_id.clone(), source_revision: saved.source_revision }],
            targets: vec![PublishTarget {
                scene_id: current.scene_id.clone(),
                path: current.path.clone(),
                expected_published_revision: published_revision,
                expected_gui_disk_hash: current.disk_hash,
                expected_editor_snapshot: current.editor_graph_revision,
            }],
            leases: binding.leases.clone(),
            folder: captured.folder,
            atlas: captured.atlas,
            registry: captured.registry,
            resources: captured.resources,
            protected_frames: captured.protected.into_iter().filter(|frame| !desired_frames.contains(&frame.id)).collect(),
        },
    )
    .map_err(engine)?;
    let mut plan = bridge.plan;
    for resource in bridge.required_resources {
        let existing = binding.leases.iter().find(|proof| proof.resource == resource);
        let renewed = existing.map(|proof| {
            binding.store.renew_lease(&LeaseRenewRequest {
                context: binding.context.clone(),
                client_id: binding.client_id.clone(),
                proof: proof.clone(),
                duration_ms: 300_000,
            })
        });
        let grant = match renewed {
            Some(Ok(grant)) => grant,
            Some(Err(error)) if error.code != ProjectErrorCode::LeaseLost => return Err(engine(error)),
            _ => binding
                .store
                .acquire_lease(&LeaseRequest {
                    context: binding.context.clone(),
                    client_id: binding.client_id.clone(),
                    resource: resource.clone(),
                    duration_ms: 300_000,
                })
                .map_err(engine)?,
        };
        let proof = grant.proof;
        binding.leases.retain(|old| old.resource != resource);
        binding.leases.push(proof);
    }
    let required: BTreeSet<_> = plan
        .source_revisions
        .iter()
        .map(|source| LeaseResource::Package(source.package_id.clone()))
        .chain(plan.targets.iter().map(|target| LeaseResource::Scene(target.scene_id.clone())))
        .chain(plan.asset_changes.iter().map(|asset| LeaseResource::Asset(asset.asset_id.clone())))
        .collect();
    plan.leases = binding.leases.iter().filter(|proof| required.contains(&proof.resource)).cloned().collect();
    plan.plan_hash = plan.computed_hash().map_err(engine)?;
    job.check()?;
    job.progress(0.75, "Publish and save in Defold");
    binding.pending_publication = Some(Arc::new((plan.transaction_id.clone(), revision, bridge.document.clone())));
    let receipt = transact(binding, &plan, &bridge.payload)?;
    if receipt.phase == PublishPhase::Aborted {
        binding.pending_publication = None;
    }
    if receipt.phase == PublishPhase::Committed {
        let target = receipt
            .targets
            .iter()
            .find(|target| target.scene_id == binding.scene.scene_id)
            .ok_or_else(|| engine("Committed publication has no target receipt"))?;
        // Defold may reserialize the GUI. The saved bytes, not the export draft,
        // are the next conflict baseline. A read failure keeps the durable receipt
        // recoverable via Check publication status without inventing a baseline.
        let saved = read(binding, &target.path, Some(target.disk_hash.clone()))?;
        let source = String::from_utf8(saved.bytes).map_err(engine)?;
        binding.baseline = SceneBaseline {
            scene_id: target.scene_id.clone(),
            path: target.path.clone(),
            gui_source: source,
            disk_hash: target.disk_hash.clone(),
            scene_hash: target.scene_hash.clone(),
            published_revision: Some(target.published_revision.clone()),
        };
        binding.pending_publication = None;
    }
    job.progress(1.0, "Publication confirmed");
    let assets = bridge.registry.assets.values().filter(|asset| asset.state == AssetState::Active).count();
    Ok(Published { document: bridge.document, receipt, assets, nodes, retained: bridge.retained })
}

pub fn publish(session: &mut Session) -> Result<Value> {
    publish_with_params(session, &Value::Null)
}

fn capture_openings(session: &Session) -> Vec<batch::OpenScene> {
    let project_id = session.active().and_then(|state| state.doc.defold.as_ref()).and_then(|design| design.project.as_ref()).map(|project| &project.project_id);
    session
        .documents()
        .iter()
        .filter(|state| state.doc.defold.as_ref().and_then(|design| design.project.as_ref()).is_some_and(|project| Some(&project.project_id) == project_id))
        .map(|state| batch::OpenScene {
            document: state.doc.clone(),
            opening: state.open_id,
            revision: state.revision,
            dirty: state.is_dirty(),
            busy: session.jobs().iter().any(|job| job.document == Some(state.doc.id)),
            binding: session.project_client.bindings.get(&state.open_id).cloned(),
        })
        .collect()
}

pub fn shared_review_status(session: &Session) -> Value {
    session
        .active()
        .and_then(|state| session.project_client.shared_reviews.get(&state.open_id))
        .map_or_else(|| json!({}), |review| review.report.as_ref().clone())
}

pub fn review_shared(session: &mut Session) -> Result<Value> {
    let state = session.active().ok_or(EngineError::NoDocument)?;
    let (snapshot, opening, revision) = (state.doc.clone(), state.open_id, state.revision);
    let binding = session.project_client.bindings.get(&opening).cloned().ok_or_else(|| engine("Open a linked project GUI first"))?;
    let openings = capture_openings(session);
    let transaction_id = new_id().map_err(engine)?;
    crate::jobs::run(
        session,
        "Review shared GUI publication",
        false,
        move |job| {
            let mut document = snapshot.as_ref().clone();
            let mut binding = binding;
            let mut review = None;
            batch::publish_if_shared(
                &mut document,
                &mut binding,
                batch::BatchRequest { revision, transaction_id, approval: None, inspect: Some((opening, &mut review)) },
                job,
                &openings,
                &mut Vec::new(),
            )?;
            Ok(review)
        },
        move |session, review| {
            if let Some(review) = review {
                if session.documents().iter().all(|state| state.open_id != opening || state.revision != revision || state.doc.id != review.document)
                    || session.project_client.bindings.get(&opening).is_none_or(|binding| {
                        binding.context != review.context
                            || review.heads.get(&binding.scene.scene_id).is_none_or(|identity| identity.source_revision != binding.source_revision)
                    })
                {
                    return Err(engine("The GUI changed while reviewing related sources; check the publication scope again"));
                }
                let report = review.report.as_ref().clone();
                session.project_client.shared_reviews.insert(opening, Arc::new(review));
                Ok(report)
            } else {
                session.project_client.shared_reviews.remove(&opening);
                Ok(json!({"openId":opening,"capturedNativeRevision":revision,"targets":[]}))
            }
        },
    )
}

pub fn publish_with_params(session: &mut Session, params: &Value) -> Result<Value> {
    let state = session.active().ok_or(EngineError::NoDocument)?;
    if state.floating.is_some() {
        return Err(engine("Finish the floating selection before publishing"));
    }
    let snapshot = Arc::clone(&state.doc);
    let document = state.doc.id;
    let open_id = state.open_id;
    let revision = state.revision;
    let previous = session.project_client.bindings.get(&open_id).cloned();
    let approval = match params.get("sharedReviewId") {
        Some(Value::String(id)) => Some(
            session
                .project_client
                .shared_reviews
                .get(&open_id)
                .filter(|review| review.review_id == *id && review.opening == open_id && review.revision == revision)
                .cloned()
                .ok_or_else(|| engine("SHARED_REVIEW_EXPIRED: Review the related GUI sources again before including their saved changes"))?,
        ),
        None | Some(Value::Null) => None,
        _ => return Err(engine("sharedReviewId must name a reviewed publication scope")),
    };
    let openings = capture_openings(session);
    let transaction_id = new_id().map_err(engine)?;
    client::run_captured_project(
        session,
        "Publish GUI to Defold",
        move |job| {
            let mut doc = snapshot.as_ref().clone();
            let mut binding = match previous {
                Some(binding) => binding,
                None => client::bind_document(&mut doc)?,
            };
            let mut related = Vec::new();
            let result = publish_captured(
                &mut doc,
                &mut binding,
                batch::BatchRequest { revision, transaction_id, approval: approval.as_deref(), inspect: None },
                job,
                &openings,
                &mut related,
            );
            // Preserve source acknowledgements and lease epochs even if publication failed.
            Ok((binding, doc, result, related))
        },
        move |session, (binding, doc, result, related)| {
            for acknowledgement in related {
                let current = session.project_client.bindings.get(&acknowledgement.opening);
                let previous = &acknowledgement.previous_binding;
                if current.is_none_or(|current| {
                    current.context != previous.context
                        || current.client_id != previous.client_id
                        || current.scene.scene_id != previous.scene.scene_id
                        || current.source_revision != previous.source_revision
                        || current.pending_publication.is_some()
                        || current.pending_source_is_some()
                }) {
                    continue;
                }
                if let Some(index) =
                    session.documents().iter().position(|state| state.open_id == acknowledgement.opening && state.doc.id == acknowledgement.document)
                {
                    let replace = acknowledgement.refresh_document
                        && session.documents().get(index).is_some_and(|state| state.revision == acknowledgement.revision)
                        && session.job_on(acknowledgement.document).is_none();
                    let saved_revision = acknowledgement.binding.last_saved_native_revision;
                    let mut acknowledged_binding = acknowledgement.binding;
                    let focused = session.active_index();
                    session.set_active(index);
                    if let Some(state) = session.active_mut() {
                        if acknowledgement.refresh_document && saved_revision != 0 {
                            state.saved_revision = saved_revision;
                        }
                        if replace {
                            state.doc = Arc::new(acknowledgement.saved_document);
                            state.revision = state.revision.saturating_add(1);
                            state.saved_revision = state.revision;
                            state.last_damage = None;
                            acknowledged_binding.last_saved_native_revision = state.revision;
                            if let Some(pending) = acknowledged_binding.pending_publication.as_mut() {
                                Arc::make_mut(pending).1 = state.revision;
                            }
                            crate::fix_selection(state);
                        }
                    }
                    if let Some(focused) = focused {
                        session.set_active(focused);
                    }
                    session.project_client.bindings.insert(acknowledgement.opening, acknowledged_binding);
                }
            }
            if let Some(index) = session.documents().iter().position(|state| state.doc.id == document && state.open_id == open_id) {
                let saved_revision = binding.last_saved_native_revision;
                let can_replace_doc = session.job_on(document).is_none();
                session.project_client.bindings.insert(open_id, binding);
                let focused = session.active_index();
                session.set_active(index);
                let mut derived_revision = None;
                if let Some(state) = session.active_mut()
                    && saved_revision == revision
                {
                    state.saved_revision = saved_revision;
                    if state.revision == revision && can_replace_doc {
                        state.doc = Arc::new(match &result {
                            Ok(published) if published.receipt.phase == PublishPhase::Committed => published.document.clone(),
                            _ => doc,
                        });
                        if state
                            .doc
                            .defold
                            .as_ref()
                            .and_then(|design| design.project.as_ref())
                            .is_some_and(|project| project.layers.values().any(|layer| matches!(layer.appearance, Some(AppearanceSource::Shared { .. }))))
                        {
                            state.revision = state.revision.saturating_add(1);
                            state.saved_revision = state.revision;
                            state.last_damage = None;
                            derived_revision = Some(state.revision);
                        }
                        crate::fix_selection(state);
                    }
                }
                if let Some(focused) = focused {
                    session.set_active(focused);
                }
                if let Some(revision) = derived_revision
                    && let Some(binding) = session.project_client.bindings.get_mut(&open_id)
                {
                    binding.last_saved_native_revision = revision;
                    if let Some(pending) = binding.pending_publication.as_mut() {
                        Arc::make_mut(pending).1 = revision;
                    }
                }
            }
            let result = result?;
            Ok(
                json!({"synced":result.receipt.phase==PublishPhase::Committed,"publication":result.receipt,"assets":result.assets,"nodes":result.nodes,"retainedResources":result.retained,
            "documentId":document.0,"openId":open_id,"capturedNativeRevision":revision,"editorSaveRequired":false}),
            )
        },
    )
}

/// Resolve the last transaction without constructing a replacement plan or saving a newer draft.
pub fn refresh(session: &mut Session) -> Result<Value> {
    let state = session.active().ok_or(EngineError::NoDocument)?;
    let snapshot = Arc::clone(&state.doc);
    let document = state.doc.id;
    let open_id = state.open_id;
    let previous = session.project_client.bindings.get(&open_id).cloned();
    client::run_captured_project(
        session,
        "Check Defold publication",
        move |_| {
            let mut doc = snapshot.as_ref().clone();
            let mut binding = match previous {
                Some(binding) => binding,
                None => client::bind_document(&mut doc)?,
            };
            let request = SceneRequest { context: binding.context.clone(), scene_id: binding.scene.scene_id.clone(), path: binding.scene.path.clone() };
            let result = (|| -> Result<Option<PublishReceipt>> {
                let Some(latest) = binding.store.latest_publication(&request).map_err(engine)? else { return Ok(None) };
                let transaction = PublishRequest { context: binding.context.clone(), transaction_id: latest.transaction_id.clone() };
                let mut receipt = binding.store.publish_status(&transaction).map_err(engine)?.ok_or_else(|| engine("Publication receipt is missing"))?;
                if receipt.phase == PublishPhase::Prepared {
                    receipt = match binding.store.commit_publish(&transaction) {
                        Ok(receipt) => receipt,
                        Err(error) => recover_commit_error(&binding, &transaction, error)?,
                    };
                }
                if receipt.context != binding.context || receipt.transaction_id != latest.transaction_id || receipt.plan_hash != latest.plan_hash {
                    return Err(engine("IDEMPOTENCY_MISMATCH: Publication status changed transaction identity"));
                }
                if receipt.phase == PublishPhase::Committed {
                    let current = binding.store.snapshot(&request).map_err(engine)?;
                    // A later dirty editor is reported, never adopted as this source's baseline.
                    if !current.dirty {
                        let _ = accept_baseline(&mut binding, &current, Some(&receipt));
                    }
                }
                Ok(Some(receipt))
            })();
            Ok((binding, result))
        },
        move |session, (mut binding, result)| {
            let mut published_revision = None;
            if let Ok(Some(receipt)) = &result {
                if receipt.phase == PublishPhase::Committed
                    && let Some(pending) = binding.pending_publication.take().filter(|pending| pending.0 == receipt.transaction_id)
                {
                    published_revision = Some(pending.1);
                    if session.job_on(document).is_none()
                        && let Some(index) =
                            session.documents().iter().position(|state| state.doc.id == document && state.open_id == open_id && state.revision == pending.1)
                    {
                        let focused = session.active_index();
                        session.set_active(index);
                        if let Some(state) = session.active_mut() {
                            state.doc = Arc::new(pending.2.clone());
                            if state
                                .doc
                                .defold
                                .as_ref()
                                .and_then(|design| design.project.as_ref())
                                .is_some_and(|project| project.layers.values().any(|layer| matches!(layer.appearance, Some(AppearanceSource::Shared { .. }))))
                            {
                                let was_saved = state.saved_revision == state.revision;
                                state.revision = state.revision.saturating_add(1);
                                if was_saved {
                                    state.saved_revision = state.revision;
                                    binding.last_saved_native_revision = state.revision;
                                }
                                state.last_damage = None;
                            }
                            crate::fix_selection(state);
                        }
                        if let Some(focused) = focused {
                            session.set_active(focused);
                        }
                    }
                } else if receipt.phase == PublishPhase::Aborted {
                    binding.pending_publication = None;
                }
            }
            if session.documents().iter().any(|state| state.doc.id == document && state.open_id == open_id) {
                session.project_client.bindings.insert(open_id, binding);
            }
            let receipt = result?;
            Ok(json!({"synced":receipt.as_ref().is_some_and(|receipt|receipt.phase==PublishPhase::Committed),
            "publication":receipt,"documentId":document.0,"openId":open_id,"capturedNativeRevision":published_revision}))
        },
    )
}

/// Cancel only a transaction for which the authority confirms no applying phase began.
pub fn cancel(session: &mut Session) -> Result<Value> {
    let state = session.active().ok_or(EngineError::NoDocument)?;
    let snapshot = Arc::clone(&state.doc);
    let document = state.doc.id;
    let open_id = state.open_id;
    let previous = session.project_client.bindings.get(&open_id).cloned();
    crate::jobs::run(
        session,
        "Cancel prepared publication",
        true,
        move |_| {
            let mut doc = snapshot.as_ref().clone();
            let mut binding = match previous {
                Some(binding) => binding,
                None => client::bind_document(&mut doc)?,
            };
            let request = SceneRequest { context: binding.context.clone(), scene_id: binding.scene.scene_id.clone(), path: binding.scene.path.clone() };
            let latest = binding.store.latest_publication(&request).map_err(engine)?.ok_or_else(|| engine("No publication to cancel"))?;
            let transaction = PublishRequest { context: binding.context.clone(), transaction_id: latest.transaction_id.clone() };
            let current = binding.store.publish_status(&transaction).map_err(engine)?.ok_or_else(|| engine("Publication receipt is missing"))?;
            if !current.phase.may_cancel() {
                return Err(engine("Publication has started applying; check its status to recover safely"));
            }
            let receipt = binding.store.cancel_publish(&transaction).map_err(engine)?;
            if receipt.context != binding.context || receipt.transaction_id != current.transaction_id || receipt.plan_hash != current.plan_hash {
                return Err(engine("IDEMPOTENCY_MISMATCH: Cancel receipt belongs to another publication"));
            }
            if receipt.phase == PublishPhase::Aborted {
                binding.pending_publication = None;
            }
            Ok((binding, receipt))
        },
        move |session, (binding, receipt)| {
            if session.documents().iter().any(|state| state.doc.id == document && state.open_id == open_id) {
                session.project_client.bindings.insert(open_id, binding);
            }
            Ok(json!({"synced":false,"publication":receipt,"documentId":document.0,"openId":open_id}))
        },
    )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "defold_project_publish_client_tests.rs"]
mod tests;
