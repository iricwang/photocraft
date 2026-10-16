//! Read-only authoritative shared-source discovery. Failure to enumerate is never an empty usage set.
use super::*;

fn identity(state: &State, scene_id: &SceneId) -> ProjectResult<SceneIdentity> {
    let mut scene = state.index.scenes.get(scene_id).cloned().ok_or_else(|| fail(ProjectErrorCode::ResourceMissing, "Shared GUI scene is not registered"))?;
    if state
        .index
        .scenes
        .values()
        .any(|other| other.scene_id != scene.scene_id && (other.path.case_key() == scene.path.case_key() || other.package_id == scene.package_id))
    {
        return Err(fail(ProjectErrorCode::NameCollision, "GUI scene identity has a path or source-package alias"));
    }
    scene.source_revision = state.index.heads.get(&scene.package_id).cloned();
    Ok(scene)
}

pub(super) fn scene_identity(store: &LocalProjectStore, request: &SceneIdentityRequest) -> ProjectResult<SceneIdentity> {
    let mut state = store.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    store.check_context(&mut state, &request.context)?;
    identity(&state, &request.scene_id)
}

fn source_document(store: &LocalProjectStore, package: &PackageId, revision: &str, total: &mut u64) -> ProjectResult<photocraft_doc::Document> {
    let source = store.package(package, revision)?;
    *total = total.checked_add(source.envelope.native.length).ok_or_else(|| fail(ProjectErrorCode::LimitExceeded, "Shared-source inventory size overflow"))?;
    if *total > store.limits.max_total_source_bytes {
        return Err(fail(ProjectErrorCode::LimitExceeded, "Shared-source discovery exceeds the bounded native-source budget"));
    }
    photocraft_format::load_from_bytes(&source.payload.native).map_err(io)
}

pub(super) fn verify_node(
    document: &photocraft_doc::Document,
    context: &ProjectContext,
    source: &SourceRevisionRef,
    node: &ArtworkSourceNode,
    asset: &AssetId,
) -> ProjectResult<()> {
    let project = document
        .defold
        .as_ref()
        .and_then(|design| design.project.as_ref())
        .ok_or_else(|| fail(ProjectErrorCode::InvalidRequest, "Canonical artwork has no GUI source attachment"))?;
    if project.project_id != context.project_id || project.package_id != source.package_id || project.scene_id != node.scene_id {
        return Err(fail(ProjectErrorCode::ProjectMismatch, "Canonical artwork belongs to another GUI source package"));
    }
    let matches: Vec<_> = project.layers.iter().filter(|(_, binding)| binding.layer_id == node.root_layer_id).collect();
    let Some((local, binding)) = matches.first().filter(|_| matches.len() == 1) else {
        return Err(fail(ProjectErrorCode::NameCollision, "Canonical artwork root is missing or aliased"));
    };
    if binding.role != LayerRole::GuiNode
        || binding.node_id.as_ref() != Some(&node.node_id)
        || binding.asset_id.as_ref() != Some(asset)
        || matches!(binding.appearance, Some(AppearanceSource::Shared { .. }))
        || document.layer(photocraft_doc::LayerId(**local)).is_none()
        || document
            .defold
            .as_ref()
            .is_none_or(|design| !design.nodes.iter().any(|link| link.layer == **local && Some(&link.node) == binding.gui_node_id.as_ref()))
    {
        return Err(fail(ProjectErrorCode::InvalidRequest, "Canonical artwork must be its owner's real GUI node, not a consumer cache"));
    }
    Ok(())
}

