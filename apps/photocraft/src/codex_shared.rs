//! Machine-local connection metadata; document threads keep their original identity.
use serde_json::{Value, json};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn directory() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("PHOTOCRAFT_CODEX_BRIDGE_DIR") {
        return Ok(PathBuf::from(path));
    }
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from).ok_or("APPDATA is unavailable")?;
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(PathBuf::from).ok_or("HOME is unavailable")?.join("Library/Application Support");
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
        .ok_or("Configuration directory unavailable")?;
    Ok(base.join("Photocraft/CodexBridge"))
}

fn read(path: &Path, limit: usize) -> Result<Value, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit as u64 {
        return Err("Invalid shared connection metadata".into());
    }
    let mut bytes = Vec::new();
    File::open(path).map_err(|e| e.to_string())?.take(limit.saturating_add(1) as u64).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("Shared connection metadata exceeds limit".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

fn write(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path.parent().ok_or("Missing connection directory")?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    serde_json::to_writer(file.as_file_mut(), value).map_err(|e| e.to_string())?;
    file.flush().map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

pub(super) fn executable() -> Result<Option<PathBuf>, String> {
    let path = directory()?.join("config.json");
    if !path.exists() {
        return Ok(None);
    }
    let config = read(&path, 64 * 1024)?;
    if config["enabled"] == false {
        return Ok(None);
    }
    if config["version"] != 1 || config["enabled"] != true {
        return Err("Invalid PhotoCraft shared connection settings".into());
    }
    let executable = config["executable"].as_str().map(PathBuf::from).ok_or("Missing shared connection component")?;
    if !executable.is_absolute() || !executable.is_file() {
        return Err("PhotoCraft shared connection component is missing; repair the connection setup".into());
    }
    Ok(Some(executable))
}

pub(super) const CANVAS: &str = ".photocraft-canvas-active.json";
pub(super) const ASSETS: &str = ".photocraft-assets-active.json";

pub(super) struct Lease {
    path: PathBuf,
    id: String,
    _lock: File,
}
impl Lease {
    pub(super) fn create(workspace: &Path, name: &str, mut value: Value) -> Result<Self, String> {
        let path = workspace.join(name);
        let lock =
            File::options().create(true).truncate(false).read(true).write(true).open(workspace.join(format!("{name}.lock"))).map_err(|e| e.to_string())?;
        lock.try_lock().map_err(|_| "This project's tool context is in use; wait for the current request to finish")?;
        let id = photocraft_automation::security::generate_token().map_err(|e| e.to_string())?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?.as_millis();
        let expires = u64::try_from(now).map_err(|_| "Invalid system clock")?.saturating_add(660_000);
        value["version"] = json!(1);
        value["leaseId"] = json!(id);
        value["expiresAtMs"] = json!(expires);
        write(&path, &value)?;
        Ok(Self { path, id, _lock: lock })
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if read(&self.path, 48 * 1024 * 1024).is_ok_and(|value| value["leaseId"] == self.id) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub(super) fn canvas_params(params: &mut Value, workspace: &Path, helper: &Path) {
    crate::ai_generation::enable_canvas_tool(params, &workspace.join(CANVAS), helper);
    params["config"]["mcp_servers"]["photocraft_canvas"]["args"] = json!(["--canvas-mcp-live", workspace.join(CANVAS)]);
}

pub(super) fn asset_params(params: &mut Value, workspace: &Path, helper: &Path) {
    params["developerInstructions"] = json!(crate::ai_asset_agent::director_instructions());
    let tools: serde_json::Map<String, Value> = [
        "design_context",
        "project_history",
        "generate_assets",
        "asset_inspect",
        "asset_extract_batch",
        "layers_preview",
        "layers_apply_batch",
        "generation_status",
    ]
    .into_iter()
    .map(|name| (name.into(), json!({"approval_mode":"approve"})))
    .collect();
    params["config"]["mcp_servers"] = json!({"photocraft_assets":{
        "command":helper,"args":["--asset-mcp-live",workspace.join(ASSETS)],"required":true,
        "tool_timeout_sec":600,"startup_timeout_sec":30,"tools":tools}});
}

fn template(workspace: &Path, params: &Value) -> Value {
    let mut params = params.clone();
    if let Some(object) = params.as_object_mut() {
        for key in ["threadId", "projectId", "model", "ephemeral"] {
            object.remove(key);
        }
    }
    json!({"workspace":workspace,"params":params})
}

pub(super) fn register(thread: &str, workspace: &Path, params: &Value) -> Result<(), String> {
    if executable()?.is_none() {
        return Ok(());
    }
    register_in(&directory()?, thread, workspace, params)
}

fn register_in(dir: &Path, thread: &str, workspace: &Path, params: &Value) -> Result<(), String> {
    if thread.is_empty() || thread.len() > 128 || !thread.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') || !workspace.is_absolute() {
        return Err("Invalid persistent design conversation binding".into());
    }
    let lock = File::options().create(true).truncate(false).read(true).write(true).open(dir.join("templates.lock")).map_err(|e| e.to_string())?;
    lock.try_lock().map_err(|_| "Design connection settings are being updated; retry after the other request finishes")?;
    let path = dir.join("templates.json");
    let mut all = if path.exists() { read(&path, 8 * 1024 * 1024)? } else { json!({}) };
    let object = all.as_object_mut().ok_or("Invalid design connection registry")?;
    if object.len() >= 2048 && !object.contains_key(thread) {
        return Err("Design connection registry is full".into());
    }
    object.insert(thread.into(), template(workspace, params));
    write(&path, &all)
}

/// Run before the desktop restarts so opening an existing thread first has live tools.
pub(super) fn bootstrap() -> Result<usize, String> {
    let helper = executable()?.ok_or("Shared connection is not configured")?;
    let root = crate::app_dirs::config_dir().ok_or("PhotoCraft configuration is unavailable")?.join("AI-Projects");
    if !root.exists() {
        return Ok(0);
    }
    let mut count = 0;
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())?.take(2048) {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            continue;
        }
        for role in ["renderer", "assets"] {
            let path = entry.path().join(format!("codex-{role}-session.json"));
            if !path.exists() {
                continue;
            }
            let state = read(&path, 64 * 1024)?;
            let Some(thread) = state["threadId"].as_str() else { continue };
            let workspace = state["workspace"].as_str().map(PathBuf::from).ok_or("Design workspace is missing")?;
            if !workspace.is_dir() {
                continue;
            }
            let mut params = crate::ai_generation::codex_thread_params(&workspace, "", role == "renderer");
            if role == "renderer" {
                canvas_params(&mut params, &workspace, &helper);
            } else {
                asset_params(&mut params, &workspace, &helper);
            }
            register(thread, &workspace, &params)?;
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lease_prevents_overlapping_context_and_releases_after_drop() {
        let dir = tempfile::tempdir().unwrap();
        let first = Lease::create(dir.path(), CANVAS, json!({"snapshot":{}})).unwrap();
        assert!(Lease::create(dir.path(), CANVAS, json!({"snapshot":{"document":{"revision":2}}})).is_err());
        drop(first);
        let second = Lease::create(dir.path(), CANVAS, json!({"snapshot":{"document":{"revision":2}}})).unwrap();
        assert_eq!(read(&dir.path().join(CANVAS), 1024).unwrap()["snapshot"]["document"]["revision"], 2);
        drop(second);
        assert!(!dir.path().join(CANVAS).exists());
    }
    #[test]
    fn registry_keeps_identity_and_does_not_freeze_model() {
        let dir = tempfile::tempdir().unwrap();
        let params = json!({"model":"selected","threadId":"wrong","projectId":"wrong","ephemeral":true,"config":{"features.image_generation":true}});
        register_in(dir.path(), "original-id", dir.path(), &params).unwrap();
        let all = read(&dir.path().join("templates.json"), 4096).unwrap();
        let saved = &all["original-id"]["params"];
        for key in ["threadId", "projectId", "model", "ephemeral"] {
            assert!(saved.get(key).is_none());
        }
        assert_eq!(saved["config"]["features.image_generation"], true);
    }
}
