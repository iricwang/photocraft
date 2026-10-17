//! Explicit, mutually acknowledged PhotoCraft / Defold IP sessions.
//! This channel never exposes the general automation listener or editor token.
use crate::{Result, commands::CommandSpec, defold_cmds::err};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;
use std::time::Instant;

pub const DEFOLD_PORT: u16 = 17654;
pub const CRAFT_PORT: u16 = 17655;
pub const LIMIT: usize = 96 * 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
pub struct Peer {
    pub id: String,
    pub address: String,
    #[serde(skip_serializing)]
    pub token: String,
    pub project: String,
    pub root: String,
}
#[derive(Clone)]
pub struct Offer {
    pub ticket: String,
    pub peer: Peer,
    pub phase: String,
    pub expires: Instant,
}
#[derive(Default)]
pub struct Binding {
    pub peers: BTreeMap<String, Peer>,
    pub offline: BTreeSet<String>,
    pub id: String,
    pub port: u16,
    pub active: Option<Peer>,
    pub connected: bool,
    pub incoming: Option<Offer>,
    pub outgoing: Option<String>,
    pub working: bool,
    pub frozen: bool,
    pub refresh: bool,
    pub opens: Vec<(Peer, String)>,
    pub error: Option<String>,
}
impl Binding {
    pub fn for_root(&self, root: &str) -> Result<Option<Peer>> {
        let candidates: Vec<_> = self.connections().into_iter().filter(|p| self.connected_to(p) && same_project(&p.root, root)).collect();
        if let Some(peer) = self.active.as_ref().filter(|p| self.connected_to(p) && same_project(&p.root, root)) {
            return Ok(Some(peer.clone()));
        }
        if candidates.len() > 1 {
            return Err(err("Select the Defold connection for this project before synchronizing"));
        }
        if let Some(peer) = candidates.first() {
            return Ok(Some((*peer).clone()));
        }
        if std::path::Path::new(root).join(".photocraft-remote").is_file() {
            return Err(err("This remote workspace is not bound. Reconnect before editing or synchronizing."));
        }
        Ok(None)
    }
    pub fn connections(&self) -> Vec<&Peer> {
        let mut peers: Vec<_> = self.peers.values().collect();
        if let Some(active) = &self.active
            && !self.peers.contains_key(&active.id)
        {
            peers.push(active);
        }
        peers
    }
    pub fn connected_to(&self, peer: &Peer) -> bool {
        !self.offline.contains(&peer.id) && (self.peers.contains_key(&peer.id) || self.connected)
    }
    pub fn peer_for_token(&self, token: &str) -> Option<&Peer> {
        if token.is_empty() {
            return None;
        }
        self.connections().into_iter().find(|peer| peer.token == token)
    }
    pub fn put(&mut self, peer: Peer) {
        if let Some(old) = &self.active {
            self.peers.entry(old.id.clone()).or_insert_with(|| old.clone());
        }
        let stale: Vec<_> = self.peers.values().filter(|old| old.address == peer.address || old.id == peer.id).map(|old| old.id.clone()).collect();
        for id in stale {
            self.peers.remove(&id);
            self.offline.remove(&id);
        }
        self.offline.remove(&peer.id);
        self.peers.insert(peer.id.clone(), peer.clone());
        self.active = Some(peer);
        self.connected = true;
        self.error = None;
    }
    pub fn select(&mut self, id: &str) -> Result<()> {
        if self.working {
            return Err(err("Wait for the current connection operation to finish"));
        }
        let peer = self.connections().into_iter().find(|p| p.id == id).cloned().ok_or_else(|| err("Unknown Defold connection"))?;
        self.connected = self.connected_to(&peer);
        self.active = Some(peer);
        Ok(())
    }
    pub fn remove_token(&mut self, token: &str) -> Option<Peer> {
        let peer = self.peer_for_token(token)?.clone();
        self.peers.remove(&peer.id);
        self.offline.remove(&peer.id);
        self.opens.retain(|(queued, _)| queued.token != token);
        if self.active.as_ref().is_some_and(|p| p.token == token) {
            self.active = self.peers.values().next().cloned();
            self.connected = self.active.as_ref().is_some_and(|p| !self.offline.contains(&p.id));
        }
        Some(peer)
    }
    pub fn mark_address(&mut self, address: &str, connected: bool) {
        let ids: Vec<_> = self.connections().into_iter().filter(|p| p.address == address).map(|p| p.id.clone()).collect();
        for id in ids {
            if connected {
                self.offline.remove(&id);
            } else {
                self.offline.insert(id);
            }
        }
        if self.active.as_ref().is_some_and(|p| p.address == address) {
            self.connected = connected;
        }
    }
}
static STATE: OnceLock<Mutex<Binding>> = OnceLock::new();
pub fn state() -> std::sync::MutexGuard<'static, Binding> {
    STATE.get_or_init(|| Mutex::new(Binding::default())).lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
