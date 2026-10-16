//! Opt-in test against an actual, patched Defold editor and an explicitly disposable project.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use photocraft_engine::{Session, defold_project_client, defold_project_model::*};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn committed(session: &mut Session, mut result: Value) -> Value {
    for _ in 0..200 {
        match result["publication"]["phase"].as_str() {
            Some("COMMITTED") => return result,
            Some("ABORTED" | "RECOVERY_REQUIRED") => panic!("Defold did not commit: {result}"),
            _ => {
                std::thread::sleep(std::time::Duration::from_millis(30));
                result = session.execute("defold.refreshPublication", json!({})).unwrap();
            }
        }
    }
    panic!("Defold publication timed out: {result}");
}

fn publish_reviewed(session: &mut Session) -> Value {
    let review = session.execute("defold.reviewSharedPublication", json!({})).unwrap();
    let params = review.get("sharedReviewId").map_or_else(|| json!({}), |id| json!({"sharedReviewId":id}));
    session.execute("defold.syncDesign", params).unwrap()
}
#[test]
#[ignore = "requires a live patched Defold editor and a disposable project marker"]
fn native_layer_source_publish_rename_delete_and_reopen_against_real_defold() {
    let gui = PathBuf::from(std::env::var("PHOTOCRAFT_DEFOLD_TEST_GUI").expect("set PHOTOCRAFT_DEFOLD_TEST_GUI to the disposable project's GUI"));
    let root = gui.ancestors().find(|path| path.join("game.project").is_file()).expect("project root");
    assert!(root.join(".internal/photocraft/test-project").is_file(), "Refusing to modify a project without the explicit disposable-test marker");
    let mut session = Session::new();
    session.execute("defold.openDesign", json!({"path":gui})).unwrap();
    // Consume a durable adapter receipt after an interrupted prior fixture run.
    // This queries the original transaction instead of silently starting over.
    session.execute("defold.refreshPublication", json!({})).unwrap();
    let original = std::fs::read(&gui).unwrap();
    let parent = session.active().unwrap().doc.defold.as_ref().unwrap().nodes.first().unwrap().clone();
    let scene = photocraft_defold_format::Scene::parse(String::from_utf8(original.clone()).unwrap()).unwrap();
    let sibling = scene.nodes.iter().find(|node| node.parent == parent.node).unwrap();
    let sibling_layer = session.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|node| node.node == sibling.id).unwrap().layer;
    // Native New Layer inserts above the selected sibling. Keep that behavior
    // while exercising insertion into the GUI parent's actual child list.
    session.execute("layer.select", json!({"layer":sibling_layer})).unwrap();
    let layer = session.execute("layer.new.layer", json!({"name":"CraftLiveIcon"})).unwrap()["layer"].as_u64().unwrap();
    let size = session.active().unwrap().doc.size;
    session.execute("paint.stroke", json!({"points":[[size.width/2,size.height/2]],"size":18,"color":"#e64868"})).unwrap();
    let source = session.execute("defold.saveDesignSource", json!({})).unwrap();
    assert_eq!(source["saved"], true);
    assert_eq!(std::fs::read(&gui).unwrap(), original, "source save must not write the runtime GUI");
    let result = session.execute("defold.syncDesign", json!({})).unwrap();
    let first = committed(&mut session, result);
    assert_eq!(first["synced"], true);
    let design = session.active().unwrap().doc.defold.as_ref().unwrap();
    let project = design.project.as_ref().unwrap();
    let asset_id = project.layers[&layer].asset_id.clone().unwrap();
    let node = design.nodes.iter().find(|node| node.layer == layer).unwrap().node.clone();
    let store = defold_project_client::store_for(&root.to_string_lossy()).unwrap();
    let description = store.describe().unwrap();
    let registry = store.read_asset_registry(&description.context).unwrap();
    let image = registry.assets[&asset_id].image.clone();
    assert!(Path::new(image.as_str()).file_name().unwrap().to_string_lossy().starts_with("CraftLiveIcon"));
    let disk = photocraft_defold_format::Scene::parse(std::fs::read_to_string(&gui).unwrap()).unwrap();
    let published = disk.nodes.iter().find(|candidate| candidate.id == node).unwrap();
    assert!(!published.texture.is_empty());
    assert_eq!(published.parent, parent.node, "a native layer inserted in a GUI node must publish under that node");
    if std::env::var_os("PHOTOCRAFT_DEFOLD_TEST_VISUAL_REVIEW").is_some() {
        println!("VISUAL_REVIEW_READY node={node} texture={} receipt={}", published.texture, first["publication"]["transactionId"]);
        // Opt-in pause gives the editor-side reviewer time to inspect the actual
        // texture graph and canvas before the destructive part of this fixture.
        std::thread::sleep(std::time::Duration::from_secs(40));
    }
    let count = registry.assets.len();
    session.execute("layer.renameLayer", json!({"layer":layer,"name":"CraftLiveRenamed"})).unwrap();
    let result = session.execute("defold.syncDesign", json!({})).unwrap();
    committed(&mut session, result);
    let registry = store.read_asset_registry(&description.context).unwrap();
    assert_eq!(registry.assets.len(), count, "rename must preserve the asset identity");
    assert_eq!(registry.assets[&asset_id].display_name, "CraftLiveRenamed");
    let renamed = registry.assets[&asset_id].image.clone();
    assert_ne!(renamed, image);
    assert!(root.join(image.as_str().trim_start_matches('/')).is_file(), "old path remains available to Defold's Undo");
    // Renaming back through native history must reuse both owned paths, not
    // create suffixed copies on each subsequent publication.
    let owned_paths = registry.path_owners.clone();
    for _ in 0..2 {
        for (command, expected) in [("edit.undo", &image), ("edit.redo", &renamed)] {
            session.execute(command, json!({})).unwrap();
            let result = session.execute("defold.syncDesign", json!({})).unwrap();
            committed(&mut session, result);
            let registry = store.read_asset_registry(&description.context).unwrap();
            assert_eq!(&registry.assets[&asset_id].image, expected);
            assert_eq!(registry.path_owners, owned_paths, "history must not grow runtime asset paths");
        }
    }
    session.execute("layer.select", json!({"layer":layer})).unwrap();
    session.execute("layer.delete", json!({})).unwrap();
    let result = session.execute("defold.syncDesign", json!({})).unwrap();
    committed(&mut session, result);
    let disk = photocraft_defold_format::Scene::parse(std::fs::read_to_string(&gui).unwrap()).unwrap();
    assert!(!disk.nodes.iter().any(|candidate| candidate.id == node));
    let registry = store.read_asset_registry(&description.context).unwrap();
    assert_ne!(registry.assets[&asset_id].state, AssetState::Active);
    session.execute("edit.undo", json!({})).unwrap();
    let result = session.execute("defold.syncDesign", json!({})).unwrap();
    committed(&mut session, result);
    let restored = photocraft_defold_format::Scene::parse(std::fs::read_to_string(&gui).unwrap()).unwrap();
    assert!(restored.nodes.iter().any(|candidate| candidate.id == node), "Undo deletion must restore the same node and asset");
    assert_eq!(store.read_asset_registry(&description.context).unwrap().assets[&asset_id].image, renamed);
    session.execute("edit.redo", json!({})).unwrap();
    let result = session.execute("defold.syncDesign", json!({})).unwrap();
    committed(&mut session, result);
    let mut reopened = Session::new();
    reopened.execute("defold.openDesign", json!({"path":gui})).unwrap();
    assert!(!reopened.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().any(|candidate| candidate.node == node));
    println!("real Defold source/publish/rename/delete/reopen receipt: {}", first["publication"]["transactionId"]);
}