struct Inventory {
    documents: BTreeMap<(PackageId, String), photocraft_doc::Document>,
    total: u64,
}
impl Inventory {
    fn new() -> Self {
        Self { documents: BTreeMap::new(), total: 0 }
    }
    fn document<'a>(&'a mut self, store: &LocalProjectStore, source: &SourceRevisionRef) -> ProjectResult<&'a photocraft_doc::Document> {
        let key = (source.package_id.clone(), source.source_revision.clone());
        if let std::collections::btree_map::Entry::Vacant(entry) = self.documents.entry(key.clone()) {
            let document = source_document(store, &source.package_id, &source.source_revision, &mut self.total)?;
            entry.insert(document);
        }
        self.documents.get(&key).ok_or_else(|| fail(ProjectErrorCode::ResourceMissing, "Shared source snapshot is missing"))
    }
}

pub(super) fn validate_source_links(store: &LocalProjectStore, request: &SaveSourceRequest, document: &photocraft_doc::Document) -> ProjectResult<()> {
    let mut pins = BTreeMap::new();
    let mut inventory = Inventory::new();
    if let Some(project) = document.defold.as_ref().and_then(|design| design.project.as_ref()) {
        for binding in project.layers.values() {
            let Some(AppearanceSource::Shared { source }) = &binding.appearance else { continue };
            if binding.role != LayerRole::GuiNode || binding.asset_id.as_ref() != Some(&source.asset_id) || source.package_id == request.package_id {
                return Err(fail(ProjectErrorCode::InvalidRequest, "Shared source identity is not a foreign GUI artwork reference"));
            }
            if pins.insert(source.package_id.clone(), source.source_revision.clone()).is_some_and(|old| old != source.source_revision) {
                return Err(fail(ProjectErrorCode::SourceConflict, "One GUI cannot pin two versions of the same shared source package"));
            }
            let revision = SourceRevisionRef { package_id: source.package_id.clone(), source_revision: source.source_revision.clone() };
            verify_node(inventory.document(store, &revision)?, &request.context, &revision, &source.source_node, &source.asset_id)?;
        }
    }
    let declared: BTreeMap<_, _> =
        request.envelope.draft.shared_sources.iter().map(|source| (source.package_id.clone(), source.source_revision.clone())).collect();
    if pins != declared {
        return Err(fail(ProjectErrorCode::InvalidRequest, "Shared native references differ from the source envelope's immutable dependency roots"));
    }
    Ok(())
}

fn discover(
    store: &LocalProjectStore,
    state: &State,
    asset: &AssetId,
    source: &SourceRevisionRef,
    node: &ArtworkSourceNode,
    strict_pins: bool,
    inventory: &mut Inventory,
) -> ProjectResult<BTreeSet<SceneId>> {
    let context = &state.checkout.context;
    verify_node(inventory.document(store, source)?, context, source, node, asset)?;
    let mut consumers = BTreeSet::new();
    for (package, revision) in &state.index.heads {
        let head = SourceRevisionRef { package_id: package.clone(), source_revision: revision.clone() };
        let document = inventory.document(store, &head)?;
        let Some(project) = document.defold.as_ref().and_then(|design| design.project.as_ref()) else { continue };
        if project.package_id != *package || project.project_id != context.project_id {
            return Err(fail(ProjectErrorCode::ProjectMismatch, "Saved GUI source identity differs from its package"));
        }
        let scene = identity(state, &project.scene_id)?;
        if scene.package_id != *package || scene.path.as_str() != project.gui_path {
            return Err(fail(ProjectErrorCode::ProjectMismatch, "Saved GUI source does not match the scene registry"));
        }
        let mut pins = Vec::new();
        for (local, binding) in project.layers.iter().filter(|(_, binding)| binding.asset_id.as_ref() == Some(asset)) {
            if binding.role != LayerRole::GuiNode
                || document.layer(photocraft_doc::LayerId(*local)).is_none()
                || document
                    .defold
                    .as_ref()
                    .is_none_or(|design| !design.nodes.iter().any(|link| link.layer == *local && Some(&link.node) == binding.gui_node_id.as_ref()))
            {
                return Err(fail(ProjectErrorCode::InvalidRequest, "Shared artwork identity has no live native GUI node"));
            }
            match &binding.appearance {
                Some(AppearanceSource::Shared { source: reference }) => {
                    if reference.asset_id != *asset
                        || reference.package_id != source.package_id
                        || reference.source_node != *node
                        || reference.package_id == *package
                    {
                        return Err(fail(ProjectErrorCode::NameCollision, "Shared asset has conflicting or self-referential canonical sources"));
                    }
                    if strict_pins && reference.source_revision != source.source_revision {
                        return Err(fail(ProjectErrorCode::SourceConflict, "Shared consumer is not pinned to this publication's canonical source"));
                    }
                    pins.push(SourceRevisionRef { package_id: reference.package_id.clone(), source_revision: reference.source_revision.clone() });
                    consumers.insert(project.scene_id.clone());
                }
                _ if *package == source.package_id
                    && project.scene_id == node.scene_id
                    && binding.layer_id == node.root_layer_id
                    && binding.node_id.as_ref() == Some(&node.node_id) => {}
                _ => return Err(fail(ProjectErrorCode::NameCollision, "An asset ID is claimed by more than one native artwork owner")),
            }
        }
        for pin in pins {
            verify_node(inventory.document(store, &pin)?, context, &pin, node, asset)?;
        }
    }
    Ok(consumers)
}

