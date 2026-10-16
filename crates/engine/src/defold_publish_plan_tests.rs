#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;

fn id<T: StableIdentity>(n: u8) -> T {
    T::from_bytes([n; 16])
}
fn desired(id_seed: u8, name: &str) -> FrozenAsset {
    let bytes = b"frozen-png-fixture".to_vec();
    FrozenAsset {
        asset_id: id(id_seed),
        display_name: name.into(),
        source: SourceRevisionRef { package_id: id(2), source_revision: "source-1".into() },
        source_node: None,
        reference_only: false,
        referencing_scenes: BTreeSet::from([id(1)]),
        output: BlobDescriptor::of_bytes(&bytes),
        bytes: Some(bytes),
        recipe: ExportRecipe { width: 32, height: 32, crop_origin: [-16, -16], node_local_transform: [1., 0., 0., 1., 0., 0.], cross_node_composition: false },
    }
}
fn input(desired: Vec<FrozenAsset>) -> AssetPlanInput {
    AssetPlanInput {
        folder: ResourcePath::parse("/photocraft_generated/challenge").unwrap(),
        atlas: ResourcePath::parse("/photocraft_generated/challenge/atlases/challenge.atlas").unwrap(),
        output_revision: "publish-1".into(),
        targets: BTreeSet::from([id(1)]),
        reference_scope: BTreeSet::from([id(1)]),
        registry: AssetRegistry::default(),
        desired,
        resources: BTreeMap::new(),
        bodies: BTreeMap::new(),
        protected_frames: vec![],
    }
}
fn next(plan: &AssetPlan, desired: Vec<FrozenAsset>) -> AssetPlanInput {
    let mut input = input(desired);
    input.registry = plan.registry.clone();
    input.resources = plan.registry.managed_files.clone();
    input.resources.extend(plan.registry.assets.values().map(|asset| (asset.image.clone(), asset.output.clone())));
    for file in &plan.files {
        if let (Some(path), Some(content)) = (&file.new_path, &file.content) {
            input.resources.insert(path.clone(), content.clone());
        }
    }
    input.bodies = plan.payload.contents.clone();
    input.output_revision = "publish-2".into();
    input
}

#[test]
fn unchanged_publish_reuses_images_and_atlas_without_payload() {
    let first = plan_assets(input(vec![desired(3, "Title")])).unwrap();
    let mut asset = desired(3, "Title");
    asset.bytes = None;
    let second = plan_assets(next(&first, vec![asset])).unwrap();
    assert!(second.files.is_empty());
    assert!(second.payload.contents.is_empty());
    assert!(second.changes.is_empty());
}

#[test]
fn rename_updates_same_asset_and_keeps_runtime_frame() {
    let first = plan_assets(input(vec![desired(3, "Title")])).unwrap();
    let second = plan_assets(next(&first, vec![desired(3, "Challenge title")])).unwrap();
    let before = first.registry.assets.get(&id(3)).unwrap();
    let after = second.registry.assets.get(&id(3)).unwrap();
    assert_eq!(before.asset_id, after.asset_id);
    assert_eq!(before.runtime_frame_id, after.runtime_frame_id);
    assert!(after.image.as_str().ends_with("Challenge title.png"));
    assert_eq!(second.files.iter().filter(|file| file.action == FileAction::Move).count(), 1);
    assert_eq!(second.registry.assets.len(), 1);
}

