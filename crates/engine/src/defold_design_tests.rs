#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;

fn fixture() -> (tempfile::TempDir, Session) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("game.project"), "[display]\nwidth=96\nheight=96\n").unwrap();
    std::fs::write(dir.path().join("main.gui"),"nodes {id: \"root\" type: TYPE_BOX position {x:48 y:48} size {x:64 y:64} alpha: 0}\nnodes {id: \"button\" parent: \"root\" type: TYPE_BOX size {x:32 y:32} color {x:0 y:0 z:1 w:1} }\n").unwrap();
    let mut s = Session::new();
    s.execute("defold.openDesign", json!({"path":dir.path().join("main.gui")})).unwrap();
    let art = s.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "button").unwrap().artwork;
    s.execute("layer.select", json!({"layer":art})).unwrap();
    (dir, s)
}
#[test]
fn native_gui_paint_erase_undo_save_reopen_and_publish() {
    let (dir, mut s) = fixture();
    assert!(s.active_gui().is_none());
    assert!(s.is_enabled("paint.stroke"));
    let before = std::fs::read(dir.path().join("main.gui")).unwrap();
    let button = s.active().unwrap().active_layer.unwrap();
    assert!(s.active().unwrap().doc.layer(button).unwrap().surface().unwrap().rgba(48, 48)[2] > 0.9);
    let composite = photocraft_compose::render(&s.active().unwrap().doc, Rect::new(48, 48, 49, 49));
    assert!(composite.px[0][3] > 0.9, "transparent parent must not erase its child");
    s.execute("paint.stroke", json!({"points":[[40,48],[56,48]],"size":10,"hardness":1.0,"color":"#ff0000"})).unwrap();
    let painted = s.active().unwrap().doc.clone();
    assert!(painted.layer(button).unwrap().surface().unwrap().rgba(48, 48)[0] > 0.9);
    s.execute("paint.stroke", json!({"points":[[44,48],[52,48]],"size":6,"hardness":1.0,"erase":true})).unwrap();
    assert!(s.active().unwrap().doc.layer(button).unwrap().surface().unwrap().rgba(48, 48)[3] < 0.1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.layer(button), painted.layer(button));
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(std::fs::read(dir.path().join("main.gui")).unwrap(), before, "editing stays local until sync");
    let result = s.execute("defold.testLegacySync", json!({})).unwrap();
    assert_eq!(result["assets"], 1);
    let disk = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert!(disk.nodes.iter().find(|n| n.id == "button").unwrap().texture.starts_with("pc_"));
    let mut exported = Session::new();
    exported.execute("defold.openGui", json!({"path":dir.path().join("main.gui")})).unwrap();
    let png = crate::defold_cmds::preview_png(&exported, "button").unwrap();
    let image = photocraft_io::import("node.png", &png).unwrap().document;
    assert_eq!(image.size, Size::new(32, 32), "all four local coordinate quadrants must survive export");
    let raster = image.layers[0].surface().unwrap();
    assert!(raster.rgba(0, 0)[2] > 0.9, "uncropped blue corner");
    assert!(raster.rgba(6, 16)[0] > 0.9, "paint survives publication");
    assert!(raster.rgba(16, 16)[3] < 0.1, "eraser transparency survives publication");
    assert_eq!(disk.nodes.iter().find(|n| n.id == "root").unwrap().alpha, 0.0);
    let mut reopened = Session::new();
    reopened.execute("defold.openDesign", json!({"path":dir.path().join("main.gui")})).unwrap();
    assert_eq!(reopened.active().unwrap().doc.layers, s.active().unwrap().doc.layers);
    assert_eq!(reopened.active().unwrap().doc.defold, s.active().unwrap().doc.defold);
}
#[test]
fn native_properties_keep_painted_pixels_and_use_native_history() {
    let (dir, mut s) = fixture();
    s.execute("paint.stroke", json!({"points":[[40,48],[56,48]],"size":10,"color":"#ff0000"})).unwrap();
    let before = s.active().unwrap().doc.clone();
    s.execute("defold.setDesignProperties", json!({"values":{"position.x":8,"alpha":0.75}})).unwrap();
    assert_eq!(scene(&s).unwrap().nodes.iter().find(|n| n.id == "button").unwrap().position[0], 8.0);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.layers, before.layers);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.execute("defold.testLegacySync", json!({})).unwrap()["assets"], 1, "property edits must not mark painted artwork pristine");
    assert!(dir.path().join("main.gui.pcraft").is_file());
}
#[test]
fn native_node_create_delete_and_undo_sync_as_hierarchy() {
    let (dir, mut s) = fixture();
    let id = s.execute("defold.addDesignNode", json!({"child":true})).unwrap()["node"].as_str().unwrap().to_string();
    s.execute("paint.stroke", json!({"points":[[45,45],[50,50]],"size":5,"color":"#00ff00"})).unwrap();
    s.execute("defold.testLegacySync", json!({})).unwrap();
    let disk = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert_eq!(disk.nodes.iter().find(|n| n.id == id).unwrap().parent, "button");
    let group = s.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == id).unwrap().layer;
    s.execute("layer.select", json!({"layer":group})).unwrap();
    s.execute("layer.delete", json!({})).unwrap();
    s.execute("defold.testLegacySync", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("defold.testLegacySync", json!({})).unwrap();
    let disk = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert!(disk.nodes.iter().any(|n| n.id == id));
}
#[test]
fn external_gui_changes_are_not_overwritten() {
    let (dir, mut s) = fixture();
    std::fs::write(dir.path().join("main.gui"), "nodes {id:\"external\"}").unwrap();
    assert!(s.execute("defold.testLegacySync", json!({})).is_err());
    assert_eq!(std::fs::read_to_string(dir.path().join("main.gui")).unwrap(), "nodes {id:\"external\"}");
}

#[test]
fn native_text_tool_keeps_runtime_text_node_and_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("game.project"), "[display]\nwidth=128\nheight=128\n").unwrap();
    let path = dir.path().join("text.gui");
    std::fs::write(&path, "nodes { id: \"label\" type: TYPE_TEXT text: \"Old\" position {x:64 y:64} size {x:100 y:30} }").unwrap();
    let mut s = Session::new();
    s.execute("defold.openDesign", json!({"path":path})).unwrap();
    s.execute("type.edit", json!({"text":"New label"})).unwrap();
    let result = s.execute("defold.testLegacySync", json!({})).unwrap();
    assert_eq!(result["assets"], 0);
    let disk = Scene::parse(std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(disk.nodes[0].kind, "TYPE_TEXT");
    assert_eq!(disk.nodes[0].text, "New label");
    s.execute("defold.testLegacySync", json!({})).unwrap();
    assert_eq!(Scene::parse(std::fs::read_to_string(&path).unwrap()).unwrap().nodes[0].text, "New label");
}

#[test]
fn invalid_properties_leave_native_history_unchanged() {
    let (_dir, mut s) = fixture();
    let before = s.active().unwrap().doc.clone();
    for p in [json!({}), json!({"values":{"size.x":-1}}), json!({"values":{"id":"root"}}), json!({"values":{"alpha":"bad"}})] {
        assert!(s.execute("defold.setDesignProperties", p).is_err());
        assert_eq!(s.active().unwrap().doc.layers, before.layers);
    }
}

