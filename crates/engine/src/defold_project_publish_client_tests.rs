#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use crate::defold_project_store::ProjectStore;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

struct Editor {
    stop: Arc<AtomicBool>,
    dirty: Arc<AtomicBool>,
    drop_apply: Arc<AtomicBool>,
    restarted: Arc<AtomicBool>,
    hold_apply: Arc<AtomicBool>,
    apply_count: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Editor {
    fn start(root: &Path) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let editor_id = EditorSessionId::from_bytes([9; 16]);
        std::fs::create_dir_all(root.join(".internal/photocraft")).unwrap();
        std::fs::write(root.join(".internal/editor.token"), "fixture-editor-token").unwrap();
        std::fs::write(
            root.join(".internal/photocraft/editor-adapter.json"),
            serde_json::to_vec(&json!({"schemaVersion":1,"port":listener.local_addr().unwrap().port(),"editorSessionId":editor_id})).unwrap(),
        )
        .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let dirty = Arc::new(AtomicBool::new(false));
        let drop_apply = Arc::new(AtomicBool::new(false));
        let restarted = Arc::new(AtomicBool::new(false));
        let normalized = restarted.clone();
        let hold_apply = Arc::new(AtomicBool::new(false));
        let hold = hold_apply.clone();
        let apply_count = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(vec![]));
        let (st, d, drop_reply, count, seen) = (stop.clone(), dirty.clone(), drop_apply.clone(), apply_count.clone(), requests.clone());
        let root = root.to_path_buf();
        let worker = thread::spawn(move || {
            let mut states = BTreeMap::<String, Value>::new();
            while !st.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                let Some((route, body)) = request(&mut stream) else { continue };
                seen.lock().unwrap().push((route.clone(), body.clone()));
                let editor_id = if normalized.load(Ordering::Acquire) { EditorSessionId::from_bytes([10; 16]) } else { editor_id.clone() };
                let mut response = match route.as_str() {
                    "/craft/v1/project" => {
                        json!({"schemaVersion":1,"editorSessionId":editor_id,"capabilities":{"targetedSave":true,"publishReceipts":true,"rotationEulerV1":true,"safeResourceRefresh":true,"managedAtlasGraphUpdates":true}})
                    }
                    "/craft/v1/snapshot" => {
                        let path = body["path"].as_str().unwrap();
                        let source = std::fs::read_to_string(root.join(path.trim_start_matches('/'))).unwrap();
                        let hash = ContentHash::sha256(source.as_bytes());
                        let source = if normalized.load(Ordering::Acquire) { restart_serialization(&source) } else { source };
                        let graph_hash = ContentHash::sha256(source.as_bytes());
                        json!({"path":path,"source":source,"diskHash":hash,"sceneHash":graph_hash,"editorGraphRevision":graph_hash.to_string(),"dirty":d.load(Ordering::Acquire),"editorSessionId":editor_id})
                    }
                    "/craft/v1/publish/prepare" => {
                        let tx = body["transactionId"].as_str().unwrap().to_owned();
                        assert!(!body["targets"][0]["changes"]["nodes"].is_null());
                        let value = json!({"state":"PREPARED"});
                        states.insert(tx, value.clone());
                        value
                    }
                    "/craft/v1/publish/apply" => {
                        count.fetch_add(1, Ordering::AcqRel);
                        for _ in 0..5000 {
                            if !hold.load(Ordering::Acquire) || st.load(Ordering::Acquire) {
                                break;
                            }
                            thread::sleep(Duration::from_millis(2));
                        }
                        let tx = body["transactionId"].as_str().unwrap();
                        let value = apply(&root, tx);
                        states.insert(tx.into(), value.clone());
                        if drop_reply.swap(false, Ordering::AcqRel) {
                            continue;
                        }
                        value
                    }
                    "/craft/v1/publish/cancel" => {
                        let value = json!({"state":"ABORTED"});
                        states.insert(body["transactionId"].as_str().unwrap().into(), value.clone());
                        value
                    }
                    "/craft/v1/publish/status" => {
                        states.get(body["transactionId"].as_str().unwrap()).cloned().unwrap_or_else(|| json!({"state":"RECOVERY_REQUIRED"}))
                    }
                    _ => panic!("Unexpected editor route {route}"),
                };
                if route.starts_with("/craft/v1/publish/") {
                    response["schemaVersion"] = json!(1);
                    response["transactionId"] = body["transactionId"].clone();
                    response["planHash"] = body["planHash"].clone();
                }
                let bytes = serde_json::to_vec(&response).unwrap();
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len());
                let _ = stream.write_all(&bytes);
            }
        });
        Self { stop, dirty, drop_apply, restarted, hold_apply, apply_count, requests, worker: Some(worker) }
    }
}
impl Drop for Editor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

