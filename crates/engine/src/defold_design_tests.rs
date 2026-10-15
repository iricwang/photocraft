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
    let result = s.execute("defold.syncDesign", json!({})).unwrap();
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
    assert_eq!(s.execute("defold.syncDesign", json!({})).unwrap()["assets"], 1, "property edits must not mark painted artwork pristine");
    assert!(dir.path().join("main.gui.pcraft").is_file());
}
#[test]
fn native_node_create_delete_and_undo_sync_as_hierarchy() {
    let (dir, mut s) = fixture();
    let id = s.execute("defold.addDesignNode", json!({"child":true})).unwrap()["node"].as_str().unwrap().to_string();
    s.execute("paint.stroke", json!({"points":[[45,45],[50,50]],"size":5,"color":"#00ff00"})).unwrap();
    s.execute("defold.syncDesign", json!({})).unwrap();
    let disk = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert_eq!(disk.nodes.iter().find(|n| n.id == id).unwrap().parent, "button");
    let group = s.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == id).unwrap().layer;
    s.execute("layer.select", json!({"layer":group})).unwrap();
    s.execute("layer.delete", json!({})).unwrap();
    s.execute("defold.syncDesign", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("defold.syncDesign", json!({})).unwrap();
    let disk = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert!(disk.nodes.iter().any(|n| n.id == id));
}
#[test]
fn external_gui_changes_are_not_overwritten() {
    let (dir, mut s) = fixture();
    std::fs::write(dir.path().join("main.gui"), "nodes {id:\"external\"}").unwrap();
    assert!(s.execute("defold.syncDesign", json!({})).is_err());
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
    let result = s.execute("defold.syncDesign", json!({})).unwrap();
    assert_eq!(result["assets"], 0);
    let disk = Scene::parse(std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(disk.nodes[0].kind, "TYPE_TEXT");
    assert_eq!(disk.nodes[0].text, "New label");
    s.execute("defold.syncDesign", json!({})).unwrap();
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
    assert_eq!(s.execute("defold.syncDesign", json!({})).unwrap()["assets"], 1);
}

#[test]
fn parent_mask_is_applied_to_child_artwork_publication() {
    let (_dir, mut s) = fixture();
    let root = s.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "root").unwrap().layer;
    s.execute("layer.select", json!({"layer":root})).unwrap();
    s.execute("layer.layerMask.hideAll", json!({})).unwrap();
    let doc = &s.active().unwrap().doc;
    let node = doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "button").unwrap();
    let bitmap = rendered(doc, own_layer(doc, node).unwrap()).unwrap();
    assert!(bitmap.px.iter().all(|p| p[3] == 0.0));
    assert!(s.execute("defold.syncDesign", json!({})).unwrap()["assets"].as_u64().unwrap() > 0);
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
    assert_eq!(s.execute("defold.syncDesign", json!({})).unwrap()["assets"], 0);
    let outside = s.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "outside").unwrap().artwork;
    s.execute("layer.select", json!({"layer":outside})).unwrap();
    s.execute("layer.setProps", json!({"opacity":0.5})).unwrap();
    assert_eq!(s.execute("defold.syncDesign", json!({})).unwrap()["assets"], 1);
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
    assert_eq!(restored.execute("defold.syncDesign", json!({})).unwrap()["assets"], 1);
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
    s.execute("defold.syncDesign", json!({})).unwrap();
    std::fs::write(dir.path().join("game.project"), "[display]\nwidth=160\nheight=144\n").unwrap();
    let mut reopened = Session::new();
    reopened.execute("defold.openDesign", json!({"path":dir.path().join("main.gui")})).unwrap();
    let doc = &reopened.active().unwrap().doc;
    let button = doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "button").unwrap().artwork;
    assert_eq!(doc.size, Size::new(160, 144));
    assert!(doc.layer(LayerId(button)).unwrap().surface().unwrap().rgba(48, 96)[2] > 0.9);
    assert_eq!(reopened.execute("defold.syncDesign", json!({})).unwrap()["assets"], 0);
    // Resolution changes while a document is open are handled at publication too.
    assert_eq!(s.execute("defold.syncDesign", json!({})).unwrap()["assets"], 0);
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
    assert_eq!(restored.execute("defold.syncDesign", json!({})).unwrap()["assets"], 1);
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
    assert_eq!(s.execute("defold.syncDesign", json!({})).unwrap()["assets"], 1);
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
    assert_eq!(s.execute("defold.syncDesign", json!({})).unwrap()["assets"], 0);
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
    assert_eq!(s.execute("defold.syncDesign", json!({})).unwrap()["assets"], 1);
    let disk = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert_eq!(disk.nodes.len(), 3, "own Artwork layers must not become extra nodes");
    let node = disk.nodes.iter().find(|n| n.id == format!("pc_node_{child}")).unwrap();
    assert_eq!(node.parent, "button");
    assert!(!node.texture.is_empty());
    assert!(disk.nodes.iter().find(|n| n.id == "button").unwrap().texture.is_empty(), "child paint must not replace the parent's artwork");
    let root = s.active().unwrap().doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "root").unwrap().layer;
    s.execute("layer.select", json!({"layer":root})).unwrap();
    let layer = s.execute("layer.new.layer", json!({"name":"Top level artwork"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("paint.stroke", json!({"points":[[15,15]],"size":8,"color":"#00ff00"})).unwrap();
    s.execute("defold.syncDesign", json!({})).unwrap();
    let disk = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    let node = disk.nodes.iter().find(|n| n.id == format!("pc_node_{layer}")).unwrap();
    assert!(node.parent.is_empty());
    assert!(!node.texture.is_empty());
    s.execute("defold.syncDesign", json!({})).unwrap();
    let mut reopened = Session::new();
    reopened.execute("defold.openDesign", json!({"path":dir.path().join("main.gui")})).unwrap();
    reopened.execute("defold.syncDesign", json!({})).unwrap();
    let disk = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert_eq!(disk.nodes.len(), 4);
    assert_eq!(disk.nodes.iter().find(|n| n.id == format!("pc_node_{child}")).unwrap().parent, "button");
    reopened.execute("layer.select", json!({"layer":child})).unwrap();
    reopened.execute("layer.delete", json!({})).unwrap();
    reopened.execute("defold.syncDesign", json!({})).unwrap();
    let disk = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert_eq!(disk.nodes.len(), 3);
    assert!(disk.nodes.iter().all(|n| n.id != format!("pc_node_{child}")));
    reopened.execute("edit.undo", json!({})).unwrap();
    reopened.execute("defold.syncDesign", json!({})).unwrap();
    let disk = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert_eq!(disk.nodes.iter().find(|n| n.id == format!("pc_node_{child}")).unwrap().parent, "button");
}

#[test]
fn generated_node_artwork_stays_with_parent_while_new_empty_layer_is_a_child() {
    let (dir, mut s) = fixture();
    let generated = s.execute("layer.new.layer", json!({"name":"AI result"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("paint.stroke", json!({"points":[[45,45]],"size":8,"color":"#ff0000"})).unwrap();
    s.edit("AI provenance", |doc, _| {
        doc.ai_generations.push(photocraft_doc::ai::GenerationGroup {
            id: 1, canvas: doc.size, area: Rect::from_size(doc.size), frame: None, selection: None,
            reference_png: None, context_png: None, mode: photocraft_doc::ai::GenerationMode::Repaint,
            versions: vec![photocraft_doc::ai::GenerationVersion {
                layer: Some(LayerId(generated)), prompt: String::new(), optimized_prompt: String::new(),
                provider: String::new(), model: String::new(),
            }],
        });
        Ok(json!({}))
    }).unwrap();
    let empty = s.execute("layer.new.layer", json!({"name":"Empty child"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("defold.syncDesign", json!({})).unwrap();
    let disk = Scene::parse(std::fs::read_to_string(dir.path().join("main.gui")).unwrap()).unwrap();
    assert_eq!(disk.nodes.len(), 3);
    assert_eq!(disk.nodes.iter().find(|n| n.id == format!("pc_node_{empty}")).unwrap().parent, "button");
    assert!(disk.nodes.iter().find(|n| n.id == "button").unwrap().texture.starts_with("pc_"));
    assert!(disk.nodes.iter().all(|n| n.id != format!("pc_node_{generated}")));
}