#[test]
fn rename_undo_redo_reuses_two_owned_paths_with_both_hash_preconditions() {
    let mut plan = plan_assets(input(vec![desired(3, "A")])).unwrap();
    let asset_id: AssetId = id(3);
    let frame = plan.registry.assets[&asset_id].runtime_frame_id.clone();
    for (iteration, name) in ["B", "A", "B", "A", "B"].into_iter().enumerate() {
        let previous = plan.registry.assets[&asset_id].clone();
        let destination = ResourcePath::parse(&format!("/photocraft_generated/challenge/images/{name}.png")).unwrap();
        let historical = plan.registry.managed_files.get(&destination).cloned();
        let mut artwork = desired(3, name);
        // The historical destination can contain an older pixel version. The replacement
        // checks that version separately from the current image's retirement precondition.
        artwork.bytes = Some(format!("pixels-{iteration}").into_bytes());
        artwork.output = BlobDescriptor::of_bytes(artwork.bytes.as_ref().unwrap());
        plan = plan_assets(next(&plan, vec![artwork])).unwrap();
        let current = &plan.registry.assets[&asset_id];
        assert_eq!(current.image, destination);
        assert_eq!(current.runtime_frame_id, frame);
        assert_eq!(plan.registry.assets.len(), 1);
        if let Some(historical) = historical {
            let replace = plan.files.iter().find(|file| file.new_path.as_ref() == Some(&destination)).unwrap();
            assert_eq!(replace.action, FileAction::Replace);
            assert_eq!(replace.old_path.as_ref(), Some(&destination));
            assert_eq!(replace.expected_old_hash.as_ref(), Some(&historical.hash));
            let retire = plan.files.iter().find(|file| file.action == FileAction::Remove).unwrap();
            assert_eq!(retire.old_path.as_ref(), Some(&previous.image));
            assert_eq!(retire.expected_old_hash.as_ref(), Some(&previous.output.hash));
            assert!(retire.new_path.is_none() && retire.content.is_none());
            assert!(!plan.files.iter().any(|file| file.action == FileAction::Move));
        }
        assert!(plan.registry.path_owners.keys().all(|path| path.as_str().ends_with("/A.png") || path.as_str().ends_with("/B.png")));
        assert!(plan.registry.path_owners.values().all(|owner| owner == &asset_id));
        assert_eq!(plan.registry.path_owners.len(), 2);
    }
}

#[test]
fn historical_path_requires_owner_and_exact_captured_checksum() {
    let a = plan_assets(input(vec![desired(3, "A")])).unwrap();
    let b = plan_assets(next(&a, vec![desired(3, "B")])).unwrap();
    let old = a.registry.assets[&id(3)].image.clone();
    let mut changed = next(&b, vec![desired(3, "A")]);
    changed.resources.insert(old.clone(), BlobDescriptor::of_bytes(b"external change"));
    assert_eq!(plan_assets(changed).unwrap_err().code, ProjectErrorCode::TargetChanged);
    let mut foreign = next(&b, vec![desired(3, "A")]);
    foreign.registry.path_owners.insert(old.clone(), id(9));
    let renamed = plan_assets(foreign).unwrap();
    assert!(renamed.registry.assets[&id(3)].image.as_str().contains("/A__"));
    assert!(renamed.files.iter().all(|file| file.new_path.as_ref() != Some(&old)));
    let mut unclaimed = next(&b, vec![desired(3, "A")]);
    unclaimed.registry.path_owners.remove(&old);
    let renamed = plan_assets(unclaimed).unwrap();
    assert!(renamed.registry.assets[&id(3)].image.as_str().contains("/A__"));
}

#[test]
fn duplicate_display_names_get_stable_collision_suffixes() {
    let a = plan_assets(input(vec![desired(3, "icon"), desired(4, "ICON")])).unwrap();
    let b = plan_assets(input(vec![desired(4, "ICON"), desired(3, "icon")])).unwrap();
    assert_eq!(a, b);
    let paths: BTreeSet<_> = a.registry.assets.values().map(|record| record.image.case_key()).collect();
    assert_eq!(paths.len(), 2);
    assert!(paths.iter().all(|path| path.contains("__")));
}

