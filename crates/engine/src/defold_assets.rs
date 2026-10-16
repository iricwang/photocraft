//! Named, owned game assets. Publication is explicit and updates use checksum guards.
#[cfg(test)]
use crate::{Result, defold_cmds::err};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[cfg(test)]
use std::path::{Path, PathBuf};

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Asset {
    pub key: String,
    pub node: String,
    pub layer: u64,
    pub image: String,
    pub hash: String,
    #[serde(default)]
    pub name: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Registry {
    pub version: u32,
    pub gui: String,
    pub assets: Vec<Asset>,
    #[serde(default)]
    pub nodes: BTreeMap<String, String>,
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub retired: BTreeMap<String, String>,
    #[cfg(test)]
    #[serde(skip)]
    pub source_hash: Option<String>,
}
#[cfg(test)]
pub(crate) fn digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}
pub(crate) fn name(label: &str) -> String {
    let mut label: String =
        label.trim().trim_matches('.').chars().take(48).map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
    if label.is_empty() {
        label = "Layer".into();
    }
    if matches!(
        label.split('.').next().unwrap_or("").to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        label.insert(0, '_');
    }
    label
}
#[cfg(test)]
pub(crate) fn path(root: &str, resource: &str) -> Result<PathBuf> {
    if !resource.starts_with("/photocraft_generated/")
        || resource.contains(['\\', ':', '\0'])
        || resource.trim_start_matches('/').split('/').any(|s| s.is_empty() || s.starts_with('.'))
    {
        return Err(err("Invalid managed asset path"));
    }
    let result = Path::new(root).join(resource.trim_start_matches('/'));
    #[cfg(not(target_arch = "wasm32"))]
    {
        let canonical_root = Path::new(root).canonicalize().map_err(err)?;
        for parent in result.ancestors() {
            if parent.exists() && !parent.canonicalize().map_err(err)?.starts_with(&canonical_root) {
                return Err(err("Managed asset leaves its project"));
            }
            if parent == Path::new(root) {
                break;
            }
        }
    }
    Ok(result)
}
#[cfg(test)]
fn read(root: &str, resource: &str) -> Result<Option<Vec<u8>>> {
    let path = path(root, resource)?;
    if !path.exists() {
        return Ok(None);
    }
    crate::file_cmds::read_file(&path.to_string_lossy()).map(Some)
}
#[cfg(test)]
pub(crate) fn load(root: &str, folder: &str) -> Result<Registry> {
    let Some(bytes) = read(root, &format!("{folder}/assets.json"))? else { return Ok(Registry::default()) };
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(err("Asset registry exceeds limit"));
    }
    let mut registry: Registry = serde_json::from_slice(&bytes).map_err(err)?;
    if registry.version != 2 {
        return Err(err("Unsupported asset registry"));
    }
    for resource in registry.files.keys().chain(registry.retired.keys()) {
        if !resource.starts_with(&format!("{folder}/")) {
            return Err(err("Asset registry leaves its GUI folder"));
        }
        path(root, resource)?;
    }
    registry.source_hash = Some(digest(&bytes));
    Ok(registry)
}
// The remote endpoint checks the same precondition before replacing or deleting a file.
#[cfg(test)]
fn change(root: &str, resource: &str, before: Option<&[u8]>, after: Option<&[u8]>) -> Result<()> {
    let target = path(root, resource)?;
    let current = read(root, resource)?;
    if current.as_deref() != before {
        return Err(err(format!("Managed asset changed during publication: {resource}")));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if Path::new(root).join(".photocraft-remote").is_file() {
            use base64::Engine;
            use sha2::Digest;
            let peer = crate::peer_binding::active_for(root)?.ok_or_else(|| err("Reconnect the asset project"))?;
            let result = crate::peer_binding::native::json_request(
                &peer.address,
                "/binding/upload",
                &peer.token,
                &serde_json::json!({
                    "path":resource,"managed":true,"expected":before.map(|bytes|format!("{:x}",sha2::Sha256::digest(bytes))),
                    "remove":after.is_none(),"data":after.map(|bytes|base64::engine::general_purpose::STANDARD.encode(bytes))
                }),
            )?;
            if result["retained"] == true {
                return Err(err("An asset is still referenced in the remote project"));
            }
        }
        if let Some(bytes) = after {
            std::fs::create_dir_all(target.parent().ok_or_else(|| err("Missing asset folder"))?).map_err(err)?;
            // Revalidate after creating directories, before the atomic replacement.
            path(root, resource)?;
            crate::file_cmds::write_file(&target.to_string_lossy(), bytes)?;
        } else if target.exists() {
            std::fs::remove_file(target).map_err(err)?;
        }
        Ok(())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (target, after);
        Err(err("Game asset publication requires desktop"))
    }
}
#[cfg(test)]
pub(crate) struct Stage {
    root: String,
    changed: Vec<(String, Option<Vec<u8>>, Vec<u8>)>,
}
#[cfg(test)]
impl Stage {
    pub fn rollback(&self) {
        for (resource, before, after) in self.changed.iter().rev() {
            let _ = change(&self.root, resource, Some(after), before.as_deref());
        }
    }
}
#[cfg(test)]
pub(crate) fn stage(root: &str, folder: &str, previous: &Registry, files: &[(String, Vec<u8>)]) -> Result<Stage> {
    let mut result = Stage { root: root.into(), changed: vec![] };
    // Validate every existing file before mutating any of them.
    for (resource, bytes) in files {
        let current = read(root, resource)?;
        let target = path(root, resource)?;
        let same_case = target.canonicalize().ok().and_then(|p| p.file_name().map(|s| s.to_owned())) == target.file_name().map(|s| s.to_owned());
        if current.as_deref() == Some(bytes) && same_case {
            continue;
        }
        let expected = if resource == &format!("{folder}/assets.json") {
            previous.source_hash.as_ref()
        } else {
            previous.files.get(resource).or_else(|| previous.files.iter().find(|(p, _)| p.to_lowercase() == resource.to_lowercase()).map(|(_, hash)| hash))
        };
        if current.as_ref().map(|b| digest(b)).as_ref() != expected {
            return Err(err(format!("Generated resource changed externally: {resource}")));
        }
        result.changed.push((resource.clone(), current, bytes.clone()));
    }
    for (resource, before, bytes) in &result.changed {
        if let Err(e) = change(root, resource, before.as_deref(), Some(bytes)) {
            result.rollback();
            return Err(e);
        }
    }
    Ok(result)
}