#[test]
fn moving_node_does_not_reset_native_layer_opacity() {
    let (_dir, mut s) = fixture();
    let art = s.active().unwrap().active_layer.unwrap();
    s.execute("layer.setProps", json!({"opacity":0.4})).unwrap();
    s.execute("defold.setDesignProperties", json!({"values":{"position.x":8}})).unwrap();
    assert!((s.active().unwrap().doc.layer(art).unwrap().opacity - 0.4).abs() < 0.001);
    assert_eq!(s.execute("defold.testLegacySync", json!({})).unwrap()["assets"], 1);
}

#[test]
fn parent_mask_is_applied_to_child_artwork_publication() {
    let (dir, mut s) = fixture();
    let root = s.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "root").unwrap().layer;
    s.execute("layer.select", json!({"layer":root})).unwrap();
    s.execute("layer.layerMask.hideAll", json!({})).unwrap();
    let doc = &s.active().unwrap().doc;
    let node = doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "button").unwrap();
    let bitmap = rendered(doc, own_layer(doc, node).unwrap()).unwrap();
    assert!(bitmap.px.iter().all(|p| p[3] == 0.0));
    assert_eq!(s.execute("defold.testLegacySync", json!({})).unwrap()["assets"], 0, "fully masked artwork needs no placeholder PNG");
    let output = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert!(!output.nodes.iter().find(|n| n.id == "button").unwrap().visible);
}

#[test]
fn gui_canvas_uses_design_resolution_and_keeps_offscreen_node_artwork() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("game.project"), "\u{feff}[display]\nwidth=200\nheight=100\nwindow_width=800\nwindow_height=400\n").unwrap();
    let path = dir.path().join("main.gui");
    std::fs::write(&path,"nodes {id: \"root\" position {x:100 y:50} size {x:400 y:800} alpha:0}\nnodes {id: \"button\" parent: \"root\" size {x:32 y:32} color {x:0 y:0 z:1}}\nnodes {id: \"outside\" position {x:-60 y:200} size {x:32 y:32}}\nnodes {id: \"far\" position {x:20000 y:20000} size {x:2 y:2}}\n").unwrap();
    let mut s = Session::new();
    s.execute("defold.openDesign", json!({"path":path})).unwrap();
    let doc = &s.active().unwrap().doc;
    assert_eq!(doc.size, Size::new(200, 100));
    let button = doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "button").unwrap().artwork;
    assert_eq!(doc.defold.as_ref().unwrap().canvas_origin, [0, 0]);
    assert_eq!(scene_bounds(doc).unwrap().0, Rect::new(-76, -116, 200, 100));
    assert_eq!(doc.layer(LayerId(button)).unwrap().surface().unwrap().content_bounds(), Rect::new(84, 34, 116, 66));
    assert_eq!(s.execute("defold.testLegacySync", json!({})).unwrap()["assets"], 0);
    let outside = s.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "outside").unwrap().artwork;
    s.execute("layer.select", json!({"layer":outside})).unwrap();
    s.execute("layer.setProps", json!({"opacity":0.5})).unwrap();
    assert_eq!(s.execute("defold.testLegacySync", json!({})).unwrap()["assets"], 1);
    let disk = Scene::parse(std::fs::read_to_string(&path).unwrap()).unwrap();
    let node = disk.nodes.iter().find(|n| n.id == "outside").unwrap();
    assert_eq!(node.position, [-60.0, 200.0, 0.0]);
    assert_eq!(node.size, [32.0, 32.0, 0.0]);
    let mut exported = Session::new();
    exported.execute("defold.openGui", json!({"path":path})).unwrap();
    let png = crate::defold_cmds::preview_png(&exported, "outside").unwrap();
    let image = photocraft_io::import("outside.png", &png).unwrap().document;
    assert_eq!(image.size, Size::new(32, 32));
    assert!((image.layers[0].surface().unwrap().rgba(16, 16)[3] - 0.5).abs() < 0.01);
}

#[test]
fn legacy_expanded_canvas_migrates_pixels_masks_and_dirty_baselines_once() {
    let (dir, mut s) = fixture();
    let path = dir.path().join("main.gui");
    let mut old = (*s.active().unwrap().doc).clone();
    crate::image_cmds::translate_doc(&mut old, "test", 0, 504).unwrap();
    old.size = Size::new(300, 600);
    old.defold.as_mut().unwrap().version = 1;
    let bindings = old.defold.as_ref().unwrap().nodes.clone();
    for b in &bindings {
        let stamp = appearance(&old, b).unwrap();
        old.defold.as_mut().unwrap().nodes.iter_mut().find(|n| n.layer == b.layer).unwrap().appearance = stamp;
    }
    let button = bindings.iter().find(|n| n.node == "button").unwrap().artwork;
    s = Session::new();
    s.add_document(old, None);
    s.execute("layer.select", json!({"layer":button})).unwrap();
    s.execute("paint.stroke", json!({"points":[[40,552],[56,552]],"size":10,"hardness":1.0,"color":"#ff0000"})).unwrap();
    s.execute("layer.layerMask.revealAll", json!({})).unwrap();
    let mut old = (*s.active().unwrap().doc).clone();
    old.layer_mut(LayerId(button)).unwrap().locks.all = true;
    old.ai_generations.push(photocraft_doc::ai::GenerationGroup {
        id: 1,
        canvas: old.size,
        area: Rect::new(40, 544, 56, 560),
        frame: None,
        selection: None,
        reference_png: None,
        context_png: None,
        mode: photocraft_doc::ai::GenerationMode::Repaint,
        versions: vec![],
    });
    let bytes = photocraft_format::save_to_bytes(&old, &Default::default()).unwrap();
    std::fs::write(dir.path().join("main.gui.pcraft"), &bytes).unwrap();
    let mut restored = Session::new();
    restored.execute("defold.openDesign", json!({"path":path})).unwrap();
    let doc = &restored.active().unwrap().doc;
    assert_eq!(doc.size, Size::new(96, 96));
    assert_eq!(doc.defold.as_ref().unwrap().version, 3);
    assert_eq!(doc.ai_generations[0].canvas, Size::new(96, 96));
    assert_eq!(doc.ai_generations[0].area, Rect::new(40, 40, 56, 56));
    assert!(doc.ai_generations[0].is_valid());
    assert!(doc.layer(LayerId(button)).unwrap().locks.all);
    assert!(doc.layer(LayerId(button)).unwrap().surface().unwrap().rgba(48, 48)[0] > 0.9);
    assert!(doc.layer(LayerId(button)).unwrap().mask.is_some());
    assert_eq!(restored.execute("defold.testLegacySync", json!({})).unwrap()["assets"], 1);
    let layer_tree = restored.active().unwrap().doc.layers.clone();
    let mut reopened = Session::new();
    reopened.execute("defold.openDesign", json!({"path":path})).unwrap();
    assert_eq!(reopened.active().unwrap().doc.layers, layer_tree);
    let mut native = Session::new();
    crate::file_cmds::open_bytes_as(&mut native, "main.gui.pcraft", &bytes, None, None).unwrap();
    assert_eq!(native.active().unwrap().doc.size, Size::new(96, 96));
    assert_eq!(native.active().unwrap().doc.layers, reopened.active().unwrap().doc.layers);
    let mut background = Session::new();
    if let crate::jobs::Started::Job(id) = background.start_open("main.gui.pcraft", crate::jobs::OpenSource::Bytes(Arc::new(bytes))).unwrap() {
        background.wait_job(id).unwrap();
    }
    assert_eq!(background.active().unwrap().doc.size, Size::new(96, 96));
    assert_eq!(background.active().unwrap().doc.layers, reopened.active().unwrap().doc.layers);
}