#[test]
fn delete_only_detaches_and_keeps_recoverable_asset_record() {
    let first = plan_assets(input(vec![desired(3, "Title")])).unwrap();
    let removed = plan_assets(next(&first, vec![])).unwrap();
    assert_eq!(removed.registry.assets.get(&id(3)).unwrap().state, AssetState::Detached);
    assert!(removed.files.iter().all(|file| file.action != FileAction::Remove));
    assert!(!removed.retained.is_empty());
    let restored = plan_assets(next(&removed, vec![desired(3, "Title")])).unwrap();
    assert_eq!(restored.registry.assets.get(&id(3)).unwrap().state, AssetState::Active);
    assert!(restored.files.iter().all(|file| file.writer != FileWriter::HostFile));
}

#[test]
fn public_path_and_frame_do_not_follow_display_name() {
    let mut first = plan_assets(input(vec![desired(3, "Title")])).unwrap();
    let record = first.registry.assets.get_mut(&id(3)).unwrap();
    record.visibility = AssetVisibility::Public;
    record.runtime_frame_id = "legacy_api".into();
    let second = plan_assets(next(&first, vec![desired(3, "New title")])).unwrap();
    let record = second.registry.assets.get(&id(3)).unwrap();
    assert!(record.image.as_str().ends_with("Title.png"));
    assert_eq!(record.runtime_frame_id, "legacy_api");
    assert!(!second.retained.is_empty());
}

#[test]
fn shared_pixel_change_requires_every_affected_gui_target() {
    let mut first = plan_assets(input(vec![desired(3, "Title")])).unwrap();
    first.registry.assets.get_mut(&id(3)).unwrap().referencing_scenes.insert(id(9));
    let mut asset = desired(3, "Title");
    asset.bytes = Some(b"new-pixels".to_vec());
    asset.output = BlobDescriptor::of_bytes(asset.bytes.as_ref().unwrap());
    asset.referencing_scenes.insert(id(9));
    assert_eq!(plan_assets(next(&first, vec![asset])).unwrap_err().code, ProjectErrorCode::InvalidRequest);
}

#[test]
fn external_changes_and_cross_node_composition_are_rejected_before_writes() {
    let mut first = plan_assets(input(vec![desired(3, "Title")])).unwrap();
    first.registry.assets.get_mut(&id(3)).unwrap().ownership = AssetOwnership::External;
    let mut asset = desired(3, "Title");
    asset.bytes = Some(b"new-pixels".to_vec());
    asset.output = BlobDescriptor::of_bytes(asset.bytes.as_ref().unwrap());
    assert!(plan_assets(next(&first, vec![asset])).is_err());
    let mut asset = desired(3, "Title");
    asset.recipe.cross_node_composition = true;
    assert_eq!(plan_assets(input(vec![asset])).unwrap_err().code, ProjectErrorCode::UnsupportedGuiFeature);
}

#[test]
fn garbage_collection_needs_complete_roots_tombstone_backup_and_exact_bytes() {
    let first = plan_assets(input(vec![desired(3, "Title")])).unwrap();
    let mut asset = first.registry.assets.get(&id(3)).unwrap().clone();
    asset.state = AssetState::Tombstoned;
    asset.referencing_scenes.clear();
    let make = || CollectionInput {
        asset: asset.clone(),
        roots: BTreeSet::new(),
        roots_complete: true,
        current: Some(asset.output.clone()),
        grace_period_elapsed: true,
        verified_backup: true,
    };
    assert!(plan_collection(make()).removal.is_some());
    for root in [
        RetentionReason::EditorHistory,
        RetentionReason::UnsavedEditor,
        RetentionReason::SourceVersion,
        RetentionReason::Publication,
        RetentionReason::AiResult,
        RetentionReason::Transaction,
    ] {
        let mut input = make();
        input.roots.insert(root.clone());
        let decision = plan_collection(input);
        assert!(decision.removal.is_none());
        assert!(decision.reasons.contains(&root));
    }
    let mut input = make();
    input.roots_complete = false;
    assert!(plan_collection(input).reasons.contains(&RetentionReason::UnknownRoots));
    let mut input = make();
    input.current = Some(BlobDescriptor::of_bytes(b"external-edit"));
    assert!(plan_collection(input).reasons.contains(&RetentionReason::ChangedOnDisk));
}

