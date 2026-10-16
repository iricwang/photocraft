//! Bounded project-store seam. Implementations run on background jobs, never in an egui frame.
use crate::defold_project_model::*;
use std::{
    collections::BTreeMap,
    sync::{Mutex, MutexGuard, PoisonError},
};

#[cfg(test)]
#[path = "defold_project_contract_tests.rs"]
mod tests;

pub trait ProjectStore: Send + Sync {
    fn describe(&self) -> ProjectResult<ProjectDescription>;
    /// Register/resolve scene identity by its full resource path, without editing the GUI file.
    fn resolve_scene(&self, request: &ResourceReadRequest) -> ProjectResult<SceneIdentity>;
    fn scene_identity(&self, _request: &SceneIdentityRequest) -> ProjectResult<SceneIdentity> {
        Err(ProjectError::new(ProjectErrorCode::ProtocolUnsupported, "This project host does not support scene identity lookup"))
    }
    fn shared_asset_usage(&self, _request: &SharedAssetRequest) -> ProjectResult<SharedAssetUsage> {
        Err(ProjectError::new(ProjectErrorCode::ProtocolUnsupported, "This project host does not support shared artwork references"))
    }
    fn snapshot(&self, request: &SceneRequest) -> ProjectResult<SceneSnapshot>;
    fn read_resource(&self, request: &ResourceReadRequest) -> ProjectResult<ResourceContent>;
    fn reveal_resource(&self, _request: &ResourceReadRequest) -> ProjectResult<()> {
        Err(ProjectError::new(ProjectErrorCode::ProtocolUnsupported, "This project host does not support opening resources in Defold"))
    }
    fn read_source(&self, request: &ReadSourceRequest) -> ProjectResult<SourcePackage>;
    fn read_asset_registry(&self, _context: &ProjectContext) -> ProjectResult<crate::defold_publish_plan::AssetRegistry> {
        Ok(crate::defold_publish_plan::AssetRegistry::default())
    }
    fn latest_publication(&self, _request: &SceneRequest) -> ProjectResult<Option<PublishReceipt>> {
        Ok(None)
    }
    fn published_scene_baseline(&self, _request: &SceneRequest) -> ProjectResult<Option<SceneBaseline>> {
        Ok(None)
    }
    fn asset_management(&self, _context: &ProjectContext) -> ProjectResult<AssetManagementSnapshot> {
        Err(ProjectError::new(ProjectErrorCode::ProtocolUnsupported, "This project host does not support asset management reports"))
    }
    fn migrate_legacy(&self, _request: &LegacyMigrationRequest) -> ProjectResult<crate::defold_migration::MigrationReport> {
        Err(ProjectError::new(ProjectErrorCode::ProtocolUnsupported, "This project host does not support legacy migration"))
    }
    fn migration_report(&self, _request: &OperationRequest) -> ProjectResult<Option<crate::defold_migration::MigrationReport>> {
        Err(ProjectError::new(ProjectErrorCode::ProtocolUnsupported, "This project host does not support migration receipts"))
    }
    fn save_source(&self, request: &SaveSourceRequest, payload: &SourcePayload) -> ProjectResult<SourceSaveReceipt>;
    fn operation_status(&self, request: &OperationRequest) -> ProjectResult<Option<SourceSaveReceipt>>;
    fn acquire_lease(&self, request: &LeaseRequest) -> ProjectResult<LeaseGrant>;
    fn renew_lease(&self, request: &LeaseRenewRequest) -> ProjectResult<LeaseGrant>;
    fn release_lease(&self, request: &LeaseReleaseRequest) -> ProjectResult<()>;
    fn prepare_publish(&self, plan: &PublishPlan, payload: &PublishPayload) -> ProjectResult<PublishReceipt>;
    fn commit_publish(&self, request: &PublishRequest) -> ProjectResult<PublishReceipt>;
    fn publish_status(&self, request: &PublishRequest) -> ProjectResult<Option<PublishReceipt>>;
    fn cancel_publish(&self, request: &PublishRequest) -> ProjectResult<PublishReceipt>;
}

