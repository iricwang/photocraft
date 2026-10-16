//! Native creative-source capture and acknowledgements, independent of Undo.
use crate::defold_project_model::*;
use crate::defold_project_store::ProjectStore;
use crate::{EngineError, Result, Session};
use photocraft_doc::{Document, defold::ProjectLink};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Default)]
pub struct ClientState {
    pub(crate) bindings: BTreeMap<u64, ClientBinding>,
    pub(crate) connection: Option<ProjectConnection>,
    pub(crate) acknowledged_openings: std::collections::BTreeSet<u64>,
    pub(crate) asset_management: BTreeMap<u64, AssetManagementSnapshot>,
    pub(crate) shared_reviews: BTreeMap<u64, Arc<crate::defold_project_publish_client::SharedPublicationReview>>,
}
pub(crate) struct ProjectConnection {
    pub root: String,
    pub description: ProjectDescription,
    #[cfg(not(target_arch = "wasm32"))]
    pub store: Arc<dyn ProjectStore>,
}
#[derive(Clone)]
pub(crate) struct ClientBinding {
    pub store: Arc<dyn ProjectStore>,
    pub context: ProjectContext,
    pub client_id: ClientId,
    pub scene: SceneIdentity,
    pub source_revision: Option<String>,
    pub leases: Vec<LeaseProof>,
    pub baseline: SceneBaseline,
    pub last_saved_native_revision: u64,
    pub pending_publication: Option<Arc<(TransactionId, u64, Document)>>,
    pending_source: Option<Arc<SourceAttempt>>,
}
impl ClientBinding {
    pub(crate) fn pending_source_is_some(&self) -> bool {
        self.pending_source.is_some()
    }
}
struct SourceAttempt {
    request: SaveSourceRequest,
    payload: SourcePayload,
}
fn engine(error: impl std::fmt::Display) -> EngineError {
    EngineError::Other(error.to_string())
}

/// Resolve pinned native sources once when opening or explicitly updating a GUI.
/// Consumer pixels are only a display cache; the immutable owner package remains authoritative.
pub(crate) fn hydrate_shared_sources(doc: &mut Document, binding: &ClientBinding) -> Result<()> {
    let Some(design) = &doc.defold else { return Ok(()) };
    let root = design.root.clone();
    let Some(project) = &design.project else { return Ok(()) };
    let mut requested = BTreeMap::new();
    for layer in project.layers.values() {
        if let Some(AppearanceSource::Shared { source }) = &layer.appearance
            && let Some(previous) = requested.insert(source.asset_id.clone(), source.clone())
            && previous != *source
        {
            return Err(engine("A GUI contains inconsistent references to the same shared asset"));
        }
    }
    if requested.len() > 256 {
        return Err(engine("A GUI exceeds the shared source limit"));
    }
    let mut packages = BTreeMap::new();
    let mut resolved = BTreeMap::new();
    let mut total = 0_u64;
    for (asset_id, reference) in requested {
        let key = (reference.package_id.clone(), reference.source_revision.clone());
        if !packages.contains_key(&key) {
            let source = binding
                .store
                .read_source(&ReadSourceRequest { context: binding.context.clone(), package_id: key.0.clone(), source_revision: Some(key.1.clone()) })
                .map_err(engine)?;
            if source.envelope.package_id != key.0 || source.envelope.source_revision != key.1 {
                return Err(engine("Shared source response does not match the pinned package version"));
            }
            source.envelope.verify_payload(&source.payload, &ProjectLimits::default()).map_err(engine)?;
            let package_bytes = source.envelope.dependencies.iter().try_fold(source.envelope.native.length, |sum, dependency| {
                sum.checked_add(dependency.content.length).ok_or_else(|| engine("Shared source size overflow"))
            })?;
            total = total.checked_add(package_bytes).ok_or_else(|| engine("Shared source size overflow"))?;
            if total > ProjectLimits::default().max_total_source_bytes {
                return Err(engine("Shared sources exceed the project transfer limit"));
            }
            let mut owner = photocraft_format::load_from_bytes(&source.payload.native).map_err(engine)?;
            crate::defold_project_dependencies::restore(&mut owner, &source, &root).map_err(engine)?;
            packages.insert(key.clone(), owner);
        }
        let owner = packages.get(&key).ok_or_else(|| engine("Pinned shared source disappeared"))?;
        let project = owner.defold.as_ref().and_then(|design| design.project.as_ref()).ok_or_else(|| engine("Shared source has no GUI identity"))?;
        if project.project_id != binding.context.project_id
            || project.package_id != reference.package_id
            || project.scene_id != reference.source_node.scene_id
            || !project.layers.values().any(|layer| {
                layer.layer_id == reference.source_node.root_layer_id
                    && layer.node_id.as_ref() == Some(&reference.source_node.node_id)
                    && layer.asset_id.as_ref() == Some(&asset_id)
                    && layer.role == LayerRole::GuiNode
                    && !matches!(layer.appearance, Some(AppearanceSource::Shared { .. }))
            })
        {
            return Err(engine("Shared source carrier identity does not match its owner package"));
        }
        resolved.insert(asset_id, (owner.clone(), reference));
    }
    crate::defold_shared_design::hydrate_shared_document(doc, &resolved)
}