/// Conservative reference check: the project must be readable before retirement is permitted.
#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
pub(crate) fn referenced(root: &str, resource: &str) -> Result<bool> {
    let mut directories = vec![PathBuf::from(root)];
    let mut count = 0;
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory).map_err(err)? {
            let entry = entry.map_err(err)?;
            count += 1;
            if count > 20000 {
                return Ok(true);
            }
            let kind = entry.file_type().map_err(err)?;
            let label = entry.file_name();
            let label = label.to_string_lossy();
            if kind.is_symlink() {
                return Ok(true);
            }
            if label.starts_with('.') || matches!(label.as_ref(), "build" | "target") {
                continue;
            }
            if kind.is_dir() {
                directories.push(entry.path());
                continue;
            }
            let path = entry.path();
            let relative = path.strip_prefix(root).map_err(err)?.to_string_lossy().replace('\\', "/");
            if format!("/{relative}") == resource
                || (relative.starts_with("photocraft_generated/") && (relative.contains("/manifests/") || label == "assets.json"))
            {
                continue;
            }
            if !matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("gui" | "atlas" | "tilesource" | "sprite" | "collection" | "go" | "script" | "gui_script" | "lua" | "particlefx" | "json" | "material")
            ) {
                continue;
            }
            if entry.metadata().map_err(err)?.len() > 16 * 1024 * 1024 {
                return Ok(true);
            }
            let bytes = std::fs::read(&path).map_err(err)?;
            let Ok(text) = String::from_utf8(bytes) else { return Ok(true) };
            if text.contains(resource) || text.contains(resource.trim_start_matches('/')) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
