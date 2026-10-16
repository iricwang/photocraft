#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::defold_publish_plan::{AssetRecord, AssetRegistry};
use photocraft_doc::{
    ColorMode, Document, Layer, LayerId, SampleType, Size,
    defold::{Design, NodeLink},
};

fn native(request: &mut SaveSourceRequest, shared: Option<SharedAppearanceRef>, asset: AssetId) -> (SourcePayload, ArtworkSourceNode) {
    let mut document = Document::new("Native", Size::new(8, 8), ColorMode::Rgb, SampleType::U8);
    let layer = Layer::raster("Artwork", document.pixel_format());
    let local = layer.id.0;
    document.layers.push(layer);
    let node = ArtworkSourceNode { scene_id: request.scene_id.clone().unwrap(), node_id: new_id().unwrap(), root_layer_id: new_id().unwrap() };
    let base = request.envelope.scene_bases[0].clone();
    let layers = BTreeMap::from([(
        local,
        LayerBinding {
            layer_id: node.root_layer_id.clone(),
            role: LayerRole::GuiNode,
            node_id: Some(node.node_id.clone()),
            asset_id: Some(asset),
            gui_node_id: Some("root".into()),
            appearance: Some(shared.clone().map_or(AppearanceSource::SelfLayer, |source| AppearanceSource::Shared { source })),
            artwork_fingerprint: None,
        },
    )]);
    request.envelope.identity_map = layers.iter().map(|(local, role)| (*local, role.layer_id.clone())).collect();
    request.envelope.draft.shared_sources =
        shared.map(|source| vec![SourceRevisionRef { package_id: source.package_id, source_revision: source.source_revision }]).unwrap_or_default();
    document.defold = Some(Design {
        version: 3,
        root: "project".into(),
        gui: format!("project{}", base.path),
        source: base.gui_source.clone(),
        disk_hash: "old".into(),
        nodes: vec![NodeLink { asset_id: "legacy".into(), layer: local, artwork: local, node: "root".into(), appearance: String::new(), text: None }],
        remote: None,
        canvas_origin: [0, 0],
        project: Some(ProjectLink {
            schema_version: 1,
            project_id: request.context.project_id.clone(),
            scene_id: node.scene_id.clone(),
            package_id: request.package_id.clone(),
            gui_path: base.path.to_string(),
            imported_gui_source: None,
            imported_disk_hash: None,
            layers,
            legacy_aliases: BTreeMap::new(),
        }),
    });
    let bytes = photocraft_format::save_to_bytes(&document, &Default::default()).unwrap();
    request.envelope.native = BlobDescriptor::of_bytes(&bytes);
    (SourcePayload { native: bytes, dependencies: BTreeMap::new() }, node)
}

fn fixture() -> (tempfile::TempDir, Arc<LocalProjectStore>, SaveSourceRequest, SourcePayload, SaveSourceRequest, SourcePayload, AssetRecord) {
    let (root, store, mut owner, _) = super::super::tests::fixture();
    let asset_id: AssetId = new_id().unwrap();
    let (owner_payload, node) = native(&mut owner, None, asset_id.clone());
    store.save_source(&owner, &owner_payload).unwrap();
    fs::copy(root.path().join("test.gui"), root.path().join("consumer.gui")).unwrap();
    let scene = store.resolve_scene_path(&ResourcePath::parse("/consumer.gui").unwrap()).unwrap();
    let mut consumer = owner.clone();
    consumer.operation_id = new_id().unwrap();
    consumer.scene_id = Some(scene.scene_id.clone());
    consumer.package_id = scene.package_id.clone();
    consumer.envelope.package_id = scene.package_id.clone();
    consumer.envelope.scene_bases[0].scene_id = scene.scene_id.clone();
    consumer.envelope.scene_bases[0].path = scene.path;
    consumer.leases = [LeaseResource::Package(scene.package_id), LeaseResource::Scene(scene.scene_id)]
        .into_iter()
        .map(|resource| {
            store
                .acquire_lease(&LeaseRequest { context: owner.context.clone(), client_id: owner.client_id.clone(), resource, duration_ms: 30_000 })
                .unwrap()
                .proof
        })
        .collect();
    let reference = SharedAppearanceRef {
        asset_id: asset_id.clone(),
        package_id: owner.package_id.clone(),
        source_revision: owner.envelope.source_revision.clone(),
        source_node: node.clone(),
    };
    let (consumer_payload, _) = native(&mut consumer, Some(reference), asset_id.clone());
    store.save_source(&consumer, &consumer_payload).unwrap();
    let image = ResourcePath::parse("/photocraft_generated/test/images/Artwork.png").unwrap();
    let asset = AssetRecord {
        asset_id: asset_id.clone(),
        display_name: "Artwork".into(),
        ownership: AssetOwnership::Managed,
        visibility: AssetVisibility::Private,
        state: AssetState::Active,
        source: Some(SourceRevisionRef { package_id: owner.package_id.clone(), source_revision: owner.envelope.source_revision.clone() }),
        source_node: Some(node),
        image: image.clone(),
        output: BlobDescriptor::of_bytes(b"PNG"),
        runtime_frame_id: crate::defold_publish_plan::runtime_frame_id(&asset_id),
        referencing_scenes: BTreeSet::from([owner.scene_id.clone().unwrap()]),
        output_revision: "published".into(),
        recipe: Some(ExportRecipe {
            width: 8,
            height: 8,
            crop_origin: [-4, -4],
            node_local_transform: [1., 0., 0., 1., -4., -4.],
            cross_node_composition: false,
        }),
    };
    let mut registry = AssetRegistry::default();
    registry.assets.insert(asset_id.clone(), asset.clone());
    registry.path_owners.insert(image.clone(), asset_id);
    registry.managed_files.insert(image, asset.output.clone());
    disk::write_json(
        &disk::scoped_path(root.path(), ".photocraft/assets/index.json", true).unwrap(),
        &serde_json::json!({"schemaVersion":1,"registry":registry,"latestByScene":{},"receipts":{}}),
    )
    .unwrap();
    (root, store, owner, owner_payload, consumer, consumer_payload, asset)
}

