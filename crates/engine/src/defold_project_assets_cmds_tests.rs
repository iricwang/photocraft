#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::{defold_project_local::LocalProjectStore, defold_project_store::ProjectStore};
use photocraft_doc::{
    ColorMode, Document, Layer, LayerContent, LayerMask, SampleType, Size, SmartObject, SmartSource,
    defold::{Design, NodeLink},
};
use std::sync::Arc;

fn fixture(linked: bool) -> (tempfile::TempDir, Arc<LocalProjectStore>, Session, Document, Vec<u8>) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("game.project"), "[display]\nwidth=16\nheight=16\n").unwrap();
    let gui = "nodes { id: \"root\" type: TYPE_BOX size {x:16 y:16} position {x:8 y:8} }\n";
    std::fs::write(root.path().join("main.gui"), gui).unwrap();
    let mut original = Document::new("Legacy creative source", Size::new(16, 16), ColorMode::Rgb, SampleType::F32);
    let mut layer = Layer::raster("Painted", original.pixel_format());
    layer.surface_mut().unwrap().write_pixel(3, 4, &[2.0, 0.25, 0.75, 0.5]);
    layer.mask = Some(LayerMask::reveal_all());
    layer.locks.all = true;
    layer.visible = false;
    layer.effects.enabled = true;
    layer.effects.reference = Some((3.0, 4.0));
    layer.effects.psd_raw = Some(Arc::new(vec![1, 2, 3, 4]));
    let carrier = layer.id.0;
    original.layers.push(layer);
    let mut smart = Layer::raster("Preserved smart source", original.pixel_format());
    let source = if linked {
        SmartSource::Linked { path: "external-source.pcraft".into() }
    } else {
        SmartSource::Embedded { file_name: "source.bin".into(), bytes: Arc::new(vec![7, 8, 9]) }
    };
    smart.content = LayerContent::Smart(SmartObject::new(source, photocraft_geom::Affine::IDENTITY, None));
    original.layers.push(smart);
    original.ai_project.last_prompt = "Preserve the hand-painted reward".into();
    original.ai_project.record("generated", "Original generation provenance");
    original.defold = Some(Design {
        version: 3,
        root: root.path().to_string_lossy().into_owned(),
        gui: root.path().join("main.gui").to_string_lossy().into_owned(),
        source: gui.into(),
        disk_hash: blake3::hash(gui.as_bytes()).to_hex().to_string(),
        nodes: vec![NodeLink { asset_id: "old-node".into(), layer: carrier, artwork: carrier, node: "root".into(), appearance: "old".into(), text: None }],
        remote: None,
        canvas_origin: [0, 0],
        project: None,
    });
    let native = photocraft_format::save_to_bytes(&original, &Default::default()).unwrap();
    std::fs::write(root.path().join("main.gui.pcraft"), &native).unwrap();
    std::fs::write(root.path().join("main.gui.photocraft.json"), b"{\"future\":true}").unwrap();
    let store = LocalProjectStore::open(root.path()).unwrap();
    let authority: Arc<dyn ProjectStore> = store.clone();
    client::register_store(root.path(), &authority).unwrap();
    let mut document = original.clone();
    let binding = client::bind_new_document(&mut document).unwrap();
    let mut session = Session::new();
    session.add_document(document, None);
    session.project_client.bindings.insert(session.active().unwrap().open_id, binding);
    (root, store, session, original, native)
}
fn reviewed(session: &mut Session) -> Value {
    session.execute("defold.refreshAssetManagement", json!({})).unwrap()["snapshot"].clone()
}
fn migration_params(snapshot: &Value) -> Value {
    json!({"input":"/main.gui.pcraft","inventoryHash":snapshot["legacyInventory"]["inventoryHash"],"operationId":OperationId::from_bytes([71;16])})
}

#[test]
fn asset_commands_without_a_linked_opening_fail_without_panicking() {
    let mut session = Session::new();
    assert_eq!(session.execute("defold.assetManagementStatus", json!({})).unwrap()["available"], false);
    assert!(session.execute("defold.refreshAssetManagement", json!({})).is_err());
    assert!(session.execute("defold.migrateLegacySource", json!({})).is_err());
}