pub fn identity() -> Result<String> {
    let mut bytes = [0; 32];
    getrandom::getrandom(&mut bytes).map_err(err)?;
    Ok(bytes.iter().map(|v| format!("{v:02x}")).collect())
}
pub fn address(input: &str, port: u16) -> Result<std::net::SocketAddr> {
    input
        .parse()
        .or_else(|_| input.parse::<std::net::IpAddr>().map(|ip| std::net::SocketAddr::new(ip, port)))
        .map_err(|_| err("Enter an IP address, optionally followed by a port"))
}
pub fn active_for(root: &str) -> Result<Option<Peer>> {
    state().for_root(root)
}
/// Compare directory identities, including Windows drive casing and extended paths.
pub fn same_project(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let normalize = |s: &str| {
        let p = std::fs::canonicalize(s).unwrap_or_else(|_| s.into());
        let s = p.to_string_lossy().replace('\\', "/");
        let s = s.strip_prefix("//?/").unwrap_or(&s).trim_end_matches('/');
        if cfg!(windows) { s.to_lowercase() } else { s.to_owned() }
    };
    normalize(a) == normalize(b)
}
pub fn online(root: &str) -> bool {
    std::path::Path::new(root).join(".photocraft-remote").is_file() || std::path::Path::new(root).join(".internal/editor.port").is_file()
}
pub fn inspect() -> Value {
    let s = state();
    json!({"id":s.id,"port":s.port,"peer":s.active,"connected":s.connected,"bindingVersion":2,"peers":s.connections().into_iter().map(|p|json!({"peer":p,"connected":s.connected_to(p)})).collect::<Vec<_>>(),"pending":s.incoming.as_ref().map(|o|json!({"peer":o.peer,"phase":o.phase})),"outgoing":s.outgoing,"working":s.working,"error":s.error})
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "defold.openBoundResource",
            label: "Open Bound Defold GUI",
            menu: &[],
            shortcut: None,
            params: r#"{"path":"/main/menu.gui","id":"optional editor session"}"#,
            enabled: crate::file_cmds::native,
            run: |session, p| {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let peer = {
                        let s = state();
                        let peer = if let Some(id) = p.get("id") {
                            s.connections().into_iter().find(|peer| Some(peer.id.as_str()) == id.as_str()).cloned()
                        } else {
                            s.active.clone()
                        };
                        peer.filter(|peer| !peer.root.is_empty()).ok_or_else(|| err("Bind Defold first"))?
                    };
                    let path = native::safe_relative(p["path"].as_str().unwrap_or(""))?;
                    if !path.to_string_lossy().ends_with(".gui") {
                        return Err(err("Choose a GUI resource"));
                    }
                    session.execute("defold.openDesign", json!({"path":std::path::Path::new(&peer.root).join(path).to_string_lossy()}))
                }
                #[cfg(target_arch = "wasm32")]
                {
                    let _ = (session, p);
                    Err(err("IP binding requires desktop"))
                }
            },
            journal: false,
        },
        CommandSpec {
            id: "defold.bind",
            label: "Bind Defold by IP",
            menu: &[],
            shortcut: None,
            params: r#"{"address":"127.0.0.1:17654"}"#,
            enabled: crate::file_cmds::native,
            run: |_, p| {
                let target = address(p["address"].as_str().unwrap_or("127.0.0.1"), DEFOLD_PORT)?.to_string();
                let mut s = state();
                if s.outgoing.is_some() || s.incoming.is_some() || s.working {
                    return Err(err("Answer the current binding request first"));
                }
                s.outgoing = Some(target);
                s.error = None;
                Ok(json!({"pending":true}))
            },
            journal: false,
        },
        CommandSpec {
            id: "defold.selectBinding",
            label: "Select Defold Connection",
            menu: &[],
            shortcut: None,
            params: r#"{"id":"editor-session-id"}"#,
            enabled: crate::file_cmds::native,
            run: |_, p| {
                state().select(p["id"].as_str().unwrap_or(""))?;
                Ok(inspect())
            },
            journal: false,
        },
        CommandSpec {
            id: "defold.unbind",
            label: "Disconnect Defold",
            menu: &[],
            shortcut: None,
            params: r#"{"id":"editor-session-id"}"#,
            enabled: crate::file_cmds::native,
            run: |_, p| {
                let mut s = state();
                if s.working {
                    return Err(err("Wait for the current connection operation to finish"));
                }
                let peer = s
                    .connections()
                    .into_iter()
                    .find(|peer| Some(peer.id.as_str()) == p["id"].as_str())
                    .cloned()
                    .ok_or_else(|| err("Unknown Defold connection"))?;
                s.remove_token(&peer.token);
                drop(s);
                #[cfg(not(target_arch = "wasm32"))]
                std::thread::spawn(move || {
                    let _ = native::json_request(&peer.address, "/binding/release", &peer.token, &json!({}));
                });
                Ok(json!({"disconnected":true}))
            },
            journal: false,
        },
        CommandSpec {
            id: "defold.bindingStatus",
            label: "Defold Binding Status",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: crate::commands::always,
            run: |_, _| Ok(inspect()),
            journal: false,
        },
    ]
}

