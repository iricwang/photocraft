//! Authority-side inventory and explicit, non-destructive legacy migration.
use super::*;
use crate::{defold_migration as migration, defold_publish_plan as assets};

fn operation_file(store: &LocalProjectStore, operation: &OperationId, name: &str, create: bool) -> ProjectResult<PathBuf> {
    disk::scoped_path(&store.root, &format!(".photocraft/migrations/{operation}/{name}"), create)
}

fn inventory(store: &LocalProjectStore, registry: &assets::AssetRegistry) -> ProjectResult<migration::MigrationInventory> {
    let mut inventory = migration::discover(&store.root, &BTreeMap::new(), &store.limits)?;
    // Correctly registered new outputs are current project assets, not migration candidates.
    // Missing ownership or changed bytes remain visible for review.
    inventory.entries.retain(|entry| {
        let Ok(path) = ResourcePath::parse(&entry.path) else { return true };
        registry.managed_files.get(&path) != Some(&entry.content) || path.as_str().ends_with(".png") && !registry.path_owners.contains_key(&path)
    });
    inventory.inventory_hash = ContentHash::sha256(&canonical_json(&serde_json::to_value(&inventory.entries).map_err(io)?)?);
    Ok(inventory)
}

pub(super) fn inspect(store: &LocalProjectStore, context: &ProjectContext) -> ProjectResult<AssetManagementSnapshot> {
    let mut state = store.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    store.check_context(&mut state, context)?;
    let registry = crate::defold_project_publication::registry(&store.root)?;
    let legacy_inventory = inventory(store, &registry)?;
    let mut warnings = vec![
        "Editor Undo, unsaved GUI graphs, AI results and retained Git/source versions are not completely enumerable; no physical collection is authorized."
            .into(),
        "Application recovery entries outside the project remain untouched and require explicit recovery review.".into(),
    ];
    let mut current = BTreeMap::new();
    let mut roots = BTreeMap::new();
    let mut total = 0u64;
    for asset in registry.assets.values() {
        let mut retained = BTreeSet::from([assets::RetentionReason::Publication, assets::RetentionReason::UnknownRoots]);
        if asset.source.is_some() {
            retained.insert(assets::RetentionReason::SourceVersion);
        }
        if !asset.referencing_scenes.is_empty() {
            retained.insert(assets::RetentionReason::CurrentWorkspace);
        }
        if asset.referencing_scenes.len() > 1 {
            retained.insert(assets::RetentionReason::SharedReference);
        }
        roots.insert(asset.asset_id.clone(), retained);
        if registry.path_owners.get(&asset.image) != Some(&asset.asset_id) {
            warnings.push(format!("{}: managed path ownership needs review", asset.image));
        }
        if asset.ownership != AssetOwnership::Managed || !asset.image.as_str().starts_with("/photocraft_generated/") {
            warnings.push(format!("{}: external or unverified location remains outside managed inspection", asset.image));
            continue;
        }
        let path = disk::scoped_path(&store.root, asset.image.as_str(), false)?;
        if !path.is_file() {
            warnings.push(format!("{}: output is missing; its source and history remain retained", asset.image));
            continue;
        }
        let size = fs::metadata(&path).map_err(io)?.len();
        total = total.checked_add(size).ok_or_else(|| fail(ProjectErrorCode::LimitExceeded, "Asset inspection size overflow"))?;
        if total > store.limits.max_total_source_bytes {
            warnings.push("Asset byte inspection reached its bounded budget; remaining outputs require review".into());
            break;
        }
        let bytes = disk::read_bounded(&path, store.limits.max_dependency_bytes)?;
        let descriptor = BlobDescriptor::of_bytes(&bytes);
        if descriptor != asset.output {
            warnings.push(format!("{}: bytes changed outside the acknowledged publication", asset.image));
        }
        current.insert(asset.image.clone(), descriptor);
    }
    let mut migrations = Vec::new();
    let directory = disk::scoped_path(&store.root, ".photocraft/migrations", false)?;
    if directory.is_dir() {
        for (index, entry) in fs::read_dir(directory).map_err(io)?.enumerate() {
            if index >= 10_000 {
                return Err(fail(ProjectErrorCode::LimitExceeded, "Migration history exceeds report limit"));
            }
            let entry = entry.map_err(io)?;
            let Ok(operation) = OperationId::parse(&entry.file_name().to_string_lossy()) else { continue };
            let path = operation_file(store, &operation, "report.json", false)?;
            if path.is_file() {
                let report: migration::MigrationReport = disk::read_json(&path, store.limits.max_control_bytes)?;
                check_schema(report.schema_version)?;
                if report.operation_id != operation {
                    return Err(fail(ProjectErrorCode::HashMismatch, "Migration report identity does not match its directory"));
                }
                migrations.push(report);
            }
        }
    }
    migrations.sort_by(|a, b| a.operation_id.cmp(&b.operation_id));
    store.check_context(&mut state, context)?;
    Ok(AssetManagementSnapshot {
        schema_version: SCHEMA_VERSION,
        context: context.clone(),
        retained: assets::retained_file_preview(&registry),
        collection: assets::collection_preview(&registry, &current, &roots),
        registry,
        legacy_inventory,
        migrations,
        warnings,
        roots_complete: false,
    })
}

