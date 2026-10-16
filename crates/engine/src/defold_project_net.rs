//! Small authenticated TLS transport for a deliberately selected project host.
//! The supplied certificate is the paired host's trust anchor, not a web PKI fallback.
use rustls::pki_types::{CertificateDer, ServerName};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

const HEADER_LIMIT: usize = 16 * 1024;
pub const BODY_LIMIT: usize = crate::defold_project_wire::TRANSFER_LIMIT;

/// Contains a credential: persist only in private connection/recovery settings.
/// Do not return this object from project inspection or serialize into Document.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostTicket {
    pub address: SocketAddr,
    pub certificate_der: Vec<u8>,
    pub token: String,
}

impl std::fmt::Debug for HostTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostTicket").field("address", &self.address).finish_non_exhaustive()
    }
}

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json(status: u16, value: &impl Serialize) -> Result<Self, String> {
        Ok(Self { status, content_type: "application/json", body: serde_json::to_vec(value).map_err(|e| e.to_string())? })
    }
    pub fn binary(body: Vec<u8>) -> Self {
        Self { status: 200, content_type: "application/octet-stream", body }
    }
}

pub fn call(ticket: &HostTicket, route: &str, body: &[u8]) -> Result<Vec<u8>, String> {
    if !route.starts_with("/v1/") || route.contains(['\r', '\n', ' ']) || body.len() > BODY_LIMIT {
        return Err("Invalid project request".into());
    }
    validate_token(&ticket.token)?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(ticket.certificate_der.clone())).map_err(|_| "Invalid paired host certificate")?;
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let socket =
        TcpStream::connect_timeout(&ticket.address, Duration::from_secs(3)).map_err(|_| "Project host is unavailable; your local draft is preserved")?;
    socket.set_read_timeout(Some(Duration::from_secs(60))).map_err(|e| e.to_string())?;
    socket.set_write_timeout(Some(Duration::from_secs(60))).map_err(|e| e.to_string())?;
    let name = ServerName::try_from("photocraft-project").map_err(|_| "Invalid project host identity")?;
    let connection = rustls::ClientConnection::new(Arc::new(config), name).map_err(|e| e.to_string())?;
    let mut stream = rustls::StreamOwned::new(connection, socket);
    write!(stream, "POST {route} HTTP/1.1\r\nHost: photocraft-project\r\nAuthorization: Bearer {}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", ticket.token, body.len()).map_err(|_| "Project request interrupted; query operation status before retrying")?;
    stream.write_all(body).map_err(|_| "Project upload interrupted; local draft is preserved")?;
    stream.flush().map_err(|e| e.to_string())?;
    let (first, headers, bytes) = read_message(&mut stream)?;
    let _ = headers;
    let status = first.split_whitespace().nth(1).ok_or("Invalid project response")?;
    if status != "200" {
        // Error DTOs contain operation context but never the request credential.
        return Err(String::from_utf8(bytes).unwrap_or_else(|_| "Project host rejected the request".into()));
    }
    Ok(bytes)
}

pub fn serve_one<S: Read + Write>(stream: &mut S, token: &str, handler: impl FnOnce(&str, &[u8]) -> Result<Response, String>) -> Result<(), String> {
    validate_token(token)?;
    let (first, headers, size) = read_head(stream)?;
    let mut request = first.split_whitespace();
    let method = request.next().ok_or("Missing project request method")?;
    let path = request.next().ok_or("Missing project request route")?;
    let auth = headers.iter().find(|(name, _)| name == "authorization").and_then(|(_, value)| value.strip_prefix("Bearer "));
    let response = if !auth.is_some_and(|candidate| equal_token(candidate, token)) {
        Response::json(401, &serde_json::json!({"code":"UNAUTHORIZED","message":"Project pairing is required"}))?
    } else if method != "POST" || !path.starts_with("/v1/") || request.next() != Some("HTTP/1.1") || request.next().is_some() {
        Response::json(400, &serde_json::json!({"code":"INVALID_REQUEST","message":"Invalid project request"}))?
    } else {
        let body = read_body(stream, size)?;
        match handler(path, &body) {
            Ok(response) => response,
            Err(message) => Response::json(409, &serde_json::json!({"code":"PROJECT_ERROR","message":message}))?,
        }
    };
    if response.body.len() > BODY_LIMIT {
        return Err("Project response exceeds transfer limit".into());
    }
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Conflict",
    };
    write!(
        stream,
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.content_type,
        response.body.len()
    )
    .map_err(|e| e.to_string())?;
    stream.write_all(&response.body).map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())
}