/// These commands must receive the host's acknowledgement before their opening can close.
pub fn is_project_commit(command: &str) -> bool {
    matches!(
        command,
        "defold.saveDesignSource"
            | "defold.syncDesign"
            | "defold.publishProjectDesign"
            | "defold.refreshPublication"
            | "defold.cancelPublication"
            | "defold.migrateLegacySource"
            | "defold.requestProjectEditing"
            | "defold.releaseProjectEditing"
    )
}

/// Project writes are serialized while normal painting can continue on a newer revision.
pub fn project_task_running(session: &Session) -> bool {
    let Some(state) = session.active() else { return false };
    let project = state.doc.defold.as_ref().and_then(|design| design.project.as_ref()).map(|project| &project.project_id);
    session.jobs().iter().any(|job| {
        is_project_commit(&job.command)
            && (job.document == Some(state.doc.id)
                || project.is_some_and(|project| {
                    session.documents().iter().any(|other| {
                        job.document == Some(other.doc.id)
                            && other.doc.defold.as_ref().and_then(|design| design.project.as_ref()).is_some_and(|link| &link.project_id == project)
                    })
                }))
    })
}

/// Established GUI identities allow editing during transport. A legacy recovery
/// opening needs its first binding adopted before new layers can acquire IDs.
pub(crate) fn run_captured_project<T: Send + 'static>(
    session: &mut Session,
    label: &str,
    work: impl FnOnce(&crate::jobs::JobCtx) -> Result<T> + Send + 'static,
    apply: impl FnOnce(&mut Session, T) -> Result<serde_json::Value> + Send + 'static,
) -> Result<serde_json::Value> {
    let ready = session.active().is_some_and(|state| {
        state.doc.defold.as_ref().is_some_and(|design| design.project.is_some()) && session.project_client.bindings.contains_key(&state.open_id)
    });
    if ready { crate::jobs::run_captured(session, label, work, apply) } else { crate::jobs::run(session, label, true, work, apply) }
}

fn client_identity() -> ProjectResult<ClientId> {
    static ID: std::sync::OnceLock<std::sync::Mutex<Option<ClientId>>> = std::sync::OnceLock::new();
    let mut identity = ID.get_or_init(|| std::sync::Mutex::new(None)).lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(identity) = identity.as_ref() {
        return Ok(identity.clone());
    }
    let generated: ClientId = new_id()?;
    *identity = Some(generated.clone());
    Ok(generated)
}

#[cfg(not(target_arch = "wasm32"))]
fn stores() -> &'static std::sync::Mutex<BTreeMap<std::path::PathBuf, std::sync::Weak<dyn ProjectStore>>> {
    use std::sync::{Mutex, OnceLock, Weak};
    static STORES: OnceLock<Mutex<BTreeMap<std::path::PathBuf, Weak<dyn ProjectStore>>>> = OnceLock::new();
    STORES.get_or_init(|| Mutex::new(BTreeMap::new()))
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn register_store(root: &std::path::Path, store: &Arc<dyn ProjectStore>) -> ProjectResult<()> {
    let root = std::fs::canonicalize(root).map_err(|e| ProjectError::new(ProjectErrorCode::Io, e.to_string()))?;
    stores().lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(root, Arc::downgrade(store));
    Ok(())
}
#[cfg(not(target_arch = "wasm32"))]
pub fn store_for(root: &str) -> ProjectResult<Arc<dyn ProjectStore>> {
    use std::sync::Weak;
    let path = std::fs::canonicalize(root).map_err(|e| ProjectError::new(ProjectErrorCode::Io, e.to_string()))?;
    let mut stores = stores().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(store) = stores.get(&path).and_then(Weak::upgrade) {
        return Ok(store);
    }
    let ticket_path = crate::defold_project_local::host_ticket_path(&path)?;
    if ticket_path.is_file() {
        let bytes = crate::defold_project_fs::read_bounded(&ticket_path, 64 * 1024)?;
        if let Ok(ticket) = serde_json::from_slice::<crate::defold_project_net::HostTicket>(&bytes) {
            let remote: Arc<dyn ProjectStore> = Arc::new(crate::defold_project_remote::RemoteProjectStore::new(ticket));
            if remote.describe().is_ok() {
                stores.insert(path, Arc::downgrade(&remote));
                return Ok(remote);
            }
        }
    }
    if path.join(".photocraft/local/remote-project.json").is_file() {
        return Err(ProjectError::new(
            ProjectErrorCode::EditorUnavailable,
            "The paired project host is offline; preserve the local draft and reconnect before saving to the project",
        ));
    }
    // Packaged builds include the host alongside the editor. Tests and an engine-only
    // checkout can use the identical in-process store while no host binary exists.
    #[cfg(not(test))]
    if let Some(binary) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(if cfg!(windows) { "photocraft-project-bridge.exe" } else { "photocraft-project-bridge" })))
        .filter(|p| p.is_file())
    {
        let mut process = std::process::Command::new(binary);
        process.arg("--project").arg(&path).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            process.creation_flags(0x0800_0000);
        }
        let mut child = process.spawn().map_err(|e| ProjectError::new(ProjectErrorCode::Io, format!("Could not start the project host: {e}")))?;
        // A cold Windows launch includes certificate setup and antivirus checks.
        // One second can expire before the host publishes its first ticket.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if let Ok(bytes) = crate::defold_project_fs::read_bounded(&ticket_path, 64 * 1024)
                && let Ok(ticket) = serde_json::from_slice(&bytes)
            {
                let remote: Arc<dyn ProjectStore> = Arc::new(crate::defold_project_remote::RemoteProjectStore::new(ticket));
                if remote.describe().is_ok() {
                    stores.insert(path, Arc::downgrade(&remote));
                    return Ok(remote);
                }
            }
            if let Some(status) = child.try_wait().map_err(|e| ProjectError::new(ProjectErrorCode::Io, format!("Could not check the project host: {e}")))? {
                return Err(ProjectError::new(ProjectErrorCode::Io, format!("The project host exited before becoming ready ({status}); reopen the GUI after checking the project host")));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        return Err(ProjectError::new(ProjectErrorCode::Io, "The project host did not become ready within 10 seconds; reopen the GUI to reconnect"));
    }
    let local: Arc<dyn ProjectStore> = crate::defold_project_local::LocalProjectStore::open(&path)?;
    stores.insert(path, Arc::downgrade(&local));
    Ok(local)
}
#[cfg(target_arch = "wasm32")]
pub fn store_for(_: &str) -> ProjectResult<Arc<dyn ProjectStore>> {
    Err(ProjectError::new(ProjectErrorCode::ProtocolUnsupported, "Project hosting requires the desktop application"))
}