#[test]
#[ignore = "requires two actual GUI resources in a live disposable Defold project"]
fn shared_native_artwork_updates_two_real_guis_and_can_be_made_independent() {
    let owner_gui = PathBuf::from(std::env::var("PHOTOCRAFT_DEFOLD_TEST_GUI").expect("owner GUI fixture"));
    let consumer_gui = PathBuf::from(std::env::var("PHOTOCRAFT_DEFOLD_TEST_OTHER_GUI").expect("consumer GUI fixture"));
    let root = owner_gui.ancestors().find(|path| path.join("game.project").is_file()).unwrap();
    assert!(root.join(".internal/photocraft/test-project").is_file(), "Disposable test marker is required");
    assert!(consumer_gui.starts_with(root) && owner_gui != consumer_gui);
    let mut session = Session::new();
    session.execute("defold.openDesign", json!({"path":owner_gui})).unwrap();
    session.execute("defold.refreshPublication", json!({})).unwrap();
    let root_layer = session.active().unwrap().doc.defold.as_ref().unwrap().nodes[0].layer;
    session.execute("layer.select", json!({"layer":root_layer})).unwrap();
    let owner_layer = session.execute("layer.new.layer", json!({"name":"CraftLiveShared"})).unwrap()["layer"].as_u64().unwrap();
    let size = session.active().unwrap().doc.size;
    session.execute("paint.stroke", json!({"points":[[size.width/2,size.height/2]],"size":18,"color":"#e64868"})).unwrap();
    let first = publish_reviewed(&mut session);
    committed(&mut session, first);
    let shared = session.execute("defold.shareNodeArtwork", json!({})).unwrap();
    let asset: AssetId = serde_json::from_value(shared["sharedAssetId"].clone()).unwrap();
    session.execute("defold.saveDesignSource", json!({})).unwrap();
    session.execute("defold.openDesign", json!({"path":consumer_gui})).unwrap();
    session.execute("defold.refreshPublication", json!({})).unwrap();
    let consumer_index = session.active_index().unwrap();
    let root_layer = session.active().unwrap().doc.defold.as_ref().unwrap().nodes[0].layer;
    session.execute("layer.select", json!({"layer":root_layer})).unwrap();
    let consumer_layer = session.execute("layer.new.layer", json!({"name":"CraftLiveSharedConsumer"})).unwrap()["layer"].as_u64().unwrap();
    session.execute("defold.attachSharedArtwork", json!({"assetId":asset})).unwrap();
    assert!(session.execute("paint.stroke", json!({"points":[[20,20]],"size":8,"color":"#00ff00"})).is_err());
    let old_owner = std::fs::read(&owner_gui).unwrap();
    let old_consumer = std::fs::read(&consumer_gui).unwrap();
    session.execute("defold.saveDesignSource", json!({})).unwrap();
    assert_eq!(std::fs::read(&owner_gui).unwrap(), old_owner);
    assert_eq!(std::fs::read(&consumer_gui).unwrap(), old_consumer);
    let result = publish_reviewed(&mut session);
    let result = committed(&mut session, result);
    assert_eq!(result["publication"]["targets"].as_array().unwrap().len(), 2);
    let store = defold_project_client::store_for(&root.to_string_lossy()).unwrap();
    let context = store.describe().unwrap().context;
    let before = store.read_asset_registry(&context).unwrap().assets[&asset].clone();
    assert_eq!(before.referencing_scenes.len(), 2);
    session.set_active(0);
    session.execute("layer.select", json!({"layer":owner_layer})).unwrap();
    session.execute("paint.stroke", json!({"points":[[size.width/2+10,size.height/2]],"size":12,"color":"#198de6"})).unwrap();
    let result = publish_reviewed(&mut session);
    let result = committed(&mut session, result);
    assert_eq!(result["publication"]["targets"].as_array().unwrap().len(), 2);
    let changed = store.read_asset_registry(&context).unwrap().assets[&asset].clone();
    assert_ne!(changed.output.hash, before.output.hash);
    assert_eq!(changed.image, before.image);
    session.execute("layer.renameLayer", json!({"layer":owner_layer,"name":"CraftLiveSharedRenamed"})).unwrap();
    let result = publish_reviewed(&mut session);
    committed(&mut session, result);
    let renamed = store.read_asset_registry(&context).unwrap().assets[&asset].clone();
    assert_ne!(renamed.image, changed.image);
    assert_eq!(renamed.runtime_frame_id, changed.runtime_frame_id);
    for path in [&owner_gui, &consumer_gui] {
        let scene = photocraft_defold_format::Scene::parse(std::fs::read_to_string(path).unwrap()).unwrap();
        let mut found = false;
        for texture in scene.proto.fields.iter().filter(|field| field.name == "textures") {
            let texture_path = scene.proto.text(&texture.children, "texture");
            if !texture_path.ends_with("/scene.atlas") {
                continue;
            }
            let atlas = root.join(texture_path.trim_start_matches('/'));
            let frames = photocraft_engine::defold_atlas::decode(&std::fs::read_to_string(atlas).unwrap()).unwrap();
            found |= frames.iter().any(|frame| frame.id == renamed.runtime_frame_id && frame.image == renamed.image);
        }
        assert!(found, "Both real GUI atlases must reference the same renamed canonical image");
    }
    session.set_active(consumer_index);
    session.execute("layer.select", json!({"layer":consumer_layer})).unwrap();
    let independent = session.execute("defold.makeArtworkIndependent", json!({})).unwrap();
    assert_ne!(independent["independentAssetId"], json!(asset));
    let size = session.active().unwrap().doc.size;
    session.execute("paint.stroke", json!({"points":[[size.width/2,size.height/2]],"size":8,"color":"#00ff00"})).unwrap();
    let result = publish_reviewed(&mut session);
    committed(&mut session, result);
    let independent_registry = store.read_asset_registry(&context).unwrap();
    assert_eq!(independent_registry.assets[&asset].output.hash, renamed.output.hash);
    assert_eq!(independent_registry.assets[&asset].referencing_scenes.len(), 1);
    for (index, layer) in [(consumer_index, consumer_layer), (0, owner_layer)] {
        session.set_active(index);
        session.execute("layer.select", json!({"layer":layer})).unwrap();
        session.execute("layer.delete", json!({})).unwrap();
        let result = publish_reviewed(&mut session);
        committed(&mut session, result);
    }
}

