#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;

fn fixture() -> (tempfile::TempDir, Record) {
    let root = tempfile::tempdir().unwrap();
    let plan: PublishPlan = serde_json::from_slice(include_bytes!("../tests/fixtures/project_v1/publish-multi-target.json")).unwrap();
    let mut targets = Vec::new();
    for target in &plan.targets {
        let path = disk::scoped_path(root.path(), target.path.as_str(), true).unwrap();
        let bytes = format!("nodes {{ id: \"{}\" }}", target.scene_id.as_str()).into_bytes();
        disk::write_atomic(&path, &bytes).unwrap();
        let hash = ContentHash::sha256(&bytes);
        targets.push(serde_json::json!({"path":target.path,"diskHash":hash,"sceneHash":hash,"editorGraphRevision":hash,"editorAhead":false}));
    }
    let adapter_receipt = Some(serde_json::json!({"schemaVersion":1,"transactionId":plan.transaction_id,"targets":targets,"resources":[]}));
    let mut receipt = empty_receipt(&plan);
    receipt.phase = PublishPhase::Saving;
    let record = Record { schema_version: 1, plan, receipt, before: vec![], adapter_receipt, apply_requested: true };
    persist(root.path(), &record).unwrap();
    (root, record)
}

#[test]
fn actual_saved_bytes_and_receipt_are_archived_before_atomic_catalog_commit() {
    let (root, record) = fixture();
    let receipt = finish(root.path(), &record, &Default::default()).unwrap();
    assert_eq!(receipt.phase, PublishPhase::Committed);
    assert_eq!(receipt.targets.len(), 2);
    for file in &receipt.files {
        let archived = archived_path(root.path(), &receipt.transaction_id, &file.content.hash, false).unwrap();
        file.content.verify(&std::fs::read(archived).unwrap()).unwrap();
    }
    assert_eq!(latest(root.path(), &receipt.targets[0].scene_id).unwrap(), Some(receipt.clone()));
    // After response loss, status reads the committed authority without a running editor.
    assert_eq!(
        status(root.path(), &PublishRequest { context: record.plan.context, transaction_id: receipt.transaction_id.clone() }, &Default::default()).unwrap(),
        Some(receipt)
    );
}

#[test]
fn published_baseline_uses_archived_saved_bytes_even_after_disk_changes() {
    let (root, record) = fixture();
    let target = &record.plan.targets[0];
    let request = SceneRequest { context: record.plan.context.clone(), scene_id: target.scene_id.clone(), path: target.path.clone() };
    assert!(published_baseline(root.path(), &request).unwrap().is_none());
    let receipt = finish(root.path(), &record, &Default::default()).unwrap();
    let original = disk::read_bounded(&disk::scoped_path(root.path(), target.path.as_str(), false).unwrap(), 4096).unwrap();
    disk::write_atomic(&disk::scoped_path(root.path(), target.path.as_str(), false).unwrap(), b"external-after-publication").unwrap();
    let baseline = published_baseline(root.path(), &request).unwrap().unwrap();
    assert_eq!(baseline.gui_source.as_bytes(), original);
    assert_eq!(baseline.published_revision.as_deref(), Some(receipt.transaction_id.as_str()));
    let archive = archived_path(root.path(), &receipt.transaction_id, &baseline.disk_hash, false).unwrap();
    std::fs::remove_file(archive).unwrap();
    assert_eq!(published_baseline(root.path(), &request).unwrap_err().code, ProjectErrorCode::ResourceMissing);
}

#[test]
fn missing_target_or_changed_file_cannot_commit_or_update_registry() {
    let (root, mut record) = fixture();
    record.adapter_receipt.as_mut().unwrap()["targets"].as_array_mut().unwrap().pop();
    assert_eq!(finish(root.path(), &record, &Default::default()).unwrap_err().code, ProjectErrorCode::SaveFailed);
    assert!(catalog(root.path()).unwrap().receipts.is_empty());
    let (root, record) = fixture();
    let path = disk::scoped_path(root.path(), record.plan.targets[0].path.as_str(), false).unwrap();
    disk::write_atomic(&path, b"externally changed").unwrap();
    assert_eq!(finish(root.path(), &record, &Default::default()).unwrap_err().code, ProjectErrorCode::TargetChanged);
    assert!(catalog(root.path()).unwrap().receipts.is_empty());
}

