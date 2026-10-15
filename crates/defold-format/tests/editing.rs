use photocraft_defold_format::Scene;
use serde_json::json;

#[test]
fn structural_edits_preserve_resources_unknown_fields_and_parent_links() {
    let s = Scene::parse("# keep\ntextures { name: \"ui\" texture: \"/a.atlas\" }\nnodes { id: \"a\" future { magic: 7 } }\n".into()).unwrap();
    let s = s.new_node("child", "TYPE_TEXT", "a", None).unwrap().edit("child", "font", &json!("default")).unwrap();
    let s = s.rename_node("a", "parent").unwrap();
    assert_eq!(s.nodes.iter().find(|n| n.id == "child").unwrap().parent, "parent");
    assert!(s.move_node("parent", "child", None).is_err());
    let s = s.move_node("child", "", Some("parent")).unwrap();
    assert_eq!(s.nodes[0].id, "child");
    assert!(s.proto.source.contains("future { magic: 7 }"));
    let s = s.remove_node("parent").unwrap();
    assert_eq!(s.nodes.len(), 1);
    assert!(s.proto.source.contains("textures { name: \"ui\""));
    assert!(s.proto.source.starts_with("# keep"));
}
#[test]
fn authored_properties_validate_enums_vectors_and_ranges() {
    let s = Scene::parse("nodes {id:\"box\"}".into()).unwrap();
    let s = s.edit("box", "pivot", &json!("PIVOT_NE")).unwrap().edit("box", "slice9.x", &json!(12)).unwrap().edit("box", "color.y", &json!(0.2)).unwrap();
    assert_eq!(s.property("box", "slice9.x").unwrap(), json!(12.0));
    assert_eq!(s.nodes[0].pivot, "PIVOT_NE");
    assert!(s.edit("box", "pivot", &json!("unknown")).is_err());
    assert!(s.edit("box", "size.x", &json!(-1)).is_err());
    assert!(s.new_node("bad/id", "TYPE_BOX", "", None).is_err());
    assert!(s.new_node("box", "TYPE_BOX", "", None).is_err());
}
#[test]
fn delete_cleans_layout_records_and_rename_updates_children() {
    let s = Scene::parse("nodes {id:\"a\"} nodes {id:\"b\" parent:\"a\"} layouts {name:\"Portrait\" nodes{id:\"b\" position{x:10}}}".into()).unwrap();
    let s = s.rename_node("b", "renamed").unwrap();
    assert!(!s.proto.source.contains("\"b\""));
    let s = s.remove_node("a").unwrap();
    assert!(s.nodes.is_empty());
    assert!(!s.proto.source.contains("renamed"));
    assert!(s.proto.source.contains("Portrait"));
}
