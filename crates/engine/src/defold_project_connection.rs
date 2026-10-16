//! Paired project access with an on-demand render cache, never a whole-project mirror.
use crate::{EngineError, Result, Session};
#[cfg(not(target_arch = "wasm32"))]
use crate::{defold_project_client as client, defold_project_model::*};
use serde_json::Value;
#[cfg(not(target_arch = "wasm32"))]
use serde_json::json;

fn engine(error: impl std::fmt::Display) -> EngineError {
    EngineError::Other(error.to_string())
}

/// Reveal an explicitly selected asset in the authoritative Defold editor.
#[cfg(not(target_arch = "wasm32"))]
pub fn reveal(session: &mut Session, params: &Value) -> Result<Value> {
    crate::file_cmds::native(session).map_err(engine)?;
    let path = ResourcePath::parse(params.get("path").and_then(Value::as_str).ok_or_else(|| engine("Choose a project resource"))?).map_err(engine)?;
    let opening = session.active().ok_or(EngineError::NoDocument)?.open_id;
    let binding = session.project_client.bindings.get(&opening).cloned().ok_or_else(|| engine("Open a linked project GUI first"))?;
    crate::jobs::run(
        session,
        "Reveal resource in Defold",
        false,
        move |job| {
            job.check()?;
            binding.store.reveal_resource(&ResourceReadRequest {context:binding.context,path:path.clone(),expected_hash:None}).map_err(engine)?;
            Ok(path)
        },
        move |_, path| Ok(json!({"opened":true,"path":path,"openId":opening})),
    )
}

#[cfg(target_arch = "wasm32")]
pub fn reveal(_: &mut Session, _: &Value) -> Result<Value> {
    Err(engine("Project resource navigation requires the desktop application"))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn connect(session: &mut Session, params: &Value) -> Result<Value> {
    let path = params
        .get("pairingFile")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| engine("Choose the project host's pairing file"))?
        .to_string();
    crate::jobs::run(
        session,
        "Connect project host",
        false,
        move |job| {
            use crate::{
                defold_project_fs as disk, defold_project_net::HostTicket, defold_project_remote::RemoteProjectStore, defold_project_store::ProjectStore,
            };
            use std::{path::PathBuf, sync::Arc};
            let bytes = disk::read_bounded(std::path::Path::new(&path), 64 * 1024).map_err(engine)?;
            let ticket: HostTicket = serde_json::from_slice(&bytes).map_err(|_| engine("Invalid project pairing file"))?;
            let store: Arc<dyn ProjectStore> = Arc::new(RemoteProjectStore::new(ticket.clone()));
            let description = store.describe().map_err(engine)?;
            check_schema(description.schema_version).map_err(engine)?;
            for required in ["sourceStoreV1", "sourceCASV1", "leaseV1", "sceneRegistryV1"] {
                if !description.capabilities.contains(required) {
                    return Err(engine("This host does not support project creative sources"));
                }
            }
            job.check()?;
            let base = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from))
                .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
                .ok_or_else(|| engine("A private local cache directory is required"))?;
            let root = base
                .join("PhotoCraft/remote-projects")
                .join(description.context.project_id.as_str())
                .join(description.context.checkout_id.as_str())
                .join(description.context.workspace_id.as_str());
            std::fs::create_dir_all(&root).map_err(engine)?;
            // Only this marker and private settings identify the remote authority. It can
            // never silently become a writable local project when its host is unavailable.
            disk::write_json(&disk::scoped_path(&root, ".photocraft/local/remote-project.json", true).map_err(engine)?, &description.context)
                .map_err(engine)?;
            let ticket_path = crate::defold_project_local::host_ticket_path(&root).map_err(engine)?;
            if let Some(parent) = ticket_path.parent() {
                std::fs::create_dir_all(parent).map_err(engine)?;
            }
            disk::write_json(&ticket_path, &ticket).map_err(engine)?;
            client::register_store(&root, &store).map_err(engine)?;
            Ok(client::ProjectConnection { root: root.to_string_lossy().into_owned(), description, store })
        },
        |session, connection| {
            let context = &connection.description.context;
            let openings: std::collections::BTreeSet<_> = session
                .documents()
                .iter()
                .filter(|state| session.project_client.bindings.get(&state.open_id).is_some_and(|binding| &binding.context == context))
                .map(|state| state.doc.id.0)
                .collect();
            // An outstanding callback owns a captured binding. Let it finish before
            // switching transports, otherwise its acknowledgement could restore the
            // obsolete endpoint over the successful reconnect.
            if session.jobs().iter().any(|job| client::is_project_commit(&job.command) && job.document.is_some_and(|id| openings.contains(&id.0))) {
                return Err(engine("Wait for this project's pending save or publication to finish, then reconnect"));
            }
            for binding in session.project_client.bindings.values_mut().filter(|binding| &binding.context == context) {
                // Change transport only. Source heads, leases, drafts, selection and
                // native history must not be adopted or reset by a connection action.
                binding.store = std::sync::Arc::clone(&connection.store);
            }
            session.project_client.connection = Some(connection);
            Ok(client::connection_status(session))
        },
    )
}

#[cfg(target_arch = "wasm32")]
pub fn connect(_: &mut Session, _: &Value) -> Result<Value> {
    Err(engine("Project hosting requires the desktop application"))
}