fn native_bridge_fixture() -> (crate::defold_design::PublicationPlan, PublicationBridgeInput) {
    use photocraft_doc::{
        ColorMode, Document, Layer, SampleType, Size,
        defold::{Design, NodeLink},
    };
    let mut doc = Document::new("GUI", Size::new(32, 32), ColorMode::Rgb, SampleType::U8);
    let layer = Layer::raster("Readable Title", doc.pixel_format());
    let local_id = layer.id.0;
    doc.layers.push(layer);
    let scene = photocraft_defold_format::Scene::parse("nodes { id: \"root\" type: TYPE_BOX size { x: 2 y: 2 } }".into()).unwrap();
    let link = NodeLink { asset_id: "legacy-identity".into(), layer: local_id, artwork: local_id, node: "root".into(), appearance: "old".into(), text: None };
    doc.defold = Some(Design {
        version: 3,
        root: "missing-project".into(),
        gui: "missing-project/challenge.gui".into(),
        source: scene.proto.source.clone(),
        disk_hash: "legacy".into(),
        nodes: vec![link.clone()],
        remote: None,
        canvas_origin: [0, 0],
        project: Some(ProjectLink {
            schema_version: 1,
            project_id: id(10),
            scene_id: id(1),
            package_id: id(2),
            gui_path: "/challenge.gui".into(),
            imported_gui_source: None,
            imported_disk_hash: None,
            layers: BTreeMap::from([(
                local_id,
                LayerBinding {
                    layer_id: id(7),
                    role: LayerRole::GuiNode,
                    node_id: Some(id(8)),
                    asset_id: Some(id(3)),
                    gui_node_id: Some("root".into()),
                    appearance: Some(AppearanceSource::SelfLayer),
                    artwork_fingerprint: None,
                },
            )]),
            legacy_aliases: BTreeMap::new(),
        }),
    });
    let png = photocraft_codecs::encode(
        &photocraft_codecs::Image::from_u8(2, 2, photocraft_codecs::ChannelLayout::Rgba, vec![255; 16]).unwrap(),
        photocraft_codecs::Format::Png,
        &Default::default(),
    )
    .unwrap();
    let old_png = "/photocraft_generated/legacy/images/old.png".to_string();
    let native = crate::defold_design::PublicationPlan {
        document: doc,
        authored: scene.clone(),
        output: scene.clone(),
        links: vec![link],
        folder: "/photocraft_generated/legacy".into(),
        files: vec![(old_png.clone(), png.clone())],
        textures: vec![],
        asset_nodes: vec![],
        registry: crate::defold_assets::Registry {
            assets: vec![crate::defold_assets::Asset {
                key: "legacy-identity".into(),
                node: "root".into(),
                layer: local_id,
                image: old_png,
                hash: crate::defold_assets::digest(&png),
                name: "Readable Title".into(),
            }],
            ..Default::default()
        },
        renames: vec![],
        recipes: BTreeMap::from([(
            "root".into(),
            ExportRecipe { width: 2, height: 2, crop_origin: [-1, -1], node_local_transform: [1.0, 0.0, 0.0, 1.0, -1.0, -1.0], cross_node_composition: false },
        )]),
    };
    let expected = ContentHash::sha256(scene.proto.source.as_bytes());
    let input = PublicationBridgeInput {
        context: ProjectContext { project_id: id(10), checkout_id: id(11), workspace_id: id(12), workspace_epoch: 1 },
        client_id: id(13),
        transaction_id: id(14),
        source_revisions: vec![SourceRevisionRef { package_id: id(2), source_revision: "source-1".into() }],
        targets: vec![PublishTarget {
            scene_id: id(1),
            path: ResourcePath::parse("/challenge.gui").unwrap(),
            expected_published_revision: None,
            expected_gui_disk_hash: expected,
            expected_editor_snapshot: "graph-1".into(),
        }],
        leases: [LeaseResource::Scene(id(1)), LeaseResource::Package(id(2)), LeaseResource::Asset(id(3))]
            .into_iter()
            .map(|resource| LeaseProof { resource, lease_epoch: 1 })
            .collect(),
        folder: ResourcePath::parse("/photocraft_generated/challenge").unwrap(),
        atlas: ResourcePath::parse("/photocraft_generated/challenge/atlases/challenge.atlas").unwrap(),
        registry: AssetRegistry::default(),
        resources: BTreeMap::new(),
        protected_frames: vec![],
    };
    (native, input)
}

