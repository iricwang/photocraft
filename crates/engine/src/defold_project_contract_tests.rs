#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use photocraft_doc::{ColorMode, Document, SampleType, Size};
use std::collections::BTreeSet;

fn id<T: StableIdentity>(seed: u8) -> T {
    T::from_bytes([seed; 16])
}
fn context() -> ProjectContext {
    ProjectContext { project_id: id(1), checkout_id: id(2), workspace_id: id(3), workspace_epoch: 1 }
}
fn store() -> FakeStore {
    FakeStore::new(ProjectDescription {
        schema_version: 1,
        context: context(),
        display_name: "Fixture".into(),
        capabilities: BTreeSet::new(),
        editor: Some(EditorDescriptor { editor_session_id: id(4), capabilities: BTreeSet::new() }),
        limits: ProjectLimits::default(),
    })
}
fn source(store: &FakeStore) -> (SaveSourceRequest, SourcePayload) {
    let package: PackageId = id(5);
    let client: ClientId = id(6);
    let lease = store
        .acquire_lease(&LeaseRequest { context: context(), client_id: client.clone(), resource: LeaseResource::Package(package.clone()), duration_ms: 10_000 })
        .unwrap();
    let doc = Document::new("Creative", Size::new(16, 16), ColorMode::Rgb, SampleType::F32);
    let native = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
    let envelope = SourceEnvelope {
        schema_version: 1,
        package_id: package.clone(),
        source_revision: "source-1".into(),
        parent_source_revision: None,
        draft_revision: "draft-1".into(),
        scene_bases: vec![],
        identity_map: BTreeMap::new(),
        native: BlobDescriptor::of_bytes(&native),
        dependencies: vec![],
        draft: DraftSnapshot::default(),
    };
    (
        SaveSourceRequest {
            schema_version: 1,
            operation_id: id(7),
            context: context(),
            client_id: client,
            scene_id: None,
            package_id: package,
            expected_source_revision: None,
            leases: vec![lease.proof],
            captured_native_revision: 12,
            envelope,
        },
        SourcePayload { native, dependencies: BTreeMap::new() },
    )
}
fn next(request: &SaveSourceRequest) -> SaveSourceRequest {
    let mut next = request.clone();
    next.operation_id = id(8);
    next.expected_source_revision = Some(request.envelope.source_revision.clone());
    next.envelope.parent_source_revision = next.expected_source_revision.clone();
    next.envelope.source_revision = "source-2".into();
    next.envelope.draft_revision = "draft-2".into();
    next
}

#[test]
fn resource_paths_cannot_escape_or_use_windows_aliases() {
    for invalid in ["../gui", "/../game.project", "/a/./b", "C:/gui", "/a\\b", "//host/a", "/NUL.png", "/a.", "/a ", "/a:b", "/"] {
        assert!(ResourcePath::parse(invalid).is_err(), "{invalid}");
    }
    assert_eq!(ResourcePath::parse("/ui/角色.gui").unwrap().as_str(), "/ui/角色.gui");
}

#[test]
fn control_decoder_rejects_wrong_types_unknown_fields_and_oversized_requests() {
    let store = store();
    let (request, _) = source(&store);
    let mut value = serde_json::to_value(&request).unwrap();
    value["capturedNativeRevision"] = serde_json::json!("12");
    assert!(decode_control::<SaveSourceRequest>(&serde_json::to_vec(&value).unwrap(), &Default::default()).is_err());
    value = serde_json::to_value(&request).unwrap();
    value["eval"] = serde_json::json!("anything");
    assert!(decode_control::<SaveSourceRequest>(&serde_json::to_vec(&value).unwrap(), &Default::default()).is_err());
    assert_eq!(
        decode_control::<SaveSourceRequest>(b"{}", &ProjectLimits { max_control_bytes: 1, ..Default::default() }).unwrap_err().code,
        ProjectErrorCode::LimitExceeded
    );
}

#[test]
fn source_save_is_cas_idempotent_and_keeps_native_bytes() {
    let store = store();
    let (request, payload) = source(&store);
    let receipt = store.save_source(&request, &payload).unwrap();
    store.advance_time(20_000);
    assert_eq!(store.save_source(&request, &payload).unwrap(), receipt);
    let loaded = store.read_source(&ReadSourceRequest { context: context(), package_id: request.package_id.clone(), source_revision: None }).unwrap();
    assert_eq!(loaded.payload, payload);
    let mut reused = request.clone();
    reused.captured_native_revision += 1;
    assert_eq!(store.save_source(&reused, &payload).unwrap_err().code, ProjectErrorCode::IdempotencyMismatch);
    assert_eq!(store.save_source(&next(&request), &payload).unwrap_err().code, ProjectErrorCode::LeaseLost);
}

#[test]
fn source_head_and_payload_failures_never_replace_committed_source() {
    let store = store();
    let (request, payload) = source(&store);
    store.save_source(&request, &payload).unwrap();
    let mut stale = next(&request);
    stale.expected_source_revision = None;
    stale.envelope.parent_source_revision = None;
    assert_eq!(store.save_source(&stale, &payload).unwrap_err().code, ProjectErrorCode::SourceConflict);
    let mut corrupted = payload.clone();
    corrupted.native.push(0);
    assert_eq!(store.save_source(&next(&request), &corrupted).unwrap_err().code, ProjectErrorCode::HashMismatch);
    let mut unknown = next(&request);
    unknown.schema_version = 2;
    assert_eq!(store.save_source(&unknown, &payload).unwrap_err().code, ProjectErrorCode::ProtocolUnsupported);
    assert_eq!(
        store.read_source(&ReadSourceRequest { context: context(), package_id: request.package_id.clone(), source_revision: None }).unwrap().envelope,
        request.envelope
    );
}

