#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use base64::Engine;
use photocraft_color::{Color, ColorMode, SampleType};
use photocraft_doc::{Document, Size};

struct Project(PathBuf);
impl Project {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("photocraft-defold-test-{}", identity().unwrap()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("game.project"), "[display]\nwidth = 800\nheight = 600\n").unwrap();
        std::fs::write(dir.join("screen.gui"),"# untouched future properties\nnodes { id: \"button\" type: TYPE_BOX position { x: 100 y: 50 } size { x: 80 y: 40 } texture: \"ui/button\" future { magic: 77 } }\nnodes { id: \"label\" type: TYPE_TEXT parent: \"button\" text: \"Play\" }\ntextures { name: \"ui\" texture: \"/ui.atlas\" }\n").unwrap();
        std::fs::write(dir.join("ui.atlas"), "images { image: \"/button.png\" }\n").unwrap();
        std::fs::write(dir.join("button.png"), png(Color::rgba(0.0, 0.0, 1.0, 1.0))).unwrap();
        Self(dir)
    }
    fn open(&self, s: &mut Session) -> u64 {
        s.execute("defold.openGui", json!({"path":self.0.join("screen.gui")})).unwrap()["id"].as_u64().unwrap()
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn png(color: Color) -> Vec<u8> {
    let d = Document::with_background("fixture", Size::new(8, 4), ColorMode::Rgb, SampleType::U8, color);
    photocraft_io::export(&d, "png", &Default::default()).unwrap().bytes
}
#[test]
fn gui_edit_save_preserves_unknowns_and_independent_history() {
    let project = Project::new();
    let mut s = Session::new();
    s.execute("file.new", json!({"width":8,"height":4})).unwrap();
    let image = s.active().unwrap().doc.clone();
    project.open(&mut s);
    assert!(s.active().is_none());
    assert!(s.execute("edit.fill", json!({"contents":"black"})).is_err());
    s.execute("defold.setNodeProperty", json!({"node":"label","property":"text","value":"开始"})).unwrap();
    s.execute("defold.setNodeProperty", json!({"node":"button","property":"position.x","value":120})).unwrap();
    assert!(s.active_gui().unwrap().dirty());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_gui().unwrap().scene.nodes[0].position[0], 100.0);
    s.execute("edit.redo", json!({})).unwrap();
    s.execute("defold.saveGui", json!({})).unwrap();
    let disk = std::fs::read_to_string(project.0.join("screen.gui")).unwrap();
    assert!(disk.contains("future { magic: 77 }"));
    assert!(disk.contains("# untouched"));
    assert!(disk.contains("开始"));
    assert!(!s.active_gui().unwrap().dirty());
    assert!(std::sync::Arc::ptr_eq(&s.documents()[0].doc, &image));
    s.execute("file.closeAll", json!({})).unwrap();
    assert!(s.documents().is_empty());
    assert!(s.gui.documents.is_empty());
}
#[test]
fn node_generation_publish_and_reopen_use_existing_ai_without_stealing_gui_focus() {
    let project = Project::new();
    let mut s = Session::new();
    let gui = project.open(&mut s);
    let opened = s.execute("defold.openNodeSource", json!({"node":"button"})).unwrap();
    let source_id = s.active().unwrap().doc.design_source_id.clone();
    assert!(source_id.is_some());
    let begin = s.execute("ai.beginGeneration", json!({"prompt":"red button","provider":"codex","destination":"layer"})).unwrap();
    s.execute("defold.activateGui", json!({"id":gui})).unwrap();
    let candidate = base64::engine::general_purpose::STANDARD.encode(png(Color::rgba(1.0, 0.0, 0.0, 0.5)));
    let result = s.execute("ai.receiveCandidate", json!({"candidateId":begin["candidateId"],"imageBase64":candidate})).unwrap();
    assert!(s.active_gui().is_some());
    assert!(s.active().is_none());
    let image = &s.documents()[0];
    assert_eq!(image.doc.layers.len(), 2);
    assert_eq!(image.doc.design_source_id, source_id);
    assert_eq!(image.doc.ai_generations.len(), 1);
    assert!(result["layerId"].is_number());
    let publish = s.execute("defold.publishNodeSource", json!({"binding":opened["binding"]})).unwrap();
    assert_eq!(publish["published"], true);
    assert_eq!(s.execute("defold.publishNodeSource", json!({"binding":opened["binding"]})).unwrap()["hash"], publish["hash"]);
    let mut reopened = Session::new();
    project.open(&mut reopened);
    reopened.execute("defold.openNodeSource", json!({"node":"button"})).unwrap();
    assert_eq!(reopened.active().unwrap().doc.design_source_id, source_id);
    assert_eq!(reopened.active().unwrap().doc.ai_generations.len(), 1);
    let target = photocraft_io::import("target.png", &std::fs::read(project.0.join("button.png")).unwrap()).unwrap().document;
    let pixel = target.layers[0].surface().unwrap().rgba(2, 2);
    assert!(pixel[0] > 0.4);
    assert!(pixel[2] > 0.4);
}
#[test]
fn save_conflicts_recovery_and_dirty_close_are_safe() {
    let project = Project::new();
    let mut s = Session::new();
    project.open(&mut s);
    s.execute("defold.setNodeProperty", json!({"node":"label","property":"text","value":"Recovered"})).unwrap();
    assert!(s.execute("file.close", json!({})).is_err());
    assert!(s.execute("file.closeAll", json!({})).is_err());
    let mut recovered = Session::new();
    project.open(&mut recovered);
    recovered.execute("defold.recoverGui", json!({})).unwrap();
    assert_eq!(recovered.active_gui().unwrap().scene.nodes[1].text, "Recovered");
    let external = "nodes { id: \"external\" }\n";
    std::fs::write(project.0.join("screen.gui"), external).unwrap();
    assert!(s.execute("defold.saveGui", json!({})).is_err());
    assert_eq!(std::fs::read_to_string(project.0.join("screen.gui")).unwrap(), external);
}
#[test]
fn publication_rejects_external_assets_editor_sessions_and_bad_paths() {
    let project = Project::new();
    let mut s = Session::new();
    let gui = project.open(&mut s);
    let b = s.execute("defold.openNodeSource", json!({})).unwrap()["binding"].clone();
    std::fs::write(project.0.join("button.png"), png(Color::BLACK)).unwrap();
    assert!(s.execute("defold.publishNodeSource", json!({"binding":b})).is_err());
    assert!(resource(&project.0.to_string_lossy(), "/../elsewhere.png").is_err());
    assert!(resource(&project.0.to_string_lossy(), "C:\\button.png").is_err());
    std::fs::create_dir(project.0.join(".internal")).unwrap();
    std::fs::write(project.0.join(".internal/editor.port"), "12345").unwrap();
    s.execute("defold.activateGui", json!({"id":gui})).unwrap();
    assert!(s.execute("defold.saveGui", json!({})).is_err());
}
#[test]
fn hostile_commands_fail_without_mutating_open_images() {
    let mut s = Session::new();
    for spec in specs() {
        assert!(s.execute(spec.id, json!({"id":99999,"path":"missing.gui","binding":"missing"})).is_err(), "{}", spec.id);
    }
}