fn restart_serialization(source: &str) -> String {
    let scene = photocraft_defold_format::Scene::parse(source.into()).unwrap();
    let mut spans = Vec::new();
    for node in scene.proto.fields.iter().filter(|field| field.name == "nodes") {
        for vector in node.children.iter().filter(|field| matches!(field.name.as_str(), "position" | "rotation" | "scale")) {
            for component in &vector.children {
                let default = if vector.name == "scale" || component.name == "w" { 1.0 } else { 0.0 };
                if source[component.value.clone()].parse::<f64>().ok() == Some(default) {
                    spans.push(component.span.clone());
                }
            }
        }
    }
    spans.sort_by_key(|span| span.start);
    let mut source = source.to_owned();
    for span in spans.into_iter().rev() {
        source.replace_range(span, "");
    }
    format!("# Editor restarted with normalized default fields\n{source}")
}
fn request(stream: &mut TcpStream) -> Option<(String, Value)> {
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut bytes = Vec::new();
    let mut block = [0; 4096];
    loop {
        let n = stream.read(&mut block).ok()?;
        if n == 0 {
            return None;
        }
        bytes.extend_from_slice(&block[..n]);
        assert!(bytes.len() < 4 * 1024 * 1024);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let head = std::str::from_utf8(&bytes[..end]).unwrap();
            let length: usize = head.lines().find_map(|line| line.strip_prefix("Content-Length: ")).unwrap().parse().unwrap();
            if bytes.len() >= end + 4 + length {
                assert!(head.contains("Authorization: Bearer fixture-editor-token"));
                return Some((
                    head.lines().next().unwrap().split_whitespace().nth(1).unwrap().into(),
                    serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap(),
                ));
            }
        }
    }
}
fn apply(root: &Path, tx: &str) -> Value {
    let raw: Value = serde_json::from_slice(&std::fs::read(root.join(format!(".photocraft/publications/{tx}/record.json"))).unwrap()).unwrap();
    let plan: PublishPlan = serde_json::from_value(raw["plan"].clone()).unwrap();
    let mut resources = Vec::new();
    for file in &plan.planned_files {
        if let Some((path, content)) = file.new_path.as_ref().zip(file.content.as_ref()) {
            let staged = root.join(format!(".internal/photocraft/transactions/{tx}/files/{}", content.hash.as_str().replace(':', "-")));
            let mut bytes = std::fs::read(staged).unwrap();
            content.verify(&bytes).unwrap();
            // Simulate Defold's own serialization: persisted GUI bytes differ from draft bytes.
            if path.as_str().ends_with(".gui") {
                bytes.extend_from_slice(b"\n\n");
            }
            let destination = root.join(path.as_str().trim_start_matches('/'));
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::write(destination, &bytes).unwrap();
            resources.push(json!({"path":path,"diskHash":ContentHash::sha256(&bytes)}));
        }
    }
    let targets: Vec<_> = plan
        .targets
        .iter()
        .map(|target| {
            let bytes = std::fs::read(root.join(target.path.as_str().trim_start_matches('/'))).unwrap();
            let hash = ContentHash::sha256(&bytes);
            json!({"path":target.path,"diskHash":hash,"sceneHash":hash,"editorGraphRevision":hash.to_string(),"editorAhead":false})
        })
        .collect();
    json!({"state":"COMMITTED","receipt":{"schemaVersion":1,"transactionId":tx,"targets":targets,"resources":resources}})
}
fn fixture() -> (tempfile::TempDir, Editor, Session) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("game.project"), "[display]\nwidth=96\nheight=96\n").unwrap();
    std::fs::write(dir.path().join("main.gui"), "nodes { id: \"root\" type: TYPE_BOX size {x:96 y:96} position{x:48 y:48} alpha:0 }\n").unwrap();
    let editor = Editor::start(dir.path());
    let mut session = Session::new();
    session.execute("defold.openDesign", json!({"path":dir.path().join("main.gui")})).unwrap();
    let root = session.active().unwrap().doc.defold.as_ref().unwrap().nodes[0].layer;
    session.execute("layer.select", json!({"layer":root})).unwrap();
    session.execute("layer.new.layer", json!({"name":"Reward"})).unwrap();
    session.execute("paint.stroke", json!({"points":[[38,48],[58,48]],"size":10,"hardness":1.0,"color":"#ff6600"})).unwrap();
    (dir, editor, session)
}
#[test]
fn public_manual_publish_preserves_source_and_asset_identity_through_rename_and_delete() {
    let (dir, editor, mut session) = fixture();
    let original = std::fs::read(dir.path().join("main.gui")).unwrap();
    session.execute("defold.saveDesignSource", json!({})).unwrap();
    assert_eq!(original, std::fs::read(dir.path().join("main.gui")).unwrap());
    assert!(!dir.path().join("photocraft_generated").exists());
    let first = session.execute("defold.syncDesign", json!({})).unwrap();
    assert_eq!(first["synced"], true, "{first}");
    let open_id = session.active().unwrap().open_id;
    let binding = &session.project_client.bindings[&open_id];
    assert_eq!(binding.baseline.gui_source, std::fs::read_to_string(dir.path().join("main.gui")).unwrap());
    let registry = binding.store.read_asset_registry(&binding.context).unwrap();
    assert_eq!(registry.assets.len(), 1);
    let first_asset = registry.assets.values().next().unwrap().clone();
    assert!(first_asset.image.as_str().ends_with("/Reward.png"));
    // An expired managed-asset proof is renewed/reacquired. Unrelated historical
    // proofs remain available for release but cannot poison this publication.
    let binding = session.project_client.bindings.get_mut(&open_id).unwrap();
    for proof in &mut binding.leases {
        if matches!(proof.resource, LeaseResource::Asset(_)) {
            proof.lease_epoch = 0;
        }
    }
    binding.leases.push(LeaseProof { resource: LeaseResource::Asset(new_id().unwrap()), lease_epoch: 0 });
    let stable_node = session.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|node| node.node != "root").unwrap().node.clone();
    session.execute("layer.renameLayer", json!({"name":"Reward Renamed"})).unwrap();
    let second = session.execute("defold.syncDesign", json!({})).unwrap();
    assert_eq!(second["synced"], true, "{second}");
    let binding = &session.project_client.bindings[&open_id];
    let registry = binding.store.read_asset_registry(&binding.context).unwrap();
    assert_eq!(registry.assets.len(), 1);
    let asset = registry.assets.values().next().unwrap();
    assert_eq!(asset.asset_id, first_asset.asset_id);
    assert_eq!(asset.runtime_frame_id, first_asset.runtime_frame_id);
    assert_ne!(asset.image, first_asset.image);
    let scene = photocraft_defold_format::Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert!(scene.nodes.iter().any(|node| node.id == stable_node && node.texture.ends_with(&asset.runtime_frame_id)));
    session.execute("layer.delete", json!({})).unwrap();
    let third = session.execute("defold.syncDesign", json!({})).unwrap();
    assert_eq!(third["synced"], true, "{third}");
    let binding = &session.project_client.bindings[&open_id];
    let registry = binding.store.read_asset_registry(&binding.context).unwrap();
    assert_eq!(registry.assets.len(), 1);
    assert_eq!(registry.assets[&first_asset.asset_id].state, AssetState::Detached);
    let scene = photocraft_defold_format::Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert!(!scene.nodes.iter().any(|node| node.id == stable_node));
    assert_eq!(editor.apply_count.load(Ordering::Acquire), 3);
    assert!(!dir.path().join("main.gui.pcraft").exists());
}
#[test]
fn lost_apply_reply_recovers_same_transaction_without_saving_a_new_source_or_reapplying() {
    let (_dir, editor, mut session) = fixture();
    editor.drop_apply.store(true, Ordering::Release);
    let first = session.execute("defold.syncDesign", json!({})).unwrap();
    assert_eq!(first["synced"], false);
    assert_eq!(first["publication"]["phase"], "RECOVERY_REQUIRED");
    let source = client::status(&session)["sourceRevision"].clone();
    assert!(session.execute("defold.cancelPublication", json!({})).unwrap_err().to_string().contains("started applying"));
    session.execute("paint.stroke", json!({"points":[[30,30],[42,30]],"size":6,"color":"#00ff00"})).unwrap();
    let edited = session.active().unwrap().doc.clone();
    let confirmed = session.execute("defold.refreshPublication", json!({})).unwrap();
    assert_eq!(confirmed["synced"], true, "{confirmed}");
    assert_eq!(confirmed["publication"]["transactionId"], first["publication"]["transactionId"]);
    assert_eq!(client::status(&session)["sourceRevision"], source);
    assert_eq!(&*session.active().unwrap().doc, &*edited, "late publication acknowledgement cannot replace newer pixels");
    assert_eq!(editor.apply_count.load(Ordering::Acquire), 1);
    assert_eq!(editor.requests.lock().unwrap().iter().filter(|(route, _)| route == "/craft/v1/publish/prepare").count(), 1);
}
#[test]
fn dirty_or_externally_changed_target_never_publishes_or_discards_local_pixels() {
    let (dir, editor, mut session) = fixture();
    let doc = session.active().unwrap().doc.clone();
    let original = std::fs::read(dir.path().join("main.gui")).unwrap();
    editor.dirty.store(true, Ordering::Release);
    assert!(session.execute("defold.syncDesign", json!({})).unwrap_err().to_string().contains("TARGET_DIRTY"));
    assert_eq!(&*session.active().unwrap().doc, &*doc);
    assert_eq!(original, std::fs::read(dir.path().join("main.gui")).unwrap());
    editor.dirty.store(false, Ordering::Release);
    std::fs::write(dir.path().join("main.gui"), b"nodes {id:\"external\" type:TYPE_BOX}\n").unwrap();
    assert!(session.execute("defold.syncDesign", json!({})).unwrap_err().to_string().contains("TARGET_CHANGED"));
    assert_eq!(&*session.active().unwrap().doc, &*doc);
    assert_eq!(editor.apply_count.load(Ordering::Acquire), 0);
    assert!(!dir.path().join("photocraft_generated").exists());
}

