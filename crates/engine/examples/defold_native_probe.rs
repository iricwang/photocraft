//! Opt-in end-to-end check, restricted to the workspace-owned live fixture.
use photocraft_engine::Session;
use serde_json::json;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("Pass isolated fixture path")?;
    if !path.replace('\\', "/").ends_with("/photocraft/plan/defold-live-project/photocraft_live_validation.gui") {
        return Err("Only the isolated fixture is permitted".into());
    }
    let mut s = Session::new();
    s.execute("defold.openDesign", json!({"path":path}))?;
    if s.active_gui().is_some() {
        return Err("Expected a native document".into());
    }
    let link = s.active().and_then(|s| s.doc.defold.clone()).ok_or("Missing native link")?;
    if link.remote.is_none() {
        return Err("Start Defold on the isolated project first".into());
    }
    if let Some(node) = link.nodes.iter().find(|n| n.node == "native_probe") {
        s.execute("layer.select", json!({"layer":node.artwork}))?;
    } else {
        s.execute("defold.addDesignNode", json!({"child":true}))?;
    }
    s.execute("paint.stroke", json!({"points":[[100,100],[120,110]],"size":12,"color":"#ff0000"}))?;
    s.execute("paint.stroke", json!({"points":[[110,105]],"size":4,"erase":true}))?;
    s.execute("defold.setDesignProperties", json!({"values":{"id":"native_probe","position.x":30}}))?;
    let result = s.execute("defold.syncDesign", json!({}))?;
    println!("Published {result}");
    let mut verify = Session::new();
    verify.execute("defold.openGui", json!({"path":path}))?;
    verify.execute("defold.connectLive", json!({}))?;
    let node = verify.active_gui().and_then(|d| d.scene.nodes.iter().find(|n| n.id == "native_probe")).ok_or("Node missing in Defold")?;
    if !node.texture.starts_with("pc_") {
        return Err("Painted pixels did not publish as an atlas".into());
    }
    // Saving all resources is permitted only in this dedicated test project.
    verify.execute("defold.saveProject", json!({"allModifiedResources":true}))?;
    let mut materialized = Session::new();
    materialized.execute("defold.openGui", json!({"path":path}))?;
    let png = photocraft_engine::defold_cmds::preview_png(&materialized, "native_probe")?;
    let published = photocraft_io::import("node.png", &png)?.document;
    let buffer = photocraft_compose::render(&published, published.bounds());
    if published.size.width <= 1 || !buffer.px.iter().any(|p| p[3] > 0.1) {
        return Err("Published pixels were empty or clipped".into());
    }
    let mut reopened = Session::new();
    reopened.execute("defold.openDesign", json!({"path":path}))?;
    reopened.execute("defold.syncDesign", json!({}))?;
    println!("PHOTOCRAFT_NATIVE_LIVE_OK native brush/eraser, child, properties, PNG/atlas, live graph, save and reopen");
    Ok(())
}