#[test]
fn inventory_command_is_read_only_and_caches_by_opening() {
    let (root, _, mut session, _, native) = fixture(false);
    let gui_before = std::fs::read(root.path().join("main.gui")).unwrap();
    let snapshot = reviewed(&mut session);
    assert_eq!(snapshot["rootsComplete"], false);
    assert!(!snapshot["warnings"].as_array().unwrap().is_empty());
    assert!(snapshot["legacyInventory"]["entries"].as_array().unwrap().iter().any(|entry| entry["path"] == "/main.gui.pcraft"));
    assert_eq!(session.execute("defold.assetManagementStatus", json!({})).unwrap()["available"], true);
    assert_eq!(std::fs::read(root.path().join("main.gui.pcraft")).unwrap(), native);
    assert_eq!(std::fs::read(root.path().join("main.gui")).unwrap(), gui_before);
    assert!(!root.path().join("photocraft_generated").exists());
    assert!(!root.path().join(".photocraft/migrations").exists());
    let old = session.active().unwrap().open_id;
    let binding = session.project_client.bindings[&old].clone();
    let document = session.active().unwrap().doc.as_ref().clone();
    session.add_document(document, None);
    session.project_client.bindings.insert(session.active().unwrap().open_id, binding);
    assert_eq!(session.execute("defold.assetManagementStatus", json!({})).unwrap()["available"], false);
}

#[test]
fn explicit_migration_command_preserves_rich_native_source_and_original_bytes() {
    let (root, store, mut session, original, native) = fixture(false);
    let snapshot = reviewed(&mut session);
    let params = migration_params(&snapshot);
    let result = session.execute("defold.migrateLegacySource", params.clone()).unwrap();
    assert_eq!(result["migration"]["state"], "sourceCommitted", "{result}");
    assert_eq!(result["reopenRequired"], true);
    let binding = &session.project_client.bindings[&session.active().unwrap().open_id];
    assert!(binding.source_revision.is_none(), "The old open draft must not adopt the new source head");
    let source = store
        .read_source(&ReadSourceRequest { context: binding.context.clone(), package_id: binding.scene.package_id.clone(), source_revision: None })
        .unwrap();
    let restored = photocraft_format::load_from_bytes(&source.payload.native).unwrap();
    assert_eq!(restored.layers, original.layers);
    assert_eq!(restored.ai_project, original.ai_project);
    assert_eq!(std::fs::read(root.path().join("main.gui.pcraft")).unwrap(), native);
    assert_eq!(std::fs::read_to_string(root.path().join("main.gui")).unwrap(), original.defold.as_ref().unwrap().source);
    let inventory: crate::defold_migration::MigrationInventory = serde_json::from_value(snapshot["legacyInventory"].clone()).unwrap();
    let operation = params["operationId"].as_str().unwrap().to_string();
    for entry in inventory.entries {
        let path = root.path().join(format!(".photocraft/migrations/{operation}/files/{}", entry.content.hash.as_str().replace(':', "-")));
        entry.content.verify(&std::fs::read(path).unwrap()).unwrap();
    }
    let repeated = session.execute("defold.migrateLegacySource", params).unwrap();
    assert_eq!(repeated["migration"], result["migration"]);
    let status = session.execute("defold.migrationStatus", json!({"operationId":operation})).unwrap();
    assert_eq!(status["migration"], result["migration"]);
}

#[test]
fn migration_command_rejects_hidden_inputs_and_changed_inventory_before_backup() {
    let (root, _, mut session, _, _) = fixture(false);
    std::fs::create_dir(root.path().join(".private")).unwrap();
    std::fs::write(root.path().join(".private/secret.gui.pcraft"), b"private").unwrap();
    let snapshot = reviewed(&mut session);
    assert!(!snapshot["legacyInventory"]["entries"].as_array().unwrap().iter().any(|entry| entry["path"].as_str().unwrap().contains(".private")));
    let mut hidden = migration_params(&snapshot);
    hidden["input"] = json!("/.private/secret.gui.pcraft");
    assert!(session.execute("defold.migrateLegacySource", hidden).is_err());
    assert!(!root.path().join(".photocraft/migrations").exists());
    std::fs::write(root.path().join("main.gui.photocraft.json"), b"changed").unwrap();
    assert!(session.execute("defold.migrateLegacySource", migration_params(&snapshot)).is_err());
    assert!(!root.path().join(".photocraft/migrations").exists());
}