#[test]
fn pending_publication_fences_leases_but_committed_history_does_not() {
    let (root, record) = fixture();
    let resource = record.plan.leases[0].resource.clone();
    assert!(resources_locked(root.path(), std::slice::from_ref(&resource), None).unwrap());
    assert!(!resources_locked(root.path(), std::slice::from_ref(&resource), Some(&record.plan.transaction_id)).unwrap());
    finish(root.path(), &record, &Default::default()).unwrap();
    assert!(!resources_locked(root.path(), std::slice::from_ref(&resource), None).unwrap());
}

#[test]
fn directory_name_never_grants_asset_overwrite_ownership() {
    let (plan, _, file) = image_boundary_fixture();
    assert_eq!(validate_file_boundary(&file, &plan, &Catalog::default()).unwrap_err().code, ProjectErrorCode::TargetChanged);
}

fn image_boundary_fixture() -> (PublishPlan, Catalog, PlannedFile) {
    let (_, record) = fixture();
    let mut plan = record.plan;
    let asset_id = AssetId::from_bytes([7; 16]);
    let image = ResourcePath::parse("/photocraft_generated/scene/images/A.png").unwrap();
    let before = BlobDescriptor::of_bytes(b"before");
    let after = BlobDescriptor::of_bytes(b"after");
    let asset = AssetRecord {
        asset_id: asset_id.clone(),
        display_name: "A".into(),
        ownership: AssetOwnership::Managed,
        visibility: AssetVisibility::Private,
        state: AssetState::Active,
        source: plan.source_revisions.first().cloned(),
        source_node: None,
        image: image.clone(),
        output: before.clone(),
        runtime_frame_id: "asset_7".into(),
        referencing_scenes: [plan.targets[0].scene_id.clone()].into_iter().collect(),
        output_revision: "published-before".into(),
        recipe: None,
    };
    plan.asset_changes.push(AssetChange {
        asset_id: asset_id.clone(),
        display_name: asset.display_name.clone(),
        ownership: asset.ownership,
        visibility: asset.visibility,
        state: asset.state,
        source: asset.source.clone(),
        source_node: asset.source_node.clone(),
        image: Some(image.clone()),
        runtime_frame_id: Some(asset.runtime_frame_id.clone()),
        referencing_scenes: asset.referencing_scenes.iter().cloned().collect(),
        recipe: None,
    });
    plan.leases.push(LeaseProof { resource: LeaseResource::Asset(asset_id.clone()), lease_epoch: 1 });
    let mut catalog = Catalog::default();
    catalog.registry.assets.insert(asset_id.clone(), asset);
    catalog.registry.path_owners.insert(image.clone(), asset_id.clone());
    catalog.registry.managed_files.insert(image.clone(), before.clone());
    let file = PlannedFile {
        writer: FileWriter::HostFile,
        action: FileAction::Replace,
        asset_id: Some(asset_id),
        old_path: Some(image.clone()),
        new_path: Some(image),
        expected_old_hash: Some(before.hash),
        content: Some(after),
    };
    (plan, catalog, file)
}

#[test]
fn managed_png_requires_explicit_identity_asset_update_and_lease() {
    let (plan, catalog, file) = image_boundary_fixture();
    validate_file_boundary(&file, &plan, &catalog).unwrap();
    let mut unnamed = file.clone();
    unnamed.asset_id = None;
    assert_eq!(validate_file_boundary(&unnamed, &plan, &catalog).unwrap_err().code, ProjectErrorCode::InvalidRequest);
    let mut no_change = plan.clone();
    no_change.asset_changes.clear();
    assert_eq!(validate_file_boundary(&file, &no_change, &catalog).unwrap_err().code, ProjectErrorCode::InvalidRequest);
    let mut external_change = plan.clone();
    external_change.asset_changes[0].ownership = AssetOwnership::External;
    assert_eq!(validate_file_boundary(&file, &external_change, &catalog).unwrap_err().code, ProjectErrorCode::InvalidRequest);
    let mut no_lease = plan;
    no_lease.leases.retain(|lease| !matches!(lease.resource, LeaseResource::Asset(_)));
    assert_eq!(validate_file_boundary(&file, &no_lease, &catalog).unwrap_err().code, ProjectErrorCode::LeaseLost);
}

