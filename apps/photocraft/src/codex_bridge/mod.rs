//! A desktop-owned app-server with authenticated local clients and one writer per turn.
mod router;

use router::{Action, Router};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

const MAX_FRAME: usize = 48 * 1024 * 1024;
const MAX_CLIENTS: usize = 4;
type Sender = mpsc::SyncSender<Value>;

pub fn default_dir() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("PHOTOCRAFT_CODEX_BRIDGE_DIR") {
        return Ok(PathBuf::from(p));
    }
    #[cfg(windows)]
    let root = std::env::var_os("APPDATA").map(PathBuf::from).ok_or("APPDATA is unavailable")?;
    #[cfg(target_os = "macos")]
    let root = std::env::var_os("HOME").map(PathBuf::from).ok_or("HOME is unavailable")?.join("Library/Application Support");
    #[cfg(not(any(windows, target_os = "macos")))]
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
        .ok_or("Configuration directory unavailable")?;
    Ok(root.join("Photocraft/CodexBridge"))
}

fn read_json(path: &Path, limit: usize) -> Result<Value, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| format!("Cannot inspect bridge metadata: {e}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit as u64 {
        return Err("Invalid bridge metadata file".into());
    }
    let file = std::fs::File::open(path).map_err(|e| format!("Cannot open bridge metadata: {e}"))?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1) as u64).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("Bridge metadata exceeds size limit".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("Invalid bridge metadata: {e}"))
}

fn frame(reader: &mut impl BufRead, limit: usize) -> Result<Option<Value>, String> {
    let mut bytes = Vec::new();
    let n = reader.take(limit.saturating_add(1) as u64).read_until(b'\n', &mut bytes).map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(None);
    }
    if bytes.len() > limit || bytes.last() != Some(&b'\n') {
        return Err("Bridge frame is oversized or incomplete".into());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if !value.is_object() {
        return Err("Bridge frame must be an object".into());
    }
    Ok(Some(value))
}

fn write_frame(writer: &mut impl Write, value: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *writer, value).map_err(|e| e.to_string())?;
    writer.write_all(b"\n").and_then(|()| writer.flush()).map_err(|e| e.to_string())
}

fn hidden(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    #[cfg(not(windows))]
    let _ = command;
}

fn is_server(args: &[OsString]) -> bool {
    args.iter().position(|a| a == "app-server").is_some_and(|i| {
        let tail = args.get(i.saturating_add(1)..).unwrap_or_default();
        !tail.iter().any(|a| matches!(a.to_str(), Some("--help" | "-h" | "generate-ts" | "generate-json-schema" | "--version" | "-V")))
            && !tail.windows(2).any(|pair| pair.first().is_some_and(|a| a == "--listen") && pair.get(1).is_some_and(|a| a != "stdio://"))
            && !tail.iter().any(|a| a.to_str().is_some_and(|s| s.starts_with("--listen=") && s != "--listen=stdio://"))
    })
}

pub fn run(args: Vec<OsString>) -> Result<i32, String> {
    let dir = default_dir()?;
    if args.first().is_some_and(|a| a == "--photocraft-client") {
        return client(&dir);
    }
    let config = read_json(&dir.join("config.json"), 64 * 1024)?;
    let real = config.get("realCodex").and_then(Value::as_str).map(PathBuf::from).ok_or("Missing real Codex executable")?;
    let same_executable = std::fs::canonicalize(&real)
        .ok()
        .zip(std::env::current_exe().ok().and_then(|p| std::fs::canonicalize(p).ok()))
        .is_some_and(|(real, current)| real == current);
    if !real.is_absolute() || !real.is_file() || same_executable {
        return Err("Invalid real Codex executable".into());
    }
    if config.get("enabled").and_then(Value::as_bool) == Some(true) && is_server(&args) {
        broker(&dir, &real, &args)
    } else {
        let mut command = Command::new(real);
        command.args(args);
        hidden(&mut command);
        command.status().map(|s| s.code().unwrap_or(1)).map_err(|e| e.to_string())
    }
}