#[test]
fn conflicting_gui_or_linked_dependency_stays_backed_up_for_review() {
    for linked in [false, true] {
        let (root, store, mut session, _, native) = fixture(linked);
        if !linked {
            std::fs::write(root.path().join("main.gui"), b"nodes {id: \"outside\"}").unwrap();
        }
        let snapshot = reviewed(&mut session);
        let result = session.execute("defold.migrateLegacySource", migration_params(&snapshot)).unwrap();
        assert_eq!(result["migration"]["state"], "needsReview", "{result}");
        assert_eq!(result["reopenRequired"], false);
        let binding = &session.project_client.bindings[&session.active().unwrap().open_id];
        assert!(
            store
                .read_source(&ReadSourceRequest { context: binding.context.clone(), package_id: binding.scene.package_id.clone(), source_revision: None })
                .is_err()
        );
        assert_eq!(std::fs::read(root.path().join("main.gui.pcraft")).unwrap(), native);
        let hash = BlobDescriptor::of_bytes(&native).hash.to_string().replace(':', "-");
        let backup = root.path().join(format!(".photocraft/migrations/{}/files/{hash}", OperationId::from_bytes([71; 16])));
        assert_eq!(std::fs::read(backup).unwrap(), native);
    }
}

#[test]
fn same_migration_operation_cannot_be_rebound_to_another_selected_input() {
    let (_root, store, mut session, _, _) = fixture(false);
    let snapshot = reviewed(&mut session);
    let params = migration_params(&snapshot);
    session.execute("defold.migrateLegacySource", params.clone()).unwrap();
    let mut rebound = params.clone();
    rebound["input"] = json!("/another.gui.pcraft");
    let failure = session.execute("defold.migrateLegacySource", rebound).unwrap_err();
    assert!(failure.to_string().contains("IdempotencyMismatch"), "{failure}");
    let binding = &session.project_client.bindings[&session.active().unwrap().open_id];
    let request = LegacyMigrationRequest {
        context: binding.context.clone(),
        client_id: binding.client_id.clone(),
        operation_id: OperationId::from_bytes([71; 16]),
        scene: binding.scene.clone(),
        input: ResourcePath::parse("/another.gui.pcraft").unwrap(),
        inventory_hash: ContentHash::parse(params["inventoryHash"].as_str().unwrap()).unwrap(),
    };
    assert_eq!(store.migrate_legacy(&request).unwrap_err().code, ProjectErrorCode::IdempotencyMismatch);
}

#[test]
fn migration_uses_recovered_save_head_and_preserves_only_the_matching_open_draft() {
    let (_root, _store, session, _, _) = fixture(false);
    let mut current = session.project_client.bindings[&session.active().unwrap().open_id].clone();
    let expected_head = current.source_revision.clone();
    let mut recovered = current.clone();
    recovered.source_revision = Some("acknowledged-save-head".into());
    recovered.last_saved_native_revision = 27;
    // The scene descriptor was captured before the lost save ACK; the runtime head is authoritative.
    assert_eq!(recovered.scene.source_revision, expected_head);
    let operation = OperationId::from_bytes([72; 16]);
    let request = migration_request(
        &recovered,
        operation.clone(),
        ResourcePath::parse("/main.gui.pcraft").unwrap(),
        BlobDescriptor::of_bytes(b"reviewed inventory").hash,
    );
    assert_eq!(request.operation_id, operation);
    assert_eq!(request.scene.source_revision, recovered.source_revision);
    restore_migration_binding(&mut current, recovered.clone(), &expected_head);
    assert_eq!(current.source_revision.as_deref(), Some("acknowledged-save-head"));
    assert_eq!(current.last_saved_native_revision, 27);

    current.source_revision = Some("newer-open-draft-head".into());
    restore_migration_binding(&mut current, recovered.clone(), &expected_head);
    assert_eq!(current.source_revision.as_deref(), Some("newer-open-draft-head"));
    current.source_revision = expected_head.clone();
    current.scene.scene_id = SceneId::from_bytes([73; 16]);
    restore_migration_binding(&mut current, recovered, &expected_head);
    assert_eq!(current.source_revision, expected_head);
    assert_eq!(current.scene.scene_id, SceneId::from_bytes([73; 16]));
}