fn validate_token(token: &str) -> Result<(), String> {
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid project session credential".into());
    }
    Ok(())
}

fn equal_token(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0_u8, |difference, (a, b)| difference | (a ^ b)) == 0
}

type Headers = Vec<(String, String)>;
fn read_message(stream: &mut impl Read) -> Result<(String, Headers, Vec<u8>), String> {
    let (first, headers, size) = read_head(stream)?;
    Ok((first, headers, read_body(stream, size)?))
}

fn read_head(stream: &mut impl Read) -> Result<(String, Headers, usize), String> {
    let mut bytes = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        stream.read_exact(&mut byte).map_err(|_| "Incomplete project HTTP headers")?;
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break;
        }
        if bytes.len() >= HEADER_LIMIT {
            return Err("Project HTTP headers exceed limit".into());
        }
    }
    let head = String::from_utf8(bytes).map_err(|_| "Invalid project HTTP headers")?;
    let mut lines = head.split("\r\n");
    let first = lines.next().ok_or("Missing project HTTP start line")?.to_string();
    let mut headers = Vec::new();
    let mut length = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or("Invalid project HTTP header")?;
        let name = name.to_ascii_lowercase();
        let value = value.trim().to_string();
        if name == "transfer-encoding" {
            return Err("Chunked project messages are not supported".into());
        }
        if headers.iter().any(|(old, _)| old == &name) {
            return Err("Duplicate project HTTP header".into());
        }
        if name == "content-length" {
            let size: usize = value.parse().map_err(|_| "Invalid project content length")?;
            if size > BODY_LIMIT {
                return Err("Project message exceeds transfer limit".into());
            }
            length = Some(size);
        }
        headers.push((name, value));
    }
    let size = length.ok_or("Project message requires a content length")?;
    Ok((first, headers, size))
}

fn read_body(stream: &mut impl Read, size: usize) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    body.try_reserve_exact(size).map_err(|_| "Not enough memory for project transfer")?;
    body.resize(size, 0);
    stream.read_exact(&mut body).map_err(|_| "Incomplete project body; operation status must be checked")?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn ambiguous_and_oversized_requests_are_rejected_before_body_allocation() {
        for header in [
            "POST /v1/x HTTP/1.1\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n",
            "POST /v1/x HTTP/1.1\r\nContent-Length: 0\r\nTransfer-Encoding: chunked\r\n\r\n",
            "POST /v1/x HTTP/1.1\r\nContent-Length: 18446744073709551615\r\n\r\n",
        ] {
            assert!(read_message(&mut Cursor::new(header.as_bytes())).is_err());
        }
    }

    #[test]
    fn paired_secret_is_never_in_debug_output() {
        let ticket = HostTicket { address: "127.0.0.1:1".parse().unwrap(), certificate_der: vec![1], token: "a".repeat(64) };
        assert!(!format!("{ticket:?}").contains(&ticket.token));
        assert!(equal_token(&ticket.token, &"a".repeat(64)));
        assert!(!equal_token(&ticket.token, &"b".repeat(64)));
    }

    #[test]
    fn unpaired_client_is_rejected_without_reading_its_declared_large_body() {
        struct Duplex {
            input: Cursor<Vec<u8>>,
            output: Vec<u8>,
        }
        impl Read for Duplex {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                self.input.read(bytes)
            }
        }
        impl Write for Duplex {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.output.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut stream = Duplex { input: Cursor::new(b"POST /v1/source/save HTTP/1.1\r\nContent-Length: 400000000\r\n\r\n".to_vec()), output: vec![] };
        serve_one(&mut stream, &"a".repeat(64), |_, _| panic!("unpaired request reached project authority")).unwrap();
        assert!(String::from_utf8(stream.output).unwrap().starts_with("HTTP/1.1 401"));
    }
}
