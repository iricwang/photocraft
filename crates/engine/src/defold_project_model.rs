//! Version-one project protocol. These types contain no connection credentials or ambient paths.
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

pub use photocraft_doc::defold::{
    AppearanceSource, ArtworkSourceNode, AssetId, CheckoutId, ClientId, EditorSessionId, LayerBinding, LayerRole, NodeId, OperationId, PackageId, ProjectId,
    ProjectLink, SceneId, SharedAppearanceRef, StableIdentity, StableLayerId, TransactionId, WorkspaceId,
};

pub const SCHEMA_VERSION: u32 = 1;
pub type ProjectResult<T> = Result<T, ProjectError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProjectErrorCode {
    ProtocolUnsupported,
    ProjectMismatch,
    WorkspaceChanged,
    LeaseLost,
    SourceConflict,
    TargetChanged,
    TargetDirty,
    EditorUnavailable,
    ResourceMissing,
    UnsupportedGuiFeature,
    InvalidTransform,
    PathInvalid,
    NameCollision,
    HashMismatch,
    DiskFull,
    IdempotencyMismatch,
    SaveFailed,
    RecoveryRequired,
    UndoBarrierRequired,
    LimitExceeded,
    Busy,
    Io,
    InvalidRequest,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectError {
    pub code: ProjectErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<PublishPhase>,
    #[serde(default)]
    pub resources: Vec<String>,
    #[serde(default)]
    pub retryable: bool,
}
impl ProjectError {
    pub fn new(code: ProjectErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), operation_id: None, phase: None, resources: vec![], retryable: false }
    }
}
impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}
impl std::error::Error for ProjectError {}

pub fn new_id<T: StableIdentity>() -> ProjectResult<T> {
    let mut bytes = [0; 16];
    getrandom::getrandom(&mut bytes).map_err(|e| ProjectError::new(ProjectErrorCode::Io, e.to_string()))?;
    Ok(T::from_bytes(bytes))
}