fn plan(store: &LocalProjectStore, owner: &SaveSourceRequest, consumer: &SaveSourceRequest, asset: &AssetRecord) -> PublishPlan {
    let source_revisions = [owner, consumer]
        .into_iter()
        .map(|request| SourceRevisionRef { package_id: request.package_id.clone(), source_revision: request.envelope.source_revision.clone() })
        .collect();
    let targets = [owner, consumer]
        .into_iter()
        .map(|request| {
            let base = &request.envelope.scene_bases[0];
            PublishTarget {
                scene_id: base.scene_id.clone(),
                path: base.path.clone(),
                expected_published_revision: None,
                expected_gui_disk_hash: base.disk_hash.clone(),
                expected_editor_snapshot: "snapshot".into(),
            }
        })
        .collect();
    let mut leases = owner.leases.clone();
    leases.extend(consumer.leases.clone());
    leases.push(
        store
            .acquire_lease(&LeaseRequest {
                context: owner.context.clone(),
                client_id: owner.client_id.clone(),
                resource: LeaseResource::Asset(asset.asset_id.clone()),
                duration_ms: 30_000,
            })
            .unwrap()
            .proof,
    );
    PublishPlan {
        schema_version: 1,
        transaction_id: new_id().unwrap(),
        context: owner.context.clone(),
        client_id: owner.client_id.clone(),
        primary_scene_id: owner.scene_id.clone().unwrap(),
        source_revisions,
        targets,
        leases,
        plan_hash: ContentHash::sha256(b""),
        planned_files: vec![],
        node_changes: vec![],
        asset_changes: vec![AssetChange {
            asset_id: asset.asset_id.clone(),
            display_name: asset.display_name.clone(),
            ownership: asset.ownership,
            visibility: asset.visibility,
            state: asset.state,
            source: asset.source.clone(),
            source_node: asset.source_node.clone(),
            image: Some(asset.image.clone()),
            runtime_frame_id: Some(asset.runtime_frame_id.clone()),
            referencing_scenes: vec![owner.scene_id.clone().unwrap(), consumer.scene_id.clone().unwrap()],
            recipe: asset.recipe.clone(),
        }],
    }
}

#[test]
fn usage_includes_unpublished_native_consumers_and_preserves_exact_pins() {
    let (_root, store, owner, _, consumer, _, asset) = fixture();
    let request = SharedAssetRequest { context: owner.context.clone(), asset_id: asset.asset_id.clone() };
    let usage = store.shared_asset_usage(&request).unwrap();
    assert_eq!(usage.consumers.len(), 2);
    assert!(usage.consumers.iter().any(|scene| Some(&scene.scene_id) == consumer.scene_id.as_ref()));
    let mut wrong = request;
    wrong.context.project_id = new_id().unwrap();
    assert_eq!(store.shared_asset_usage(&wrong).unwrap_err().code, ProjectErrorCode::ProjectMismatch);
    let identity = store.scene_identity(&SceneIdentityRequest { context: owner.context, scene_id: consumer.scene_id.unwrap() }).unwrap();
    assert_eq!(identity.source_revision.as_ref(), Some(&consumer.envelope.source_revision));
}

