//! The production control dispatcher, with authority limited to this test's own output files.

use std::path::PathBuf;
use std::sync::Arc;

use photocraft_engine::Session;
use photocraft_ui_egui::control::{ControlRequest, Outcome, handle};
use photocraft_ui_egui::{PhotocraftApp, Services};
use serde_json::{Value, json};

fn call(app: &mut PhotocraftApp, ctx: &egui::Context, method: &str, params: Value) -> Value {
    let (request, _) = ControlRequest::new(method, params);
    match handle(app, ctx, &request) {
        Outcome::Done(value) => {
            assert_eq!(value["ok"], true, "{method}: {value}");
            value["result"].clone()
        }
        _ => panic!("synchronous test unexpectedly started an asynchronous request"),
    }
}

fn target_pixels(app: &PhotocraftApp) -> Vec<f32> {
    let st = app.session.active().unwrap();
    let layer = st.doc.layers.iter().find(|l| l.name == "Target").unwrap();
    layer.surface().unwrap().read_region(st.doc.bounds())
}

#[test]
fn review_save_apply_undo_reopen_and_export_through_control() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::env::var_os("CRAFT_AI_ARTIFACT_DIR").map(PathBuf::from).unwrap_or_else(|| temporary.path().to_owned());
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let review = root.join("review.pcraft");
    let applied = root.join("applied.pcraft");
    let exported = root.join("applied.png");
    let allowed = vec![review.clone(), applied.clone(), exported.clone()];
    let reads = allowed.clone();
    let writes = allowed;
    let services = Services {
        import: Some(Box::new(|name, bytes, _| photocraft_io::import(name, bytes).map(|r| (r.document, r.warnings)).map_err(|e| e.to_string()))),
        export: Some(Box::new(|doc, path, _| photocraft_io::export(doc, path, &Default::default()).map(|r| (r.bytes, r.warnings)).map_err(|e| e.to_string()))),
        automation_read: Some(Box::new(move |path| {
            let path = PathBuf::from(path);
            if !reads.contains(&path) {
                return Err("read outside the test files".into());
            }
            let name = path.file_name().and_then(|n| n.to_str()).ok_or("invalid file name")?.to_owned();
            std::fs::read(path).map(|bytes| (name, bytes)).map_err(|e| e.to_string())
        })),
        automation_write: Some(Box::new(move |path, bytes| {
            let path = PathBuf::from(path);
            if !writes.contains(&path) {
                return Err("write outside the test files".into());
            }
            photocraft_format::atomic_write(&path, bytes).map_err(|e| e.to_string())
        })),
        ..Default::default()
    };
    let mut app = PhotocraftApp::new(Session::new(), services);
    let ctx = egui::Context::default();
    let script: Vec<(String, Value)> = serde_json::from_str(include_str!("../../../scripts/ai-p0-preview.json")).unwrap();
    for (method, params) in script {
        call(&mut app, &ctx, &method, params);
    }
    let original = target_pixels(&app);
    let base = app.session.active().unwrap().doc.clone();
    let candidate = app.session.ai.candidate().unwrap();
    let preview = candidate.preview(&app.session).unwrap();
    assert!(!Arc::ptr_eq(&base, &preview));
    // Saving during review must write the committed original, never the displayed candidate.
    call(&mut app, &ctx, "app.save", json!({"path": review}));
    let review_doc = photocraft_format::load_path(&review).unwrap();
    let review_layer = review_doc.layers.iter().find(|l| l.name == "Target").unwrap();
    assert_eq!(review_layer.surface().unwrap().read_region(review_doc.bounds()), original);
    // Save As can rename the source document; take a fresh candidate after that metadata edit.
    let prepared = call(&mut app, &ctx, "engine.execute", json!({"command": "ai.prepareTestCandidate", "params": {"prompt": "Local transaction test"}}));
    let id = prepared["candidateId"].as_u64().unwrap();
    let steps = app.session.active().unwrap().history.past_len();
    call(&mut app, &ctx, "engine.execute", json!({"command": "ai.applyCandidate", "params": {"candidateId": id}}));
    let result = target_pixels(&app);
    assert_ne!(result, original);
    assert_eq!(app.session.active().unwrap().history.past_len(), steps + 1);
    call(&mut app, &ctx, "engine.execute", json!({"command": "edit.undo"}));
    assert_eq!(target_pixels(&app), original);
    call(&mut app, &ctx, "engine.execute", json!({"command": "edit.redo"}));
    assert_eq!(target_pixels(&app), result);
    call(&mut app, &ctx, "app.save", json!({"path": applied}));
    call(&mut app, &ctx, "app.open", json!({"path": applied}));
    assert_eq!(target_pixels(&app), result);
    assert!(app.session.ai.candidate().is_none());
    call(&mut app, &ctx, "app.save", json!({"path": exported}));
    let exported = std::fs::read(exported).unwrap();
    assert!(exported.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(app.session.active().unwrap().path.as_deref(), applied.to_str(), "flat export keeps the native project path");

    // Default desktop generation stores ordinary document content without a separate Apply.
    use base64::Engine as _;
    let image = photocraft_codecs::Image::from_u8(2, 2, photocraft_codecs::ChannelLayout::Rgba, [200, 30, 10, 255].repeat(4)).unwrap();
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap());
    let began =
        call(&mut app, &ctx, "engine.execute", json!({"command":"ai.beginGeneration","params":{"prompt":"red","provider":"codex","destination":"layer"}}));
    let receipt =
        call(&mut app, &ctx, "engine.execute", json!({"command":"ai.receiveCandidate","params":{"candidateId":began["candidateId"],"imageBase64":encoded}}));
    let id = photocraft_doc::LayerId(receipt["layerId"].as_u64().unwrap());
    let ai_pixels = app.session.active().unwrap().doc.layer(id).unwrap().surface().unwrap().read_region(app.session.active().unwrap().doc.bounds());
    assert_eq!(target_pixels(&app), result, "generation does not replace the target");
    call(&mut app, &ctx, "app.save", json!({"path": applied}));
    call(&mut app, &ctx, "app.open", json!({"path": applied}));
    let saved = &app.session.active().unwrap().doc;
    assert_eq!(saved.layer(id).unwrap().name, "AI 1");
    assert_eq!(saved.layer(id).unwrap().surface().unwrap().read_region(saved.bounds()), ai_pixels);
    assert_eq!(target_pixels(&app), result);
}
