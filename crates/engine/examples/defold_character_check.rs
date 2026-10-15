//! Read the actual saved character source; publish only into a workspace fixture.
use photocraft_defold_format::Scene;
use photocraft_engine::Session;
use serde_json::json;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let gui = r"X:\Projs\FortySix-MiniGame\assets\defold\gui\ui_window_character.gui";
    let mut session = Session::new();
    let opened = session.execute("defold.openDesign", json!({"path":gui}))?;
    let mut doc = (*session.active().ok_or("No document")?.doc).clone();
    let link = doc.defold.as_ref().ok_or("No GUI link")?;
    let source = link.source.clone();
    let authored = Scene::parse(source.clone())?;
    let snap_text = std::fs::read_to_string("plan/character-defold-0.json.txt")?;
    let snap: serde_json::Value = serde_json::from_str(snap_text.lines().find_map(|l| l.strip_prefix("PHOTOCRAFT_JSON:")).ok_or("No editor snapshot")?)?;
    for n in &authored.nodes {
        let remote = snap["nodes"].as_array().ok_or("No remote nodes")?.iter().find(|r| r["id"] == n.id).ok_or("Missing editor node")?;
        for (i, coordinate) in n.position.iter().enumerate() {
            if (remote["properties"]["position"].get(i).and_then(|v| v.as_f64()).ok_or("No remote position")? - coordinate).abs() > 0.001 {
                return Err(format!("Editor/source position mismatch: {}", n.id).into());
            }
        }
    }
    println!(
        "{}",
        json!({"opened":opened,"gui":link.gui,"size":[doc.size.width,doc.size.height],"origin":link.canvas_origin,"nodesComparedWithDefold":authored.nodes.len()})
    );
    let target = std::env::current_dir()?.join("plan/character-origin-fixture");
    std::fs::create_dir_all(&target)?;
    let target = target.canonicalize()?;
    std::fs::write(target.join("game.project"), "[display]\nwidth=2340\nheight=1080\n")?;
    std::fs::write(target.join("character.gui"), &source)?;
    let link = doc.defold.as_mut().ok_or("No GUI link")?;
    link.root = target.to_string_lossy().into_owned();
    link.gui = target.join("character.gui").to_string_lossy().into_owned();
    link.disk_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    link.remote = None;
    let layers = doc.layers.clone();
    let mut fixture = Session::new();
    fixture.add_document(doc, None);
    let result = fixture.execute("defold.syncDesign", json!({}))?;
    let published = Scene::parse(std::fs::read_to_string(target.join("character.gui"))?)?;
    if result["assets"] == 0 {
        for n in &authored.nodes {
            if published.nodes.iter().find(|p| p.id == n.id).ok_or("Published node missing")?.position != n.position {
                return Err(format!("Publication changed position: {}", n.id).into());
            }
        }
    }
    let mut reopened = Session::new();
    reopened.execute("defold.openDesign", json!({"path":target.join("character.gui")}))?;
    if reopened.active().ok_or("No reopened source")?.doc.layers != layers {
        return Err("Reopen changed native layers".into());
    }
    println!("fixture publication and native reopen: {result}");
    Ok(())
}