#[test]
fn reopened_draft_saved_again_can_publish_against_its_ancestor_receipt() {
    let (dir, _editor, mut first) = fixture();
    assert_eq!(first.execute("defold.syncDesign", json!({})).unwrap()["synced"], true);
    let mut reopened = Session::new();
    reopened.execute("defold.openDesign", json!({"path":dir.path().join("main.gui")})).unwrap();
    let saved = reopened.execute("defold.saveDesignSource", json!({})).unwrap();
    assert_eq!(saved["saved"], true);
    let published = reopened.execute("defold.syncDesign", json!({})).unwrap();
    assert_eq!(published["synced"], true, "{published}");
}

struct LostLeaseStore(Arc<dyn ProjectStore>);
macro_rules! delegate {
    ($name:ident,$request:ty,$output:ty) => {
        fn $name(&self, request: &$request) -> ProjectResult<$output> {
            self.0.$name(request)
        }
    };
}
impl ProjectStore for LostLeaseStore {
    fn describe(&self) -> ProjectResult<ProjectDescription> {
        self.0.describe()
    }
    delegate!(resolve_scene, ResourceReadRequest, SceneIdentity);
    delegate!(snapshot, SceneRequest, SceneSnapshot);
    delegate!(read_resource, ResourceReadRequest, ResourceContent);
    delegate!(read_source, ReadSourceRequest, SourcePackage);
    delegate!(read_asset_registry, ProjectContext, defold_publish_plan::AssetRegistry);
    delegate!(latest_publication, SceneRequest, Option<PublishReceipt>);
    fn save_source(&self, request: &SaveSourceRequest, payload: &SourcePayload) -> ProjectResult<SourceSaveReceipt> {
        self.0.save_source(request, payload)
    }
    delegate!(operation_status, OperationRequest, Option<SourceSaveReceipt>);
    delegate!(acquire_lease, LeaseRequest, LeaseGrant);
    delegate!(renew_lease, LeaseRenewRequest, LeaseGrant);
    delegate!(release_lease, LeaseReleaseRequest, ());
    fn prepare_publish(&self, plan: &PublishPlan, payload: &PublishPayload) -> ProjectResult<PublishReceipt> {
        self.0.prepare_publish(plan, payload)
    }
    fn commit_publish(&self, _: &PublishRequest) -> ProjectResult<PublishReceipt> {
        Err(ProjectError::new(ProjectErrorCode::LeaseLost, "Fixture lease expired before commit"))
    }
    delegate!(publish_status, PublishRequest, Option<PublishReceipt>);
    delegate!(cancel_publish, PublishRequest, PublishReceipt);
}
#[test]
fn prepared_publication_preserves_lost_lease_reason_and_can_be_cancelled_without_applying() {
    let (dir, editor, mut session) = fixture();
    session.execute("defold.saveDesignSource", json!({})).unwrap();
    let open_id = session.active().unwrap().open_id;
    let binding = session.project_client.bindings.get_mut(&open_id).unwrap();
    binding.store = Arc::new(LostLeaseStore(binding.store.clone()));
    let pending = session.execute("defold.syncDesign", json!({})).unwrap();
    assert_eq!(pending["publication"]["phase"], "PREPARED");
    assert_eq!(pending["publication"]["error"]["code"], "LEASE_LOST");
    assert_eq!(pending["synced"], false);
    let cancelled = session.execute("defold.cancelPublication", json!({})).unwrap();
    assert_eq!(cancelled["publication"]["phase"], "ABORTED");
    assert_eq!(cancelled["publication"]["transactionId"], pending["publication"]["transactionId"]);
    assert_eq!(editor.apply_count.load(Ordering::Acquire), 0);
    assert!(!dir.path().join("photocraft_generated").exists());
    assert!(client::status(&session)["sourceRevision"].is_string());
}