fn resource_path(root: &str, gui: &str) -> Result<ResourcePath> {
    let relative = std::path::Path::new(gui).strip_prefix(root).map_err(engine)?;
    ResourcePath::parse(&format!("/{}", relative.to_string_lossy().replace('\\', "/"))).map_err(engine)
}

/// Import identities without saving creative pixels or touching game resources.
pub fn prepare_document(doc: &mut Document) -> Result<()> {
    let Some(design) = doc.defold.as_ref() else { return Ok(()) };
    let store = store_for(&design.root).map_err(engine)?;
    let description = store.describe().map_err(engine)?;
    let scene = store
        .resolve_scene(&ResourceReadRequest { context: description.context.clone(), path: resource_path(&design.root, &design.gui)?, expected_hash: None })
        .map_err(engine)?;
    if let Some(project) = &design.project {
        if project.project_id != description.context.project_id || project.scene_id != scene.scene_id || project.package_id != scene.package_id {
            return Err(engine("The design belongs to another project or GUI; create an independent design copy to rebind it"));
        }
    } else if let Some(design) = &mut doc.defold {
        let original = store
            .read_resource(&ResourceReadRequest { context: description.context.clone(), path: scene.path.clone(), expected_hash: None })
            .map_err(engine)?;
        let expected = if design.disk_hash.contains(':') {
            ContentHash::parse(&design.disk_hash).map_err(engine)?
        } else {
            ContentHash::parse(&format!("blake3:{}", design.disk_hash)).map_err(engine)?
        };
        if !expected.verify(&original.bytes) {
            return Err(engine("TARGET_CHANGED: The GUI changed during import; preserve this draft before reconnecting"));
        }
        let original_source = String::from_utf8(original.bytes).map_err(engine)?;
        design.project = Some(ProjectLink {
            schema_version: SCHEMA_VERSION,
            project_id: description.context.project_id,
            scene_id: scene.scene_id,
            package_id: scene.package_id,
            gui_path: scene.path.to_string(),
            layers: BTreeMap::new(),
            legacy_aliases: BTreeMap::new(),
            imported_gui_source: Some(original_source),
            imported_disk_hash: Some(original.version.content.hash.to_string()),
        });
    }
    crate::defold_design_reconcile::initialize(doc)
}

/// Restore only acknowledged source; local crash recovery remains a separate path.
pub fn load_saved_document(root: &str, gui: &str) -> Result<Option<Document>> {
    Ok(load_saved_document_with_binding(root, gui)?.map(|(document, _)| document))
}

