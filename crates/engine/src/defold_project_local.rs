//! Project-owned immutable creative revisions and fenced single-writer leases.
use super::defold_project_fs as disk;
use super::defold_project_model::*;
use super::defold_project_store::ProjectStore;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

const INDEX_LIMIT: u64 = 64 * 1024 * 1024;
const MAX_LEASE_MS: u64 = 5 * 60 * 1000;

#[path = "defold_project_asset_store.rs"]
mod asset_management;
#[path = "defold_project_shared_store.rs"]
mod shared_artwork;

fn fail(code: ProjectErrorCode, message: impl Into<String>) -> ProjectError {
    ProjectError::new(code, message)
}
fn io(error: impl std::fmt::Display) -> ProjectError {
    fail(ProjectErrorCode::Io, error.to_string())
}
fn now() -> ProjectResult<u64> {
    u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).map_err(io)?.as_millis()).map_err(io)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectRecord {
    schema_version: u32,
    project_id: ProjectId,
    display_name: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckoutRecord {
    context: ProjectContext,
    root_locator: String,
    workspace_stamp: String,
}
pub type SceneRecord = SceneIdentity;
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceOperation {
    request_hash: ContentHash,
    receipt: SourceSaveReceipt,
}
fn schema_version() -> u32 {
    SCHEMA_VERSION
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    #[serde(default = "schema_version")]
    schema_version: u32,
    #[serde(default)]
    heads: BTreeMap<PackageId, String>,
    #[serde(default)]
    scenes: BTreeMap<SceneId, SceneRecord>,
    #[serde(default)]
    operations: BTreeMap<OperationId, SourceOperation>,
}
impl Default for Index {
    fn default() -> Self {
        Self { schema_version: SCHEMA_VERSION, heads: BTreeMap::new(), scenes: BTreeMap::new(), operations: BTreeMap::new() }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LeaseBook {
    #[serde(default = "schema_version")]
    schema_version: u32,
    next_epoch: u64,
    grants: BTreeMap<String, LeaseGrant>,
}
impl Default for LeaseBook {
    fn default() -> Self {
        Self { schema_version: SCHEMA_VERSION, next_epoch: 0, grants: BTreeMap::new() }
    }
}
struct State {
    checkout: CheckoutRecord,
    index: Index,
    leases: LeaseBook,
}

/// A process holds the OS lock for this authority's lifetime. Repeated opens in
/// the same process share it; another process connects through the host ticket.
pub struct LocalProjectStore {
    root: PathBuf,
    project: ProjectRecord,
    limits: ProjectLimits,
    state: Mutex<State>,
    migration_gate: Mutex<()>,
    _authority_lock: File,
}

/// Credentials are per-user, not part of a shared game project or its mirror.
pub fn host_ticket_path(root: &Path) -> ProjectResult<PathBuf> {
    let canonical = fs::canonicalize(root).map_err(io)?;
    let base = if let Some(path) = std::env::var_os("LOCALAPPDATA") {
        PathBuf::from(path).join("PhotoCraft")
    } else if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        PathBuf::from(path).join("photocraft")
    } else if let Some(path) = std::env::var_os("HOME") {
        PathBuf::from(path).join(".local/state/photocraft")
    } else {
        return Err(fail(ProjectErrorCode::Io, "A private user settings directory is required for project pairing"));
    };
    let key = ContentHash::sha256(canonical.to_string_lossy().as_bytes()).to_string().replace(':', "-");
    Ok(base.join("project-hosts").join(key).join("ticket.json"))
}

impl LocalProjectStore {
    pub fn open(root: &Path) -> ProjectResult<Arc<Self>> {
        static OPEN: OnceLock<Mutex<BTreeMap<PathBuf, Weak<LocalProjectStore>>>> = OnceLock::new();
        let root = fs::canonicalize(root).map_err(io)?;
        let registry = OPEN.get_or_init(|| Mutex::new(BTreeMap::new()));
        let mut registry = registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(store) = registry.get(&root).and_then(Weak::upgrade) {
            return Ok(store);
        }
        let game = disk::scoped_path(&root, "game.project", false)?;
        if !game.is_file() {
            return Err(fail(ProjectErrorCode::ResourceMissing, "Choose a project containing game.project"));
        }
        let guard = disk::lock_project(&root)?;
        let project_path = disk::scoped_path(&root, ".photocraft/project.json", true)?;
        let project: ProjectRecord = if project_path.is_file() {
            let record: ProjectRecord = disk::read_json(&project_path, 64 * 1024)?;
            check_schema(record.schema_version)?;
            record
        } else {
            let record = ProjectRecord {
                schema_version: SCHEMA_VERSION,
                project_id: new_id()?,
                display_name: root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Defold project".into()),
            };
            disk::write_json(&project_path, &record)?;
            record
        };
        let checkout_path = disk::scoped_path(&root, ".photocraft/local/checkout.json", true)?;
        let locator = root.to_string_lossy().into_owned();
        let stamp = workspace_stamp(&root)?;
        let mut checkout: CheckoutRecord = if checkout_path.is_file() {
            disk::read_json(&checkout_path, 64 * 1024)?
        } else {
            CheckoutRecord {
                context: ProjectContext { project_id: project.project_id.clone(), checkout_id: new_id()?, workspace_id: new_id()?, workspace_epoch: 1 },
                root_locator: locator.clone(),
                workspace_stamp: stamp.clone(),
            }
        };
        if checkout.context.project_id != project.project_id || checkout.root_locator != locator {
            checkout.context = ProjectContext { project_id: project.project_id.clone(), checkout_id: new_id()?, workspace_id: new_id()?, workspace_epoch: 1 };
            checkout.root_locator = locator;
        }
        if checkout.workspace_stamp != stamp {
            checkout.context.workspace_id = new_id()?;
            checkout.context.workspace_epoch =
                checkout.context.workspace_epoch.checked_add(1).ok_or_else(|| fail(ProjectErrorCode::WorkspaceChanged, "Workspace epoch exhausted"))?;
            checkout.workspace_stamp = stamp;
        }
        disk::write_json(&checkout_path, &checkout)?;
        let index_path = disk::scoped_path(&root, ".photocraft/sources/index.json", true)?;
        let index: Index = if index_path.is_file() { disk::read_json(&index_path, INDEX_LIMIT)? } else { Index::default() };
        check_schema(index.schema_version)?;
        let leases_path = disk::scoped_path(&root, ".photocraft/local/leases.json", true)?;
        let mut leases: LeaseBook = if leases_path.is_file() { disk::read_json(&leases_path, INDEX_LIMIT)? } else { LeaseBook::default() };
        check_schema(leases.schema_version)?;
        leases.next_epoch = leases.next_epoch.checked_add(1).ok_or_else(|| fail(ProjectErrorCode::LeaseLost, "Lease epoch exhausted"))?;
        leases.grants.clear();
        disk::write_json(&leases_path, &leases)?;
        let ignore = disk::scoped_path(&root, ".photocraft/local/.gitignore", true)?;
        if !ignore.exists() {
            disk::write_atomic(&ignore, b"*\n")?;
        }
        let transfer_budget = (crate::defold_project_wire::TRANSFER_LIMIT - crate::defold_project_wire::METADATA_LIMIT - 16) as u64;
        let limits = ProjectLimits { max_native_bytes: transfer_budget, max_total_source_bytes: transfer_budget, ..Default::default() };
        let store = Arc::new(Self {
            root: root.clone(),
            project,
            limits,
            state: Mutex::new(State { checkout, index, leases }),
            migration_gate: Mutex::new(()),
            _authority_lock: guard,
        });
        registry.insert(root, Arc::downgrade(&store));
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve/register by full resource location, never by its basename.
    pub fn resolve_scene_path(&self, path: &ResourcePath) -> ProjectResult<SceneRecord> {
        self.resolve_scene(&ResourceReadRequest { context: self.describe()?.context, path: path.clone(), expected_hash: None })
    }

    fn persist_index(&self, index: &Index) -> ProjectResult<()> {
        disk::write_json(&disk::scoped_path(&self.root, ".photocraft/sources/index.json", true)?, index)
    }
    fn persist_leases(&self, book: &LeaseBook) -> ProjectResult<()> {
        disk::write_json(&disk::scoped_path(&self.root, ".photocraft/local/leases.json", true)?, book)
    }
    fn refresh_workspace(&self, state: &mut State) -> ProjectResult<()> {
        let project: ProjectRecord = disk::read_json(&disk::scoped_path(&self.root, ".photocraft/project.json", false)?, 64 * 1024)?;
        if project.project_id != self.project.project_id {
            return Err(fail(ProjectErrorCode::ProjectMismatch, "The project at this location has changed"));
        }
        let stamp = workspace_stamp(&self.root)?;
        if stamp != state.checkout.workspace_stamp {
            state.checkout.context.workspace_epoch =
                state.checkout.context.workspace_epoch.checked_add(1).ok_or_else(|| fail(ProjectErrorCode::WorkspaceChanged, "Workspace epoch exhausted"))?;
            state.checkout.context.workspace_id = new_id()?;
            state.checkout.workspace_stamp = stamp;
            state.leases.grants.clear();
            let path = disk::scoped_path(&self.root, ".photocraft/sources/index.json", false)?;
            state.index = if path.is_file() { disk::read_json(&path, INDEX_LIMIT)? } else { Index::default() };
            check_schema(state.index.schema_version)?;
            disk::write_json(&disk::scoped_path(&self.root, ".photocraft/local/checkout.json", true)?, &state.checkout)?;
            self.persist_leases(&state.leases)?;
        }
        Ok(())
    }
    fn check_context(&self, state: &mut State, context: &ProjectContext) -> ProjectResult<()> {
        self.refresh_workspace(state)?;
        if context.project_id != state.checkout.context.project_id || context.checkout_id != state.checkout.context.checkout_id {
            return Err(fail(ProjectErrorCode::ProjectMismatch, "This request belongs to another project working copy"));
        }
        if *context != state.checkout.context {
            return Err(fail(ProjectErrorCode::WorkspaceChanged, "Workspace changed; preserve this draft before reconnecting"));
        }
        Ok(())
    }
    fn check_leases(&self, state: &State, client: &ClientId, proofs: &[LeaseProof]) -> ProjectResult<()> {
        let time = now()?;
        for proof in proofs {
            let key = serde_json::to_string(&proof.resource).map_err(io)?;
            if !state.leases.grants.get(&key).is_some_and(|grant| grant.client_id == *client && grant.proof == *proof && grant.expires_at_ms > time) {
                return Err(fail(ProjectErrorCode::LeaseLost, "Editing permission expired or was transferred; the local draft is preserved"));
            }
        }
        Ok(())
    }
    fn revision_path(&self, package: &PackageId, revision: &str, file: &str, create: bool) -> ProjectResult<PathBuf> {
        if revision.is_empty()
            || revision.len() > 128
            || !revision.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
            || matches!(revision, "." | "..")
        {
            return Err(fail(ProjectErrorCode::PathInvalid, "Invalid source revision"));
        }
        disk::scoped_path(&self.root, &format!(".photocraft/sources/{}/revisions/{revision}/{file}", package.as_str()), create)
    }
    fn package(&self, package: &PackageId, revision: &str) -> ProjectResult<SourcePackage> {
        let envelope: SourceEnvelope = disk::read_json(&self.revision_path(package, revision, "revision.json", false)?, self.limits.max_control_bytes)?;
        if envelope.package_id != *package || envelope.source_revision != revision {
            return Err(fail(ProjectErrorCode::HashMismatch, "Source revision identity does not match its location"));
        }
        envelope.validate(&self.limits)?;
        let native = disk::read_bounded(&self.revision_path(package, revision, "document.pcraft", false)?, self.limits.max_native_bytes)?;
        let mut dependencies = BTreeMap::new();
        for dependency in &envelope.dependencies {
            if !dependencies.contains_key(&dependency.content.hash) {
                let name = dependency.content.hash.as_str().replace(':', "-");
                let bytes =
                    disk::read_bounded(&self.revision_path(package, revision, &format!("dependencies/{name}"), false)?, self.limits.max_dependency_bytes)?;
                dependencies.insert(dependency.content.hash.clone(), bytes);
            }
        }
        let payload = SourcePayload { native, dependencies };
        envelope.verify_payload(&payload, &self.limits)?;
        Ok(SourcePackage { envelope, payload })
    }
    fn validate_publish_state(&self, state: &mut State, plan: &PublishPlan) -> ProjectResult<()> {
        self.check_context(state, &plan.context)?;
        self.check_leases(state, &plan.client_id, &plan.leases)?;
        for source in &plan.source_revisions {
            if state.index.heads.get(&source.package_id) != Some(&source.source_revision) {
                return Err(fail(ProjectErrorCode::SourceConflict, "Saved creative source changed before publication"));
            }
            let _ = self.package(&source.package_id, &source.source_revision)?;
        }
        for target in &plan.targets {
            let scene = state.index.scenes.get(&target.scene_id).ok_or_else(|| fail(ProjectErrorCode::ResourceMissing, "Publish scene is not registered"))?;
            if scene.path != target.path {
                return Err(fail(ProjectErrorCode::ProjectMismatch, "Publication targets another GUI resource"));
            }
            if !plan.source_revisions.iter().any(|source| source.package_id == scene.package_id) {
                return Err(fail(ProjectErrorCode::SourceConflict, "Each GUI target requires an acknowledged creative package revision"));
            }
        }
        shared_artwork::validate_publication(self, state, plan)?;
        Ok(())
    }
}

impl ProjectStore for LocalProjectStore {
    fn describe(&self) -> ProjectResult<ProjectDescription> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.refresh_workspace(&mut state)?;
        Ok(ProjectDescription {
            schema_version: SCHEMA_VERSION,
            context: state.checkout.context.clone(),
            display_name: self.project.display_name.clone(),
            capabilities: BTreeSet::from([
                "sourceStoreV1".into(),
                "sourceCASV1".into(),
                "leaseV1".into(),
                "sceneRegistryV1".into(),
                "assetManagementV1".into(),
                "legacyMigrationV1".into(),
                "publishedBaselineV1".into(),
                "resourceRevealV1".into(),
                "sharedArtworkV1".into(),
            ]),
            editor: crate::defold_project_editor::describe(&self.root).ok(),
            limits: self.limits.clone(),
        })
    }
    fn resolve_scene(&self, request: &ResourceReadRequest) -> ProjectResult<SceneIdentity> {
        if !request.path.as_str().ends_with(".gui") {
            return Err(fail(ProjectErrorCode::InvalidRequest, "Choose a GUI resource"));
        }
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_context(&mut state, &request.context)?;
        let resource = disk::scoped_path(&self.root, request.path.as_str(), false)?;
        let bytes = disk::read_bounded(&resource, self.limits.max_gui_source_bytes)?;
        if request.expected_hash.as_ref().is_some_and(|hash| !hash.verify(&bytes)) {
            return Err(fail(ProjectErrorCode::TargetChanged, "GUI changed before scene registration"));
        }
        if let Some(scene) = state.index.scenes.values().find(|scene| scene.path.case_key() == request.path.case_key()) {
            if scene.path != request.path {
                return Err(fail(ProjectErrorCode::NameCollision, "GUI names differ only in case; resolve their location before editing"));
            }
            return Ok(scene.clone());
        }
        let scene = SceneIdentity { scene_id: new_id()?, path: request.path.clone(), package_id: new_id()?, source_revision: None };
        let mut index = state.index.clone();
        index.scenes.insert(scene.scene_id.clone(), scene.clone());
        self.persist_index(&index)?;
        state.index = index;
        Ok(scene)
    }
    fn snapshot(&self, request: &SceneRequest) -> ProjectResult<SceneSnapshot> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_context(&mut state, &request.context)?;
        let scene = state.index.scenes.get(&request.scene_id).ok_or_else(|| fail(ProjectErrorCode::ResourceMissing, "Unknown GUI scene"))?;
        if scene.path != request.path {
            return Err(fail(ProjectErrorCode::TargetChanged, "GUI resource location changed"));
        }
        match crate::defold_project_editor::snapshot(&self.root, request) {
            Ok(snapshot) => return Ok(snapshot),
            Err(error) if error.code == ProjectErrorCode::EditorUnavailable => {}
            Err(error) => return Err(error),
        }
        let bytes = disk::read_bounded(&disk::scoped_path(&self.root, request.path.as_str(), false)?, self.limits.max_gui_source_bytes)?;
        let source = String::from_utf8(bytes.clone()).map_err(io)?;
        photocraft_defold_format::Scene::parse(source.clone()).map_err(|e| fail(ProjectErrorCode::InvalidRequest, e))?;
        let hash = ContentHash::sha256(&bytes);
        Ok(SceneSnapshot {
            scene_id: request.scene_id.clone(),
            path: request.path.clone(),
            source,
            disk_hash: hash.clone(),
            scene_hash: hash.clone(),
            editor_graph_revision: format!("disk:{hash}"),
            dirty: false,
            editor_session_id: None,
            dependencies: vec![],
        })
    }
    fn scene_identity(&self, request: &SceneIdentityRequest) -> ProjectResult<SceneIdentity> {
        shared_artwork::scene_identity(self, request)
    }
    fn shared_asset_usage(&self, request: &SharedAssetRequest) -> ProjectResult<SharedAssetUsage> {
        shared_artwork::usage(self, request)
    }
    fn read_resource(&self, request: &ResourceReadRequest) -> ProjectResult<ResourceContent> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_context(&mut state, &request.context)?;
        if request.path.as_str().split('/').any(|part| part.starts_with('.')) {
            return Err(fail(ProjectErrorCode::PathInvalid, "Internal project data requires a dedicated source operation"));
        }
        let path = disk::scoped_path(&self.root, request.path.as_str(), false)?;
        if !path.exists() {
            return Err(fail(ProjectErrorCode::ResourceMissing, "Project resource does not exist"));
        }
        let bytes = disk::read_bounded(&path, self.limits.max_dependency_bytes)?;
        let content = BlobDescriptor::of_bytes(&bytes);
        if request.expected_hash.as_ref().is_some_and(|hash| *hash != content.hash) {
            return Err(fail(ProjectErrorCode::TargetChanged, "Project resource has changed"));
        }
        Ok(ResourceContent { version: ResourceVersion { path: request.path.clone(), content }, bytes })
    }
    fn reveal_resource(&self, request: &ResourceReadRequest) -> ProjectResult<()> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_context(&mut state, &request.context)?;
        if request.path.as_str().split('/').any(|part| part.starts_with('.')) {
            return Err(fail(ProjectErrorCode::PathInvalid, "Internal project data cannot be opened through a game resource operation"));
        }
        let extension = Path::new(request.path.as_str()).extension().and_then(|value| value.to_str()).unwrap_or_default().to_ascii_lowercase();
        if !matches!(
            extension.as_str(),
            "gui"
                | "atlas"
                | "png"
                | "jpg"
                | "jpeg"
                | "font"
                | "material"
                | "tilesource"
                | "tilemap"
                | "sprite"
                | "label"
                | "model"
                | "mesh"
                | "particlefx"
                | "collection"
                | "go"
                | "script"
                | "gui_script"
                | "render_script"
                | "lua"
                | "render"
                | "vp"
                | "fp"
                | "buffer"
                | "animationset"
                | "ttf"
                | "otf"
        ) {
            return Err(fail(ProjectErrorCode::UnsupportedGuiFeature, "This file type is not a supported Defold resource"));
        }
        let path = disk::scoped_path(&self.root, request.path.as_str(), false)?;
        if !path.is_file() {
            return Err(fail(ProjectErrorCode::ResourceMissing, "Defold resource does not exist"));
        }
        if let Some(expected) = &request.expected_hash {
            let bytes = disk::read_bounded(&path, self.limits.max_dependency_bytes)?;
            if !expected.verify(&bytes) {
                return Err(fail(ProjectErrorCode::TargetChanged, "Defold resource changed before it could be opened"));
            }
        }
        crate::defold_project_editor::reveal_resource(&self.root, request)
    }
    fn read_source(&self, request: &ReadSourceRequest) -> ProjectResult<SourcePackage> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_context(&mut state, &request.context)?;
        let revision = request
            .source_revision
            .as_ref()
            .or_else(|| state.index.heads.get(&request.package_id))
            .ok_or_else(|| fail(ProjectErrorCode::ResourceMissing, "No saved creative source exists for this package"))?;
        self.package(&request.package_id, revision)
    }
    fn save_source(&self, request: &SaveSourceRequest, payload: &SourcePayload) -> ProjectResult<SourceSaveReceipt> {
        request.validate(&self.limits)?;
        request.envelope.verify_payload(payload, &self.limits)?;
        let native_document = photocraft_format::load_from_bytes(&payload.native)
            .map_err(|e| fail(ProjectErrorCode::InvalidRequest, format!("Invalid native creative document: {e}")))?;
        if let Some(project) = native_document.defold.as_ref().and_then(|design| design.project.as_ref())
            && (project.project_id != request.context.project_id
                || project.package_id != request.package_id
                || request.scene_id.as_ref() != Some(&project.scene_id)
                || project.layers.iter().map(|(local, binding)| (*local, binding.layer_id.clone())).collect::<BTreeMap<_, _>>()
                    != request.envelope.identity_map)
        {
            return Err(fail(ProjectErrorCode::ProjectMismatch, "Native source identities do not match their envelope"));
        }
        let request_hash = ContentHash::sha256(&serde_json::to_vec(request).map_err(io)?);
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_context(&mut state, &request.context)?;
        if let Some(previous) = state.index.operations.get(&request.operation_id) {
            return if previous.request_hash == request_hash {
                Ok(previous.receipt.clone())
            } else {
                Err(fail(ProjectErrorCode::IdempotencyMismatch, "Operation identity was reused for different creative content"))
            };
        }
        self.check_leases(&state, &request.client_id, &request.leases)?;
        if crate::defold_project_publication::resources_locked(
            &self.root,
            &request.leases.iter().map(|lease| lease.resource.clone()).collect::<Vec<_>>(),
            None,
        )? {
            return Err(fail(ProjectErrorCode::Busy, "A pending publication owns this source"));
        }
        for source in &request.envelope.draft.shared_sources {
            let _ = self.package(&source.package_id, &source.source_revision)?;
        }
        shared_artwork::validate_source_links(self, request, &native_document)?;
        if state.index.heads.get(&request.package_id) != request.expected_source_revision.as_ref() {
            return Err(fail(ProjectErrorCode::SourceConflict, "Creative source changed on the project host; the local draft is preserved"));
        }
        if let Some(scene_id) = &request.scene_id {
            let scene = state.index.scenes.get(scene_id).ok_or_else(|| fail(ProjectErrorCode::ResourceMissing, "Unknown GUI source"))?;
            if scene.package_id != request.package_id || !request.envelope.scene_bases.iter().any(|base| base.scene_id == *scene_id && base.path == scene.path)
            {
                return Err(fail(ProjectErrorCode::ProjectMismatch, "Creative source does not belong to this GUI"));
            }
        }
        let revision = &request.envelope.source_revision;
        let envelope_path = self.revision_path(&request.package_id, revision, "revision.json", true)?;
        if envelope_path.is_file() {
            let existing = self.package(&request.package_id, revision)?;
            if existing.envelope != request.envelope || existing.payload != *payload {
                return Err(fail(ProjectErrorCode::SourceConflict, "This immutable source revision already exists with different content"));
            }
            // An upload may have completed before a crash interrupted the head commit.
            // Its complete, verified immutable objects can be committed on retry.
        } else {
            disk::write_atomic(&self.revision_path(&request.package_id, revision, "document.pcraft", true)?, &payload.native)?;
            for (hash, bytes) in &payload.dependencies {
                disk::write_atomic(
                    &self.revision_path(&request.package_id, revision, &format!("dependencies/{}", hash.as_str().replace(':', "-")), true)?,
                    bytes,
                )?;
            }
            disk::write_json(&self.revision_path(&request.package_id, revision, "dependencies.json", true)?, &request.envelope.dependencies)?;
            disk::write_json(&envelope_path, &request.envelope)?;
        }
        // Reopen and verify before exposing a new head. An interrupted upload is not a saved draft.
        let _ = self.package(&request.package_id, revision)?;
        self.check_context(&mut state, &request.context)?;
        self.check_leases(&state, &request.client_id, &request.leases)?;
        let receipt = SourceSaveReceipt {
            schema_version: SCHEMA_VERSION,
            operation_id: request.operation_id.clone(),
            context: request.context.clone(),
            package_id: request.package_id.clone(),
            source_revision: revision.clone(),
            draft_revision: request.envelope.draft_revision.clone(),
            captured_native_revision: request.captured_native_revision,
            content: request.envelope.native.clone(),
        };
        let mut index = state.index.clone();
        index.heads.insert(request.package_id.clone(), revision.clone());
        if let Some(scene) = request.scene_id.as_ref().and_then(|id| index.scenes.get_mut(id)) {
            scene.source_revision = Some(revision.clone());
        }
        index.operations.insert(request.operation_id.clone(), SourceOperation { request_hash, receipt: receipt.clone() });
        self.persist_index(&index)?;
        state.index = index;
        Ok(receipt)
    }
    fn operation_status(&self, request: &OperationRequest) -> ProjectResult<Option<SourceSaveReceipt>> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_context(&mut state, &request.context)?;
        Ok(state.index.operations.get(&request.operation_id).map(|operation| operation.receipt.clone()))
    }
    fn acquire_lease(&self, request: &LeaseRequest) -> ProjectResult<LeaseGrant> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_context(&mut state, &request.context)?;
        let key = serde_json::to_string(&request.resource).map_err(io)?;
        if crate::defold_project_publication::resources_locked(&self.root, std::slice::from_ref(&request.resource), None)? {
            return Err(fail(ProjectErrorCode::Busy, "A publication owns this creative resource"));
        }
        let time = now()?;
        if let Some(grant) = state.leases.grants.get(&key).filter(|grant| grant.expires_at_ms > time && grant.client_id == request.client_id) {
            return Ok(grant.clone());
        }
        if state.leases.grants.get(&key).is_some_and(|grant| grant.expires_at_ms > time && grant.client_id != request.client_id) {
            return Err(fail(ProjectErrorCode::LeaseLost, "Another client is editing this creative resource"));
        }
        state.leases.next_epoch = state.leases.next_epoch.checked_add(1).ok_or_else(|| fail(ProjectErrorCode::LeaseLost, "Lease epoch exhausted"))?;
        let grant = LeaseGrant {
            client_id: request.client_id.clone(),
            proof: LeaseProof { resource: request.resource.clone(), lease_epoch: state.leases.next_epoch },
            expires_at_ms: time
                .checked_add(request.duration_ms.clamp(1000, MAX_LEASE_MS))
                .ok_or_else(|| fail(ProjectErrorCode::LeaseLost, "Lease time overflow"))?,
        };
        state.leases.grants.insert(key, grant.clone());
        self.persist_leases(&state.leases)?;
        Ok(grant)
    }
    fn renew_lease(&self, request: &LeaseRenewRequest) -> ProjectResult<LeaseGrant> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_context(&mut state, &request.context)?;
        self.check_leases(&state, &request.client_id, std::slice::from_ref(&request.proof))?;
        let key = serde_json::to_string(&request.proof.resource).map_err(io)?;
        let grant = LeaseGrant {
            client_id: request.client_id.clone(),
            proof: request.proof.clone(),
            expires_at_ms: now()?
                .checked_add(request.duration_ms.clamp(1000, MAX_LEASE_MS))
                .ok_or_else(|| fail(ProjectErrorCode::LeaseLost, "Lease time overflow"))?,
        };
        state.leases.grants.insert(key, grant.clone());
        self.persist_leases(&state.leases)?;
        Ok(grant)
    }
    fn release_lease(&self, request: &LeaseReleaseRequest) -> ProjectResult<()> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_context(&mut state, &request.context)?;
        self.check_leases(&state, &request.client_id, std::slice::from_ref(&request.proof))?;
        if crate::defold_project_publication::resources_locked(&self.root, std::slice::from_ref(&request.proof.resource), None)? {
            return Err(fail(ProjectErrorCode::Busy, "A publication owns this creative resource"));
        }
        state.leases.grants.remove(&serde_json::to_string(&request.proof.resource).map_err(io)?);
        self.persist_leases(&state.leases)
    }
    fn read_asset_registry(&self, context: &ProjectContext) -> ProjectResult<crate::defold_publish_plan::AssetRegistry> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_context(&mut state, context)?;
        crate::defold_project_publication::registry(&self.root)
    }
    fn asset_management(&self, context: &ProjectContext) -> ProjectResult<AssetManagementSnapshot> {
        asset_management::inspect(self, context)
    }
    fn migrate_legacy(&self, request: &LegacyMigrationRequest) -> ProjectResult<crate::defold_migration::MigrationReport> {
        asset_management::migrate(self, request)
    }
    fn migration_report(&self, request: &OperationRequest) -> ProjectResult<Option<crate::defold_migration::MigrationReport>> {
        asset_management::report(self, request)
    }
    fn latest_publication(&self, request: &SceneRequest) -> ProjectResult<Option<PublishReceipt>> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_context(&mut state, &request.context)?;
        let scene = state.index.scenes.get(&request.scene_id).ok_or_else(|| fail(ProjectErrorCode::ResourceMissing, "Unknown GUI scene"))?;
        if scene.path != request.path {
            return Err(fail(ProjectErrorCode::TargetChanged, "GUI resource location changed"));
        }
        crate::defold_project_publication::latest_for_context(&self.root, &request.scene_id, &request.context)
    }
    fn published_scene_baseline(&self, request: &SceneRequest) -> ProjectResult<Option<SceneBaseline>> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_context(&mut state, &request.context)?;
        let scene = state.index.scenes.get(&request.scene_id).ok_or_else(|| fail(ProjectErrorCode::ResourceMissing, "Unknown GUI scene"))?;
        if scene.path != request.path {
            return Err(fail(ProjectErrorCode::TargetChanged, "GUI resource location changed"));
        }
        crate::defold_project_publication::published_baseline(&self.root, request)
    }
    fn prepare_publish(&self, plan: &PublishPlan, payload: &PublishPayload) -> ProjectResult<PublishReceipt> {
        plan.verify_payload(payload, &self.limits)?;
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_context(&mut state, &plan.context)?;
        if let Some(receipt) = crate::defold_project_publication::stored_receipt(&self.root, &plan.transaction_id)? {
            return if receipt.plan_hash == plan.plan_hash {
                Ok(receipt)
            } else {
                Err(fail(ProjectErrorCode::IdempotencyMismatch, "Publication identity was reused"))
            };
        }
        self.validate_publish_state(&mut state, plan)?;
        crate::defold_project_publication::prepare(&self.root, plan, payload, &self.limits)
    }
    fn commit_publish(&self, request: &PublishRequest) -> ProjectResult<PublishReceipt> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_context(&mut state, &request.context)?;
        let plan = crate::defold_project_publication::saved_plan(&self.root, &request.transaction_id)?
            .ok_or_else(|| fail(ProjectErrorCode::ResourceMissing, "Publication was not prepared"))?;
        self.check_context(&mut state, &plan.context)?;
        if crate::defold_project_publication::stored_receipt(&self.root, &request.transaction_id)?
            .is_some_and(|receipt| receipt.phase == PublishPhase::Prepared)
        {
            self.validate_publish_state(&mut state, &plan)?;
        }
        crate::defold_project_publication::commit(&self.root, request, &self.limits)
    }
    fn publish_status(&self, request: &PublishRequest) -> ProjectResult<Option<PublishReceipt>> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_context(&mut state, &request.context)?;
        if let Some(plan) = crate::defold_project_publication::saved_plan(&self.root, &request.transaction_id)? {
            self.check_context(&mut state, &plan.context)?;
        }
        crate::defold_project_publication::status(&self.root, request, &self.limits)
    }
    fn cancel_publish(&self, request: &PublishRequest) -> ProjectResult<PublishReceipt> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_context(&mut state, &request.context)?;
        if let Some(plan) = crate::defold_project_publication::saved_plan(&self.root, &request.transaction_id)? {
            self.check_context(&mut state, &plan.context)?;
        }
        crate::defold_project_publication::cancel(&self.root, request, &self.limits)
    }
}

