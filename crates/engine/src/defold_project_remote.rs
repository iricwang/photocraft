//! Same store contract across a paired TLS connection; binary source bodies stay binary.
use crate::defold_project_model::*;
use crate::defold_project_net::{self as net, HostTicket, Response};
use crate::defold_project_store::ProjectStore;
use crate::defold_project_wire as wire;
use serde::{Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;

pub struct RemoteProjectStore {
    ticket: HostTicket,
}
impl RemoteProjectStore {
    pub fn new(ticket: HostTicket) -> Self {
        Self { ticket }
    }
    fn call(&self, path: &str, bytes: &[u8]) -> ProjectResult<Vec<u8>> {
        net::call(&self.ticket, path, bytes).map_err(|message| {
            serde_json::from_str(&message).unwrap_or_else(|_| {
                let mut error = ProjectError::new(ProjectErrorCode::Io, message);
                error.retryable = true;
                error
            })
        })
    }
    fn json<T: Serialize, R: DeserializeOwned>(&self, path: &str, input: &T) -> ProjectResult<R> {
        let bytes = serde_json::to_vec(input).map_err(invalid)?;
        if bytes.len() > wire::METADATA_LIMIT {
            return Err(ProjectError::new(ProjectErrorCode::LimitExceeded, "Project control request exceeds limit"));
        }
        let result = self.call(path, &bytes)?;
        decode_control(&result, &ProjectLimits::default())
    }
}
fn invalid(error: impl std::fmt::Display) -> ProjectError {
    ProjectError::new(ProjectErrorCode::InvalidRequest, error.to_string())
}
fn source_transfer(metadata: impl Serialize, payload: &SourcePayload) -> ProjectResult<Vec<u8>> {
    let dependencies = payload.dependencies.iter().map(|(hash, bytes)| (hash.to_string(), bytes.clone())).collect();
    wire::encode(serde_json::to_value(metadata).map_err(invalid)?, &payload.native, &dependencies).map_err(invalid)
}
fn source_payload(transfer: wire::Transfer) -> ProjectResult<(serde_json::Value, SourcePayload)> {
    let dependencies =
        transfer.dependencies.into_iter().map(|(hash, bytes)| ContentHash::parse(&hash).map(|hash| (hash, bytes))).collect::<ProjectResult<_>>()?;
    Ok((transfer.metadata, SourcePayload { native: transfer.native, dependencies }))
}

impl ProjectStore for RemoteProjectStore {
    fn describe(&self) -> ProjectResult<ProjectDescription> {
        self.json("/v1/describe", &serde_json::json!({}))
    }
    fn resolve_scene(&self, request: &ResourceReadRequest) -> ProjectResult<SceneIdentity> {
        self.json("/v1/scene/resolve", request)
    }
    fn scene_identity(&self, request: &SceneIdentityRequest) -> ProjectResult<SceneIdentity> {
        let identity: SceneIdentity = self.json("/v1/scene/identity", request)?;
        if identity.scene_id != request.scene_id {
            return Err(ProjectError::new(ProjectErrorCode::TargetChanged, "Scene response belongs to another GUI"));
        }
        Ok(identity)
    }
    fn shared_asset_usage(&self, request: &SharedAssetRequest) -> ProjectResult<SharedAssetUsage> {
        let usage: SharedAssetUsage = self.json("/v1/asset/usage", request)?;
        if usage.asset.asset_id != request.asset_id || usage.consumers.len() > 256 || usage.source_refs.len() > 256 {
            return Err(ProjectError::new(ProjectErrorCode::InvalidRequest, "Shared asset response has a different identity or exceeds the target limit"));
        }
        Ok(usage)
    }
    fn snapshot(&self, request: &SceneRequest) -> ProjectResult<SceneSnapshot> {
        self.json("/v1/scene/snapshot", request)
    }
    fn read_resource(&self, request: &ResourceReadRequest) -> ProjectResult<ResourceContent> {
        let bytes = self.call("/v1/resource/read", &serde_json::to_vec(request).map_err(invalid)?)?;
        let transfer = wire::decode(&bytes).map_err(invalid)?;
        if !transfer.dependencies.is_empty() {
            return Err(invalid("Unexpected resource dependencies"));
        }
        let version: ResourceVersion = serde_json::from_value(transfer.metadata).map_err(invalid)?;
        version.content.verify(&transfer.native)?;
        Ok(ResourceContent { version, bytes: transfer.native })
    }
    fn reveal_resource(&self, request: &ResourceReadRequest) -> ProjectResult<()> {
        self.json("/v1/resource/reveal", request)
    }
    fn read_source(&self, request: &ReadSourceRequest) -> ProjectResult<SourcePackage> {
        let bytes = self.call("/v1/source/read", &serde_json::to_vec(request).map_err(invalid)?)?;
        let (metadata, payload) = source_payload(wire::decode(&bytes).map_err(invalid)?)?;
        let envelope: SourceEnvelope = serde_json::from_value(metadata).map_err(invalid)?;
        if envelope.package_id != request.package_id || request.source_revision.as_ref().is_some_and(|revision| revision != &envelope.source_revision) {
            return Err(ProjectError::new(ProjectErrorCode::TargetChanged, "Source response belongs to another package or revision"));
        }
        envelope.verify_payload(&payload, &ProjectLimits::default())?;
        Ok(SourcePackage { envelope, payload })
    }
    fn save_source(&self, request: &SaveSourceRequest, payload: &SourcePayload) -> ProjectResult<SourceSaveReceipt> {
        let bytes = self.call("/v1/source/save", &source_transfer(request, payload)?)?;
        decode_control(&bytes, &ProjectLimits::default())
    }
    fn operation_status(&self, request: &OperationRequest) -> ProjectResult<Option<SourceSaveReceipt>> {
        self.json("/v1/operation/status", request)
    }
    fn read_asset_registry(&self, context: &ProjectContext) -> ProjectResult<crate::defold_publish_plan::AssetRegistry> {
        self.json("/v1/asset/registry", context)
    }
    fn asset_management(&self, context: &ProjectContext) -> ProjectResult<AssetManagementSnapshot> {
        let snapshot: AssetManagementSnapshot = self.json("/v1/asset/management", context)?;
        if &snapshot.context != context || snapshot.schema_version != SCHEMA_VERSION {
            return Err(ProjectError::new(ProjectErrorCode::ProjectMismatch, "Asset report belongs to another project workspace"));
        }
        Ok(snapshot)
    }
    fn migrate_legacy(&self, request: &LegacyMigrationRequest) -> ProjectResult<crate::defold_migration::MigrationReport> {
        self.json("/v1/migration/run", request)
    }
    fn migration_report(&self, request: &OperationRequest) -> ProjectResult<Option<crate::defold_migration::MigrationReport>> {
        self.json("/v1/migration/status", request)
    }
    fn latest_publication(&self, request: &SceneRequest) -> ProjectResult<Option<PublishReceipt>> {
        self.json("/v1/publish/latest", request)
    }
    fn published_scene_baseline(&self, request: &SceneRequest) -> ProjectResult<Option<SceneBaseline>> {
        let baseline: Option<SceneBaseline> = self.json("/v1/publish/baseline", request)?;
        if baseline.as_ref().is_some_and(|baseline| baseline.scene_id != request.scene_id || baseline.path != request.path) {
            return Err(ProjectError::new(ProjectErrorCode::TargetChanged, "Published baseline belongs to another GUI"));
        }
        Ok(baseline)
    }
    fn acquire_lease(&self, request: &LeaseRequest) -> ProjectResult<LeaseGrant> {
        self.json("/v1/lease/acquire", request)
    }
    fn renew_lease(&self, request: &LeaseRenewRequest) -> ProjectResult<LeaseGrant> {
        self.json("/v1/lease/renew", request)
    }
    fn release_lease(&self, request: &LeaseReleaseRequest) -> ProjectResult<()> {
        self.json("/v1/lease/release", request)
    }
    fn prepare_publish(&self, plan: &PublishPlan, payload: &PublishPayload) -> ProjectResult<PublishReceipt> {
        let dependencies = payload.contents.iter().map(|(hash, bytes)| (hash.to_string(), bytes.clone())).collect();
        let bytes = wire::encode(serde_json::to_value(plan).map_err(invalid)?, &[], &dependencies).map_err(invalid)?;
        decode_control(&self.call("/v1/publish/prepare", &bytes)?, &ProjectLimits::default())
    }
    fn commit_publish(&self, request: &PublishRequest) -> ProjectResult<PublishReceipt> {
        self.json("/v1/publish/commit", request)
    }
    fn publish_status(&self, request: &PublishRequest) -> ProjectResult<Option<PublishReceipt>> {
        self.json("/v1/publish/status", request)
    }
    fn cancel_publish(&self, request: &PublishRequest) -> ProjectResult<PublishReceipt> {
        self.json("/v1/publish/cancel", request)
    }
}

/// The standalone host and transport tests use exactly this router. It cannot
/// select a host directory or evaluate client code: its store is already scoped.
pub fn dispatch(store: &dyn ProjectStore, route: &str, bytes: &[u8]) -> Result<Response, String> {
    let result = route_request(store, route, bytes);
    match result {
        Ok(response) => Ok(response),
        Err(error) => Response::json(409, &error),
    }
}
fn route_request(store: &dyn ProjectStore, route: &str, bytes: &[u8]) -> ProjectResult<Response> {
    let limits = store.describe()?.limits;
    let json = |value: serde_json::Value| Response::json(200, &value).map_err(invalid);
    match route {
        "/v1/describe" => {
            let _: serde_json::Value = decode_control(bytes, &limits)?;
            json(serde_json::to_value(store.describe()?).map_err(invalid)?)
        }
        "/v1/scene/resolve" => json(serde_json::to_value(store.resolve_scene(&decode_control(bytes, &limits)?)?).map_err(invalid)?),
        "/v1/scene/identity" => json(serde_json::to_value(store.scene_identity(&decode_control(bytes, &limits)?)?).map_err(invalid)?),
        "/v1/scene/snapshot" => json(serde_json::to_value(store.snapshot(&decode_control(bytes, &limits)?)?).map_err(invalid)?),
        "/v1/resource/read" => {
            let resource = store.read_resource(&decode_control(bytes, &limits)?)?;
            Ok(Response::binary(wire::encode(serde_json::to_value(resource.version).map_err(invalid)?, &resource.bytes, &BTreeMap::new()).map_err(invalid)?))
        }
        "/v1/resource/reveal" => {
            store.reveal_resource(&decode_control(bytes, &limits)?)?;
            json(serde_json::Value::Null)
        }
        "/v1/source/read" => {
            let source = store.read_source(&decode_control(bytes, &limits)?)?;
            Ok(Response::binary(source_transfer(source.envelope, &source.payload)?))
        }
        "/v1/source/save" => {
            let (metadata, payload) = source_payload(wire::decode(bytes).map_err(invalid)?)?;
            let request: SaveSourceRequest = serde_json::from_value(metadata).map_err(invalid)?;
            json(serde_json::to_value(store.save_source(&request, &payload)?).map_err(invalid)?)
        }
        "/v1/operation/status" => json(serde_json::to_value(store.operation_status(&decode_control(bytes, &limits)?)?).map_err(invalid)?),
        "/v1/asset/registry" => json(serde_json::to_value(store.read_asset_registry(&decode_control(bytes, &limits)?)?).map_err(invalid)?),
        "/v1/asset/usage" => json(serde_json::to_value(store.shared_asset_usage(&decode_control(bytes, &limits)?)?).map_err(invalid)?),
        "/v1/asset/management" => json(serde_json::to_value(store.asset_management(&decode_control(bytes, &limits)?)?).map_err(invalid)?),
        "/v1/migration/run" => json(serde_json::to_value(store.migrate_legacy(&decode_control(bytes, &limits)?)?).map_err(invalid)?),
        "/v1/migration/status" => json(serde_json::to_value(store.migration_report(&decode_control(bytes, &limits)?)?).map_err(invalid)?),
        "/v1/publish/latest" => json(serde_json::to_value(store.latest_publication(&decode_control(bytes, &limits)?)?).map_err(invalid)?),
        "/v1/publish/baseline" => json(serde_json::to_value(store.published_scene_baseline(&decode_control(bytes, &limits)?)?).map_err(invalid)?),
        "/v1/lease/acquire" => json(serde_json::to_value(store.acquire_lease(&decode_control(bytes, &limits)?)?).map_err(invalid)?),
        "/v1/lease/renew" => json(serde_json::to_value(store.renew_lease(&decode_control(bytes, &limits)?)?).map_err(invalid)?),
        "/v1/lease/release" => {
            store.release_lease(&decode_control(bytes, &limits)?)?;
            json(serde_json::Value::Null)
        }
        "/v1/publish/prepare" => {
            let (metadata, payload) = source_payload(wire::decode(bytes).map_err(invalid)?)?;
            if !payload.native.is_empty() {
                return Err(invalid("Publish binary data must be addressed by its digest"));
            }
            let plan: PublishPlan = serde_json::from_value(metadata).map_err(invalid)?;
            json(serde_json::to_value(store.prepare_publish(&plan, &PublishPayload { contents: payload.dependencies })?).map_err(invalid)?)
        }
        "/v1/publish/commit" => json(serde_json::to_value(store.commit_publish(&decode_control(bytes, &limits)?)?).map_err(invalid)?),
        "/v1/publish/status" => json(serde_json::to_value(store.publish_status(&decode_control(bytes, &limits)?)?).map_err(invalid)?),
        "/v1/publish/cancel" => json(serde_json::to_value(store.cancel_publish(&decode_control(bytes, &limits)?)?).map_err(invalid)?),
        _ => Err(invalid("Unknown project operation")),
    }
}
