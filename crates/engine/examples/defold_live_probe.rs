//! Explicit opt-in live editor check on a caller-created isolated fixture only.
use photocraft_engine::Session;
use serde_json::json;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("Pass an absolute .gui path")?;
    let mutate = args.next().as_deref() == Some("--mutate-fixture");
    if mutate && !path.ends_with("photocraft_live_validation.gui") {
        return Err("Mutation requires the isolated photocraft_live_validation.gui fixture".into());
    }
    let disk = std::fs::read(&path)?;
    let mut s = Session::new();
    s.execute("defold.openGui", json!({"path":path}))?;
    s.execute("defold.connectLive", json!({}))?;
    let d = s.active_gui().ok_or("No GUI")?;
    println!("Connected: {} nodes; editor {}", d.scene.nodes.len(), d.live.as_ref().ok_or("No live state")?.snapshot.version);
    if !mutate {
        return Ok(());
    }
    // Only this workspace-owned test project may clean up an interrupted run.
    if path.replace('\\', "/").contains("/photocraft/plan/defold-live-project/") {
        let roots: Vec<_> =
            s.active_gui().ok_or("No GUI")?.scene.nodes.iter().filter(|n| n.parent.is_empty() && n.id.starts_with("pc_")).map(|n| n.id.clone()).collect();
        for id in roots {
            s.execute("defold.deleteNode", json!({"node":id}))?;
        }
    }
    s.execute("defold.addNode", json!({"id":"pc_parent","kind":"TYPE_BOX"}))?;
    s.execute("defold.addNode", json!({"id":"pc_child","kind":"TYPE_TEXT","parent":"pc_parent"}))?;
    s.execute("defold.setNodeProperties", json!({"node":"pc_child","values":{"position.x":123,"position.y":45,"text":"PhotoCraft live"}}))?;
    s.execute("defold.refreshLive", json!({}))?;
    let n = s.active_gui().and_then(|d| d.scene.nodes.iter().find(|n| n.id == "pc_child")).ok_or("Child missing")?;
    if n.parent != "pc_parent" || n.position[0] != 123.0 || n.position[1] != 45.0 || n.text != "PhotoCraft live" {
        return Err("Live property/parent mismatch".into());
    }
    s.execute("edit.undo", json!({}))?;
    s.execute("edit.redo", json!({}))?;
    s.execute("defold.moveNode", json!({"node":"pc_child","parent":""}))?;
    s.execute("edit.undo", json!({}))?;
    s.execute("defold.deleteNode", json!({"node":"pc_parent"}))?;
    s.execute("edit.undo", json!({}))?;
    let n = s.active_gui().and_then(|d| d.scene.nodes.iter().find(|n| n.id == "pc_child")).ok_or("Undo did not restore child")?;
    if n.parent != "pc_parent" || n.text != "PhotoCraft live" {
        return Err("Undo lost subtree properties".into());
    }
    s.execute("defold.addNode", json!({"id":"pc_peer","after":"pc_parent"}))?;
    let nodes = &s.active_gui().ok_or("No GUI")?.scene.nodes;
    if nodes.iter().position(|n| n.id == "pc_peer") >= nodes.iter().position(|n| n.id == "pc_parent") {
        return Err("Sibling-below order mismatch".into());
    }
    if s.execute("defold.setNodeProperty", json!({"node":"pc_child","property":"rotation.z","value":90})).is_ok() {
        return Err("Legacy rotation was not guarded".into());
    }
    if std::fs::read(&path)? != disk {
        return Err("Live editing unexpectedly wrote GUI disk contents".into());
    }
    s.execute("defold.saveProject", json!({"allModifiedResources":true}))?;
    if !std::fs::read_to_string(&path)?.contains("pc_child") {
        return Err("Defold project save did not persist nodes".into());
    }
    let mut other = Session::new();
    other.execute("defold.openGui", json!({"path":path}))?;
    other.execute("defold.connectLive", json!({}))?;
    other.execute("defold.setNodeProperty", json!({"node":"pc_peer","property":"alpha","value":0.7}))?;
    if s.execute("defold.setNodeProperty", json!({"node":"pc_peer","property":"alpha","value":0.2})).is_ok() {
        return Err("Stale edit was accepted".into());
    }
    s.execute("defold.refreshLive", json!({}))?;
    if s.active_gui().ok_or("No GUI")?.can_undo() {
        return Err("External edit did not invalidate local history".into());
    }
    s.execute("defold.deleteNode", json!({"node":"pc_parent"}))?;
    s.execute("defold.deleteNode", json!({"node":"pc_peer"}))?;
    s.execute("defold.refreshLive", json!({}))?;
    if s.active_gui().is_some_and(|d| d.scene.nodes.iter().any(|n| n.id.starts_with("pc_"))) {
        return Err("Fixture cleanup failed".into());
    }
    s.execute("defold.saveProject", json!({"allModifiedResources":true}))?;
    println!(
        "PHOTOCRAFT_LIVE_OK child/sibling edits, legacy rotation guard, undo/redo, reparent, delete/restore, conflict rejection, explicit save; test nodes cleaned"
    );
    Ok(())
}
