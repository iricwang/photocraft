//! Read-only compare of authored and live GUI fields; never publishes.
use photocraft_engine::Session;
use serde_json::json;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::current_dir()?.join("plan/sync-live-fixture");
    let snapshot =
        photocraft_engine::defold_live::request(&root.to_string_lossy(), &json!({"op":"snapshot","path":"/assets/defold/gui/ui_window_character.gui"}))?;
    std::fs::write("plan/sync-expected.json", serde_json::to_vec(&snapshot)?)?;
    let path = r"X:\Projs\FortySix-MiniGame\assets\defold\gui\ui_window_character.gui";
    let mut s = Session::new();
    s.execute("defold.openGui", json!({"path":path}))?;
    let disk = s.active_gui().ok_or("No GUI")?.scene.clone();
    s.execute("defold.connectLive", json!({}))?;
    let live = &s.active_gui().ok_or("No GUI")?.scene;
    println!("disk nodes {}, live nodes {}", disk.nodes.len(), live.nodes.len());
    for n in &disk.nodes {
        for p in photocraft_defold_format::properties(&n.kind) {
            let a = disk.property(&n.id, &p.path)?;
            let b = live.property(&n.id, &p.path)?;
            let same = match (a.as_f64(), b.as_f64()) {
                (Some(a), Some(b)) => (a - b).abs() < 1e-5,
                _ => a == b,
            };
            if !same {
                println!("{} {} disk={} live={}", n.id, p.path, a, b);
            }
        }
    }
    Ok(())
}