#[test]
fn changed_project_resolution_reanchors_without_changing_gui_positions() {
    let (dir, mut s) = fixture();
    s.execute("defold.testLegacySync", json!({})).unwrap();
    std::fs::write(dir.path().join("game.project"), "[display]\nwidth=160\nheight=144\n").unwrap();
    let mut reopened = Session::new();
    reopened.execute("defold.openDesign", json!({"path":dir.path().join("main.gui")})).unwrap();
    let doc = &reopened.active().unwrap().doc;
    let button = doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "button").unwrap().artwork;
    assert_eq!(doc.size, Size::new(160, 144));
    assert!(doc.layer(LayerId(button)).unwrap().surface().unwrap().rgba(48, 96)[2] > 0.9);
    assert_eq!(reopened.execute("defold.testLegacySync", json!({})).unwrap()["assets"], 0);
    // Resolution changes while a document is open are handled at publication too.
    assert_eq!(s.execute("defold.testLegacySync", json!({})).unwrap()["assets"], 0);
    assert_eq!(s.active().unwrap().doc.size, Size::new(160, 144));
}

#[test]
fn invalid_project_dimensions_are_not_silently_replaced_by_defaults() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.gui"), "nodes {id:\"n\"}").unwrap();
    for width in ["0", "-1", "1.5", "NaN", "16385"] {
        std::fs::write(dir.path().join("game.project"), format!("[display]\nwidth={width}\nheight=100\n")).unwrap();
        assert!(Session::new().execute("defold.openDesign", json!({"path":dir.path().join("main.gui")})).is_err());
    }
}

#[test]
fn canvas_migration_keeps_an_unsynced_node_deletion() {
    let (_dir, mut s) = fixture();
    let group = s.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "button").unwrap().layer;
    s.execute("layer.select", json!({"layer":group})).unwrap();
    s.execute("layer.delete", json!({})).unwrap();
    let mut old = (*s.active().unwrap().doc).clone();
    old.defold.as_mut().unwrap().version = 1;
    assert!(normalize_canvas(&mut old, Size::new(96, 96)).unwrap());
    assert!(old.layer(LayerId(group)).is_none());
    assert!(!reconcile(&mut old).unwrap().nodes.iter().any(|n| n.id == "button"));
}

#[test]
fn legacy_portrait_board_returns_to_defold_reference_without_flipping_pixels() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("game.project"), "[display]\nwidth=160\nheight=80\n").unwrap();
    let path = dir.path().join("main.gui");
    std::fs::write(&path, "nodes {id:\"root\" position {x:40 y:80} size {x:80 y:160} alpha:0}\nnodes {id:\"button\" parent:\"root\" position {x:0 y:50} size {x:16 y:16} color {x:0 y:0 z:1}}").unwrap();
    let mut s = Session::new();
    s.execute("defold.openDesign", json!({"path":path})).unwrap();
    assert_eq!(s.active().unwrap().doc.size, Size::new(160, 80));
    let art = s.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "button").unwrap().artwork;
    s.execute("layer.select", json!({"layer":art})).unwrap();
    s.execute("paint.stroke", json!({"points":[[40,-50]],"size":4,"hardness":1.0,"color":"#ff0000"})).unwrap();
    let mut old = (*s.active().unwrap().doc).clone();
    // Simulate the previously shipped portrait-board heuristic.
    normalize_canvas(&mut old, Size::new(80, 160)).unwrap();
    let bytes = photocraft_format::save_to_bytes(&old, &Default::default()).unwrap();
    std::fs::write(dir.path().join("main.gui.pcraft"), &bytes).unwrap();
    let mut restored = Session::new();
    restored.execute("defold.openDesign", json!({"path":path})).unwrap();
    assert_eq!(restored.active().unwrap().doc.size, Size::new(160, 80));
    let pixels = restored.active().unwrap().doc.layer(LayerId(art)).unwrap().surface().unwrap();
    assert!(pixels.rgba(40, -50)[0] > 0.9);
    assert!(pixels.rgba(33, -57)[2] > 0.9);
    assert_eq!(restored.execute("defold.testLegacySync", json!({})).unwrap()["assets"], 1);
    let disk = Scene::parse(std::fs::read_to_string(&path).unwrap()).unwrap();
    let button = disk.nodes.iter().find(|n| n.id == "button").unwrap();
    assert_eq!(button.position, [0.0, 50.0, 0.0]);
    assert_eq!(button.size, [16.0, 16.0, 0.0]);
    assert_eq!(restored.active().unwrap().doc.size, Size::new(160, 80));
    let mut native = Session::new();
    crate::file_cmds::open_bytes_as(&mut native, "main.gui.pcraft", &bytes, None, None).unwrap();
    assert_eq!(native.active().unwrap().doc.size, Size::new(160, 80));
    assert_eq!(native.active().unwrap().doc.layers, restored.active().unwrap().doc.layers);
}

#[test]
fn negative_gui_coordinates_fit_paint_properties_and_publication() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("game.project"), "[display]\nwidth=160\nheight=80\n").unwrap();
    let path = dir.path().join("character.gui");
    let source =
        "nodes {id:\"root\" position {y:45} size {y:90}}\nnodes {id:\"panel\" parent:\"root\" position {x:-30 y:-65} size {x:32 y:16} color {x:0 y:0 z:1}}";
    std::fs::write(&path, source).unwrap();
    let mut s = Session::new();
    s.execute("defold.openDesign", json!({"path":path})).unwrap();
    let doc = &s.active().unwrap().doc;
    assert_eq!(doc.defold.as_ref().unwrap().canvas_origin, [0, 0]);
    assert_eq!(doc.size, Size::new(160, 80));
    let art = doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "panel").unwrap().artwork;
    assert_eq!(doc.layer(LayerId(art)).unwrap().surface().unwrap().content_bounds(), Rect::new(-46, 92, -14, 108));
    s.execute("layer.select", json!({"layer":art})).unwrap();
    s.execute("paint.stroke", json!({"points":[[-30,100]],"size":4,"hardness":1.0,"color":"#ff0000"})).unwrap();
    s.execute("defold.setDesignProperties", json!({"values":{"position.x":-25}})).unwrap();
    assert!(s.active().unwrap().doc.layer(LayerId(art)).unwrap().surface().unwrap().rgba(-25, 100)[0] > 0.9);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.active().unwrap().doc.layer(LayerId(art)).unwrap().surface().unwrap().rgba(-30, 100)[0] > 0.9);
    assert_eq!(s.execute("defold.testLegacySync", json!({})).unwrap()["assets"], 1);
    let saved = Scene::parse(std::fs::read_to_string(&path).unwrap()).unwrap();
    let panel = saved.nodes.iter().find(|n| n.id == "panel").unwrap();
    assert_eq!(panel.position, [-30.0, -65.0, 0.0]);
    assert_eq!(panel.size, [32.0, 16.0, 0.0]);
    let mut exported = Session::new();
    exported.execute("defold.openGui", json!({"path":path})).unwrap();
    let png = crate::defold_cmds::preview_png(&exported, "panel").unwrap();
    let image = photocraft_io::import("panel.png", &png).unwrap().document;
    assert_eq!(image.size, Size::new(32, 16));
    assert!(image.layers[0].surface().unwrap().rgba(16, 8)[0] > 0.9);
    let mut reopened = Session::new();
    reopened.execute("defold.openDesign", json!({"path":path})).unwrap();
    assert_eq!(reopened.active().unwrap().doc.layers, s.active().unwrap().doc.layers);
    assert_eq!(reopened.active().unwrap().doc.defold, s.active().unwrap().doc.defold);
}