#[test]
fn host_requires_all_native_consumers_and_rejects_canonical_takeover() {
    let (_root, store, owner, _, consumer, _, asset) = fixture();
    let complete = plan(&store, &owner, &consumer, &asset);
    let state = store.state.lock().unwrap();
    validate_publication(&store, &state, &complete).unwrap();
    let mut missing = complete.clone();
    missing.targets.retain(|target| Some(&target.scene_id) != consumer.scene_id.as_ref());
    assert_eq!(validate_publication(&store, &state, &missing).unwrap_err().code, ProjectErrorCode::SourceConflict);
    let mut missing = complete.clone();
    missing.asset_changes[0].referencing_scenes.pop();
    assert_eq!(validate_publication(&store, &state, &missing).unwrap_err().code, ProjectErrorCode::InvalidRequest);
    let mut takeover = complete.clone();
    takeover.asset_changes[0].source_node = None;
    assert_eq!(validate_publication(&store, &state, &takeover).unwrap_err().code, ProjectErrorCode::TargetChanged);
    let mut takeover = complete;
    takeover.asset_changes[0].source_node.as_mut().unwrap().root_layer_id = new_id().unwrap();
    assert_eq!(validate_publication(&store, &state, &takeover).unwrap_err().code, ProjectErrorCode::TargetChanged);
}

#[test]
fn source_envelope_cannot_omit_shared_roots_and_pin_must_name_a_real_owner() {
    let (_root, store, owner, _, mut consumer, payload, asset) = fixture();
    consumer.operation_id = new_id().unwrap();
    consumer.expected_source_revision = Some(consumer.envelope.source_revision.clone());
    consumer.envelope.parent_source_revision = consumer.expected_source_revision.clone();
    consumer.envelope.source_revision = "consumer-2".into();
    consumer.envelope.draft.shared_sources.clear();
    assert_eq!(store.save_source(&consumer, &payload).unwrap_err().code, ProjectErrorCode::InvalidRequest);
    let mut document = photocraft_format::load_from_bytes(&payload.native).unwrap();
    let role = document.defold.as_mut().unwrap().project.as_mut().unwrap().layers.values_mut().next().unwrap();
    role.appearance = Some(AppearanceSource::SelfLayer);
    let bytes = photocraft_format::save_to_bytes(&document, &Default::default()).unwrap();
    consumer.envelope.native = BlobDescriptor::of_bytes(&bytes);
    store.save_source(&consumer, &SourcePayload { native: bytes, dependencies: BTreeMap::new() }).unwrap();
    assert_eq!(
        store.shared_asset_usage(&SharedAssetRequest { context: owner.context, asset_id: asset.asset_id }).unwrap_err().code,
        ProjectErrorCode::NameCollision
    );
}

#[test]
fn owner_new_revision_requires_consumer_pin_advance_but_not_new_canonical_identity() {
    let (_root, store, mut owner, owner_payload, mut consumer, consumer_payload, asset) = fixture();
    let mut publication = plan(&store, &owner, &consumer, &asset);
    owner.operation_id = new_id().unwrap();
    owner.expected_source_revision = Some(owner.envelope.source_revision.clone());
    owner.envelope.parent_source_revision = owner.expected_source_revision.clone();
    owner.envelope.source_revision = "owner-2".into();
    store.save_source(&owner, &owner_payload).unwrap();
    publication.source_revisions.iter_mut().find(|source| source.package_id == owner.package_id).unwrap().source_revision = "owner-2".into();
    publication.asset_changes[0].source.as_mut().unwrap().source_revision = "owner-2".into();
    {
        let state = store.state.lock().unwrap();
        assert_eq!(validate_publication(&store, &state, &publication).unwrap_err().code, ProjectErrorCode::SourceConflict);
    }
    consumer.operation_id = new_id().unwrap();
    consumer.expected_source_revision = Some(consumer.envelope.source_revision.clone());
    consumer.envelope.parent_source_revision = consumer.expected_source_revision.clone();
    consumer.envelope.source_revision = "consumer-2".into();
    consumer.envelope.draft.shared_sources[0].source_revision = "owner-2".into();
    let mut document = photocraft_format::load_from_bytes(&consumer_payload.native).unwrap();
    let local = *document.defold.as_ref().unwrap().project.as_ref().unwrap().layers.keys().next().unwrap();
    assert!(document.layer(LayerId(local)).is_some());
    if let Some(AppearanceSource::Shared { source }) =
        &mut document.defold.as_mut().unwrap().project.as_mut().unwrap().layers.get_mut(&local).unwrap().appearance
    {
        source.source_revision = "owner-2".into();
    }
    let bytes = photocraft_format::save_to_bytes(&document, &Default::default()).unwrap();
    consumer.envelope.native = BlobDescriptor::of_bytes(&bytes);
    store.save_source(&consumer, &SourcePayload { native: bytes, dependencies: BTreeMap::new() }).unwrap();
    publication.source_revisions.iter_mut().find(|source| source.package_id == consumer.package_id).unwrap().source_revision = "consumer-2".into();
    let state = store.state.lock().unwrap();
    validate_publication(&store, &state, &publication).unwrap();
}