#[test]
fn gui_layer_commands_add_child_reparent_rename_and_undo_without_touching_images() {
    let project = Project::new();
    let mut s = Session::new();
    s.execute("file.new", json!({"width":8,"height":4})).unwrap();
    let image = s.active().unwrap().doc.clone();
    project.open(&mut s);
    s.execute("layer.new.layer", json!({"id":"child","parent":"button"})).unwrap();
    s.execute("defold.setNodeProperties", json!({"node":"child","values":{"position.x":17,"position.y":29,"pivot":"PIVOT_NE"}})).unwrap();
    s.execute("defold.moveNode", json!({"node":"child","parent":""})).unwrap();
    s.execute("layer.renameLayer", json!({"name":"renamed"})).unwrap();
    s.execute("layer.delete", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    let n = s.active_gui().unwrap().scene.nodes.iter().find(|n| n.id == "renamed").unwrap();
    assert_eq!(n.position, [17.0, 29.0, 0.0]);
    assert_eq!(n.parent, "");
    s.execute("defold.saveGui", json!({})).unwrap();
    assert!(std::sync::Arc::ptr_eq(&s.documents()[0].doc, &image));
}

#[test]
fn selected_layer_publication_keeps_alpha_and_deleted_bindings_fail() {
    let project = Project::new();
    let mut s = Session::new();
    let gui = project.open(&mut s);
    let binding = s.execute("defold.openNodeSource", json!({})).unwrap()["binding"].clone();
    let begin = s.execute("ai.beginGeneration", json!({"prompt":"transparent button","provider":"codex","destination":"layer"})).unwrap();
    s.execute(
        "ai.receiveCandidate",
        json!({"candidateId":begin["candidateId"],"imageBase64":base64::engine::general_purpose::STANDARD.encode(png(Color::rgba(1.0,0.0,0.0,0.5)))}),
    )
    .unwrap();
    s.execute("defold.bindSourceLayer", json!({"binding":binding})).unwrap();
    s.execute("defold.publishNodeSource", json!({"binding":binding})).unwrap();
    assert!(!s.active().unwrap().is_dirty());
    let target = photocraft_io::import("target.png", &std::fs::read(project.0.join("button.png")).unwrap()).unwrap().document;
    let pixel = target.layers[0].surface().unwrap().rgba(2, 2);
    assert!(pixel[0] > 0.95 && pixel[2] < 0.05 && (pixel[3] - 0.5).abs() < 0.02, "{pixel:?}");
    s.execute("layer.delete", json!({})).unwrap();
    assert!(s.execute("defold.publishNodeSource", json!({"binding":binding})).is_err());
    s.execute("defold.activateGui", json!({"id":gui})).unwrap();
    s.execute("file.closeOthers", json!({})).unwrap();
    assert!(s.active_gui().is_some());
    assert!(s.documents().is_empty());
}

#[test]
fn bridge_probe_reports_capabilities_without_exposing_tokens() {
    use std::io::{Read, Write};
    let project = Project::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::fs::create_dir(project.0.join(".internal")).unwrap();
    std::fs::write(project.0.join(".internal/editor.port"), port.to_string()).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
        let mut buf = [0; 2048];
        let _ = stream.read(&mut buf).unwrap();
        let body = r#"{"paths":{"/preview/{path}":{}},"sessionToken":"do-not-expose"}"#;
        write!(stream, "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
    });
    let mut s = Session::new();
    project.open(&mut s);
    let status = s.execute("defold.bridgeStatus", json!({})).unwrap();
    assert_eq!(status["reachable"], true);
    assert_eq!(status["onlineTransactions"], false);
    assert!(!status.to_string().contains("do-not-expose"));
    server.join().unwrap();
}