#[test]
fn unchanged_parent_space_position_does_not_drift_on_sync() {
    let (dir, mut s) = fixture();
    let y = 1.600200057029724_f64;
    s.execute("defold.setDesignProperties", json!({"values":{"position.y":y}})).unwrap();
    assert_eq!(s.execute("defold.testLegacySync", json!({})).unwrap()["assets"], 0);
    let scene = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert_eq!(scene.nodes.iter().find(|n| n.id == "button").unwrap().position[1], y);
}

#[test]
fn live_preflight_accepts_defold_float_rounding_but_rejects_edits() {
    for size in [1616.14, 684.64, 784.8, 618.03, 1462.81, 812.212] {
        let disk = Scene::parse(format!("nodes {{id:\"label\" size {{x:{size} y:32}}}}")).unwrap();
        let live = disk.edit("label", "size.x", &json!(size as f32 as f64)).unwrap();
        assert!(matches_nodes(&disk, &live), "normal float conversion: {size}");
        let changed = live.edit("label", "size.x", &json!(size + 0.01)).unwrap();
        assert!(!matches_nodes(&disk, &changed), "real size changes remain protected");
        let renamed = live.rename_node("label", "other").unwrap();
        assert!(!matches_nodes(&disk, &renamed));
    }
}

#[test]
fn ordinary_new_layers_publish_inside_nodes_and_at_root() {
    let (dir, mut s) = fixture();
    let child = s.execute("layer.new.layer", json!({"name":"New artwork"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("paint.stroke", json!({"points":[[45,45]],"size":8,"color":"#ff0000"})).unwrap();
    assert_eq!(s.execute("defold.testLegacySync", json!({})).unwrap()["assets"], 1);
    let disk = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert_eq!(disk.nodes.len(), 3, "own Artwork layers must not become extra nodes");
    let node = disk.nodes.iter().find(|n| n.id == "New artwork").unwrap();
    assert_eq!(node.parent, "button");
    assert!(!node.texture.is_empty());
    assert!(disk.nodes.iter().find(|n| n.id == "button").unwrap().texture.is_empty(), "child paint must not replace the parent's artwork");
    let root = s.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "root").unwrap().layer;
    s.execute("layer.select", json!({"layer":root})).unwrap();
    let _layer = s.execute("layer.new.layer", json!({"name":"Top level artwork"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("paint.stroke", json!({"points":[[15,15]],"size":8,"color":"#00ff00"})).unwrap();
    s.execute("defold.testLegacySync", json!({})).unwrap();
    let disk = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    let node = disk.nodes.iter().find(|n| n.id == "Top level artwork").unwrap();
    assert!(node.parent.is_empty());
    assert!(!node.texture.is_empty());
    s.execute("defold.testLegacySync", json!({})).unwrap();
    let mut reopened = Session::new();
    reopened.execute("defold.openDesign", json!({"path":dir.path().join("main.gui")})).unwrap();
    reopened.execute("defold.testLegacySync", json!({})).unwrap();
    let disk = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert_eq!(disk.nodes.len(), 4);
    assert_eq!(disk.nodes.iter().find(|n| n.id == "New artwork").unwrap().parent, "button");
    reopened.execute("layer.select", json!({"layer":child})).unwrap();
    reopened.execute("layer.delete", json!({})).unwrap();
    reopened.execute("defold.testLegacySync", json!({})).unwrap();
    let disk = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert_eq!(disk.nodes.len(), 3);
    assert!(disk.nodes.iter().all(|n| n.id != "New artwork"));
    reopened.execute("edit.undo", json!({})).unwrap();
    reopened.execute("defold.testLegacySync", json!({})).unwrap();
    let disk = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert_eq!(disk.nodes.iter().find(|n| n.id == "New artwork").unwrap().parent, "button");
}

#[test]
fn generated_node_artwork_stays_with_parent_while_new_empty_layer_is_a_child() {
    let (dir, mut s) = fixture();
    let target = s.active().unwrap().active_layer.unwrap();
    let owner = node_for_layer(&s.active().unwrap().doc, target).unwrap().layer;
    s.edit("AI artwork", |doc, active| {
        let id = insert_artwork(doc, target, Layer::raster("AI result", PixelFormat::RGBA8))?;
        *active = Some(id);
        Ok(())
    })
    .unwrap();
    s.execute("paint.stroke", json!({"points":[[45,45]],"size":8,"color":"#ff0000"})).unwrap();
    let art_root = s.active().unwrap().doc.layer(LayerId(owner)).unwrap().children().unwrap()[0].id;
    s.execute("layer.select", json!({"layer":art_root.0})).unwrap();
    let _empty = s.execute("layer.new.layer", json!({"name":"Empty child"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("defold.testLegacySync", json!({})).unwrap();
    let disk = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert_eq!(disk.nodes.len(), 3);
    assert_eq!(disk.nodes.iter().find(|n| n.id == "Empty child").unwrap().parent, "button");
    assert!(disk.nodes.iter().find(|n| n.id == "button").unwrap().texture.starts_with("pc_"));
    assert!(disk.nodes.iter().all(|n| n.id != "AI result"));
}

#[test]
fn publication_reuses_content_assets_and_manual_edits_do_not_write_project() {
    let (dir, mut s) = fixture();
    let original = std::fs::read(dir.path().join("main.gui")).unwrap();
    s.execute("paint.stroke", json!({"points":[[40,48]],"size":8,"color":"#ff0000"})).unwrap();
    s.execute("layer.translate", json!({"dx":4,"dy":0})).unwrap();
    assert_eq!(std::fs::read(dir.path().join("main.gui")).unwrap(), original);
    assert!(!dir.path().join("photocraft_generated").exists());
    s.execute("defold.testLegacySync", json!({})).unwrap();
    fn inventory(dir: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
        let mut result = std::collections::BTreeMap::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                result.extend(inventory(&path));
            } else {
                result.insert(path.clone(), std::fs::read(path).unwrap());
            }
        }
        result
    }
    let assets = dir.path().join("photocraft_generated");
    let first = inventory(&assets);
    assert_eq!(first.keys().filter(|p| p.extension().is_some_and(|e| e == "atlas")).count(), 1);
    assert_eq!(first.keys().filter(|p| p.extension().is_some_and(|e| e == "json")).count(), 1);
    s.execute("defold.testLegacySync", json!({})).unwrap();
    assert_eq!(first, inventory(&assets), "unchanged sync must reuse every resource");
    let group = s.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "button").unwrap().layer;
    s.execute("layer.select", json!({"layer":group})).unwrap();
    s.execute("layer.translate", json!({"dx":8,"dy":0})).unwrap();
    assert_eq!(first, inventory(&assets), "moving stays local");
    s.execute("defold.testLegacySync", json!({})).unwrap();
    assert_eq!(first, inventory(&assets), "node placement does not create a new texture version");
}

#[test]
fn identical_node_artwork_keeps_readable_names_in_one_gui_atlas() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("game.project"), "[display]\nwidth=128\nheight=128\n").unwrap();
    std::fs::write(
        dir.path().join("main.gui"),
        r#"nodes {id:"a" position {x:32 y:64} size {x:16 y:16} color {x:0 y:0 z:1}}
nodes {id:"b" position {x:96 y:64} size {x:16 y:16} color {x:0 y:0 z:1}}"#,
    )
    .unwrap();
    let mut s = Session::new();
    s.execute("defold.openDesign", json!({"path":dir.path().join("main.gui")})).unwrap();
    for (node, x) in [("a", 32), ("b", 96)] {
        let art = s.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == node).unwrap().artwork;
        s.execute("layer.select", json!({"layer":art})).unwrap();
        s.edit("identical artwork", |doc, _| {
            doc.layer_mut(LayerId(art)).unwrap().surface_mut().unwrap().fill_rect(Rect::new(x - 2, 62, x + 2, 66), &[1.0, 0.0, 0.0, 1.0]);
            Ok(())
        })
        .unwrap();
    }
    assert_eq!(s.execute("defold.testLegacySync", json!({})).unwrap()["assets"], 2);
    let out = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert_ne!(out.nodes[0].texture, out.nodes[1].texture);
    assert!(out.nodes[0].texture.ends_with("/a"));
    assert!(out.nodes[1].texture.ends_with("/b"));
    let textures: Vec<_> = out.proto.fields.iter().filter(|f| f.name == "textures").collect();
    assert_eq!(textures.len(), 1);
    let atlas = out.proto.text(&textures[0].children, "texture");
    let atlas = photocraft_defold_format::TextProto::parse(std::fs::read_to_string(dir.path().join(atlas.trim_start_matches('/'))).unwrap()).unwrap();
    assert_eq!(atlas.fields.iter().filter(|f| f.name == "images").count(), 2);
}

fn reopened_published_artwork() -> (tempfile::TempDir, Session, String) {
    let (dir, mut source) = fixture();
    source.execute("paint.stroke", json!({"points":[[40,48]],"size":8,"color":"#ff0000"})).unwrap();
    source.execute("defold.testLegacySync", json!({})).unwrap();
    let gui = dir.path().join("main.gui");
    let scene = Scene::parse(std::fs::read_to_string(&gui).unwrap()).unwrap();
    let texture = scene.nodes.iter().find(|n| n.id == "button").unwrap().texture.clone();
    // Import the published GUI as another artist/machine would, without a native source.
    std::fs::remove_file(dir.path().join("main.gui.pcraft")).unwrap();
    let mut imported = Session::new();
    imported.execute("defold.openDesign", json!({"path":gui})).unwrap();
    let art = imported.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "button").unwrap().artwork;
    imported.execute("layer.select", json!({"layer":art})).unwrap();
    (dir, imported, texture)
}

#[test]
fn later_publication_keeps_unchanged_generated_textures_in_the_shared_atlas() {
    let (dir, mut s, original_texture) = reopened_published_artwork();
    let _new = s.execute("layer.new.layer", json!({"name":"Second published artwork"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("paint.stroke", json!({"points":[[50,48]],"size":8,"color":"#00ff00"})).unwrap();
    assert_eq!(s.execute("defold.testLegacySync", json!({})).unwrap()["assets"], 1);
    let scene = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert_eq!(scene.nodes.iter().find(|n| n.id == "button").unwrap().texture, original_texture);
    let texture = scene.nodes.iter().find(|n| n.id == "Second published artwork").unwrap().texture.clone();
    assert_ne!(texture, original_texture);
    let alias = original_texture.split('/').next().unwrap();
    let field = scene.proto.fields.iter().find(|f| f.name == "textures" && scene.proto.text(&f.children, "name") == alias).unwrap();
    let atlas_path = scene.proto.text(&field.children, "texture");
    let atlas = photocraft_defold_format::TextProto::parse(std::fs::read_to_string(dir.path().join(atlas_path.trim_start_matches('/'))).unwrap()).unwrap();
    let images: Vec<_> = atlas.fields.iter().filter(|f| f.name == "images").map(|f| atlas.text(&f.children, "image")).collect();
    assert_eq!(images.len(), 2, "old artwork must survive publication of only the new layer");
    for texture in [&original_texture, &texture] {
        let stem = texture.split('/').nth(1).unwrap();
        assert!(images.iter().any(|image| Path::new(image).file_stem().unwrap() == stem));
    }
}

#[test]
fn missing_retained_texture_stops_before_gui_or_assets_are_published() {
    let (dir, mut s, texture) = reopened_published_artwork();
    let gui = dir.path().join("main.gui");
    let before = std::fs::read(&gui).unwrap();
    let scene = Scene::parse(String::from_utf8(before.clone()).unwrap()).unwrap();
    let alias = texture.split('/').next().unwrap();
    let field = scene.proto.fields.iter().find(|f| f.name == "textures" && scene.proto.text(&f.children, "name") == alias).unwrap();
    let atlas_path = dir.path().join(scene.proto.text(&field.children, "texture").trim_start_matches('/'));
    let atlas = photocraft_defold_format::TextProto::parse(std::fs::read_to_string(&atlas_path).unwrap()).unwrap();
    let image = atlas.fields.iter().find(|f| f.name == "images").unwrap();
    std::fs::remove_file(dir.path().join(atlas.text(&image.children, "image").trim_start_matches('/'))).unwrap();
    s.execute("layer.new.layer", json!({"name":"New artwork"})).unwrap();
    s.execute("paint.stroke", json!({"points":[[50,48]],"size":8,"color":"#00ff00"})).unwrap();
    assert!(s.execute("defold.testLegacySync", json!({})).unwrap_err().to_string().contains("Missing generated texture"));
    assert_eq!(std::fs::read(&gui).unwrap(), before);
    assert_eq!(std::fs::read_dir(atlas_path.parent().unwrap()).unwrap().count(), 1);
}

#[test]
fn named_images_update_in_place_and_rename_retires_the_previous_file() {
    let (dir, mut s) = fixture();
    let layer = s.execute("layer.new.layer", json!({"name":"RewardIcon"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("paint.stroke", json!({"points":[[45,45]],"size":8,"color":"#ff0000"})).unwrap();
    s.execute("defold.testLegacySync", json!({})).unwrap();
    let node = || Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    let scene = node();
    assert!(scene.nodes.iter().any(|n| n.id == "RewardIcon" && n.texture.ends_with("/RewardIcon")));
    let link = s.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|b| b.layer == layer).unwrap().clone();
    let folder = dir.path().join("photocraft_generated");
    fn files(root: &Path) -> Vec<std::path::PathBuf> {
        let mut result = vec![];
        for e in std::fs::read_dir(root).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                result.extend(files(&p));
            } else {
                result.push(p);
            }
        }
        result.sort();
        result
    }
    let inventory = files(&folder);
    let image = inventory.iter().find(|p| p.file_name().unwrap() == "RewardIcon.png").unwrap().clone();
    let original = std::fs::read(&image).unwrap();
    s.execute("paint.stroke", json!({"points":[[46,45]],"size":5,"color":"#00ff00"})).unwrap();
    s.execute("defold.testLegacySync", json!({})).unwrap();
    assert_eq!(files(&folder), inventory, "pixel edits update the existing resource files");
    assert_ne!(std::fs::read(&image).unwrap(), original);
    s.edit("Rename layer", |doc, _| {
        doc.layer_mut(LayerId(layer)).unwrap().name = "PrizeIcon".into();
        Ok(())
    })
    .unwrap();
    s.execute("defold.testLegacySync", json!({})).unwrap();
    let renamed = s.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|b| b.layer == layer).unwrap();
    assert_eq!(renamed.asset_id, link.asset_id);
    assert_eq!(renamed.node, "RewardIcon", "a layer display-name edit must preserve the script-facing GUI ID");
    assert_eq!(node().nodes.len(), 3);
    assert!(!image.exists());
    assert!(image.with_file_name("PrizeIcon.png").exists());
    assert_eq!(files(&folder).len(), inventory.len());
    s.execute("defold.testLegacySync", json!({})).unwrap();
    assert_eq!(files(&folder).len(), inventory.len());
}
#[test]
fn duplicate_names_use_stable_suffixes_and_foreign_pixel_changes_are_rejected() {
    let (dir, mut s) = fixture();
    for _ in 0..2 {
        s.execute("layer.new.layer", json!({"name":"Reward"})).unwrap();
        s.execute("paint.stroke", json!({"points":[[45,45]],"size":8,"color":"#ff0000"})).unwrap();
    }
    s.execute("defold.testLegacySync", json!({})).unwrap();
    let scene = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    let rewards: Vec<_> = scene.nodes.iter().filter(|n| n.id == "Reward" || n.id.starts_with("Reward__")).collect();
    assert_eq!(rewards.len(), 2);
    assert_ne!(rewards[0].texture, rewards[1].texture);
    let stem = rewards[0].texture.split_once('/').unwrap().1;
    fn locate(root: &Path, filename: &str) -> Option<std::path::PathBuf> {
        for e in std::fs::read_dir(root).ok()? {
            let p = e.ok()?.path();
            if p.is_dir() {
                if let Some(p) = locate(&p, filename) {
                    return Some(p);
                }
            } else if p.file_name()?.to_str()? == filename {
                return Some(p);
            }
        }
        None
    }
    let image = locate(&dir.path().join("photocraft_generated"), &format!("{stem}.png")).unwrap();
    std::fs::write(&image, b"externally edited").unwrap();
    let gui = std::fs::read(dir.path().join("main.gui")).unwrap();
    assert!(s.execute("defold.testLegacySync", json!({})).is_err());
    assert_eq!(std::fs::read(dir.path().join("main.gui")).unwrap(), gui);
    assert_eq!(std::fs::read(image).unwrap(), b"externally edited");
}

#[test]
fn transparent_rgb_padding_does_not_expand_a_published_brush_texture() {
    let (dir, mut s) = fixture();
    let layer = s.execute("layer.new.layer", json!({"name":"SmallBrush"})).unwrap()["layer"].as_u64().unwrap();
    s.edit("Transparent RGB padding", |doc, _| {
        doc.layer_mut(LayerId(layer)).unwrap().surface_mut().unwrap().fill_rect(Rect::new(0, 0, 96, 96), &[1.0, 1.0, 1.0, 0.0]);
        Ok(())
    })
    .unwrap();
    s.execute("paint.stroke", json!({"points":[[45,45]],"size":8,"color":"#ff0000"})).unwrap();
    s.execute("defold.testLegacySync", json!({})).unwrap();
    let scene = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    let node = scene.nodes.iter().find(|n| n.id == "SmallBrush").unwrap();
    assert!(node.size[0] < 32.0 && node.size[1] < 32.0);
    let world = world(&scene, "SmallBrush").unwrap().apply(Point::new(0.0, 0.0));
    assert!((world.x - 45.0).abs() < 2.0 && (world.y - 51.0).abs() < 2.0);
}

#[test]
fn renaming_an_image_preserves_frames_used_by_another_gui() {
    let (dir, mut s) = fixture();
    let layer = s.execute("layer.new.layer", json!({"name":"SharedIcon"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("paint.stroke", json!({"points":[[45,45]],"size":8,"color":"#ff0000"})).unwrap();
    s.execute("defold.testLegacySync", json!({})).unwrap();
    std::fs::copy(dir.path().join("main.gui"), dir.path().join("other.gui")).unwrap();
    s.execute("layer.renameLayer", json!({"layer":layer,"name":"RenamedIcon"})).unwrap();
    s.execute("defold.testLegacySync", json!({})).unwrap();
    let scene = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    let texture = scene.proto.fields.iter().find(|f| f.name == "textures").unwrap();
    let atlas = dir.path().join(scene.proto.text(&texture.children, "texture").trim_start_matches('/'));
    let text = std::fs::read_to_string(&atlas).unwrap();
    assert!(text.contains("SharedIcon.png") && text.contains("RenamedIcon.png"));
    std::fs::remove_file(dir.path().join("other.gui")).unwrap();
    s.execute("defold.testLegacySync", json!({})).unwrap();
    let text = std::fs::read_to_string(atlas).unwrap();
    assert!(!text.contains("SharedIcon.png") && text.contains("RenamedIcon.png"));
}

#[test]
fn undoing_and_redoing_a_published_rename_keeps_the_asset_identity() {
    let (dir, mut s) = fixture();
    let layer = s.execute("layer.new.layer", json!({"name":"OriginalIcon"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("paint.stroke", json!({"points":[[45,45]],"size":8,"color":"#ff0000"})).unwrap();
    s.execute("defold.testLegacySync", json!({})).unwrap();
    let key = s.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.layer == layer).unwrap().asset_id.clone();
    s.execute("layer.renameLayer", json!({"layer":layer,"name":"RenamedIcon"})).unwrap();
    s.execute("defold.testLegacySync", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("defold.testLegacySync", json!({})).unwrap();
    let node = s.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.layer == layer).unwrap();
    assert_eq!(node.node, "OriginalIcon");
    assert_eq!(node.asset_id, key);
    s.execute("edit.redo", json!({})).unwrap();
    s.execute("defold.testLegacySync", json!({})).unwrap();
    let node = s.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.layer == layer).unwrap();
    assert_eq!(node.node, "OriginalIcon");
    assert_eq!(node.asset_id, key);
    assert_eq!(Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap().nodes.len(), 3);
}

fn project_fixture() -> (tempfile::TempDir, Session) {
    let (dir, mut session) = fixture();
    session
        .edit("Attach project fixture", |doc, _| {
            doc.defold.as_mut().unwrap().project = Some(defold::ProjectLink {
                schema_version: 1,
                project_id: defold::ProjectId::from_bytes([1; 16]),
                scene_id: defold::SceneId::from_bytes([2; 16]),
                package_id: defold::PackageId::from_bytes([3; 16]),
                gui_path: "/main.gui".into(),
                layers: Default::default(),
                legacy_aliases: Default::default(),
                imported_gui_source: None,
                imported_disk_hash: None,
            });
            crate::defold_design_reconcile::initialize(doc)
        })
        .unwrap();
    (dir, session)
}

#[test]
fn structural_layers_and_artwork_layers_have_explicit_undoable_roles() {
    let (_dir, mut s) = project_fixture();
    let child = LayerId(s.execute("layer.new.layer", json!({"name":"Reward"})).unwrap()["layer"].as_u64().unwrap());
    let initial = s.active().unwrap().doc.clone();
    let node = node_for_layer(&initial, child).unwrap().clone();
    let link = initial.defold.as_ref().unwrap();
    let metadata = link.project.as_ref().unwrap().layers[&child.0].clone();
    assert_eq!(metadata.role, defold::LayerRole::GuiNode);
    assert_eq!(metadata.appearance, Some(defold::AppearanceSource::SelfLayer));
    assert_eq!(Scene::parse(link.source.clone()).unwrap().nodes.iter().find(|n| n.id == node.node).unwrap().parent, "button");
    s.edit("Add artwork version", |doc, _| insert_artwork(doc, child, Layer::raster("Version", PixelFormat::RGBA8))).unwrap();
    let promoted = s.active().unwrap().doc.clone();
    let owner = node_for_layer(&promoted, child).unwrap();
    assert_ne!(owner.layer, child.0);
    assert_eq!(owner.asset_id, node.asset_id);
    let project = promoted.defold.as_ref().unwrap().project.as_ref().unwrap();
    assert_eq!(project.layers[&owner.layer].node_id, metadata.node_id);
    assert_eq!(project.layers[&owner.layer].layer_id, metadata.layer_id, "canonical GUI carrier identity survives wrapping");
    assert_ne!(project.layers[&child.0].layer_id, metadata.layer_id, "the old pixel is now a distinct artwork layer");
    let count = promoted.defold.as_ref().unwrap().nodes.len();
    s.execute("layer.select", json!({"layer":child.0})).unwrap();
    let artwork = LayerId(s.execute("layer.new.layer", json!({"name":"Shading"})).unwrap()["layer"].as_u64().unwrap());
    assert_eq!(s.active().unwrap().doc.defold.as_ref().unwrap().project.as_ref().unwrap().layers[&artwork.0].role, defold::LayerRole::ArtworkLayer);
    assert_eq!(s.active().unwrap().doc.defold.as_ref().unwrap().nodes.len(), count);
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.defold, initial.defold);
    assert_eq!(s.active().unwrap().doc.layers, initial.layers);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.defold, promoted.defold);
    let bytes = photocraft_format::save_to_bytes(&promoted, &Default::default()).unwrap();
    assert_eq!(photocraft_format::load_from_bytes(&bytes).unwrap().defold, promoted.defold);
}

#[test]
fn self_layer_node_ai_result_stays_in_its_owner_and_dirty_target_is_recovered() {
    use base64::Engine;
    let (_dir, mut s) = project_fixture();
    let layer = LayerId(s.execute("layer.new.layer", json!({"name":"Icon"})).unwrap()["layer"].as_u64().unwrap());
    let initial = s.active().unwrap().doc.clone();
    let node = node_for_layer(&initial, layer).unwrap().clone();
    let request = s.execute("ai.beginGeneration", json!({"prompt":"fixture", "provider":"codex", "destination":"layer"})).unwrap();
    let image = photocraft_codecs::Image::from_u8(4, 4, photocraft_codecs::ChannelLayout::Rgba, [255, 0, 0, 255].repeat(16)).unwrap();
    let png = base64::engine::general_purpose::STANDARD.encode(photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap());
    let result = s.execute("ai.receiveCandidate", json!({"candidateId":request["candidateId"], "imageBase64":png})).unwrap();
    assert_eq!(result["recovered"], false);
    let generated = LayerId(result["layerId"].as_u64().unwrap());
    let doc = &s.active().unwrap().doc;
    let owner = node_for_layer(doc, generated).unwrap();
    assert_eq!(owner.node, node.node);
    assert_eq!(owner.asset_id, node.asset_id);
    assert_eq!(doc.defold.as_ref().unwrap().nodes.len(), initial.defold.as_ref().unwrap().nodes.len());
    assert_eq!(doc.layer(layer).unwrap().content, initial.layer(layer).unwrap().content);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.layers, initial.layers);
    s.execute("layer.select", json!({"layer":layer.0})).unwrap();
    let request = s.execute("ai.beginGeneration", json!({"prompt":"fixture", "provider":"codex", "destination":"layer"})).unwrap();
    s.execute("paint.stroke", json!({"points":[[45,45]], "size":4, "color":"#00ff00"})).unwrap();
    let painted = s.active().unwrap().doc.clone();
    let result = s.execute("ai.receiveCandidate", json!({"candidateId":request["candidateId"], "imageBase64":png})).unwrap();
    assert_eq!(result["recovered"], true);
    assert!(s.active().unwrap().doc.defold.is_none());
    assert_eq!(s.documents().iter().find(|st| st.doc.id == painted.id).unwrap().doc.layers, painted.layers);
}

#[test]
fn empty_artwork_keeps_structure_without_placeholder_png_or_hidden_children() {
    let (dir, mut s) = project_fixture();
    let child = s.execute("layer.new.layer", json!({"name":"Empty"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("layer.select", json!({"layer":child})).unwrap();
    let descendant = s.execute("defold.addDesignNode", json!({"child":true})).unwrap()["node"].as_str().unwrap().to_string();
    s.execute("paint.stroke", json!({"points":[[45,45]], "size":4, "color":"#00ff00"})).unwrap();
    s.execute("defold.testLegacySync", json!({})).unwrap();
    let disk = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    let empty = disk.nodes.iter().find(|n| n.id == "Empty").unwrap();
    assert!(empty.texture.is_empty());
    assert!(!empty.visible);
    assert!(empty.enabled);
    let child = disk.nodes.iter().find(|n| n.id == descendant).unwrap();
    assert_eq!(child.parent, "Empty");
    assert!(child.visible);
    assert!(!child.texture.is_empty());
}

#[test]
fn duplicated_gui_node_keeps_artwork_roles_but_gets_new_stable_identities() {
    let (_dir, mut s) = project_fixture();
    let initial = s.active().unwrap().doc.clone();
    let button = initial.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "button").unwrap();
    let original_binding = initial.defold.as_ref().unwrap().project.as_ref().unwrap().layers[&button.layer].clone();
    s.execute("layer.select", json!({"layer":button.layer})).unwrap();
    let copy = LayerId(s.execute("layer.duplicate", json!({})).unwrap()["layer"].as_u64().unwrap());
    let doc = &s.active().unwrap().doc;
    let copied = node_for_layer(doc, copy).unwrap();
    let project = doc.defold.as_ref().unwrap().project.as_ref().unwrap();
    let binding = &project.layers[&copy.0];
    assert_ne!(binding.node_id, original_binding.node_id);
    assert_ne!(binding.asset_id, original_binding.asset_id);
    assert_ne!(binding.layer_id, original_binding.layer_id);
    assert_eq!(project.layers[&copied.artwork].role, defold::LayerRole::ArtworkRoot);
    assert_eq!(doc.defold.as_ref().unwrap().nodes.len(), 3, "copying an artwork member must not invent another GUI child");
    let scene = scene(&s).unwrap();
    let n = scene.nodes.iter().find(|n| n.id == copied.node).unwrap();
    assert_eq!(n.parent, "root");
    assert_eq!(n.color[2], 1.0);
    assert_eq!(n.size[0], 32.0);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.defold, initial.defold);
}

#[test]
fn property_bookkeeping_does_not_composite_unrelated_gui_nodes() {
    let (_dir, mut s) = project_fixture();
    APPEARANCE_CALLS.with(|calls| calls.set(0));
    s.execute("defold.setDesignProperties", json!({"values":{"id":"button_script_id"}})).unwrap();
    assert_eq!(APPEARANCE_CALLS.with(|calls| calls.get()), 0, "an ID edit is metadata only");
    let selected = selected(&s).unwrap();
    assert_eq!(s.active().unwrap().doc.layer(LayerId(selected.layer)).unwrap().name, "button", "script ID and image display name are independent");
    s.execute("defold.setDesignProperties", json!({"values":{"position.x":8}})).unwrap();
    assert!(APPEARANCE_CALLS.with(|calls| calls.get()) <= 2, "moving a leaf must not composite its parent or siblings");
    assert_eq!(scene(&s).unwrap().nodes.iter().find(|n| n.id == "button_script_id").unwrap().position[0], 8.0);
}

#[test]
fn publication_plan_uses_frozen_resources_and_never_touches_project_files() {
    let (dir, mut s) = project_fixture();
    s.execute("layer.new.layer", json!({"name":"SnapshotIcon"})).unwrap();
    s.execute("paint.stroke", json!({"points":[[45,45]],"size":4,"color":"#ff0000"})).unwrap();
    let mut doc = s.active().unwrap().doc.as_ref().clone();
    let inputs = capture_publication_inputs(&doc, None, None).unwrap();
    let before = std::fs::read(dir.path().join("main.gui")).unwrap();
    // No valid project exists at this location; a planner that reads it will fail.
    let missing = dir.path().join("unavailable");
    let link = doc.defold.as_mut().unwrap();
    link.root = missing.to_string_lossy().into_owned();
    link.gui = missing.join("main.gui").to_string_lossy().into_owned();
    let plan = plan_publication(doc, inputs).unwrap();
    assert!(plan.files.iter().any(|(path, _)| path.ends_with("SnapshotIcon.png")));
    assert!(plan.output.nodes.iter().any(|n| n.id == "SnapshotIcon" && !n.texture.is_empty()));
    assert_eq!(std::fs::read(dir.path().join("main.gui")).unwrap(), before);
    assert!(!dir.path().join("photocraft_generated").exists());
    assert!(!missing.exists());
}

#[test]
fn explicit_single_frame_atlas_binding_opens_without_relying_on_png_basename() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("game.project"), "[display]\nwidth=96\nheight=96\n").unwrap();
    let image = photocraft_codecs::Image::from_u8(4, 4, photocraft_codecs::ChannelLayout::Rgba, [0, 255, 0, 255].repeat(16)).unwrap();
    let png = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap();
    std::fs::write(dir.path().join("ReadableName.png"), png).unwrap();
    std::fs::write(dir.path().join("ui.atlas"),"animations { id: \"stable_frame\" images { image: \"/ReadableName.png\" } }\nanimations { id: \"unrelated_animation\" images { image: \"/ReadableName.png\" } images { image: \"/ReadableName.png\" } }\n").unwrap();
    std::fs::write(dir.path().join("main.gui"),"nodes { id: \"icon\" type: TYPE_BOX position { x:48 y:48 } size { x:4 y:4 } texture: \"ui/stable_frame\" }\ntextures { name: \"ui\" texture: \"/ui.atlas\" }").unwrap();
    let mut s = Session::new();
    let result = s.execute("defold.openDesign", json!({"path":dir.path().join("main.gui")})).unwrap();
    assert_eq!(result["warnings"], json!([]));
    let doc = &s.active().unwrap().doc;
    let node = &doc.defold.as_ref().unwrap().nodes[0];
    assert!(doc.layer(LayerId(node.artwork)).unwrap().surface().unwrap().rgba(48, 48)[1] > 0.99);
    assert!(resource_choices(&s, "texture").unwrap().contains(&"ui/stable_frame".to_string()));
}

#[test]
fn formal_creative_source_wins_over_legacy_sidecar_without_publishing_game_files() {
    let (dir, mut s) = fixture();
    let original = s.active().unwrap().doc.clone();
    let old = photocraft_format::save_to_bytes(&original, &Default::default()).unwrap();
    std::fs::write(dir.path().join("main.gui.pcraft"), old.clone()).unwrap();
    let gui_before = std::fs::read(dir.path().join("main.gui")).unwrap();
    s.execute("layer.new.layer", json!({"name":"Draft only"})).unwrap();
    s.execute("paint.stroke", json!({"points":[[45,45]],"size":4,"color":"#ff0000"})).unwrap();
    let draft = s.active().unwrap().doc.clone();
    s.execute("defold.saveDesignSource", json!({})).unwrap();
    assert_eq!(std::fs::read(dir.path().join("main.gui")).unwrap(), gui_before);
    assert_eq!(std::fs::read(dir.path().join("main.gui.pcraft")).unwrap(), old, "formal save must not rewrite an old sidecar");
    assert!(!dir.path().join("photocraft_generated").exists());
    let mut restored = Session::new();
    restored.execute("defold.openDesign", json!({"path":dir.path().join("main.gui")})).unwrap();
    assert_eq!(restored.active().unwrap().doc.layers, draft.layers);
    assert_eq!(restored.active().unwrap().doc.defold, draft.defold);
}
