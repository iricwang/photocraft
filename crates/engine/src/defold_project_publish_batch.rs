//! Freeze all shared-source owners and consumers before a single editor transaction.
use super::*;

#[derive(Clone)]
pub(super) struct OpenScene {
    pub document: Arc<Document>,
    pub opening: u64,
    pub revision: u64,
    pub dirty: bool,
    pub busy: bool,
    pub binding: Option<ClientBinding>,
}
pub(super) struct RelatedAcknowledgement {
    pub opening: u64,
    pub document: photocraft_doc::DocId,
    pub revision: u64,
    pub binding: ClientBinding,
    pub saved_document: Document,
    pub previous_binding: ClientBinding,
    pub refresh_document: bool,
}
struct FrozenScene {
    document: Document,
    binding: ClientBinding,
    revision: u64,
    target: Option<PublishTarget>,
}

pub(crate) struct SharedPublicationReview {
    pub context: ProjectContext,
    pub document: photocraft_doc::DocId,
    pub opening: u64,
    pub revision: u64,
    pub review_id: String,
    pub heads: BTreeMap<SceneId, SceneIdentity>,
    pub report: Arc<Value>,
}
pub(super) struct BatchRequest<'a> {
    pub revision: u64,
    pub transaction_id: TransactionId,
    pub approval: Option<&'a SharedPublicationReview>,
    pub inspect: Option<(u64, &'a mut Option<SharedPublicationReview>)>,
}

fn references(document: &Document) -> Vec<SharedAppearanceRef> {
    document
        .defold
        .as_ref()
        .and_then(|design| design.project.as_ref())
        .into_iter()
        .flat_map(|project| project.layers.values())
        .filter_map(|layer| match &layer.appearance {
            Some(AppearanceSource::Shared { source }) => Some(source.clone()),
            _ => None,
        })
        .collect()
}

fn scene_request(binding: &ClientBinding) -> SceneRequest {
    SceneRequest { context: binding.context.clone(), scene_id: binding.scene.scene_id.clone(), path: binding.scene.path.clone() }
}

fn freeze_scene(primary: &ClientBinding, scene: &SceneIdentity, openings: &[OpenScene], root: &str, budget: &mut u64) -> Result<FrozenScene> {
    let opened: Vec<_> = openings
        .iter()
        .filter(|opening| {
            opening
                .document
                .defold
                .as_ref()
                .and_then(|d| d.project.as_ref())
                .is_some_and(|p| p.project_id == primary.context.project_id && p.scene_id == scene.scene_id)
        })
        .collect();
    for opening in &opened {
        if opening.dirty || opening.busy {
            return Err(engine(format!(
                "TARGET_DIRTY: Shared GUI {} has an unfinished PhotoCraft draft or task; preserve it before publishing the shared source",
                scene.path
            )));
        }
        if opening.binding.as_ref().is_none_or(|binding| binding.context != primary.context || binding.source_revision != scene.source_revision) {
            return Err(engine(format!("SOURCE_CONFLICT: Shared GUI {} has a different open source version", scene.path)));
        }
        if opening.binding.as_ref().is_some_and(|binding| binding.pending_publication.is_some() || binding.pending_source_is_some()) {
            return Err(engine("RECOVERY_REQUIRED: A related GUI has an unacknowledged source or publication"));
        }
    }
    let gui = std::path::Path::new(&root).join(scene.path.as_str().trim_start_matches('/'));
    let source_revision = scene.source_revision.clone().ok_or_else(|| engine("A shared GUI has no complete saved native source"))?;
    let source = primary
        .store
        .read_source(&ReadSourceRequest {
            context: primary.context.clone(),
            package_id: scene.package_id.clone(),
            source_revision: Some(source_revision.clone()),
        })
        .map_err(engine)?;
    if source.envelope.package_id != scene.package_id || source.envelope.source_revision != source_revision {
        return Err(engine("SOURCE_CONFLICT: Shared source response changed package identity"));
    }
    source.envelope.verify_payload(&source.payload, &ProjectLimits::default()).map_err(engine)?;
    let package_bytes = source.envelope.dependencies.iter().try_fold(source.envelope.native.length, |sum, dependency| {
        sum.checked_add(dependency.content.length).ok_or_else(|| engine("Shared publication source size overflow"))
    })?;
    *budget = budget.checked_add(package_bytes).ok_or_else(|| engine("Shared publication source size overflow"))?;
    if *budget > ProjectLimits::default().max_total_source_bytes {
        return Err(engine("Shared GUI sources exceed the publication transfer budget"));
    }
    let mut document = photocraft_format::load_from_bytes(&source.payload.native).map_err(engine)?;
    crate::defold_project_dependencies::restore(&mut document, &source, root).map_err(engine)?;
    let design = document.defold.as_mut().ok_or_else(|| engine("Shared native source has no linked GUI"))?;
    let project = design.project.as_ref().ok_or_else(|| engine("Shared native source has no stable GUI identity"))?;
    if project.project_id != primary.context.project_id
        || project.scene_id != scene.scene_id
        || project.package_id != scene.package_id
        || project.gui_path != scene.path.as_str()
    {
        return Err(engine("Shared native source belongs to another GUI"));
    }
    design.root = root.to_owned();
    design.gui = gui.to_string_lossy().into_owned();
    let mut binding = primary.clone();
    binding.scene = scene.clone();
    binding.source_revision = Some(source_revision);
    binding.baseline = source
        .envelope
        .scene_bases
        .into_iter()
        .find(|base| base.scene_id == scene.scene_id && base.path == scene.path)
        .ok_or_else(|| engine("Shared creative source has no target baseline"))?;
    binding.last_saved_native_revision = 0;
    binding.pending_publication = None;
    let revision = opened.first().map_or(0, |opening| opening.revision);
    Ok(FrozenScene { document, binding, revision, target: None })
}