/// In-memory protocol oracle for integration tests. This is not a durable production store.
pub struct FakeStore {
    state: Mutex<FakeState>,
}
struct FakePublication {
    plan: PublishPlan,
    receipt: PublishReceipt,
}
struct FakeState {
    description: ProjectDescription,
    now_ms: u64,
    next_lease_epoch: u64,
    leases: BTreeMap<LeaseResource, LeaseGrant>,
    heads: BTreeMap<PackageId, String>,
    sources: BTreeMap<(PackageId, String), SourcePackage>,
    operations: BTreeMap<OperationId, (ContentHash, SourceSaveReceipt)>,
    scenes: BTreeMap<SceneId, SceneSnapshot>,
    identities: BTreeMap<ResourcePath, SceneIdentity>,
    resources: BTreeMap<ResourcePath, ResourceContent>,
    publications: BTreeMap<TransactionId, FakePublication>,
    failure: Option<FailurePoint>,
}
impl FakeStore {
    pub fn new(description: ProjectDescription) -> Self {
        Self {
            state: Mutex::new(FakeState {
                description,
                now_ms: 0,
                next_lease_epoch: 1,
                leases: BTreeMap::new(),
                heads: BTreeMap::new(),
                sources: BTreeMap::new(),
                operations: BTreeMap::new(),
                scenes: BTreeMap::new(),
                identities: BTreeMap::new(),
                resources: BTreeMap::new(),
                publications: BTreeMap::new(),
                failure: None,
            }),
        }
    }
    fn state(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
    pub fn advance_time(&self, milliseconds: u64) {
        let mut state = self.state();
        state.now_ms = state.now_ms.saturating_add(milliseconds);
    }
    pub fn inject_failure(&self, point: FailurePoint) {
        self.state().failure = Some(point);
    }
    pub fn seed_snapshot(&self, snapshot: SceneSnapshot) {
        self.state().scenes.insert(snapshot.scene_id.clone(), snapshot);
    }
    pub fn seed_resource(&self, content: ResourceContent) {
        self.state().resources.insert(content.version.path.clone(), content);
    }
    /// Restart invalidates leases but keeps acknowledged source and publication records.
    pub fn restart(&self) {
        self.state().leases.clear();
    }
    pub fn set_workspace(&self, context: ProjectContext) {
        let mut state = self.state();
        state.description.context = context;
        state.leases.clear();
    }
}
fn check_context(actual: &ProjectContext, requested: &ProjectContext) -> ProjectResult<()> {
    if actual.project_id != requested.project_id || actual.checkout_id != requested.checkout_id {
        Err(ProjectError::new(ProjectErrorCode::ProjectMismatch, "This is another project or authority checkout"))
    } else if actual.workspace_id != requested.workspace_id || actual.workspace_epoch != requested.workspace_epoch {
        Err(ProjectError::new(ProjectErrorCode::WorkspaceChanged, "The authority workspace changed"))
    } else {
        Ok(())
    }
}
fn fault(state: &mut FakeState, point: FailurePoint) -> ProjectResult<()> {
    if state.failure == Some(point) {
        state.failure = None;
        Err(ProjectError::new(ProjectErrorCode::Io, format!("Injected failure: {point:?}")))
    } else {
        Ok(())
    }
}
fn check_leases(state: &FakeState, client: &ClientId, proofs: &[LeaseProof]) -> ProjectResult<()> {
    for proof in proofs {
        if !state.leases.get(&proof.resource).is_some_and(|lease| lease.client_id == *client && lease.proof == *proof && lease.expires_at_ms > state.now_ms) {
            return Err(ProjectError::new(ProjectErrorCode::LeaseLost, "Lease is expired, belongs to another client, or has an old epoch"));
        }
    }
    Ok(())
}
fn duration(milliseconds: u64) -> ProjectResult<()> {
    if (1_000..=300_000).contains(&milliseconds) {
        Ok(())
    } else {
        Err(ProjectError::new(ProjectErrorCode::InvalidRequest, "Lease duration must be 1000..300000 milliseconds"))
    }
}
fn transaction_owns(state: &FakeState, resource: &LeaseResource) -> bool {
    state.publications.values().any(|p| !p.receipt.phase.is_terminal() && p.plan.leases.iter().any(|lease| &lease.resource == resource))
}
impl ProjectStore for FakeStore {
    fn describe(&self) -> ProjectResult<ProjectDescription> {
        Ok(self.state().description.clone())
    }
    fn resolve_scene(&self, request: &ResourceReadRequest) -> ProjectResult<SceneIdentity> {
        let mut state = self.state();
        check_context(&state.description.context, &request.context)?;
        if !request.path.as_str().ends_with(".gui") {
            return Err(ProjectError::new(ProjectErrorCode::PathInvalid, "Scene path must identify a GUI resource"));
        }
        let snapshot = state.scenes.values().find(|scene| scene.path.case_key() == request.path.case_key());
        let resource = state.resources.values().find(|resource| resource.version.path.case_key() == request.path.case_key());
        let hash = snapshot
            .map(|s| &s.disk_hash)
            .or_else(|| resource.map(|r| &r.version.content.hash))
            .ok_or_else(|| ProjectError::new(ProjectErrorCode::ResourceMissing, "GUI resource does not exist"))?;
        if request.expected_hash.as_ref().is_some_and(|expected| expected != hash) {
            return Err(ProjectError::new(ProjectErrorCode::TargetChanged, "GUI changed before identity registration"));
        }
        if let Some(identity) = state.identities.values().find(|identity| identity.path.case_key() == request.path.case_key()) {
            let mut identity = identity.clone();
            identity.source_revision = state.heads.get(&identity.package_id).cloned();
            return Ok(identity);
        }
        let scene_id = if let Some(snapshot) = snapshot { snapshot.scene_id.clone() } else { new_id()? };
        let identity = SceneIdentity { scene_id, path: request.path.clone(), package_id: new_id()?, source_revision: None };
        state.identities.insert(identity.path.clone(), identity.clone());
        Ok(identity)
    }
    fn snapshot(&self, request: &SceneRequest) -> ProjectResult<SceneSnapshot> {
        let state = self.state();
        check_context(&state.description.context, &request.context)?;
        let scene = state.scenes.get(&request.scene_id).ok_or_else(|| ProjectError::new(ProjectErrorCode::ResourceMissing, "Scene is not registered"))?;
        if scene.path != request.path {
            return Err(ProjectError::new(ProjectErrorCode::TargetChanged, "Scene path changed"));
        }
        Ok(scene.clone())
    }
    fn read_resource(&self, request: &ResourceReadRequest) -> ProjectResult<ResourceContent> {
        let state = self.state();
        check_context(&state.description.context, &request.context)?;
        let resource = state.resources.get(&request.path).ok_or_else(|| ProjectError::new(ProjectErrorCode::ResourceMissing, "Resource is missing"))?;
        if request.expected_hash.as_ref().is_some_and(|hash| hash != &resource.version.content.hash) {
            return Err(ProjectError::new(ProjectErrorCode::TargetChanged, "Resource changed since its manifest"));
        }
        Ok(resource.clone())
    }
    fn read_source(&self, request: &ReadSourceRequest) -> ProjectResult<SourcePackage> {
        let state = self.state();
        check_context(&state.description.context, &request.context)?;
        let revision = request
            .source_revision
            .as_ref()
            .or_else(|| state.heads.get(&request.package_id))
            .ok_or_else(|| ProjectError::new(ProjectErrorCode::ResourceMissing, "Source package has no head"))?;
        state
            .sources
            .get(&(request.package_id.clone(), revision.clone()))
            .cloned()
            .ok_or_else(|| ProjectError::new(ProjectErrorCode::ResourceMissing, "Source revision is missing"))
    }
    fn save_source(&self, request: &SaveSourceRequest, payload: &SourcePayload) -> ProjectResult<SourceSaveReceipt> {
        let mut state = self.state();
        check_context(&state.description.context, &request.context)?;
        request.validate(&state.description.limits)?;
        request.envelope.verify_payload(payload, &state.description.limits)?;
        let value = serde_json::to_value(request).map_err(|e| ProjectError::new(ProjectErrorCode::InvalidRequest, e.to_string()))?;
        let fingerprint = ContentHash::sha256(&canonical_json(&value)?);
        if let Some((previous, receipt)) = state.operations.get(&request.operation_id) {
            if previous != &fingerprint {
                return Err(ProjectError::new(ProjectErrorCode::IdempotencyMismatch, "operationId was already used for different input"));
            }
            return Ok(receipt.clone());
        }
        check_leases(&state, &request.client_id, &request.leases)?;
        if request.leases.iter().any(|proof| transaction_owns(&state, &proof.resource)) {
            return Err(ProjectError::new(ProjectErrorCode::Busy, "A publication holds the source transaction lock"));
        }
        if state.heads.get(&request.package_id) != request.expected_source_revision.as_ref() {
            return Err(ProjectError::new(ProjectErrorCode::SourceConflict, "Source head changed"));
        }
        let key = (request.package_id.clone(), request.envelope.source_revision.clone());
        if state.sources.contains_key(&key) {
            return Err(ProjectError::new(ProjectErrorCode::SourceConflict, "An immutable source revision cannot be replaced"));
        }
        for shared in &request.envelope.draft.shared_sources {
            if !state.sources.contains_key(&(shared.package_id.clone(), shared.source_revision.clone())) {
                return Err(ProjectError::new(ProjectErrorCode::ResourceMissing, "Referenced shared source revision is missing"));
            }
        }
        photocraft_format::load_from_bytes(&payload.native).map_err(|e| ProjectError::new(ProjectErrorCode::InvalidRequest, e.to_string()))?;
        fault(&mut state, FailurePoint::SourceBeforeWrite)?;
        fault(&mut state, FailurePoint::SourceAfterObjects)?;
        let receipt = SourceSaveReceipt {
            schema_version: SCHEMA_VERSION,
            operation_id: request.operation_id.clone(),
            context: request.context.clone(),
            package_id: request.package_id.clone(),
            source_revision: request.envelope.source_revision.clone(),
            draft_revision: request.envelope.draft_revision.clone(),
            captured_native_revision: request.captured_native_revision,
            content: request.envelope.native.clone(),
        };
        state.sources.insert(key, SourcePackage { envelope: request.envelope.clone(), payload: payload.clone() });
        state.heads.insert(request.package_id.clone(), receipt.source_revision.clone());
        state.operations.insert(request.operation_id.clone(), (fingerprint, receipt.clone()));
        // Models a committed request whose response is lost. The caller must query status.
        fault(&mut state, FailurePoint::SourceAfterCommit)?;
        Ok(receipt)
    }
    fn operation_status(&self, request: &OperationRequest) -> ProjectResult<Option<SourceSaveReceipt>> {
        let state = self.state();
        check_context(&state.description.context, &request.context)?;
        Ok(state.operations.get(&request.operation_id).map(|(_, receipt)| receipt.clone()))
    }
    fn acquire_lease(&self, request: &LeaseRequest) -> ProjectResult<LeaseGrant> {
        duration(request.duration_ms)?;
        let mut state = self.state();
        check_context(&state.description.context, &request.context)?;
        if transaction_owns(&state, &request.resource) {
            return Err(ProjectError::new(ProjectErrorCode::Busy, "A publication holds this resource"));
        }
        if let Some(current) = state.leases.get(&request.resource).filter(|lease| lease.expires_at_ms > state.now_ms) {
            if current.client_id != request.client_id {
                return Err(ProjectError::new(ProjectErrorCode::Busy, "Another client is editing this resource"));
            }
            return Ok(current.clone());
        }
        let epoch = state.next_lease_epoch;
        state.next_lease_epoch = epoch.checked_add(1).ok_or_else(|| ProjectError::new(ProjectErrorCode::LimitExceeded, "Lease epoch exhausted"))?;
        let grant = LeaseGrant {
            client_id: request.client_id.clone(),
            proof: LeaseProof { resource: request.resource.clone(), lease_epoch: epoch },
            expires_at_ms: state.now_ms.saturating_add(request.duration_ms),
        };
        state.leases.insert(request.resource.clone(), grant.clone());
        Ok(grant)
    }
    fn renew_lease(&self, request: &LeaseRenewRequest) -> ProjectResult<LeaseGrant> {
        duration(request.duration_ms)?;
        let mut state = self.state();
        check_context(&state.description.context, &request.context)?;
        check_leases(&state, &request.client_id, std::slice::from_ref(&request.proof))?;
        let grant =
            LeaseGrant { client_id: request.client_id.clone(), proof: request.proof.clone(), expires_at_ms: state.now_ms.saturating_add(request.duration_ms) };
        state.leases.insert(grant.proof.resource.clone(), grant.clone());
        Ok(grant)
    }
    fn release_lease(&self, request: &LeaseReleaseRequest) -> ProjectResult<()> {
        let mut state = self.state();
        check_context(&state.description.context, &request.context)?;
        check_leases(&state, &request.client_id, std::slice::from_ref(&request.proof))?;
        if transaction_owns(&state, &request.proof.resource) {
            return Err(ProjectError::new(ProjectErrorCode::Busy, "A publication owns this lease"));
        }
        state.leases.remove(&request.proof.resource);
        Ok(())
    }
    fn prepare_publish(&self, plan: &PublishPlan, payload: &PublishPayload) -> ProjectResult<PublishReceipt> {
        let mut state = self.state();
        check_context(&state.description.context, &plan.context)?;
        plan.verify_payload(payload, &state.description.limits)?;
        if let Some(previous) = state.publications.get(&plan.transaction_id) {
            if previous.plan.plan_hash != plan.plan_hash {
                return Err(ProjectError::new(ProjectErrorCode::IdempotencyMismatch, "transactionId was reused with another plan"));
            }
            return Ok(previous.receipt.clone());
        }
        if state.description.editor.is_none() {
            return Err(ProjectError::new(ProjectErrorCode::EditorUnavailable, "No Defold adapter is registered"));
        }
        check_leases(&state, &plan.client_id, &plan.leases)?;
        if plan.leases.iter().any(|proof| transaction_owns(&state, &proof.resource)) {
            return Err(ProjectError::new(ProjectErrorCode::Busy, "Another publication owns a target"));
        }
        for source in &plan.source_revisions {
            if !state.sources.contains_key(&(source.package_id.clone(), source.source_revision.clone())) {
                return Err(ProjectError::new(ProjectErrorCode::ResourceMissing, "Published source revision is missing"));
            }
        }
        for target in &plan.targets {
            let current =
                state.scenes.get(&target.scene_id).ok_or_else(|| ProjectError::new(ProjectErrorCode::ResourceMissing, "Publish target is missing"))?;
            if current.dirty {
                return Err(ProjectError::new(ProjectErrorCode::TargetDirty, "Target has uncoordinated changes"));
            }
            if current.path != target.path
                || current.disk_hash != target.expected_gui_disk_hash
                || current.editor_graph_revision != target.expected_editor_snapshot
            {
                return Err(ProjectError::new(ProjectErrorCode::TargetChanged, "Publish target changed"));
            }
        }
        fault(&mut state, FailurePoint::Prepare)?;
        let receipt = PublishReceipt {
            schema_version: SCHEMA_VERSION,
            transaction_id: plan.transaction_id.clone(),
            context: plan.context.clone(),
            phase: PublishPhase::Prepared,
            plan_hash: plan.plan_hash.clone(),
            source_revisions: plan.source_revisions.clone(),
            targets: vec![],
            files: vec![],
            retained: vec![],
            error: None,
        };
        state.publications.insert(plan.transaction_id.clone(), FakePublication { plan: plan.clone(), receipt: receipt.clone() });
        Ok(receipt)
    }
    fn commit_publish(&self, request: &PublishRequest) -> ProjectResult<PublishReceipt> {
        let state = self.state();
        check_context(&state.description.context, &request.context)?;
        let existing = state
            .publications
            .get(&request.transaction_id)
            .ok_or_else(|| ProjectError::new(ProjectErrorCode::ResourceMissing, "Transaction has not been prepared"))?;
        if existing.receipt.phase == PublishPhase::Committed {
            return Ok(existing.receipt.clone());
        }
        if existing.receipt.phase != PublishPhase::Prepared {
            return Err(ProjectError::new(ProjectErrorCode::RecoveryRequired, "Transaction cannot be blindly replayed"));
        }
        // FakeStore deliberately cannot claim editor persistence. Tests inject a verified adapter receipt.
        Err(ProjectError::new(ProjectErrorCode::EditorUnavailable, "FakeStore needs complete_publication with a verified fake-editor receipt"))
    }
    fn publish_status(&self, request: &PublishRequest) -> ProjectResult<Option<PublishReceipt>> {
        let state = self.state();
        check_context(&state.description.context, &request.context)?;
        Ok(state.publications.get(&request.transaction_id).map(|p| p.receipt.clone()))
    }
    fn cancel_publish(&self, request: &PublishRequest) -> ProjectResult<PublishReceipt> {
        let mut state = self.state();
        check_context(&state.description.context, &request.context)?;
        let publication = state
            .publications
            .get_mut(&request.transaction_id)
            .ok_or_else(|| ProjectError::new(ProjectErrorCode::ResourceMissing, "Transaction has not been prepared"))?;
        if publication.receipt.phase == PublishPhase::Aborted {
            return Ok(publication.receipt.clone());
        }
        if !publication.receipt.phase.may_cancel() {
            return Err(ProjectError::new(ProjectErrorCode::RecoveryRequired, "Formal changes already began; query or recover the transaction"));
        }
        publication.receipt.phase = PublishPhase::Aborted;
        Ok(publication.receipt.clone())
    }
}

impl FakeStore {
    /// Tests model actual adapter steps explicitly; no method invents successful GUI persistence.
    pub fn complete_publication(&self, receipt: PublishReceipt) -> ProjectResult<()> {
        let mut state = self.state();
        check_context(&state.description.context, &receipt.context)?;
        let publication =
            state.publications.get_mut(&receipt.transaction_id).ok_or_else(|| ProjectError::new(ProjectErrorCode::ResourceMissing, "Unknown transaction"))?;
        if receipt.plan_hash != publication.plan.plan_hash || !publication.receipt.phase.allows(receipt.phase) {
            return Err(ProjectError::new(ProjectErrorCode::InvalidRequest, "Invalid adapter phase or plan hash"));
        }
        if receipt.phase == PublishPhase::Committed
            && (receipt.error.is_some()
                || receipt.targets.len() != publication.plan.targets.len()
                || publication
                    .plan
                    .targets
                    .iter()
                    .any(|target| !receipt.targets.iter().any(|saved| saved.scene_id == target.scene_id && saved.path == target.path)))
        {
            return Err(ProjectError::new(ProjectErrorCode::SaveFailed, "Committed receipt does not cover every target"));
        }
        publication.receipt = receipt;
        Ok(())
    }
}