pub(super) fn usage(store: &LocalProjectStore, request: &SharedAssetRequest) -> ProjectResult<SharedAssetUsage> {
    let mut state = store.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    store.check_context(&mut state, &request.context)?;
    let registry = crate::defold_project_publication::registry(&store.root)?;
    let asset = registry.assets.get(&request.asset_id).cloned().ok_or_else(|| fail(ProjectErrorCode::ResourceMissing, "Shared artwork is not registered"))?;
    let source = asset.source.as_ref().ok_or_else(|| fail(ProjectErrorCode::UnsupportedGuiFeature, "Asset has no canonical native source"))?;
    let node = asset
        .source_node
        .as_ref()
        .ok_or_else(|| fail(ProjectErrorCode::UnsupportedGuiFeature, "Asset canonical node needs an explicit publication before sharing"))?;
    let mut inventory = Inventory::new();
    let mut consumers = discover(store, &state, &request.asset_id, source, node, false, &mut inventory)?;
    consumers.extend(asset.referencing_scenes.iter().cloned());
    consumers.insert(node.scene_id.clone());
    let consumers = consumers.iter().map(|scene| identity(&state, scene)).collect::<ProjectResult<Vec<_>>>()?;
    let mut heads = BTreeMap::new();
    for scene in &consumers {
        let revision =
            scene.source_revision.clone().ok_or_else(|| fail(ProjectErrorCode::SourceConflict, "Every shared-artwork GUI needs a saved creative source"))?;
        heads.insert(scene.package_id.clone(), revision);
    }
    Ok(SharedAssetUsage {
        asset,
        consumers,
        source_refs: heads.into_iter().map(|(package_id, source_revision)| SourceRevisionRef { package_id, source_revision }).collect(),
    })
}