pub(crate) fn load_saved_document_with_binding(root: &str, gui: &str) -> Result<Option<(Document, ClientBinding)>> {
    let store = store_for(root).map_err(engine)?;
    let description = store.describe().map_err(engine)?;
    let scene = store
        .resolve_scene(&ResourceReadRequest { context: description.context.clone(), path: resource_path(root, gui)?, expected_hash: None })
        .map_err(engine)?;
    if scene.source_revision.is_none() {
        return Ok(None);
    }
    let source = store
        .read_source(&ReadSourceRequest {
            context: description.context.clone(),
            package_id: scene.package_id.clone(),
            source_revision: scene.source_revision.clone(),
        })
        .map_err(engine)?;
    let mut doc = photocraft_format::load_from_bytes(&source.payload.native).map_err(engine)?;
    crate::defold_project_dependencies::restore(&mut doc, &source, root).map_err(engine)?;
    let design = doc.defold.as_mut().ok_or_else(|| engine("Project source has no linked GUI document"))?;
    let project = design.project.as_ref().ok_or_else(|| engine("Project source has no stable GUI identities"))?;
    if project.scene_id != scene.scene_id || project.package_id != scene.package_id {
        return Err(engine("Project source identity differs from its scene registration"));
    }
    design.root = root.into();
    design.gui = gui.into();
    prepare_document(&mut doc)?;
    let baseline = source
        .envelope
        .scene_bases
        .into_iter()
        .find(|base| base.scene_id == scene.scene_id)
        .ok_or_else(|| engine("Saved creative source has no scene baseline"))?;
    let binding = ClientBinding {
        store,
        context: description.context,
        client_id: client_identity().map_err(engine)?,
        source_revision: scene.source_revision.clone(),
        scene,
        leases: vec![],
        baseline,
        last_saved_native_revision: 0,
        pending_publication: None,
        pending_source: None,
    };
    hydrate_shared_sources(&mut doc, &binding)?;
    Ok(Some((doc, binding)))
}

/// Freeze the absent source head at import time; another client's later save must conflict.
pub(crate) fn bind_new_document(doc: &mut Document) -> Result<ClientBinding> {
    prepare_document(doc)?;
    let design = doc.defold.as_ref().ok_or_else(|| engine("GUI project attachment is missing"))?;
    let project = design.project.as_ref().ok_or_else(|| engine("Stable GUI project identities are missing"))?;
    let store = store_for(&design.root).map_err(engine)?;
    let description = store.describe().map_err(engine)?;
    let scene = store
        .resolve_scene(&ResourceReadRequest {
            context: description.context.clone(),
            path: ResourcePath::parse(&project.gui_path).map_err(engine)?,
            expected_hash: None,
        })
        .map_err(engine)?;
    let source = project.imported_gui_source.clone().ok_or_else(|| engine("Original GUI source is missing"))?;
    let disk_hash = ContentHash::parse(project.imported_disk_hash.as_deref().ok_or_else(|| engine("Original GUI checksum is missing"))?).map_err(engine)?;
    let baseline = SceneBaseline {
        scene_id: scene.scene_id.clone(),
        path: scene.path.clone(),
        scene_hash: ContentHash::sha256(source.as_bytes()),
        gui_source: source,
        disk_hash,
        published_revision: None,
    };
    Ok(ClientBinding {
        store,
        context: description.context,
        client_id: client_identity().map_err(engine)?,
        source_revision: None,
        scene,
        leases: vec![],
        baseline,
        last_saved_native_revision: 0,
        pending_publication: None,
        pending_source: None,
    })
}

pub(crate) fn bind_document(doc: &mut Document) -> Result<ClientBinding> {
    // A generic native/recovery opening has no acknowledged runtime source head.
    // Never adopt the host's newest head as the base of this potentially older
    // draft. Existing project sources will fail CAS and preserve both versions.
    bind_new_document(doc)
}
pub(crate) fn acquire_or_renew(binding: &mut ClientBinding) -> Result<()> {
    reconcile_pending(binding)?;
    let mut proofs: Vec<_> = binding.leases.iter().filter(|proof| matches!(proof.resource, LeaseResource::Asset(_))).cloned().collect();
    for resource in [LeaseResource::Package(binding.scene.package_id.clone()), LeaseResource::Scene(binding.scene.scene_id.clone())] {
        let proof = if let Some(previous) = binding.leases.iter().find(|proof| proof.resource == resource) {
            match binding.store.renew_lease(&LeaseRenewRequest {
                context: binding.context.clone(),
                client_id: binding.client_id.clone(),
                proof: previous.clone(),
                duration_ms: 300_000,
            }) {
                Ok(grant) => grant.proof,
                Err(error) if error.code == ProjectErrorCode::LeaseLost => {
                    binding
                        .store
                        .acquire_lease(&LeaseRequest { context: binding.context.clone(), client_id: binding.client_id.clone(), resource, duration_ms: 300_000 })
                        .map_err(engine)?
                        .proof
                }
                Err(error) => return Err(engine(error)),
            }
        } else {
            binding
                .store
                .acquire_lease(&LeaseRequest { context: binding.context.clone(), client_id: binding.client_id.clone(), resource, duration_ms: 300_000 })
                .map_err(engine)?
                .proof
        };
        proofs.push(proof);
    }
    binding.leases = proofs;
    Ok(())
}