#[test]
#[ignore = "stops only the authenticated host for a marked disposable fixture"]
fn stop_only_the_disposable_fixture_project_host() {
    use photocraft_engine::{defold_project_local, defold_project_net, defold_project_remote::RemoteProjectStore, defold_project_store::ProjectStore};
    let gui = PathBuf::from(std::env::var("PHOTOCRAFT_DEFOLD_TEST_GUI").expect("disposable GUI fixture"));
    let root = gui.ancestors().find(|path| path.join("game.project").is_file()).unwrap().canonicalize().unwrap();
    assert!(root.join(".internal/photocraft/test-project").is_file(), "Disposable marker is required");
    let ticket_path = defold_project_local::host_ticket_path(&root).unwrap();
    if !ticket_path.exists() {
        return;
    }
    let ticket: defold_project_net::HostTicket = serde_json::from_slice(&std::fs::read(ticket_path).unwrap()).unwrap();
    assert!(ticket.address.ip().is_loopback());
    let project: Value = serde_json::from_slice(&std::fs::read(root.join(".photocraft/project.json")).unwrap()).unwrap();
    let description = RemoteProjectStore::new(ticket.clone()).describe().unwrap();
    assert_eq!(json!(description.context.project_id), project["projectId"]);
    defold_project_net::call(&ticket, "/v1/host/shutdown", b"{}").unwrap();
}
