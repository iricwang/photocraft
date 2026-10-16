//! Frozen-input asset planning. No filesystem, network, editor or document mutation occurs here.
use crate::{
    defold_atlas::{self, AtlasFrame},
    defold_project_model::*,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetRecord {
    pub asset_id: AssetId,
    pub display_name: String,
    pub ownership: AssetOwnership,
    pub visibility: AssetVisibility,
    pub state: AssetState,
    pub source: Option<SourceRevisionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_node: Option<ArtworkSourceNode>,
    pub image: ResourcePath,
    pub output: BlobDescriptor,
    pub runtime_frame_id: String,
    pub referencing_scenes: BTreeSet<SceneId>,
    /// Source versions and published bytes are retained independently of the current PNG path.
    pub output_revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<ExportRecipe>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetRegistry {
    pub schema_version: u32,
    pub revision: String,
    pub assets: BTreeMap<AssetId, AssetRecord>,
    /// Explicit ownership for atlas and image files; location alone never grants overwrite permission.
    #[serde(default)]
    pub managed_files: BTreeMap<ResourcePath, BlobDescriptor>,
    /// Asset ownership survives a rename, independently from the asset's current image path.
    #[serde(default)]
    pub path_owners: BTreeMap<ResourcePath, AssetId>,
}
impl Default for AssetRegistry {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: "unpublished".into(),
            assets: BTreeMap::new(),
            managed_files: BTreeMap::new(),
            path_owners: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrozenAsset {
    pub asset_id: AssetId,
    pub display_name: String,
    pub source: SourceRevisionRef,
    pub source_node: Option<ArtworkSourceNode>,
    /// Full desired references, including consumers outside the current GUI.
    pub referencing_scenes: BTreeSet<SceneId>,
    pub output: BlobDescriptor,
    /// Omit for placement/property-only edits. Existing bytes can be read from the frozen resource set for a rename.
    pub bytes: Option<Vec<u8>>,
    /// Baked pixels must already be node-local, straight-alpha RGBA8 in sRGB.
    pub recipe: ExportRecipe,
    /// Consumers contribute references and atlas frames, never canonical bytes or names.
    pub reference_only: bool,
}

impl ExportRecipe {
    pub(crate) fn validate(&self) -> ProjectResult<()> {
        if self.cross_node_composition {
            return Err(ProjectError::new(
                ProjectErrorCode::UnsupportedGuiFeature,
                "Cross-node composition needs an explicit authored group; interactive GUI nodes cannot be flattened implicitly",
            ));
        }
        if self.width == 0
            || self.height == 0
            || self.width > 8192
            || self.height > 8192
            || u64::from(self.width) * u64::from(self.height) > 16 * 1024 * 1024
            || self.node_local_transform.iter().any(|v| !v.is_finite())
        {
            return Err(ProjectError::new(ProjectErrorCode::InvalidTransform, "Invalid baked-artwork size or transform"));
        }
        Ok(())
    }
}

pub struct AssetPlanInput {
    /// Stable per-scene managed output directory. Renaming a GUI does not change this directory.
    pub folder: ResourcePath,
    pub atlas: ResourcePath,
    pub output_revision: String,
    pub targets: BTreeSet<SceneId>,
    /// Only these complete native projections may detach absent references. The target set is
    /// the whole transaction quorum and must never be used as a per-scene pruning scope.
    pub reference_scope: BTreeSet<SceneId>,
    pub registry: AssetRegistry,
    pub desired: Vec<FrozenAsset>,
    /// Complete resource inventory for collision/precondition checks; unchanged pixels need no bytes.
    pub resources: BTreeMap<ResourcePath, BlobDescriptor>,
    pub bodies: BTreeMap<ContentHash, Vec<u8>>,
    /// Explicit runtime/history compatibility frames, never implicitly harvested by directory scan.
    pub protected_frames: Vec<AtlasFrame>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AssetPlan {
    pub registry: AssetRegistry,
    pub files: Vec<PlannedFile>,
    pub payload: PublishPayload,
    pub changes: Vec<AssetChange>,
    pub frames: Vec<AtlasFrame>,
    pub retained: Vec<RetainedResource>,
}

fn invalid(message: impl Into<String>) -> ProjectError {
    ProjectError::new(ProjectErrorCode::InvalidRequest, message)
}
fn managed_path(path: &ResourcePath) -> bool {
    path.as_str().starts_with("/photocraft_generated/")
}
fn in_folder(folder: &ResourcePath, path: &ResourcePath) -> bool {
    path.as_str().starts_with(&format!("{}/", folder.as_str()))
}
fn short_id(id: &AssetId) -> String {
    id.as_str().chars().filter(|c| *c != '-').take(12).collect()
}
pub fn runtime_frame_id(id: &AssetId) -> String {
    format!("pc_{}", id.as_str().replace('-', ""))
}

/// Names are display labels only. Neither node nor asset identity is derived from this function.
pub fn image_label(label: &str) -> String {
    let label = crate::defold_assets::name(label);
    let trimmed = label.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() { "Layer".into() } else { trimmed.into() }
}

fn resource_at<'a>(resources: &'a BTreeMap<ResourcePath, BlobDescriptor>, path: &ResourcePath) -> Option<(&'a ResourcePath, &'a BlobDescriptor)> {
    resources.iter().find(|(existing, _)| existing.case_key() == path.case_key())
}
fn path_owner<'a>(registry: &'a AssetRegistry, path: &ResourcePath) -> Option<&'a AssetId> {
    registry.path_owners.iter().find(|(existing, _)| existing.case_key() == path.case_key()).map(|(_, owner)| owner)
}
fn output_bytes(asset: &FrozenAsset, input: &AssetPlanInput) -> ProjectResult<Vec<u8>> {
    let bytes = asset
        .bytes
        .as_ref()
        .or_else(|| input.bodies.get(&asset.output.hash))
        .ok_or_else(|| ProjectError::new(ProjectErrorCode::ResourceMissing, "Changed or renamed artwork requires its verified frozen bytes"))?;
    asset.output.verify(bytes)?;
    Ok(bytes.clone())
}
fn png_dimensions(bytes: &[u8]) -> ProjectResult<(u32, u32)> {
    if bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n".as_slice()) || bytes.get(8..16) != Some(b"\0\0\0\rIHDR".as_slice()) {
        return Err(invalid("Native artwork exporter did not produce a PNG with an IHDR header"));
    }
    let read = |start: usize| -> ProjectResult<u32> {
        let array: [u8; 4] = bytes.get(start..start + 4).ok_or_else(|| invalid("Truncated PNG size"))?.try_into().map_err(|_| invalid("Invalid PNG size"))?;
        Ok(u32::from_be_bytes(array))
    };
    Ok((read(16)?, read(20)?))
}
fn add_file(files: &mut Vec<PlannedFile>, payload: &mut PublishPayload, file: PlannedFile, bytes: Vec<u8>) -> ProjectResult<()> {
    file.validate()?;
    let content = file.content.as_ref().ok_or_else(|| invalid("Expected content for planned output"))?;
    content.verify(&bytes)?;
    payload.contents.insert(content.hash.clone(), bytes);
    files.push(file);
    Ok(())
}
fn change(record: &AssetRecord) -> AssetChange {
    AssetChange {
        asset_id: record.asset_id.clone(),
        display_name: record.display_name.clone(),
        ownership: record.ownership,
        visibility: record.visibility,
        state: record.state,
        source: record.source.clone(),
        source_node: record.source_node.clone(),
        image: Some(record.image.clone()),
        runtime_frame_id: Some(record.runtime_frame_id.clone()),
        referencing_scenes: record.referencing_scenes.iter().cloned().collect(),
        recipe: record.recipe.clone(),
    }
}