fn external_property(dir: &Path, node: &str, property: &str, value: Value) {
    let scene = photocraft_defold_format::Scene::parse(std::fs::read_to_string(dir.join("main.gui")).unwrap()).unwrap();
    let next = scene.edit(node, property, &value).unwrap();
    std::fs::write(dir.join("main.gui"), next.proto.source).unwrap();
}
fn draft_pixels(session: &Session, layer: u64) -> Vec<f32> {
    session.active().unwrap().doc.layer(photocraft_doc::LayerId(layer)).unwrap().surface().unwrap().read_region(photocraft_doc::Rect::new(0, 0, 96, 96))
}
#[test]
fn project_review_merges_external_property_with_paint_and_save_undo_preserves_pixels_and_baseline() {
    let (dir, _editor, mut session) = fixture();
    session.execute("defold.syncDesign", json!({})).unwrap();
    session.execute("paint.stroke", json!({"points":[[43,43],[53,43]],"size":6,"color":"#0000ff"})).unwrap();
    let link = session.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|node| node.node != "root").unwrap().clone();
    let pixels = draft_pixels(&session, link.artwork);
    let identities = session.active().unwrap().doc.defold.as_ref().unwrap().project.as_ref().unwrap().layers.clone();
    external_property(dir.path(), "root", "enabled", json!(false));
    let review = session.execute("defold.reviewProjectChanges", json!({})).unwrap();
    assert_eq!(review["blocked"], false, "{review}");
    assert!(review["items"].as_array().unwrap().iter().any(|item| item["property"] == "enabled" && item["kind"] == "externalOnly"));
    let applied = session.execute("defold.applyProjectReview", json!({"reviewId":review["reviewId"]})).unwrap();
    assert_eq!(applied["applied"], true, "{applied}");
    assert_eq!(crate::defold_design::scene(&session).unwrap().property("root", "enabled").unwrap(), json!(false));
    assert_eq!(draft_pixels(&session, link.artwork), pixels);
    assert_eq!(session.active().unwrap().doc.defold.as_ref().unwrap().project.as_ref().unwrap().layers, identities);
    let opening = session.active().unwrap().open_id;
    let accepted = session.project_client.bindings[&opening].baseline.clone();
    session.execute("defold.saveDesignSource", json!({})).unwrap();
    session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(crate::defold_design::scene(&session).unwrap().property("root", "enabled").unwrap(), json!(true));
    assert_eq!(draft_pixels(&session, link.artwork), pixels);
    assert_eq!(session.project_client.bindings[&opening].baseline, accepted);
    let published = session.execute("defold.syncDesign", json!({})).unwrap();
    assert_eq!(published["synced"], true, "{published}");
}
#[test]
fn project_review_conflicting_transform_requires_explicit_choice_and_rejects_stale_target() {
    let (dir, _editor, mut session) = fixture();
    session.execute("defold.syncDesign", json!({})).unwrap();
    let node = session.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|node| node.node != "root").unwrap().node.clone();
    session.execute("defold.setDesignProperties", json!({"values":{"position.x":58.0}})).unwrap();
    external_property(dir.path(), &node, "position.x", json!(68.0));
    let review = session.execute("defold.reviewProjectChanges", json!({})).unwrap();
    let item = review["items"].as_array().unwrap().iter().find(|item| item["node"] == node && item["property"] == "position.x").unwrap();
    assert_eq!(item["kind"], "conflict", "{review}");
    assert_eq!(item["canTakeTheirs"], true, "{review}");
    assert!(session.execute("defold.applyProjectReview", json!({"reviewId":review["reviewId"]})).is_err());
    let choices = json!({item["index"].to_string():"theirs"});
    session.execute("defold.applyProjectReview", json!({"reviewId":review["reviewId"],"choices":choices})).unwrap();
    let opening = session.active().unwrap().open_id;
    let projected = review_projection(&session.active().unwrap().doc, &session.project_client.bindings[&opening]).unwrap();
    assert_eq!(projected.scene.property(&node, "position.x").unwrap(), json!(68.0));
    let review = session.execute("defold.reviewProjectChanges", json!({})).unwrap();
    external_property(dir.path(), &node, "position.x", json!(78.0));
    let revision = session.active().unwrap().revision;
    let error = session.execute("defold.applyProjectReview", json!({"reviewId":review["reviewId"]})).unwrap_err();
    assert!(error.to_string().contains("STALE_REVIEW"), "{error}");
    assert_eq!(session.active().unwrap().revision, revision);
}
#[test]
fn project_review_deleted_painted_node_and_dirty_editor_keep_the_native_draft() {
    let (dir, editor, mut session) = fixture();
    session.execute("defold.syncDesign", json!({})).unwrap();
    session.execute("paint.stroke", json!({"points":[[43,43]],"size":6,"color":"#0000ff"})).unwrap();
    let link = session.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|node| node.node != "root").unwrap().clone();
    let pixels = draft_pixels(&session, link.artwork);
    editor.dirty.store(true, Ordering::Release);
    let review = session.execute("defold.reviewProjectChanges", json!({})).unwrap();
    assert_eq!(review["targetDirty"], true);
    assert!(session.execute("defold.applyProjectReview", json!({"reviewId":review["reviewId"]})).unwrap_err().to_string().contains("TARGET_DIRTY"));
    editor.dirty.store(false, Ordering::Release);
    let scene = photocraft_defold_format::Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    std::fs::write(dir.path().join("main.gui"), scene.remove_node(&link.node).unwrap().proto.source).unwrap();
    let review = session.execute("defold.reviewProjectChanges", json!({})).unwrap();
    assert_eq!(review["blocked"], true, "{review}");
    assert!(review["items"].as_array().unwrap().iter().any(|item| item["property"] == "$node" && item["preserveArtwork"] == true));
    let revision = session.active().unwrap().revision;
    assert!(session.execute("defold.applyProjectReview", json!({"reviewId":review["reviewId"]})).is_err());
    assert_eq!(session.active().unwrap().revision, revision);
    assert_eq!(draft_pixels(&session, link.artwork), pixels);
}

#[test]
fn project_review_reopened_source_uses_published_archive_and_publish_accepts_fresh_editor_serialization() {
    let (dir, editor, mut session) = fixture();
    session.execute("defold.syncDesign", json!({})).unwrap();
    let exact_disk = std::fs::read(dir.path().join("main.gui")).unwrap();
    let discovery_path = dir.path().join(".internal/photocraft/editor-adapter.json");
    let mut discovery: Value = serde_json::from_slice(&std::fs::read(&discovery_path).unwrap()).unwrap();
    discovery["editorSessionId"] = json!(EditorSessionId::from_bytes([10; 16]));
    std::fs::write(discovery_path, serde_json::to_vec(&discovery).unwrap()).unwrap();
    editor.restarted.store(true, Ordering::Release);
    let mut reopened = Session::new();
    reopened.execute("defold.openDesign", json!({"path":dir.path().join("main.gui")})).unwrap();
    reopened.execute("defold.saveDesignSource", json!({})).unwrap();
    let result = reopened.execute("defold.syncDesign", json!({})).unwrap();
    assert_eq!(result["synced"], true, "{result}");
    assert_eq!(exact_disk, std::fs::read(dir.path().join("main.gui")).unwrap());
    // Reopen the pre-publication creative source: D now changes after the last
    // publish. The shared base must come from that publication's durable archive.
    let mut another = Session::new();
    another.execute("defold.openDesign", json!({"path":dir.path().join("main.gui")})).unwrap();
    external_property(dir.path(), "root", "enabled", json!(false));
    let reviewed = another.execute("defold.reviewProjectChanges", json!({})).unwrap();
    assert_eq!(reviewed["blocked"], false, "{reviewed}");
    assert!(reviewed["items"].as_array().unwrap().iter().any(|item| item["property"] == "enabled" && item["kind"] == "externalOnly"));
    another.execute("defold.applyProjectReview", json!({"reviewId":reviewed["reviewId"]})).unwrap();
}

