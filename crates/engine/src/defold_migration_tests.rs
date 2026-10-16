#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use photocraft_doc::{
    ColorMode, Document, Layer, LayerMask, SampleType, Size,
    defold::{Design, NodeLink},
};

fn id<T: StableIdentity>(n: u8) -> T {
    T::from_bytes([n; 16])
}
fn legacy(gui: &str) -> Document {
    let mut doc = Document::new("Preserved GUI", Size::new(16, 16), ColorMode::Rgb, SampleType::F32);
    let mut layer = Layer::raster("Painted", doc.pixel_format());
    layer.surface_mut().unwrap().write_pixel(2, 3, &[2.0, 0.25, 0.5, 0.75]);
    layer.mask = Some(LayerMask::reveal_all());
    let local = layer.id.0;
    doc.layers.push(layer);
    doc.defold = Some(Design {
        version: 3,
        root: "old-checkout".into(),
        gui: "old-checkout/ui/main.gui".into(),
        source: gui.into(),
        disk_hash: blake3::hash(gui.as_bytes()).to_hex().to_string(),
        nodes: vec![NodeLink { asset_id: "old-not-uuid".into(), layer: local, artwork: local, node: "root".into(), appearance: "old".into(), text: None }],
        remote: None,
        canvas_origin: [0, 0],
        project: None,
    });
    doc
}

#[test]
fn seven_legacy_forms_are_classified_without_claiming_current_source_revisions() {
    assert_eq!(classify("/ui/main.gui.pcraft"), Some(LegacyKind::GuiNative));
    assert_eq!(classify("/ui/main.gui.photocraft.json"), Some(LegacyKind::GuiSidecar));
    assert_eq!(classify("/ui/main.gui.photocraft-recovery.json"), Some(LegacyKind::GuiRecovery));
    assert_eq!(classify("/.photocraft/sources/abc.pcraft"), Some(LegacyKind::SharedNative));
    assert_eq!(classify("/photocraft_generated/menu/assets.json"), Some(LegacyKind::AssetRegistry));
    assert_eq!(classify("/images/a.photocraft-backup-123.png"), Some(LegacyKind::ImageBackup));
    assert_eq!(classify("/.photocraft/sources/uuid/revisions/r1/document.pcraft"), None);
}

#[test]
fn stable_identity_mapping_is_package_scoped_and_keeps_creative_layers_unchanged() {
    let original = legacy("nodes { id: \"root\" }");
    let context = ProjectContext { project_id: id(1), checkout_id: id(2), workspace_id: id(3), workspace_epoch: 1 };
    let scene = SceneIdentity { scene_id: id(4), path: ResourcePath::parse("/ui/main.gui").unwrap(), package_id: id(5), source_revision: None };
    let baseline = SceneBaseline {
        scene_id: scene.scene_id.clone(),
        path: scene.path.clone(),
        gui_source: original.defold.as_ref().unwrap().source.clone(),
        disk_hash: ContentHash::sha256(b"source"),
        scene_hash: ContentHash::sha256(b"source"),
        published_revision: None,
    };
    let mut a = original.clone();
    let mut b = original.clone();
    let mappings = attach_identities(&mut a, &context, &scene, &baseline).unwrap();
    attach_identities(&mut b, &context, &scene, &baseline).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.layers, original.layers);
    assert_eq!(mappings[0].ownership, AssetOwnership::External);
    let mut c = original;
    attach_identities(&mut c, &context, &SceneIdentity { package_id: id(6), ..scene }, &baseline).unwrap();
    assert_ne!(a.defold.as_ref().unwrap().project.as_ref().unwrap().layers, c.defold.as_ref().unwrap().project.as_ref().unwrap().layers);
}

#[cfg(not(target_arch = "wasm32"))]
fn fixture() -> (tempfile::TempDir, std::sync::Arc<crate::defold_project_local::LocalProjectStore>, NativeMigrationRequest, Vec<u8>) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("game.project"), b"[project]\ntitle = migration\n").unwrap();
    std::fs::create_dir(root.path().join("ui")).unwrap();
    let gui = "nodes { id: \"root\" type: TYPE_BOX }";
    std::fs::write(root.path().join("ui/main.gui"), gui).unwrap();
    let doc = legacy(gui);
    let bytes = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
    std::fs::write(root.path().join("ui/main.gui.pcraft"), &bytes).unwrap();
    std::fs::write(root.path().join("ui/main.gui.photocraft.json"), b"{\"unknownFuture\":[1,2,3]}").unwrap();
    let store = crate::defold_project_local::LocalProjectStore::open(root.path()).unwrap();
    let description = store.describe().unwrap();
    let scene = store
        .resolve_scene(&ResourceReadRequest { context: description.context.clone(), path: ResourcePath::parse("/ui/main.gui").unwrap(), expected_hash: None })
        .unwrap();
    let inventory = discover(root.path(), &BTreeMap::new(), &description.limits).unwrap();
    let request = NativeMigrationRequest {
        context: description.context,
        client_id: id(10),
        operation_id: id(11),
        scene,
        input: ResourcePath::parse("/ui/main.gui.pcraft").unwrap(),
        inventory,
        recovery: BTreeMap::new(),
    };
    (root, store, request, bytes)
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn raw_backup_and_native_import_are_idempotent_without_game_or_sidecar_changes() {
    let (root, store, request, original) = fixture();
    let before_gui = std::fs::read(root.path().join("ui/main.gui")).unwrap();
    let report = migrate_native(root.path(), store.as_ref(), &request).unwrap();
    assert_eq!(report.state, MigrationState::SourceCommitted, "{:?}", report.notes);
    assert_eq!(migrate_native(root.path(), store.as_ref(), &request).unwrap(), report);
    assert_eq!(std::fs::read(root.path().join("ui/main.gui.pcraft")).unwrap(), original);
    assert_eq!(std::fs::read(root.path().join("ui/main.gui")).unwrap(), before_gui);
    for entry in &request.inventory.entries {
        let backup = root.path().join(format!(".photocraft/migrations/{}/files/{}", request.operation_id, entry.content.hash.as_str().replace(':', "-")));
        entry.content.verify(&std::fs::read(backup).unwrap()).unwrap();
    }
    let restored = store.read_source(&ReadSourceRequest { context: request.context, package_id: request.scene.package_id, source_revision: None }).unwrap();
    let doc = photocraft_format::load_from_bytes(&restored.payload.native).unwrap();
    assert_eq!(doc.layers, photocraft_format::load_from_bytes(&original).unwrap().layers);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn changed_gui_retains_complete_legacy_container_without_fabricating_a_baseline() {
    let (root, store, request, original) = fixture();
    std::fs::write(root.path().join("ui/main.gui"), b"nodes { id: \"external\" }").unwrap();
    let report = migrate_native(root.path(), store.as_ref(), &request).unwrap();
    assert_eq!(report.state, MigrationState::NeedsReview);
    assert!(report.source.is_none());
    assert_eq!(std::fs::read(root.path().join("ui/main.gui.pcraft")).unwrap(), original);
    assert!(store.read_source(&ReadSourceRequest { context: request.context, package_id: request.scene.package_id, source_revision: None }).is_err());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn changed_inventory_input_aborts_before_acknowledging_backup() {
    let (root, _, request, _) = fixture();
    std::fs::write(root.path().join("ui/main.gui.pcraft"), b"modified after discovery").unwrap();
    assert!(backup(root.path(), &request.operation_id, &request.inventory, &BTreeMap::new()).is_err());
    assert!(!root.path().join(format!(".photocraft/migrations/{}/inventory.json", request.operation_id)).exists());
}
