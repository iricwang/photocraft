//! Durable host journal. The adapter is the single executor for game resource mutations.
//! The source store calls these functions while holding its authority mutex.
use crate::{
    defold_project_editor as editor, defold_project_fs as disk,
    defold_project_model::*,
    defold_publish_plan::{AssetRecord, AssetRegistry},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const CATALOG_LIMIT: u64 = 64 * 1024 * 1024;
fn error(code: ProjectErrorCode, text: impl Into<String>) -> ProjectError {
    ProjectError::new(code, text)
}
fn io(error: impl std::fmt::Display) -> ProjectError {
    ProjectError::new(ProjectErrorCode::Io, error.to_string())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    schema_version: u32,
    plan: PublishPlan,
    receipt: PublishReceipt,
    before: Vec<ResourceVersion>,
    adapter_receipt: Option<serde_json::Value>,
    // Older journals did not record the send boundary. Never infer that they were unapplied.
    #[serde(default = "unknown_apply_intent")]
    apply_requested: bool,
}
fn unknown_apply_intent() -> bool {
    true
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Catalog {
    schema_version: u32,
    registry: AssetRegistry,
    latest_by_scene: BTreeMap<SceneId, TransactionId>,
    receipts: BTreeMap<TransactionId, PublishReceipt>,
}
impl Default for Catalog {
    fn default() -> Self {
        Self { schema_version: SCHEMA_VERSION, registry: AssetRegistry::default(), latest_by_scene: BTreeMap::new(), receipts: BTreeMap::new() }
    }
}
fn catalog(root: &Path) -> ProjectResult<Catalog> {
    let path = disk::scoped_path(root, ".photocraft/assets/index.json", false)?;
    if !path.is_file() {
        return Ok(Catalog::default());
    }
    let catalog: Catalog = disk::read_json(&path, CATALOG_LIMIT)?;
    check_schema(catalog.schema_version)?;
    check_schema(catalog.registry.schema_version)?;
    Ok(catalog)
}
pub(crate) fn registry(root: &Path) -> ProjectResult<AssetRegistry> {
    Ok(catalog(root)?.registry)
}
pub(crate) fn published_baseline(root: &Path, request: &SceneRequest) -> ProjectResult<Option<SceneBaseline>> {
    let catalog = catalog(root)?;
    let Some(transaction) = catalog.latest_by_scene.get(&request.scene_id) else { return Ok(None) };
    let receipt = catalog.receipts.get(transaction).ok_or_else(|| error(ProjectErrorCode::RecoveryRequired, "Published baseline receipt is missing"))?;
    if receipt.phase != PublishPhase::Committed || receipt.context.project_id != request.context.project_id {
        return Err(error(ProjectErrorCode::ProjectMismatch, "Published baseline does not belong to this project"));
    }
    let target = receipt
        .targets
        .iter()
        .find(|target| target.scene_id == request.scene_id && target.path == request.path)
        .ok_or_else(|| error(ProjectErrorCode::TargetChanged, "Published baseline belongs to another GUI path"))?;
    let version = receipt
        .files
        .iter()
        .find(|file| file.path == target.path && file.content.hash == target.disk_hash)
        .ok_or_else(|| error(ProjectErrorCode::RecoveryRequired, "Published GUI has no archived version descriptor"))?;
    let path = disk::scoped_path(
        root,
        &format!(".photocraft/publications/{}/files/{}", receipt.transaction_id.as_str(), version.content.hash.as_str().replace(':', "-")),
        false,
    )?;
    if !path.is_file() {
        return Err(error(ProjectErrorCode::ResourceMissing, "Published GUI archive is missing; the current disk file cannot replace its baseline"));
    }
    let bytes = disk::read_bounded(&path, version.content.length)?;
    version.content.verify(&bytes)?;
    let gui_source = String::from_utf8(bytes).map_err(|_| error(ProjectErrorCode::HashMismatch, "Published GUI archive is not UTF-8"))?;
    Ok(Some(SceneBaseline {
        scene_id: target.scene_id.clone(),
        path: target.path.clone(),
        gui_source,
        disk_hash: target.disk_hash.clone(),
        scene_hash: target.scene_hash.clone(),
        published_revision: Some(target.published_revision.clone()),
    }))
}
#[cfg(test)]
pub(crate) fn latest(root: &Path, scene: &SceneId) -> ProjectResult<Option<PublishReceipt>> {
    let catalog = catalog(root)?;
    Ok(catalog.latest_by_scene.get(scene).and_then(|id| catalog.receipts.get(id)).cloned())
}
pub(crate) fn latest_for_context(root: &Path, scene: &SceneId, context: &ProjectContext) -> ProjectResult<Option<PublishReceipt>> {
    let committed = catalog(root)?;
    let directory = root.join(".photocraft/publications");
    let mut pending = None;
    if directory.exists() {
        for (index, entry) in std::fs::read_dir(directory).map_err(io)?.enumerate() {
            if index >= 10_000 {
                return Err(error(ProjectErrorCode::LimitExceeded, "Publication history exceeds lookup limit"));
            }
            let entry = entry.map_err(io)?;
            if !entry.file_type().map_err(io)?.is_dir() {
                continue;
            }
            let Ok(id) = TransactionId::parse(&entry.file_name().to_string_lossy()) else { continue };
            if committed.receipts.contains_key(&id) {
                continue;
            }
            if let Some(record) = read_record(root, &id)?
                && record.plan.context == *context
                && !record.receipt.phase.is_terminal()
                && record.plan.targets.iter().any(|target| target.scene_id == *scene)
            {
                if pending.is_some() {
                    return Err(error(
                        ProjectErrorCode::RecoveryRequired,
                        "Several pending transactions claim this scene; inspect the project recovery journal",
                    ));
                }
                pending = Some(record.receipt);
            }
        }
    }
    Ok(pending.or_else(|| committed.latest_by_scene.get(scene).and_then(|id| committed.receipts.get(id)).cloned()))
}
fn record_path(root: &Path, id: &TransactionId, create: bool) -> ProjectResult<PathBuf> {
    disk::scoped_path(root, &format!(".photocraft/publications/{}/record.json", id.as_str()), create)
}
fn read_record(root: &Path, id: &TransactionId) -> ProjectResult<Option<Record>> {
    let path = record_path(root, id, false)?;
    if !path.is_file() {
        return Ok(None);
    }
    let record: Record = disk::read_json(&path, CATALOG_LIMIT)?;
    check_schema(record.schema_version)?;
    if &record.plan.transaction_id != id || record.receipt.transaction_id != *id || record.receipt.plan_hash != record.plan.plan_hash {
        return Err(error(ProjectErrorCode::HashMismatch, "Publication journal identity changed"));
    }
    Ok(Some(record))
}
fn persist(root: &Path, record: &Record) -> ProjectResult<()> {
    disk::write_json(&record_path(root, &record.plan.transaction_id, true)?, record)
}
fn archived_path(root: &Path, transaction: &TransactionId, hash: &ContentHash, stage: bool) -> ProjectResult<PathBuf> {
    let folder = if stage { ".internal/photocraft/transactions" } else { ".photocraft/publications" };
    disk::scoped_path(root, &format!("{folder}/{}/files/{}", transaction.as_str(), hash.as_str().replace(':', "-")), true)
}
fn archive_bytes(root: &Path, transaction: &TransactionId, content: &BlobDescriptor, bytes: &[u8], stage: bool) -> ProjectResult<()> {
    content.verify(bytes)?;
    let path = archived_path(root, transaction, &content.hash, stage)?;
    if path.is_file() {
        content.verify(&disk::read_bounded(&path, content.length)?)?;
    } else {
        disk::write_atomic(&path, bytes)?;
    }
    content.verify(&disk::read_bounded(&path, content.length)?)
}

pub(crate) fn resources_locked(root: &Path, resources: &[LeaseResource], except: Option<&TransactionId>) -> ProjectResult<bool> {
    let directory = root.join(".photocraft/publications");
    if !directory.exists() {
        return Ok(false);
    }
    let committed = catalog(root)?.receipts;
    let mut count = 0usize;
    for entry in std::fs::read_dir(directory).map_err(io)? {
        count += 1;
        if count > 10_000 {
            return Err(error(ProjectErrorCode::LimitExceeded, "Publication history needs an indexed archive before more writes"));
        }
        let entry = entry.map_err(io)?;
        if !entry.file_type().map_err(io)?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
        let Ok(id) = TransactionId::parse(&name) else { continue };
        if except == Some(&id) || committed.contains_key(&id) {
            continue;
        }
        if let Some(record) = read_record(root, &id)?
            && !record.receipt.phase.is_terminal()
            && record.plan.leases.iter().any(|lease| resources.contains(&lease.resource))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn empty_receipt(plan: &PublishPlan) -> PublishReceipt {
    PublishReceipt {
        schema_version: SCHEMA_VERSION,
        transaction_id: plan.transaction_id.clone(),
        context: plan.context.clone(),
        phase: PublishPhase::Preparing,
        plan_hash: plan.plan_hash.clone(),
        source_revisions: plan.source_revisions.clone(),
        targets: vec![],
        files: vec![],
        retained: vec![],
        error: None,
    }
}
fn validate_file_boundary(file: &PlannedFile, plan: &PublishPlan, catalog: &Catalog) -> ProjectResult<()> {
    for path in file.old_path.iter().chain(file.new_path.iter()) {
        let is_target = plan.targets.iter().any(|target| target.path == *path);
        if !(is_target && path.as_str().ends_with(".gui"))
            && !(path.as_str().starts_with("/photocraft_generated/") && (path.as_str().ends_with(".png") || path.as_str().ends_with(".atlas")))
        {
            return Err(error(ProjectErrorCode::PathInvalid, "Publication may only update target GUIs and explicitly managed PNG/atlas resources"));
        }
    }
    if file.old_path.iter().chain(file.new_path.iter()).any(managed_png) {
        return validate_image_boundary(file, plan, &catalog.registry);
    }
    if let Some(asset) = &file.asset_id
        && file
            .old_path
            .iter()
            .chain(file.new_path.iter())
            .any(|path| catalog.registry.path_owners.iter().any(|(owned, owner)| owned.case_key() == path.case_key() && owner != asset))
    {
        return Err(error(ProjectErrorCode::TargetChanged, "Existing output belongs to another stable asset"));
    }
    if let Some(old) = &file.old_path
        && !plan.targets.iter().any(|target| target.path == *old)
        && catalog.registry.managed_files.get(old).is_none_or(|content| Some(&content.hash) != file.expected_old_hash.as_ref())
        && !catalog
            .registry
            .assets
            .values()
            .any(|asset| asset.ownership == AssetOwnership::Managed && asset.image == *old && Some(&asset.output.hash) == file.expected_old_hash.as_ref())
    {
        return Err(error(ProjectErrorCode::TargetChanged, "Existing output is not owned at the declared version"));
    }
    Ok(())
}

fn managed_png(path: &ResourcePath) -> bool {
    path.as_str().starts_with("/photocraft_generated/") && path.as_str().ends_with(".png")
}

/// The host must establish ownership independently of the client's pure planner. Historical
/// filenames are reusable only when both their owner and their exact published bytes are known.
fn validate_image_boundary(file: &PlannedFile, plan: &PublishPlan, registry: &AssetRegistry) -> ProjectResult<()> {
    let asset_id = file.asset_id.as_ref().ok_or_else(|| error(ProjectErrorCode::InvalidRequest, "Managed PNG actions require a stable asset ID"))?;
    let change = plan
        .asset_changes
        .iter()
        .find(|change| change.asset_id == *asset_id)
        .ok_or_else(|| error(ProjectErrorCode::InvalidRequest, "Managed PNG action has no matching asset update"))?;
    if change.ownership != AssetOwnership::Managed {
        return Err(error(ProjectErrorCode::InvalidRequest, "Managed PNG actions require a managed asset update"));
    }
    if !plan.leases.iter().any(|lease| lease.resource == LeaseResource::Asset(asset_id.clone())) {
        return Err(error(ProjectErrorCode::LeaseLost, "Managed PNG action requires its asset lease"));
    }
    let previous = registry.assets.get(asset_id);
    if previous.is_some_and(|asset| asset.ownership != AssetOwnership::Managed) {
        return Err(error(ProjectErrorCode::TargetChanged, "External assets cannot be claimed by a publication"));
    }
    for scene in previous.into_iter().flat_map(|asset| &asset.referencing_scenes).chain(&change.referencing_scenes) {
        if !plan.targets.iter().any(|target| target.scene_id == *scene) {
            return Err(error(ProjectErrorCode::InvalidRequest, "Managed PNG action omits an affected scene"));
        }
        if !plan.leases.iter().any(|lease| lease.resource == LeaseResource::Scene(scene.clone())) {
            return Err(error(ProjectErrorCode::LeaseLost, "Managed PNG action requires every affected scene lease"));
        }
    }
    if let Some(destination) = &file.new_path
        && change.image.as_ref().is_none_or(|image| image.case_key() != destination.case_key())
    {
        return Err(error(ProjectErrorCode::InvalidRequest, "Managed PNG destination differs from its asset update"));
    }
    for path in file.old_path.iter().chain(file.new_path.iter()) {
        if !managed_png(path) {
            return Err(error(ProjectErrorCode::PathInvalid, "A managed PNG action cannot change resource type"));
        }
        let key = path.case_key();
        let owners: Vec<_> = registry.path_owners.iter().filter(|(owned, _)| owned.case_key() == key).map(|(_, owner)| owner).collect();
        if owners.iter().any(|owner| *owner != asset_id) || registry.assets.values().any(|asset| asset.image.case_key() == key && asset.asset_id != *asset_id) {
            return Err(error(ProjectErrorCode::TargetChanged, "Existing output belongs to another stable asset"));
        }
        let known_path = file.old_path.as_ref() == Some(path)
            || registry.managed_files.keys().any(|owned| owned.case_key() == key)
            || registry.assets.values().any(|asset| asset.image.case_key() == key);
        if known_path && (owners.is_empty() || previous.is_none()) {
            return Err(error(ProjectErrorCode::TargetChanged, "Existing PNG ownership is incomplete; review legacy asset ownership before publishing"));
        }
    }
    if let Some(old) = &file.old_path {
        let key = old.case_key();
        let versions: Vec<_> = registry.managed_files.iter().filter(|(owned, _)| owned.case_key() == key).map(|(_, content)| content).collect();
        if versions.is_empty() || versions.iter().any(|content| Some(&content.hash) != file.expected_old_hash.as_ref()) {
            return Err(error(ProjectErrorCode::TargetChanged, "Existing PNG is not owned at the declared version"));
        }
    }
    Ok(())
}

fn validate_asset_updates(plan: &PublishPlan, registry: &AssetRegistry) -> ProjectResult<()> {
    for update in &plan.asset_changes {
        let previous = registry.assets.get(&update.asset_id);
        if !plan.leases.iter().any(|lease| lease.resource == LeaseResource::Asset(update.asset_id.clone())) {
            return Err(error(ProjectErrorCode::LeaseLost, "Asset update requires its asset lease"));
        }
        for scene in previous.into_iter().flat_map(|asset| &asset.referencing_scenes).chain(&update.referencing_scenes) {
            if !plan.targets.iter().any(|target| target.scene_id == *scene) {
                return Err(error(ProjectErrorCode::InvalidRequest, "Asset update omits an affected scene"));
            }
        }
        let writes: Vec<_> = plan
            .planned_files
            .iter()
            .filter(|file| file.asset_id.as_ref() == Some(&update.asset_id) && file.new_path.as_ref().is_some_and(managed_png))
            .collect();
        if let Some(previous) = previous {
            if update.ownership != previous.ownership || update.runtime_frame_id.as_ref().is_some_and(|frame| frame != &previous.runtime_frame_id) {
                return Err(error(ProjectErrorCode::TargetChanged, "Publication cannot change an existing asset's ownership or runtime frame identity"));
            }
            if update.image.as_ref().is_some_and(|image| image != &previous.image)
                && (writes.len() != 1
                    || !plan.planned_files.iter().any(|file| {
                        file.asset_id.as_ref() == Some(&update.asset_id)
                            && file.old_path.as_ref() == Some(&previous.image)
                            && file.expected_old_hash.as_ref() == Some(&previous.output.hash)
                    }))
            {
                return Err(error(ProjectErrorCode::InvalidRequest, "Asset relocation requires verified destination and previous current image actions"));
            }
        } else if writes.len() != 1
            || update.ownership != AssetOwnership::Managed
            || update.runtime_frame_id.as_deref() != Some(crate::defold_publish_plan::runtime_frame_id(&update.asset_id).as_str())
        {
            return Err(error(ProjectErrorCode::InvalidRequest, "New assets require one managed PNG output and their stable runtime frame identity"));
        }
        if writes.iter().any(|file| file.new_path != update.image) {
            return Err(error(ProjectErrorCode::InvalidRequest, "Asset output action does not match its declared image"));
        }
    }
    Ok(())
}

fn scene_atlas(scene: &SceneId) -> String {
    format!("/photocraft_generated/{scene}/scene.atlas")
}
fn scene_alias(scene: &SceneId) -> String {
    format!("pc_{}", scene.as_str().replace('-', ""))
}

/// Atlas identity belongs to its GUI; frames belong to stable assets. Neither an alias nor a
/// generated-directory path is sufficient evidence to adopt arbitrary existing files.
fn validate_atlas_updates(
    plan: &PublishPlan,
    payload: &PublishPayload,
    registry: &AssetRegistry,
    mut read: impl FnMut(&ResourcePath, &BlobDescriptor) -> ProjectResult<Vec<u8>>,
) -> ProjectResult<()> {
    let mut declared = BTreeMap::new();
    for update in &plan.node_changes {
        for texture in &update.textures {
            let managed = texture.texture.as_str().starts_with("/photocraft_generated/");
            if !managed && texture.name != scene_alias(&update.scene_id) {
                continue;
            }
            if texture.name != scene_alias(&update.scene_id) || texture.texture.as_str() != scene_atlas(&update.scene_id) {
                return Err(error(ProjectErrorCode::InvalidRequest, "Managed atlas alias and path must belong to the updated scene"));
            }
            if declared.insert(texture.texture.clone(), (&update.scene_id, texture.name.as_str())).is_some() {
                return Err(error(ProjectErrorCode::InvalidRequest, "Managed atlas has duplicate declarations"));
            }
        }
        let expected_alias = scene_alias(&update.scene_id);
        for node in &update.nodes {
            if let Some((alias, _)) = node.values.get("texture").and_then(serde_json::Value::as_str).and_then(|texture| texture.split_once('/'))
                && alias.strip_prefix("pc_").is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
                && (alias != expected_alias || !update.textures.iter().any(|texture| texture.name == alias))
            {
                return Err(error(ProjectErrorCode::InvalidRequest, "Managed GUI texture has no matching scene atlas declaration"));
            }
        }
    }
    for file in &plan.planned_files {
        let is_atlas = file.old_path.iter().chain(file.new_path.iter()).any(|path| path.as_str().ends_with(".atlas"));
        if !is_atlas {
            continue;
        }
        if file.asset_id.is_some()
            || file.writer != FileWriter::EditorResource
            || !matches!(file.action, FileAction::Create | FileAction::Replace)
            || file.new_path.as_ref().is_none_or(|path| !declared.contains_key(path))
        {
            return Err(error(ProjectErrorCode::InvalidRequest, "Managed atlas writes require their scene declaration and cannot claim an asset identity"));
        }
        if let Some(old) = &file.old_path
            && registry.managed_files.get(old).is_none_or(|content| Some(&content.hash) != file.expected_old_hash.as_ref())
        {
            return Err(error(ProjectErrorCode::TargetChanged, "Existing atlas lacks exact managed-version evidence"));
        }
    }
    for (path, (scene, alias)) in declared {
        let proposed = plan.planned_files.iter().find(|file| file.new_path.as_ref() == Some(&path));
        let bytes = if let Some(file) = proposed {
            let descriptor = file.content.as_ref().ok_or_else(|| error(ProjectErrorCode::InvalidRequest, "Atlas write has no content descriptor"))?;
            let bytes = payload.contents.get(&descriptor.hash).ok_or_else(|| error(ProjectErrorCode::ResourceMissing, "Atlas bytes are missing"))?;
            descriptor.verify(bytes)?;
            bytes.clone()
        } else {
            let descriptor = registry
                .managed_files
                .get(&path)
                .ok_or_else(|| error(ProjectErrorCode::TargetChanged, "Atlas declaration cannot claim an unregistered resource"))?;
            let bytes = read(&path, descriptor)?;
            descriptor.verify(&bytes)?;
            bytes
        };
        let source = std::str::from_utf8(&bytes).map_err(|_| error(ProjectErrorCode::InvalidRequest, "Managed atlas must contain UTF-8 source"))?;
        let frames = crate::defold_atlas::decode(source)?;
        for frame in &frames {
            let output = plan.planned_files.iter().find(|file| file.new_path.as_ref() == Some(&frame.image) && file.asset_id.is_some());
            let owner = output.and_then(|file| file.asset_id.as_ref()).or_else(|| registry.path_owners.get(&frame.image));
            let owner = owner.ok_or_else(|| error(ProjectErrorCode::TargetChanged, "Atlas image has no explicit managed asset owner"))?;
            let previous = registry.assets.get(owner);
            let change = plan.asset_changes.iter().find(|change| change.asset_id == *owner);
            let ownership = change.map(|change| change.ownership).or_else(|| previous.map(|asset| asset.ownership));
            let image = change.and_then(|change| change.image.as_ref()).or_else(|| previous.map(|asset| &asset.image));
            let identity = change.and_then(|change| change.runtime_frame_id.as_deref()).or_else(|| previous.map(|asset| asset.runtime_frame_id.as_str()));
            if ownership != Some(AssetOwnership::Managed) || image != Some(&frame.image) || identity != Some(frame.id.as_str()) {
                return Err(error(ProjectErrorCode::TargetChanged, "Atlas frame differs from its managed asset identity or image"));
            }
            if output.is_none() {
                let descriptor = registry
                    .managed_files
                    .get(&frame.image)
                    .ok_or_else(|| error(ProjectErrorCode::TargetChanged, "Atlas image has no verified managed version"))?;
                descriptor.verify(&read(&frame.image, descriptor)?)?;
            }
        }
        if let Some(update) = plan.node_changes.iter().find(|update| update.scene_id == *scene) {
            let prefix = format!("{alias}/");
            for node in &update.nodes {
                if let Some(frame) = node.values.get("texture").and_then(serde_json::Value::as_str).and_then(|texture| texture.strip_prefix(&prefix)) {
                    let frame = frames
                        .iter()
                        .find(|known| known.id == frame)
                        .ok_or_else(|| error(ProjectErrorCode::ResourceMissing, "GUI node refers to an absent managed atlas frame"))?;
                    let owner = plan
                        .planned_files
                        .iter()
                        .find(|file| file.new_path.as_ref() == Some(&frame.image) && file.asset_id.is_some())
                        .and_then(|file| file.asset_id.as_ref())
                        .or_else(|| registry.path_owners.get(&frame.image))
                        .ok_or_else(|| error(ProjectErrorCode::TargetChanged, "Managed GUI frame has no asset owner"))?;
                    let referenced = plan
                        .asset_changes
                        .iter()
                        .find(|change| change.asset_id == *owner)
                        .map(|change| change.referencing_scenes.contains(scene))
                        .or_else(|| registry.assets.get(owner).map(|asset| asset.referencing_scenes.contains(scene)))
                        .unwrap_or(false);
                    if !referenced {
                        return Err(error(ProjectErrorCode::InvalidRequest, "Managed GUI frame is absent from the asset's scene references"));
                    }
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn prepare(root: &Path, plan: &PublishPlan, payload: &PublishPayload, limits: &ProjectLimits) -> ProjectResult<PublishReceipt> {
    plan.verify_payload(payload, limits)?;
    let current = catalog(root)?;
    if let Some(receipt) = current.receipts.get(&plan.transaction_id) {
        if receipt.plan_hash != plan.plan_hash {
            return Err(error(ProjectErrorCode::IdempotencyMismatch, "Transaction ID already belongs to another plan"));
        }
        return Ok(receipt.clone());
    }
    if let Some(record) = read_record(root, &plan.transaction_id)? {
        if record.plan.plan_hash != plan.plan_hash {
            return Err(error(ProjectErrorCode::IdempotencyMismatch, "Transaction ID already belongs to another plan"));
        }
        return Ok(record.receipt);
    }
    if resources_locked(root, &plan.leases.iter().map(|lease| lease.resource.clone()).collect::<Vec<_>>(), None)? {
        return Err(error(ProjectErrorCode::Busy, "Another publication owns a target or source package"));
    }
    for target in &plan.targets {
        let previous = current
            .latest_by_scene
            .get(&target.scene_id)
            .and_then(|id| current.receipts.get(id))
            .and_then(|receipt| receipt.targets.iter().find(|saved| saved.scene_id == target.scene_id));
        if previous.map(|saved| &saved.published_revision) != target.expected_published_revision.as_ref() {
            return Err(error(ProjectErrorCode::TargetChanged, "Published scene baseline changed"));
        }
    }
    for file in &plan.planned_files {
        validate_file_boundary(file, plan, &current)?;
    }
    validate_asset_updates(plan, &current.registry)?;
    validate_atlas_updates(plan, payload, &current.registry, |path, descriptor| {
        disk::read_bounded(&disk::scoped_path(root, path.as_str(), false)?, descriptor.length)
    })?;
    // Probe capabilities before storing a transaction lock. This has no editor graph effects.
    editor::describe(root)?;
    let mut record = Record {
        schema_version: SCHEMA_VERSION,
        plan: plan.clone(),
        receipt: empty_receipt(plan),
        before: vec![],
        adapter_receipt: None,
        apply_requested: false,
    };
    for file in &plan.planned_files {
        if let Some(old) = &file.old_path {
            let bytes = disk::read_bounded(&disk::scoped_path(root, old.as_str(), false)?, limits.max_dependency_bytes)?;
            let content = BlobDescriptor::of_bytes(&bytes);
            if Some(&content.hash) != file.expected_old_hash.as_ref() {
                return Err(error(ProjectErrorCode::TargetChanged, "Output changed before staging"));
            }
            archive_bytes(root, &plan.transaction_id, &content, &bytes, false)?;
            archive_bytes(root, &plan.transaction_id, &content, &bytes, true)?;
            record.before.push(ResourceVersion { path: old.clone(), content });
        }
        if let Some(new) = &file.new_path
            && file.old_path.as_ref().is_none_or(|old| old.case_key() != new.case_key())
            && disk::scoped_path(root, new.as_str(), false)?.exists()
        {
            return Err(error(ProjectErrorCode::NameCollision, "New output path already exists"));
        }
        if let Some(content) = &file.content {
            let bytes = payload.contents.get(&content.hash).ok_or_else(|| error(ProjectErrorCode::ResourceMissing, "Staged publication body is missing"))?;
            archive_bytes(root, &plan.transaction_id, content, bytes, false)?;
            archive_bytes(root, &plan.transaction_id, content, bytes, true)?;
        }
    }
    persist(root, &record)?;
    match editor::prepare(root, plan) {
        Ok(outcome) => consume(root, record, outcome, limits),
        Err(failure) if definitive_prepare_rejection(failure.code) => {
            record.receipt.phase = PublishPhase::Aborted;
            record.receipt.error = Some(failure);
            persist(root, &record)?;
            Ok(record.receipt)
        }
        Err(failure) => failed(root, record, failure),
    }
}
fn definitive_prepare_rejection(code: ProjectErrorCode) -> bool {
    matches!(
        code,
        ProjectErrorCode::TargetDirty
            | ProjectErrorCode::TargetChanged
            | ProjectErrorCode::InvalidRequest
            | ProjectErrorCode::InvalidTransform
            | ProjectErrorCode::UnsupportedGuiFeature
            | ProjectErrorCode::UndoBarrierRequired
            | ProjectErrorCode::NameCollision
            | ProjectErrorCode::Busy
            | ProjectErrorCode::PathInvalid
            | ProjectErrorCode::LeaseLost
            | ProjectErrorCode::ProjectMismatch
            | ProjectErrorCode::WorkspaceChanged
    )
}

fn failed(root: &Path, mut record: Record, mut failure: ProjectError) -> ProjectResult<PublishReceipt> {
    record.receipt.phase = PublishPhase::RecoveryRequired;
    failure.phase = Some(PublishPhase::RecoveryRequired);
    record.receipt.error = Some(failure);
    persist(root, &record)?;
    Ok(record.receipt)
}
pub(crate) fn commit(root: &Path, request: &PublishRequest, limits: &ProjectLimits) -> ProjectResult<PublishReceipt> {
    if let Some(receipt) = catalog(root)?.receipts.get(&request.transaction_id) {
        return Ok(receipt.clone());
    }
    let mut record = read_record(root, &request.transaction_id)?.ok_or_else(|| error(ProjectErrorCode::ResourceMissing, "Publication was not prepared"))?;
    if record.receipt.phase == PublishPhase::Aborted {
        return Ok(record.receipt);
    }
    if record.receipt.phase != PublishPhase::Prepared {
        return status(root, request, limits)?.ok_or_else(|| error(ProjectErrorCode::ResourceMissing, "Publication status disappeared"));
    }
    record.receipt.phase = PublishPhase::Applying;
    record.apply_requested = true;
    persist(root, &record)?;
    match editor::apply(root, &record.plan) {
        Ok(outcome) => consume(root, record, outcome, limits),
        Err(failure) => failed(root, record, failure),
    }
}
pub(crate) fn status(root: &Path, request: &PublishRequest, limits: &ProjectLimits) -> ProjectResult<Option<PublishReceipt>> {
    if let Some(receipt) = catalog(root)?.receipts.get(&request.transaction_id) {
        return Ok(Some(receipt.clone()));
    }
    let Some(record) = read_record(root, &request.transaction_id)? else { return Ok(None) };
    if record.receipt.phase.is_terminal() {
        return Ok(Some(record.receipt));
    }
    let outcome = editor::status(root, &record.plan);
    let plan = record.plan.clone();
    recover_status(root, record, outcome, limits, || editor::prepare(root, &plan)).map(Some)
}
fn recover_status(
    root: &Path,
    record: Record,
    outcome: ProjectResult<editor::EditorStatus>,
    limits: &ProjectLimits,
    prepare: impl FnOnce() -> ProjectResult<editor::EditorStatus>,
) -> ProjectResult<PublishReceipt> {
    match outcome {
        Ok(outcome) => consume(root, record, outcome, limits),
        Err(failure) if failure.code == ProjectErrorCode::ResourceMissing && !record.apply_requested => match prepare() {
            Ok(outcome) => consume(root, record, outcome, limits),
            Err(failure) if definitive_prepare_rejection(failure.code) => abort_unapplied(root, record, Some(failure)),
            Err(failure) => failed(root, record, failure),
        },
        Err(failure) => failed(root, record, failure),
    }
}
fn abort_unapplied(root: &Path, mut record: Record, failure: Option<ProjectError>) -> ProjectResult<PublishReceipt> {
    record.receipt.phase = PublishPhase::Aborted;
    record.receipt.error = failure;
    persist(root, &record)?;
    Ok(record.receipt)
}
pub(crate) fn cancel(root: &Path, request: &PublishRequest, limits: &ProjectLimits) -> ProjectResult<PublishReceipt> {
    if let Some(receipt) = catalog(root)?.receipts.get(&request.transaction_id) {
        return Ok(receipt.clone());
    }
    let record = read_record(root, &request.transaction_id)?.ok_or_else(|| error(ProjectErrorCode::ResourceMissing, "Publication does not exist"))?;
    if record.receipt.phase == PublishPhase::Aborted {
        return Ok(record.receipt);
    }
    if record.apply_requested && record.receipt.phase != PublishPhase::Prepared {
        return Err(error(ProjectErrorCode::RecoveryRequired, "Publication may have changed the editor; query its recoverable status"));
    }
    match editor::cancel(root, &record.plan) {
        Ok(outcome) => consume(root, record, outcome, limits),
        Err(failure) if failure.code == ProjectErrorCode::ResourceMissing && !record.apply_requested => abort_unapplied(root, record, Some(failure)),
        Err(failure) => failed(root, record, failure),
    }
}
pub(crate) fn saved_plan(root: &Path, transaction: &TransactionId) -> ProjectResult<Option<PublishPlan>> {
    Ok(read_record(root, transaction)?.map(|record| record.plan))
}
pub(crate) fn stored_receipt(root: &Path, transaction: &TransactionId) -> ProjectResult<Option<PublishReceipt>> {
    if let Some(receipt) = catalog(root)?.receipts.get(transaction) {
        return Ok(Some(receipt.clone()));
    }
    Ok(read_record(root, transaction)?.map(|record| record.receipt))
}

fn consume(root: &Path, mut record: Record, outcome: editor::EditorStatus, limits: &ProjectLimits) -> ProjectResult<PublishReceipt> {
    let phase = match outcome.state.as_str() {
        "PREPARING" => PublishPhase::Preparing,
        "PREPARED" => PublishPhase::Prepared,
        "APPLYING" => PublishPhase::Applying,
        "APPLIED_UNSAVED" => PublishPhase::AppliedUnsaved,
        "SAVING" => PublishPhase::Saving,
        "COMMITTED" => PublishPhase::Committed,
        "ABORTED" => PublishPhase::Aborted,
        _ => PublishPhase::RecoveryRequired,
    };
    if phase == PublishPhase::Committed {
        record.adapter_receipt = outcome.receipt;
        return match finish(root, &record, limits) {
            Ok(receipt) => Ok(receipt),
            Err(failure) => failed(root, record, failure),
        };
    }
    record.receipt.phase = phase;
    record.receipt.error = outcome.error;
    persist(root, &record)?;
    Ok(record.receipt)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditorReceipt {
    schema_version: u32,
    transaction_id: TransactionId,
    targets: Vec<EditorTarget>,
    resources: Vec<EditorResource>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditorTarget {
    path: ResourcePath,
    disk_hash: ContentHash,
    scene_hash: ContentHash,
    editor_graph_revision: Option<String>,
    editor_ahead: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditorResource {
    path: ResourcePath,
    disk_hash: ContentHash,
}

fn finish(root: &Path, record: &Record, limits: &ProjectLimits) -> ProjectResult<PublishReceipt> {
    let raw =
        record.adapter_receipt.clone().ok_or_else(|| error(ProjectErrorCode::SaveFailed, "Defold claimed completion without its saved-resource receipt"))?;
    let editor: EditorReceipt = serde_json::from_value(raw.clone()).map_err(|e| error(ProjectErrorCode::SaveFailed, e.to_string()))?;
    check_schema(editor.schema_version)?;
    if editor.transaction_id != record.plan.transaction_id || editor.targets.len() != record.plan.targets.len() {
        return Err(error(ProjectErrorCode::SaveFailed, "Defold receipt does not cover this transaction"));
    }
    let mut receipt = empty_receipt(&record.plan);
    receipt.phase = PublishPhase::Committed;
    let mut actual = BTreeMap::new();
    for target in &record.plan.targets {
        let saved = editor
            .targets
            .iter()
            .find(|saved| saved.path == target.path)
            .ok_or_else(|| error(ProjectErrorCode::SaveFailed, "A GUI target is missing from the saved receipt"))?;
        let bytes = disk::read_bounded(&disk::scoped_path(root, target.path.as_str(), false)?, limits.max_gui_source_bytes)?;
        if !saved.disk_hash.verify(&bytes) {
            return Err(error(ProjectErrorCode::TargetChanged, "Saved GUI changed before host acknowledgement"));
        }
        let content = BlobDescriptor::of_bytes(&bytes);
        archive_bytes(root, &record.plan.transaction_id, &content, &bytes, false)?;
        actual.insert(target.path.clone(), content);
        receipt.targets.push(PublishedTarget {
            scene_id: target.scene_id.clone(),
            path: target.path.clone(),
            published_revision: record.plan.transaction_id.as_str().into(),
            disk_hash: saved.disk_hash.clone(),
            scene_hash: saved.scene_hash.clone(),
            editor_graph_revision: saved.editor_graph_revision.clone().unwrap_or_else(|| saved.scene_hash.to_string()),
            editor_ahead: saved.editor_ahead,
        });
    }
    for file in &record.plan.planned_files {
        if let Some(path) = &file.new_path
            && !actual.contains_key(path)
        {
            let saved = editor
                .resources
                .iter()
                .find(|saved| saved.path == *path)
                .ok_or_else(|| error(ProjectErrorCode::SaveFailed, "An output is missing from the saved resource receipt"))?;
            let bytes = disk::read_bounded(&disk::scoped_path(root, path.as_str(), false)?, limits.max_dependency_bytes)?;
            if !saved.disk_hash.verify(&bytes) {
                return Err(error(ProjectErrorCode::TargetChanged, "Saved output changed before host acknowledgement"));
            }
            let content = BlobDescriptor::of_bytes(&bytes);
            archive_bytes(root, &record.plan.transaction_id, &content, &bytes, false)?;
            actual.insert(path.clone(), content);
        }
        if matches!(file.action, FileAction::Move | FileAction::Remove)
            && let Some(path) = &file.old_path
        {
            receipt.retained.push(RetainedResource {
                path: path.clone(),
                reason: "Previous path retained for editor history and explicit garbage collection; it is not another active asset".into(),
            });
        }
    }
    receipt.files = actual.iter().map(|(path, content)| ResourceVersion { path: path.clone(), content: content.clone() }).collect();
    let mut catalog = catalog(root)?;
    for update in &record.plan.asset_changes {
        let old = catalog.registry.assets.get(&update.asset_id);
        let path = update
            .image
            .clone()
            .or_else(|| old.map(|asset| asset.image.clone()))
            .ok_or_else(|| error(ProjectErrorCode::SaveFailed, "Asset update has no output location"))?;
        let output = actual
            .get(&path)
            .cloned()
            .or_else(|| old.map(|asset| asset.output.clone()))
            .ok_or_else(|| error(ProjectErrorCode::SaveFailed, "Asset update has no verified output bytes"))?;
        let frame = update
            .runtime_frame_id
            .clone()
            .or_else(|| old.map(|asset| asset.runtime_frame_id.clone()))
            .ok_or_else(|| error(ProjectErrorCode::SaveFailed, "Asset update has no runtime frame identity"))?;
        let asset = AssetRecord {
            asset_id: update.asset_id.clone(),
            display_name: update.display_name.clone(),
            ownership: update.ownership,
            visibility: update.visibility,
            state: update.state,
            source: update.source.clone(),
            source_node: update.source_node.clone(),
            image: path,
            output,
            runtime_frame_id: frame,
            referencing_scenes: update.referencing_scenes.iter().cloned().collect(),
            output_revision: record.plan.transaction_id.as_str().into(),
            recipe: update.recipe.clone().or_else(|| old.and_then(|asset| asset.recipe.clone())),
        };
        if asset.state != AssetState::Active {
            receipt.retained.push(RetainedResource {
                path: asset.image.clone(),
                reason: "Detached creative output is retained for source history, editor Undo and explicit collection".into(),
            });
        } else if asset.visibility == AssetVisibility::Public && old.is_some_and(|old| old.display_name != asset.display_name) {
            receipt.retained.push(RetainedResource {
                path: asset.image.clone(),
                reason: "Public asset keeps its compatible runtime path after a display-name edit".into(),
            });
        }
        catalog.registry.assets.insert(asset.asset_id.clone(), asset);
    }
    for file in &record.plan.planned_files {
        if let Some(path) = &file.new_path
            && path.as_str().starts_with("/photocraft_generated/")
            && let Some(content) = actual.get(path)
        {
            catalog.registry.managed_files.insert(path.clone(), content.clone());
        }
        if let Some(owner) = &file.asset_id {
            for path in file.old_path.iter().chain(file.new_path.iter()).filter(|path| path.as_str().starts_with("/photocraft_generated/")) {
                catalog.registry.path_owners.insert(path.clone(), owner.clone());
            }
            if let Some(path) = &file.old_path
                && let Some(previous) = record.before.iter().find(|previous| previous.path == *path)
            {
                catalog.registry.managed_files.entry(path.clone()).or_insert_with(|| previous.content.clone());
            }
        }
        // Retained old paths stay explicitly owned until a separately checked GC transaction.
    }
    catalog.registry.revision = record.plan.transaction_id.as_str().into();
    for target in &receipt.targets {
        catalog.latest_by_scene.insert(target.scene_id.clone(), receipt.transaction_id.clone());
    }
    catalog.receipts.insert(receipt.transaction_id.clone(), receipt.clone());
    // This atomic catalog pointer is the authority for both registry and acknowledged receipt.
    // All original and actual saved bytes are already durable before it can become visible.
    disk::write_json(&disk::scoped_path(root, &format!(".photocraft/publications/{}/adapter-receipt.json", receipt.transaction_id.as_str()), true)?, &raw)?;
    disk::write_json(&disk::scoped_path(root, ".photocraft/assets/index.json", true)?, &catalog)?;
    let mut completed = record.clone();
    completed.receipt = receipt.clone();
    let _ = persist(root, &completed); // The catalog remains authoritative after response/audit-write loss.
    Ok(receipt)
}

#[cfg(test)]
#[path = "defold_project_publication_tests.rs"]
mod tests;
