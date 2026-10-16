mod common;

use photocraft_doc::{ColorMode, SampleType, defold::*};
use std::collections::BTreeMap;

fn linked_document() -> photocraft_doc::Document {
    let mut doc = common::rich_doc(ColorMode::Rgb, SampleType::U16);
    let layer = doc.layers[0].id.0;
    let stable = StableLayerId::from_bytes([4; 16]);
    doc.defold = Some(Design {
        version: 3,
        root: "legacy-root".into(),
        gui: "legacy.gui".into(),
        source: "nodes {}".into(),
        disk_hash: "legacy-hash".into(),
        remote: None,
        canvas_origin: [0, 0],
        nodes: vec![NodeLink { asset_id: "legacy".into(), layer, artwork: layer, node: "title".into(), appearance: "pixels".into(), text: None }],
        project: Some(ProjectLink {
            schema_version: 1,
            project_id: ProjectId::from_bytes([1; 16]),
            scene_id: SceneId::from_bytes([2; 16]),
            package_id: PackageId::from_bytes([3; 16]),
            gui_path: "/ui/title.gui".into(),
            imported_gui_source: None,
            imported_disk_hash: None,
            layers: BTreeMap::from([(
                layer,
                LayerBinding {
                    layer_id: stable,
                    role: LayerRole::GuiNode,
                    node_id: Some(NodeId::from_bytes([5; 16])),
                    asset_id: Some(AssetId::from_bytes([6; 16])),
                    gui_node_id: Some("title".into()),
                    appearance: Some(AppearanceSource::SelfLayer),
                    artwork_fingerprint: Some("content-only".into()),
                },
            )]),
            legacy_aliases: BTreeMap::from([("old-node".into(), "title".into())]),
        }),
    });
    doc
}

#[test]
fn project_attachment_roundtrips_without_changing_creative_content() {
    let doc = linked_document();
    let bytes = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
    let loaded = photocraft_format::load_from_bytes(&bytes).unwrap();
    assert_eq!(doc, loaded);
}

#[test]
fn native_id_remapping_preserves_project_ids_and_relinks_native_layers() {
    let doc = linked_document();
    let bytes = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
    let loaded = photocraft_format::load_from_bytes_with(&bytes, &photocraft_format::LoadOptions { preserve_ids: false, ..Default::default() }).unwrap();
    let original = doc.defold.as_ref().unwrap();
    let link = loaded.defold.as_ref().unwrap();
    assert_ne!(loaded.layers[0].id, doc.layers[0].id);
    assert_eq!(link.nodes[0].layer, loaded.layers[0].id.0);
    assert_eq!(link.nodes[0].artwork, loaded.layers[0].id.0);
    let project = link.project.as_ref().unwrap();
    assert_eq!(project.project_id, original.project.as_ref().unwrap().project_id);
    assert_eq!(project.layers[&loaded.layers[0].id.0], original.project.as_ref().unwrap().layers[&doc.layers[0].id.0]);
}

#[test]
fn unsupported_project_attachment_fails_instead_of_dropping_unknown_semantics() {
    let mut doc = linked_document();
    doc.defold.as_mut().unwrap().project.as_mut().unwrap().schema_version = 2;
    let bytes = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
    assert!(photocraft_format::load_from_bytes(&bytes).is_err());
}

#[test]
fn legacy_attachment_still_roundtrips_without_project_metadata() {
    let mut doc = linked_document();
    doc.defold.as_mut().unwrap().project = None;
    let bytes = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
    assert_eq!(doc, photocraft_format::load_from_bytes(&bytes).unwrap());
}
