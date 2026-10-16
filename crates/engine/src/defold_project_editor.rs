//! Typed, loopback-only adapter. No editor-script evaluation is exposed by this transport.
use crate::defold_project_fs as disk;
use crate::defold_project_model::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

const RESPONSE_LIMIT: u64 = 32 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Discovery {
    schema_version: u32,
    port: u16,
    editor_session_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct EditorStatus {
    pub state: String,
    pub receipt: Option<Value>,
    pub error: Option<ProjectError>,
}

fn fail(code: ProjectErrorCode, message: impl Into<String>) -> ProjectError {
    ProjectError::new(code, message)
}
fn unavailable() -> ProjectError {
    fail(ProjectErrorCode::EditorUnavailable, "Open this project in a compatible Defold editor, then publish again; the creative source is preserved")
}

fn discovery(root: &Path) -> ProjectResult<Discovery> {
    let path = disk::scoped_path(root, ".internal/photocraft/editor-adapter.json", false)?;
    let discovery: Discovery = disk::read_json(&path, 16 * 1024).map_err(|_| unavailable())?;
    if discovery.schema_version != SCHEMA_VERSION || discovery.port == 0 || discovery.editor_session_id.is_empty() {
        return Err(unavailable());
    }
    Ok(discovery)
}

fn call(root: &Path, route: &str, body: &Value) -> ProjectResult<Value> {
    let endpoint = discovery(root)?;
    let token_path = disk::scoped_path(root, ".internal/editor.token", false)?;
    let token = String::from_utf8(disk::read_bounded(&token_path, 1024).map_err(|_| unavailable())?).map_err(|_| unavailable())?;
    let token = token.trim();
    if token.is_empty() || token.len() > 512 || !token.bytes().all(|c| c.is_ascii_graphic()) {
        return Err(unavailable());
    }
    let body = serde_json::to_vec(body).map_err(|e| fail(ProjectErrorCode::InvalidRequest, e.to_string()))?;
    if body.len() > 4 * 1024 * 1024 {
        return Err(fail(ProjectErrorCode::LimitExceeded, "Editor request exceeds control limit"));
    }
    let address = SocketAddr::from(([127, 0, 0, 1], endpoint.port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(900)).map_err(|_| unavailable())?;
    stream.set_read_timeout(Some(Duration::from_secs(30))).map_err(|_| unavailable())?;
    stream.set_write_timeout(Some(Duration::from_secs(5))).map_err(|_| unavailable())?;
    write!(stream, "POST {route} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).map_err(|_| interrupted())?;
    stream.write_all(&body).map_err(|_| interrupted())?;
    let mut response = Vec::new();
    stream.take(RESPONSE_LIMIT + 1).read_to_end(&mut response).map_err(|_| interrupted())?;
    if response.len() as u64 > RESPONSE_LIMIT {
        return Err(fail(ProjectErrorCode::LimitExceeded, "Editor response exceeds control limit"));
    }
    let (status, body) = parse_response(&response)?;
    let value: Value =
        serde_json::from_slice(body).map_err(|_| fail(ProjectErrorCode::ProtocolUnsupported, "Defold returned an invalid structured response"))?;
    if status != 200 {
        return Err(parse_error(value.get("error").unwrap_or(&value)));
    }
    if route == "/craft/v1/project" && value.get("editorSessionId").and_then(Value::as_str) != Some(&endpoint.editor_session_id) {
        return Err(unavailable());
    }
    Ok(value)
}

fn interrupted() -> ProjectError {
    let mut error = fail(ProjectErrorCode::Io, "Editor acknowledgement was interrupted; query the transaction before retrying");
    error.retryable = true;
    error
}

fn parse_response(bytes: &[u8]) -> ProjectResult<(u16, &[u8])> {
    let end = bytes.windows(4).position(|v| v == b"\r\n\r\n").ok_or_else(unavailable)?;
    if end > 16 * 1024 {
        return Err(unavailable());
    }
    let head = std::str::from_utf8(&bytes[..end]).map_err(|_| unavailable())?;
    let status = head.lines().next().and_then(|line| line.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).ok_or_else(unavailable)?;
    // Jetty uses a length for JSON responses. Refuse an ambiguous/chunked reply.
    let mut length = None;
    for line in head.lines().skip(1) {
        let (name, value) = line.split_once(':').ok_or_else(unavailable)?;
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(fail(ProjectErrorCode::ProtocolUnsupported, "Chunked editor responses are unsupported"));
        }
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err(unavailable());
            }
            length = Some(value.trim().parse::<usize>().map_err(|_| unavailable())?);
        }
    }
    let body = &bytes[end + 4..];
    if length.is_some_and(|length| length != body.len()) {
        return Err(interrupted());
    }
    Ok((status, body))
}