#[cfg(not(target_arch = "wasm32"))]
fn cache_resource(
    store: &dyn crate::defold_project_store::ProjectStore,
    context: &ProjectContext,
    root: &std::path::Path,
    path: &ResourcePath,
    total: &mut u64,
    limit: u64,
) -> Result<Vec<ResourcePath>> {
    use crate::defold_project_fs as disk;
    let resource = store.read_resource(&ResourceReadRequest { context: context.clone(), path: path.clone(), expected_hash: None }).map_err(engine)?;
    resource.version.content.verify(&resource.bytes).map_err(engine)?;
    *total = total.checked_add(resource.bytes.len() as u64).ok_or_else(|| engine("GUI dependency size overflow"))?;
    if *total > limit {
        return Err(engine("GUI dependencies exceed the project transfer limit"));
    }
    let destination = disk::scoped_path(root, path.as_str(), true).map_err(engine)?;
    disk::write_atomic(&destination, &resource.bytes).map_err(engine)?;
    dependencies(path, &resource.bytes)
}

/// Only fields used to resolve render dependencies are followed. Scripts, builds,
/// repository internals and unrelated project resources are never transferred.
#[cfg(not(target_arch = "wasm32"))]
fn dependencies(path: &ResourcePath, bytes: &[u8]) -> Result<Vec<ResourcePath>> {
    if ![".gui", ".atlas", ".font", ".tilesource", ".material"].iter().any(|extension| path.as_str().ends_with(extension)) {
        return Ok(vec![]);
    }
    let text = std::str::from_utf8(bytes).map_err(engine)?;
    let proto = photocraft_defold_format::TextProto::parse(text.into()).map_err(engine)?;
    let mut pending: Vec<_> = proto.fields.iter().map(|field| (field, 0)).collect();
    let mut resources = std::collections::BTreeSet::new();
    while let Some((field, depth)) = pending.pop() {
        if depth > 128 || resources.len() > 4096 {
            return Err(engine("GUI dependency tree exceeds limits"));
        }
        if field.message {
            pending.extend(field.children.iter().map(|child| (child, depth + 1)));
        } else if ["texture", "image", "font", "material", "template"].contains(&field.name.as_str()) {
            let value = proto.text(std::slice::from_ref(field), &field.name);
            if value.starts_with('/') && !value.starts_with("/builtins/") {
                resources.insert(ResourcePath::parse(&value).map_err(engine)?);
            }
        }
    }
    Ok(resources.into_iter().collect())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn open_gui(session: &mut Session, params: &Value) -> Result<Value> {
    let path = ResourcePath::parse(params.get("path").and_then(Value::as_str).ok_or_else(|| engine("Choose a GUI resource path"))?).map_err(engine)?;
    if !path.as_str().ends_with(".gui") {
        return Err(engine("Choose a Defold GUI resource"));
    }
    let connection = session.project_client.connection.as_ref().ok_or_else(|| engine("Connect a project host first"))?;
    let root = connection.root.clone();
    let context = connection.description.context.clone();
    let store = std::sync::Arc::clone(&connection.store);
    let limit = connection.description.limits.max_total_source_bytes;
    crate::jobs::run(
        session,
        "Open project GUI",
        false,
        move |job| {
            let mut pending = vec![path.clone(), ResourcePath::parse("/game.project").map_err(engine)?];
            let mut visited = std::collections::BTreeSet::new();
            let mut total = 0_u64;
            while let Some(resource) = pending.pop() {
                job.check()?;
                if visited.len() > 4096 {
                    return Err(engine("GUI dependency tree exceeds limits"));
                }
                if !visited.insert(resource.clone()) {
                    continue;
                }
                pending.extend(cache_resource(store.as_ref(), &context, std::path::Path::new(&root), &resource, &mut total, limit)?);
            }
            let file = std::path::Path::new(&root).join(path.as_str().trim_start_matches('/'));
            let mut temporary = Session::new();
            let result = temporary.execute("defold.openDesign", json!({"path":file.to_string_lossy()}))?;
            let document = temporary.active().ok_or(EngineError::NoDocument)?.doc.as_ref().clone();
            let source_known = client::status(&temporary)["sourceKnownSaved"] == true;
            let opening = temporary.active().ok_or(EngineError::NoDocument)?.open_id;
            let binding = temporary.project_client.bindings.remove(&opening);
            Ok((document, file.to_string_lossy().into_owned(), result, source_known, binding))
        },
        |session, (doc, path, result, source_known, binding)| {
            if let Some(index) = session.documents().iter().position(|state| state.doc.defold.as_ref().is_some_and(|design| design.gui == path)) {
                session.set_active(index);
                return Ok(json!({"document":index,"restored":true}));
            }
            let index = session.add_document(doc, Some(path));
            if let Some(state) = session.active() {
                let open_id = state.open_id;
                if source_known {
                    session.project_client.acknowledged_openings.insert(open_id);
                }
                if let Some(binding) = binding {
                    session.project_client.bindings.insert(open_id, binding);
                }
            }
            Ok(json!({"document":index,"warnings":result["warnings"]}))
        },
    )
}
#[cfg(target_arch = "wasm32")]
pub fn open_gui(_: &mut Session, _: &Value) -> Result<Value> {
    Err(engine("Project hosting requires the desktop application"))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    #[test]
    fn render_cache_follows_only_explicit_render_dependencies() {
        let path = ResourcePath::parse("/main.gui").unwrap();
        let resources=dependencies(&path,br#"script: "/main.script" textures { name: "ui" texture: "/ui.atlas" } fonts {name:"font" font:"/default.font"} nodes { id: "/not/a/resource" texture: "ui/icon" } material: "/builtins/materials/gui.material""#).unwrap();
        assert_eq!(resources, vec![ResourcePath::parse("/default.font").unwrap(), ResourcePath::parse("/ui.atlas").unwrap()]);
        assert!(dependencies(&path, br#"textures {texture: "/../secret"}"#).is_err());
    }
}