fn visit_package(
    package: &PackageId,
    scenes: &BTreeMap<SceneId, FrozenScene>,
    visiting: &mut BTreeSet<PackageId>,
    done: &mut BTreeSet<PackageId>,
    order: &mut Vec<SceneId>,
) -> Result<()> {
    if done.contains(package) {
        return Ok(());
    }
    if visiting.len() >= 64 || !visiting.insert(package.clone()) {
        return Err(engine("Shared GUI source packages form a cycle or exceed the dependency limit"));
    }
    let (scene_id, scene) = scenes
        .iter()
        .find(|(_, scene)| scene.binding.scene.package_id == *package)
        .ok_or_else(|| engine("Shared owner package is outside the frozen publication targets"))?;
    for reference in references(&scene.document) {
        visit_package(&reference.package_id, scenes, visiting, done, order)?;
    }
    visiting.remove(package);
    done.insert(package.clone());
    order.push(scene_id.clone());
    Ok(())
}

fn acquire(binding: &mut ClientBinding, resources: BTreeSet<LeaseResource>) -> Result<()> {
    for resource in resources {
        let previous = binding.leases.iter().find(|proof| proof.resource == resource);
        let renewed = previous.map(|proof| {
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
        binding.leases.retain(|proof| proof.resource != resource);
        binding.leases.push(grant.proof);
    }
    Ok(())
}

fn acknowledge_related(
    scenes: &BTreeMap<SceneId, FrozenScene>,
    openings: &[OpenScene],
    primary_scene: &SceneId,
    acknowledgements: &mut Vec<RelatedAcknowledgement>,
    committed: bool,
) {
    acknowledgements.clear();
    for opening in openings {
        let Some(project) = opening.document.defold.as_ref().and_then(|d| d.project.as_ref()) else { continue };
        if &project.scene_id == primary_scene {
            continue;
        }
        if let Some(scene) = scenes.get(&project.scene_id) {
            let Some(previous_binding) = opening.binding.clone() else { continue };
            let refresh_document = committed || previous_binding.source_revision != scene.binding.source_revision;
            let mut document = scene.document.clone();
            document.id = opening.document.id;
            acknowledgements.push(RelatedAcknowledgement {
                opening: opening.opening,
                document: opening.document.id,
                revision: opening.revision,
                binding: scene.binding.clone(),
                saved_document: document,
                previous_binding,
                refresh_document,
            });
        }
    }
}

/// `None` means the established single-scene path is sufficient. Once a shared
/// graph is found, no game write occurs until every source and target is checked.
pub(super) fn publish_if_shared(
    document: &mut Document,
    binding: &mut ClientBinding,
    request: BatchRequest<'_>,
    job: &crate::jobs::JobCtx,
    openings: &[OpenScene],
    acknowledgements: &mut Vec<RelatedAcknowledgement>,
) -> Result<Option<Published>> {
    let BatchRequest { revision, transaction_id, approval, inspect } = request;
    let registry = binding.store.read_asset_registry(&binding.context).map_err(engine)?;
    let shared = references(document);
    let supports_shared = binding.store.describe().map_err(engine)?.capabilities.contains("sharedArtworkV1");
    if !supports_shared {
        if !shared.is_empty() {
            return Err(engine("This project host cannot publish shared artwork"));
        }
        return Ok(None);
    }
    let primary_scene = binding.scene.scene_id.clone();
    let project_root = &document.defold.as_ref().ok_or_else(|| engine("Missing GUI project render context"))?.root;
    let primary_project = document.defold.as_ref().and_then(|d| d.project.as_ref()).ok_or_else(|| engine("Missing stable GUI identity"))?;
    let mut scenes = BTreeMap::from([(primary_scene.clone(), FrozenScene { document: document.clone(), binding: binding.clone(), revision, target: None })]);
    let mut inspected = BTreeSet::new();
    let mut source_budget = 0_u64;
    let mut queued: BTreeSet<_> = shared.iter().map(|reference| reference.asset_id.clone()).collect();
    queued.extend(
        registry
            .assets
            .values()
            .filter(|asset| {
                asset.source_node.is_some()
                    && (asset.referencing_scenes.contains(&primary_scene)
                        || asset.source_node.as_ref().is_some_and(|node| node.scene_id == primary_scene)
                            && asset.source.as_ref().is_some_and(|source| source.package_id == primary_project.package_id))
            })
            .map(|asset| asset.asset_id.clone()),
    );
    while let Some(asset_id) = queued.pop_first() {
        if !inspected.insert(asset_id.clone()) {
            continue;
        }
        if inspected.len() > 256 {
            return Err(engine("Shared publication exceeds the asset dependency limit"));
        }
        let usage = binding.store.shared_asset_usage(&SharedAssetRequest { context: binding.context.clone(), asset_id }).map_err(engine)?;
        for identity in usage.consumers {
            if scenes.contains_key(&identity.scene_id) {
                continue;
            }
            if scenes.len() >= 64 {
                return Err(engine("Shared publication exceeds the GUI target limit"));
            }
            let scene = freeze_scene(binding, &identity, openings, project_root, &mut source_budget)?;
            queued.extend(references(&scene.document).into_iter().map(|reference| reference.asset_id));
            queued.extend(
                registry
                    .assets
                    .values()
                    .filter(|asset| {
                        asset.source_node.is_some()
                            && (asset.referencing_scenes.contains(&identity.scene_id)
                                || asset.source_node.as_ref().is_some_and(|node| node.scene_id == identity.scene_id)
                                    && asset.source.as_ref().is_some_and(|source| source.package_id == identity.package_id))
                    })
                    .map(|asset| asset.asset_id.clone()),
            );
            scenes.insert(identity.scene_id, scene);
        }
    }
    if scenes.len() == 1 && shared.is_empty() {
        return Ok(None);
    }
    let captured_primary = crate::defold_project_dependencies::capture(document).map_err(engine)?;
    let primary_bytes = captured_primary.bodies.values().try_fold(captured_primary.native.len() as u64, |sum, bytes| {
        sum.checked_add(bytes.len() as u64).ok_or_else(|| engine("Shared publication source size overflow"))
    })?;
    if source_budget.checked_add(primary_bytes).is_none_or(|bytes| bytes > ProjectLimits::default().max_total_source_bytes) {
        return Err(engine("Shared GUI sources exceed the publication transfer budget"));
    }
    drop(captured_primary);
    // Validate canonical carriers against the prospective source documents,
    // before pinning any consumer to a new immutable owner revision.
    for scene in scenes.values() {
        for reference in references(&scene.document) {
            let owner = scenes
                .values()
                .find(|owner| owner.binding.scene.package_id == reference.package_id)
                .ok_or_else(|| engine("Shared owner is absent from the frozen source set"))?;
            let project =
                owner.document.defold.as_ref().and_then(|design| design.project.as_ref()).ok_or_else(|| engine("Shared owner has no stable GUI identity"))?;
            if project.scene_id != reference.source_node.scene_id
                || !project.layers.values().any(|layer| {
                    layer.layer_id == reference.source_node.root_layer_id
                        && layer.node_id.as_ref() == Some(&reference.source_node.node_id)
                        && layer.asset_id.as_ref() == Some(&reference.asset_id)
                        && layer.role == LayerRole::GuiNode
                        && !matches!(layer.appearance, Some(AppearanceSource::Shared { .. }))
                })
            {
                return Err(engine("Shared owner artwork was removed or changed identity; detach its consumers before publishing"));
            }
            crate::defold_shared_design::extract_shared_artwork(&owner.document, &reference.source_node)?;
        }
    }
    let mut order = Vec::new();
    let mut done = BTreeSet::new();
    for scene in scenes.values() {
        visit_package(&scene.binding.scene.package_id, &scenes, &mut BTreeSet::new(), &mut done, &mut order)?;
    }
    let heads: BTreeMap<_, _> = scenes
        .values()
        .map(|scene| {
            let mut identity = scene.binding.scene.clone();
            identity.source_revision = scene.binding.source_revision.clone();
            (identity.scene_id.clone(), identity)
        })
        .collect();
    let mut needs_approval = false;
    let mut impacts = Vec::new();
    for scene in scenes.values() {
        let latest = binding.store.latest_publication(&scene_request(&scene.binding)).map_err(engine)?;
        if latest.as_ref().is_some_and(|receipt| !receipt.phase.is_terminal()) {
            return Err(engine("RECOVERY_REQUIRED: A shared GUI has an unfinished publication"));
        }
        let published_source = latest
            .as_ref()
            .filter(|receipt| receipt.phase == PublishPhase::Committed)
            .and_then(|receipt| receipt.source_revisions.iter().find(|source| source.package_id == scene.binding.scene.package_id));
        let unpublished =
            scene.binding.scene.scene_id != primary_scene && published_source.map(|source| &source.source_revision) != scene.binding.source_revision.as_ref();
        needs_approval |= unpublished;
        impacts.push(json!({"path":scene.binding.scene.path,"sourceRevision":scene.binding.source_revision,
            "unpublishedSource":unpublished,"primary":scene.binding.scene.scene_id == primary_scene}));
    }
    if let Some((opening, result)) = inspect {
        let review_id: OperationId = new_id().map_err(engine)?;
        let report = json!({"openId":opening,"capturedNativeRevision":revision,"sharedReviewId":review_id,
            "targets":impacts,"requiresRelatedSourceApproval":needs_approval});
        *result = Some(SharedPublicationReview {
            context: binding.context.clone(),
            document: document.id,
            opening,
            revision,
            review_id: review_id.to_string(),
            heads,
            report: Arc::new(report),
        });
        return Ok(None);
    }
    if let Some(approval) = approval {
        if approval.context != binding.context || approval.document != document.id || approval.revision != revision || approval.heads != heads {
            return Err(engine("SHARED_REVIEW_EXPIRED: The GUI or a related source changed; review the publication scope again"));
        }
    } else if needs_approval {
        return Err(engine(
            "SHARED_REVIEW_REQUIRED: Related GUIs contain saved changes that have not been published; review and explicitly include those changes before synchronizing",
        ));
    }
    let resources: BTreeSet<_> = scenes
        .values()
        .flat_map(|scene| [LeaseResource::Package(scene.binding.scene.package_id.clone()), LeaseResource::Scene(scene.binding.scene.scene_id.clone())])
        .chain(scenes.values().flat_map(|scene| {
            scene
                .document
                .defold
                .as_ref()
                .and_then(|d| d.project.as_ref())
                .into_iter()
                .flat_map(|project| project.layers.values())
                .filter_map(|layer| layer.asset_id.clone())
                .map(LeaseResource::Asset)
        }))
        .chain(inspected.into_iter().map(LeaseResource::Asset))
        .collect();
    acquire(binding, resources)?;
    // Acquire the whole sorted lease set before any creative-source CAS. A
    // consumer's newer open draft is never used as hidden publication input.
    for scene in scenes.values_mut() {
        scene.binding.leases = binding.leases.clone();
        let identity = binding
            .store
            .scene_identity(&SceneIdentityRequest { context: binding.context.clone(), scene_id: scene.binding.scene.scene_id.clone() })
            .map_err(engine)?;
        if identity.source_revision != scene.binding.source_revision
            || identity.path != scene.binding.scene.path
            || identity.package_id != scene.binding.scene.package_id
        {
            return Err(engine("SOURCE_CONFLICT: A shared GUI source head changed before publication"));
        }
        let request = scene_request(&scene.binding);
        let latest = binding.store.latest_publication(&request).map_err(engine)?;
        if latest.as_ref().is_some_and(|receipt| !receipt.phase.is_terminal()) {
            return Err(engine("RECOVERY_REQUIRED: A shared GUI has an unfinished publication"));
        }
        let current = binding.store.snapshot(&request).map_err(engine)?;
        let published = accept_baseline(&mut scene.binding, &current, latest.as_ref())?;
        scene.target = Some(PublishTarget {
            scene_id: current.scene_id,
            path: current.path,
            expected_published_revision: published,
            expected_gui_disk_hash: current.disk_hash,
            expected_editor_snapshot: current.editor_graph_revision,
        });
    }
    let mut heads: BTreeMap<_, _> =
        scenes.values().filter_map(|scene| scene.binding.source_revision.clone().map(|revision| (scene.binding.scene.package_id.clone(), revision))).collect();
    for scene_id in &order {
        job.check()?;
        let scene = scenes.get_mut(scene_id).ok_or_else(|| engine("Frozen shared GUI disappeared"))?;
        let mut changed = false;
        if let Some(project) = scene.document.defold.as_mut().and_then(|design| design.project.as_mut()) {
            for layer in project.layers.values_mut() {
                if let Some(AppearanceSource::Shared { source }) = &mut layer.appearance {
                    let head = heads.get(&source.package_id).ok_or_else(|| engine("Canonical shared package has no acknowledged source head"))?;
                    if source.source_revision != *head {
                        source.source_revision.clone_from(head);
                        changed = true;
                    }
                }
            }
        }
        if *scene_id == primary_scene || changed {
            client::hydrate_shared_sources(&mut scene.document, &scene.binding)?;
            let result = client::save_captured_document(&scene.document, &mut scene.binding, scene.revision);
            if *scene_id == primary_scene {
                *binding = scene.binding.clone();
                if result.is_ok() {
                    *document = scene.document.clone();
                }
            }
            // Even a later source failure must return all acknowledged heads to
            // the correct openings, without overwriting concurrent painting.
            if let Err(error) = result {
                acknowledge_related(&scenes, openings, &primary_scene, acknowledgements, false);
                return Err(error);
            }
        }
        let head = scene.binding.source_revision.clone().ok_or_else(|| engine("Shared GUI source was not acknowledged"))?;
        heads.insert(scene.binding.scene.package_id.clone(), head);
        acknowledge_related(&scenes, openings, &primary_scene, acknowledgements, false);
    }
    acknowledge_related(&scenes, openings, &primary_scene, acknowledgements, false);
    let result = (|| -> Result<Published> {
        let shared_assets: BTreeSet<_> = scenes.values().flat_map(|scene| references(&scene.document)).map(|reference| reference.asset_id).collect();
        let mut all_resources = BTreeMap::new();
        let mut inputs = Vec::new();
        let mut node_count = 0;
        for scene in scenes.values_mut() {
            client::hydrate_shared_sources(&mut scene.document, &scene.binding)?;
            let mut captured = capture_assets(&scene.binding, &scene.document)?;
            if let Some(design) = &scene.document.defold
                && let Some(project) = &design.project
            {
                captured.native.shared_owners.extend(
                    design
                        .nodes
                        .iter()
                        .filter(|node| project.layers.get(&node.layer).and_then(|layer| layer.asset_id.as_ref()).is_some_and(|id| shared_assets.contains(id)))
                        .map(|node| node.asset_id.clone()),
                );
            }
            if captured.registry != registry {
                return Err(engine("TARGET_CHANGED: Asset inventory changed while preparing shared publication"));
            }
            for (path, resource) in captured.resources {
                if all_resources.get(&path).is_some_and(|old: &ResourceContent| old.version.content != resource.version.content) {
                    return Err(engine("TARGET_CHANGED: A shared resource changed during capture"));
                }
                all_resources.insert(path, resource);
            }
            let native = crate::defold_design::plan_publication(scene.document.clone(), captured.native)?;
            node_count += native.output.nodes.len();
            inputs.push(defold_publish_plan::PublicationSceneInput {
                native,
                target: scene.target.clone().ok_or_else(|| engine("Shared GUI has no frozen target"))?,
                folder: captured.folder,
                atlas: captured.atlas,
                protected_frames: captured.protected,
            });
        }
        let source_revisions = heads.into_iter().map(|(package_id, source_revision)| SourceRevisionRef { package_id, source_revision }).collect();
        let bridge = defold_publish_plan::bridge_publication_batch(defold_publish_plan::PublicationBatchInput {
            context: binding.context.clone(),
            client_id: binding.client_id.clone(),
            transaction_id,
            source_revisions,
            leases: binding.leases.clone(),
            registry,
            resources: all_resources,
            scenes: inputs,
        })
        .map_err(engine)?;
        let mut plan = bridge.plan;
        plan.primary_scene_id = primary_scene.clone();
        acquire(binding, bridge.required_resources)?;
        let required: BTreeSet<_> = plan
            .source_revisions
            .iter()
            .map(|source| LeaseResource::Package(source.package_id.clone()))
            .chain(plan.targets.iter().map(|target| LeaseResource::Scene(target.scene_id.clone())))
            .chain(plan.asset_changes.iter().map(|asset| LeaseResource::Asset(asset.asset_id.clone())))
            .collect();
        plan.leases = binding.leases.iter().filter(|proof| required.contains(&proof.resource)).cloned().collect();
        plan.plan_hash = plan.computed_hash().map_err(engine)?;
        let published_doc = bridge.documents.get(&primary_scene).cloned().ok_or_else(|| engine("Shared publication has no primary document"))?;
        binding.pending_publication = Some(Arc::new((plan.transaction_id.clone(), revision, published_doc.clone())));
        for (scene_id, scene) in &mut scenes {
            if let Some(document) = bridge.documents.get(scene_id) {
                scene.binding.pending_publication = Some(Arc::new((plan.transaction_id.clone(), scene.revision, document.clone())));
            }
        }
        job.check()?;
        job.progress(0.75, "Publish shared GUI artwork");
        let receipt = transact(binding, &plan, &bridge.payload)?;
        if receipt.phase == PublishPhase::Committed {
            for (scene_id, scene) in &mut scenes {
                let target =
                    receipt.targets.iter().find(|target| &target.scene_id == scene_id).ok_or_else(|| engine("Shared publication omitted a target receipt"))?;
                let content = read(&scene.binding, &target.path, Some(target.disk_hash.clone()))?;
                scene.binding.baseline = SceneBaseline {
                    scene_id: scene_id.clone(),
                    path: target.path.clone(),
                    gui_source: String::from_utf8(content.bytes).map_err(engine)?,
                    disk_hash: target.disk_hash.clone(),
                    scene_hash: target.scene_hash.clone(),
                    published_revision: Some(target.published_revision.clone()),
                };
                if let Some(document) = bridge.documents.get(scene_id) {
                    scene.document = document.clone();
                }
                scene.binding.pending_publication = None;
            }
            if let Some(primary) = scenes.get(&primary_scene) {
                binding.baseline = primary.binding.baseline.clone();
            }
            binding.pending_publication = None;
        } else if receipt.phase == PublishPhase::Aborted {
            binding.pending_publication = None;
            for scene in scenes.values_mut() {
                scene.binding.pending_publication = None;
            }
        }
        Ok(Published {
            document: published_doc,
            receipt,
            assets: bridge.registry.assets.values().filter(|asset| asset.state == AssetState::Active).count(),
            nodes: node_count,
            retained: bridge.retained,
        })
    })();
    let committed = result.as_ref().is_ok_and(|published| published.receipt.phase == PublishPhase::Committed);
    acknowledge_related(&scenes, openings, &primary_scene, acknowledgements, committed);
    result.map(Some)
}