#[test]
fn managed_png_rejects_missing_foreign_or_unversioned_historical_ownership() {
    let (plan, catalog, file) = image_boundary_fixture();
    for mutation in 0..5 {
        let mut incomplete = catalog.clone();
        match mutation {
            0 => incomplete.registry.path_owners.clear(),
            1 => {
                incomplete.registry.path_owners.insert(file.old_path.clone().unwrap(), AssetId::from_bytes([8; 16]));
            }
            2 => incomplete.registry.managed_files.clear(),
            3 => incomplete.registry.assets.clear(),
            _ => incomplete.registry.assets.values_mut().for_each(|asset| asset.ownership = AssetOwnership::External),
        }
        assert_eq!(validate_file_boundary(&file, &plan, &incomplete).unwrap_err().code, ProjectErrorCode::TargetChanged);
    }
    let mut modified = catalog;
    modified.registry.managed_files.insert(file.old_path.clone().unwrap(), BlobDescriptor::of_bytes(b"different-publication"));
    assert_eq!(validate_file_boundary(&file, &plan, &modified).unwrap_err().code, ProjectErrorCode::TargetChanged);
}

#[test]
fn managed_png_requires_previous_and_new_affected_scenes() {
    let (plan, catalog, file) = image_boundary_fixture();
    let affected = plan.targets[1].scene_id.clone();
    for existing_reference in [false, true] {
        let mut plan = plan.clone();
        let mut catalog = catalog.clone();
        if existing_reference {
            catalog.registry.assets.values_mut().for_each(|asset| {
                asset.referencing_scenes.insert(affected.clone());
            });
        } else {
            plan.asset_changes[0].referencing_scenes.push(affected.clone());
        }
        validate_file_boundary(&file, &plan, &catalog).unwrap();
        let mut no_lease = plan.clone();
        no_lease.leases.retain(|lease| lease.resource != LeaseResource::Scene(affected.clone()));
        assert_eq!(validate_file_boundary(&file, &no_lease, &catalog).unwrap_err().code, ProjectErrorCode::LeaseLost);
        plan.targets.retain(|target| target.scene_id != affected);
        assert_eq!(validate_file_boundary(&file, &plan, &catalog).unwrap_err().code, ProjectErrorCode::InvalidRequest);
    }
}

#[test]
fn managed_png_destination_must_match_its_asset_update() {
    let (mut plan, catalog, file) = image_boundary_fixture();
    plan.asset_changes[0].image = Some(ResourcePath::parse("/photocraft_generated/other.png").unwrap());
    assert_eq!(validate_file_boundary(&file, &plan, &catalog).unwrap_err().code, ProjectErrorCode::InvalidRequest);
    plan.asset_changes[0].image = None;
    assert_eq!(validate_file_boundary(&file, &plan, &catalog).unwrap_err().code, ProjectErrorCode::InvalidRequest);
}

#[test]
fn managed_png_allows_create_and_verified_undo_replace_with_retained_current_path() {
    let (plan, mut catalog, mut file) = image_boundary_fixture();
    let mut create = file.clone();
    create.action = FileAction::Create;
    create.old_path = None;
    create.expected_old_hash = None;
    validate_file_boundary(&create, &plan, &Catalog::default()).unwrap();
    // A retained older A can replace the current B without inventing a third name. Both
    // preconditions are independently checked, while removing B only retains its bytes.
    let current = ResourcePath::parse("/photocraft_generated/scene/images/B.png").unwrap();
    let current_content = BlobDescriptor::of_bytes(b"current-B");
    let owner = file.asset_id.clone().unwrap();
    catalog.registry.assets.get_mut(&owner).unwrap().image = current.clone();
    catalog.registry.path_owners.insert(current.clone(), owner.clone());
    catalog.registry.managed_files.insert(current.clone(), current_content.clone());
    validate_file_boundary(&file, &plan, &catalog).unwrap();
    file.action = FileAction::Remove;
    file.old_path = Some(current);
    file.new_path = None;
    file.content = None;
    file.expected_old_hash = Some(current_content.hash);
    validate_file_boundary(&file, &plan, &catalog).unwrap();
    file.expected_old_hash = Some(ContentHash::sha256(b"stale-B"));
    assert_eq!(validate_file_boundary(&file, &plan, &catalog).unwrap_err().code, ProjectErrorCode::TargetChanged);
}