pub(crate) fn parse_error(value: &Value) -> ProjectError {
    let code = value.get("code").and_then(Value::as_str).unwrap_or("RECOVERY_REQUIRED");
    let code = serde_json::from_value::<ProjectErrorCode>(Value::String(code.into())).unwrap_or(ProjectErrorCode::RecoveryRequired);
    let mut error = fail(code, value.get("message").and_then(Value::as_str).unwrap_or("Defold could not complete this transaction"));
    error.resources = value
        .get("paths")
        .or_else(|| value.get("resources"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(8192)
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    error
}

pub(crate) fn describe(root: &Path) -> ProjectResult<EditorDescriptor> {
    let value = call(root, "/craft/v1/project", &json!({}))?;
    if value.get("schemaVersion").and_then(Value::as_u64) != Some(1) {
        return Err(unavailable());
    }
    let capabilities = value.get("capabilities").and_then(Value::as_object).ok_or_else(unavailable)?;
    for required in ["targetedSave", "publishReceipts", "rotationEulerV1", "safeResourceRefresh", "managedAtlasGraphUpdates"] {
        if capabilities.get(required) != Some(&Value::Bool(true)) {
            return Err(unavailable());
        }
    }
    Ok(EditorDescriptor {
        editor_session_id: EditorSessionId::parse(value.get("editorSessionId").and_then(Value::as_str).ok_or_else(unavailable)?).map_err(|_| unavailable())?,
        capabilities: capabilities.iter().filter_map(|(name, flag)| (flag == &Value::Bool(true)).then_some(name.clone())).collect::<BTreeSet<_>>(),
    })
}

pub(crate) fn snapshot(root: &Path, request: &SceneRequest) -> ProjectResult<SceneSnapshot> {
    let value = call(root, "/craft/v1/snapshot", &json!({"path":request.path}))?;
    decode_snapshot(value, request)
}

fn decode_snapshot(value: Value, request: &SceneRequest) -> ProjectResult<SceneSnapshot> {
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned).ok_or_else(unavailable);
    if text("path")? != request.path.as_str() {
        return Err(fail(ProjectErrorCode::TargetChanged, "Editor snapshot refers to another resource"));
    }
    let source = text("source")?;
    let scene_hash = ContentHash::parse(&text("sceneHash")?)?;
    let editor_graph_revision = text("editorGraphRevision")?;
    // The editor's semantic scene hash includes protobuf defaults. Its graph
    // revision separately fingerprints the exact text captured in this session.
    if !ContentHash::parse(&editor_graph_revision)?.verify(source.as_bytes()) {
        return Err(fail(ProjectErrorCode::HashMismatch, "Editor snapshot checksum does not match its source"));
    }
    Ok(SceneSnapshot {
        scene_id: request.scene_id.clone(),
        path: request.path.clone(),
        source,
        disk_hash: ContentHash::parse(&text("diskHash")?)?,
        scene_hash,
        editor_graph_revision,
        dirty: value.get("dirty").and_then(Value::as_bool).ok_or_else(unavailable)?,
        editor_session_id: Some(EditorSessionId::parse(&text("editorSessionId")?).map_err(|_| unavailable())?),
        dependencies: vec![],
    })
}

/// Explicit user navigation only. Publication and snapshot reads never open editor tabs.
pub(crate) fn reveal_resource(root: &Path, request: &ResourceReadRequest) -> ProjectResult<()> {
    describe(root)?;
    let reply = call(root, "/craft/v1/resource/open", &json!({"path":request.path}))?;
    if reply.get("schemaVersion").and_then(Value::as_u64) != Some(u64::from(SCHEMA_VERSION))
        || reply.get("path").and_then(Value::as_str) != Some(request.path.as_str())
        || reply.get("opened") != Some(&Value::Bool(true))
    {
        return Err(fail(ProjectErrorCode::ProtocolUnsupported, "Defold did not confirm opening the requested resource"));
    }
    Ok(())
}

fn decode_status(value: Value, plan: &PublishPlan) -> ProjectResult<EditorStatus> {
    if value.get("schemaVersion").and_then(Value::as_u64) != Some(u64::from(SCHEMA_VERSION))
        || value.get("transactionId").and_then(Value::as_str) != Some(plan.transaction_id.as_str())
        || value.get("planHash").and_then(Value::as_str) != Some(plan.plan_hash.as_str())
    {
        return Err(fail(ProjectErrorCode::IdempotencyMismatch, "Defold acknowledgement belongs to another transaction or plan"));
    }
    let state = value.get("state").and_then(Value::as_str).ok_or_else(unavailable)?.to_string();
    if !["PREPARED", "APPLYING", "APPLIED_UNSAVED", "SAVING", "COMMITTED", "ABORTED", "RECOVERY_REQUIRED"].contains(&state.as_str()) {
        return Err(unavailable());
    }
    Ok(EditorStatus {
        state,
        receipt: value.get("receipt").filter(|v| !v.is_null()).cloned(),
        error: value.get("error").filter(|v| !v.is_null()).map(parse_error),
    })
}

pub(crate) fn prepare(root: &Path, plan: &PublishPlan) -> ProjectResult<EditorStatus> {
    let editor = describe(root)?;
    let mut targets = Vec::new();
    for target in &plan.targets {
        let update = plan
            .node_changes
            .iter()
            .find(|change| change.scene_id == target.scene_id)
            .ok_or_else(|| fail(ProjectErrorCode::InvalidRequest, "Publication target has no GUI update"))?;
        targets.push(json!({"path":target.path,"expectedDiskHash":target.expected_gui_disk_hash,"expectedEditorGraphRevision":target.expected_editor_snapshot,
            "changes":{"nodes":update.nodes,"textures":update.textures,"renames":update.renames}}));
    }
    let resources: Vec<_> = plan
        .planned_files
        .iter()
        .filter(|file| file.writer != FileWriter::EditorResource || file.new_path.as_ref().is_some_and(|p| !p.as_str().ends_with(".gui")))
        .filter_map(|file| {
            file.new_path.as_ref().zip(file.content.as_ref()).map(|(path, content)| {
                json!({
                    "action":if file.action == FileAction::Replace {"replace"} else {"create"}, "path":path,
                    "expectedDiskHash":if file.action == FileAction::Move {None} else {file.expected_old_hash.as_ref()},"newContentHash":content.hash
                })
            })
        })
        .collect();
    let mut request = json!({"schemaVersion":1,"transactionId":plan.transaction_id,"planHash":plan.plan_hash,"editorSessionId":editor.editor_session_id,"targets":targets,"resourceUpdates":resources});
    // The single-scene form is retained for adapters upgraded incrementally.
    if let Some(target) = targets.first().filter(|_| targets.len() == 1)
        && let (Some(object), Some(target)) = (request.as_object_mut(), target.as_object())
    {
        object.extend(target.clone());
    }
    decode_status(call(root, "/craft/v1/publish/prepare", &request)?, plan)
}

fn operation(root: &Path, route: &str, plan: &PublishPlan) -> ProjectResult<EditorStatus> {
    decode_status(call(root, route, &json!({"transactionId":plan.transaction_id,"planHash":plan.plan_hash}))?, plan)
}
pub(crate) fn apply(root: &Path, plan: &PublishPlan) -> ProjectResult<EditorStatus> {
    operation(root, "/craft/v1/publish/apply", plan)
}
pub(crate) fn status(root: &Path, plan: &PublishPlan) -> ProjectResult<EditorStatus> {
    match operation(root, "/craft/v1/publish/status", plan) {
        Err(failure) if matches!(failure.code, ProjectErrorCode::EditorUnavailable | ProjectErrorCode::Io) => {
            // A completed editor journal survives shutdown and a lost HTTP reply.
            // The authority still verifies every saved file before committing it.
            let path = disk::scoped_path(root, &format!(".internal/photocraft/adapter-transactions/{}.json", plan.transaction_id), false)?;
            if !path.is_file() {
                return Err(failure);
            }
            let value: Value = disk::read_json(&path, RESPONSE_LIMIT)?;
            if value.get("state").and_then(Value::as_str) != Some("COMMITTED") {
                return Err(failure);
            }
            decode_status(value, plan)
        }
        outcome => outcome,
    }
}
pub(crate) fn cancel(root: &Path, plan: &PublishPlan) -> ProjectResult<EditorStatus> {
    operation(root, "/craft/v1/publish/cancel", plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_checks_exact_text_revision_separately_from_semantic_defaults() {
        let plan: PublishPlan = serde_json::from_slice(include_bytes!("../tests/fixtures/project_v1/publish-multi-target.json")).unwrap();
        let target = &plan.targets[0];
        let request = SceneRequest { context: plan.context, scene_id: target.scene_id.clone(), path: target.path.clone() };
        let source = "nodes { id: \"icon\" rotation { z: 0.0 } }";
        let semantic = ContentHash::sha256(b"canonical protobuf including default fields");
        let mut value = json!({"path":request.path,"source":source,"diskHash":ContentHash::sha256(source.as_bytes()),
            "sceneHash":semantic,"editorGraphRevision":ContentHash::sha256(source.as_bytes()).to_string(),
            "dirty":false,"editorSessionId":EditorSessionId::from_bytes([19;16])});
        assert_eq!(decode_snapshot(value.clone(), &request).unwrap().scene_hash, semantic);
        value["source"] = json!("different text");
        assert_eq!(decode_snapshot(value, &request).unwrap_err().code, ProjectErrorCode::HashMismatch);
    }
    #[test]
    fn rejects_truncated_and_ambiguous_editor_receipts() {
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n{}").is_err());
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}").is_err());
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{}").is_err());
        assert_eq!(parse_error(&json!({"code":"TARGET_DIRTY","message":"Preserve draft"})).code, ProjectErrorCode::TargetDirty);
        let plan: PublishPlan = serde_json::from_slice(include_bytes!("../tests/fixtures/project_v1/publish-multi-target.json")).unwrap();
        assert_eq!(
            decode_status(
                json!({"schemaVersion":1,"state":"PREPARED","transactionId":plan.transaction_id,"planHash":ContentHash::sha256(b"wrong plan")}),
                &plan
            )
            .unwrap_err()
            .code,
            ProjectErrorCode::IdempotencyMismatch
        );
    }
}
