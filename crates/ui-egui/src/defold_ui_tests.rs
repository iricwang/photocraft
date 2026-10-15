#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use crate::ai_service::{self, Job};
use base64::Engine;

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("game.project"), "[display]\nwidth=800\nheight=600\n").unwrap();
    std::fs::write(
        dir.path().join("scene.gui"),
        "nodes { id: \"button\" position {x:400 y:300} texture: \"ui/button\" size { x: 160 y: 80 } } textures {name: \"ui\" texture: \"/ui.atlas\"}",
    )
    .unwrap();
    std::fs::write(dir.path().join("ui.atlas"), "images {image: \"/button.png\"}").unwrap();
    std::fs::write(dir.path().join("button.png"), png()).unwrap();
    dir
}
fn png() -> Vec<u8> {
    let image = photocraft_codecs::Image::from_u8(8, 4, photocraft_codecs::ChannelLayout::Rgba, [255, 0, 0, 128].repeat(32)).unwrap();
    photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap()
}
#[test]
fn gui_ai_adapter_uses_existing_service_and_keeps_focus_draft_and_references() {
    let dir = project();
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let called = calls.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let rx = std::cell::RefCell::new(Some(rx));
    let mut app = PhotocraftApp::new(
        photocraft_engine::Session::new(),
        crate::Services {
            ai_generate: Some(Box::new(move |req| {
                called.set(called.get() + 1);
                assert_eq!(req.prompt, "Node draft");
                assert!(req.project.is_some());
                Ok(Job { cancel: Default::default(), result: rx.borrow_mut().take().unwrap() })
            })),
            ..Default::default()
        },
    );
    app.run("file.new", json!({"width":12,"height":12})).unwrap();
    crate::ai_project::sync_prompt(&mut app);
    app.ui.ai.prompt = "Image draft".into();
    crate::ai_project::remember_prompt(&mut app);
    app.background_jobs = true;
    app.open_path(&dir.path().join("scene.gui").to_string_lossy()).unwrap();
    assert!(app.jobs.opens.is_empty());
    assert!(app.session.active_gui().is_none());
    assert!(app.session.active().unwrap().doc.defold.is_some());
    crate::ai_project::sync_prompt(&mut app);
    app.ui.ai.prompt = "Node draft".into();
    let gui = app.session.active().unwrap().doc.id;
    let node = app.session.active().unwrap().doc.defold.as_ref().unwrap().nodes[0].layer;
    app.run("layer.select", json!({"layer":node})).unwrap();
    ai_service::start(&mut app).unwrap();
    assert_eq!(app.session.documents().len(), 2);
    assert_eq!(app.session.active().unwrap().doc.size.width, 800);
    crate::ai_project::sync_prompt(&mut app);
    app.session.set_active(0);
    crate::ai_project::sync_prompt(&mut app);
    let square = photocraft_codecs::Image::from_u8(8, 8, photocraft_codecs::ChannelLayout::Rgba, [0, 255, 0, 255].repeat(64)).unwrap();
    let generated = photocraft_codecs::encode(&square, photocraft_codecs::Format::Png, &Default::default()).unwrap();
    tx.send(Ok(base64::engine::general_purpose::STANDARD.encode(generated))).unwrap();
    ai_service::tick(&mut app, &egui::Context::default());
    assert_eq!(app.session.active().unwrap().doc.size.width, 12);
    app.session.set_active(1);
    crate::ai_project::sync_prompt(&mut app);
    assert_eq!(app.session.active().unwrap().doc.id, gui);
    assert_eq!(app.ui.ai.prompt, "Node draft");
    assert_eq!(calls.get(), 1);
    assert_eq!(app.session.documents()[1].doc.ai_generations.len(), 1, "{}", app.ui.status);
    let doc = &app.session.active().unwrap().doc;
    let result = doc.ai_generations[0].versions[0].layer.unwrap();
    assert_eq!(photocraft_engine::defold_design::node_for_layer(doc, result).unwrap().node, "button");
    assert_eq!(app.run("defold.syncDesign", json!({})).unwrap()["assets"], 1);
    app.session.set_active(0);
    crate::ai_project::sync_prompt(&mut app);
    assert_eq!(app.ui.ai.prompt, "Image draft");
}
#[test]
fn closing_dirty_gui_uses_native_document_prompt() {
    let dir = project();
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
    app.open_path(&dir.path().join("scene.gui").to_string_lossy()).unwrap();
    app.run("defold.setDesignProperties", json!({"values":{"position.x":10}})).unwrap();
    assert!(app.has_unsaved_work());
    assert!(crate::discard_ui::intercept(&mut app, "file.exit", &json!({})));
    assert!(app.ui.defold.close.is_none());
    assert!(app.discard.is_some());
    assert!(!crate::menus::modal_allows(&app, "file.new"));
}
#[test]
fn game_project_opens_gui_picker_without_creating_an_image_document() {
    let dir = project();
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
    app.background_jobs = true;
    app.open_path(&dir.path().join("game.project").to_string_lossy()).unwrap();
    assert!(app.ui.defold.project_open);
    assert_eq!(app.ui.defold.project_files, vec!["/scene.gui"]);
    assert!(app.session.documents().is_empty());
    assert!(app.jobs.opens.is_empty());
}

#[test]
fn project_picker_click_opens_nested_gui_from_canonical_windows_root() {
    use egui_kittest::kittest::Queryable;
    let dir = project();
    let folder = dir.path().join("界面 assets").join("nested");
    std::fs::create_dir_all(&folder).unwrap();
    let gui_path = folder.join("button.gui");
    std::fs::rename(dir.path().join("scene.gui"), &gui_path).unwrap();
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
    app.background_jobs = true;
    app.open_path(&dir.path().join("game.project").to_string_lossy()).unwrap();
    let canonical_root = dir.path().canonicalize().unwrap();
    assert_eq!(app.ui.defold.project_root, canonical_root.to_string_lossy());
    #[cfg(windows)]
    assert!(app.ui.defold.project_root.starts_with(r"\\?\"));
    let mut h =
        egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 900.0)).build_ui_state(|ui, app| super::panels::project_window(app, ui.ctx()), app);
    h.run_steps(4);
    h.get_by_label("/界面 assets/nested/button.gui").click();
    h.run_steps(4);
    assert!(!h.state().ui.defold.project_open, "click must open the GUI and close its picker");
    let doc = &h.state().session.active().expect("Native GUI opened from the project picker").doc;
    let link = doc.defold.as_ref().unwrap();
    assert_eq!(link.gui, gui_path.canonicalize().unwrap().to_string_lossy());
    assert_eq!(link.nodes[0].node, "button");
    assert_eq!(h.state().session.documents().len(), 1);
    assert!(h.state().session.active_gui().is_none());
    assert!(h.state().jobs.opens.is_empty());
    assert!(!h.state().ui.status_error, "{}", h.state().ui.status);
}

#[test]
fn preview_framing_includes_nodes_outside_the_display() {
    let scene =
        photocraft_defold_format::Scene::parse("nodes {id: \"info\" type: TYPE_TEXT position {x:540 y:2280} scale {x:1.5 y:1.5} size {x:1000 y:60}}".into())
            .unwrap();
    let frame = egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 500.0));
    let (board, zoom) = framed_board(&scene.nodes, 2340.0, 1080.0, frame, None);
    assert!((zoom - 500.0 / 2325.0).abs() < 1e-6);
    let center = Pos2::new(board.left() + 540.0 * zoom, board.bottom() - 2280.0 * zoom);
    assert!(frame.contains(center));
    assert!(center.y < 20.0);
}