#[test]
fn metadata_only_updates_cannot_relocate_reidentify_or_claim_assets() {
    let (plan, catalog, file) = image_boundary_fixture();
    validate_asset_updates(&plan, &catalog.registry).unwrap();
    for mutation in 0..3 {
        let mut malformed = plan.clone();
        match mutation {
            0 => malformed.asset_changes[0].image = Some(ResourcePath::parse("/photocraft_generated/foreign.png").unwrap()),
            1 => malformed.asset_changes[0].runtime_frame_id = Some("new-runtime-name".into()),
            _ => malformed.asset_changes[0].ownership = AssetOwnership::External,
        }
        assert!(validate_asset_updates(&malformed, &catalog.registry).is_err());
    }
    assert_eq!(validate_asset_updates(&plan, &AssetRegistry::default()).unwrap_err().code, ProjectErrorCode::InvalidRequest);
    let mut create = plan;
    let owner = file.asset_id.clone().unwrap();
    create.asset_changes[0].runtime_frame_id = Some(crate::defold_publish_plan::runtime_frame_id(&owner));
    create.planned_files.push(PlannedFile { action: FileAction::Create, old_path: None, expected_old_hash: None, ..file });
    validate_asset_updates(&create, &AssetRegistry::default()).unwrap();
}

#[test]
fn metadata_only_detach_display_and_recipe_updates_preserve_verified_output() {
    let (mut plan, catalog, _) = image_boundary_fixture();
    let update = &mut plan.asset_changes[0];
    update.display_name = "Renamed in source".into();
    update.state = AssetState::Detached;
    update.referencing_scenes.clear();
    update.recipe =
        Some(ExportRecipe { width: 8, height: 8, crop_origin: [3, 4], node_local_transform: [1., 0., 0., 1., 0., 0.], cross_node_composition: false });
    validate_asset_updates(&plan, &catalog.registry).unwrap();
}

#[test]
fn returning_to_history_requires_the_current_image_precondition_too() {
    let (mut plan, mut catalog, file) = image_boundary_fixture();
    let owner = file.asset_id.clone().unwrap();
    let current = ResourcePath::parse("/photocraft_generated/scene/images/B.png").unwrap();
    let current_content = BlobDescriptor::of_bytes(b"current-B");
    let old = catalog.registry.assets.get_mut(&owner).unwrap();
    old.image = current.clone();
    old.output = current_content.clone();
    plan.planned_files.push(file);
    assert_eq!(validate_asset_updates(&plan, &catalog.registry).unwrap_err().code, ProjectErrorCode::InvalidRequest);
    plan.planned_files.push(PlannedFile {
        writer: FileWriter::HostFile,
        action: FileAction::Remove,
        asset_id: Some(owner),
        old_path: Some(current),
        new_path: None,
        expected_old_hash: Some(current_content.hash),
        content: None,
    });
    validate_asset_updates(&plan, &catalog.registry).unwrap();
}

fn atlas_boundary_fixture() -> (PublishPlan, Catalog, PublishPayload, BTreeMap<ResourcePath, Vec<u8>>) {
    let (mut plan, catalog, _) = image_boundary_fixture();
    let scene = plan.targets[0].scene_id.clone();
    let path = ResourcePath::parse(&scene_atlas(&scene)).unwrap();
    let asset = catalog.registry.assets.values().next().unwrap();
    let bytes = crate::defold_atlas::encode(&[crate::defold_atlas::AtlasFrame { id: asset.runtime_frame_id.clone(), image: asset.image.clone() }]).unwrap();
    let descriptor = BlobDescriptor::of_bytes(&bytes);
    plan.node_changes[0].textures.push(GuiTexture { name: scene_alias(&scene), texture: path.clone() });
    plan.node_changes[0].nodes[0].values.insert("texture".into(), serde_json::json!(format!("{}/{}", scene_alias(&scene), asset.runtime_frame_id)));
    plan.planned_files.push(PlannedFile {
        writer: FileWriter::EditorResource,
        action: FileAction::Create,
        asset_id: None,
        old_path: None,
        new_path: Some(path),
        expected_old_hash: None,
        content: Some(descriptor.clone()),
    });
    let payload = PublishPayload { contents: BTreeMap::from([(descriptor.hash, bytes)]) };
    let images = BTreeMap::from([(asset.image.clone(), b"before".to_vec())]);
    (plan, catalog, payload, images)
}

