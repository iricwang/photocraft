//! Defold GUI workspace, optimistic disk saves and explicit existing-asset publication.
//! Image documents/AI jobs retain their original model and command path.
use crate::{EngineError, Result, Session, commands::CommandSpec};
use photocraft_defold_format::{Scene, TextProto};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};

pub(super) fn err(e: impl ToString) -> EngineError {
    EngineError::Other(e.to_string())
}
fn string<'a>(p: &'a Value, k: &str) -> Result<&'a str> {
    p.get(k).and_then(Value::as_str).ok_or_else(|| err(format!("Missing string {k}")))
}
fn hash(b: &[u8]) -> String {
    blake3::hash(b).to_hex().to_string()
}
fn identity() -> Result<String> {
    let mut b = [0_u8; 16];
    getrandom::getrandom(&mut b).map_err(err)?;
    Ok(b.iter().map(|b| format!("{b:02x}")).collect())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum DocumentFocus {
    Image(u64),
    Gui(u64),
    None,
}
#[derive(Default)]
pub struct GuiWorkspace {
    pub documents: Vec<GuiDocument>,
    pub active: Option<u64>,
    next_id: u64,
    source_stamps: std::collections::HashMap<String, String>,
    #[cfg(test)]
    pub(crate) design_stamps: std::collections::HashMap<String, (String, Option<String>)>,
}
pub struct GuiDocument {
    pub id: u64,
    pub path: String,
    pub root: String,
    pub scene: Scene,
    pub selected: Option<String>,
    pub revision: u64,
    pub width: f32,
    pub height: f32,
    pub live: Option<crate::defold_live::LiveState>,
    base: String,
    undo: Vec<String>,
    redo: Vec<String>,
    pub bindings: Sidecar,
    bindings_base: Option<String>,
}
impl GuiDocument {
    pub fn dirty(&self) -> bool {
        self.live.is_none() && self.scene.proto.source != self.base
    }
    pub fn can_undo(&self) -> bool {
        if let Some(live) = &self.live {
            return !live.undo.is_empty() && live.error.is_none();
        }
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        if let Some(live) = &self.live {
            return !live.redo.is_empty() && live.error.is_none();
        }
        !self.redo.is_empty()
    }
    pub fn selected_node(&self) -> Option<&photocraft_defold_format::Node> {
        self.selected.as_ref().and_then(|id| self.scene.nodes.iter().find(|n| n.id == *id))
    }
    fn sidecar_path(&self) -> String {
        format!("{}.photocraft.json", self.path)
    }
    fn recovery_path(&self) -> String {
        format!("{}.photocraft-recovery.json", self.path)
    }
    fn recoverable(&self, source: &str) -> Result<()> {
        let bytes = serde_json::to_vec(&json!({"version":1,"baseHash":hash(self.base.as_bytes()),"source":source})).map_err(err)?;
        crate::file_cmds::write_file(&self.recovery_path(), &bytes)
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Sidecar {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub bindings: Vec<Binding>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Binding {
    pub id: String,
    pub node: String,
    pub design_source_id: String,
    pub source: String,
    pub target: String,
    pub texture: String,
    pub target_hash: String,
    pub dependencies: Vec<(String, String)>,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub source_layer: Option<u64>,
}
impl Session {
    pub fn focus(&self) -> DocumentFocus {
        if let Some(id) = self.gui.active { DocumentFocus::Gui(id) } else { self.active().map_or(DocumentFocus::None, |d| DocumentFocus::Image(d.open_id)) }
    }
    pub fn restore_focus(&mut self, focus: &DocumentFocus) {
        match focus {
            DocumentFocus::Image(id) => {
                if let Some(i) = self.docs.iter().position(|d| d.open_id == *id) {
                    self.set_active(i);
                }
            }
            DocumentFocus::Gui(id) => {
                let _ = self.activate_gui(*id);
            }
            DocumentFocus::None => {
                self.active = None;
                self.gui.active = None;
            }
        }
    }
    pub fn active_gui(&self) -> Option<&GuiDocument> {
        self.gui.active.and_then(|id| self.gui.documents.iter().find(|d| d.id == id))
    }
    pub(crate) fn active_gui_mut(&mut self) -> Result<&mut GuiDocument> {
        self.gui.active.and_then(|id| self.gui.documents.iter_mut().find(|d| d.id == id)).ok_or_else(|| err("No GUI document"))
    }
    pub fn activate_gui(&mut self, id: u64) -> Result<()> {
        if !self.gui.documents.iter().any(|d| d.id == id) {
            return Err(err("GUI document was closed"));
        }
        self.active = None;
        self.gui.active = Some(id);
        self.sync_tools_mask_target();
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn canonical(path: &Path) -> Result<PathBuf> {
    path.canonicalize().map_err(err)
}
#[cfg(target_arch = "wasm32")]
fn canonical(_: &Path) -> Result<PathBuf> {
    Err(err("Defold projects require the desktop application"))
}
fn read(path: &Path) -> Result<Vec<u8>> {
    crate::file_cmds::read_file(&path.to_string_lossy())
}
fn text(path: &Path) -> Result<String> {
    #[cfg(not(target_arch = "wasm32"))]
    let bytes = {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(path).map_err(err)?.take(photocraft_defold_format::MAX_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(err)?;
        bytes
    };
    #[cfg(target_arch = "wasm32")]
    let bytes = read(path)?;
    if bytes.len() > photocraft_defold_format::MAX_BYTES {
        return Err(err("Defold resource exceeds 16 MiB"));
    }
    String::from_utf8(bytes).map_err(err)
}
/// Defold /paths are project-relative, never OS-absolute. Symlinks cannot escape the root.
pub fn resource(root: &str, value: &str) -> Result<PathBuf> {
    let relative = value.strip_prefix('/').unwrap_or(value);
    if relative.is_empty() || relative.contains(['\\', ':']) || Path::new(relative).components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(err("Invalid project resource path"));
    }
    let root = canonical(Path::new(root))?;
    let path = canonical(&root.join(relative))?;
    if !path.starts_with(&root) {
        return Err(err("Resource leaves the Defold project"));
    }
    Ok(path)
}
fn relative(root: &str, path: &Path) -> Result<String> {
    Ok(format!("/{}", path.strip_prefix(Path::new(root)).map_err(err)?.to_string_lossy().replace('\\', "/")))
}
/// GUI's default layout uses design resolution, not the desktop window or node bounds.
pub(crate) fn project_size(root: &str) -> Result<photocraft_doc::Size> {
    let config = text(&Path::new(root).join("game.project"))?;
    let mut display = false;
    let mut size = photocraft_doc::Size::new(960, 640);
    for line in config.trim_start_matches('\u{feff}').lines().map(str::trim) {
        if line.starts_with('[') {
            display = line == "[display]";
        }
        if !display {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let slot = match key.trim() {
            "width" => &mut size.width,
            "height" => &mut size.height,
            _ => continue,
        };
        let value = value
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|v| (1..=16384).contains(v))
            .ok_or_else(|| err(format!("Invalid display.{} in game.project: expected 1..16384", key.trim())))?;
        *slot = value;
    }
    Ok(size)
}
fn open(s: &mut Session, p: &Value) -> Result<Value> {
    let path = canonical(Path::new(string(p, "path")?))?;
    if path.extension().and_then(|e| e.to_str()).is_none_or(|e| !e.eq_ignore_ascii_case("gui")) {
        return Err(err("Expected a .gui file"));
    }
    let path = path.to_string_lossy().into_owned();
    if let Some(id) = s.gui.documents.iter().find(|d| d.path == path).map(|d| d.id) {
        s.activate_gui(id)?;
        return inspect(s, &Value::Null);
    }
    let root = Path::new(&path).ancestors().skip(1).find(|p| p.join("game.project").is_file()).ok_or_else(|| err("No game.project above this GUI resource"))?;
    let root = canonical(root)?.to_string_lossy().into_owned();
    let base = text(Path::new(&path))?;
    let scene = Scene::parse(base.clone()).map_err(err)?;
    let sidepath = format!("{path}.photocraft.json");
    let (bindings, bindings_base) = if Path::new(&sidepath).is_file() {
        let raw = text(Path::new(&sidepath))?;
        let bindings: Sidecar = serde_json::from_str(&raw).map_err(err)?;
        if bindings.version != 1 {
            return Err(err("Unsupported GUI binding version"));
        }
        let mut ids = std::collections::HashSet::new();
        let mut nodes = std::collections::HashSet::new();
        if bindings.bindings.len() > 8192
            || bindings.bindings.iter().any(|b| b.id.is_empty() || b.design_source_id.is_empty() || !ids.insert(&b.id) || !nodes.insert(&b.node))
        {
            return Err(err("Invalid or ambiguous GUI bindings"));
        }
        (bindings, Some(hash(raw.as_bytes())))
    } else {
        (Sidecar { version: 1, bindings: vec![] }, None)
    };
    let size = project_size(&root)?;
    s.gui.next_id = s.gui.next_id.checked_add(1).ok_or_else(|| err("GUI identity exhausted"))?;
    let id = s.gui.next_id;
    let selected = scene.nodes.first().map(|n| n.id.clone());
    s.gui.documents.push(GuiDocument {
        id,
        path,
        root,
        scene,
        selected,
        revision: 0,
        width: size.width as f32,
        height: size.height as f32,
        live: None,
        base,
        undo: vec![],
        redo: vec![],
        bindings,
        bindings_base,
    });
    s.activate_gui(id)?;
    inspect(s, &Value::Null)
}
fn has_gui(s: &Session) -> std::result::Result<(), String> {
    s.active_gui().map(|_| ()).ok_or("No GUI document".into())
}
pub(crate) fn has_any_document(s: &Session) -> std::result::Result<(), String> {
    if s.active_gui().is_some() || !s.documents().is_empty() { Ok(()) } else { Err("No document".into()) }
}
pub(crate) fn close_set(s: &mut Session, keep_active: bool) -> Result<()> {
    let keep = if keep_active { s.gui.active } else { None };
    if s.gui.documents.iter().any(|d| Some(d.id) != keep && d.dirty()) {
        return Err(err("Save or close unsaved GUI documents first"));
    }
    s.gui.documents.retain(|d| Some(d.id) == keep);
    s.gui.active = keep;
    Ok(())
}
pub fn preview_png(s: &Session, node: &str) -> Result<Vec<u8>> {
    let d = s.active_gui().ok_or_else(|| err("No GUI"))?;
    let (path, _) = resolve(d, node)?;
    read(&path)
}
pub fn inspect(s: &mut Session, _: &Value) -> Result<Value> {
    let d = s.active_gui().ok_or_else(|| err("No GUI document"))?;
    Ok(
        json!({"id":d.id,"path":d.path,"root":d.root,"dirty":d.dirty(),"revision":d.revision,"selected":d.selected,"nodes":d.scene.nodes,"bindings":d.bindings,"width":d.width,"height":d.height,"recoveryAvailable":Path::new(&d.recovery_path()).is_file(),"preview":"approximate","editorDetected":Path::new(&d.root).join(".internal/editor.port").is_file(),"live":d.live.is_some(),"syncError":d.live.as_ref().and_then(|s|s.error.clone())}),
    )
}
fn select(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.active_gui_mut()?;
    let node = string(p, "node")?;
    if !d.scene.nodes.iter().any(|n| n.id == node) {
        return Err(err("Unknown GUI node"));
    }
    d.selected = Some(node.into());
    Ok(Value::Null)
}
fn edit(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.active_gui_mut()?;
    let node = p.get("node").and_then(Value::as_str).or(d.selected.as_deref()).ok_or_else(|| err("Select a GUI node"))?;
    if d.live.is_some() {
        let node = node.to_string();
        let property = string(p, "property")?;
        let value = p.get("value").ok_or_else(|| err("Missing value"))?;
        let (key, value) = crate::defold_live::property_value(d, &node, property, value)?;
        crate::defold_live::mutate(d, json!({"op":"set","node":node,"values":{key:value}}))?;
        return Ok(json!({"synced":true}));
    }
    let next = d.scene.edit(node, string(p, "property")?, p.get("value").ok_or_else(|| err("Missing value"))?).map_err(err)?;
    if next.proto.source == d.scene.proto.source {
        return Ok(Value::Null);
    }
    d.recoverable(&next.proto.source)?;
    d.undo.push(d.scene.proto.source.clone());
    while d.undo.len() > 100 || d.undo.iter().map(String::len).sum::<usize>() > 32 * 1024 * 1024 {
        d.undo.remove(0);
    }
    d.redo.clear();
    d.scene = next;
    d.revision += 1;
    Ok(json!({"revision":d.revision}))
}
fn history(s: &mut Session, redo: bool) -> Result<Value> {
    let d = s.active_gui_mut()?;
    if d.live.is_some() {
        crate::defold_live::history(d, redo)?;
        return Ok(json!({"synced":true}));
    }
    let source = (if redo { &d.redo } else { &d.undo }).last().ok_or_else(|| err("No GUI history"))?.clone();
    let scene = Scene::parse(source.clone()).map_err(err)?;
    d.recoverable(&source)?;
    let old = d.scene.proto.source.clone();
    if redo {
        d.redo.pop();
        d.undo.push(old);
    } else {
        d.undo.pop();
        d.redo.push(old);
    }
    d.scene = scene;
    d.revision += 1;
    Ok(Value::Null)
}
/// Guard offline disk writers while Defold advertises an editor session. Live GUI writes use editor transactions; asset publication remains guarded.
fn offline(d: &GuiDocument) -> Result<()> {
    if Path::new(&d.root).join(".internal/editor.port").is_file() {
        return Err(err(
            "Defold editor session detected. Close Defold before offline save/publish; PNG publication while the editor is open is not available yet. A stale editor.port must be removed by the project owner.",
        ));
    }
    Ok(())
}
fn save(s: &mut Session, _: &Value) -> Result<Value> {
    let d = s.active_gui_mut()?;
    if d.live.is_some() {
        return Err(err("Changes are already in Defold's live scene. Save the project in Defold; PhotoCraft will not save unrelated dirty resources."));
    }
    offline(d)?;
    if text(Path::new(&d.path))? != d.base {
        return Err(err("GUI changed on disk. Reopen/compare before saving; local edits are retained in recovery."));
    }
    crate::file_cmds::write_file(&d.path, d.scene.proto.source.as_bytes())?;
    d.base = d.scene.proto.source.clone();
    // A clean recovery checkpoint prevents a previous draft from resurfacing after a save.
    d.recoverable(&d.base)?;
    Ok(json!({"path":d.path,"saved":true}))
}
fn recover(s: &mut Session, _: &Value) -> Result<Value> {
    let d = s.active_gui_mut()?;
    let p: Value = serde_json::from_str(&text(Path::new(&d.recovery_path()))?).map_err(err)?;
    if p["version"] != 1 || p["baseHash"] != hash(d.base.as_bytes()) {
        return Err(err("Recovery base changed. Keep the recovery file for manual comparison."));
    }
    let source = string(&p, "source")?;
    let scene = Scene::parse(source.into()).map_err(err)?;
    d.undo.push(d.scene.proto.source.clone());
    d.redo.clear();
    d.scene = scene;
    d.revision += 1;
    Ok(Value::Null)
}
fn close(s: &mut Session, p: &Value) -> Result<Value> {
    let id = p.get("id").and_then(Value::as_u64).or(s.gui.active).ok_or_else(|| err("No GUI document"))?;
    let i = s.gui.documents.iter().position(|d| d.id == id).ok_or_else(|| err("Unknown GUI document"))?;
    let d = s.gui.documents.get(i).ok_or_else(|| err("Unknown GUI document"))?;
    if d.dirty() && p.get("discard").and_then(Value::as_bool) != Some(true) {
        return Err(err("GUI has unsaved changes: save or explicitly discard"));
    }
    if d.dirty() {
        d.recoverable(&d.base)?;
    }
    s.gui.documents.remove(i);
    if s.gui.active == Some(id) {
        s.gui.active = None;
        if let Some(id) = s.gui.documents.last().map(|d| d.id) {
            s.activate_gui(id)?;
        } else if !s.docs.is_empty() {
            s.set_active(0);
        }
    }
    Ok(Value::Null)
}

/// Resolve a static atlas image. Animated sequences, tile sources and custom name rewrites are
/// deliberately not guessed: publication must target exactly one existing source PNG.
fn resolve(d: &GuiDocument, node: &str) -> Result<(PathBuf, Vec<(String, String)>)> {
    let n = d.scene.nodes.iter().find(|n| n.id == node).ok_or_else(|| err("Unknown node"))?;
    if n.kind != "TYPE_BOX" || !n.editable {
        return Err(err("Select a non-template Box node with an existing atlas PNG"));
    }
    let (alias, image) = n.texture.split_once('/').ok_or_else(|| err("Node needs an atlas/image texture binding"))?;
    let textures: Vec<_> = d.scene.proto.fields.iter().filter(|f| f.name == "textures" && d.scene.proto.text(&f.children, "name") == alias).collect();
    if textures.len() != 1 {
        return Err(err("Missing or ambiguous texture alias"));
    }
    let atlas = d.scene.proto.text(&textures.first().ok_or_else(|| err("No texture"))?.children, "texture");
    let path = resource(&d.root, &atlas)?;
    if path.extension().and_then(|e| e.to_str()) != Some("atlas") {
        return Err(err("Only static atlas source images are supported for publication"));
    }
    let raw = text(&path)?;
    let p = TextProto::parse(raw.clone()).map_err(err)?;
    if !p.text(&p.fields, "rename_patterns").is_empty() {
        return Err(err("Atlas rename patterns require manual binding"));
    }
    let mut images: Vec<_> = p
        .fields
        .iter()
        .filter(|f| f.name == "images")
        .map(|f| p.text(&f.children, "image"))
        .filter(|s| Path::new(s).file_stem().and_then(|s| s.to_str()) == Some(image))
        .collect();
    for animation in p.fields.iter().filter(|f| f.name == "animations" && p.text(&f.children, "id") == image) {
        let frames: Vec<_> = animation.children.iter().filter(|f| f.name == "images").collect();
        if frames.len() != 1 {
            return Err(err("Multi-frame atlas animations require the Defold animation resolver"));
        }
        let frame = frames.first().ok_or_else(|| err("Animation has no image"))?;
        images.push(p.text(&frame.children, "image"));
    }
    if images.len() != 1 {
        return Err(err("Atlas image is missing or ambiguous"));
    }
    let png = resource(&d.root, images.first().ok_or_else(|| err("No atlas image"))?)?;
    if png.extension().and_then(|e| e.to_str()).is_none_or(|e| !e.eq_ignore_ascii_case("png")) {
        return Err(err("P2 publication requires an existing PNG"));
    }
    Ok((png, vec![(atlas, hash(raw.as_bytes()))]))
}
fn write_bindings(d: &mut GuiDocument, next: Sidecar) -> Result<()> {
    let path = d.sidecar_path();
    let current = if Path::new(&path).is_file() { Some(hash(text(Path::new(&path))?.as_bytes())) } else { None };
    if current != d.bindings_base {
        return Err(err("GUI binding file changed externally"));
    }
    let bytes = serde_json::to_vec_pretty(&next).map_err(err)?;
    crate::file_cmds::write_file(&path, &bytes)?;
    d.bindings_base = Some(hash(&bytes));
    d.bindings = next;
    Ok(())
}
fn source(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.active_gui().ok_or_else(|| err("No GUI document"))?;
    let node = p.get("node").and_then(Value::as_str).or(d.selected.as_deref()).ok_or_else(|| err("Select a node"))?.to_string();
    let gui_id = d.id;
    if let Some(b) = d.bindings.bindings.iter().find(|b| b.node == node).cloned() {
        let path = resource(&d.root, &b.source)?;
        if let Some(i) = s.docs.iter().position(|st| st.doc.design_source_id.as_deref() == Some(&b.design_source_id)) {
            let opened = s.docs.get(i).and_then(|st| st.path.as_deref()).map(Path::new).map(canonical).transpose()?;
            if opened.as_ref() != Some(&path) {
                return Err(err("This source identity is already open from another path; close that copy first"));
            }
            s.set_active(i);
            return Ok(json!({"document":i,"binding":b.id}));
        }
        let bytes = read(&path)?;
        let doc = photocraft_format::load_from_bytes(&bytes).map_err(err)?;
        if doc.design_source_id.as_deref() != Some(&b.design_source_id) {
            return Err(err("Design source identity does not match its binding"));
        }
        s.gui.source_stamps.insert(b.design_source_id.clone(), hash(&bytes));
        let i = s.add_document(doc, Some(path.to_string_lossy().into_owned()));
        return Ok(json!({"document":i,"binding":b.id}));
    }
    let (target, dependencies) = resolve(d, &node)?;
    let bytes = read(&target)?;
    let mut doc = photocraft_io::import(&target.to_string_lossy(), &bytes).map_err(err)?.document;
    let design_source_id = identity()?;
    doc.design_source_id = Some(design_source_id.clone());
    doc.name = format!("{node} · Defold source");
    let source_path = Path::new(&d.root).join(".photocraft").join("sources").join(format!("{design_source_id}.pcraft"));
    #[cfg(not(target_arch = "wasm32"))]
    {
        let parent = source_path.parent().ok_or_else(|| err("Invalid design source path"))?;
        let existing = parent.ancestors().find(|p| p.exists()).ok_or_else(|| err("Source root is missing"))?;
        if !canonical(existing)?.starts_with(Path::new(&d.root)) {
            return Err(err("Source folder leaves project"));
        }
        std::fs::create_dir_all(parent).map_err(err)?;
        let resolved = canonical(parent)?;
        if !resolved.starts_with(Path::new(&d.root)) {
            return Err(err("Source folder leaves project"));
        }
    }
    let b = Binding {
        id: identity()?,
        node: node.clone(),
        design_source_id,
        source: relative(&d.root, &source_path)?,
        target: relative(&d.root, &target)?,
        texture: d.scene.nodes.iter().find(|n| n.id == node).map(|n| n.texture.clone()).ok_or_else(|| err("Node missing"))?,
        target_hash: hash(&bytes),
        dependencies,
        width: doc.size.width,
        height: doc.size.height,
        source_layer: None,
    };
    let saved = photocraft_format::save_to_bytes(&doc, &Default::default()).map_err(err)?;
    crate::file_cmds::write_file(&source_path.to_string_lossy(), &saved)?;
    s.gui.source_stamps.insert(b.design_source_id.clone(), hash(&saved));
    let d = s.gui.documents.iter_mut().find(|d| d.id == gui_id).ok_or_else(|| err("GUI closed"))?;
    let mut next = d.bindings.clone();
    next.bindings.push(b.clone());
    write_bindings(d, next)?;
    let i = s.add_document(doc, Some(source_path.to_string_lossy().into_owned()));
    Ok(json!({"document":i,"binding":b.id,"source":source_path,"target":target}))
}
fn resource_info(s: &mut Session, _: &Value) -> Result<Value> {
    let d = s.active_gui().ok_or_else(|| err("No GUI"))?;
    let node = d.selected.as_deref().ok_or_else(|| err("Select a GUI node"))?;
    let (path, dependencies) = resolve(d, node)?;
    Ok(
        json!({"gui":d.path,"node":node,"image":path,"dependencies":dependencies,"editorSessionDetected":Path::new(&d.root).join(".internal/editor.port").is_file(),"onlineTransactions":d.live.is_some()}),
    )
}
fn publish(s: &mut Session, p: &Value) -> Result<Value> {
    let binding = string(p, "binding")?;
    if s.gui.documents.iter().flat_map(|d| &d.bindings.bindings).filter(|b| b.id == binding).count() != 1 {
        return Err(err("Open exactly one GUI with this binding identity"));
    }
    let (gi, b) = s
        .gui
        .documents
        .iter()
        .enumerate()
        .find_map(|(i, d)| d.bindings.bindings.iter().find(|b| b.id == binding).cloned().map(|b| (i, b)))
        .ok_or_else(|| err("Open the bound GUI before publishing"))?;
    let d = s.gui.documents.get(gi).ok_or_else(|| err("GUI closed"))?;
    offline(d)?;
    if Some(hash(text(Path::new(&d.sidecar_path()))?.as_bytes())) != d.bindings_base {
        return Err(err("GUI binding file changed externally"));
    }
    if text(Path::new(&d.path))? != d.base {
        return Err(err("GUI changed on disk; reopen before publishing"));
    }
    let (target, _) = resolve(d, &b.node)?;
    if relative(&d.root, &target)? != b.target || d.scene.nodes.iter().find(|n| n.id == b.node).is_none_or(|n| n.texture != b.texture) {
        return Err(err("Node resource binding changed"));
    }
    for (path, expected) in &b.dependencies {
        if hash(&read(&resource(&d.root, path)?)?) != *expected {
            return Err(err("Atlas changed since source binding; rebind before publishing"));
        }
    }
    let old = read(&target)?;
    let old_hash = hash(&old);
    let candidates: Vec<_> = s.docs.iter().filter(|st| st.doc.design_source_id.as_deref() == Some(&b.design_source_id)).collect();
    if candidates.len() != 1 {
        return Err(err("Open exactly one copy of the bound design source"));
    }
    let st = candidates.first().ok_or_else(|| err("Source closed"))?;
    let source_open = st.open_id;
    if s.job_on(st.doc.id).is_some() || st.floating.is_some() {
        return Err(err("Finish source edits before publishing"));
    }
    if st.doc.size.width != b.width || st.doc.size.height != b.height {
        return Err(err("Source dimensions changed; publication must preserve the existing PNG dimensions"));
    }
    // The visible source composite is explicit: original source + selected AI version, including alpha.
    let mut export = (*st.doc).clone();
    if let Some(layer) = b.source_layer {
        let selected = st.doc.layer(photocraft_doc::LayerId(layer)).ok_or_else(|| err("Bound source layer was deleted"))?;
        if !st.doc.layers.iter().any(|l| l.id == selected.id) {
            return Err(err("Select a top-level source layer or use the visible composite"));
        }
        export.layers = vec![selected.clone()];
    }
    crate::color_cmds::convert_document(&mut export, photocraft_cms::Builtin::Srgb.profile(), photocraft_cms::Intent::RelativeColorimetric, true)?;
    let pixels = photocraft_compose::flatten_to_surface(&export, export.pixel_format(), None);
    export.layers = vec![photocraft_doc::Layer::new("Published image", photocraft_doc::LayerContent::Raster(pixels))];
    let output = photocraft_io::export(&export, "png", &Default::default()).map_err(err)?;
    if !output.warnings.is_empty() {
        return Err(err(format!("Export requires review: {}", output.warnings.join("; "))));
    }
    let new_hash = hash(&output.bytes);
    if old_hash != b.target_hash && old_hash != new_hash {
        return Err(err("Target PNG changed externally; publication stopped"));
    }
    let source_path = resource(&d.root, &b.source)?;
    let disk_source = read(&source_path)?;
    if s.gui.source_stamps.get(&b.design_source_id).is_none_or(|h| *h != hash(&disk_source)) {
        // A normal explicit source Save is accepted only if it is the very document now being
        // published. Any other disk contents require re-opening, never a blind overwrite.
        let disk_doc = photocraft_format::load_from_bytes(&disk_source).map_err(err)?;
        if disk_doc != *st.doc {
            return Err(err("Editable source changed on disk; save/reopen it before publishing"));
        }
    }
    let native = photocraft_format::save_to_bytes(&st.doc, &Default::default()).map_err(err)?;
    // Recovery source saved first. A failed publish never destroys the editable result.
    crate::file_cmds::write_file(&source_path.to_string_lossy(), &native)?;
    s.gui.source_stamps.insert(b.design_source_id.clone(), hash(&native));
    if old_hash != new_hash {
        let backup = format!("{}.photocraft-backup-{}.png", target.to_string_lossy(), b.target_hash);
        if !Path::new(&backup).is_file() {
            crate::file_cmds::write_file(&backup, &old)?;
        }
        if hash(&read(&target)?) != old_hash {
            return Err(err("PNG changed during export"));
        }
        crate::file_cmds::write_file(&target.to_string_lossy(), &output.bytes)?;
    }
    let d = s.gui.documents.get_mut(gi).ok_or_else(|| err("GUI closed"))?;
    let mut next = d.bindings.clone();
    if let Some(b) = next.bindings.iter_mut().find(|b| b.id == binding) {
        b.target_hash = new_hash.clone();
    }
    write_bindings(d, next)?;
    if let Some(st) = s.docs.iter_mut().find(|st| st.open_id == source_open) {
        st.saved_to(source_path.to_string_lossy().into_owned());
    }
    Ok(json!({"published":true,"target":target,"hash":new_hash,"source":source_path}))
}
fn bind_layer(s: &mut Session, p: &Value) -> Result<Value> {
    let id = string(p, "binding")?;
    let st = s.active().ok_or_else(|| err("Open the image source"))?;
    let source = st.doc.design_source_id.clone().ok_or_else(|| err("Not a bound design source"))?;
    let layer = if p.get("composite").and_then(Value::as_bool) == Some(true) {
        None
    } else {
        let layer = st.active_layer.ok_or_else(|| err("Select a source layer"))?;
        if !st.doc.layers.iter().any(|l| l.id == layer) {
            return Err(err("Select a top-level layer or group"));
        }
        Some(layer.0)
    };
    let d = s
        .gui
        .documents
        .iter_mut()
        .find(|d| d.bindings.bindings.iter().any(|b| b.id == id && b.design_source_id == source))
        .ok_or_else(|| err("Binding does not belong to the current source"))?;
    let mut next = d.bindings.clone();
    if let Some(b) = next.bindings.iter_mut().find(|b| b.id == id) {
        b.source_layer = layer;
    }
    write_bindings(d, next)?;
    Ok(json!({"layer":layer}))
}

fn selected(d: &GuiDocument, p: &Value) -> Result<String> {
    Ok(p.get("node").and_then(Value::as_str).or(d.selected.as_deref()).ok_or_else(|| err("Select a GUI node"))?.into())
}
fn commit(d: &mut GuiDocument, next: Scene) -> Result<Value> {
    if next.proto.source == d.scene.proto.source {
        return Ok(Value::Null);
    }
    d.recoverable(&next.proto.source)?;
    d.undo.push(d.scene.proto.source.clone());
    d.redo.clear();
    while d.undo.len() > 100 || d.undo.iter().map(String::len).sum::<usize>() > 32 * 1024 * 1024 {
        d.undo.remove(0);
    }
    d.scene = next;
    d.revision += 1;
    if d.selected.as_ref().is_some_and(|id| !d.scene.nodes.iter().any(|n| n.id == *id)) {
        d.selected = d.scene.nodes.first().map(|n| n.id.clone());
    }
    Ok(json!({"revision":d.revision}))
}
fn add_node(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.active_gui_mut()?;
    let id = if let Some(id) = p.get("id").and_then(Value::as_str) {
        id.to_string()
    } else {
        (1..=8193).map(|i| format!("node_{i}")).find(|id| !d.scene.nodes.iter().any(|n| n.id == *id)).ok_or_else(|| err("Node limit"))?
    };
    let kind = p.get("kind").and_then(Value::as_str).unwrap_or("TYPE_BOX");
    let parent = p.get("parent").and_then(Value::as_str).unwrap_or("");
    let after = p.get("after").and_then(Value::as_str);
    let next = d.scene.new_node(&id, kind, parent, after).map_err(err)?;
    if d.live.is_some() {
        let mut values = json!({"manual_size":[100,40,0]});
        if kind == "TYPE_TEXT" {
            values["text"] = json!("Text");
            if let Some(f) = d.scene.proto.fields.iter().find(|f| f.name == "fonts") {
                values["font"] = json!(d.scene.proto.text(&f.children, "name"));
            }
        }
        let mut req =
            json!({"op":"add","id":id,"kind":format!("gui-node-type-{}",kind.trim_start_matches("TYPE_").to_lowercase()),"parent":parent,"values":values});
        if let Some(after) = after {
            req["after"] = json!(after);
        }
        crate::defold_live::mutate(d, req)?;
    } else {
        commit(d, next)?;
    }
    d.selected = Some(id.clone());
    Ok(json!({"node":id}))
}
fn delete_node(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.active_gui_mut()?;
    let id = selected(d, p)?;
    if d.live.is_some() {
        crate::defold_live::mutate(d, json!({"op":"delete","node":id}))?;
        Ok(Value::Null)
    } else {
        let next = d.scene.remove_node(&id).map_err(err)?;
        commit(d, next)
    }
}
fn rename_node(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.active_gui_mut()?;
    let id = selected(d, p)?;
    let name = string(p, "name")?;
    let next = d.scene.rename_node(&id, name).map_err(err)?;
    // Bindings use stable authored IDs. Refuse a rename until a future binding migration
    // can atomically update its sidecar too; never silently retarget a generated source.
    if d.bindings.bindings.iter().any(|b| b.node == id) {
        return Err(err("This node has a design-source binding; keep its ID to preserve the link"));
    }
    if d.live.is_some() {
        crate::defold_live::mutate(d, json!({"op":"set","node":id,"values":{"id":name}}))?;
    } else {
        commit(d, next)?;
    }
    d.selected = Some(name.into());
    Ok(Value::Null)
}
fn move_node(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.active_gui_mut()?;
    let id = selected(d, p)?;
    let parent = string(p, "parent")?;
    let before = p.get("before").and_then(Value::as_str);
    let next = d.scene.move_node(&id, parent, before).map_err(err)?;
    if d.live.is_some() {
        if d.scene.nodes.iter().find(|n| n.id == id).is_none_or(|n| n.parent != parent) {
            let mut req = json!({"op":"move","node":id,"parent":parent});
            if let Some(before) = before {
                req["after"] = json!(before);
            }
            crate::defold_live::mutate(d, req)?;
            return Ok(Value::Null);
        }
        let order: Vec<_> = next.nodes.iter().filter(|n| n.parent == parent).map(|n| n.id.clone()).collect();
        crate::defold_live::mutate(d, json!({"op":"reorder","parent":parent,"order":order}))?;
        Ok(Value::Null)
    } else {
        commit(d, next)
    }
}
fn edit_many(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.active_gui_mut()?;
    let id = selected(d, p)?;
    let values = p.get("values").and_then(Value::as_object).ok_or_else(|| err("Expected values"))?;
    if values.is_empty() || values.len() > 64 {
        return Err(err("Invalid property count"));
    }
    if let Some(live) = &d.live {
        let mut node = live.snapshot.nodes.iter().find(|n| n.id == id).cloned().ok_or_else(|| err("Missing live node"))?;
        let mut merged = serde_json::Map::new();
        for (k, v) in values {
            let (key, value) = crate::defold_live::node_property_value(&node, k, v)?;
            node.properties.insert(key.clone(), value.clone());
            merged.insert(key, value);
        }
        crate::defold_live::mutate(d, json!({"op":"set","node":id,"values":merged}))?;
        Ok(Value::Null)
    } else {
        let mut next = d.scene.clone();
        for (k, v) in values {
            next = next.edit(&id, k, v).map_err(err)?;
        }
        commit(d, next)
    }
}

pub fn specs() -> Vec<CommandSpec> {
    macro_rules! spec {
        ($id:literal,$label:literal,$params:literal,$en:expr,$run:expr) => {
            CommandSpec { id: $id, label: $label, menu: &[], shortcut: None, params: $params, enabled: $en, run: $run, journal: false }
        };
    }
    vec![
        spec!("defold.openProject", "Open Defold Project", r#"{"path":"project directory"}"#, crate::file_cmds::native, open_project),
        spec!("defold.openNodeResourceInEditor", "Open Node Resource in Defold", "{}", has_gui, |s, _| {
            let d = s.active_gui().ok_or_else(|| err("No GUI"))?;
            let node = d.selected.as_deref().ok_or_else(|| err("Select a node"))?;
            let (target, _) = resolve(d, node)?;
            crate::defold_live::request(&d.root, &json!({"op":"openResource","path":crate::defold_live::path(d)?,"resource":relative(&d.root,&target)?}))?;
            Ok(Value::Null)
        }),
        spec!("defold.connectLive", "Connect Defold Editor", "{}", has_gui, |s, _| {
            let d = s.active_gui_mut()?;
            if d.dirty() {
                return Err(err("Save offline GUI edits before connecting"));
            }
            crate::defold_live::connect(d)?;
            Ok(json!({"connected":true}))
        }),
        spec!("defold.saveProject", "Save Defold Project", r#"{"allModifiedResources":true}"#, has_gui, |s, p| {
            if p["allModifiedResources"] != true {
                return Err(err("Defold saves all modified project resources; explicit allModifiedResources: true is required"));
            }
            let d = s.active_gui_mut()?;
            let live = d.live.as_ref().ok_or_else(|| err("Connect to Defold first"))?;
            let snapshot = crate::defold_live::request(
                &d.root,
                &json!({"op":"saveProject","path":crate::defold_live::path(d)?,"expected":live.snapshot.nodes,"allModifiedResources":true}),
            )?;
            crate::defold_live::accept(d, snapshot)?;
            d.base = text(Path::new(&d.path))?;
            Ok(json!({"saved":"all-modified-project-resources"}))
        }),
        spec!("defold.refreshLive", "Refresh Defold Scene", "{}", has_gui, |s, _| {
            let d = s.active_gui_mut()?;
            if d.live.is_none() {
                return Err(err("Connect to Defold first"));
            }
            crate::defold_live::connect(d)?;
            Ok(Value::Null)
        }),
        spec!("defold.receiveLiveSnapshot", "Receive Defold Scene", r#"{"id":number,"revision":number,"snapshot":object}"#, has_gui, |s, p| {
            let d = s.active_gui_mut()?;
            if Some(d.id) != p["id"].as_u64() || Some(d.revision) != p["revision"].as_u64() {
                return Ok(json!({"stale":true}));
            }
            if d.live.as_ref().is_none_or(|s| s.error.is_some()) {
                return Ok(json!({"stale":true}));
            }
            let snapshot: crate::defold_live::Snapshot = serde_json::from_value(p["snapshot"].clone()).map_err(err)?;
            if snapshot.path != crate::defold_live::path(d)? {
                return Err(err("Snapshot resource does not match"));
            }
            crate::defold_live::refresh(d, snapshot)?;
            Ok(Value::Null)
        }),
        spec!(
            "defold.addNode",
            "Add GUI Node",
            r#"{"id":string?,"kind":"TYPE_BOX|TYPE_TEXT|TYPE_PIE|TYPE_PARTICLEFX","parent":string?,"after":string?}"#,
            has_gui,
            add_node
        ),
        spec!("defold.deleteNode", "Delete GUI Node", r#"{"node":string?}"#, has_gui, delete_node),
        spec!("defold.renameNode", "Rename GUI Node", r#"{"node":string?,"name":string}"#, has_gui, rename_node),
        spec!("defold.moveNode", "Move GUI Node", r#"{"node":string?,"parent":string,"before":string?}"#, has_gui, move_node),
        spec!("defold.setNodeProperties", "Transform GUI Node", r#"{"node":string?,"values":{property:value}}"#, has_gui, edit_many),
        spec!("defold.openGui", "Open Defold GUI", r#"{"path":"absolute .gui path"}"#, crate::file_cmds::native, open),
        spec!("defold.activateGui", "Activate GUI", r#"{"id":number}"#, crate::commands::always, |s, p| {
            s.activate_gui(p["id"].as_u64().ok_or_else(|| err("Missing GUI id"))?)?;
            Ok(Value::Null)
        }),
        spec!("defold.inspectGui", "Inspect GUI", "{}", has_gui, inspect),
        spec!("defold.selectNode", "Select GUI Node", r#"{"node":"id"}"#, has_gui, select),
        spec!(
            "defold.setNodeProperty",
            "Edit GUI Node",
            r#"{"node":"id"?,"property":"position.x|position.y|size.x|size.y|scale.x|scale.y|rotation.z|text|alpha|visible|enabled","value":value}"#,
            has_gui,
            edit
        ),
        spec!("defold.saveGui", "Save GUI", "{}", has_gui, save),
        spec!(
            "defold.undo",
            "Undo GUI",
            "{}",
            |s| {
                has_gui(s)?;
                if s.active_gui().is_some_and(|d| d.can_undo()) { Ok(()) } else { Err("No GUI history".into()) }
            },
            |s, _| history(s, false)
        ),
        spec!(
            "defold.redo",
            "Redo GUI",
            "{}",
            |s| {
                has_gui(s)?;
                if s.active_gui().is_some_and(|d| d.can_redo()) { Ok(()) } else { Err("No GUI history".into()) }
            },
            |s, _| history(s, true)
        ),
        spec!("defold.closeGui", "Close GUI", r#"{"id":number?,"discard":bool?}"#, crate::commands::always, close),
        spec!("defold.recoverGui", "Recover GUI Draft", "{}", has_gui, recover),
        spec!("defold.openNodeSource", "Edit Node Image Source", r#"{"node":"id"?}"#, has_gui, source),
        spec!("defold.nodeResource", "Locate Node Resource", "{}", has_gui, resource_info),
        spec!("defold.revealNodeResource", "Show Resource in Folder", r#"{"dryRun":bool=false}"#, has_gui, |s, p| {
            let v = resource_info(s, p)?;
            crate::layer_menu_cmds::reveal(string(&v, "image")?, p.get("dryRun").and_then(Value::as_bool).unwrap_or(false))
        }),
        spec!("defold.bridgeStatus", "Inspect Defold Editor Capabilities", "{}", has_gui, bridge_status),
        spec!("defold.publishNodeSource", "Publish Node Image", r#"{"binding":"binding id"}"#, crate::file_cmds::native, publish),
        spec!("defold.bindSourceLayer", "Link Source Layer", r#"{"binding":"binding id","composite":bool=false}"#, crate::commands::always, bind_layer),
    ]
}

fn open_project(_: &mut Session, p: &Value) -> Result<Value> {
    let root = canonical(Path::new(string(p, "path")?))?;
    if !root.join("game.project").is_file() {
        return Err(err("Choose the folder containing game.project"));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut pending = vec![(root.clone(), 0)];
        let mut seen = std::collections::HashSet::new();
        let mut files = Vec::new();
        let mut count = 0;
        while let Some((dir, depth)) = pending.pop() {
            if depth > 32 || !seen.insert(dir.clone()) {
                continue;
            }
            for e in std::fs::read_dir(&dir).map_err(err)? {
                count += 1;
                if count > 200000 {
                    return Err(err("Project scan exceeds limit"));
                }
                let e = e.map_err(err)?;
                let kind = e.file_type().map_err(err)?;
                if kind.is_symlink() {
                    continue;
                }
                let path = e.path();
                if kind.is_dir() {
                    if !matches!(e.file_name().to_str(), Some("build" | ".internal" | ".git" | ".photocraft" | "node_modules")) {
                        pending.push((path, depth + 1));
                    }
                } else if path.extension().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case("gui")) {
                    if files.len() >= 4096 {
                        return Err(err("Project has more than 4096 GUI resources"));
                    }
                    files.push(relative(&root.to_string_lossy(), &path)?);
                }
            }
        }
        files.sort();
        Ok(json!({"root":root,"files":files}))
    }
    #[cfg(target_arch = "wasm32")]
    {
        Err(err("Defold projects require desktop"))
    }
}

fn bridge_status(s: &mut Session, _: &Value) -> Result<Value> {
    let d = s.active_gui().ok_or_else(|| err("No GUI document"))?;
    let portfile = Path::new(&d.root).join(".internal/editor.port");
    if !portfile.is_file() {
        return Ok(json!({"sessionDetected":false,"mode":"offline","onlineTransactions":d.live.is_some()}));
    }
    let port: u16 = text(&portfile)?.trim().parse().map_err(err)?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::io::{Read, Write};
        let probe = || -> Result<Value> {
            let mut stream = std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), std::time::Duration::from_millis(500))
                .map_err(err)?;
            stream.set_read_timeout(Some(std::time::Duration::from_millis(500))).map_err(err)?;
            stream.set_write_timeout(Some(std::time::Duration::from_millis(500))).map_err(err)?;
            stream.write_all(b"GET /openapi.json HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n").map_err(err)?;
            let mut bytes = Vec::new();
            stream.take(2 * 1024 * 1024).read_to_end(&mut bytes).map_err(err)?;
            let response = String::from_utf8(bytes).map_err(err)?;
            let (headers, body) = response.split_once("\r\n\r\n").ok_or_else(|| err("Invalid editor HTTP response"))?;
            if !headers.lines().next().is_some_and(|l| l.contains(" 200 ")) {
                return Err(err("Editor capability endpoint did not return HTTP 200"));
            }
            let api: Value = serde_json::from_str(body).map_err(err)?;
            // Never return the API's session token/security material to the UI, journal or logs.
            let routes = api.get("paths").and_then(Value::as_object).map(|p| p.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
            Ok(json!({"sessionDetected":true,"reachable":true,"routes":routes,"onlineTransactions":d.live.is_some(),"mode":"read-only probe"}))
        };
        match probe() {
            Ok(v) => Ok(v),
            Err(e) => Ok(json!({"sessionDetected":true,"reachable":false,"error":e.to_string(),"onlineTransactions":d.live.is_some()})),
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = port;
        Err(err("Editor probing requires desktop"))
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "defold_cmds_tests.rs"]
mod tests;
