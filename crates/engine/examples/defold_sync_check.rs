//! Real-editor smoke test, restricted to a workspace-owned fixture project.
use photocraft_engine::Session;
use serde_json::json;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let relative = std::env::args().nth(1).unwrap_or_else(|| "main.gui".into());
    if relative.contains("..") || relative.contains(':') || relative.starts_with(['/', '\\']) {
        return Err("Expected fixture-relative GUI".into());
    }
    let resource = format!("/{}", relative.replace('\\', "/"));
    let path = std::env::current_dir()?.join("plan/sync-live-fixture").join(relative);
    let mut session = Session::new();
    println!("open: {}", session.execute("defold.openDesign", json!({"path":path}))?);
    let binding = session.active().ok_or("No document")?.doc.defold.as_ref().ok_or("No link")?.nodes.iter().find(|n| n.node != "root").ok_or("No child node")?.clone();
    session.execute("layer.select", json!({"layer":binding.artwork}))?;
    let added = session.execute("layer.new.layer", json!({"name":"Sync test layer"}))?["layer"].as_u64().ok_or("No layer id")?;
    session.execute("paint.stroke", json!({"points":[[42,42],[54,54]],"size":8,"color":"#ff0000"}))?;
    println!("sync: {}", session.execute("defold.syncDesign", json!({}))?);
    println!("sync again: {}", session.execute("defold.syncDesign", json!({}))?);
    let design = session.active().ok_or("No document")?.doc.defold.as_ref().ok_or("No link")?;
    let live = photocraft_engine::defold_live::request(&design.root, &json!({"op":"snapshot","path":resource}))?;
    let child = live.nodes.iter().find(|n| n.id == format!("pc_node_{added}")).ok_or("Child missing in running Defold")?;
    if child.parent != binding.node { return Err("Wrong live child parent".into()); }
    println!("Verified live child: {} -> {}", child.id, child.parent);
    let root = session.active().ok_or("No document")?.doc.defold.as_ref().ok_or("No link")?.nodes.iter().find(|n| n.node == "root").ok_or("No root")?.layer;
    session.execute("layer.select", json!({"layer":root}))?;
    session.execute("layer.new.layer", json!({"name":"New top level layer"}))?;
    session.execute("paint.stroke", json!({"points":[[10,10],[20,20]],"size":4,"color":"#00ff00"}))?;
    println!("top level sync: {}", session.execute("defold.syncDesign", json!({}))?);
    Ok(())
}