#[test]
fn native_bridge_emits_typed_gui_png_atlas_without_old_sidecar_or_node_rename() {
    let (native, input) = native_bridge_fixture();
    let output = bridge_publication(native, input).unwrap();
    output.plan.verify_payload(&output.payload, &Default::default()).unwrap();
    assert_eq!(output.plan.node_changes[0].nodes[0].id, "root");
    assert!(output.plan.node_changes[0].renames.is_empty());
    assert_eq!(output.plan.planned_files.len(), 3);
    assert!(
        output
            .plan
            .planned_files
            .iter()
            .all(|file| !file.new_path.as_ref().is_some_and(|path| path.as_str().ends_with("assets.json") || path.as_str().ends_with(".pcraft")))
    );
    assert_eq!(output.registry.assets.get(&id(3)).unwrap().recipe.as_ref().unwrap().width, 2);
    assert_eq!(output.registry.assets.get(&id(3)).unwrap().recipe.as_ref().unwrap().crop_origin, [-1, -1]);
}

fn shared_batch_fixture() -> PublicationBatchInput {
    let (owner, input) = native_bridge_fixture();
    let source_node = ArtworkSourceNode { scene_id: id(1), node_id: id(8), root_layer_id: id(7) };
    let (mut consumer, _) = native_bridge_fixture();
    let local = consumer.links[0].layer;
    let design = consumer.document.defold.as_mut().unwrap();
    design.gui = "missing-project/consumer.gui".into();
    let project = design.project.as_mut().unwrap();
    project.scene_id = id(20);
    project.package_id = id(21);
    project.gui_path = "/consumer.gui".into();
    let role = project.layers.get_mut(&local).unwrap();
    role.node_id = Some(id(22));
    role.layer_id = id(23);
    role.appearance =
        Some(AppearanceSource::Shared { source: SharedAppearanceRef { asset_id: id(3), package_id: id(2), source_revision: "source-1".into(), source_node } });
    consumer.document.layer_mut(photocraft_doc::LayerId(local)).unwrap().name = "Consumer label must not rename owner".into();
    // Proxy pixels are deliberately invalid and its crop differs. A correct consumer never reads them.
    consumer.files[0].1 = b"invalid consumer PNG".to_vec();
    consumer.recipes.clear();
    let owner_target = input.targets[0].clone();
    let mut consumer_target = owner_target.clone();
    consumer_target.scene_id = id(20);
    consumer_target.path = ResourcePath::parse("/consumer.gui").unwrap();
    let mut sources = input.source_revisions;
    sources.push(SourceRevisionRef { package_id: id(21), source_revision: "consumer-1".into() });
    let mut leases = input.leases;
    leases.extend([LeaseResource::Scene(id(20)), LeaseResource::Package(id(21))].into_iter().map(|resource| LeaseProof { resource, lease_epoch: 1 }));
    PublicationBatchInput {
        context: input.context,
        client_id: input.client_id,
        transaction_id: input.transaction_id,
        source_revisions: sources,
        leases,
        registry: input.registry,
        resources: input.resources,
        // Consumer first proves the union planner resolves dependencies itself.
        scenes: vec![
            PublicationSceneInput {
                native: consumer,
                target: consumer_target,
                folder: ResourcePath::parse("/photocraft_generated/consumer").unwrap(),
                atlas: ResourcePath::parse("/photocraft_generated/consumer/atlases/consumer.atlas").unwrap(),
                protected_frames: vec![],
            },
            PublicationSceneInput { native: owner, target: owner_target, folder: input.folder, atlas: input.atlas, protected_frames: vec![] },
        ],
    }
}

