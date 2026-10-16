//! Authenticated editor-script transport. Tokens never enter inspection or logs.
#[cfg(not(target_arch = "wasm32"))]
use super::defold_cmds::resource;
use super::defold_cmds::{GuiDocument, err};
use crate::Result;
use photocraft_defold_format::Scene;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteNode {
    pub id: String,
    pub parent: String,
    pub properties: BTreeMap<String, Value>,
    #[serde(deserialize_with = "vec_or_empty")]
    pub writable: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: String,
    pub path: String,
    #[serde(deserialize_with = "vec_or_empty")]
    pub nodes: Vec<RemoteNode>,
}

/// Accept JSON roundoff and the exact protobuf-float value of the prior snapshot
/// after Defold saves and reloads it. This is directional: rounding both sides
/// would hide real editor changes within a float cell.
pub fn same_nodes(a: &[RemoteNode], b: &[RemoteNode]) -> bool {
    fn value_eq(a: &Value, b: &Value, depth: usize) -> bool {
        if depth > 128 {
            return false;
        }
        match (a, b) {
            (Value::Number(a), Value::Number(b)) => match (a.as_f64(), b.as_f64()) {
                (Some(current), Some(expected)) => {
                    current == expected
                        || (current.is_finite()
                            && expected.is_finite()
                            && ((current - expected).abs() <= 4.0 * f64::EPSILON * current.abs().max(expected.abs()).max(1.0)
                                || (b.is_f64() && (expected as f32).is_finite() && current == f64::from(expected as f32))))
                }
                _ => a == b,
            },
            (Value::Array(a), Value::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(a, b)| value_eq(a, b, depth + 1)),
            (Value::Object(a), Value::Object(b)) => a.len() == b.len() && a.iter().all(|(k, a)| b.get(k).is_some_and(|b| value_eq(a, b, depth + 1))),
            _ => a == b,
        }
    }
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.id == b.id
                && a.parent == b.parent
                && a.writable == b.writable
                && a.properties.len() == b.properties.len()
                && a.properties.iter().all(|(k, a)| b.properties.get(k).is_some_and(|b| value_eq(a, b, 0)))
        })
}
fn vec_or_empty<'de, D, T>(d: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum List<T> {
        Items(Vec<T>),
        Empty(BTreeMap<String, Value>),
    }
    match List::deserialize(d)? {
        List::Items(v) => Ok(v),
        List::Empty(v) if v.is_empty() => Ok(vec![]),
        _ => Err(serde::de::Error::custom("Expected array")),
    }
}
#[derive(Clone, Debug)]
pub struct LiveState {
    pub snapshot: Snapshot,
    pub error: Option<String>,
    pub undo: Vec<(Value, Value)>,
    pub redo: Vec<(Value, Value)>,
}

