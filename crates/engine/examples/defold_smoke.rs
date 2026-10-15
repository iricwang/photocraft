//! Offline deterministic integration fixture. No model calls. Compile the resulting game with Bob.
use base64::Engine;
use photocraft_color::{Color, ColorMode, SampleType};
use photocraft_doc::{Document, Size};
use photocraft_engine::Session;
use serde_json::json;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).ok_or("pass an empty output directory")?);
    if dir.exists() {
        return Err("output directory already exists".into());
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::create_dir_all(dir.join("input"))?;
    std::fs::write(dir.join("input/game.input_binding"), "")?;
    std::fs::write(
        dir.join("game.project"),
        "[project]\ntitle = PhotoCraft Defold P2 smoke\n[bootstrap]\nmain_collection = /main.collectionc\n[display]\nwidth = 960\nheight = 640\n[graphics]\ndefault_texture_min_filter = linear\ndefault_texture_mag_filter = linear\n",
    )?;
    std::fs::write(
        dir.join("main.collection"),
        "name: \"main\"\ninstances { id: \"ui\" prototype: \"/main.go\" position { x: 0 y: 0 z: 0 } rotation { x: 0 y: 0 z: 0 w: 1 } }\n",
    )?;
    std::fs::write(dir.join("main.go"), "components { id: \"gui\" component: \"/main.gui\" position { x: 0 y: 0 z: 0 } rotation { x: 0 y: 0 z: 0 w: 1 } }\n")?;
    std::fs::write(
        dir.join("main.gui"),
        r#"script: "/main.gui_script"
fonts { name: "default" font: "/builtins/fonts/default.font" }
textures { name: "ui" texture: "/ui.atlas" }
nodes { id: "background" type: TYPE_BOX position {x:480 y:320 z:0} size {x:960 y:640 z:0} color {x:0.06 y:0.08 z:0.12 w:1} size_mode: SIZE_MODE_MANUAL }
nodes { id: "button" type: TYPE_BOX position {x:480 y:320 z:0} size {x:256 y:96 z:0} texture: "ui/button" size_mode: SIZE_MODE_MANUAL }
nodes { id: "label" type: TYPE_TEXT parent: "button" position {x:0 y:0 z:0} size {x:220 y:40 z:0} font: "default" text: "Before editing" }
material: "/builtins/materials/gui.material"
adjust_reference: ADJUST_REFERENCE_PARENT
max_nodes: 512
"#,
    )?;
    std::fs::write(
        dir.join("main.gui_script"),
        "function init(self)\n print('PHOTOCRAFT_P2_RUNTIME_OK', gui.get_text(gui.get_node('label')), gui.get_position(gui.get_node('button')).x)\nend\nfunction update(self, dt)\n self.time = (self.time or 0) + dt\n if self.time > 1 then msg.post('@system:', 'exit', {code=0}) end\nend\n",
    )?;
    std::fs::write(dir.join("ui.atlas"), "images { image: \"/button.png\" }\nextrude_borders: 2\n")?;
    let image = |color| -> std::result::Result<Vec<u8>, Box<dyn std::error::Error>> {
        let doc = Document::with_background("fixture", Size::new(256, 96), ColorMode::Rgb, SampleType::U8, color);
        Ok(photocraft_io::export(&doc, "png", &Default::default())?.bytes)
    };
    std::fs::write(dir.join("button.png"), image(Color::rgba(0.1, 0.3, 0.6, 1.0))?)?;
    let mut s = Session::new();
    let opened = s.execute("defold.openGui", json!({"path":dir.join("main.gui")}))?;
    s.execute("defold.setNodeProperty", json!({"node":"label","property":"text","value":"Published from PhotoCraft"}))?;
    s.execute("defold.setNodeProperty", json!({"node":"button","property":"position.x","value":500}))?;
    s.execute("defold.saveGui", json!({}))?;
    let binding = s.execute("defold.openNodeSource", json!({"node":"button"}))?;
    let begin = s.execute("ai.beginGeneration", json!({"prompt":"offline deterministic regression fixture","provider":"codex","destination":"layer"}))?;
    s.execute("defold.activateGui", json!({"id":opened["id"]}))?;
    s.execute(
        "ai.receiveCandidate",
        json!({"candidateId":begin["candidateId"],"imageBase64":base64::engine::general_purpose::STANDARD.encode(image(Color::rgba(0.1,0.75,0.55,1.0))?)}),
    )?;
    let published = s.execute("defold.publishNodeSource", json!({"binding":binding["binding"]}))?;
    println!("{}", serde_json::to_string_pretty(&json!({"fixture":"offline, no model calls","gui":opened["path"],"receipt":published,"focus":s.focus()}))?);
    Ok(())
}