fn template(dir: &Path, v: &mut Value) -> Result<(), String> {
    if v.get("method").and_then(Value::as_str) != Some("thread/resume") {
        return Ok(());
    }
    let Some(id) = v.pointer("/params/threadId").and_then(Value::as_str) else { return Ok(()) };
    let path = dir.join("templates.json");
    if !path.exists() {
        return Ok(());
    }
    let templates = read_json(&path, 8 * 1024 * 1024)?;
    let Some(saved) = templates.get(id) else { return Ok(()) };
    let workspace = saved.get("workspace").and_then(Value::as_str).ok_or("Registered design workspace is missing")?;
    if !Path::new(workspace).is_absolute() {
        return Err("Registered design workspace must be absolute".into());
    }
    let settings = saved.get("params").and_then(Value::as_object).ok_or("Registered design settings are invalid")?;
    let params = v.get_mut("params").and_then(Value::as_object_mut).ok_or("Missing thread resume parameters")?;
    for (key, value) in settings {
        if !matches!(key.as_str(), "threadId" | "projectId") {
            params.insert(key.clone(), value.clone());
        }
    }
    params.insert("cwd".into(), Value::String(workspace.into()));
    Ok(())
}

enum Event {
    Input(usize, Value),
    Backend(Value),
    Connected(usize, Sender, TcpStream),
    Gone(usize),
    BackendGone,
}

fn enqueue(sender: &Sender, mut value: Value) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match sender.try_send(value) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
            Err(mpsc::TrySendError::Full(returned)) => {
                if Instant::now() >= deadline {
                    return false;
                }
                value = returned;
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

fn send_actions(actions: Vec<Action>, backend: &mut impl Write, clients: &mut HashMap<usize, Sender>) -> Result<(), String> {
    for action in actions {
        match action {
            Action::Backend(v) => write_frame(backend, &v)?,
            Action::Client(id, v) => {
                if clients.get(&id).is_some_and(|c| !enqueue(c, v)) {
                    clients.remove(&id);
                }
            }
            Action::Broadcast(v) => clients.retain(|_, c| enqueue(c, v.clone())),
        }
    }
    Ok(())
}

fn authenticate(stream: TcpStream, token: &str) -> Result<(BufReader<TcpStream>, TcpStream), String> {
    if !stream.peer_addr().map_err(|e| e.to_string())?.ip().is_loopback() {
        return Err("Non-local client refused".into());
    }
    // Windows accepts can inherit the nonblocking listener's mode.
    stream.set_nonblocking(false).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(Duration::from_secs(3))).map_err(|e| e.to_string())?;
    let mut auth_reader = stream.try_clone().map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline.checked_duration_since(Instant::now()).ok_or("Authentication timed out")?;
        auth_reader.set_read_timeout(Some(remaining)).map_err(|e| e.to_string())?;
        let mut byte = [0u8; 1];
        auth_reader.read_exact(&mut byte).map_err(|e| e.to_string())?;
        bytes.extend(byte);
        if bytes.len() > 4096 {
            return Err("Authentication frame exceeds size limit".into());
        }
        if byte.first() == Some(&b'\n') {
            break;
        }
    }
    let hello: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid authentication frame")?;
    let actual = hello.get("token").and_then(Value::as_str).unwrap_or("");
    let equal = actual.len() == token.len() && actual.bytes().zip(token.bytes()).fold(0u8, |a, (b, c)| a | (b ^ c)) == 0;
    if hello.get("version").and_then(Value::as_u64) != Some(1) || !equal {
        return Err("Authentication failed".into());
    }
    stream.set_read_timeout(None).map_err(|e| e.to_string())?;
    // Windows socket clones can retain their own authentication receive timeout.
    auth_reader.set_read_timeout(None).map_err(|e| e.to_string())?;
    let reader = BufReader::new(auth_reader);
    let mut writer = stream;
    write_frame(&mut writer, &json!({"ok":true,"version":1}))?;
    Ok((reader, writer))
}