#[cfg(not(target_arch = "wasm32"))]
fn quote(s: &str) -> String {
    let mut q = String::from("\"");
    for b in s.bytes() {
        q.push_str(&format!("\\{b:03}"));
    }
    q.push('"');
    q
}
#[cfg(not(target_arch = "wasm32"))]
pub fn request(root: &str, request: &Value) -> Result<Snapshot> {
    use std::io::{Read, Write};
    if let Some(peer) = crate::peer_binding::active_for(root)? {
        let payload = serde_json::to_string(request).map_err(err)?;
        let script = include_str!("defold_live.lua").replacen("REQUEST", &format!("json.decode({})", quote(&payload)), 1);
        let body = crate::peer_binding::native::http(&peer.address, "/binding/eval", &peer.token, script.as_bytes(), "POST")?;
        let body = String::from_utf8(body).map_err(err)?;
        let data = body.lines().find_map(|l| l.strip_prefix("PHOTOCRAFT_JSON:")).ok_or_else(|| err("Editor did not acknowledge the operation"))?;
        let snap: Snapshot = serde_json::from_str(data).map_err(err)?;
        if snap.nodes.len() > 8192 {
            return Err(err("GUI node limit"));
        }
        return Ok(snap);
    }
    let port_path = resource(root, "/.internal/editor.port")?;
    let port: u16 = std::fs::read_to_string(port_path).map_err(err)?.trim().parse().map_err(err)?;
    let token_path = resource(root, "/.internal/editor.token")?;
    let token = std::fs::read_to_string(token_path).map_err(err)?;
    let token = token.trim();
    if token.len() > 512 || token.is_empty() || token.contains(['\r', '\n']) {
        return Err(err("Invalid editor session token"));
    }
    let payload = serde_json::to_string(request).map_err(err)?;
    if payload.len() > 4 * 1024 * 1024 {
        return Err(err("Editor request exceeds limit"));
    }
    let script = include_str!("defold_live.lua").replacen("REQUEST", &format!("json.decode({})", quote(&payload)), 1);
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream =
        std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(700)).map_err(|_| err("Defold editor is not reachable"))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(4))).map_err(err)?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(2))).map_err(err)?;
    write!(stream,"POST /eval HTTP/1.0\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",script.len(),script).map_err(|_|err("Editor request failed; refresh to reconcile before retrying"))?;
    let mut bytes = Vec::new();
    stream.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| err("Editor response interrupted; refresh before retrying"))?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(err("Editor response exceeds limit"));
    }
    let response = String::from_utf8(bytes).map_err(err)?;
    let (head, body) = response.split_once("\r\n\r\n").ok_or_else(|| err("Invalid editor response"))?;
    if !head.lines().next().is_some_and(|l| l.contains(" 200 ")) {
        return Err(err(format!("Defold rejected edit: {}", body.chars().take(700).collect::<String>())));
    }
    let data = body.lines().find_map(|l| l.strip_prefix("PHOTOCRAFT_JSON:")).ok_or_else(|| err("Editor did not acknowledge the operation"))?;
    let snap: Snapshot = serde_json::from_str(data).map_err(err)?;
    if snap.nodes.len() > 8192 {
        return Err(err("GUI node limit"));
    }
    Ok(snap)
}
#[cfg(target_arch = "wasm32")]
pub fn request(_: &str, _: &Value) -> Result<Snapshot> {
    Err(err("Live Defold requires desktop"))
}
pub fn path(d: &GuiDocument) -> Result<String> {
    Ok(format!("/{}", Path::new(&d.path).strip_prefix(&d.root).map_err(err)?.to_string_lossy().replace('\\', "/")))
}
#[cfg(not(target_arch = "wasm32"))]
pub fn preview(root: &str, path: &str, width: u32, height: u32) -> Result<Vec<u8>> {
    use std::io::{Read, Write};
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err(err("Defold preview supports dimensions up to 4096"));
    }
    if let Some(peer) = crate::peer_binding::active_for(root)? {
        return crate::peer_binding::native::http(
            &peer.address,
            "/binding/preview",
            &peer.token,
            &serde_json::to_vec(&json!({"path":path,"width":width,"height":height})).map_err(err)?,
            "POST",
        );
    }
    let port: u16 = std::fs::read_to_string(resource(root, "/.internal/editor.port")?).map_err(err)?.trim().parse().map_err(err)?;
    let path: String = path
        .trim_start_matches('/')
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"/-_.".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    let mut stream =
        std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), std::time::Duration::from_millis(700)).map_err(err)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(4))).map_err(err)?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(2))).map_err(err)?;
    write!(stream, "GET /preview/{path}?width={width}&height={height} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n").map_err(err)?;
    let mut bytes = Vec::new();
    stream.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(err)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(err("Preview exceeds limit"));
    }
    let split = bytes.windows(4).position(|b| b == b"\r\n\r\n").ok_or_else(|| err("Invalid preview response"))?;
    let head = std::str::from_utf8(bytes.get(..split).ok_or_else(|| err("Invalid header"))?).map_err(err)?;
    if !head.lines().next().is_some_and(|l| l.contains(" 200 ")) {
        return Err(err("Defold preview unavailable"));
    }
    Ok(bytes.get(split + 4..).ok_or_else(|| err("Missing preview"))?.to_vec())
}
pub fn connect(d: &mut GuiDocument) -> Result<()> {
    let snap = request(&d.root, &json!({"op":"snapshot","path":path(d)?}))?;
    refresh(d, snap)
}
pub fn refresh(d: &mut GuiDocument, snap: Snapshot) -> Result<()> {
    let changed = d.live.as_ref().is_some_and(|s| serde_json::to_value(&s.snapshot.nodes).ok() != serde_json::to_value(&snap.nodes).ok());
    if changed && let Some(s) = &mut d.live {
        s.undo.clear();
        s.redo.clear();
    }
    accept(d, snap)
}
fn apply(d: &mut GuiDocument, mut req: Value) -> Result<()> {
    let state = d.live.as_ref().ok_or_else(|| err("Connect to Defold first"))?;
    if state.error.is_some() {
        return Err(err("Refresh Defold after a conflict or interrupted operation"));
    }
    req["path"] = json!(path(d)?);
    req["expected"] = json!(state.snapshot.nodes);
    match request(&d.root, &req) {
        Ok(snap) => accept(d, snap),
        Err(e) => {
            if let Some(s) = &mut d.live {
                s.error = Some(e.to_string());
            }
            Err(e)
        }
    }
}
pub fn mutate(d: &mut GuiDocument, req: Value) -> Result<()> {
    let state = d.live.as_ref().ok_or_else(|| err("Connect to Defold first"))?;
    let inverse = inverse(&state.snapshot, &req)?;
    apply(d, req.clone())?;
    if let Some(s) = &mut d.live {
        s.undo.push((req, inverse));
        s.redo.clear();
        while s.undo.len() > 100 || s.undo.iter().map(|v| serde_json::to_vec(v).map_or(0, |b| b.len())).sum::<usize>() > 8 * 1024 * 1024 {
            s.undo.remove(0);
        }
    }
    Ok(())
}
pub fn history(d: &mut GuiDocument, redo: bool) -> Result<()> {
    let s = d.live.as_ref().ok_or_else(|| err("Not connected"))?;
    let pair = (if redo { &s.redo } else { &s.undo }).last().ok_or_else(|| err("No GUI history"))?.clone();
    apply(d, if redo { pair.0.clone() } else { pair.1.clone() })?;
    if let Some(s) = &mut d.live {
        if redo {
            s.redo.pop();
            s.undo.push(pair);
        } else {
            s.undo.pop();
            s.redo.push(pair);
        }
    }
    Ok(())
}
fn inverse(snapshot: &Snapshot, req: &Value) -> Result<Value> {
    let find = |id: &str| snapshot.nodes.iter().find(|n| n.id == id).ok_or_else(|| err("Node no longer exists"));
    let id = req["node"].as_str().unwrap_or_default();
    match req["op"].as_str().unwrap_or_default() {
        "set" => {
            let n = find(id)?;
            let mut values = serde_json::Map::new();
            for k in req["values"].as_object().ok_or_else(|| err("Expected values"))?.keys() {
                values.insert(k.clone(), n.properties.get(k).cloned().ok_or_else(|| err("Unknown property"))?);
            }
            Ok(json!({"op":"set","node":req["values"].get("id").and_then(Value::as_str).unwrap_or(id),"values":values}))
        }
        "add" => Ok(json!({"op":"delete","node":req["id"]})),
        "delete" => {
            let n = find(id)?;
            let values = attachment(snapshot, n, 0)?;
            Ok(restore_order(snapshot, n, json!({"op":"add","id":id,"parent":n.parent,"kind":n.properties.get("type"),"values":values})))
        }
        "reorder" => {
            let order: Vec<_> = snapshot.nodes.iter().filter(|n| Some(n.parent.as_str()) == req["parent"].as_str()).map(|n| n.id.clone()).collect();
            Ok(json!({"op":"reorder","parent":req["parent"],"order":order}))
        }
        "move" => {
            let n = find(id)?;
            Ok(restore_order(snapshot, n, json!({"op":"move","node":id,"parent":n.parent})))
        }
        _ => Err(err("Unsupported live operation")),
    }
}
fn restore_order(s: &Snapshot, n: &RemoteNode, mut req: Value) -> Value {
    let mut siblings = s.nodes.iter().filter(|v| v.parent == n.parent).skip_while(|v| v.id != n.id);
    siblings.next();
    if let Some(next) = siblings.next() {
        req["after"] = json!(next.id);
    }
    req
}
/// Defold's script API uses quaternions; its inspector and GUI files use degrees.
pub fn rotation_degrees(value: &Value) -> Value {
    let Some(q) = value.as_array().filter(|a| a.len() == 4) else { return value.clone() };
    let [x, y, z, w] = std::array::from_fn(|i| q.get(i).and_then(Value::as_f64).unwrap_or_default());
    json!([
        (2.0 * (w * x + y * z)).atan2(1.0 - 2.0 * (x * x + y * y)).to_degrees(),
        (2.0 * (w * y - z * x)).clamp(-1.0, 1.0).asin().to_degrees(),
        (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z)).to_degrees()
    ])
}
fn attachment(s: &Snapshot, n: &RemoteNode, depth: usize) -> Result<Value> {
    if depth > 128 {
        return Err(err("GUI depth limit"));
    }
    let mut values = serde_json::Map::new();
    for k in &n.writable {
        if let Some(v) = n.properties.get(k) {
            values.insert(k.clone(), v.clone());
        }
    }
    values.insert("type".into(), n.properties.get("type").cloned().unwrap_or(Value::Null));
    let children = s.nodes.iter().filter(|c| c.parent == n.id).map(|c| attachment(s, c, depth + 1)).collect::<Result<Vec<_>>>()?;
    if !children.is_empty() {
        values.insert("nodes".into(), json!(children));
    }
    Ok(Value::Object(values))
}
pub fn property_value(d: &GuiDocument, id: &str, property: &str, value: &Value) -> Result<(String, Value)> {
    let n = d.live.as_ref().and_then(|s| s.snapshot.nodes.iter().find(|n| n.id == id)).ok_or_else(|| err("Missing live node"))?;
    node_property_value(n, property, value)
}
pub(super) fn node_property_value(n: &RemoteNode, property: &str, value: &Value) -> Result<(String, Value)> {
    let (field, axis) = property.split_once('.').map_or((property, None), |(f, c)| (f, Some(c)));
    let key = match field {
        "size" => "manual_size",
        "xanchor" => "x_anchor",
        "yanchor" => "y_anchor",
        k => k,
    }
    .to_string();
    if !n.writable.contains(&key) {
        return Err(err("Defold marks this property read-only"));
    }
    if let Some(axis) = axis {
        let i = ["x", "y", "z", "w"].iter().position(|c| *c == axis).ok_or_else(|| err("Invalid vector component"))?;
        let raw = n.properties.get(&key).ok_or_else(|| err("Expected vector"))?;
        let raw = if key == "rotation" { rotation_degrees(raw) } else { raw.clone() };
        let mut v = raw.as_array().cloned().ok_or_else(|| err("Expected vector"))?;
        let c = v.get_mut(i).ok_or_else(|| err("Vector component unavailable"))?;
        let number = value.as_f64().filter(|v| v.is_finite() && v.abs() <= 1_000_000.0).ok_or_else(|| err("Invalid number"))?;
        *c = json!(number);
        return Ok((key.clone(), json!(v)));
    }
    if key == "rotation" {
        if !value.as_array().is_some_and(|a| a.len() == 3 && a.iter().all(|v| v.as_f64().is_some_and(|f| f.is_finite() && f.abs() <= 1_000_000.0))) {
            return Err(err("Rotation requires X, Y, Z degrees"));
        }
        return Ok((key, value.clone()));
    }
    let mut value = value.clone();
    if let Some(s) = value.as_str() {
        if s.len() > 65536 {
            return Err(err("Text limit"));
        }
        if matches!(field, "pivot" | "blend_mode" | "xanchor" | "yanchor" | "adjust_mode" | "size_mode" | "clipping_mode" | "outerBounds") {
            value = json!(s.replace('_', "-").to_lowercase());
        }
    }
    Ok((key, value))
}
pub fn accept(d: &mut GuiDocument, snapshot: Snapshot) -> Result<()> {
    if snapshot.nodes.len() > 8192 {
        return Err(err("GUI node limit"));
    }
    let mut source = String::new();
    // Preserve resource declarations from disk. Live nodes are authoritative; this
    // view is never used for offline .gui saving while connected.
    for f in &d.scene.proto.fields {
        if f.name != "nodes"
            && f.name != "layouts"
            && let Some(raw) = d.scene.proto.source.get(f.span.clone())
        {
            source.push_str(raw);
            source.push('\n');
        }
    }
    for n in &snapshot.nodes {
        source.push_str(&format!("nodes {{ id: {} parent: {}\n", serde_json::to_string(&n.id).map_err(err)?, serde_json::to_string(&n.parent).map_err(err)?));
        let kind = n.properties.get("type").and_then(Value::as_str).unwrap_or("gui-node-type-custom").trim_start_matches("gui-node-type-").to_uppercase();
        source.push_str(&format!("type: TYPE_{kind}\n"));
        for (key, v) in &n.properties {
            let key = match key.as_str() {
                "manual_size" => "size",
                "x_anchor" => "xanchor",
                "y_anchor" => "yanchor",
                k => k,
            };
            if matches!(key, "id" | "type" | "nodes") {
                continue;
            }
            if !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                continue;
            }
            let degrees = if key == "rotation" { rotation_degrees(v) } else { v.clone() };
            let v = &degrees;
            if let Some(a) = v.as_array() {
                if a.iter().all(Value::is_number) && a.len() <= 4 {
                    source.push_str(&format!("{key} {{ "));
                    for (c, v) in ["x", "y", "z", "w"].into_iter().zip(a) {
                        source.push_str(&format!("{c}: {v} "));
                    }
                    source.push_str("}\n");
                }
            } else if v.is_boolean() || v.is_number() {
                source.push_str(&format!("{key}: {v}\n"));
            } else if let Some(s) = v.as_str() {
                let enum_value = matches!(key, "pivot" | "blend_mode" | "xanchor" | "yanchor" | "adjust_mode" | "size_mode" | "clipping_mode" | "outerBounds");
                source.push_str(&format!("{key}: {}\n", if enum_value { s.replace('-', "_").to_uppercase() } else { serde_json::to_string(s).map_err(err)? }));
            }
        }
        source.push_str("}\n");
    }
    let next = Scene::parse(source).map_err(err)?;
    if d.scene.proto.source != next.proto.source {
        d.scene = next;
        d.revision += 1;
    }
    if d.selected.as_ref().is_none_or(|id| !d.scene.nodes.iter().any(|n| n.id == *id)) {
        d.selected = d.scene.nodes.first().map(|n| n.id.clone());
    }
    let (undo, redo) = d.live.take().map_or_else(|| (vec![], vec![]), |s| (s.undo, s.redo));
    d.live = Some(LiveState { snapshot, error: None, undo, redo });
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn saved_float_positions_reconnect_without_accepting_real_editor_changes() {
        let make = |value| super::RemoteNode {
            id: "node".into(),
            parent: "root".into(),
            properties: std::collections::BTreeMap::from([("position".into(), serde_json::json!([value, 0, 0]))]),
            writable: vec!["position".into()],
        };
        for value in [593.3821531455611_f64, 1086.916680490431, -593.3821531455611] {
            let rounded = f64::from(value as f32);
            let expected = vec![make(value)];
            let reloaded = vec![make(rounded)];
            assert!(super::same_nodes(&reloaded, &expected));
            assert!(!super::same_nodes(&expected, &reloaded));
            let adjacent = f64::from(f32::from_bits((value as f32).to_bits() + 1));
            assert!(!super::same_nodes(&[make(adjacent)], &expected));
            assert!(!super::same_nodes(&[make(rounded + 0.0000001)], &expected));
            let mut renamed = reloaded.clone();
            renamed[0].id = "other".into();
            assert!(!super::same_nodes(&renamed, &expected));
        }
        let mut expected = make(0.0);
        expected.properties.insert("integer".into(), serde_json::json!(16_777_217));
        let mut changed = expected.clone();
        changed.properties.insert("integer".into(), serde_json::json!(16_777_216));
        assert!(!super::same_nodes(&[changed], &[expected]));
    }
    #[test]
    fn legacy_snapshot_rounding_is_accepted_but_editor_changes_are_not() {
        let make = |v| super::RemoteNode {
            id: "node".into(),
            parent: "".into(),
            properties: std::collections::BTreeMap::from([("color".into(), serde_json::json!([v, 0, 0]))]),
            writable: vec!["color".into()],
        };
        let value = 0.9685999751091003_f64;
        let current = vec![make(value)];
        assert!(super::same_nodes(&current, &[make(0.9685999751091005)]));
        assert!(!super::same_nodes(&current, &[make(f32::from_bits((value as f32).to_bits() + 1) as f64)]));
        let mut changed = current.clone();
        changed[0].parent = "other".into();
        assert!(!super::same_nodes(&current, &changed));
    }
    #[test]
    fn snapshot_numbers_roundtrip_without_false_editor_conflicts() {
        for value in [0.9685999751091003_f64, 0.9020000100135803, 202.63070678710938, 1.6002000570297241] {
            let source = serde_json::json!({"version":"1.13.1","path":"/a.gui","nodes":[{
                "id":"node","parent":"","properties":{"position":[value,0,0]},"writable":["position"]
            }]});
            let snapshot: super::Snapshot = serde_json::from_str(&serde_json::to_string(&source).unwrap()).unwrap();
            assert_eq!(snapshot.nodes[0].properties["position"][0].as_f64().unwrap().to_bits(), value.to_bits());
            let restored: super::Snapshot = serde_json::from_str(&serde_json::to_string(&snapshot).unwrap()).unwrap();
            assert_eq!(restored.nodes[0].properties["position"][0].as_f64().unwrap().to_bits(), value.to_bits());
        }
    }
    use super::*;
    #[test]
    fn rotation_roundtrip_and_component_edits_use_degrees() {
        let q = json!([0.0, 0.0, std::f64::consts::FRAC_1_SQRT_2, std::f64::consts::FRAC_1_SQRT_2]);
        assert!((rotation_degrees(&q)[2].as_f64().unwrap() - 90.0).abs() < 1e-8);
        let n = RemoteNode {
            id: "a".into(),
            parent: "".into(),
            properties: BTreeMap::from([("rotation".into(), json!([15.0, -20.0, 90.0]))]),
            writable: vec!["rotation".into()],
        };
        let (_, v) = node_property_value(&n, "rotation.z", &json!(45)).unwrap();
        assert_eq!(v, json!([15.0, -20.0, 45.0]));
        assert!(node_property_value(&n, "rotation.w", &json!(0)).is_err());
        let mut readonly = n;
        readonly.writable.clear();
        assert!(node_property_value(&readonly, "rotation.z", &json!(45)).is_err());
    }
    #[test]
    fn restore_deleted_node_keeps_original_sibling_order_and_nested_properties() {
        let make = |id: &str, parent: &str| RemoteNode {
            id: id.into(),
            parent: parent.into(),
            properties: BTreeMap::from([("id".into(), json!(id)), ("type".into(), json!("gui-node-type-box"))]),
            writable: vec!["id".into()],
        };
        let s = Snapshot { version: "test".into(), path: "/a.gui".into(), nodes: vec![make("first", ""), make("child", "first"), make("last", "")] };
        let inverse = inverse(&s, &json!({"op":"delete","node":"first"})).unwrap();
        assert_eq!(inverse["after"], "last");
        assert_eq!(inverse["values"]["nodes"][0]["id"], "child");
    }
    #[test]
    fn empty_lua_tables_are_lists_but_nonempty_objects_are_rejected() {
        assert!(serde_json::from_value::<Snapshot>(json!({"version":"test","path":"/a.gui","nodes":{}})).unwrap().nodes.is_empty());
        assert!(serde_json::from_value::<Snapshot>(json!({"version":"test","path":"/a.gui","nodes":{"bad":1}})).is_err());
    }
}