#[test]
fn managed_atlas_requires_scene_alias_explicit_ownership_and_exact_image_bytes() {
    let (plan, catalog, payload, images) = atlas_boundary_fixture();
    let read = |path: &ResourcePath, _: &BlobDescriptor| images.get(path).cloned().ok_or_else(|| error(ProjectErrorCode::ResourceMissing, "test resource"));
    validate_atlas_updates(&plan, &payload, &catalog.registry, read).unwrap();
    for mutation in 0..6 {
        let mut malformed = plan.clone();
        let mut catalog = catalog.clone();
        match mutation {
            0 => malformed.node_changes[0].textures[0].name = "wrong_alias".into(),
            1 => malformed.node_changes[0].textures.clear(),
            2 => catalog.registry.path_owners.clear(),
            3 => malformed.planned_files[0].asset_id = Some(AssetId::from_bytes([7; 16])),
            4 => malformed.node_changes[0].textures[0].texture = ResourcePath::parse(&scene_atlas(&malformed.targets[1].scene_id)).unwrap(),
            _ => malformed.asset_changes[0].referencing_scenes.clear(),
        }
        assert!(validate_atlas_updates(&malformed, &payload, &catalog.registry, read).is_err());
    }
    assert_eq!(
        validate_atlas_updates(&plan, &payload, &catalog.registry, |_, _| Ok(b"outside-change".to_vec())).unwrap_err().code,
        ProjectErrorCode::HashMismatch
    );
}

#[test]
fn managed_atlas_cannot_claim_existing_resources_without_a_write_or_verified_registry_entry() {
    let (mut plan, mut catalog, payload, mut images) = atlas_boundary_fixture();
    let file = plan.planned_files.remove(0);
    let path = file.new_path.unwrap();
    let descriptor = file.content.unwrap();
    let bytes = payload.contents[&descriptor.hash].clone();
    assert_eq!(
        validate_atlas_updates(&plan, &PublishPayload::default(), &catalog.registry, |path, _| Ok(images[path].clone())).unwrap_err().code,
        ProjectErrorCode::TargetChanged
    );
    catalog.registry.managed_files.insert(path.clone(), descriptor);
    images.insert(path, bytes);
    validate_atlas_updates(&plan, &PublishPayload::default(), &catalog.registry, |path, _| Ok(images[path].clone())).unwrap();
    let alias = scene_alias(&plan.targets[0].scene_id);
    plan.node_changes[0].nodes[0].values.insert("texture".into(), serde_json::json!(format!("{alias}/missing_frame")));
    assert_eq!(
        validate_atlas_updates(&plan, &PublishPayload::default(), &catalog.registry, |path, _| Ok(images[path].clone())).unwrap_err().code,
        ProjectErrorCode::ResourceMissing
    );
}

#[test]
fn managed_atlas_rejects_forged_frame_to_image_mapping() {
    let (plan, catalog, mut payload, images) = atlas_boundary_fixture();
    for (id, image) in [("wrong_frame", "/photocraft_generated/scene/images/A.png"), ("asset_7", "/photocraft_generated/foreign.png")] {
        let mut plan = plan.clone();
        let bytes = crate::defold_atlas::encode(&[crate::defold_atlas::AtlasFrame { id: id.into(), image: ResourcePath::parse(image).unwrap() }]).unwrap();
        let descriptor = BlobDescriptor::of_bytes(&bytes);
        plan.planned_files[0].content = Some(descriptor.clone());
        payload.contents.insert(descriptor.hash, bytes);
        assert_eq!(
            validate_atlas_updates(&plan, &payload, &catalog.registry, |path, _| Ok(images[path].clone())).unwrap_err().code,
            ProjectErrorCode::TargetChanged
        );
    }
}

#[test]
fn preparing_crash_can_repeat_only_prepare_after_definitive_missing_receipt() {
    let (root, mut record) = fixture();
    record.apply_requested = false;
    record.receipt.phase = PublishPhase::Preparing;
    record.adapter_receipt = None;
    persist(root.path(), &record).unwrap();
    let restarted = read_record(root.path(), &record.plan.transaction_id).unwrap().unwrap();
    let called = std::cell::Cell::new(false);
    let receipt =
        recover_status(root.path(), restarted, Err(error(ProjectErrorCode::ResourceMissing, "Adapter has no transaction")), &Default::default(), || {
            called.set(true);
            Ok(editor::EditorStatus { state: "PREPARED".into(), receipt: None, error: None })
        })
        .unwrap();
    assert!(called.get());
    assert_eq!(receipt.phase, PublishPhase::Prepared);
    assert_eq!(receipt.transaction_id, record.plan.transaction_id);
    assert_eq!(receipt.plan_hash, record.plan.plan_hash);
    assert!(!read_record(root.path(), &receipt.transaction_id).unwrap().unwrap().apply_requested);
}