/// A canonical project-relative resource path; resolving it still requires symlink/junction checks.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ResourcePath(String);
impl ResourcePath {
    pub fn parse(path: &str) -> ProjectResult<Self> {
        if path.len() > 4096
            || !path.starts_with('/')
            || path.starts_with("//")
            || path.contains(['\\', ':', '<', '>', '"', '|', '?', '*'])
            || path.chars().any(char::is_control)
            || path
                .get(1..)
                .is_none_or(|p| p.split('/').any(|part| part.is_empty() || matches!(part, "." | "..") || part.ends_with(['.', ' ']) || reserved_name(part)))
        {
            return Err(ProjectError::new(ProjectErrorCode::PathInvalid, "Expected a project-relative POSIX resource path"));
        }
        Ok(Self(path.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn case_key(&self) -> String {
        self.0.to_lowercase()
    }
}
fn reserved_name(part: &str) -> bool {
    let base = part.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(
        base.as_str(),
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
    )
}
impl TryFrom<String> for ResourcePath {
    type Error = ProjectError;
    fn try_from(v: String) -> ProjectResult<Self> {
        Self::parse(&v)
    }
}
impl From<ResourcePath> for String {
    fn from(v: ResourcePath) -> Self {
        v.0
    }
}
impl fmt::Display for ResourcePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Algorithm-prefixed digest, never confused with an untyped legacy BLAKE3 string.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContentHash(String);
impl ContentHash {
    pub fn parse(value: &str) -> ProjectResult<Self> {
        let valid = value
            .split_once(':')
            .is_some_and(|(algorithm, digest)| matches!(algorithm, "sha256" | "blake3") && digest.len() == 64 && digest.bytes().all(|c| c.is_ascii_hexdigit()));
        if !valid {
            return Err(ProjectError::new(ProjectErrorCode::HashMismatch, "Expected sha256: or blake3: with 64 hexadecimal digits"));
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
    pub fn sha256(bytes: &[u8]) -> Self {
        Self(format!("sha256:{:x}", sha2::Sha256::digest(bytes)))
    }
    pub fn blake3(bytes: &[u8]) -> Self {
        Self(format!("blake3:{}", blake3::hash(bytes).to_hex()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn verify(&self, bytes: &[u8]) -> bool {
        if self.0.starts_with("sha256:") { self == &Self::sha256(bytes) } else { self == &Self::blake3(bytes) }
    }
}
impl TryFrom<String> for ContentHash {
    type Error = ProjectError;
    fn try_from(v: String) -> ProjectResult<Self> {
        Self::parse(&v)
    }
}
impl From<ContentHash> for String {
    fn from(v: ContentHash) -> Self {
        v.0
    }
}
impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectContext {
    pub project_id: ProjectId,
    /// The authority's working copy, not a remote client's cache identity.
    pub checkout_id: CheckoutId,
    pub workspace_id: WorkspaceId,
    pub workspace_epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectLimits {
    pub max_control_bytes: u64,
    pub max_native_bytes: u64,
    pub max_dependency_bytes: u64,
    pub max_total_source_bytes: u64,
    pub max_dependencies: u32,
    pub max_resources: u32,
    pub max_gui_source_bytes: u64,
}
impl Default for ProjectLimits {
    fn default() -> Self {
        Self {
            max_control_bytes: 4 << 20,
            max_native_bytes: 1 << 30,
            max_dependency_bytes: 256 << 20,
            max_total_source_bytes: 2 << 30,
            max_dependencies: 4096,
            max_resources: 8192,
            max_gui_source_bytes: 16 << 20,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorDescriptor {
    pub editor_session_id: EditorSessionId,
    pub capabilities: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDescription {
    pub schema_version: u32,
    pub context: ProjectContext,
    pub display_name: String,
    pub capabilities: BTreeSet<String>,
    pub editor: Option<EditorDescriptor>,
    pub limits: ProjectLimits,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SceneRequest {
    pub context: ProjectContext,
    pub scene_id: SceneId,
    pub path: ResourcePath,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneIdentity {
    pub scene_id: SceneId,
    pub path: ResourcePath,
    pub package_id: PackageId,
    pub source_revision: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneSnapshot {
    pub scene_id: SceneId,
    pub path: ResourcePath,
    pub source: String,
    pub disk_hash: ContentHash,
    pub scene_hash: ContentHash,
    pub editor_graph_revision: String,
    pub dirty: bool,
    pub editor_session_id: Option<EditorSessionId>,
    #[serde(default)]
    pub dependencies: Vec<ResourceVersion>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlobDescriptor {
    pub hash: ContentHash,
    pub length: u64,
}
impl BlobDescriptor {
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self { hash: ContentHash::sha256(bytes), length: bytes.len() as u64 }
    }
    pub fn verify(&self, bytes: &[u8]) -> ProjectResult<()> {
        if bytes.len() as u64 != self.length || !self.hash.verify(bytes) {
            Err(ProjectError::new(ProjectErrorCode::HashMismatch, "Content length or digest differs from its descriptor"))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceVersion {
    pub path: ResourcePath,
    pub content: BlobDescriptor,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceReadRequest {
    pub context: ProjectContext,
    pub path: ResourcePath,
    pub expected_hash: Option<ContentHash>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceContent {
    pub version: ResourceVersion,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceRevisionRef {
    pub package_id: PackageId,
    pub source_revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SceneBaseline {
    pub scene_id: SceneId,
    pub path: ResourcePath,
    pub gui_source: String,
    pub disk_hash: ContentHash,
    pub scene_hash: ContentHash,
    pub published_revision: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceDependency {
    pub dependency_id: String,
    pub logical_path: String,
    pub content: BlobDescriptor,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DraftSnapshot {
    /// Full pending GUI source per scene; these are drafts, never authoritative current GUI files.
    #[serde(default)]
    pub gui_sources: BTreeMap<SceneId, String>,
    /// A GUI package references these authoritative packages; it must not save their cached pixels as a new shared source.
    #[serde(default)]
    pub shared_sources: Vec<SourceRevisionRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceEnvelope {
    pub schema_version: u32,
    pub package_id: PackageId,
    pub source_revision: String,
    pub parent_source_revision: Option<String>,
    pub draft_revision: String,
    pub scene_bases: Vec<SceneBaseline>,
    pub identity_map: BTreeMap<u64, StableLayerId>,
    pub native: BlobDescriptor,
    pub dependencies: Vec<SourceDependency>,
    pub draft: DraftSnapshot,
}

/// Binary bodies travel separately from the control JSON. Never base64 an entire creative source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePayload {
    pub native: Vec<u8>,
    pub dependencies: BTreeMap<ContentHash, Vec<u8>>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePackage {
    pub envelope: SourceEnvelope,
    pub payload: SourcePayload,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "resourceKind", content = "resourceId", rename_all = "camelCase")]
pub enum LeaseResource {
    Scene(SceneId),
    Package(PackageId),
    Asset(AssetId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeaseProof {
    pub resource: LeaseResource,
    pub lease_epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeaseRequest {
    pub context: ProjectContext,
    pub client_id: ClientId,
    pub resource: LeaseResource,
    pub duration_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeaseRenewRequest {
    pub context: ProjectContext,
    pub client_id: ClientId,
    pub proof: LeaseProof,
    pub duration_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeaseReleaseRequest {
    pub context: ProjectContext,
    pub client_id: ClientId,
    pub proof: LeaseProof,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseGrant {
    pub client_id: ClientId,
    pub proof: LeaseProof,
    pub expires_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveSourceRequest {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub context: ProjectContext,
    pub client_id: ClientId,
    pub scene_id: Option<SceneId>,
    pub package_id: PackageId,
    pub expected_source_revision: Option<String>,
    pub leases: Vec<LeaseProof>,
    pub captured_native_revision: u64,
    pub envelope: SourceEnvelope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadSourceRequest {
    pub context: ProjectContext,
    pub package_id: PackageId,
    pub source_revision: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationRequest {
    pub context: ProjectContext,
    pub operation_id: OperationId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSaveReceipt {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub context: ProjectContext,
    pub package_id: PackageId,
    pub source_revision: String,
    pub draft_revision: String,
    pub captured_native_revision: u64,
    pub content: BlobDescriptor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PublishPhase {
    Preparing,
    Prepared,
    Applying,
    AppliedUnsaved,
    Saving,
    Committed,
    Aborted,
    RecoveryRequired,
}
impl PublishPhase {
    pub fn may_cancel(self) -> bool {
        matches!(self, Self::Preparing | Self::Prepared)
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Committed | Self::Aborted)
    }
    pub fn allows(self, next: Self) -> bool {
        self == next
            || matches!(
                (self, next),
                (Self::Preparing, Self::Prepared | Self::Aborted | Self::RecoveryRequired)
                    | (Self::Prepared, Self::Applying | Self::Aborted | Self::RecoveryRequired)
                    | (Self::Applying, Self::AppliedUnsaved | Self::RecoveryRequired)
                    | (Self::AppliedUnsaved, Self::Saving | Self::RecoveryRequired)
                    | (Self::Saving, Self::Committed | Self::RecoveryRequired)
            )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileWriter {
    HostFile,
    EditorResource,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileAction {
    Create,
    Replace,
    Move,
    Remove,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlannedFile {
    pub writer: FileWriter,
    pub action: FileAction,
    pub asset_id: Option<AssetId>,
    pub old_path: Option<ResourcePath>,
    pub new_path: Option<ResourcePath>,
    pub expected_old_hash: Option<ContentHash>,
    pub content: Option<BlobDescriptor>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishTarget {
    pub scene_id: SceneId,
    pub path: ResourcePath,
    pub expected_published_revision: Option<String>,
    pub expected_gui_disk_hash: ContentHash,
    pub expected_editor_snapshot: String,
}

/// The bridge accepts known node properties, never caller-supplied Lua or executable expressions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuiNodeData {
    pub id: String,
    pub parent: String,
    pub kind: String,
    pub values: BTreeMap<String, serde_json::Value>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuiTexture {
    pub name: String,
    pub texture: ResourcePath,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeRename {
    pub from: String,
    pub to: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SceneUpdate {
    pub scene_id: SceneId,
    pub nodes: Vec<GuiNodeData>,
    pub textures: Vec<GuiTexture>,
    pub renames: Vec<NodeRename>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssetOwnership {
    Managed,
    External,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssetVisibility {
    Private,
    Public,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssetState {
    Active,
    Detached,
    Tombstoned,
}
/// Reproducible mapping for the baked sRGB straight-alpha RGBA8 output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportRecipe {
    pub width: u32,
    pub height: u32,
    pub crop_origin: [i32; 2],
    pub node_local_transform: [f64; 6],
    pub cross_node_composition: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetChange {
    pub asset_id: AssetId,
    pub display_name: String,
    pub ownership: AssetOwnership,
    pub visibility: AssetVisibility,
    pub state: AssetState,
    pub source: Option<SourceRevisionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_node: Option<ArtworkSourceNode>,
    pub image: Option<ResourcePath>,
    pub runtime_frame_id: Option<String>,
    pub referencing_scenes: Vec<SceneId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<ExportRecipe>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishPlan {
    pub schema_version: u32,
    pub transaction_id: TransactionId,
    pub context: ProjectContext,
    pub client_id: ClientId,
    pub primary_scene_id: SceneId,
    pub source_revisions: Vec<SourceRevisionRef>,
    pub targets: Vec<PublishTarget>,
    pub leases: Vec<LeaseProof>,
    pub plan_hash: ContentHash,
    pub planned_files: Vec<PlannedFile>,
    pub node_changes: Vec<SceneUpdate>,
    pub asset_changes: Vec<AssetChange>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PublishPayload {
    pub contents: BTreeMap<ContentHash, Vec<u8>>,
}

/// Project-authority inventory. Clients display this report rather than walking local mirrors.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetManagementSnapshot {
    pub schema_version: u32,
    pub context: ProjectContext,
    pub registry: crate::defold_publish_plan::AssetRegistry,
    pub retained: Vec<RetainedResource>,
    pub collection: Vec<crate::defold_publish_plan::CollectionDecision>,
    pub legacy_inventory: crate::defold_migration::MigrationInventory,
    pub migrations: Vec<crate::defold_migration::MigrationReport>,
    pub warnings: Vec<String>,
    pub roots_complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SceneIdentityRequest {
    pub context: ProjectContext,
    pub scene_id: SceneId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedAssetRequest {
    pub context: ProjectContext,
    pub asset_id: AssetId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedAssetUsage {
    pub asset: crate::defold_publish_plan::AssetRecord,
    pub consumers: Vec<SceneIdentity>,
    /// Current package heads, including the canonical owner. Historical pinned dependencies
    /// remain in their immutable envelopes but are not silently advanced by this read.
    pub source_refs: Vec<SourceRevisionRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyMigrationRequest {
    pub context: ProjectContext,
    pub client_id: ClientId,
    pub operation_id: OperationId,
    /// Includes the source head captured when the user selected this migration target.
    pub scene: SceneIdentity,
    pub input: ResourcePath,
    pub inventory_hash: ContentHash,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishRequest {
    pub context: ProjectContext,
    pub transaction_id: TransactionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedTarget {
    pub scene_id: SceneId,
    pub path: ResourcePath,
    pub published_revision: String,
    pub disk_hash: ContentHash,
    pub scene_hash: ContentHash,
    pub editor_graph_revision: String,
    pub editor_ahead: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetainedResource {
    pub path: ResourcePath,
    pub reason: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishReceipt {
    pub schema_version: u32,
    pub transaction_id: TransactionId,
    pub context: ProjectContext,
    pub phase: PublishPhase,
    pub plan_hash: ContentHash,
    pub source_revisions: Vec<SourceRevisionRef>,
    pub targets: Vec<PublishedTarget>,
    pub files: Vec<ResourceVersion>,
    pub retained: Vec<RetainedResource>,
    pub error: Option<ProjectError>,
}

/// Shared fault-injection checkpoints; production never interprets these as a success override.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FailurePoint {
    SourceBeforeWrite,
    SourceAfterObjects,
    SourceAfterCommit,
    Prepare,
    InstallImages,
    ApplyAtlas,
    ApplyGraph,
    SaveTargets,
    PersistReceipt,
}

pub fn check_schema(version: u32) -> ProjectResult<()> {
    if version == SCHEMA_VERSION {
        Ok(())
    } else {
        Err(ProjectError::new(ProjectErrorCode::ProtocolUnsupported, format!("Unsupported schema version {version}")))
    }
}
pub fn decode_control<T: serde::de::DeserializeOwned>(bytes: &[u8], limits: &ProjectLimits) -> ProjectResult<T> {
    if bytes.len() as u64 > limits.max_control_bytes {
        return Err(ProjectError::new(ProjectErrorCode::LimitExceeded, "Control request exceeds limit"));
    }
    serde_json::from_slice(bytes).map_err(|e| ProjectError::new(ProjectErrorCode::InvalidRequest, e.to_string()))
}

fn revision(value: &str) -> ProjectResult<()> {
    if value.is_empty() || value.len() > 128 || !value.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.')) {
        Err(ProjectError::new(ProjectErrorCode::InvalidRequest, "Invalid opaque revision token"))
    } else {
        Ok(())
    }
}
fn limit(ok: bool, message: &str) -> ProjectResult<()> {
    if ok { Ok(()) } else { Err(ProjectError::new(ProjectErrorCode::LimitExceeded, message)) }
}
fn invalid(message: &str) -> ProjectError {
    ProjectError::new(ProjectErrorCode::InvalidRequest, message)
}

impl SourceEnvelope {
    pub fn validate(&self, limits: &ProjectLimits) -> ProjectResult<()> {
        check_schema(self.schema_version)?;
        revision(&self.source_revision)?;
        revision(&self.draft_revision)?;
        if let Some(parent) = &self.parent_source_revision {
            revision(parent)?;
        }
        limit(self.native.length <= limits.max_native_bytes, "Native source exceeds limit")?;
        limit(self.dependencies.len() <= limits.max_dependencies as usize, "Too many creative dependencies")?;
        limit(
            self.scene_bases.len() <= limits.max_resources as usize && self.identity_map.len() <= limits.max_resources as usize,
            "Source identity table exceeds limit",
        )?;
        let mut scenes = BTreeSet::new();
        for base in &self.scene_bases {
            if !scenes.insert(&base.scene_id) {
                return Err(invalid("Duplicate scene baseline"));
            }
            limit(base.gui_source.len() as u64 <= limits.max_gui_source_bytes, "GUI baseline exceeds limit")?;
            // The baseline can be serialized from an editor graph; diskHash intentionally
            // identifies the exact disk bytes, not that graph's formatting or float encoding.
        }
        let mut ids = BTreeSet::new();
        if self.identity_map.values().any(|id| !ids.insert(id)) {
            return Err(invalid("Duplicate stable layer identity"));
        }
        let mut dependencies = BTreeSet::new();
        let mut total = self.native.length;
        for dependency in &self.dependencies {
            if dependency.dependency_id.is_empty() || !dependencies.insert(&dependency.dependency_id) {
                return Err(invalid("Duplicate or empty dependency identity"));
            }
            ResourcePath::parse(&format!("/{}", dependency.logical_path))?;
            limit(dependency.content.length <= limits.max_dependency_bytes, "Creative dependency exceeds limit")?;
            total = total.checked_add(dependency.content.length).ok_or_else(|| ProjectError::new(ProjectErrorCode::LimitExceeded, "Source size overflow"))?;
        }
        limit(total <= limits.max_total_source_bytes, "Complete creative source exceeds limit")?;
        for (scene, source) in &self.draft.gui_sources {
            if !scenes.contains(scene) {
                return Err(invalid("Draft GUI has no three-way baseline"));
            }
            limit(source.len() as u64 <= limits.max_gui_source_bytes, "Draft GUI exceeds limit")?;
        }
        let mut shared_packages = BTreeSet::new();
        for shared in &self.draft.shared_sources {
            revision(&shared.source_revision)?;
            if shared.package_id == self.package_id || !shared_packages.insert(&shared.package_id) {
                return Err(invalid("Shared source must name another unique authoritative package"));
            }
        }
        Ok(())
    }
    pub fn verify_payload(&self, payload: &SourcePayload, limits: &ProjectLimits) -> ProjectResult<()> {
        self.validate(limits)?;
        self.native.verify(&payload.native)?;
        let expected: BTreeSet<_> = self.dependencies.iter().map(|d| &d.content.hash).collect();
        if payload.dependencies.keys().any(|h| !expected.contains(h)) {
            return Err(invalid("Unreferenced dependency upload"));
        }
        for dependency in &self.dependencies {
            let bytes = payload
                .dependencies
                .get(&dependency.content.hash)
                .ok_or_else(|| ProjectError::new(ProjectErrorCode::ResourceMissing, "Creative dependency was not uploaded"))?;
            dependency.content.verify(bytes)?;
        }
        Ok(())
    }
}
impl SaveSourceRequest {
    pub fn validate(&self, limits: &ProjectLimits) -> ProjectResult<()> {
        check_schema(self.schema_version)?;
        self.envelope.validate(limits)?;
        if self.package_id != self.envelope.package_id || self.expected_source_revision != self.envelope.parent_source_revision {
            return Err(invalid("Source envelope does not match package or CAS baseline"));
        }
        if !self.leases.iter().any(|p| p.resource == LeaseResource::Package(self.package_id.clone())) {
            return Err(ProjectError::new(ProjectErrorCode::LeaseLost, "Source save requires its package lease"));
        }
        if let Some(scene) = &self.scene_id {
            if !self.envelope.scene_bases.iter().any(|b| &b.scene_id == scene) {
                return Err(invalid("Source has no matching scene baseline"));
            }
            if !self.leases.iter().any(|p| p.resource == LeaseResource::Scene(scene.clone())) {
                return Err(ProjectError::new(ProjectErrorCode::LeaseLost, "GUI source save requires its scene lease"));
            }
        }
        Ok(())
    }
}

impl PlannedFile {
    pub fn validate(&self) -> ProjectResult<()> {
        let shape = match self.action {
            FileAction::Create => self.old_path.is_none() && self.expected_old_hash.is_none() && self.new_path.is_some() && self.content.is_some(),
            FileAction::Replace => self.old_path.is_some() && self.old_path == self.new_path && self.expected_old_hash.is_some() && self.content.is_some(),
            FileAction::Move => {
                self.old_path.is_some()
                    && self.new_path.is_some()
                    && self.old_path != self.new_path
                    && self.expected_old_hash.is_some()
                    && self.content.is_some()
            }
            FileAction::Remove => self.old_path.is_some() && self.new_path.is_none() && self.expected_old_hash.is_some() && self.content.is_none(),
        };
        if !shape {
            return Err(invalid("File action is missing its required paths, content or precondition"));
        }
        for path in self.old_path.iter().chain(self.new_path.iter()) {
            let lower = path.as_str().to_ascii_lowercase();
            let editor = lower.ends_with(".gui") || lower.ends_with(".atlas");
            if editor != (self.writer == FileWriter::EditorResource) {
                return Err(invalid("GUI and atlas files must have editorResource writer"));
            }
        }
        Ok(())
    }
}

impl PublishPlan {
    /// SHA-256 of canonical JSON with planHash omitted. Object keys are sorted at every depth.
    pub fn computed_hash(&self) -> ProjectResult<ContentHash> {
        let mut value = serde_json::to_value(self).map_err(|e| invalid(&e.to_string()))?;
        if let Some(object) = value.as_object_mut() {
            object.remove("planHash");
        }
        let bytes = canonical_json(&value)?;
        Ok(ContentHash::sha256(&bytes))
    }
    pub fn validate(&self, limits: &ProjectLimits) -> ProjectResult<()> {
        check_schema(self.schema_version)?;
        if self.computed_hash()? != self.plan_hash {
            return Err(ProjectError::new(ProjectErrorCode::HashMismatch, "Publish planHash does not match canonical plan"));
        }
        limit(
            self.targets.len() <= limits.max_resources as usize && self.planned_files.len() <= limits.max_resources as usize,
            "Publish plan exceeds resource limit",
        )?;
        let mut scenes = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for target in &self.targets {
            if !scenes.insert(&target.scene_id) || !paths.insert(target.path.case_key()) {
                return Err(invalid("Duplicate publish target"));
            }
            if !self.leases.iter().any(|p| p.resource == LeaseResource::Scene(target.scene_id.clone())) {
                return Err(ProjectError::new(ProjectErrorCode::LeaseLost, "Publish target has no scene lease"));
            }
        }
        if !scenes.contains(&self.primary_scene_id) {
            return Err(invalid("Primary scene is not a publish target"));
        }
        let mut packages = BTreeSet::new();
        for source in &self.source_revisions {
            revision(&source.source_revision)?;
            if !packages.insert(&source.package_id) {
                return Err(invalid("Duplicate source package"));
            }
            if !self.leases.iter().any(|p| p.resource == LeaseResource::Package(source.package_id.clone())) {
                return Err(ProjectError::new(ProjectErrorCode::LeaseLost, "Published source has no package lease"));
            }
        }
        let mut changed_paths = BTreeSet::new();
        let mut total = 0u64;
        for file in &self.planned_files {
            file.validate()?;
            let mut own = BTreeSet::new();
            for path in file.old_path.iter().chain(file.new_path.iter()) {
                own.insert(path.case_key());
            }
            if own.into_iter().any(|path| !changed_paths.insert(path)) {
                return Err(invalid("One resource path has multiple writers/actions"));
            }
            if let Some(content) = &file.content {
                limit(content.length <= limits.max_dependency_bytes, "Published file exceeds limit")?;
                total = total.checked_add(content.length).ok_or_else(|| ProjectError::new(ProjectErrorCode::LimitExceeded, "Publish size overflow"))?;
            }
        }
        limit(total <= limits.max_total_source_bytes, "Publish payload exceeds limit")?;
        let mut updates = BTreeSet::new();
        for update in &self.node_changes {
            if !scenes.contains(&update.scene_id) || !updates.insert(&update.scene_id) {
                return Err(invalid("Scene update has no unique target"));
            }
            update.validate(limits)?;
        }
        let mut asset_ids = BTreeSet::new();
        for asset in &self.asset_changes {
            if let Some(recipe) = &asset.recipe {
                recipe.validate()?;
            }
            if !asset_ids.insert(&asset.asset_id) {
                return Err(invalid("Duplicate asset update"));
            }
            if !self.leases.iter().any(|p| p.resource == LeaseResource::Asset(asset.asset_id.clone())) {
                return Err(ProjectError::new(ProjectErrorCode::LeaseLost, "Asset update requires its asset lease"));
            }
            if asset.referencing_scenes.iter().any(|scene| !scenes.contains(scene)) {
                return Err(invalid("Shared asset affected scenes are missing from targets"));
            }
        }
        Ok(())
    }
    pub fn verify_payload(&self, payload: &PublishPayload, limits: &ProjectLimits) -> ProjectResult<()> {
        self.validate(limits)?;
        let expected: BTreeSet<_> = self.planned_files.iter().filter_map(|f| f.content.as_ref().map(|c| &c.hash)).collect();
        if payload.contents.keys().any(|hash| !expected.contains(hash)) {
            return Err(invalid("Unreferenced publish upload"));
        }
        for file in &self.planned_files {
            if let Some(content) = &file.content {
                let bytes = payload
                    .contents
                    .get(&content.hash)
                    .ok_or_else(|| ProjectError::new(ProjectErrorCode::ResourceMissing, "Publish content was not uploaded"))?;
                content.verify(bytes)?;
            }
        }
        Ok(())
    }
}

impl SceneUpdate {
    pub fn validate(&self, limits: &ProjectLimits) -> ProjectResult<()> {
        limit(self.nodes.len() <= limits.max_resources as usize, "Too many GUI nodes")?;
        let mut nodes = BTreeMap::new();
        for node in &self.nodes {
            if node.id.is_empty() || node.id.len() > 512 || nodes.insert(node.id.as_str(), node).is_some() {
                return Err(invalid("Duplicate or invalid GUI node ID"));
            }
            if let Some(rotation) = node.values.get("rotation")
                && !rotation.as_array().is_some_and(|a| a.len() == 3 && a.iter().all(|v| v.as_f64().is_some_and(f64::is_finite)))
            {
                return Err(ProjectError::new(ProjectErrorCode::InvalidTransform, "rotation must be finite Euler XYZ degrees"));
            }
        }
        for node in &self.nodes {
            let mut current = node;
            let mut seen = BTreeSet::new();
            while !current.parent.is_empty() {
                if !seen.insert(current.id.as_str()) {
                    return Err(invalid("Cyclic GUI hierarchy"));
                }
                current = nodes.get(current.parent.as_str()).copied().ok_or_else(|| invalid("Missing GUI parent"))?;
                if seen.len() > 128 {
                    return Err(invalid("GUI hierarchy exceeds depth limit"));
                }
            }
        }
        Ok(())
    }
}

pub fn canonical_json(value: &serde_json::Value) -> ProjectResult<Vec<u8>> {
    fn ordered(value: &serde_json::Value, depth: usize) -> ProjectResult<serde_json::Value> {
        if depth > 128 {
            return Err(invalid("Control JSON exceeds depth limit"));
        }
        Ok(match value {
            serde_json::Value::Object(map) => {
                let mut sorted = BTreeMap::new();
                for (key, value) in map {
                    sorted.insert(key.clone(), ordered(value, depth + 1)?);
                }
                serde_json::Value::Object(sorted.into_iter().collect())
            }
            serde_json::Value::Array(values) => serde_json::Value::Array(values.iter().map(|v| ordered(v, depth + 1)).collect::<ProjectResult<_>>()?),
            other => other.clone(),
        })
    }
    serde_json::to_vec(&ordered(value, 0)?).map_err(|e| invalid(&e.to_string()))
}
