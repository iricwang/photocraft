//! Asset management is an explicit host job; canvas edits never inspect or clean game files.
use crate::{EngineError, Result, Session, commands::CommandSpec, defold_project_client as client, defold_project_model::*};
use serde_json::{Value, json};

fn engine(error: impl std::fmt::Display) -> EngineError {
    EngineError::Other(error.to_string())
}
fn enabled(session: &Session) -> std::result::Result<(), String> {
    crate::file_cmds::native(session)?;
    let state = session.active().ok_or_else(|| "Open a linked Defold GUI first".to_string())?;
    if session.project_client.bindings.contains_key(&state.open_id) { Ok(()) } else { Err("Connect this GUI to its project host first".into()) }
}
fn capture(session: &Session) -> Result<(u64, client::ClientBinding)> {
    let opening = session.active().ok_or(EngineError::NoDocument)?.open_id;
    let binding = session.project_client.bindings.get(&opening).cloned().ok_or_else(|| engine("Connect this GUI to its project host first"))?;
    Ok((opening, binding))
}
fn cache(session: &mut Session, opening: u64, snapshot: AssetManagementSnapshot) {
    if session.documents().iter().any(|state| state.open_id == opening)
        && session.project_client.bindings.get(&opening).is_some_and(|binding| binding.context == snapshot.context)
    {
        session.project_client.asset_management.insert(opening, snapshot);
    }
}
pub fn cached_context(session: &Session) -> Option<ProjectContext> {
    let opening = session.active()?.open_id;
    session.project_client.bindings.get(&opening).map(|binding| binding.context.clone())
}
pub fn cached_status(session: &Session) -> Value {
    let Some(state) = session.active() else { return json!({"available":false}) };
    let Some(binding) = session.project_client.bindings.get(&state.open_id) else { return json!({"available":false}) };
    match session.project_client.asset_management.get(&state.open_id).filter(|snapshot| snapshot.context == binding.context) {
        Some(snapshot) => json!({"available":true,"openId":state.open_id,"snapshot":snapshot}),
        None => json!({"available":false,"openId":state.open_id}),
    }
}
pub fn refresh(session: &mut Session) -> Result<Value> {
    let (opening, binding) = capture(session)?;
    crate::jobs::run(
        session,
        "Inspect project assets",
        false,
        move |job| {
            job.check()?;
            job.progress(0.1, "Read project asset ownership and retained legacy sources");
            let snapshot = binding.store.asset_management(&binding.context).map_err(engine)?;
            job.check()?;
            if snapshot.context != binding.context {
                return Err(engine("Asset report belongs to another project workspace"));
            }
            Ok(snapshot)
        },
        move |session, snapshot| {
            let result = json!({"openId":opening,"snapshot":snapshot});
            cache(session, opening, snapshot);
            Ok(result)
        },
    )
}
fn migration_request(binding: &client::ClientBinding, operation_id: OperationId, input: ResourcePath, inventory_hash: ContentHash) -> LegacyMigrationRequest {
    let mut scene = binding.scene.clone();
    scene.source_revision = binding.source_revision.clone();
    LegacyMigrationRequest { context: binding.context.clone(), client_id: binding.client_id.clone(), operation_id, scene, input, inventory_hash }
}
fn restore_migration_binding(current: &mut client::ClientBinding, recovered: client::ClientBinding, expected_source_revision: &Option<String>) {
    if current.context == recovered.context
        && current.client_id == recovered.client_id
        && current.scene.scene_id == recovered.scene.scene_id
        && current.scene.package_id == recovered.scene.package_id
        && current.scene.path == recovered.scene.path
        && current.source_revision == *expected_source_revision
    {
        // This is the acknowledged pre-migration save, including its cleared pending attempt.
        // The imported source head is deliberately never adopted by this still-open draft.
        *current = recovered;
    }
}
fn migrate_request(binding: &client::ClientBinding, request: &LegacyMigrationRequest) -> ProjectResult<crate::defold_migration::MigrationReport> {
    match binding.store.migrate_legacy(request) {
        Err(failure) if matches!(failure.code, ProjectErrorCode::Io | ProjectErrorCode::RecoveryRequired | ProjectErrorCode::SaveFailed) => {
            let report = binding.store.migration_report(&OperationRequest { context: request.context.clone(), operation_id: request.operation_id.clone() });
            if let Ok(Some(report)) = report
                && report.operation_id == request.operation_id
                && report.inventory_hash == request.inventory_hash
            {
                // A report identifies an operation but not its selected input. Replaying the
                // exact request lets the host validate that binding before reporting success.
                binding.store.migrate_legacy(request)
            } else {
                Err(failure)
            }
        }
        result => result,
    }
}
pub fn migrate(session: &mut Session, params: &Value) -> Result<Value> {
    let input =
        ResourcePath::parse(params.get("input").and_then(Value::as_str).ok_or_else(|| engine("Choose a reviewed legacy native source"))?).map_err(engine)?;
    let inventory_hash =
        ContentHash::parse(params.get("inventoryHash").and_then(Value::as_str).ok_or_else(|| engine("Refresh the legacy inventory first"))?).map_err(engine)?;
    let operation_id = match params.get("operationId") {
        None => new_id().map_err(engine)?,
        Some(value) => OperationId::parse(value.as_str().ok_or_else(|| engine("Invalid migration operation ID"))?).map_err(engine)?,
    };
    let (opening, mut binding) = capture(session)?;
    let expected_source_revision = binding.source_revision.clone();
    let worker_operation_id = operation_id.clone();
    crate::jobs::run(
        session,
        "Migrate legacy creative source",
        true,
        move |job| {
            job.check()?;
            let outcome = (|| {
                client::acquire_or_renew(&mut binding)?;
                job.check()?;
                // Acquiring also reconciles a lost save acknowledgment. Capture the CAS only
                // after that reconciliation, while keeping the caller's migration identity.
                let request = migration_request(&binding, worker_operation_id.clone(), input, inventory_hash);
                job.progress(0.1, "Back up original bytes before importing the complete native source");
                migrate_request(&binding, &request).map_err(engine)
            })();
            // Do not treat a response failure as a failure to commit. Keep the operation identity
            // available for an explicit receipt query, and keep acquired leases for normal release.
            let outcome = outcome.map_err(|failure| engine(format!("Migration {worker_operation_id}: {failure}")));
            let snapshot = outcome.as_ref().ok().and_then(|_| binding.store.asset_management(&binding.context).ok());
            Ok((binding, outcome, snapshot))
        },
        move |session, (binding, outcome, snapshot)| {
            if let Some(current) = session.project_client.bindings.get_mut(&opening) {
                restore_migration_binding(current, binding, &expected_source_revision);
            }
            if let Some(snapshot) = snapshot {
                cache(session, opening, snapshot);
            }
            let report = outcome?;
            Ok(json!({"operationId":operation_id,"migration":report,"reopenRequired":report.state==crate::defold_migration::MigrationState::SourceCommitted}))
        },
    )
}
pub fn migration_status(session: &mut Session, params: &Value) -> Result<Value> {
    let operation_id =
        OperationId::parse(params.get("operationId").and_then(Value::as_str).ok_or_else(|| engine("Choose a migration operation"))?).map_err(engine)?;
    let (opening, binding) = capture(session)?;
    crate::jobs::run(
        session,
        "Read migration receipt",
        false,
        move |job| {
            job.check()?;
            let report =
                binding.store.migration_report(&OperationRequest { context: binding.context.clone(), operation_id: operation_id.clone() }).map_err(engine)?;
            Ok((binding.context, operation_id, report))
        },
        move |session, (context, operation_id, report)| {
            if let Some(snapshot) = session.project_client.asset_management.get_mut(&opening).filter(|snapshot| snapshot.context == context)
                && let Some(report) = &report
            {
                snapshot.migrations.retain(|known| known.operation_id != report.operation_id);
                snapshot.migrations.push(report.clone());
            }
            Ok(json!({"operationId":operation_id,"migration":report}))
        },
    )
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "defold.refreshAssetManagement",
            label: "Inspect project assets",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled,
            run: |s, _| refresh(s),
            journal: false,
        },
        CommandSpec {
            id: "defold.assetManagementStatus",
            label: "Project asset management status",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: |_| Ok(()),
            run: |s, _| Ok(cached_status(s)),
            journal: false,
        },
        CommandSpec {
            id: "defold.migrateLegacySource",
            label: "Migrate legacy creative source",
            menu: &[],
            shortcut: None,
            params: "{input,inventoryHash,operationId?}",
            enabled: crate::defold_project_cmds::linked_idle,
            run: migrate,
            journal: false,
        },
        CommandSpec {
            id: "defold.migrationStatus",
            label: "Read migration receipt",
            menu: &[],
            shortcut: None,
            params: "{operationId}",
            enabled,
            run: migration_status,
            journal: false,
        },
    ]
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "defold_project_assets_cmds_tests.rs"]
mod tests;