fn reconcile_pending(binding: &mut ClientBinding) -> Result<Option<SourceSaveReceipt>> {
    let Some(attempt) = binding.pending_source.clone() else { return Ok(None) };
    let request = OperationRequest { context: attempt.request.context.clone(), operation_id: attempt.request.operation_id.clone() };
    let receipt = match binding.store.operation_status(&request).map_err(engine)? {
        Some(receipt) => receipt,
        None => match binding.store.save_source(&attempt.request, &attempt.payload) {
            Ok(receipt) => receipt,
            Err(error) => {
                if let Ok(Some(receipt)) = binding.store.operation_status(&request) {
                    receipt
                } else {
                    if !error.retryable && !matches!(error.code, ProjectErrorCode::Io | ProjectErrorCode::RecoveryRequired) {
                        binding.pending_source = None;
                    }
                    return Err(engine(error));
                }
            }
        },
    };
    if receipt.schema_version != SCHEMA_VERSION
        || receipt.context != attempt.request.context
        || receipt.draft_revision != attempt.request.envelope.draft_revision
        || receipt.captured_native_revision != attempt.request.captured_native_revision
        || receipt.operation_id != attempt.request.operation_id
        || receipt.package_id != attempt.request.package_id
        || receipt.source_revision != attempt.request.envelope.source_revision
        || receipt.content != attempt.request.envelope.native
    {
        return Err(engine("HASH_MISMATCH: Creative source acknowledgement does not match the captured operation"));
    }
    binding.source_revision = Some(receipt.source_revision.clone());
    binding.last_saved_native_revision = receipt.captured_native_revision;
    binding.pending_source = None;
    Ok(Some(receipt))
}

/// Save one already captured document on the worker before exporting runtime assets.
pub(crate) fn save_captured_document(saved_document: &Document, binding: &mut ClientBinding, captured_revision: u64) -> Result<SourceSaveReceipt> {
    let operation_id: OperationId = new_id().map_err(engine)?;
    let source_revision: OperationId = new_id().map_err(engine)?;
    let context = binding.context.clone();
    let client_id = binding.client_id.clone();
    let scene_id = binding.scene.scene_id.clone();
    let package_id = binding.scene.package_id.clone();
    let expected_source_revision = binding.source_revision.clone();
    let baseline = binding.baseline.clone();
    // Creative-source writes need package/scene ownership. Runtime asset leases
    // are retained for handoff, but their expiry must not block saving a draft.
    let leases = binding.leases.iter().filter(|proof| !matches!(proof.resource, LeaseResource::Asset(_))).cloned().collect();
    let captured = crate::defold_project_dependencies::capture(saved_document).map_err(engine)?;
    let native = captured.native;
    let design = saved_document.defold.as_ref().ok_or_else(|| engine("GUI project attachment disappeared"))?;
    let project = design.project.as_ref().ok_or_else(|| engine("GUI project identity disappeared"))?;
    let mut shared_sources = BTreeMap::new();
    for layer in project.layers.values() {
        if let Some(photocraft_doc::defold::AppearanceSource::Shared { source }) = &layer.appearance
            && let Some(previous) = shared_sources.insert(source.package_id.clone(), source.source_revision.clone())
            && previous != source.source_revision
        {
            return Err(engine("A GUI cannot pin different revisions of the same shared source package"));
        }
    }
    let request = SaveSourceRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        context: context.clone(),
        client_id,
        scene_id: Some(scene_id.clone()),
        package_id: package_id.clone(),
        expected_source_revision: expected_source_revision.clone(),
        leases,
        captured_native_revision: captured_revision,
        envelope: SourceEnvelope {
            schema_version: SCHEMA_VERSION,
            package_id,
            source_revision: source_revision.to_string(),
            parent_source_revision: expected_source_revision,
            draft_revision: format!("native-{captured_revision}"),
            scene_bases: vec![baseline],
            identity_map: captured.identity_map,
            native: BlobDescriptor::of_bytes(&native),
            dependencies: captured.dependencies,
            draft: DraftSnapshot {
                gui_sources: BTreeMap::from([(scene_id, design.source.clone())]),
                shared_sources: shared_sources.into_iter().map(|(package_id, source_revision)| SourceRevisionRef { package_id, source_revision }).collect(),
            },
        },
    };
    let payload = SourcePayload { native, dependencies: captured.bodies };
    binding.pending_source = Some(Arc::new(SourceAttempt { request, payload }));
    reconcile_pending(binding)?.ok_or_else(|| engine("Creative source acknowledgement is missing"))
}