#[test]
fn project_review_published_paint_remains_protected_from_external_appearance_after_save_and_undo() {
    let (dir, _editor, mut session) = fixture();
    session.execute("defold.syncDesign", json!({})).unwrap();
    let link = session.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|node| node.node != "root").unwrap().clone();
    let pixels = draft_pixels(&session, link.artwork);
    external_property(dir.path(), &link.node, "color.x", json!(0.25));
    let review = session.execute("defold.reviewProjectChanges", json!({})).unwrap();
    let item = review["items"].as_array().unwrap().iter().find(|item| item["node"] == link.node && item["property"] == "color.x").unwrap();
    assert_eq!(item["kind"], "conflict", "{review}");
    assert_eq!(item["preserveArtwork"], true);
    assert_eq!(item["canKeepMine"], true);
    assert_eq!(item["canTakeTheirs"], false);
    let choices = json!({item["index"].to_string():"theirs"});
    assert!(session.execute("defold.applyProjectReview", json!({"reviewId":review["reviewId"],"choices":choices})).is_err());
    let choices = json!({item["index"].to_string():"mine"});
    session.execute("defold.applyProjectReview", json!({"reviewId":review["reviewId"],"choices":choices})).unwrap();
    assert_eq!(draft_pixels(&session, link.artwork), pixels);
    session.execute("defold.saveDesignSource", json!({})).unwrap();
    session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(draft_pixels(&session, link.artwork), pixels);
}

