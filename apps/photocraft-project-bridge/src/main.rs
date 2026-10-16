//! Private, project-scoped host for native PhotoCraft clients.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo, clippy::unimplemented, clippy::unreachable)]

use photocraft_engine::defold_project_local::{LocalProjectStore, host_ticket_path};
use photocraft_engine::defold_project_net::{self as net, HostTicket, Response};
use photocraft_engine::defold_project_remote::dispatch;
use photocraft_engine::defold_project_store::ProjectStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::fs;
use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

fn main() {
    if let Err(error) = run() {
        eprintln!("PhotoCraft project host: {error}");
        std::process::exit(1);
    }
}

struct Options {
    project: PathBuf,
    listen: SocketAddr,
    allow_lan: bool,
    pairing: Option<PathBuf>,
    rotate: bool,
    idle_seconds: u64,
}
fn options() -> Result<Options, String> {
    let mut project = None;
    let mut listen = "127.0.0.1:0".parse::<SocketAddr>().map_err(|e| e.to_string())?;
    let mut allow_lan = false;
    let mut pairing = None;
    let mut rotate = false;
    let mut idle_seconds = 1800;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--project" => project = Some(PathBuf::from(args.next().ok_or("--project requires a directory")?)),
            "--listen" => listen = args.next().ok_or("--listen requires an IP and port")?.parse::<SocketAddr>().map_err(|e| e.to_string())?,
            "--allow-lan" => allow_lan = true,
            "--export-pairing" => pairing = Some(PathBuf::from(args.next().ok_or("--export-pairing requires a private output file")?)),
            "--rotate-token" => rotate = true,
            "--idle-seconds" => {
                idle_seconds = args.next().ok_or("--idle-seconds requires a duration")?.parse::<u64>().map_err(|e| e.to_string())?.clamp(30, 86400)
            }
            _ => {
                return Err(
                    "Usage: photocraft-project-bridge --project DIR [--listen IP:PORT --allow-lan] [--export-pairing FILE] [--rotate-token] [--idle-seconds N]"
                        .into(),
                );
            }
        }
    }
    if !listen.ip().is_loopback() && !allow_lan {
        return Err("LAN access requires --allow-lan".into());
    }
    Ok(Options { project: project.ok_or("--project is required")?, listen, allow_lan, pairing, rotate, idle_seconds })
}

fn private_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("Missing private settings directory")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file().set_permissions(fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    }
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.error.to_string())?;
    Ok(())
}

fn tls_config(certificate: &[u8], key: Vec<u8>) -> Result<Arc<rustls::ServerConfig>, String> {
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(vec![CertificateDer::from(certificate.to_vec())], PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)))
        .map_err(|e| e.to_string())?;
    Ok(Arc::new(config))
}

fn configure_client(socket: &std::net::TcpStream) -> Result<(), String> {
    // Windows accepted sockets can inherit the listener's nonblocking mode.
    // Each client has its own worker, so TLS must use a blocking socket here.
    socket.set_nonblocking(false).map_err(|e| e.to_string())?;
    socket.set_read_timeout(Some(Duration::from_secs(60))).map_err(|e| e.to_string())?;
    socket.set_write_timeout(Some(Duration::from_secs(60))).map_err(|e| e.to_string())
}

fn credentials(directory: &Path, rotate: bool) -> Result<(Vec<u8>, Vec<u8>, String), String> {
    let certificate = directory.join("certificate.der");
    let private_key = directory.join("private-key.der");
    if !certificate.is_file() || !private_key.is_file() {
        let key = rcgen::generate_simple_self_signed(vec!["photocraft-project".into(), "localhost".into()]).map_err(|e| e.to_string())?;
        private_write(&private_key, &key.signing_key.serialize_der())?;
        private_write(&certificate, key.cert.der().as_ref())?;
    }
    let token_path = directory.join("session-token");
    if rotate || !token_path.is_file() {
        private_write(&token_path, photocraft_engine::peer_binding::identity().map_err(|e| e.to_string())?.as_bytes())?;
    }
    let token = fs::read_to_string(token_path).map_err(|e| e.to_string())?;
    if token.len() != 64 || !token.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("Private project credential is malformed".into());
    }
    Ok((fs::read(certificate).map_err(|e| e.to_string())?, fs::read(private_key).map_err(|e| e.to_string())?, token))
}