pub fn save_source(session: &mut Session) -> Result<serde_json::Value> {
    let st = session.active().ok_or(EngineError::NoDocument)?;
    let open_id = st.open_id;
    let previous = session.project_client.bindings.get(&open_id).cloned();
    let snapshot = Arc::clone(&st.doc);
    let document = st.doc.id;
    let captured_revision = st.revision;
    run_captured_project(
        session,
        "Save GUI creative source",
        move |job| {
            job.check()?;
            let mut saved_document = snapshot.as_ref().clone();
            let mut binding = match previous {
                Some(binding) => binding,
                None => bind_document(&mut saved_document)?,
            };
            // The known host head stays outside Undo. Role changes remain in the captured document.
            if saved_document.defold.as_ref().and_then(|design| design.project.as_ref()).is_none() {
                prepare_document(&mut saved_document)?;
            }
            let receipt = acquire_or_renew(&mut binding).and_then(|()| save_captured_document(&saved_document, &mut binding, captured_revision));
            Ok((receipt, binding, saved_document))
        },
        move |session, (receipt, binding, saved_document)| {
            if let Some(index) = session.documents().iter().position(|st| st.doc.id == document && st.open_id == open_id) {
                let acknowledged_revision = binding.last_saved_native_revision;
                session.project_client.bindings.insert(open_id, binding);
                let active = session.active_index();
                let replace_captured =
                    session.documents().get(index).is_some_and(|state| state.revision == captured_revision && session.job_on(state.doc.id).is_none());
                session.set_active(index);
                if let Some(st) = session.active_mut() {
                    // Acknowledgements apply to the captured revision, never to edits
                    // made while the host was saving it.
                    if receipt.is_ok() && replace_captured {
                        st.doc = Arc::new(saved_document);
                    }
                    if acknowledged_revision != 0 {
                        st.saved_revision = acknowledged_revision;
                    }
                }
                if let Some(active) = active {
                    session.set_active(active);
                }
            }
            let receipt = receipt?;
            Ok(json!({"saved":true,"documentId":document.0,"openId":open_id,"sourceReceipt":receipt,"capturedNativeRevision":captured_revision}))
        },
    )
}

pub fn status(session: &Session) -> serde_json::Value {
    let Some(st) = session.active() else {
        return json!({});
    };
    let binding = session.project_client.bindings.get(&st.open_id);
    json!({"linked":st.doc.defold.is_some(),"sourceKnownSaved":binding.is_some_and(|binding|binding.source_revision.is_some()) || session.project_client.acknowledged_openings.contains(&st.open_id),"sourceRevision":binding.and_then(|binding|binding.source_revision.as_deref()),
        "savedNativeRevision":binding.map(|binding|binding.last_saved_native_revision),"nativeRevision":st.revision,"unsaved":st.is_dirty()})
}

pub fn connection_status(session: &Session) -> serde_json::Value {
    session
        .project_client
        .connection
        .as_ref()
        .map_or_else(|| json!({"connected":false}), |connection| json!({"connected":true,"cacheRoot":connection.root,"project":connection.description}))
}

/// Explicitly request a GUI/package lease before starting collaborative work.
/// Acquiring permission does not save the draft or publish game resources.
pub fn request_editing(session: &mut Session) -> Result<serde_json::Value> {
    let state = session.active().ok_or(EngineError::NoDocument)?;
    let open_id = state.open_id;
    let snapshot = Arc::clone(&state.doc);
    let previous = session.project_client.bindings.get(&open_id).cloned();
    crate::jobs::run(
        session,
        "Request project editing",
        true,
        move |job| {
            job.check()?;
            let mut doc = snapshot.as_ref().clone();
            let mut binding = match previous {
                Some(binding) => binding,
                None => bind_document(&mut doc)?,
            };
            let outcome = acquire_or_renew(&mut binding);
            Ok((binding, outcome))
        },
        move |session, (binding, outcome)| {
            let resources: Vec<_> = binding.leases.iter().map(|proof| proof.resource.clone()).collect();
            if session.documents().iter().any(|state| state.open_id == open_id) {
                session.project_client.bindings.insert(open_id, binding);
            }
            outcome?;
            Ok(json!({"editing":true,"openId":open_id,"resources":resources}))
        },
    )
}

pub fn release_editing(session: &mut Session) -> Result<serde_json::Value> {
    let state = session.active().ok_or(EngineError::NoDocument)?;
    let open_id = state.open_id;
    let Some(mut binding) = session.project_client.bindings.get(&open_id).cloned() else {
        return Ok(json!({"released":true}));
    };
    let shared: Vec<_> = session
        .project_client
        .bindings
        .iter()
        .filter(|(id, other)| **id != open_id && other.context == binding.context)
        .flat_map(|(_, other)| other.leases.iter().map(|proof| proof.resource.clone()))
        .collect();
    crate::jobs::run(
        session,
        "Release project editing",
        true,
        move |_| {
            reconcile_pending(&mut binding)?;
            for proof in &binding.leases {
                if shared.contains(&proof.resource) {
                    continue;
                }
                match binding.store.release_lease(&LeaseReleaseRequest {
                    context: binding.context.clone(),
                    client_id: binding.client_id.clone(),
                    proof: proof.clone(),
                }) {
                    Ok(()) => {}
                    Err(error) if error.code == ProjectErrorCode::LeaseLost => {}
                    Err(error) => return Err(engine(error)),
                }
            }
            binding.leases.clear();
            Ok(binding)
        },
        move |session, binding| {
            if session.documents().iter().any(|state| state.open_id == open_id) {
                session.project_client.bindings.insert(open_id, binding);
            }
            Ok(json!({"released":true,"openId":open_id}))
        },
    )
}

