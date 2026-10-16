//! Non-destructive legacy discovery and native-source migration. No game output is deleted or claimed.
use crate::defold_project_model::*;
#[cfg(not(target_arch = "wasm32"))]
use crate::defold_project_store::ProjectStore;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LegacyKind {
    GuiNative,
    GuiSidecar,
    GuiRecovery,
    SharedNative,
    AssetRegistry,
    GeneratedOutput,
    ImageBackup,
    AppRecovery,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyEntry {
    pub path: String,
    pub kind: LegacyKind,
    pub content: BlobDescriptor,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationInventory {
    pub schema_version: u32,
    pub entries: Vec<LegacyEntry>,
    pub inventory_hash: ContentHash,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MigrationState {
    BackedUp,
    NeedsReview,
    SourceCommitted,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetMapping {
    pub asset_id: AssetId,
    pub gui_node_id: String,
    pub legacy_identity: String,
    /// A mapping does not grant ownership. Importing an old source cannot silently take over game files.
    pub ownership: AssetOwnership,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationReport {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub inventory_hash: ContentHash,
    pub state: MigrationState,
    pub source: Option<SourceSaveReceipt>,
    pub mappings: Vec<AssetMapping>,
    pub notes: Vec<String>,
}

fn error(code: ProjectErrorCode, message: impl Into<String>) -> ProjectError {
    ProjectError::new(code, message)
}
pub fn classify(path: &str) -> Option<LegacyKind> {
    let shared = path.strip_prefix("/.photocraft/sources/").is_some_and(|tail| !tail.contains('/') && !tail.starts_with('.') && tail.ends_with(".pcraft"));
    if !shared && path.split('/').any(|part| part.starts_with('.')) {
        return None;
    }
    if path.ends_with(".gui.pcraft") {
        Some(LegacyKind::GuiNative)
    } else if path.ends_with(".gui.photocraft-recovery.json") {
        Some(LegacyKind::GuiRecovery)
    } else if path.ends_with(".gui.photocraft.json") {
        Some(LegacyKind::GuiSidecar)
    } else if shared {
        Some(LegacyKind::SharedNative)
    } else if path.contains(".photocraft-backup-") && path.ends_with(".png") {
        Some(LegacyKind::ImageBackup)
    } else if path.starts_with("/photocraft_generated/") && path.ends_with("/assets.json") {
        Some(LegacyKind::AssetRegistry)
    } else if path.starts_with("/photocraft_generated/") && [".png", ".atlas", ".json"].iter().any(|extension| path.ends_with(extension)) {
        Some(LegacyKind::GeneratedOutput)
    } else {
        None
    }
}
fn stable<T: StableIdentity>(package: &PackageId, label: &str) -> T {
    let digest = blake3::hash(format!("photocraft-migration-v1:{}:{label}", package.as_str()).as_bytes());
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    T::from_bytes(bytes)
}

/// Preserve the complete native container and attach stable identities. No renderer or flattening.
pub fn attach_identities(
    document: &mut photocraft_doc::Document,
    context: &ProjectContext,
    scene: &SceneIdentity,
    baseline: &SceneBaseline,
) -> ProjectResult<Vec<AssetMapping>> {
    use photocraft_doc::{Layer, LayerContent, defold::ProjectLink};
    let design =
        document.defold.as_ref().ok_or_else(|| error(ProjectErrorCode::UnsupportedGuiFeature, "Legacy source is not a complete linked GUI document"))?;
    if let Some(project) = &design.project {
        if project.project_id != context.project_id || project.scene_id != scene.scene_id || project.package_id != scene.package_id {
            return Err(error(ProjectErrorCode::ProjectMismatch, "Native source already belongs to another project identity"));
        }
        return Ok(vec![]);
    }
    let nodes = design.nodes.clone();
    let carriers: BTreeMap<_, _> = nodes.iter().map(|node| (node.layer, node)).collect();
    if carriers.len() != nodes.len() {
        return Err(error(ProjectErrorCode::UnsupportedGuiFeature, "Legacy nodes share an ambiguous native carrier"));
    }
    let roots: BTreeSet<_> = nodes.iter().filter(|node| node.layer != node.artwork).map(|node| node.artwork).collect();
    let mut layers = BTreeMap::new();
    let mut mappings = Vec::new();
    let mut aliases = BTreeMap::new();
    fn walk(
        items: &[Layer],
        in_artwork: bool,
        carriers: &BTreeMap<u64, &photocraft_doc::defold::NodeLink>,
        roots: &BTreeSet<u64>,
        package: &PackageId,
        layers: &mut BTreeMap<u64, LayerBinding>,
        depth: usize,
    ) -> ProjectResult<()> {
        if depth > 128 {
            return Err(error(ProjectErrorCode::LimitExceeded, "Native layer hierarchy exceeds migration limit"));
        }
        for layer in items {
            let role = if carriers.contains_key(&layer.id.0) {
                LayerRole::GuiNode
            } else if roots.contains(&layer.id.0) {
                LayerRole::ArtworkRoot
            } else if in_artwork {
                LayerRole::ArtworkLayer
            } else {
                LayerRole::DesignOnly
            };
            let binding = LayerBinding {
                layer_id: stable(package, &format!("layer:{}", layer.id.0)),
                role,
                node_id: None,
                asset_id: None,
                gui_node_id: None,
                appearance: None,
                artwork_fingerprint: None,
            };
            if layers.insert(layer.id.0, binding).is_some() {
                return Err(error(ProjectErrorCode::InvalidRequest, "Legacy source has duplicate local layer IDs"));
            }
            if let LayerContent::Group(group) = &layer.content {
                walk(&group.children, in_artwork || role == LayerRole::ArtworkRoot, carriers, roots, package, layers, depth + 1)?;
            }
        }
        Ok(())
    }
    walk(&document.layers, false, &carriers, &roots, &scene.package_id, &mut layers, 0)?;
    let mut asset_ids = BTreeSet::new();
    for node in &nodes {
        let artwork = layers
            .get(&node.artwork)
            .ok_or_else(|| error(ProjectErrorCode::UnsupportedGuiFeature, "Legacy node artwork is missing; keep the full native source for review"))?
            .layer_id
            .clone();
        let binding = layers.get_mut(&node.layer).ok_or_else(|| error(ProjectErrorCode::UnsupportedGuiFeature, "Legacy node carrier is missing"))?;
        // Legacy asset IDs had per-GUI scope even when their spelling happened to be a UUID.
        // Only an existing verified ProjectLink can prove a globally reusable identity.
        let asset_id: AssetId = stable(&scene.package_id, &format!("asset:{}:{}", node.node, node.asset_id));
        if !asset_ids.insert(asset_id.clone()) {
            return Err(error(ProjectErrorCode::InvalidRequest, "Ambiguous legacy asset identity"));
        }
        binding.node_id = Some(stable(&scene.package_id, &format!("node:{}", node.node)));
        binding.asset_id = Some(asset_id.clone());
        binding.gui_node_id = Some(node.node.clone());
        binding.appearance = Some(if node.layer == node.artwork { AppearanceSource::SelfLayer } else { AppearanceSource::Subtree { root_layer_id: artwork } });
        aliases.insert(format!("legacy:{}:{}", node.node, node.asset_id), asset_id.as_str().into());
        mappings.push(AssetMapping { asset_id, gui_node_id: node.node.clone(), legacy_identity: node.asset_id.clone(), ownership: AssetOwnership::External });
    }
    if let Some(design) = &mut document.defold {
        design.project = Some(ProjectLink {
            schema_version: SCHEMA_VERSION,
            project_id: context.project_id.clone(),
            scene_id: scene.scene_id.clone(),
            package_id: scene.package_id.clone(),
            gui_path: scene.path.to_string(),
            imported_gui_source: Some(baseline.gui_source.clone()),
            imported_disk_hash: Some(baseline.disk_hash.to_string()),
            layers,
            legacy_aliases: aliases,
        });
    }
    Ok(mappings)
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use crate::defold_project_fs as disk;
    use std::path::{Path, PathBuf};
    fn io(error: impl std::fmt::Display) -> ProjectError {
        super::error(ProjectErrorCode::Io, error.to_string())
    }
    fn migration_file(root: &Path, operation: &OperationId, suffix: &str, create: bool) -> ProjectResult<PathBuf> {
        disk::scoped_path(root, &format!(".photocraft/migrations/{}/{suffix}", operation.as_str()), create)
    }
    /// Discovery reads only selected legacy forms. Current source revisions, VCS, build outputs and links are not traversed.
    pub fn discover(root: &Path, recovery: &BTreeMap<String, PathBuf>, limits: &ProjectLimits) -> ProjectResult<MigrationInventory> {
        let root = std::fs::canonicalize(root).map_err(io)?;
        let mut directories = vec![root.clone()];
        let mut entries = Vec::new();
        let mut visited = 0usize;
        let mut total = 0u64;
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(directory).map_err(io)? {
                visited += 1;
                if visited > 100_000 {
                    return Err(error(ProjectErrorCode::LimitExceeded, "Legacy inventory exceeds path limit"));
                }
                let entry = entry.map_err(io)?;
                let relative = format!("/{}", entry.path().strip_prefix(&root).map_err(io)?.to_string_lossy().replace('\\', "/"));
                let kind = entry.file_type().map_err(io)?;
                if kind.is_symlink() {
                    return Err(error(ProjectErrorCode::PathInvalid, format!("Inventory encountered a link: {relative}")));
                }
                let actual = std::fs::canonicalize(entry.path()).map_err(io)?;
                if !actual.starts_with(&root) {
                    return Err(error(ProjectErrorCode::PathInvalid, "Legacy inventory crosses the project boundary"));
                }
                if kind.is_dir() {
                    let name = entry.file_name();
                    if matches!(name.to_str(), Some(".git" | ".internal" | "build" | "target"))
                        || name.to_str().is_some_and(|name| name.starts_with('.')) && relative != "/.photocraft"
                        || relative.starts_with("/.photocraft/") && relative != "/.photocraft/sources"
                    {
                        continue;
                    }
                    if relative.starts_with("/.photocraft/sources/") {
                        continue;
                    }
                    directories.push(actual);
                } else if kind.is_file()
                    && let Some(kind) = classify(&relative)
                {
                    let bytes = disk::read_bounded(&actual, limits.max_native_bytes)?;
                    total = total.checked_add(bytes.len() as u64).ok_or_else(|| error(ProjectErrorCode::LimitExceeded, "Legacy inventory size overflow"))?;
                    if total > limits.max_total_source_bytes {
                        return Err(error(ProjectErrorCode::LimitExceeded, "Legacy inventory exceeds total byte budget; migrate smaller groups"));
                    }
                    entries.push(LegacyEntry { path: relative, kind, content: BlobDescriptor::of_bytes(&bytes) });
                }
            }
        }
        for (label, path) in recovery {
            if label.is_empty() || label.contains(['/', '\\', ':']) {
                return Err(error(ProjectErrorCode::PathInvalid, "Recovery label must be an opaque name"));
            }
            let bytes = disk::read_bounded(path, limits.max_native_bytes)?;
            total = total.checked_add(bytes.len() as u64).ok_or_else(|| error(ProjectErrorCode::LimitExceeded, "Legacy inventory size overflow"))?;
            if total > limits.max_total_source_bytes {
                return Err(error(ProjectErrorCode::LimitExceeded, "Legacy inventory exceeds total byte budget"));
            }
            entries.push(LegacyEntry { path: format!("recovery:{label}"), kind: LegacyKind::AppRecovery, content: BlobDescriptor::of_bytes(&bytes) });
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let inventory_hash = ContentHash::sha256(&canonical_json(&serde_json::to_value(&entries).map_err(io)?)?);
        Ok(MigrationInventory { schema_version: SCHEMA_VERSION, entries, inventory_hash })
    }
    pub fn backup(root: &Path, operation: &OperationId, inventory: &MigrationInventory, recovery: &BTreeMap<String, PathBuf>) -> ProjectResult<()> {
        check_schema(inventory.schema_version)?;
        let expected = ContentHash::sha256(&canonical_json(&serde_json::to_value(&inventory.entries).map_err(io)?)?);
        if expected != inventory.inventory_hash {
            return Err(error(ProjectErrorCode::HashMismatch, "Legacy inventory was changed"));
        }
        let manifest = migration_file(root, operation, "inventory.json", true)?;
        if manifest.exists() {
            let previous: MigrationInventory = disk::read_json(&manifest, 4 * 1024 * 1024)?;
            if previous != *inventory {
                return Err(error(ProjectErrorCode::IdempotencyMismatch, "Migration operation already names another inventory"));
            }
        }
        for entry in &inventory.entries {
            let source = if let Some(label) = entry.path.strip_prefix("recovery:") {
                recovery.get(label).cloned().ok_or_else(|| error(ProjectErrorCode::ResourceMissing, "Recovery inventory source is missing"))?
            } else {
                disk::scoped_path(root, ResourcePath::parse(&entry.path)?.as_str(), false)?
            };
            let bytes = disk::read_bounded(&source, entry.content.length)?;
            entry.content.verify(&bytes)?;
            let target = migration_file(root, operation, &format!("files/{}", entry.content.hash.as_str().replace(':', "-")), true)?;
            if target.exists() {
                entry.content.verify(&disk::read_bounded(&target, entry.content.length)?)?;
            } else {
                disk::write_atomic(&target, &bytes)?;
            }
            entry.content.verify(&disk::read_bounded(&target, entry.content.length)?)?;
        }
        disk::write_json(&manifest, inventory)
    }

    pub struct NativeMigrationRequest {
        pub context: ProjectContext,
        pub client_id: ClientId,
        pub operation_id: OperationId,
        pub scene: SceneIdentity,
        pub input: ResourcePath,
        pub inventory: MigrationInventory,
        pub recovery: BTreeMap<String, PathBuf>,
    }
    /// Import only a full native source whose recorded GUI baseline still matches disk. Ambiguous
    /// generations/JSON-only sources are backed up and reported for review instead of flattened.
    pub fn migrate_native(root: &Path, store: &dyn ProjectStore, request: &NativeMigrationRequest) -> ProjectResult<MigrationReport> {
        let report_path = migration_file(root, &request.operation_id, "report.json", false)?;
        if report_path.is_file() {
            let report: MigrationReport = disk::read_json(&report_path, 4 * 1024 * 1024)?;
            if report.inventory_hash != request.inventory.inventory_hash {
                return Err(error(ProjectErrorCode::IdempotencyMismatch, "Migration inventory differs from its recorded operation"));
            }
            if report.state == MigrationState::SourceCommitted {
                return Ok(report);
            }
        }
        backup(root, &request.operation_id, &request.inventory, &request.recovery)?;
        let entry = request
            .inventory
            .entries
            .iter()
            .find(|entry| entry.path == request.input.as_str() && matches!(entry.kind, LegacyKind::GuiNative | LegacyKind::SharedNative))
            .ok_or_else(|| error(ProjectErrorCode::ResourceMissing, "The selected native source is not in the backed-up inventory"))?;
        let native = disk::read_bounded(&disk::scoped_path(root, request.input.as_str(), false)?, entry.content.length)?;
        entry.content.verify(&native)?;
        let mut report = MigrationReport {
            schema_version: SCHEMA_VERSION,
            operation_id: request.operation_id.clone(),
            inventory_hash: request.inventory.inventory_hash.clone(),
            state: MigrationState::NeedsReview,
            source: None,
            mappings: vec![],
            notes: vec![],
        };
        let attempt = (|| -> ProjectResult<SourceSaveReceipt> {
            let mut document = photocraft_format::load_from_bytes(&native)
                .map_err(|e| error(ProjectErrorCode::UnsupportedGuiFeature, format!("Complete legacy container retained: {e}")))?;
            let design = document.defold.as_ref().ok_or_else(|| {
                error(ProjectErrorCode::UnsupportedGuiFeature, "Standalone legacy creative asset requires an explicit shared-package migration")
            })?;
            let current =
                store.read_resource(&ResourceReadRequest { context: request.context.clone(), path: request.scene.path.clone(), expected_hash: None })?;
            let matches = design.disk_hash == blake3::hash(&current.bytes).to_hex().as_str()
                || design
                    .project
                    .as_ref()
                    .and_then(|p| p.imported_disk_hash.as_ref())
                    .is_some_and(|hash| ContentHash::parse(hash).is_ok_and(|hash| hash.verify(&current.bytes)));
            if !matches {
                return Err(error(
                    ProjectErrorCode::TargetChanged,
                    "GUI changed since this legacy draft; original bytes are backed up and no new source head was committed",
                ));
            }
            let gui_source = String::from_utf8(current.bytes).map_err(io)?;
            let baseline = SceneBaseline {
                scene_id: request.scene.scene_id.clone(),
                path: request.scene.path.clone(),
                gui_source: gui_source.clone(),
                disk_hash: current.version.content.hash.clone(),
                scene_hash: ContentHash::sha256(gui_source.as_bytes()),
                published_revision: None,
            };
            report.mappings = attach_identities(&mut document, &request.context, &request.scene, &baseline)?;
            let bytes = photocraft_format::save_to_bytes(&document, &Default::default()).map_err(io)?;
            let restored = photocraft_format::load_from_bytes(&bytes).map_err(io)?;
            if restored != document {
                return Err(error(ProjectErrorCode::HashMismatch, "Migrated native source did not round-trip exactly"));
            }
            // Linked external smart sources need explicit dependency capture before migration can be acknowledged.
            if has_linked_sources(&document.layers) {
                return Err(error(
                    ProjectErrorCode::UnsupportedGuiFeature,
                    "Linked smart sources need dependency packaging; complete source retained for review",
                ));
            }
            if let Some(previous) =
                store.operation_status(&OperationRequest { context: request.context.clone(), operation_id: request.operation_id.clone() })?
            {
                if previous.package_id != request.scene.package_id || previous.content != BlobDescriptor::of_bytes(&bytes) {
                    return Err(error(ProjectErrorCode::IdempotencyMismatch, "Migration operation was committed for different native bytes"));
                }
                return Ok(previous);
            }
            let leases = [LeaseResource::Scene(request.scene.scene_id.clone()), LeaseResource::Package(request.scene.package_id.clone())]
                .into_iter()
                .map(|resource| {
                    store
                        .acquire_lease(&LeaseRequest { context: request.context.clone(), client_id: request.client_id.clone(), resource, duration_ms: 300_000 })
                        .map(|lease| lease.proof)
                })
                .collect::<ProjectResult<Vec<_>>>()?;
            let project = document
                .defold
                .as_ref()
                .and_then(|d| d.project.as_ref())
                .ok_or_else(|| error(ProjectErrorCode::InvalidRequest, "Migrated project attachment is missing"))?;
            let revision = format!("migration-{}", request.operation_id.as_str());
            let source = SaveSourceRequest {
                schema_version: SCHEMA_VERSION,
                operation_id: request.operation_id.clone(),
                context: request.context.clone(),
                client_id: request.client_id.clone(),
                scene_id: Some(request.scene.scene_id.clone()),
                package_id: request.scene.package_id.clone(),
                expected_source_revision: request.scene.source_revision.clone(),
                leases,
                captured_native_revision: 0,
                envelope: SourceEnvelope {
                    schema_version: SCHEMA_VERSION,
                    package_id: request.scene.package_id.clone(),
                    source_revision: revision.clone(),
                    parent_source_revision: request.scene.source_revision.clone(),
                    draft_revision: revision,
                    scene_bases: vec![baseline],
                    identity_map: project.layers.iter().map(|(id, binding)| (*id, binding.layer_id.clone())).collect(),
                    native: BlobDescriptor::of_bytes(&bytes),
                    dependencies: vec![],
                    draft: DraftSnapshot {
                        gui_sources: BTreeMap::from([(
                            request.scene.scene_id.clone(),
                            document.defold.as_ref().map(|design| design.source.clone()).unwrap_or_default(),
                        )]),
                        shared_sources: vec![],
                    },
                },
            };
            store.save_source(&source, &SourcePayload { native: bytes, dependencies: BTreeMap::new() })
        })();
        match attempt {
            Ok(receipt) => {
                report.state = MigrationState::SourceCommitted;
                report.source = Some(receipt);
                report.notes.push("Original files remain unchanged. Asset mappings are external until ownership is verified explicitly.".into());
            }
            Err(failure) => report.notes.push(failure.to_string()),
        }
        disk::write_json(&migration_file(root, &request.operation_id, "report.json", true)?, &report)?;
        Ok(report)
    }
    fn has_linked_sources(layers: &[photocraft_doc::Layer]) -> bool {
        layers.iter().any(|layer| match &layer.content {
            photocraft_doc::LayerContent::Smart(object) => matches!(object.source, photocraft_doc::SmartSource::Linked { .. }),
            photocraft_doc::LayerContent::Group(group) => has_linked_sources(&group.children),
            _ => false,
        })
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub use native::{NativeMigrationRequest, backup, discover, migrate_native};

#[cfg(test)]
#[path = "defold_migration_tests.rs"]
mod tests;