#[test]
fn captured_publication_keeps_painting_responsive_and_never_overwrites_new_strokes() {
    let (dir, editor, mut session) = fixture();
    let before = session.active().unwrap().revision;
    let layer = session.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|node| node.node != "root").unwrap().artwork;
    editor.hold_apply.store(true, Ordering::Release);
    let crate::jobs::Started::Job(job) = session.start("defold.syncDesign", json!({})).unwrap() else { panic!("Publication must run in background") };
    for _ in 0..1000 {
        if editor.apply_count.load(Ordering::Acquire) > 0 {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(editor.apply_count.load(Ordering::Acquire), 1);
    assert!(!session.is_enabled("defold.syncDesign"));
    assert!(session.is_enabled("paint.stroke"));
    session.execute("paint.stroke", json!({"points":[[48,48]],"size":6,"color":"#0000ff"})).unwrap();
    let pixels = draft_pixels(&session, layer);
    let after = session.active().unwrap().revision;
    assert!(after > before);
    editor.hold_apply.store(false, Ordering::Release);
    let published = session.wait_job(job).unwrap();
    assert_eq!(published["synced"], true, "{published}");
    assert_eq!(published["capturedNativeRevision"], before);
    assert_eq!(session.active().unwrap().revision, after);
    assert_eq!(session.active().unwrap().saved_revision, before);
    assert!(session.active().unwrap().is_dirty());
    assert_eq!(draft_pixels(&session, layer), pixels);
    let binding = &session.project_client.bindings[&session.active().unwrap().open_id];
    let registry = binding.store.read_asset_registry(&binding.context).unwrap();
    let asset = registry.assets.values().next().unwrap();
    let bytes = std::fs::read(dir.path().join(asset.image.as_str().trim_start_matches('/'))).unwrap();
    let frozen = photocraft_io::import("frozen.png", &bytes).unwrap().document;
    let surface = frozen.layers[0].surface().unwrap();
    assert!(surface.read_region(surface.content_bounds()).chunks(4).all(|pixel| pixel[2] < 0.01));
    session.execute("defold.saveDesignSource", json!({})).unwrap();
    assert!(!session.active().unwrap().is_dirty());
    assert_eq!(draft_pixels(&session, layer), pixels);
}

#[test]
fn captured_publication_ack_does_not_invalidate_a_running_native_job_snapshot() {
    let (_dir, editor, mut session) = fixture();
    editor.hold_apply.store(true, Ordering::Release);
    let crate::jobs::Started::Job(publication) = session.start("defold.syncDesign", json!({})).unwrap() else { panic!("Expected publication worker") };
    for _ in 0..1000 {
        if editor.apply_count.load(Ordering::Acquire) > 0 {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(editor.apply_count.load(Ordering::Acquire), 1);
    let original = session.active().unwrap().doc.clone();
    let gate = Arc::new(AtomicBool::new(false));
    let worker_gate = gate.clone();
    let crate::jobs::Started::Job(native) = session
        .start_job(
            "layer.new.layer",
            json!({}),
            "Native snapshot job",
            true,
            move |_| {
                for _ in 0..5000 {
                    if worker_gate.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    thread::sleep(Duration::from_millis(2));
                }
                Err(engine("Native test worker gate timed out"))
            },
            |session, ()| session.execute("paint.stroke", json!({"points":[[48,48]],"size":6,"color":"#0000ff"})),
        )
        .unwrap()
    else {
        panic!("Expected native worker")
    };
    editor.hold_apply.store(false, Ordering::Release);
    let receipt = session.wait_job(publication).unwrap();
    assert_eq!(receipt["synced"], true, "{receipt}");
    assert!(Arc::ptr_eq(&original, &session.active().unwrap().doc));
    gate.store(true, Ordering::Release);
    session.wait_job(native).unwrap();
    assert!(session.active().unwrap().is_dirty());
}

fn add_shared_consumer(root: &Path, session: &mut Session, asset: &AssetId) -> usize {
    std::fs::write(root.join("consumer.gui"), "nodes {id:\"consumer\" type:TYPE_BOX position{x:32 y:32} size{x:16 y:16}}\n").unwrap();
    session.execute("defold.openDesign", json!({"path":root.join("consumer.gui")})).unwrap();
    let index = session.active_index().unwrap();
    let carrier = session.active().unwrap().doc.defold.as_ref().unwrap().nodes[0].layer;
    session.select_layer(photocraft_doc::LayerId(carrier)).unwrap();
    session.execute("defold.attachSharedArtwork", json!({"assetId":asset})).unwrap();
    index
}
fn current_registry(session: &Session) -> defold_publish_plan::AssetRegistry {
    let binding = &session.project_client.bindings[&session.active().unwrap().open_id];
    binding.store.read_asset_registry(&binding.context).unwrap()
}
#[test]
fn shared_artwork_two_gui_publication_source_pin_updates_and_independence_preserve_owner() {
    let (dir, _editor, mut session) = fixture();
    session.execute("defold.syncDesign", json!({})).unwrap();
    let owner_index = session.active_index().unwrap();
    let owner_link = session.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node != "root").unwrap().clone();
    let asset = current_registry(&session).assets.values().next().unwrap().asset_id.clone();
    let shared = session.execute("defold.shareNodeArtwork", json!({})).unwrap();
    assert_eq!(shared["sharedAssetId"], json!(asset));
    let consumer_index = add_shared_consumer(dir.path(), &mut session, &asset);
    let consumer_open = session.active().unwrap().open_id;
    let carrier = session.active().unwrap().doc.defold.as_ref().unwrap().nodes[0].layer;
    let proxy = session.active().unwrap().doc.defold.as_ref().unwrap().nodes[0].artwork;
    session.select_layer(photocraft_doc::LayerId(proxy)).unwrap();
    assert!(session.execute("paint.stroke", json!({"points":[[32,32]],"size":4,"color":"#ffffff"})).is_err());
    session.select_layer(photocraft_doc::LayerId(carrier)).unwrap();
    session.execute("layer.translate", json!({"dx":3,"dy":4})).unwrap();
    let moved = crate::defold_design::scene(&session).unwrap();
    assert_eq!(moved.property("consumer", "position.x").unwrap(), json!(35.0));
    assert_eq!(moved.property("consumer", "position.y").unwrap(), json!(28.0));
    session.execute("defold.saveDesignSource", json!({})).unwrap();
    let binding = &session.project_client.bindings[&consumer_open];
    let package = binding
        .store
        .read_source(&ReadSourceRequest {
            context: binding.context.clone(),
            package_id: binding.scene.package_id.clone(),
            source_revision: binding.source_revision.clone(),
        })
        .unwrap();
    assert_eq!(package.envelope.draft.shared_sources.len(), 1);
    let stripped = photocraft_format::load_from_bytes(&package.payload.native).unwrap();
    let proxy = stripped.defold.as_ref().unwrap().nodes[0].artwork;
    assert_eq!(stripped.layer(photocraft_doc::LayerId(proxy)).unwrap().children().unwrap().len(), 0, "proxy pixels are not a second source");
    let publication = session.execute("defold.syncDesign", json!({})).unwrap();
    assert_eq!(publication["synced"], true, "{publication}");
    assert_eq!(publication["publication"]["targets"].as_array().unwrap().len(), 2);
    let first = current_registry(&session).assets[&asset].clone();
    assert_eq!(first.referencing_scenes.len(), 2);
    session.set_active(owner_index);
    session.select_layer(photocraft_doc::LayerId(owner_link.artwork)).unwrap();
    session.execute("paint.stroke", json!({"points":[[48,48]],"size":5,"hardness":1.0,"color":"#0000ff"})).unwrap();
    session
        .edit("Insert native AI version", |doc, _| {
            let mut version = photocraft_doc::Layer::raster("AI version", doc.pixel_format());
            version.surface_mut().unwrap().fill_rect(photocraft_geom::Rect::new(45, 46, 47, 48), &[0.0, 1.0, 0.0, 1.0]);
            crate::defold_design::insert_artwork(doc, photocraft_doc::LayerId(owner_link.artwork), version)?;
            Ok(())
        })
        .unwrap();
    let publication = session.execute("defold.syncDesign", json!({})).unwrap();
    assert_eq!(publication["synced"], true, "{publication}");
    assert_eq!(publication["publication"]["targets"].as_array().unwrap().len(), 2);
    let updated = current_registry(&session).assets[&asset].clone();
    assert_ne!(updated.output.hash, first.output.hash);
    assert_eq!(updated.source_node, first.source_node, "AI promotion retains the canonical carrier locator");
    let canonical_bytes = std::fs::read(dir.path().join(updated.image.as_str().trim_start_matches('/'))).unwrap();
    session.set_active(consumer_index);
    session.select_layer(photocraft_doc::LayerId(carrier)).unwrap();
    let source = session.active().unwrap().doc.defold.as_ref().unwrap().project.as_ref().unwrap().layers[&carrier].appearance.clone();
    let Some(AppearanceSource::Shared { source }) = source else { panic!("consumer lost reference") };
    assert_eq!(source.source_revision, updated.source.as_ref().unwrap().source_revision);
    session.execute("defold.makeArtworkIndependent", json!({})).unwrap();
    let independent = session.active().unwrap().doc.defold.as_ref().unwrap().project.as_ref().unwrap().layers[&carrier].asset_id.clone().unwrap();
    assert_ne!(independent, asset);
    // Select an actual native raster within the independent source's complete subtree.
    fn raster(layer: &photocraft_doc::Layer) -> Option<photocraft_doc::LayerId> {
        if matches!(layer.content, photocraft_doc::LayerContent::Raster(_)) { Some(layer.id) } else { layer.children().into_iter().flatten().find_map(raster) }
    }
    let raster = session.active().unwrap().doc.layer(photocraft_doc::LayerId(carrier)).and_then(raster).unwrap();
    session.select_layer(raster).unwrap();
    session.execute("paint.stroke", json!({"points":[[35,36]],"size":6,"color":"#00ff00"})).unwrap();
    let publication = session.execute("defold.syncDesign", json!({})).unwrap();
    assert_eq!(publication["synced"], true, "{publication}");
    let registry = current_registry(&session);
    assert_eq!(registry.assets[&asset].output, updated.output);
    assert_eq!(std::fs::read(dir.path().join(updated.image.as_str().trim_start_matches('/'))).unwrap(), canonical_bytes);
    assert_eq!(registry.assets[&asset].referencing_scenes.len(), 1);
    assert!(registry.assets.contains_key(&independent));
}
#[test]
fn shared_artwork_dirty_consumer_or_editor_blocks_before_any_source_head_changes() {
    let (dir, editor, mut session) = fixture();
    session.execute("defold.syncDesign", json!({})).unwrap();
    let owner_index = session.active_index().unwrap();
    let owner_open = session.active().unwrap().open_id;
    let owner_art = session.active().unwrap().active_layer.unwrap();
    let asset = current_registry(&session).assets.values().next().unwrap().asset_id.clone();
    let consumer_index = add_shared_consumer(dir.path(), &mut session, &asset);
    session.execute("defold.syncDesign", json!({})).unwrap();
    let before_owner = session.project_client.bindings[&owner_open].source_revision.clone();
    let owner_disk = std::fs::read(dir.path().join("main.gui")).unwrap();
    let consumer_disk = std::fs::read(dir.path().join("consumer.gui")).unwrap();
    session.execute("defold.setDesignProperties", json!({"values":{"position.x":71}})).unwrap();
    session.set_active(owner_index);
    session.select_layer(owner_art).unwrap();
    session.execute("paint.stroke", json!({"points":[[43,48]],"size":3,"color":"#0000ff"})).unwrap();
    let failure = session.execute("defold.syncDesign", json!({})).unwrap_err();
    assert!(failure.to_string().contains("TARGET_DIRTY"), "{failure}");
    assert_eq!(session.project_client.bindings[&owner_open].source_revision, before_owner);
    assert_eq!(std::fs::read(dir.path().join("main.gui")).unwrap(), owner_disk);
    assert_eq!(std::fs::read(dir.path().join("consumer.gui")).unwrap(), consumer_disk);
    session.set_active(consumer_index);
    session.execute("defold.saveDesignSource", json!({})).unwrap();
    session.set_active(owner_index);
    let review = session.execute("defold.reviewSharedPublication", json!({})).unwrap();
    editor.dirty.store(true, Ordering::Release);
    let failure = session.execute("defold.syncDesign", json!({"sharedReviewId":review["sharedReviewId"]})).unwrap_err();
    assert!(failure.to_string().contains("TARGET_DIRTY"), "{failure}");
    assert_eq!(session.project_client.bindings[&owner_open].source_revision, before_owner);
    assert_eq!(std::fs::read(dir.path().join("main.gui")).unwrap(), owner_disk);
    assert_eq!(std::fs::read(dir.path().join("consumer.gui")).unwrap(), consumer_disk);
}

#[test]
fn shared_artwork_erasing_owner_publishes_transparent_same_identity_to_every_consumer() {
    let (dir, _editor, mut session) = fixture();
    session.execute("defold.syncDesign", json!({})).unwrap();
    let owner_index = session.active_index().unwrap();
    let owner_art = session.active().unwrap().active_layer.unwrap();
    let original = current_registry(&session).assets.values().next().unwrap().clone();
    add_shared_consumer(dir.path(), &mut session, &original.asset_id);
    session.execute("defold.syncDesign", json!({})).unwrap();
    session.set_active(owner_index);
    session.select_layer(owner_art).unwrap();
    session.execute("paint.stroke", json!({"points":[[48,48]],"size":96,"hardness":1.0,"erase":true})).unwrap();
    let publication = session.execute("defold.syncDesign", json!({})).unwrap();
    assert_eq!(publication["synced"], true, "{publication}");
    assert_eq!(publication["publication"]["targets"].as_array().unwrap().len(), 2);
    let current = current_registry(&session).assets[&original.asset_id].clone();
    assert_eq!(current.asset_id, original.asset_id);
    assert_eq!(current.runtime_frame_id, original.runtime_frame_id);
    assert_eq!(current.source_node, original.source_node);
    let bytes = std::fs::read(dir.path().join(current.image.as_str().trim_start_matches('/'))).unwrap();
    let image = photocraft_io::import("shared.png", &bytes).unwrap().document;
    assert_eq!(image.size, photocraft_geom::Size::new(1, 1));
    assert!(photocraft_compose::render(&image, image.bounds()).px.iter().all(|pixel| pixel[3] == 0.0));
}

#[test]
fn shared_artwork_saved_unpublished_related_changes_require_review_and_explicit_inclusion() {
    let (dir, _editor, mut session) = fixture();
    session.execute("defold.syncDesign", json!({})).unwrap();
    let owner_index = session.active_index().unwrap();
    let owner_open = session.active().unwrap().open_id;
    let asset = current_registry(&session).assets.values().next().unwrap().asset_id.clone();
    add_shared_consumer(dir.path(), &mut session, &asset);
    session.execute("defold.syncDesign", json!({})).unwrap();
    let carrier = session.active().unwrap().doc.defold.as_ref().unwrap().nodes[0].layer;
    session.select_layer(photocraft_doc::LayerId(carrier)).unwrap();
    session.execute("defold.setDesignProperties", json!({"values":{"position.x":73}})).unwrap();
    session.execute("defold.saveDesignSource", json!({})).unwrap();
    assert!(!session.active().unwrap().is_dirty());
    let consumer_open = session.active().unwrap().open_id;
    let consumer_head = session.project_client.bindings[&consumer_open].source_revision.clone();
    let owner_head = session.project_client.bindings[&owner_open].source_revision.clone();
    let owner_disk = std::fs::read(dir.path().join("main.gui")).unwrap();
    let consumer_disk = std::fs::read(dir.path().join("consumer.gui")).unwrap();
    session.set_active(owner_index);
    let error = session.execute("defold.syncDesign", json!({})).unwrap_err();
    assert!(error.to_string().contains("SHARED_REVIEW_REQUIRED"), "{error}");
    assert_eq!(session.project_client.bindings[&owner_open].source_revision, owner_head);
    assert_eq!(session.project_client.bindings[&consumer_open].source_revision, consumer_head);
    assert_eq!(std::fs::read(dir.path().join("main.gui")).unwrap(), owner_disk);
    assert_eq!(std::fs::read(dir.path().join("consumer.gui")).unwrap(), consumer_disk);
    let review = session.execute("defold.reviewSharedPublication", json!({})).unwrap();
    assert_eq!(review["requiresRelatedSourceApproval"], true);
    assert!(review["targets"].as_array().unwrap().iter().any(|target| target["path"] == "/consumer.gui" && target["unpublishedSource"] == true));
    let result = session.execute("defold.syncDesign", json!({"sharedReviewId":review["sharedReviewId"]})).unwrap();
    assert_eq!(result["synced"], true, "{result}");
    assert_eq!(result["publication"]["targets"].as_array().unwrap().len(), 2);
    let scene = photocraft_defold_format::Scene::parse(std::fs::read_to_string(dir.path().join("consumer.gui")).unwrap()).unwrap();
    assert!(scene.property("consumer", "position.x").unwrap().as_f64().unwrap() > 60.0);
}
#[test]
fn shared_artwork_changed_related_head_invalidates_review_without_publishing_or_saving() {
    let (dir, _editor, mut session) = fixture();
    session.execute("defold.syncDesign", json!({})).unwrap();
    let owner_index = session.active_index().unwrap();
    let owner_open = session.active().unwrap().open_id;
    let asset = current_registry(&session).assets.values().next().unwrap().asset_id.clone();
    let consumer_index = add_shared_consumer(dir.path(), &mut session, &asset);
    session.execute("defold.syncDesign", json!({})).unwrap();
    let carrier = session.active().unwrap().doc.defold.as_ref().unwrap().nodes[0].layer;
    session.select_layer(photocraft_doc::LayerId(carrier)).unwrap();
    session.execute("defold.setDesignProperties", json!({"values":{"position.x":62}})).unwrap();
    session.execute("defold.saveDesignSource", json!({})).unwrap();
    session.set_active(owner_index);
    let review = session.execute("defold.reviewSharedPublication", json!({})).unwrap();
    session.set_active(consumer_index);
    session.execute("defold.setDesignProperties", json!({"values":{"position.x":74}})).unwrap();
    session.execute("defold.saveDesignSource", json!({})).unwrap();
    let consumer_open = session.active().unwrap().open_id;
    let consumer_head = session.project_client.bindings[&consumer_open].source_revision.clone();
    let owner_head = session.project_client.bindings[&owner_open].source_revision.clone();
    let owner_disk = std::fs::read(dir.path().join("main.gui")).unwrap();
    let consumer_disk = std::fs::read(dir.path().join("consumer.gui")).unwrap();
    session.set_active(owner_index);
    let error = session.execute("defold.syncDesign", json!({"sharedReviewId":review["sharedReviewId"]})).unwrap_err();
    assert!(error.to_string().contains("SHARED_REVIEW_EXPIRED"), "{error}");
    assert_eq!(session.project_client.bindings[&owner_open].source_revision, owner_head);
    assert_eq!(session.project_client.bindings[&consumer_open].source_revision, consumer_head);
    assert_eq!(std::fs::read(dir.path().join("main.gui")).unwrap(), owner_disk);
    assert_eq!(std::fs::read(dir.path().join("consumer.gui")).unwrap(), consumer_disk);
}

#[test]
fn shared_artwork_old_clean_managed_output_is_rebaked_once_to_upgrade_recipe() {
    let (dir, _editor, mut session) = fixture();
    session.execute("defold.syncDesign", json!({})).unwrap();
    let before = current_registry(&session).assets.values().next().unwrap().clone();
    let path = dir.path().join(".photocraft/assets/index.json");
    let mut catalog: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let record = &mut catalog["registry"]["assets"][before.asset_id.as_str()];
    record["recipe"] = Value::Null;
    record["sourceNode"] = Value::Null;
    std::fs::write(&path, serde_json::to_vec(&catalog).unwrap()).unwrap();
    let opening = session.active().unwrap().open_id;
    let binding = &session.project_client.bindings[&opening];
    let captured = capture_assets(binding, &session.active().unwrap().doc).unwrap();
    assert!(captured.native.force_bake.contains(before.asset_id.as_str()));
    let result = session.execute("defold.syncDesign", json!({})).unwrap();
    assert_eq!(result["synced"], true, "{result}");
    let after = current_registry(&session).assets[&before.asset_id].clone();
    assert_eq!(after.asset_id, before.asset_id);
    assert_eq!(after.runtime_frame_id, before.runtime_frame_id);
    assert_eq!(after.recipe, before.recipe);
    assert_eq!(after.source_node, before.source_node);
    let binding = &session.project_client.bindings[&opening];
    let captured = capture_assets(binding, &session.active().unwrap().doc).unwrap();
    assert!(!captured.native.force_bake.contains(before.asset_id.as_str()));
}

#[test]
fn managed_texture_without_asset_record_is_rebaked_with_a_real_recipe() {
    let (dir, _editor, mut session) = fixture();
    session.execute("defold.syncDesign", json!({})).unwrap();
    let path = dir.path().join(".photocraft/assets/index.json");
    let mut catalog: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    catalog["registry"]["assets"] = json!({});
    std::fs::write(&path, serde_json::to_vec(&catalog).unwrap()).unwrap();
    let state = session.active().unwrap();
    // Model opening the runtime GUI as a legacy import, rather than deleting a
    // registry underneath an already-open source that still has authored texture IDs.
    let mut imported = state.doc.as_ref().clone();
    imported.defold.as_mut().unwrap().source = std::fs::read_to_string(dir.path().join("main.gui")).unwrap();
    let node = imported.defold.as_ref().unwrap().nodes.iter().find(|node| node.node != "root").unwrap().clone();
    let captured = capture_assets(&session.project_client.bindings[&state.open_id], &imported).unwrap();
    assert!(captured.native.force_bake.contains(&node.asset_id));
    let planned = crate::defold_design::plan_publication(imported.clone(), captured.native).unwrap();
    assert!(planned.recipes.contains_key(&node.node));
    for link in &mut imported.defold.as_mut().unwrap().nodes {
        link.asset_id.clear();
    }
    let captured = capture_assets(&session.project_client.bindings[&state.open_id], &imported).unwrap();
    let planned = crate::defold_design::plan_publication(imported, captured.native).unwrap();
    assert!(planned.recipes.contains_key(&node.node));
}

#[test]
fn shared_artwork_hidden_owner_draft_can_save_but_cannot_silently_hide_consumers() {
    let (dir, _editor, mut session) = fixture();
    session.execute("defold.syncDesign", json!({})).unwrap();
    let owner_index = session.active_index().unwrap();
    let owner_open = session.active().unwrap().open_id;
    let owner_layer = session.active().unwrap().active_layer.unwrap();
    let asset = current_registry(&session).assets.values().next().unwrap().clone();
    add_shared_consumer(dir.path(), &mut session, &asset.asset_id);
    session.execute("defold.syncDesign", json!({})).unwrap();
    session.set_active(owner_index);
    session.select_layer(owner_layer).unwrap();
    session.execute("defold.setDesignProperties", json!({"values":{"enabled":false}})).unwrap();
    session.execute("defold.saveDesignSource", json!({})).unwrap();
    let saved = session.project_client.bindings[&owner_open].source_revision.clone();
    let owner_disk = std::fs::read(dir.path().join("main.gui")).unwrap();
    let consumer_disk = std::fs::read(dir.path().join("consumer.gui")).unwrap();
    let error = session.execute("defold.syncDesign", json!({})).unwrap_err();
    assert!(error.to_string().contains("instance visibility"), "{error}");
    assert_eq!(session.project_client.bindings[&owner_open].source_revision, saved);
    assert_eq!(std::fs::read(dir.path().join("main.gui")).unwrap(), owner_disk);
    assert_eq!(std::fs::read(dir.path().join("consumer.gui")).unwrap(), consumer_disk);
    assert_eq!(current_registry(&session).assets[&asset.asset_id].output, asset.output);
}