/// Releasing a closed document must not block a frame. The host still fences an
/// in-flight publication, and a failed release expires naturally on its lease.
pub(crate) fn on_close(session: &mut Session, open_id: u64) {
    let Some(binding) = session.project_client.bindings.remove(&open_id) else { return };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let unused: Vec<_> = binding
            .leases
            .iter()
            .filter(|proof| {
                !session
                    .project_client
                    .bindings
                    .values()
                    .any(|other| other.context == binding.context && other.leases.iter().any(|other| other.resource == proof.resource))
            })
            .cloned()
            .collect();
        let _ = std::thread::Builder::new().name("photocraft-release-leases".into()).spawn(move || {
            for proof in unused {
                let _ = binding.store.release_lease(&LeaseReleaseRequest { context: binding.context.clone(), client_id: binding.client_id.clone(), proof });
            }
        });
    }
    #[cfg(target_arch = "wasm32")]
    let _ = binding;
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Session) {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("game.project"), "[display]\nwidth=32\nheight=32\n").unwrap();
        std::fs::write(directory.path().join("main.gui"), "nodes { id:\"icon\" type:TYPE_BOX position {x:16 y:16} size {x:16 y:16} }").unwrap();
        let mut session = Session::new();
        session.execute("defold.openDesign", json!({"path":directory.path().join("main.gui")})).unwrap();
        (directory, session)
    }

    #[test]
    fn requesting_and_releasing_editing_does_not_save_source_or_publish_game_files() {
        let (directory, mut session) = fixture();
        let original = std::fs::read(directory.path().join("main.gui")).unwrap();
        assert_eq!(session.execute("defold.requestProjectEditing", json!({})).unwrap()["editing"], true);
        let opening = session.active().unwrap().open_id;
        let binding = &session.project_client.bindings[&opening];
        assert_eq!(binding.leases.len(), 2);
        assert!(binding.source_revision.is_none());
        assert_eq!(
            binding
                .store
                .read_source(&ReadSourceRequest { context: binding.context.clone(), package_id: binding.scene.package_id.clone(), source_revision: None })
                .unwrap_err()
                .code,
            ProjectErrorCode::ResourceMissing
        );
        let first_epoch = binding.leases[0].lease_epoch;
        assert_eq!(session.execute("defold.releaseProjectEditing", json!({})).unwrap()["released"], true);
        assert!(session.project_client.bindings[&opening].leases.is_empty());
        session.execute("defold.requestProjectEditing", json!({})).unwrap();
        assert_ne!(session.project_client.bindings[&opening].leases[0].lease_epoch, first_epoch);
        assert_eq!(std::fs::read(directory.path().join("main.gui")).unwrap(), original);
        assert!(!directory.path().join("photocraft_generated").exists());
    }

    #[test]
    fn non_journaled_project_commands_cannot_race_a_document_job() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (_directory, mut session) = fixture();
        let gate = Arc::new(AtomicBool::new(false));
        let worker_gate = gate.clone();
        let started = session
            .start_job(
                "defold.saveDesignSource",
                json!({}),
                "Gated source save",
                true,
                move |_| {
                    for _ in 0..500 {
                        if worker_gate.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(engine("Test source gate timed out"))
                },
                |_, ()| Ok(json!({"saved":true})),
            )
            .unwrap();
        let crate::jobs::Started::Job(job) = started else { panic!("Source save must run on a worker") };
        for command in [
            "defold.saveDesignSource",
            "defold.syncDesign",
            "defold.publishProjectDesign",
            "defold.reviewProjectChanges",
            "defold.applyProjectReview",
            "defold.refreshPublication",
            "defold.cancelPublication",
            "defold.requestProjectEditing",
            "defold.releaseProjectEditing",
            "defold.migrateLegacySource",
        ] {
            let failure = session.start(command, json!({})).unwrap_err().to_string();
            assert!(failure.contains("still running"), "{command}: {failure}");
        }
        gate.store(true, Ordering::Release);
        assert_eq!(session.wait_job(job).unwrap()["saved"], true);
        assert!(session.active().unwrap().doc.defold.is_some());
    }

    #[test]
    fn switching_gui_tabs_cannot_start_a_second_project_writer() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (directory, mut session) = fixture();
        std::fs::write(directory.path().join("other.gui"), "nodes { id:\"other\" type:TYPE_BOX size{x:16 y:16} }").unwrap();
        session.execute("defold.openDesign", json!({"path":directory.path().join("other.gui")})).unwrap();
        session.set_active(0);
        let gate = Arc::new(AtomicBool::new(false));
        let worker_gate = gate.clone();
        let started = session
            .start_job(
                "defold.publishProjectDesign",
                json!({}),
                "Gated project publication",
                true,
                move |_| {
                    for _ in 0..500 {
                        if worker_gate.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(engine("Test project gate timed out"))
                },
                |_, ()| Ok(json!({"synced":true})),
            )
            .unwrap();
        let crate::jobs::Started::Job(job) = started else { panic!("Publication must run on a worker") };
        session.set_active(1);
        for command in ["defold.saveDesignSource", "defold.syncDesign", "defold.releaseProjectEditing"] {
            assert!(session.start(command, json!({})).unwrap_err().to_string().contains("still running"), "{command}");
        }
        session.execute("layer.new.layer", json!({"name":"Painting may continue"})).unwrap();
        gate.store(true, Ordering::Release);
        assert_eq!(session.wait_job(job).unwrap()["synced"], true);
    }

    #[test]
    fn source_head_survives_undo_and_first_save_keeps_original_target_baseline() {
        let (directory, mut session) = fixture();
        let original = std::fs::read(directory.path().join("main.gui")).unwrap();
        session.execute("layer.new.layer", json!({"name":"First draft"})).unwrap();
        let first = session.execute("defold.saveDesignSource", json!({})).unwrap();
        session.execute("layer.new.layer", json!({"name":"Second draft"})).unwrap();
        let second = session.execute("defold.saveDesignSource", json!({})).unwrap();
        session.execute("edit.undo", json!({})).unwrap();
        let third = session.execute("defold.saveDesignSource", json!({})).unwrap();
        assert_ne!(first["sourceReceipt"]["sourceRevision"], second["sourceReceipt"]["sourceRevision"]);
        assert_ne!(third["sourceReceipt"]["sourceRevision"], second["sourceReceipt"]["sourceRevision"]);
        let binding = &session.project_client.bindings[&session.active().unwrap().open_id];
        let source = binding
            .store
            .read_source(&ReadSourceRequest { context: binding.context.clone(), package_id: binding.scene.package_id.clone(), source_revision: None })
            .unwrap();
        assert_eq!(source.envelope.parent_source_revision.as_deref(), second["sourceReceipt"]["sourceRevision"].as_str());
        assert_eq!(source.envelope.scene_bases[0].gui_source.as_bytes(), original);
        assert_eq!(std::fs::read(directory.path().join("main.gui")).unwrap(), original);
    }

    #[test]
    fn lost_acknowledgement_recovers_the_original_source_operation() {
        let (_directory, session) = fixture();
        let mut doc = session.active().unwrap().doc.as_ref().clone();
        let mut binding = bind_document(&mut doc).unwrap();
        let fake = Arc::new(crate::defold_project_store::FakeStore::new(binding.store.describe().unwrap()));
        binding.store = fake.clone();
        binding.leases.clear();
        acquire_or_renew(&mut binding).unwrap();
        fake.inject_failure(FailurePoint::SourceAfterCommit);
        let receipt = save_captured_document(&doc, &mut binding, 12).unwrap();
        assert_eq!(binding.source_revision.as_deref(), Some(receipt.source_revision.as_str()));
        assert!(binding.pending_source.is_none());
        let source = fake.read_source(&ReadSourceRequest { context: binding.context, package_id: binding.scene.package_id, source_revision: None }).unwrap();
        assert_eq!(source.envelope.source_revision, receipt.source_revision);
    }

    #[test]
    fn saving_a_stale_draft_preserves_its_imported_baseline_instead_of_adopting_external_changes() {
        let (directory, mut session) = fixture();
        let original = std::fs::read_to_string(directory.path().join("main.gui")).unwrap();
        std::fs::write(directory.path().join("main.gui"), "nodes { id:\"changed_in_defold\" type:TYPE_BOX }").unwrap();
        session.execute("defold.saveDesignSource", json!({})).unwrap();
        let binding = &session.project_client.bindings[&session.active().unwrap().open_id];
        assert_eq!(binding.baseline.gui_source, original);
        assert!(session.execute("defold.syncDesign", json!({})).unwrap_err().to_string().contains("TARGET_CHANGED"));
        assert!(std::fs::read_to_string(directory.path().join("main.gui")).unwrap().contains("changed_in_defold"));
    }

    #[test]
    fn opening_freezes_source_cas_before_another_client_saves() {
        let (directory, mut first) = fixture();
        let gui = directory.path().join("main.gui");
        let mut second = Session::new();
        second.execute("defold.openDesign", json!({"path":gui})).unwrap();
        first.execute("layer.new.layer", json!({"name":"First client"})).unwrap();
        first.execute("defold.saveDesignSource", json!({})).unwrap();
        second.execute("layer.new.layer", json!({"name":"Second client draft"})).unwrap();
        let local = second.active().unwrap().doc.clone();
        assert!(second.execute("defold.saveDesignSource", json!({})).unwrap_err().to_string().contains("SourceConflict"));
        assert_eq!(second.active().unwrap().doc.layers, local.layers);
        second.close(0);
        let mut reopened = Session::new();
        reopened.execute("defold.openDesign", json!({"path":gui})).unwrap();
        assert!(status(&reopened)["sourceKnownSaved"] == true);
        first.execute("layer.new.layer", json!({"name":"Later host edit"})).unwrap();
        first.execute("defold.saveDesignSource", json!({})).unwrap();
        reopened.execute("layer.new.layer", json!({"name":"Stale formal opening"})).unwrap();
        assert!(reopened.execute("defold.saveDesignSource", json!({})).unwrap_err().to_string().contains("SourceConflict"));
    }
}