pub(super) fn report(store: &LocalProjectStore, request: &OperationRequest) -> ProjectResult<Option<migration::MigrationReport>> {
    let mut state = store.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    store.check_context(&mut state, &request.context)?;
    let path = operation_file(store, &request.operation_id, "request.json", false)?;
    if !path.is_file() {
        return Ok(None);
    }
    let original: LegacyMigrationRequest = disk::read_json(&path, store.limits.max_control_bytes)?;
    if original.context != request.context || original.operation_id != request.operation_id {
        return Err(fail(ProjectErrorCode::IdempotencyMismatch, "Migration operation belongs to another context"));
    }
    let path = operation_file(store, &request.operation_id, "report.json", false)?;
    if !path.is_file() {
        return Ok(None);
    }
    let report: migration::MigrationReport = disk::read_json(&path, store.limits.max_control_bytes)?;
    check_schema(report.schema_version)?;
    if report.operation_id != request.operation_id || report.inventory_hash != original.inventory_hash {
        return Err(fail(ProjectErrorCode::HashMismatch, "Migration receipt identity changed"));
    }
    Ok(Some(report))
}

pub(super) fn migrate(store: &LocalProjectStore, request: &LegacyMigrationRequest) -> ProjectResult<migration::MigrationReport> {
    let _gate = store.migration_gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    {
        let mut state = store.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        store.check_context(&mut state, &request.context)?;
        let scene =
            state.index.scenes.get(&request.scene.scene_id).ok_or_else(|| fail(ProjectErrorCode::ResourceMissing, "Migration GUI is not registered"))?;
        if scene.path != request.scene.path || scene.package_id != request.scene.package_id {
            return Err(fail(ProjectErrorCode::ProjectMismatch, "Migration source belongs to another scene package"));
        }
    }
    let request_path = operation_file(store, &request.operation_id, "request.json", false)?;
    if request_path.is_file() {
        let original: LegacyMigrationRequest = disk::read_json(&request_path, store.limits.max_control_bytes)?;
        if original != *request {
            return Err(fail(ProjectErrorCode::IdempotencyMismatch, "Migration operation already belongs to another selected input"));
        }
        if let Some(report) = report(store, &OperationRequest { context: request.context.clone(), operation_id: request.operation_id.clone() })?
            && report.state == migration::MigrationState::SourceCommitted
        {
            return Ok(report);
        }
    }
    let registry = store.read_asset_registry(&request.context)?;
    let inventory = inventory(store, &registry)?;
    if inventory.inventory_hash != request.inventory_hash {
        return Err(fail(ProjectErrorCode::TargetChanged, "Legacy inventory changed; refresh the review before migration"));
    }
    let entry = inventory
        .entries
        .iter()
        .find(|entry| entry.path == request.input.as_str())
        .ok_or_else(|| fail(ProjectErrorCode::PathInvalid, "Only an input from the reviewed project legacy inventory can be migrated"))?;
    if !matches!(entry.kind, migration::LegacyKind::GuiNative | migration::LegacyKind::SharedNative)
        || entry.kind == migration::LegacyKind::GuiNative && request.input.as_str() != format!("{}.pcraft", request.scene.path)
    {
        return Err(fail(ProjectErrorCode::UnsupportedGuiFeature, "Select the complete native source for this GUI; other legacy bytes remain review inputs"));
    }
    // Raw content is durable and the inventory manifest is atomic before the operation can
    // create a formal source head. A repeated request is tied to exactly the same selection.
    migration::backup(&store.root, &request.operation_id, &inventory, &BTreeMap::new())?;
    disk::write_json(&operation_file(store, &request.operation_id, "request.json", true)?, request)?;
    migration::migrate_native(
        &store.root,
        store,
        &migration::NativeMigrationRequest {
            context: request.context.clone(),
            client_id: request.client_id.clone(),
            operation_id: request.operation_id.clone(),
            scene: request.scene.clone(),
            input: request.input.clone(),
            inventory,
            recovery: BTreeMap::new(),
        },
    )
}