/// Recheck authoritative native identities while LocalStore holds its write/state lock. Discovery
/// includes saved-but-not-yet-published consumers, so registry references alone are not a quorum.
pub(super) fn validate_publication(store: &LocalProjectStore, state: &State, plan: &PublishPlan) -> ProjectResult<()> {
    let registry = crate::defold_project_publication::registry(&store.root)?;
    let mut inventory = Inventory::new();
    for update in &plan.asset_changes {
        let previous = registry.assets.get(&update.asset_id);
        if previous.is_none() && update.ownership == AssetOwnership::Managed && update.source_node.is_none() {
            return Err(fail(ProjectErrorCode::InvalidRequest, "New managed artwork requires a verified canonical native GUI node"));
        }
        if let Some(old) = previous.and_then(|asset| asset.source_node.as_ref())
            && (update.source_node.as_ref() != Some(old)
                || update.source.as_ref().map(|source| &source.package_id) != previous.and_then(|asset| asset.source.as_ref().map(|source| &source.package_id)))
        {
            return Err(fail(ProjectErrorCode::TargetChanged, "Publication cannot replace or erase a canonical artwork owner"));
        }
        let Some(node) = &update.source_node else { continue };
        let source = update.source.as_ref().ok_or_else(|| fail(ProjectErrorCode::InvalidRequest, "Canonical artwork has no source revision"))?;
        let changed_source = previous.is_none_or(|asset| asset.source.as_ref() != Some(source) || asset.source_node.as_ref() != Some(node));
        if changed_source && !plan.source_revisions.contains(source) {
            return Err(fail(ProjectErrorCode::SourceConflict, "Canonical source registration requires its frozen current package head"));
        }
        if previous.is_some_and(|asset| asset.source.as_ref().is_some_and(|old| old.package_id != source.package_id)) {
            return Err(fail(ProjectErrorCode::TargetChanged, "A consumer cannot claim a legacy asset's source package"));
        }
        let consumers = discover(store, state, &update.asset_id, source, node, true, &mut inventory)?;
        let mut affected = consumers.clone();
        affected.extend(update.referencing_scenes.iter().cloned());
        if changed_source {
            affected.insert(node.scene_id.clone());
        }
        for scene_id in affected {
            let scene = identity(state, &scene_id)?;
            if !plan.targets.iter().any(|target| target.scene_id == scene_id && target.path == scene.path)
                || !plan
                    .source_revisions
                    .iter()
                    .any(|revision| revision.package_id == scene.package_id && Some(&revision.source_revision) == scene.source_revision.as_ref())
            {
                return Err(fail(ProjectErrorCode::SourceConflict, "Shared publication omits an affected GUI or its frozen source head"));
            }
            for resource in [LeaseResource::Scene(scene_id), LeaseResource::Package(scene.package_id)] {
                if !plan.leases.iter().any(|proof| proof.resource == resource) {
                    return Err(fail(ProjectErrorCode::LeaseLost, "Shared publication needs every affected scene and package lease"));
                }
            }
        }
        if consumers.iter().any(|scene| !update.referencing_scenes.contains(scene)) {
            return Err(fail(ProjectErrorCode::InvalidRequest, "Asset metadata omits a saved shared consumer"));
        }
    }
    // Even a metadata-free scene edit cannot invent a new shared consumer or use a stale pin.
    for source in &plan.source_revisions {
        let document = inventory.document(store, source)?;
        let Some(project) = document.defold.as_ref().and_then(|design| design.project.as_ref()) else { continue };
        for binding in project.layers.values() {
            let Some(AppearanceSource::Shared { source: reference }) = &binding.appearance else { continue };
            let expected = SourceRevisionRef { package_id: reference.package_id.clone(), source_revision: reference.source_revision.clone() };
            let (canonical, node, references) = if let Some(update) = plan.asset_changes.iter().find(|update| update.asset_id == reference.asset_id) {
                (update.source.as_ref(), update.source_node.as_ref(), update.referencing_scenes.iter().collect::<BTreeSet<_>>())
            } else if let Some(asset) = registry.assets.get(&reference.asset_id) {
                (asset.source.as_ref(), asset.source_node.as_ref(), asset.referencing_scenes.iter().collect())
            } else {
                return Err(fail(ProjectErrorCode::ResourceMissing, "Shared consumer has no canonical managed asset"));
            };
            if canonical != Some(&expected)
                || node != Some(&reference.source_node)
                || !references.contains(&project.scene_id)
                || binding.asset_id.as_ref() != Some(&reference.asset_id)
                || !plan.source_revisions.contains(&expected)
            {
                return Err(fail(ProjectErrorCode::SourceConflict, "Shared consumer differs from the canonical publication and its reference inventory"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "defold_project_shared_store_tests.rs"]
mod tests;