fn accept_loop(listener: TcpListener, token: String, tx: mpsc::SyncSender<Event>, stop: Arc<AtomicBool>) {
    let count = Arc::new(AtomicUsize::new(0));
    let mut serial = 0usize;
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                if count.load(Ordering::Relaxed) >= MAX_CLIENTS {
                    continue;
                }
                serial = serial.saturating_add(1);
                let id = serial;
                let token = token.clone();
                let tx = tx.clone();
                let count = count.clone();
                count.fetch_add(1, Ordering::Relaxed);
                std::thread::spawn(move || {
                    if let Ok((mut reader, mut writer)) = authenticate(stream, &token) {
                        let (out, rx) = mpsc::sync_channel::<Value>(4);
                        if let Ok(socket) = writer.try_clone()
                            && tx.send(Event::Connected(id, out, socket)).is_ok()
                        {
                            std::thread::spawn(move || {
                                while let Ok(v) = rx.recv() {
                                    if write_frame(&mut writer, &v).is_err() {
                                        break;
                                    }
                                }
                                let _ = writer.shutdown(Shutdown::Both);
                            });
                            while let Ok(Some(v)) = frame(&mut reader, MAX_FRAME) {
                                if tx.send(Event::Input(id, v)).is_err() {
                                    break;
                                }
                            }
                            let _ = tx.send(Event::Gone(id));
                        }
                    }
                    count.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => break,
        }
    }
}

fn broker(dir: &Path, real: &Path, args: &[OsString]) -> Result<i32, String> {
    let owner_lock = std::fs::File::options().create(true).truncate(false).read(true).write(true).open(dir.join("owner.lock")).map_err(|e| e.to_string())?;
    owner_lock.try_lock().map_err(|_| "A shared Codex desktop backend is already running")?;
    let endpoint_path = dir.join("endpoint.json");
    if let Ok(old) = read_json(&endpoint_path, 4096)
        && let Some(address) = old.get("address").and_then(Value::as_str).and_then(|s| s.parse::<SocketAddr>().ok())
        && address.ip().is_loopback()
        && TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok()
    {
        return Err("A shared Codex desktop backend is already running".into());
    }
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let token = photocraft_automation::security::generate_token().map_err(|e| e.to_string())?;
    let mut command = Command::new(real);
    command.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
    hidden(&mut command);
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let result = (|| {
        let mut input = child.stdin.take().ok_or("Backend stdin unavailable")?;
        let output = child.stdout.take().ok_or("Backend stdout unavailable")?;
        let mut descriptor = tempfile::NamedTempFile::new_in(dir).map_err(|e| e.to_string())?;
        serde_json::to_writer(&mut descriptor, &json!({"version":1,"address":address.to_string(),"token":token,"pid":std::process::id()}))
            .map_err(|e| e.to_string())?;
        descriptor.flush().map_err(|e| e.to_string())?;
        descriptor.persist(&endpoint_path).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel(4);
        let accept_tx = tx.clone();
        let accept_stop = stop.clone();
        let accept = std::thread::spawn(move || accept_loop(listener, token, accept_tx, accept_stop));
        let back_tx = tx.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            let mut forwarding = true;
            while let Ok(Some(v)) = frame(&mut reader, MAX_FRAME) {
                if forwarding && back_tx.send(Event::Backend(v)).is_err() {
                    forwarding = false;
                }
            }
            let _ = back_tx.send(Event::BackendGone);
        });
        let desktop_tx = tx.clone();
        std::thread::spawn(move || {
            let mut reader = std::io::stdin().lock();
            while let Ok(Some(v)) = frame(&mut reader, MAX_FRAME) {
                if desktop_tx.send(Event::Input(0, v)).is_err() {
                    return;
                }
            }
            let _ = desktop_tx.send(Event::Gone(0));
        });
        let (desktop_out, desktop_rx) = mpsc::sync_channel(4);
        let desktop_write_tx = tx.clone();
        std::thread::spawn(move || {
            let mut writer = std::io::stdout().lock();
            while let Ok(v) = desktop_rx.recv() {
                if write_frame(&mut writer, &v).is_err() {
                    break;
                }
            }
            let _ = desktop_write_tx.send(Event::Gone(0));
        });
        let mut clients = HashMap::from([(0, desktop_out)]);
        let mut sockets = HashMap::new();
        let mut router = Router::default();
        let outcome = loop {
            let event = match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(e) => e,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                        break Ok(());
                    }
                    continue;
                }
                Err(_) => break Ok(()),
            };
            let actions = match event {
                Event::Input(id, mut v) => {
                    if !clients.contains_key(&id) {
                        continue;
                    }
                    if let Err(message) = template(dir, &mut v) {
                        vec![Action::Client(id, json!({"id":v.get("id"),"error":{"code":-32001,"message":message}}))]
                    } else {
                        router.client(id, v)
                    }
                }
                Event::Backend(v) => router.backend(v),
                Event::Connected(id, out, socket) => {
                    clients.insert(id, out);
                    sockets.insert(id, socket);
                    continue;
                }
                Event::Gone(0) | Event::BackendGone => break Ok(()),
                Event::Gone(id) => {
                    clients.remove(&id);
                    sockets.remove(&id);
                    router.disconnected(id);
                    continue;
                }
            };
            if let Err(e) = send_actions(actions, &mut input, &mut clients) {
                break Err(e);
            }
            if !clients.contains_key(&0) {
                break Err("Desktop connection stopped reading".into());
            }
            sockets.retain(|id, s| {
                if clients.contains_key(id) {
                    true
                } else {
                    let _ = s.shutdown(Shutdown::Both);
                    false
                }
            });
        };
        stop.store(true, Ordering::Relaxed);
        for socket in sockets.values() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        drop(input);
        drop(clients);
        drop(rx);
        let _ = accept.join();
        outcome
    })();
    let deadline = Instant::now() + Duration::from_secs(3);
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code().unwrap_or(1),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break 1;
            }
        }
    };
    if read_json(&endpoint_path, 4096).ok().and_then(|v| v.get("pid").and_then(Value::as_u64)) == Some(u64::from(std::process::id())) {
        let _ = std::fs::remove_file(endpoint_path);
    }
    result.map(|()| code)
}