#[test]
fn apply_intent_or_ambiguous_network_failure_never_repeats_prepare() {
    for (apply_requested, code) in [(true, ProjectErrorCode::ResourceMissing), (false, ProjectErrorCode::Io)] {
        let (root, mut record) = fixture();
        record.apply_requested = apply_requested;
        record.receipt.phase = PublishPhase::RecoveryRequired;
        record.adapter_receipt = None;
        let called = std::cell::Cell::new(false);
        let receipt = recover_status(root.path(), record.clone(), Err(error(code, "Unknown acknowledgement")), &Default::default(), || {
            called.set(true);
            Ok(editor::EditorStatus { state: "PREPARED".into(), receipt: None, error: None })
        })
        .unwrap();
        assert!(!called.get());
        assert_eq!(receipt.phase, PublishPhase::RecoveryRequired);
        assert!(resources_locked(root.path(), &[record.plan.leases[0].resource.clone()], None).unwrap());
    }
}

#[test]
fn old_journals_without_send_boundary_are_conservative_and_unapplied_abort_releases_locks() {
    let (root, mut record) = fixture();
    let mut legacy = serde_json::to_value(&record).unwrap();
    legacy.as_object_mut().unwrap().remove("applyRequested");
    assert!(serde_json::from_value::<Record>(legacy).unwrap().apply_requested);
    record.apply_requested = false;
    record.receipt.phase = PublishPhase::RecoveryRequired;
    let resource = record.plan.leases[0].resource.clone();
    let receipt = abort_unapplied(root.path(), record, Some(error(ProjectErrorCode::ResourceMissing, "Adapter did not prepare this transaction"))).unwrap();
    assert_eq!(receipt.phase, PublishPhase::Aborted);
    assert!(!resources_locked(root.path(), &[resource], None).unwrap());
}

#[test]
fn committed_rename_persists_both_path_owners_and_verified_historical_bytes() {
    let (root, mut record) = fixture();
    let owner = AssetId::from_bytes([7; 16]);
    let old = ResourcePath::parse("/photocraft_generated/scene/images/A.png").unwrap();
    let new = ResourcePath::parse("/photocraft_generated/scene/images/B.png").unwrap();
    let old_bytes = b"older-image";
    let new_bytes = b"current-image";
    let before = BlobDescriptor::of_bytes(old_bytes);
    let after = BlobDescriptor::of_bytes(new_bytes);
    disk::write_atomic(&disk::scoped_path(root.path(), old.as_str(), true).unwrap(), old_bytes).unwrap();
    disk::write_atomic(&disk::scoped_path(root.path(), new.as_str(), true).unwrap(), new_bytes).unwrap();
    record.before.push(ResourceVersion { path: old.clone(), content: before.clone() });
    record.plan.planned_files.push(PlannedFile {
        writer: FileWriter::HostFile,
        action: FileAction::Move,
        asset_id: Some(owner.clone()),
        old_path: Some(old.clone()),
        new_path: Some(new.clone()),
        expected_old_hash: Some(before.hash.clone()),
        content: Some(after.clone()),
    });
    record.adapter_receipt.as_mut().unwrap()["resources"].as_array_mut().unwrap().push(serde_json::json!({"path":new,"diskHash":after.hash}));
    finish(root.path(), &record, &Default::default()).unwrap();
    let registry = registry(root.path()).unwrap();
    assert_eq!(registry.path_owners.get(&old), Some(&owner));
    assert_eq!(registry.path_owners.get(&new), Some(&owner));
    assert_eq!(registry.managed_files.get(&old), Some(&before));
    assert_eq!(registry.managed_files.get(&new), Some(&after));
    assert_eq!(disk::read_bounded(&disk::scoped_path(root.path(), old.as_str(), false).unwrap(), 1024).unwrap(), old_bytes);
}