/// Include the current ref's value, not only HEAD's branch name. Worktree gitdirs
/// are read-only metadata; their contents never become transferable resources.
fn workspace_stamp(root: &Path) -> ProjectResult<String> {
    let marker = root.join(".git");
    let git = if marker.is_file() {
        let bytes = disk::read_bounded(&marker, 4096)?;
        let text = String::from_utf8(bytes).map_err(io)?;
        let location = text.trim().strip_prefix("gitdir:").ok_or_else(|| fail(ProjectErrorCode::InvalidRequest, "Invalid Git working-copy marker"))?.trim();
        root.join(location)
    } else {
        marker
    };
    if !git.join("HEAD").is_file() {
        return Ok("filesystem".into());
    }
    let head = disk::read_bounded(&git.join("HEAD"), 4096)?;
    let mut bytes = head.clone();
    if let Some(reference) = std::str::from_utf8(&head).map_err(io)?.trim().strip_prefix("ref: ") {
        if !reference.starts_with("refs/") || reference.split('/').any(|p| matches!(p, "" | "." | "..")) || reference.contains(['\\', ':']) {
            return Err(fail(ProjectErrorCode::InvalidRequest, "Invalid Git reference"));
        }
        let ref_path = git.join(reference);
        if ref_path.is_file() {
            bytes.extend_from_slice(&disk::read_bounded(&ref_path, 4096)?);
        } else if git.join("packed-refs").is_file() {
            let packed = disk::read_bounded(&git.join("packed-refs"), 16 * 1024 * 1024)?;
            if let Some(line) = std::str::from_utf8(&packed).map_err(io)?.lines().find(|line| line.split_whitespace().nth(1) == Some(reference)) {
                bytes.extend_from_slice(line.as_bytes());
            }
        }
    }
    Ok(ContentHash::sha256(&bytes).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::{ColorMode, SampleType};
    use photocraft_doc::{Document, Size};

    pub(super) fn fixture() -> (tempfile::TempDir, Arc<LocalProjectStore>, SaveSourceRequest, SourcePayload) {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("game.project"), b"[project]\ntitle = Test\n").unwrap();
        fs::write(directory.path().join("test.gui"), b"nodes { type: TYPE_BOX id: \"root\" size { x: 8 y: 8 } }\n").unwrap();
        let store = LocalProjectStore::open(directory.path()).unwrap();
        let description = store.describe().unwrap();
        let scene = store.resolve_scene_path(&ResourcePath::parse("/test.gui").unwrap()).unwrap();
        let client: ClientId = new_id().unwrap();
        let mut leases = vec![];
        for resource in [LeaseResource::Package(scene.package_id.clone()), LeaseResource::Scene(scene.scene_id.clone())] {
            leases.push(
                store
                    .acquire_lease(&LeaseRequest { context: description.context.clone(), client_id: client.clone(), resource, duration_ms: MAX_LEASE_MS })
                    .unwrap()
                    .proof,
            );
        }
        let snapshot =
            store.snapshot(&SceneRequest { context: description.context.clone(), scene_id: scene.scene_id.clone(), path: scene.path.clone() }).unwrap();
        let doc = Document::new("Test", Size::new(8, 8), ColorMode::Rgb, SampleType::U8);
        let native = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
        let request = SaveSourceRequest {
            schema_version: SCHEMA_VERSION,
            operation_id: new_id().unwrap(),
            context: description.context,
            client_id: client,
            scene_id: Some(scene.scene_id.clone()),
            package_id: scene.package_id.clone(),
            expected_source_revision: None,
            leases,
            captured_native_revision: 1,
            envelope: SourceEnvelope {
                schema_version: SCHEMA_VERSION,
                package_id: scene.package_id,
                source_revision: "version-1".into(),
                parent_source_revision: None,
                draft_revision: "draft-1".into(),
                scene_bases: vec![SceneBaseline {
                    scene_id: scene.scene_id,
                    path: scene.path,
                    gui_source: snapshot.source,
                    disk_hash: snapshot.disk_hash,
                    scene_hash: snapshot.scene_hash,
                    published_revision: None,
                }],
                identity_map: BTreeMap::new(),
                native: BlobDescriptor::of_bytes(&native),
                dependencies: vec![],
                draft: DraftSnapshot::default(),
            },
        };
        (directory, store, request, SourcePayload { native, dependencies: BTreeMap::new() })
    }

    #[test]
    fn source_is_durable_idempotent_and_does_not_publish_game_resources() {
        let (directory, store, request, payload) = fixture();
        let before = fs::read(directory.path().join("test.gui")).unwrap();
        let receipt = store.save_source(&request, &payload).unwrap();
        assert_eq!(store.save_source(&request, &payload).unwrap(), receipt);
        assert_eq!(
            store
                .read_source(&ReadSourceRequest { context: request.context.clone(), package_id: request.package_id.clone(), source_revision: None })
                .unwrap()
                .payload,
            payload
        );
        assert_eq!(fs::read(directory.path().join("test.gui")).unwrap(), before);
        assert!(!directory.path().join("photocraft_generated").exists());
        let mut changed = request.clone();
        changed.captured_native_revision += 1;
        assert_eq!(store.save_source(&changed, &payload).unwrap_err().code, ProjectErrorCode::IdempotencyMismatch);
        drop(store);
        let restarted = LocalProjectStore::open(directory.path()).unwrap();
        assert_eq!(
            restarted.operation_status(&OperationRequest { context: request.context.clone(), operation_id: request.operation_id }).unwrap(),
            Some(receipt)
        );
        assert_eq!(
            restarted.read_source(&ReadSourceRequest { context: request.context, package_id: request.package_id, source_revision: None }).unwrap().payload,
            payload
        );
    }

    #[test]
    fn source_cas_hashes_and_expired_authority_are_enforced() {
        let (_directory, store, request, payload) = fixture();
        let mut corrupted = payload.clone();
        corrupted.native.push(1);
        assert_eq!(store.save_source(&request, &corrupted).unwrap_err().code, ProjectErrorCode::HashMismatch);
        assert!(store.operation_status(&OperationRequest { context: request.context.clone(), operation_id: request.operation_id.clone() }).unwrap().is_none());
        store.save_source(&request, &payload).unwrap();
        let mut stale = request.clone();
        stale.operation_id = new_id().unwrap();
        stale.envelope.source_revision = "version-2".into();
        assert_eq!(store.save_source(&stale, &payload).unwrap_err().code, ProjectErrorCode::SourceConflict);
        let other =
            LeaseRequest { context: request.context.clone(), client_id: new_id().unwrap(), resource: request.leases[0].resource.clone(), duration_ms: 1000 };
        assert_eq!(store.acquire_lease(&other).unwrap_err().code, ProjectErrorCode::LeaseLost);
        store
            .release_lease(&LeaseReleaseRequest { context: request.context.clone(), client_id: request.client_id.clone(), proof: request.leases[0].clone() })
            .unwrap();
        assert!(store.acquire_lease(&other).is_ok());
        stale.expected_source_revision = Some("version-1".into());
        stale.envelope.parent_source_revision = stale.expected_source_revision.clone();
        assert_eq!(store.save_source(&stale, &payload).unwrap_err().code, ProjectErrorCode::LeaseLost);
    }

    #[test]
    fn complete_orphan_upload_can_resume_after_head_commit_was_interrupted() {
        let (directory, store, request, payload) = fixture();
        let old_index = store.state.lock().unwrap().index.clone();
        store.save_source(&request, &payload).unwrap();
        store.persist_index(&old_index).unwrap();
        drop(store);
        let store = LocalProjectStore::open(directory.path()).unwrap();
        let mut retried = request.clone();
        retried.leases = request
            .leases
            .iter()
            .map(|proof| {
                store
                    .acquire_lease(&LeaseRequest {
                        context: request.context.clone(),
                        client_id: request.client_id.clone(),
                        resource: proof.resource.clone(),
                        duration_ms: MAX_LEASE_MS,
                    })
                    .unwrap()
                    .proof
            })
            .collect();
        assert_eq!(store.save_source(&retried, &payload).unwrap().source_revision, "version-1");
    }

    #[test]
    fn equal_basenames_in_different_folders_have_separate_scene_and_package_identities() {
        let (directory, store, request, _payload) = fixture();
        fs::create_dir(directory.path().join("other")).unwrap();
        fs::copy(directory.path().join("test.gui"), directory.path().join("other/test.gui")).unwrap();
        let other = store.resolve_scene_path(&ResourcePath::parse("/other/test.gui").unwrap()).unwrap();
        assert_ne!(Some(other.scene_id), request.scene_id);
        assert_ne!(other.package_id, request.package_id);
        let clone = LocalProjectStore::open(directory.path()).unwrap();
        assert!(Arc::ptr_eq(&store, &clone));
    }

    #[test]
    fn reveal_rejects_hidden_unknown_missing_changed_and_foreign_resources_before_editor_io() {
        let (directory, store, request, _payload) = fixture();
        fs::write(directory.path().join("launch.cmd"), b"do not execute").unwrap();
        fs::create_dir(directory.path().join("folder.png")).unwrap();
        let mut reveal = ResourceReadRequest { context: request.context.clone(), path: ResourcePath::parse("/test.gui").unwrap(), expected_hash: None };
        for (path, code) in [
            ("/.photocraft/secret.png", ProjectErrorCode::PathInvalid),
            ("/launch.cmd", ProjectErrorCode::UnsupportedGuiFeature),
            ("/missing.png", ProjectErrorCode::ResourceMissing),
            ("/folder.png", ProjectErrorCode::PathInvalid),
        ] {
            reveal.path = ResourcePath::parse(path).unwrap();
            assert_eq!(store.reveal_resource(&reveal).unwrap_err().code, code);
        }
        reveal.path = ResourcePath::parse("/test.gui").unwrap();
        reveal.expected_hash = Some(ContentHash::sha256(b"stale bytes"));
        assert_eq!(store.reveal_resource(&reveal).unwrap_err().code, ProjectErrorCode::TargetChanged);
        reveal.context.project_id = ProjectId::from_bytes([112; 16]);
        assert_eq!(store.reveal_resource(&reveal).unwrap_err().code, ProjectErrorCode::ProjectMismatch);
        assert!(store.describe().unwrap().capabilities.contains("resourceRevealV1"));
    }
}