#[test]
fn shared_batch_has_one_authoritative_png_two_atlases_and_preserves_owner_geometry() {
    let output = bridge_publication_batch(shared_batch_fixture()).unwrap();
    output.plan.verify_payload(&output.payload, &Default::default()).unwrap();
    let asset = &output.registry.assets[&id(3)];
    assert_eq!(asset.display_name, "Readable Title");
    assert_eq!(asset.source.as_ref().unwrap().package_id, id(2));
    assert_eq!(asset.referencing_scenes, BTreeSet::from([id(1), id(20)]));
    assert_eq!(output.plan.asset_changes.len(), 1);
    assert_eq!(output.plan.planned_files.iter().filter(|file| file.asset_id.is_some()).count(), 1);
    assert_eq!(output.plan.planned_files.iter().filter(|file| file.new_path.as_ref().unwrap().as_str().ends_with(".atlas")).count(), 2);
    let consumer = output.plan.node_changes.iter().find(|scene| scene.scene_id == id(20)).unwrap();
    assert_eq!(consumer.nodes[0].values["manual_size"][0], serde_json::json!(2.0));
    assert!(consumer.nodes[0].values["texture"].as_str().unwrap().ends_with(&asset.runtime_frame_id));
}

#[test]
fn shared_batch_rejects_missing_owner_wrong_pin_and_multiple_canonical_owners() {
    let mut input = shared_batch_fixture();
    input.scenes.pop();
    input.source_revisions.retain(|source| source.package_id != id(2));
    assert!(bridge_publication_batch(input).is_err());
    let mut input = shared_batch_fixture();
    let role = input.scenes[0].native.document.defold.as_mut().unwrap().project.as_mut().unwrap().layers.values_mut().next().unwrap();
    if let Some(AppearanceSource::Shared { source }) = &mut role.appearance {
        source.source_revision = "stale".into();
    }
    assert!(bridge_publication_batch(input).is_err());
    let mut input = shared_batch_fixture();
    let role = input.scenes[0].native.document.defold.as_mut().unwrap().project.as_mut().unwrap().layers.values_mut().next().unwrap();
    role.appearance = Some(AppearanceSource::SelfLayer);
    // Consumer cannot promote its borrowed AssetId to a second editable master.
    assert!(bridge_publication_batch(input).is_err());
}

#[test]
fn per_scene_pruning_keeps_other_targets_and_shared_consumers_cannot_rename_or_replace() {
    let first = plan_assets(input(vec![desired(3, "Owner")])).unwrap();
    let mut reference = desired(3, "Owner");
    reference.reference_only = true;
    reference.bytes = None;
    reference.referencing_scenes.insert(id(20));
    let mut consumer = next(&first, vec![reference.clone()]);
    consumer.targets.insert(id(20));
    consumer.reference_scope = BTreeSet::from([id(20)]);
    consumer.folder = ResourcePath::parse("/photocraft_generated/consumer").unwrap();
    consumer.atlas = ResourcePath::parse("/photocraft_generated/consumer/atlases/a.atlas").unwrap();
    let combined = plan_assets(consumer).unwrap();
    assert_eq!(combined.registry.assets[&id(3)].image, first.registry.assets[&id(3)].image);
    let mut removed = next(&combined, vec![]);
    removed.targets.insert(id(20));
    removed.reference_scope = BTreeSet::from([id(20)]);
    let detached = plan_assets(removed).unwrap();
    assert_eq!(detached.registry.assets[&id(3)].referencing_scenes, BTreeSet::from([id(1)]));
    reference.display_name = "Steal owner path".into();
    let mut forged = next(&combined, vec![reference]);
    forged.targets.insert(id(20));
    assert!(plan_assets(forged).is_err());
}
