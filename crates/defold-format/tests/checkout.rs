//! Read-only conformance against the user's pinned Defold checkout; no upstream assets copied.
#[test]
#[ignore = "Set DEFOLD_CHECKOUT to the checked-out Defold source tree"]
fn upstream_gui_roundtrips() {
    let root = std::path::PathBuf::from(std::env::var("DEFOLD_CHECKOUT").expect("DEFOLD_CHECKOUT"));
    let samples = [
        "test_project/gui/simple.gui",
        "test_project/gui/scene.gui",
        "test_project/gui/layouts.gui",
        "test_project/gui/layers.gui",
        "test_project/gui/size_mode.gui",
        "gui_project/gui/template/window.gui",
        "gui_project/gui/template_layout/panel_lp_button_lp.gui",
        "gui_project/gui/resources/panel.gui",
    ];
    for sample in samples {
        let path = root.join("editor/test/resources").join(sample);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let scene = photocraft_defold_format::Scene::parse(String::from_utf8(bytes.clone()).unwrap()).unwrap_or_else(|e| panic!("{sample}: {e}"));
        assert_eq!(scene.proto.source.as_bytes(), bytes, "{sample}");
        if let Some(node) = scene.nodes.iter().find(|n| n.editable) {
            let next = scene.edit(&node.id, "position.x", &serde_json::json!(node.position[0] + 1.0)).unwrap();
            assert_eq!(next.nodes.len(), scene.nodes.len());
        }
        println!("{sample}: {} nodes, exact no-op roundtrip", scene.nodes.len());
    }
}