fn client(dir: &Path) -> Result<i32, String> {
    let info = read_json(&dir.join("endpoint.json"), 4096)?;
    let address: SocketAddr =
        info.get("address").and_then(Value::as_str).ok_or("Missing shared connection address")?.parse().map_err(|_| "Invalid shared connection address")?;
    if !address.ip().is_loopback() {
        return Err("Shared connection must be local".into());
    }
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(3))
        .map_err(|_| "Shared Codex connection is unavailable. Start Codex with the PhotoCraft bridge enabled.")?;
    stream.set_read_timeout(Some(Duration::from_secs(4))).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(Duration::from_secs(4))).map_err(|e| e.to_string())?;
    write_frame(&mut stream, &json!({"version":1,"token":info.get("token")}))?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    if frame(&mut reader, 4096)?.as_ref().and_then(|v| v.get("ok")).and_then(Value::as_bool) != Some(true) {
        return Err("Shared connection authentication failed".into());
    }
    stream.set_read_timeout(None).map_err(|e| e.to_string())?;
    reader.get_ref().set_read_timeout(None).map_err(|e| e.to_string())?;
    let shutdown = stream.try_clone().map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        let mut input = std::io::stdin().lock();
        while let Ok(Some(v)) = frame(&mut input, MAX_FRAME) {
            if write_frame(&mut stream, &v).is_err() {
                break;
            }
        }
        let _ = stream.shutdown(Shutdown::Both);
    });
    let mut stdout = std::io::stdout().lock();
    while let Some(v) = frame(&mut reader, MAX_FRAME)? {
        write_frame(&mut stdout, &v)?;
    }
    let _ = shutdown.shutdown(Shutdown::Both);
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_protocol_rejects_large_and_incomplete_frames() {
        assert!(frame(&mut std::io::Cursor::new(b"{\"long\":12345}\n"), 8).is_err());
        assert!(frame(&mut std::io::Cursor::new(b"{}"), 8).is_err());
        assert_eq!(frame(&mut std::io::Cursor::new(b"{}\n"), 8).unwrap(), Some(json!({})));
    }
    #[test]
    fn healthy_clients_survive_notification_bursts() {
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(json!({"first":true})).unwrap();
        let reader = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            assert_eq!(rx.recv().unwrap()["first"], true);
            assert_eq!(rx.recv().unwrap()["second"], true);
        });
        assert!(enqueue(&tx, json!({"second":true})));
        reader.join().unwrap();
    }
    #[test]
    fn only_registered_resume_receives_settings() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("templates.json"),
            json!({"same":{"workspace":dir.path(),"params":{"threadId":"wrong","projectId":"wrong","developerInstructions":"design","config":{"a":1}}}})
                .to_string(),
        )
        .unwrap();
        let mut v = json!({"method":"thread/resume","params":{"threadId":"same","projectId":"original"}});
        template(dir.path(), &mut v).unwrap();
        assert_eq!(v["params"]["threadId"], "same");
        assert_eq!(v["params"]["projectId"], "original");
        assert_eq!(v["params"]["developerInstructions"], "design");
        let mut other = json!({"method":"thread/resume","params":{"threadId":"other"}});
        let before = other.clone();
        template(dir.path(), &mut other).unwrap();
        assert_eq!(other, before);
    }
    #[test]
    fn local_auth_and_eof() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let task = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let (mut reader, _writer) = authenticate(stream, "secret").unwrap();
            assert!(frame(&mut reader, 1024).unwrap().is_none());
        });
        let mut client = TcpStream::connect(address).unwrap();
        write_frame(&mut client, &json!({"version":1,"token":"secret"})).unwrap();
        let mut reader = BufReader::new(client.try_clone().unwrap());
        assert_eq!(frame(&mut reader, 4096).unwrap().unwrap()["ok"], true);
        client.shutdown(Shutdown::Both).unwrap();
        task.join().unwrap();
    }
    #[test]
    fn authenticated_reader_survives_idle_past_handshake_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let task = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let (mut reader, mut writer) = authenticate(stream, "secret").unwrap();
            assert_eq!(reader.get_ref().read_timeout().unwrap(), None);
            let value = frame(&mut reader, MAX_FRAME).unwrap().unwrap();
            assert_eq!(value["payload"].as_str().unwrap().len(), 2 * 1024 * 1024);
            write_frame(&mut writer, &value).unwrap();
        });
        let mut client = TcpStream::connect(address).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
        write_frame(&mut client, &json!({"version":1,"token":"secret"})).unwrap();
        let mut reader = BufReader::new(client.try_clone().unwrap());
        assert_eq!(frame(&mut reader, 4096).unwrap().unwrap()["ok"], true);
        std::thread::sleep(Duration::from_secs(4));
        let value = json!({"payload":"x".repeat(2 * 1024 * 1024)});
        write_frame(&mut client, &value).unwrap();
        assert_eq!(frame(&mut reader, MAX_FRAME).unwrap().unwrap(), value);
        task.join().unwrap();
    }
    #[test]
    fn nonblocking_accept_waits_for_delayed_authentication_and_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let task = std::thread::spawn(move || {
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(1)),
                    Err(e) => panic!("accept failed: {e}"),
                }
            };
            // Exercise the inherited Windows mode on all platforms.
            stream.set_nonblocking(true).unwrap();
            let (mut reader, _writer) = authenticate(stream, "secret").unwrap();
            assert_eq!(frame(&mut reader, 4096).unwrap().unwrap()["method"], "initialize");
        });
        let mut client = TcpStream::connect(address).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        write_frame(&mut client, &json!({"version":1,"token":"secret"})).unwrap();
        let mut reader = BufReader::new(client.try_clone().unwrap());
        assert_eq!(frame(&mut reader, 4096).unwrap().unwrap()["ok"], true);
        std::thread::sleep(Duration::from_millis(30));
        write_frame(&mut client, &json!({"id":1,"method":"initialize"})).unwrap();
        task.join().unwrap();
    }
    #[test]
    fn wrong_auth_is_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let task = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            assert!(authenticate(stream, "secret").is_err());
        });
        let mut client = TcpStream::connect(address).unwrap();
        write_frame(&mut client, &json!({"version":1,"token":"wrong"})).unwrap();
        task.join().unwrap();
    }
}