/// Create/replace/rename only explicitly identified managed outputs. Deletion is a reference change;
/// physical collection is a separate reviewed plan with complete retention roots.
pub fn plan_assets(input: AssetPlanInput) -> ProjectResult<AssetPlan> {
    check_schema(input.registry.schema_version)?;
    if !managed_path(&input.folder) || !in_folder(&input.folder, &input.atlas) || !input.atlas.as_str().ends_with(".atlas") {
        return Err(ProjectError::new(ProjectErrorCode::PathInvalid, "Managed publication needs an owned folder and atlas path"));
    }
    if input.targets.is_empty() || input.output_revision.is_empty() || !input.reference_scope.is_subset(&input.targets) {
        return Err(invalid("Asset publication needs frozen target and output revisions"));
    }
    let mut keys = BTreeSet::new();
    let mut name_counts = BTreeMap::new();
    for desired in &input.desired {
        desired.recipe.validate()?;
        if !keys.insert(&desired.asset_id) || desired.referencing_scenes.is_empty() {
            return Err(invalid("Desired asset identity is duplicated or has no scene reference"));
        }
        *name_counts.entry(image_label(&desired.display_name).to_lowercase()).or_insert(0usize) += 1;
    }
    for (id, record) in &input.registry.assets {
        if id != &record.asset_id {
            return Err(invalid("Registry key does not match asset identity"));
        }
    }
    let mut output =
        AssetPlan { registry: input.registry.clone(), files: vec![], payload: PublishPayload::default(), changes: vec![], frames: vec![], retained: vec![] };
    let mut chosen = BTreeMap::<String, AssetId>::new();
    let mut ordered: Vec<_> = input.desired.iter().collect();
    ordered.sort_by(|a, b| a.asset_id.cmp(&b.asset_id));
    for desired in ordered {
        let previous = input.registry.assets.get(&desired.asset_id);
        if desired.reference_only
            && previous.is_none_or(|old| {
                old.source.as_ref() != Some(&desired.source)
                    || old.source_node != desired.source_node
                    || old.recipe.as_ref() != Some(&desired.recipe)
                    || old.output != desired.output
                    || old.display_name != desired.display_name
                    || desired.bytes.is_some()
            })
        {
            return Err(invalid("Shared consumers may only update references to a verified canonical asset"));
        }
        let label = image_label(&desired.display_name);
        let plain = ResourcePath::parse(&format!("{}/images/{label}.png", input.folder))?;
        let occupied = path_owner(&input.registry, &plain).is_some_and(|owner| owner != &desired.asset_id)
            || resource_at(&input.resources, &plain).is_some_and(|(path, _)| {
                previous.is_none_or(|old| old.image.case_key() != path.case_key()) && path_owner(&input.registry, path) != Some(&desired.asset_id)
            });
        let duplicate = name_counts.get(&label.to_lowercase()).copied().unwrap_or(0) > 1;
        let candidate =
            if occupied || duplicate { ResourcePath::parse(&format!("{}/images/{label}__{}.png", input.folder, short_id(&desired.asset_id)))? } else { plain };
        let mut path = if let Some(old) = previous {
            if desired.reference_only || old.ownership == AssetOwnership::External || old.visibility == AssetVisibility::Public {
                if old.image != candidate {
                    output.retained.push(RetainedResource {
                        path: old.image.clone(),
                        reason: "Public or external paths remain compatible; changing a display name does not authorize dynamic-reference migration".into(),
                    });
                }
                old.image.clone()
            } else if old.display_name == desired.display_name && !duplicate {
                old.image.clone()
            } else {
                candidate
            }
        } else {
            candidate
        };
        // Reuse the existing spelling on case-insensitive paths; do not create a second alias.
        if let Some((existing, _)) = resource_at(&input.resources, &path) {
            path = existing.clone();
        }
        if chosen.insert(path.case_key(), desired.asset_id.clone()).is_some() {
            return Err(ProjectError::new(ProjectErrorCode::NameCollision, "Managed output paths collide after stable naming"));
        }
        if path_owner(&input.registry, &path).is_some_and(|owner| owner != &desired.asset_id) {
            return Err(ProjectError::new(ProjectErrorCode::NameCollision, "Managed filename is reserved by another stable asset"));
        }
        if let Some((present_path, present)) = resource_at(&input.resources, &path) {
            if previous.is_none_or(|old| old.image.case_key() != present_path.case_key()) {
                if path_owner(&input.registry, present_path) != Some(&desired.asset_id) {
                    return Err(ProjectError::new(ProjectErrorCode::NameCollision, "Proposed managed filename belongs to another resource"));
                }
                if resource_at(&input.registry.managed_files, present_path).map(|(_, content)| content) != Some(present) {
                    return Err(ProjectError::new(ProjectErrorCode::TargetChanged, "Historical managed image changed outside the captured registry"));
                }
            } else if previous.is_some_and(|old| old.output != *present) {
                return Err(ProjectError::new(ProjectErrorCode::TargetChanged, "Managed image was changed outside the captured registry"));
            }
        }
        if let Some(old) = previous {
            let current = resource_at(&input.resources, &old.image)
                .ok_or_else(|| ProjectError::new(ProjectErrorCode::ResourceMissing, "Previously published image is missing"))?;
            if current.1 != &old.output {
                return Err(ProjectError::new(ProjectErrorCode::TargetChanged, "Previously published image changed externally"));
            }
            let changed = old.output != desired.output || old.image != path;
            if old.ownership == AssetOwnership::External && changed {
                return Err(invalid("Editing an external image requires a new managed asset identity"));
            }
            if changed && old.referencing_scenes.iter().chain(desired.referencing_scenes.iter()).any(|scene| !input.targets.contains(scene)) {
                return Err(invalid("Shared asset update is missing an affected scene target"));
            }
            if changed {
                if !managed_path(&old.image) || !in_folder(&input.folder, &path) {
                    return Err(ProjectError::new(ProjectErrorCode::PathInvalid, "Managed image leaves its declared output boundary"));
                }
                let destination = resource_at(&input.resources, &path).map(|(_, content)| content);
                let returning = old.image != path && destination.is_some();
                let file = PlannedFile {
                    writer: FileWriter::HostFile,
                    action: if old.image == path || returning { FileAction::Replace } else { FileAction::Move },
                    asset_id: Some(desired.asset_id.clone()),
                    old_path: Some(if returning { path.clone() } else { old.image.clone() }),
                    new_path: Some(path.clone()),
                    expected_old_hash: Some(if returning {
                        destination.map_or_else(|| old.output.hash.clone(), |content| content.hash.clone())
                    } else {
                        old.output.hash.clone()
                    }),
                    content: Some(desired.output.clone()),
                };
                add_file(&mut output.files, &mut output.payload, file, output_bytes(desired, &input)?)?;
                if returning {
                    let retained = PlannedFile {
                        writer: FileWriter::HostFile,
                        action: FileAction::Remove,
                        asset_id: Some(desired.asset_id.clone()),
                        old_path: Some(old.image.clone()),
                        new_path: None,
                        expected_old_hash: Some(old.output.hash.clone()),
                        content: None,
                    };
                    retained.validate()?;
                    output.files.push(retained);
                }
                if old.image != path {
                    output.retained.push(RetainedResource {
                        path: old.image.clone(),
                        reason: "Previous asset path retained for Undo and explicit collection; its stable owner can reuse it".into(),
                    });
                }
            }
        } else {
            if desired.referencing_scenes.iter().any(|scene| !input.targets.contains(scene)) {
                return Err(invalid("New asset has a reference outside publication targets"));
            }
            let file = PlannedFile {
                writer: FileWriter::HostFile,
                action: FileAction::Create,
                asset_id: Some(desired.asset_id.clone()),
                old_path: None,
                new_path: Some(path.clone()),
                expected_old_hash: None,
                content: Some(desired.output.clone()),
            };
            add_file(&mut output.files, &mut output.payload, file, output_bytes(desired, &input)?)?;
        }
        let record = AssetRecord {
            asset_id: desired.asset_id.clone(),
            display_name: desired.display_name.clone(),
            ownership: previous.map_or(AssetOwnership::Managed, |old| old.ownership),
            visibility: previous.map_or(AssetVisibility::Private, |old| old.visibility),
            state: AssetState::Active,
            source: Some(desired.source.clone()),
            source_node: desired.source_node.clone(),
            image: path,
            output: desired.output.clone(),
            runtime_frame_id: previous.map_or_else(|| runtime_frame_id(&desired.asset_id), |old| old.runtime_frame_id.clone()),
            referencing_scenes: desired.referencing_scenes.clone(),
            output_revision: previous
                .filter(|old| old.output == desired.output)
                .map_or_else(|| input.output_revision.clone(), |old| old.output_revision.clone()),
            recipe: Some(desired.recipe.clone()),
        };
        if previous != Some(&record) {
            output.changes.push(change(&record));
        }
        output.frames.push(AtlasFrame { id: record.runtime_frame_id.clone(), image: record.image.clone() });
        output.registry.assets.insert(record.asset_id.clone(), record);
    }
    // Desired is the complete set for targets. Assets used elsewhere lose only the target references.
    for record in output.registry.assets.values_mut().filter(|record| !keys.contains(&record.asset_id)) {
        let old_references = record.referencing_scenes.clone();
        record.referencing_scenes.retain(|scene| !input.reference_scope.contains(scene));
        if record.referencing_scenes != old_references {
            if record.referencing_scenes.is_empty() {
                record.state = AssetState::Detached;
            }
            output.changes.push(change(record));
            output.retained.push(RetainedResource {
                path: record.image.clone(),
                reason: "Detached output retained for source history, editor Undo, published recovery and explicit garbage collection".into(),
            });
        }
    }
    for frame in &input.protected_frames {
        if let Some(current) = output.frames.iter().find(|current| current.id == frame.id) {
            if current != frame {
                return Err(invalid("Protected runtime frame conflicts with desired artwork"));
            }
        } else {
            if resource_at(&input.resources, &frame.image).is_none() {
                return Err(ProjectError::new(ProjectErrorCode::ResourceMissing, "Protected atlas frame image is missing"));
            }
            output.frames.push(frame.clone());
            output
                .retained
                .push(RetainedResource { path: frame.image.clone(), reason: "Runtime or editor-history frame retained explicitly in the final atlas".into() });
        }
    }
    output.frames.sort_by(|a, b| a.id.cmp(&b.id));
    let atlas_bytes = defold_atlas::encode(&output.frames)?;
    let atlas_content = BlobDescriptor::of_bytes(&atlas_bytes);
    let previous_atlas = input.resources.get(&input.atlas);
    if let Some(previous) = previous_atlas
        && input.registry.managed_files.get(&input.atlas) != Some(previous)
    {
        return Err(ProjectError::new(ProjectErrorCode::TargetChanged, "Existing atlas is not owned at this exact version by the project asset registry"));
    }
    if previous_atlas != Some(&atlas_content) {
        let file = PlannedFile {
            writer: FileWriter::EditorResource,
            action: if previous_atlas.is_some() { FileAction::Replace } else { FileAction::Create },
            asset_id: None,
            old_path: previous_atlas.map(|_| input.atlas.clone()),
            new_path: Some(input.atlas.clone()),
            expected_old_hash: previous_atlas.map(|c| c.hash.clone()),
            content: Some(atlas_content),
        };
        add_file(&mut output.files, &mut output.payload, file, atlas_bytes)?;
    }
    for file in &output.files {
        // The adapter installs the new name while retaining the old path for protected history.
        // Keep that ownership explicit until a separate, evidence-backed collection finishes.
        if let (Some(path), Some(content)) = (&file.new_path, &file.content) {
            output.registry.managed_files.insert(path.clone(), content.clone());
        }
        if let Some(owner) = &file.asset_id {
            for path in file.old_path.iter().chain(file.new_path.iter()) {
                output.registry.path_owners.insert(path.clone(), owner.clone());
            }
            if let Some(path) = &file.old_path
                && let Some((_, content)) = resource_at(&input.resources, path)
            {
                output.registry.managed_files.entry(path.clone()).or_insert_with(|| content.clone());
            }
        }
    }
    output.registry.revision = input.output_revision;
    Ok(output)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RetentionReason {
    CurrentWorkspace,
    SourceVersion,
    Publication,
    SharedReference,
    EditorHistory,
    UnsavedEditor,
    Transaction,
    AiResult,
    Trash,
    PublicApi,
    ExternalOwner,
    UnknownRoots,
    ChangedOnDisk,
    NotTombstoned,
    GracePeriod,
    MissingBackup,
}

pub struct CollectionInput {
    pub asset: AssetRecord,
    pub roots: BTreeSet<RetentionReason>,
    pub roots_complete: bool,
    pub current: Option<BlobDescriptor>,
    pub grace_period_elapsed: bool,
    pub verified_backup: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionDecision {
    pub asset_id: AssetId,
    pub reasons: BTreeSet<RetentionReason>,
    pub removal: Option<PlannedFile>,
}

/// A read-only UI report. Incomplete editor/Git/Undo roots always retain, and a preview never
/// turns detachment into deletion or invents verified backups/grace-period evidence.
pub fn collection_preview(
    registry: &AssetRegistry,
    current: &BTreeMap<ResourcePath, BlobDescriptor>,
    roots: &BTreeMap<AssetId, BTreeSet<RetentionReason>>,
) -> Vec<CollectionDecision> {
    registry
        .assets
        .values()
        .filter(|asset| asset.state != AssetState::Active)
        .map(|asset| {
            plan_collection(CollectionInput {
                asset: asset.clone(),
                roots: roots.get(&asset.asset_id).cloned().unwrap_or_default(),
                roots_complete: false,
                current: current.get(&asset.image).cloned(),
                grace_period_elapsed: false,
                verified_backup: false,
            })
        })
        .collect()
}

/// Older names retained by a move are still tracked even though the active asset now points at
/// its new readable path. These cannot disappear into an unindexed directory scan.
pub fn retained_file_preview(registry: &AssetRegistry) -> Vec<RetainedResource> {
    registry.managed_files.keys().filter(|path| path.as_str().ends_with(".png") && !registry.assets.values().any(|asset| asset.image == **path))
        .map(|path| RetainedResource { path: path.clone(), reason: "Previous published path remains explicitly owned for compatibility, editor Undo and recovery; complete reference evidence is required before removal".into() }).collect()
}

/// Planning a delete is not permission to execute it. The host rechecks roots and the exact hash
/// under its transaction lock. GUI/atlas changes require a new publication, not post-commit GC.
pub fn plan_collection(input: CollectionInput) -> CollectionDecision {
    let mut reasons = input.roots;
    if !input.roots_complete {
        reasons.insert(RetentionReason::UnknownRoots);
    }
    if input.asset.ownership == AssetOwnership::External {
        reasons.insert(RetentionReason::ExternalOwner);
    }
    if input.asset.visibility == AssetVisibility::Public {
        reasons.insert(RetentionReason::PublicApi);
    }
    if input.asset.state != AssetState::Tombstoned {
        reasons.insert(RetentionReason::NotTombstoned);
    }
    if !input.asset.referencing_scenes.is_empty() {
        reasons.insert(RetentionReason::SharedReference);
    }
    if input.current.as_ref() != Some(&input.asset.output) {
        reasons.insert(RetentionReason::ChangedOnDisk);
    }
    if !input.grace_period_elapsed {
        reasons.insert(RetentionReason::GracePeriod);
    }
    if !input.verified_backup {
        reasons.insert(RetentionReason::MissingBackup);
    }
    if !managed_path(&input.asset.image) {
        reasons.insert(RetentionReason::ExternalOwner);
    }
    let removal = reasons.is_empty().then(|| PlannedFile {
        writer: FileWriter::HostFile,
        action: FileAction::Remove,
        asset_id: Some(input.asset.asset_id.clone()),
        old_path: Some(input.asset.image),
        new_path: None,
        expected_old_hash: Some(input.asset.output.hash),
        content: None,
    });
    CollectionDecision { asset_id: input.asset.asset_id, reasons, removal }
}

/// The caller captures these records from the authority before rendering. Leases for newly
/// discovered asset changes can be acquired from `required_resources`, then the plan is rehashed.
pub(crate) struct PublicationBridgeInput {
    pub context: ProjectContext,
    pub client_id: ClientId,
    pub transaction_id: TransactionId,
    pub source_revisions: Vec<SourceRevisionRef>,
    pub targets: Vec<PublishTarget>,
    pub leases: Vec<LeaseProof>,
    pub folder: ResourcePath,
    pub atlas: ResourcePath,
    pub registry: AssetRegistry,
    pub resources: BTreeMap<ResourcePath, ResourceContent>,
    pub protected_frames: Vec<AtlasFrame>,
}
pub(crate) struct PublicationBridgeOutput {
    pub plan: PublishPlan,
    pub payload: PublishPayload,
    pub registry: AssetRegistry,
    pub retained: Vec<RetainedResource>,
    pub required_resources: BTreeSet<LeaseResource>,
    /// Native authoring remains editable. GUI output bytes are in the publish payload only.
    pub document: photocraft_doc::Document,
}

pub(crate) struct PublicationSceneInput {
    pub native: crate::defold_design::PublicationPlan,
    pub target: PublishTarget,
    pub folder: ResourcePath,
    pub atlas: ResourcePath,
    pub protected_frames: Vec<AtlasFrame>,
}
pub(crate) struct PublicationBatchInput {
    pub context: ProjectContext,
    pub client_id: ClientId,
    pub transaction_id: TransactionId,
    pub source_revisions: Vec<SourceRevisionRef>,
    pub leases: Vec<LeaseProof>,
    pub registry: AssetRegistry,
    pub resources: BTreeMap<ResourcePath, ResourceContent>,
    pub scenes: Vec<PublicationSceneInput>,
}
pub(crate) struct PublicationBatchOutput {
    pub plan: PublishPlan,
    pub payload: PublishPayload,
    pub registry: AssetRegistry,
    pub retained: Vec<RetainedResource>,
    pub required_resources: BTreeSet<LeaseResource>,
    pub documents: BTreeMap<SceneId, photocraft_doc::Document>,
}

/// Plan the whole dependency closure. Owners render first; consumers only reference the owner's
/// resulting immutable PNG and recipe. Each per-GUI projection prunes only its own references.
pub(crate) fn bridge_publication_batch(input: PublicationBatchInput) -> ProjectResult<PublicationBatchOutput> {
    if input.scenes.is_empty() || input.scenes.len() > 64 {
        return Err(invalid("Shared publication needs between one and 64 frozen GUI targets"));
    }
    let mut remaining = BTreeMap::new();
    let mut packages = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut atlas_paths = BTreeSet::new();
    let mut targets = Vec::new();
    for scene in input.scenes {
        let project = scene
            .native
            .document
            .defold
            .as_ref()
            .and_then(|design| design.project.as_ref())
            .ok_or_else(|| invalid("Shared GUI has no native project attachment"))?;
        if project.project_id != input.context.project_id
            || project.scene_id != scene.target.scene_id
            || project.gui_path != scene.target.path.as_str()
            || !packages.insert(project.package_id.clone())
            || !paths.insert(scene.target.path.case_key())
            || !atlas_paths.insert(scene.atlas.case_key())
        {
            return Err(invalid("Shared publication has aliased scene, package, GUI or atlas identities"));
        }
        targets.push(scene.target.clone());
        if remaining.insert(project.scene_id.clone(), scene).is_some() {
            return Err(invalid("Shared publication repeats a GUI scene"));
        }
    }
    let primary_scene_id = targets.first().ok_or_else(|| invalid("Shared publication has no target"))?.scene_id.clone();
    let sources: BTreeMap<_, _> = input.source_revisions.iter().map(|source| (source.package_id.clone(), source.source_revision.clone())).collect();
    if sources.len() != input.source_revisions.len() || sources.keys().cloned().collect::<BTreeSet<_>>() != packages {
        return Err(invalid("Shared publication must freeze exactly one source version for every GUI package"));
    }
    let mut completed = BTreeSet::new();
    let mut registry = input.registry.clone();
    let mut resources = input.resources;
    let mut planned_files = Vec::new();
    let mut node_changes = Vec::new();
    let mut payload = PublishPayload::default();
    let mut retained = Vec::new();
    let mut documents = BTreeMap::new();
    while !remaining.is_empty() {
        let ready = remaining
            .iter()
            .find_map(|(scene_id, scene)| {
                let project = scene.native.document.defold.as_ref()?.project.as_ref()?;
                let ready = project.layers.values().all(|binding| match &binding.appearance {
                    Some(AppearanceSource::Shared { source }) => completed.contains(&source.package_id),
                    _ => true,
                });
                ready.then(|| scene_id.clone())
            })
            .ok_or_else(|| invalid("Shared artwork dependency is missing, self-referential or cyclic"))?;
        let scene = remaining.remove(&ready).ok_or_else(|| invalid("Shared GUI disappeared from frozen inputs"))?;
        let package = scene
            .native
            .document
            .defold
            .as_ref()
            .and_then(|design| design.project.as_ref())
            .ok_or_else(|| invalid("Shared GUI attachment is missing"))?
            .package_id
            .clone();
        let output = bridge_publication(
            scene.native,
            PublicationBridgeInput {
                context: input.context.clone(),
                client_id: input.client_id.clone(),
                transaction_id: input.transaction_id.clone(),
                source_revisions: input.source_revisions.clone(),
                targets: targets.clone(),
                leases: input.leases.clone(),
                folder: scene.folder,
                atlas: scene.atlas,
                registry,
                resources: resources.clone(),
                protected_frames: scene.protected_frames,
            },
        )?;
        for file in &output.plan.planned_files {
            if let (Some(path), Some(content)) = (&file.new_path, &file.content) {
                let bytes = output.payload.contents.get(&content.hash).ok_or_else(|| invalid("Planned shared output bytes are missing"))?.clone();
                resources.insert(path.clone(), ResourceContent { version: ResourceVersion { path: path.clone(), content: content.clone() }, bytes });
            }
        }
        for (hash, bytes) in output.payload.contents {
            if payload.contents.get(&hash).is_some_and(|previous| previous != &bytes) {
                return Err(ProjectError::new(ProjectErrorCode::HashMismatch, "Shared publication contains conflicting bytes for one hash"));
            }
            payload.contents.insert(hash, bytes);
        }
        registry = output.registry;
        retained.extend(output.retained);
        planned_files.extend(output.plan.planned_files);
        node_changes.extend(output.plan.node_changes);
        documents.insert(ready, output.document);
        completed.insert(package);
    }
    // Reconcile against the original authoritative registry after every complete scene projection.
    // Taking the last per-scene change would either lose references or retain deleted consumers.
    let asset_changes: Vec<_> = registry.assets.values().filter(|asset| input.registry.assets.get(&asset.asset_id) != Some(*asset)).map(change).collect();
    let mut written = BTreeSet::new();
    for file in &planned_files {
        let paths: BTreeSet<_> = file.old_path.iter().chain(file.new_path.iter()).map(ResourcePath::case_key).collect();
        if paths.into_iter().any(|path| !written.insert(path)) {
            return Err(invalid("Multiple shared GUI projections attempted to write one resource"));
        }
    }
    let required_resources = targets
        .iter()
        .map(|target| LeaseResource::Scene(target.scene_id.clone()))
        .chain(input.source_revisions.iter().map(|source| LeaseResource::Package(source.package_id.clone())))
        .chain(asset_changes.iter().map(|asset| LeaseResource::Asset(asset.asset_id.clone())))
        .collect();
    let mut plan = PublishPlan {
        schema_version: SCHEMA_VERSION,
        transaction_id: input.transaction_id,
        context: input.context,
        client_id: input.client_id,
        primary_scene_id,
        source_revisions: input.source_revisions,
        targets,
        leases: input.leases,
        plan_hash: ContentHash::sha256(b""),
        planned_files,
        node_changes,
        asset_changes,
    };
    plan.plan_hash = plan.computed_hash()?;
    Ok(PublicationBatchOutput { plan, payload, registry, retained, required_resources, documents })
}

/// A native artwork subtree can use every native compositing tool. Its pixels may not depend on
/// another independently interactive GUI node, because Defold would composite those nodes separately.
pub fn validate_composition(document: &photocraft_doc::Document) -> ProjectResult<()> {
    use photocraft_doc::{BlendMode, Layer, LayerContent};
    let Some(project) = document.defold.as_ref().and_then(|design| design.project.as_ref()) else { return Ok(()) };
    fn visit(layers: &[Layer], project: &ProjectLink, inside_artwork: bool) -> ProjectResult<()> {
        let has_gui = layers.iter().any(|layer| project.layers.get(&layer.id.0).is_some_and(|binding| binding.role == LayerRole::GuiNode));
        for layer in layers {
            let role = project.layers.get(&layer.id.0).map(|binding| binding.role);
            let artwork = inside_artwork || matches!(role, Some(LayerRole::ArtworkRoot | LayerRole::DesignOnly));
            if !artwork {
                let cross_node = role == Some(LayerRole::GuiNode) && (layer.clipped || !matches!(layer.blend, BlendMode::Normal | BlendMode::PassThrough));
                let cross_adjustment = has_gui && matches!(layer.content, LayerContent::Adjustment(_));
                if cross_node || cross_adjustment {
                    return Err(ProjectError::new(
                        ProjectErrorCode::UnsupportedGuiFeature,
                        format!(
                            "Layer '{}' depends on another GUI node while compositing; keep the effect inside one artwork container before publishing",
                            layer.name
                        ),
                    ));
                }
            }
            if let LayerContent::Group(group) = &layer.content {
                visit(&group.children, project, artwork)?;
            }
        }
        Ok(())
    }
    visit(&document.layers, project, false)
}

/// Convert the native pure renderer's result to a project transaction. The old per-GUI registry,
/// sidecar and hash-named atlas are not emitted. All writes remain a frozen plan until the host commits.
pub(crate) fn bridge_publication(mut native: crate::defold_design::PublicationPlan, input: PublicationBridgeInput) -> ProjectResult<PublicationBridgeOutput> {
    validate_composition(&native.document)?;
    let project =
        native.document.defold.as_ref().and_then(|d| d.project.as_ref()).ok_or_else(|| invalid("Native GUI has no stable project identities"))?.clone();
    if project.project_id != input.context.project_id {
        return Err(ProjectError::new(ProjectErrorCode::ProjectMismatch, "Native source belongs to another project"));
    }
    let source = input
        .source_revisions
        .iter()
        .find(|source| source.package_id == project.package_id)
        .ok_or_else(|| invalid("Native GUI source revision was not acknowledged"))?
        .clone();
    let target =
        input.targets.iter().find(|target| target.scene_id == project.scene_id).ok_or_else(|| invalid("Native GUI has no frozen publish target"))?.clone();
    let mut desired = BTreeMap::new();
    let mut node_assets = BTreeMap::new();
    for asset in &native.registry.assets {
        let link = native.links.iter().find(|link| link.node == asset.node).ok_or_else(|| invalid("Rendered asset has no GUI node binding"))?;
        let binding = project.layers.get(&link.layer).ok_or_else(|| invalid("Rendered GUI node has no stable layer binding"))?;
        let id = binding.asset_id.as_ref().ok_or_else(|| invalid("GUI node has no stable asset identity"))?.clone();
        if binding.role != LayerRole::GuiNode {
            return Err(invalid("Only GUI node artwork produces a runtime asset"));
        }
        if matches!(binding.appearance, Some(AppearanceSource::Shared { .. })) {
            // A proxy can be resampled or empty on this canvas. Neither its pixels nor its
            // display label can redefine the canonical asset owned by another source package.
            continue;
        }
        let bytes = native
            .files
            .iter()
            .find(|(path, _)| path == &asset.image)
            .map(|(_, bytes)| bytes.clone())
            .ok_or_else(|| invalid("Pure renderer did not include its declared PNG"))?;
        let output = BlobDescriptor::of_bytes(&bytes);
        let old = input.registry.assets.get(&id);
        let mut references = old.map(|record| record.referencing_scenes.clone()).unwrap_or_default();
        references.insert(project.scene_id.clone());
        let (width, height) = png_dimensions(&bytes)?;
        let recipe = native
            .recipes
            .get(&asset.node)
            .cloned()
            .or_else(|| old.filter(|record| record.output == output).and_then(|record| record.recipe.clone()))
            .ok_or_else(|| invalid("Canonical artwork needs a real node-local export recipe; render its native source again"))?;
        if recipe.width != width || recipe.height != height {
            return Err(invalid("Canonical export recipe does not describe its PNG"));
        }
        let source_node = ArtworkSourceNode {
            scene_id: project.scene_id.clone(),
            node_id: binding.node_id.clone().ok_or_else(|| invalid("Canonical GUI node has no stable identity"))?,
            root_layer_id: binding.layer_id.clone(),
        };
        if old.is_some_and(|record| {
            record.source.as_ref().is_some_and(|previous| previous.package_id != source.package_id)
                || record.source_node.as_ref().is_some_and(|previous| previous != &source_node)
        }) {
            return Err(invalid("A consumer cannot replace the canonical artwork owner; make an independent asset first"));
        }
        let frozen = FrozenAsset {
            asset_id: id.clone(),
            display_name: native.document.layer(photocraft_doc::LayerId(link.layer)).map(|layer| layer.name.clone()).unwrap_or_else(|| asset.name.clone()),
            source: source.clone(),
            source_node: Some(source_node),
            referencing_scenes: references,
            output,
            bytes: Some(bytes),
            recipe,
            reference_only: false,
        };
        if desired.insert(id.clone(), frozen).is_some() {
            return Err(invalid("An asset ID is claimed by multiple native artwork owners"));
        }
        node_assets.insert(asset.node.clone(), id);
    }
    for (local, binding) in &project.layers {
        let Some(AppearanceSource::Shared { source: reference }) = &binding.appearance else { continue };
        if binding.role != LayerRole::GuiNode || binding.asset_id.as_ref() != Some(&reference.asset_id) {
            return Err(invalid("Shared artwork must be attached to its identified GUI node"));
        }
        let node = native.links.iter().find(|link| link.layer == *local).ok_or_else(|| invalid("Shared GUI node has no native binding"))?.node.clone();
        let canonical = input.registry.assets.get(&reference.asset_id).ok_or_else(|| invalid("Shared artwork has no published canonical owner"))?;
        let expected = SourceRevisionRef { package_id: reference.package_id.clone(), source_revision: reference.source_revision.clone() };
        if canonical.source.as_ref() != Some(&expected)
            || canonical.source_node.as_ref() != Some(&reference.source_node)
            || !input.source_revisions.contains(&expected)
            || reference.package_id == project.package_id
        {
            return Err(invalid("Shared artwork pin differs from its frozen canonical owner"));
        }
        let recipe = canonical.recipe.clone().ok_or_else(|| invalid("Shared artwork has no canonical export recipe"))?;
        recipe.validate()?;
        crate::defold_design::apply_shared_recipe(&mut native, &node, &recipe).map_err(|error| invalid(error.to_string()))?;
        let mut references = canonical.referencing_scenes.clone();
        references.insert(project.scene_id.clone());
        let frozen = FrozenAsset {
            asset_id: canonical.asset_id.clone(),
            display_name: canonical.display_name.clone(),
            source: expected,
            source_node: canonical.source_node.clone(),
            referencing_scenes: references,
            output: canonical.output.clone(),
            bytes: None,
            recipe,
            reference_only: true,
        };
        if let Some(previous) = desired.insert(reference.asset_id.clone(), frozen.clone())
            && previous != frozen
        {
            return Err(invalid("Shared artwork has conflicting pins inside this GUI"));
        }
        node_assets.insert(node, reference.asset_id.clone());
    }
    let desired_frames: BTreeSet<_> = desired.keys().map(runtime_frame_id).collect();
    let inventory = input.resources.iter().map(|(path, resource)| (path.clone(), resource.version.content.clone())).collect();
    let bodies = input.resources.values().map(|resource| (resource.version.content.hash.clone(), resource.bytes.clone())).collect();
    let mut assets = plan_assets(AssetPlanInput {
        folder: input.folder,
        atlas: input.atlas.clone(),
        output_revision: input.transaction_id.as_str().into(),
        targets: input.targets.iter().map(|target| target.scene_id.clone()).collect(),
        reference_scope: BTreeSet::from([project.scene_id.clone()]),
        registry: input.registry,
        desired: desired.into_values().collect(),
        resources: inventory,
        bodies,
        protected_frames: input.protected_frames.into_iter().filter(|frame| !desired_frames.contains(&frame.id)).collect(),
    })?;
    let alias = format!("pc_{}", project.scene_id.as_str().replace('-', ""));
    for (node, id) in node_assets {
        let record = assets.registry.assets.get(&id).ok_or_else(|| invalid("Planned asset record is missing"))?;
        native.output = native.output.edit(&node, "texture", &serde_json::json!(format!("{alias}/{}", record.runtime_frame_id))).map_err(invalid)?;
    }
    let old_aliases: BTreeSet<_> =
        native.textures.iter().filter_map(|texture| texture.get("name").and_then(serde_json::Value::as_str)).chain(std::iter::once(alias.as_str())).collect();
    let mut raw = native.output.proto.source.clone();
    for field in native
        .output
        .proto
        .fields
        .iter()
        .rev()
        .filter(|field| field.name == "textures" && old_aliases.contains(native.output.proto.text(&field.children, "name").as_str()))
    {
        raw.replace_range(field.span.clone(), "");
    }
    let texture = GuiTexture { name: alias, texture: input.atlas };
    raw.push_str(&format!(
        "\ntextures {{ name: {} texture: {} }}\n",
        serde_json::to_string(&texture.name).map_err(|e| invalid(e.to_string()))?,
        serde_json::to_string(texture.texture.as_str()).map_err(|e| invalid(e.to_string()))?
    ));
    native.output = photocraft_defold_format::Scene::parse(raw).map_err(invalid)?;
    let nodes: Vec<GuiNodeData> =
        serde_json::from_value(crate::defold_design::live_nodes(&native.output).map_err(|e| invalid(e.to_string()))?).map_err(|e| invalid(e.to_string()))?;
    let renames: Vec<NodeRename> = native.renames.into_iter().map(serde_json::from_value).collect::<Result<_, _>>().map_err(|e| invalid(e.to_string()))?;
    let gui_bytes = native.output.proto.source.as_bytes().to_vec();
    let gui_content = BlobDescriptor::of_bytes(&gui_bytes);
    if gui_content.hash != target.expected_gui_disk_hash {
        add_file(
            &mut assets.files,
            &mut assets.payload,
            PlannedFile {
                writer: FileWriter::EditorResource,
                action: FileAction::Replace,
                asset_id: None,
                old_path: Some(target.path.clone()),
                new_path: Some(target.path.clone()),
                expected_old_hash: Some(target.expected_gui_disk_hash.clone()),
                content: Some(gui_content),
            },
            gui_bytes,
        )?;
    }
    let mut plan = PublishPlan {
        schema_version: SCHEMA_VERSION,
        transaction_id: input.transaction_id,
        context: input.context,
        client_id: input.client_id,
        primary_scene_id: project.scene_id.clone(),
        source_revisions: input.source_revisions,
        targets: input.targets,
        leases: input.leases,
        plan_hash: ContentHash::sha256(b""),
        planned_files: assets.files,
        node_changes: vec![SceneUpdate { scene_id: project.scene_id, nodes, textures: vec![texture], renames }],
        asset_changes: assets.changes,
    };
    plan.plan_hash = plan.computed_hash()?;
    let required_resources = plan
        .targets
        .iter()
        .map(|target| LeaseResource::Scene(target.scene_id.clone()))
        .chain(plan.source_revisions.iter().map(|source| LeaseResource::Package(source.package_id.clone())))
        .chain(plan.asset_changes.iter().map(|asset| LeaseResource::Asset(asset.asset_id.clone())))
        .collect();
    Ok(PublicationBridgeOutput {
        plan,
        payload: assets.payload,
        registry: assets.registry,
        retained: assets.retained,
        required_resources,
        document: native.document,
    })
}

#[cfg(test)]
#[path = "defold_publish_plan_tests.rs"]
mod tests;