#[cfg(test)]
pub(crate) fn retire(root: &str, registry: &mut Registry) -> Vec<String> {
    let mut warnings = vec![];
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut candidates: Vec<_> = registry.retired.clone().into_iter().collect();
        // Remove obsolete atlases before testing whether their PNG files remain referenced.
        candidates.sort_by_key(|(p, _)| {
            if p.ends_with(".json") {
                0
            } else if p.ends_with(".atlas") {
                1
            } else {
                2
            }
        });
        for (resource, expected) in candidates {
            if registry.files.keys().any(|p| p.to_lowercase() == resource.to_lowercase()) {
                registry.retired.remove(&resource);
                continue;
            }
            let result = (|| {
                let Some(bytes) = read(root, &resource)? else {
                    registry.retired.remove(&resource);
                    return Ok(());
                };
                if digest(&bytes) != expected {
                    return Err(err(format!("Retained externally modified asset: {resource}")));
                }
                if !referenced(root, &resource)? {
                    change(root, &resource, Some(&bytes), None)?;
                    registry.retired.remove(&resource);
                }
                Ok(())
            })();
            if let Err(e) = result {
                warnings.push(e.to_string());
            }
        }
    }
    let _ = (root, registry);
    warnings
}
#[cfg(test)]
pub(crate) fn legacy(root: &str, folder: &str, registry: &mut Registry) -> Result<()> {
    #[cfg(not(target_arch = "wasm32"))]
    for directory in ["images", "atlases", "manifests"] {
        let directory = path(root, &format!("{folder}/{directory}"))?;
        if !directory.exists() {
            continue;
        }
        for entry in std::fs::read_dir(directory).map_err(err)? {
            let entry = entry.map_err(err)?;
            if !entry.file_type().map_err(err)?.is_file() {
                continue;
            }
            let file = entry.path();
            let Some(stem) = file.file_stem().and_then(|s| s.to_str()) else { continue };
            let stem = stem.strip_prefix("img_").unwrap_or(stem);
            if stem.len() != 64 || !stem.bytes().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }
            let bytes = std::fs::read(&file).map_err(err)?;
            if digest(&bytes) != stem {
                continue;
            }
            let resource = format!("/{}", file.strip_prefix(root).map_err(err)?.to_string_lossy().replace('\\', "/"));
            if !registry.files.contains_key(&resource) {
                registry.retired.insert(resource, stem.into());
            }
        }
    }
    let _ = (root, folder, registry);
    Ok(())
}

/// A shared atlas may have unsaved frame references in another GUI. Keep its old slots.
#[cfg(test)]
pub(crate) fn shared_atlas(root: &str, gui: &str, atlas: &str) -> Result<bool> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut directories = vec![PathBuf::from(root)];
        let mut count = 0;
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(directory).map_err(err)? {
                let entry = entry.map_err(err)?;
                count += 1;
                if count > 20000 {
                    return Ok(true);
                }
                let label = entry.file_name();
                let label = label.to_string_lossy();
                if label.starts_with('.') || matches!(label.as_ref(), "build" | "target") {
                    continue;
                }
                let kind = entry.file_type().map_err(err)?;
                if kind.is_symlink() {
                    return Ok(true);
                }
                if kind.is_dir() {
                    directories.push(entry.path());
                } else if entry.path().extension().is_some_and(|s| s.eq_ignore_ascii_case("gui"))
                    && !crate::peer_binding::same_project(&entry.path().to_string_lossy(), gui)
                {
                    if entry.metadata().map_err(err)?.len() > 16 * 1024 * 1024 {
                        return Ok(true);
                    }
                    let text = std::fs::read_to_string(entry.path()).map_err(err)?;
                    if text.contains(atlas) {
                        return Ok(true);
                    }
                }
            }
        }
    }
    let _ = (root, gui, atlas);
    Ok(false)
}