struct ConnectionCount(Arc<AtomicUsize>);
impl Drop for ConnectionCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn run() -> Result<(), String> {
    let options = options()?;
    let store = LocalProjectStore::open(&options.project).map_err(|e| e.to_string())?;
    let ticket_path = host_ticket_path(store.root()).map_err(|e| e.to_string())?;
    let private = ticket_path.parent().ok_or("Missing private host directory")?;
    let (certificate, key, token) = credentials(private, options.rotate)?;
    let config = tls_config(&certificate, key)?;
    let listener = TcpListener::bind(options.listen).map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    if options.allow_lan && address.ip().is_unspecified() && options.pairing.is_some() {
        return Err("Export a pairing file using an explicit reachable host IP, not 0.0.0.0".into());
    }
    let ticket = HostTicket { address, certificate_der: certificate, token };
    let bytes = serde_json::to_vec_pretty(&ticket).map_err(|e| e.to_string())?;
    private_write(&ticket_path, &bytes)?;
    if let Some(path) = &options.pairing {
        private_write(path, &bytes)?;
    }
    let description = store.describe().map_err(|e| e.to_string())?;
    println!("PhotoCraft project host ready: {} at {address}", description.display_name);
    let stop = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicUsize::new(0));
    let last_used = Arc::new(std::sync::Mutex::new(Instant::now()));
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((socket, _)) => {
                if active.fetch_add(1, Ordering::SeqCst) >= 4 {
                    active.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                let count = ConnectionCount(Arc::clone(&active));
                let store = Arc::clone(&store);
                let config = Arc::clone(&config);
                let token = ticket.token.clone();
                let stop = Arc::clone(&stop);
                let used = Arc::clone(&last_used);
                let _ = std::thread::Builder::new().name("project-client".into()).spawn(move || {
                    let _count = count;
                    let serve = || -> Result<(), String> {
                        configure_client(&socket)?;
                        let connection = rustls::ServerConnection::new(config).map_err(|e| e.to_string())?;
                        let mut stream = rustls::StreamOwned::new(connection, socket);
                        net::serve_one(&mut stream, &token, |route, body| {
                            *used.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
                            if route == "/v1/host/shutdown" {
                                stop.store(true, Ordering::SeqCst);
                                Response::json(200, &serde_json::json!({"stopping":true}))
                            } else {
                                dispatch(store.as_ref(), route, body)
                            }
                        })
                    };
                    // Malformed input does not terminate the authority or expose request contents.
                    if serve().is_err() {
                        eprintln!("Project client disconnected or sent an invalid request");
                    }
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if active.load(Ordering::SeqCst) == 0
                    && last_used.lock().unwrap_or_else(std::sync::PoisonError::into_inner).elapsed().as_secs() >= options.idle_seconds
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    while active.load(Ordering::SeqCst) != 0 {
        std::thread::sleep(Duration::from_millis(25));
    }
    // A stale endpoint is harmless: its token cannot acquire the released OS lock.
    let _ = fs::remove_file(&ticket_path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_engine::defold_project_model::{
        OperationRequest, ProjectErrorCode, ResourcePath, ResourceReadRequest, SceneIdentityRequest, SceneRequest, SharedAssetRequest, new_id,
    };
    use photocraft_engine::defold_project_remote::RemoteProjectStore;

    #[test]
    fn paired_tls_connection_uses_the_production_project_router() {
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("game.project"), b"[project]\ntitle = Fixture\n").unwrap();
        fs::write(project.path().join("test.gui"), b"nodes { type: TYPE_BOX id: \"root\" }\n").unwrap();
        let store = LocalProjectStore::open(project.path()).unwrap();
        let secret = tempfile::tempdir().unwrap();
        let (certificate, key, token) = credentials(secret.path(), false).unwrap();
        let config = tls_config(&certificate, key).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let ticket = HostTicket { address: listener.local_addr().unwrap(), certificate_der: certificate, token: token.clone() };
        let server = std::thread::spawn(move || {
            for _ in 0..7 {
                let (socket, _) = listener.accept().unwrap();
                socket.set_nonblocking(true).unwrap();
                configure_client(&socket).unwrap();
                let connection = rustls::ServerConnection::new(Arc::clone(&config)).unwrap();
                let mut stream = rustls::StreamOwned::new(connection, socket);
                net::serve_one(&mut stream, &token, |route, bytes| dispatch(store.as_ref(), route, bytes)).unwrap();
            }
        });
        let client = RemoteProjectStore::new(ticket);
        let description = client.describe().unwrap();
        let scene = client
            .resolve_scene(&ResourceReadRequest { context: description.context.clone(), path: ResourcePath::parse("/test.gui").unwrap(), expected_hash: None })
            .unwrap();
        assert_eq!(scene.path.as_str(), "/test.gui");
        assert_eq!(client.scene_identity(&SceneIdentityRequest { context: description.context.clone(), scene_id: scene.scene_id.clone() }).unwrap(), scene);
        assert_eq!(
            client.shared_asset_usage(&SharedAssetRequest { context: description.context.clone(), asset_id: new_id().unwrap() }).unwrap_err().code,
            ProjectErrorCode::ResourceMissing
        );
        let assets = client.asset_management(&description.context).unwrap();
        assert_eq!(assets.context, description.context);
        assert!(!assets.roots_complete);
        assert!(client.migration_report(&OperationRequest { context: description.context.clone(), operation_id: new_id().unwrap() }).unwrap().is_none());
        assert!(client.published_scene_baseline(&SceneRequest { context: description.context, scene_id: scene.scene_id, path: scene.path }).unwrap().is_none());
        server.join().unwrap();
    }
}