#[test]
fn lost_save_response_is_recovered_by_operation_status_after_restart() {
    let store = store();
    let (request, payload) = source(&store);
    store.inject_failure(FailurePoint::SourceAfterCommit);
    assert_eq!(store.save_source(&request, &payload).unwrap_err().code, ProjectErrorCode::Io);
    store.restart();
    let receipt = store.operation_status(&OperationRequest { context: context(), operation_id: request.operation_id.clone() }).unwrap().unwrap();
    assert_eq!(store.save_source(&request, &payload).unwrap(), receipt);
    let old_epoch = request.leases[0].lease_epoch;
    let lease = store
        .acquire_lease(&LeaseRequest {
            context: context(),
            client_id: request.client_id.clone(),
            resource: request.leases[0].resource.clone(),
            duration_ms: 10_000,
        })
        .unwrap();
    assert!(lease.proof.lease_epoch > old_epoch);
}

#[test]
fn failures_before_source_commit_leave_no_head_or_receipt() {
    for checkpoint in [FailurePoint::SourceBeforeWrite, FailurePoint::SourceAfterObjects] {
        let store = store();
        let (request, payload) = source(&store);
        store.inject_failure(checkpoint);
        assert!(store.save_source(&request, &payload).is_err());
        assert!(store.operation_status(&OperationRequest { context: context(), operation_id: request.operation_id.clone() }).unwrap().is_none());
        assert!(store.read_source(&ReadSourceRequest { context: context(), package_id: request.package_id.clone(), source_revision: None }).is_err());
        assert!(store.save_source(&request, &payload).is_ok());
    }
}

#[test]
fn workspace_change_fences_stale_source_and_leases() {
    let store = store();
    let (request, payload) = source(&store);
    let mut moved = context();
    moved.workspace_epoch += 1;
    store.set_workspace(moved);
    assert_eq!(store.save_source(&request, &payload).unwrap_err().code, ProjectErrorCode::WorkspaceChanged);
}

#[test]
fn resolve_uses_complete_gui_path_and_only_registers_metadata() {
    let store = store();
    for path in ["/old/challenge.gui", "/new/challenge.gui"] {
        let path = ResourcePath::parse(path).unwrap();
        let bytes = b"nodes {}".to_vec();
        store.seed_resource(ResourceContent { version: ResourceVersion { path, content: BlobDescriptor::of_bytes(&bytes) }, bytes });
    }
    let request = ResourceReadRequest { context: context(), path: ResourcePath::parse("/old/challenge.gui").unwrap(), expected_hash: None };
    let first = store.resolve_scene(&request).unwrap();
    assert_eq!(first, store.resolve_scene(&request).unwrap());
    let second = store.resolve_scene(&ResourceReadRequest { path: ResourcePath::parse("/new/challenge.gui").unwrap(), ..request.clone() }).unwrap();
    assert_ne!(first.scene_id, second.scene_id);
    assert_eq!(store.read_resource(&request).unwrap().bytes, b"nodes {}");
}

#[test]
fn euler_only_and_acyclic_node_hierarchy_is_required() {
    let mut update = SceneUpdate {
        scene_id: id(1),
        nodes: vec![GuiNodeData {
            id: "root".into(),
            parent: "".into(),
            kind: "box".into(),
            values: BTreeMap::from([("rotation".into(), serde_json::json!([0, 0, 45]))]),
        }],
        textures: vec![],
        renames: vec![],
    };
    update.validate(&Default::default()).unwrap();
    update.nodes[0].values.insert("rotation".into(), serde_json::json!([0, 0, 0, 1]));
    assert_eq!(update.validate(&Default::default()).unwrap_err().code, ProjectErrorCode::InvalidTransform);
    update.nodes[0].values.clear();
    update.nodes[0].parent = "root".into();
    assert_eq!(update.validate(&Default::default()).unwrap_err().code, ProjectErrorCode::InvalidRequest);
}

#[test]
fn writer_boundary_includes_case_insensitive_resource_extensions() {
    let file = PlannedFile {
        writer: FileWriter::HostFile,
        action: FileAction::Create,
        asset_id: None,
        old_path: None,
        new_path: Some(ResourcePath::parse("/ui/New.GUI").unwrap()),
        expected_old_hash: None,
        content: Some(BlobDescriptor::of_bytes(b"")),
    };
    assert!(file.validate().is_err());
}

#[test]
fn phase_machine_cannot_claim_commit_without_apply_and_save() {
    assert!(!PublishPhase::Prepared.allows(PublishPhase::Committed));
    assert!(!PublishPhase::Applying.may_cancel());
    assert!(!PublishPhase::RecoveryRequired.is_terminal());
    assert!(!PublishPhase::Committed.allows(PublishPhase::Prepared));
}

#[test]
fn protocol_fixture_hash_and_roundtrip_are_stable() {
    let bytes = include_bytes!("../tests/fixtures/project_v1/publish-multi-target.json");
    let plan: PublishPlan = decode_control(bytes, &Default::default()).unwrap();
    plan.validate(&Default::default()).unwrap();
    let encoded = serde_json::to_vec(&plan).unwrap();
    assert_eq!(serde_json::from_slice::<PublishPlan>(&encoded).unwrap(), plan);
    let fixture: SourceEnvelope = decode_control(include_bytes!("../tests/fixtures/project_v1/source-envelope.json"), &Default::default()).unwrap();
    fixture.validate(&Default::default()).unwrap();
}