#[cfg(not(target_arch = "wasm32"))]
pub mod native {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::path::{Component, Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub fn http(target: &str, route: &str, token: &str, body: &[u8], method: &str) -> Result<Vec<u8>> {
        let address = address(target, DEFOLD_PORT)?;
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(900)).map_err(|_| err("The other application is not reachable"))?;
        stream.set_read_timeout(Some(Duration::from_secs(30))).map_err(err)?;
        stream.set_write_timeout(Some(Duration::from_secs(10))).map_err(err)?;
        write!(
            stream,
            "{method} {route} HTTP/1.0\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .map_err(err)?;
        stream.write_all(body).map_err(err)?;
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        reader.by_ref().take(8193).read_line(&mut status).map_err(err)?;
        if status.len() > 8192 {
            return Err(err("Peer header limit"));
        }
        let mut size = 0usize;
        let mut chunked = false;
        let mut headers = 0usize;
        loop {
            let mut line = String::new();
            reader.by_ref().take(8193).read_line(&mut line).map_err(err)?;
            headers += line.len();
            if headers > 32768 {
                return Err(err("Peer header limit"));
            }
            if line == "\r\n" {
                break;
            }
            if line.is_empty() {
                return Err(err("Incomplete peer response"));
            }
            let lower = line.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("content-length:") {
                size = v.trim().parse().map_err(err)?;
            }
            if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
                chunked = true;
            }
        }
        if size > LIMIT {
            return Err(err("Peer response exceeds limit"));
        }
        let mut out = Vec::new();
        if chunked {
            loop {
                let mut line = String::new();
                reader.by_ref().take(100).read_line(&mut line).map_err(err)?;
                let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16).map_err(err)?;
                if size == 0 {
                    break;
                }
                if size > LIMIT.saturating_sub(out.len()) {
                    return Err(err("Peer response exceeds limit"));
                }
                let before = out.len();
                reader.by_ref().take(size as u64).read_to_end(&mut out).map_err(err)?;
                if out.len() - before != size {
                    return Err(err("Incomplete peer response"));
                }
                let mut crlf = [0; 2];
                reader.read_exact(&mut crlf).map_err(err)?;
                if crlf != *b"\r\n" {
                    return Err(err("Invalid chunk"));
                }
            }
        } else {
            reader.take((LIMIT + 1) as u64).read_to_end(&mut out).map_err(err)?;
        }
        if out.len() > LIMIT || (size > 0 && out.len() != size) {
            return Err(err("Incomplete or oversized peer response"));
        }
        if status.split_whitespace().nth(1) != Some("200") {
            return Err(err(format!("Peer rejected request: {}", String::from_utf8_lossy(&out).chars().take(300).collect::<String>())));
        }
        Ok(out)
    }
    pub fn json_request(target: &str, route: &str, token: &str, body: &Value) -> Result<Value> {
        serde_json::from_slice(&http(target, route, token, &serde_json::to_vec(body).map_err(err)?, "POST")?).map_err(err)
    }
    pub fn own_address(target: &str, port: u16) -> Result<String> {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").map_err(err)?;
        socket.connect(address(target, DEFOLD_PORT)?).map_err(err)?;
        Ok(SocketAddr::new(socket.local_addr().map_err(err)?.ip(), port).to_string())
    }
    pub fn start() -> Result<()> {
        let explicit = std::env::var("PHOTOCRAFT_BIND_PORT").ok();
        let port = explicit.as_ref().map(|value| value.parse::<u16>().map_err(err)).transpose()?.unwrap_or(CRAFT_PORT);
        let listener = match TcpListener::bind(("0.0.0.0", port)) {
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse && explicit.is_none() => TcpListener::bind(("0.0.0.0", 0)).map_err(err)?,
            result => result.map_err(err)?,
        };
        let port = listener.local_addr().map_err(err)?.port();
        {
            let mut s = state();
            s.id = identity()?;
            s.port = port;
        }
        std::thread::spawn(move || {
            let count = Arc::new(AtomicUsize::new(0));
            for stream in listener.incoming().flatten() {
                if count.fetch_add(1, Ordering::AcqRel) >= 8 {
                    count.fetch_sub(1, Ordering::AcqRel);
                    continue;
                }
                let count = count.clone();
                std::thread::spawn(move || {
                    let _ = serve(stream);
                    count.fetch_sub(1, Ordering::AcqRel);
                });
            }
        });
        Ok(())
    }
    fn serve(mut stream: TcpStream) -> Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(3))).map_err(err)?;
        stream.set_write_timeout(Some(Duration::from_secs(3))).map_err(err)?;
        let mut reader = BufReader::new(stream.try_clone().map_err(err)?);
        let mut line = String::new();
        reader.by_ref().take(8193).read_line(&mut line).map_err(err)?;
        let route = line.split_whitespace().nth(1).unwrap_or("").to_owned();
        let mut token = String::new();
        let mut size = 0usize;
        let mut total = 0;
        loop {
            line.clear();
            reader.by_ref().take(8193).read_line(&mut line).map_err(err)?;
            total += line.len();
            if total > 16384 || line.is_empty() {
                return Err(err("Invalid peer headers"));
            }
            if line == "\r\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                if key.eq_ignore_ascii_case("authorization") {
                    token = value.trim().strip_prefix("Bearer ").unwrap_or("").to_owned();
                }
                if key.eq_ignore_ascii_case("content-length") {
                    size = value.trim().parse().map_err(err)?;
                }
            }
        }
        if size > 16384 {
            return Err(err("Binding request too large"));
        }
        let mut bytes = vec![0; size];
        reader.read_exact(&mut bytes).map_err(err)?;
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let result = if route == "/binding/request" {
            let declared = address(body["address"].as_str().unwrap_or(""), DEFOLD_PORT);
            let source = stream.peer_addr().map_err(err)?;
            if declared.as_ref().is_ok_and(|a| a.ip() == source.ip() || a.ip().is_loopback() && source.ip().is_loopback()) {
                route_request(&route, &token, &body)
            } else {
                Err(err("The advertised peer IP does not match the connection"))
            }
        } else {
            route_request(&route, &token, &body)
        };
        let (status, reply) = match result {
            Ok(v) => ("200 OK", v),
            Err(e) => ("409 Conflict", json!({"error":e.to_string()})),
        };
        let reply = serde_json::to_vec(&reply).map_err(err)?;
        write!(stream, "HTTP/1.0 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", reply.len()).map_err(err)?;
        stream.write_all(&reply).map_err(err)
    }
    pub(super) fn route_request(route: &str, token: &str, body: &Value) -> Result<Value> {
        let mut s = state();
        if s.incoming.as_ref().is_some_and(|o| o.expires < Instant::now()) {
            s.incoming = None;
        }
        match route {
            "/binding/status" => Ok(
                json!({"app":"PhotoCraft","id":s.id,"bound":s.active.as_ref().map(|p|&p.id),"bindingVersion":2,"boundPeers":s.connections().into_iter().map(|p|&p.id).collect::<Vec<_>>()}),
            ),
            "/binding/request" => {
                let peer_id = body["id"].as_str().filter(|v| v.len() == 64).ok_or_else(|| err("Invalid peer identity"))?;
                let target = address(body["address"].as_str().unwrap_or(""), DEFOLD_PORT)?.to_string();
                if s.incoming.is_some() || s.outgoing.is_some() || s.working {
                    return Err(err("Another binding request is pending"));
                }
                let ticket = identity()?;
                s.incoming = Some(Offer {
                    ticket: ticket.clone(),
                    peer: Peer {
                        id: peer_id.into(),
                        address: target,
                        token: ticket.clone(),
                        project: body["project"].as_str().unwrap_or("").into(),
                        root: String::new(),
                    },
                    phase: "confirm".into(),
                    expires: Instant::now() + Duration::from_secs(180),
                });
                Ok(json!({"ticket":ticket,"id":s.id}))
            }
            "/binding/result" => {
                let offer = s.incoming.as_ref().filter(|o| o.ticket == token).ok_or_else(|| err("Binding request expired or cancelled"))?;
                Ok(json!({"phase":offer.phase,"id":s.id}))
            }
            "/binding/cancel" => {
                if s.incoming.as_ref().is_some_and(|o| o.ticket == token) {
                    s.incoming = None;
                }
                Ok(json!({"cancelled":true}))
            }
            "/binding/commit" => {
                let offer =
                    s.incoming.as_ref().filter(|o| o.ticket == token && o.phase == "ready").ok_or_else(|| err("Workspace is not ready to switch"))?.clone();
                // The UI owns completion: it must recheck open documents and outstanding workers.
                if let Some(o) = &mut s.incoming {
                    o.phase = "commit".into();
                }
                Ok(json!({"phase":"committing","id":s.id,"peer":offer.peer.id}))
            }
            "/binding/committed" => {
                if s.peer_for_token(token).is_some() {
                    Ok(json!({"phase":"bound","id":s.id}))
                } else {
                    Ok(json!({"phase":"pending"}))
                }
            }
            "/binding/release" => {
                s.remove_token(token);
                Ok(json!({"released":true}))
            }
            "/binding/open" => {
                let peer = s.peer_for_token(token).cloned().ok_or_else(|| err("Peer is not bound"))?;
                let path = body["path"].as_str().ok_or_else(|| err("Missing resource path"))?;
                safe_relative(path)?;
                if !path.ends_with(".gui") || s.opens.len() >= 16 {
                    return Err(err("Open a GUI resource"));
                }
                s.opens.push((peer, path.into()));
                Ok(json!({"queued":true}))
            }
            _ => Err(err("Unknown binding endpoint")),
        }
    }
    pub fn safe_relative(path: &str) -> Result<PathBuf> {
        let path = path.strip_prefix('/').unwrap_or(path);
        if path.is_empty() || path.contains(['\\', ':', '\0']) || path.split('/').any(|s| s.starts_with('.') || s.is_empty()) {
            return Err(err("Invalid resource path"));
        }
        let p = Path::new(path);
        if p.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(err("Invalid resource path"));
        }
        Ok(p.to_path_buf())
    }
    pub fn mirror(peer: &mut Peer) -> Result<()> {
        // Local projects share the original directory. Reconnection must never copy over it.
        if address(&peer.address, DEFOLD_PORT)?.ip().is_loopback() && Path::new(&peer.project).join("game.project").is_file() {
            peer.root = std::fs::canonicalize(&peer.project).map_err(err)?.to_string_lossy().into_owned();
            return Ok(());
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let manifest = loop {
            match json_request(&peer.address, "/binding/manifest", &peer.token, &json!({})) {
                Ok(manifest) => break manifest,
                Err(error) if Instant::now() >= deadline => return Err(error),
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        };
        let project = manifest["project"].as_str().ok_or_else(|| err("Missing project identity"))?;
        let files = manifest["files"].as_array().filter(|f| f.len() <= 20000).ok_or_else(|| err("Project exceeds file limit"))?;
        let cache = std::env::temp_dir().join("photocraft-peers").join(blake3::hash(format!("{}:{project}", peer.address).as_bytes()).to_hex().as_str());
        std::fs::create_dir_all(&cache).map_err(err)?;
        let cache = std::fs::canonicalize(cache).map_err(err)?;
        let mut total = 0u64;
        for file in files {
            let rel = safe_relative(file["path"].as_str().unwrap_or(""))?;
            let size = file["size"].as_u64().filter(|n| *n <= 64 * 1024 * 1024).ok_or_else(|| err("Resource exceeds 64 MB"))?;
            total = total.saturating_add(size);
            if total > 1024 * 1024 * 1024 {
                return Err(err("Project exceeds 1 GB transfer limit"));
            }
            let target = cache.join(rel);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(err)?;
                if !std::fs::canonicalize(parent).map_err(err)?.starts_with(&cache) {
                    return Err(err("Resource escapes cache"));
                }
            }
            if target.exists() && std::fs::symlink_metadata(&target).map_err(err)?.file_type().is_symlink() {
                return Err(err("Symlink in project cache"));
            }
            let data = http(&peer.address, "/binding/file", &peer.token, &serde_json::to_vec(&json!({"path":file["path"]})).map_err(err)?, "POST")?;
            if data.len() as u64 != size {
                return Err(err("Project changed during transfer; reconnect"));
            }
            use sha2::Digest;
            let digest = format!("{:x}", sha2::Sha256::digest(&data));
            if file["sha256"].as_str() != Some(digest.as_str()) {
                return Err(err("Resource changed during transfer; refresh the project"));
            }
            let mut temp = tempfile::NamedTempFile::new_in(target.parent().ok_or_else(|| err("Invalid cache path"))?).map_err(err)?;
            temp.write_all(&data).map_err(err)?;
            temp.persist(&target).map_err(err)?;
        }
        if !cache.join("game.project").is_file() {
            return Err(err("Defold did not provide game.project"));
        }
        std::fs::write(cache.join(".photocraft-remote"), peer.id.as_bytes()).map_err(err)?;
        std::fs::write(cache.join(".photocraft-resources.json"), serde_json::to_vec(&manifest).map_err(err)?).map_err(err)?;
        peer.project = project.into();
        peer.root = cache.to_string_lossy().into_owned();
        Ok(())
    }
    /// Resolve project identity before deciding whether a connection needs confirmation.
    pub fn resolve_project(peer: &mut Peer) -> Result<()> {
        let project = json_request(&peer.address, "/binding/project", &peer.token, &json!({}))?;
        peer.project = project["project"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| err("Update Defold to support project connections"))?.into();
        if address(&peer.address, DEFOLD_PORT)?.ip().is_loopback() && Path::new(&peer.project).join("game.project").is_file() {
            peer.root = std::fs::canonicalize(&peer.project).map_err(err)?.to_string_lossy().into_owned();
        } else {
            let cache =
                std::env::temp_dir().join("photocraft-peers").join(blake3::hash(format!("{}:{}", peer.address, peer.project).as_bytes()).to_hex().as_str());
            peer.root = cache.to_string_lossy().into_owned();
        }
        Ok(())
    }
    pub fn prepare_project(peer: &mut Peer) -> Result<()> {
        resolve_project(peer)?;
        // An existing mirror contains editable GUI sources. Reconnect only renews credentials.
        if Path::new(&peer.root).join("game.project").is_file() {
            return Ok(());
        }
        mirror(peer)
    }
    pub fn accept_commit(ticket: &str) -> Result<Peer> {
        let mut s = state();
        let peer = s
            .incoming
            .as_ref()
            .filter(|o| o.ticket == ticket && o.phase == "commit" && o.expires > Instant::now())
            .ok_or_else(|| err("Binding was cancelled or expired before connecting"))?
            .peer
            .clone();
        s.put(peer.clone());
        s.incoming = None;
        Ok(peer)
    }
    pub fn replace(peer: Peer) {
        state().put(peer);
    }
    pub fn upload(root: &str, files: &[(String, Vec<u8>)]) -> Result<()> {
        use base64::Engine;
        if Path::new(root).join(".photocraft-remote").is_file()
            && let Some(peer) = active_for(root)?
        {
            for (path, bytes) in files {
                json_request(
                    &peer.address,
                    "/binding/upload",
                    &peer.token,
                    &json!({"path":path,"data":base64::engine::general_purpose::STANDARD.encode(bytes)}),
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connections_keep_independent_credentials_and_project_routing() {
        let mut s = Binding::default();
        let peer = |id: &str, root: &str, port: u16| Peer {
            id: id.into(),
            address: format!("127.0.0.1:{port}"),
            token: format!("secret-{id}"),
            project: root.into(),
            root: root.into(),
        };
        let a = peer("a", "/game/a", 19001);
        let b = peer("b", "/game/b", 19002);
        s.put(a.clone());
        s.put(b.clone());
        assert_eq!(s.for_root("/game/a").unwrap().unwrap().id, "a");
        assert_eq!(s.for_root("/game/b").unwrap().unwrap().id, "b");
        assert!(s.for_root("/game/unbound").unwrap().is_none());

        assert_eq!(s.connections().len(), 2);
        assert_eq!(s.peer_for_token("secret-a").unwrap().root, "/game/a");
        assert_eq!(s.peer_for_token("secret-b").unwrap().root, "/game/b");
        s.put(Peer { token: "renewed-a".into(), ..a });
        assert!(s.peer_for_token("secret-a").is_none());
        assert!(s.peer_for_token("secret-b").is_some());
        assert!(s.remove_token("secret-a").is_none());
        s.remove_token("renewed-a");
        assert_eq!(s.connections().len(), 1);
        assert_eq!(s.active.as_ref().unwrap().id, "b");
        assert!(s.connected_to(&b));
        s.mark_address(&b.address, false);
        assert!(!s.connected_to(&b));
        assert!(s.select("unknown").is_err());
    }
    #[test]
    fn reconnecting_one_endpoint_does_not_disconnect_other_endpoints() {
        let mut s = Binding::default();
        let peer = |id: &str, address: &str| Peer { id: id.into(), address: address.into(), token: id.into(), project: id.into(), root: id.into() };
        s.put(peer("old", "127.0.0.1:19001"));
        s.put(peer("other", "127.0.0.1:19002"));
        s.put(peer("new", "127.0.0.1:19001"));
        assert!(s.peer_for_token("old").is_none());
        assert!(s.peer_for_token("other").is_some());
        assert_eq!(s.connections().len(), 2);
        s.mark_address("127.0.0.1:19001", false);
        assert!(s.connected_to(s.peer_for_token("other").unwrap()));
        assert!(!s.connected_to(s.peer_for_token("new").unwrap()));
        s.select("other").unwrap();
        assert!(s.connected);
    }
    #[test]
    fn connection_commands_reject_missing_or_unknown_ids_without_panicking() {
        for id in ["defold.selectBinding", "defold.unbind"] {
            for params in [Value::Null, json!({}), json!({"id":-1}), json!({"id":"unknown"})] {
                let mut session = crate::Session::new();
                assert!(session.execute(id, params).is_err());
            }
        }
    }
    #[test]
    fn ip_binding_rejects_hostnames_and_urls() {
        assert_eq!(address("127.0.0.1", DEFOLD_PORT).unwrap().port(), DEFOLD_PORT);
        assert!(address("192.168.0.2:17654", DEFOLD_PORT).is_ok());
        for s in ["localhost", "http://127.0.0.1", "127.0.0.1\r\nX: bad", ""] {
            assert!(address(s, DEFOLD_PORT).is_err());
        }
    }
    #[test]
    fn project_identity_does_not_depend_on_a_gui_or_path_spelling() {
        assert!(same_project("/project/a/", "/project/a"));
        assert!(!same_project("", ""));
        assert!(!same_project("/project/a", "/project/b"));
        if cfg!(windows) {
            assert!(same_project("X:\\Projs\\Game", "x:/projs/game/"));
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn local_project_binding_never_mirrors_or_overwrites_existing_resources() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("game.project"), "project").unwrap();
        std::fs::write(root.path().join("work.gui.pcraft"), "unsaved design source").unwrap();
        let mut peer = Peer {
            id: "editor".into(),
            address: "127.0.0.1:1".into(),
            token: "ticket".into(),
            project: root.path().to_string_lossy().into_owned(),
            root: String::new(),
        };
        native::mirror(&mut peer).unwrap(); // Port 1 is deliberately unreachable: no network is needed.
        assert!(same_project(&peer.root, &peer.project));
        assert_eq!(std::fs::read_to_string(root.path().join("work.gui.pcraft")).unwrap(), "unsaved design source");
        assert!(!root.path().join(".photocraft-remote").exists());
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn remote_resources_cannot_escape_or_read_session_files() {
        for s in ["../secret", "/.internal/editor.token", "C:/secret", "a\\b", "//host/file", "a/./b", "a//b"] {
            assert!(native::safe_relative(s).is_err(), "{s}");
        }
        assert!(native::safe_relative("/assets/gui/menu.gui").is_ok());
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn binding_cannot_commit_before_consent_or_reveal_its_token() {
        let saved = std::mem::take(&mut *state());
        state().id = "craft".into();
        let offer = native::route_request("/binding/request", "", &json!({"id":"a".repeat(64),"address":"127.0.0.1:17654"})).unwrap();
        let ticket = offer["ticket"].as_str().unwrap();
        assert_eq!(native::route_request("/binding/result", ticket, &json!({})).unwrap()["phase"], "confirm");
        assert!(native::route_request("/binding/commit", ticket, &json!({})).is_err());
        assert!(native::route_request("/binding/result", "wrong", &json!({})).is_err());
        assert!(!inspect().to_string().contains(ticket));
        state().incoming.as_mut().unwrap().phase = "ready".into();
        native::route_request("/binding/commit", ticket, &json!({})).unwrap();
        assert!(state().active.is_none(), "Only the UI may commit after rechecking files and workers");
        native::route_request("/binding/cancel", ticket, &json!({})).unwrap();
        assert!(state().incoming.is_none());
        state().active =
            Some(Peer { id: "defold".into(), address: "127.0.0.1:17654".into(), token: "private".into(), project: "game".into(), root: String::new() });
        assert!(!inspect().to_string().contains("private"));
        native::route_request("/binding/release", "old-token", &json!({})).unwrap();
        assert!(state().active.is_some());
        native::route_request("/binding/release", "private", &json!({})).unwrap();
        assert!(state().active.is_none());
        *state() = saved;
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn transport_decodes_chunked_defold_responses_and_rejects_truncation() {
        use std::io::{Read, Write};
        for (body, expected) in [
            ("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n", Some(b"abcde".to_vec())),
            ("HTTP/1.0 200 OK\r\nContent-Length: 5\r\n\r\nabc", None),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let target = listener.local_addr().unwrap().to_string();
            let thread = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut b = [0];
                    stream.read_exact(&mut b).unwrap();
                    request.push(b[0]);
                }
                assert!(String::from_utf8(request).unwrap().contains("Authorization: Bearer session"));
                stream.write_all(body.as_bytes()).unwrap();
            });
            let result = native::http(&target, "/binding/status", "session", &[], "POST");
            if let Some(expected) = expected {
                assert_eq!(result.unwrap(), expected);
            } else {
                assert!(result.is_err());
            }
            thread.join().unwrap();
        }
    }
}
