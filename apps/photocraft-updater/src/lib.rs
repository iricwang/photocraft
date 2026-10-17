//! File-level incremental updates. The application bundle is an immutable, exact baseline.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const CONFIG: &str = "Contents/Resources/patch-config.json";
const MAX_JSON: u64 = 16 * 1024 * 1024;
const MAX_PACKAGE: u64 = 512 * 1024 * 1024;
const MAX_TREE: u64 = 2 * 1024 * 1024 * 1024;
const MAX_FILES: usize = 32768;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema: u32,
    pub channel: String,
    pub platform: String,
    pub revision: u64,
    pub version: String,
    pub feed: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub size: u64,
    pub sha256: String,
    pub mode: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub config: Config,
    pub files: BTreeMap<String, Entry>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub schema: u32,
    pub base: Snapshot,
    pub target: Snapshot,
    pub changed: Vec<String>,
    pub blocks: BTreeMap<String, Vec<Block>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    pub from: u64,
    pub to: u64,
    pub base_tree: String,
    pub target_tree: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
    pub notes: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feed {
    pub schema: u32,
    pub channel: String,
    pub platform: String,
    pub latest: u64,
    pub releases: Vec<Release>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ticket {
    pub app: PathBuf,
    pub workspace: PathBuf,
    pub header: Header,
}

pub fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}
pub fn config(app: &Path) -> Result<Config> {
    let value: Config = read_json(&app.join(CONFIG))?;
    if value.schema != 1 || value.channel.is_empty() || value.version.is_empty() {
        return Err("Invalid patch configuration".into());
    }
    allowed_url(&value.feed)?;
    Ok(value)
}
pub fn init(app: &Path, revision: u64, feed: &str, version: &str) -> Result<()> {
    allowed_url(feed)?;
    let config = Config { schema: 1, channel: "ai-p0".into(), platform: platform(), revision, version: version.into(), feed: feed.into() };
    fs::create_dir_all(app.join("Contents/Resources"))?;
    write_json(&app.join(CONFIG), &config)?;
    set_mode(&app.join(CONFIG), 0o644)
}
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = read_bounded(File::open(path)?, MAX_JSON)?;
    Ok(serde_json::from_slice(&bytes)?)
}
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().ok_or("No parent directory")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}
fn read_bounded(reader: impl Read, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err("Input exceeds size limit".into());
    }
    Ok(bytes)
}
pub fn sha256(path: &Path) -> Result<String> {
    let mut input = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        digest.update(buf.get(..n).ok_or("Invalid read length")?);
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn safe_path(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 1024
        || value.contains(['\\', ':', '\0'])
        || !value.is_ascii()
        || value.split('/').any(|s| s.is_empty() || s == "." || s == ".." || s.ends_with(['.', ' ']))
        || Path::new(value).components().any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(format!("Unsafe bundle path: {value}").into());
    }
    Ok(())
}
#[cfg(unix)]
fn mode(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}
#[cfg(not(unix))]
fn mode(_: &fs::Metadata) -> u32 {
    0o644
}
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}
#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> Result<()> {
    Ok(())
}

fn scan(root: &Path, dir: &Path, files: &mut BTreeMap<String, Entry>, bytes: &mut u64, depth: usize) -> Result<()> {
    if depth > 32 {
        return Err("Bundle is nested too deeply".into());
    }
    for child in fs::read_dir(dir)? {
        let child = child?;
        let path = child.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            scan(root, &path, files, bytes, depth + 1)?;
        } else if meta.is_file() {
            let name = path.strip_prefix(root)?.to_str().ok_or("Non UTF-8 bundle path")?.replace(std::path::MAIN_SEPARATOR, "/");
            safe_path(&name)?;
            *bytes = bytes.checked_add(meta.len()).ok_or("Bundle size overflow")?;
            if *bytes > MAX_TREE || files.len() >= MAX_FILES {
                return Err("Bundle exceeds size/file limit".into());
            }
            files.insert(name, Entry { size: meta.len(), sha256: sha256(&path)?, mode: mode(&meta) });
        } else {
            return Err(format!("Symlinks and special files are unsupported: {}", path.display()).into());
        }
    }
    Ok(())
}
pub fn snapshot(app: &Path) -> Result<Snapshot> {
    if !fs::symlink_metadata(app)?.is_dir() {
        return Err("Application must be a real directory".into());
    }
    let mut files = BTreeMap::new();
    scan(app, app, &mut files, &mut 0, 0)?;
    Ok(Snapshot { config: config(app)?, files })
}
pub fn tree(snapshot: &Snapshot) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(snapshot)?)))
}
fn validate_snapshot(s: &Snapshot) -> Result<()> {
    if s.config.schema != 1 || s.files.len() > MAX_FILES {
        return Err("Invalid snapshot".into());
    }
    let mut size = 0u64;
    let mut folded = std::collections::BTreeSet::new();
    for (name, entry) in &s.files {
        safe_path(name)?;
        if !folded.insert(name.to_ascii_lowercase()) {
            return Err("Case-colliding bundle paths".into());
        }
        if entry.sha256.len() != 64 || !entry.sha256.bytes().all(|b| b.is_ascii_hexdigit()) || entry.mode & !0o777 != 0 {
            return Err("Invalid file metadata".into());
        }
        size = size.checked_add(entry.size).ok_or("Bundle size overflow")?;
        if size > MAX_TREE {
            return Err("Expanded bundle exceeds limit".into());
        }
        let mut ancestor = Path::new(name).parent();
        while let Some(path) = ancestor {
            if s.files.contains_key(&path.to_string_lossy().to_string()) {
                return Err("File/directory collision".into());
            }
            ancestor = path.parent();
        }
    }
    Ok(())
}
const BLOCK_SIZE: u64 = 64 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Block {
    Copy { offset: u64, size: u64 },
    Data { size: u64 },
}
impl Block {
    fn size(&self) -> u64 {
        match self {
            Self::Copy { size, .. } | Self::Data { size } => *size,
        }
    }
}
fn file_blocks(base: Option<&Path>, target: &Path) -> Result<Vec<Block>> {
    let mut known = BTreeMap::<([u8; 32], u64), u64>::new();
    let mut buf = vec![0u8; BLOCK_SIZE as usize];
    if let Some(base) = base {
        let mut source = File::open(base)?;
        let mut offset = 0u64;
        let length = source.metadata()?.len();
        while offset < length {
            let size = BLOCK_SIZE.min(length - offset);
            let bytes = buf.get_mut(..usize::try_from(size)?).ok_or("Invalid chunk length")?;
            source.read_exact(bytes)?;
            known.entry((Sha256::digest(&*bytes).into(), size)).or_insert(offset);
            offset += size;
        }
    }
    let mut source = File::open(target)?;
    let length = source.metadata()?.len();
    let mut offset = 0u64;
    let mut blocks = Vec::new();
    while offset < length {
        let size = BLOCK_SIZE.min(length - offset);
        let bytes = buf.get_mut(..usize::try_from(size)?).ok_or("Invalid chunk length")?;
        source.read_exact(bytes)?;
        let key = (Sha256::digest(&*bytes).into(), size);
        blocks.push(match known.get(&key) {
            Some(offset) => Block::Copy { offset: *offset, size },
            None => Block::Data { size },
        });
        offset += size;
    }
    Ok(blocks)
}
fn make_header(base_root: &Path, target_root: &Path) -> Result<Header> {
    let base = snapshot(base_root)?;
    let target = snapshot(target_root)?;
    let changed: Vec<_> = target.files.iter().filter(|(name, entry)| base.files.get(*name) != Some(*entry)).map(|(name, _)| name.clone()).collect();
    let mut blocks = BTreeMap::new();
    for name in &changed {
        let old = base_root.join(name);
        blocks.insert(name.clone(), file_blocks(base.files.contains_key(name).then_some(old.as_path()), &target_root.join(name))?);
    }
    let header = Header { schema: 1, base, target, changed, blocks };
    validate_header(&header)?;
    Ok(header)
}
fn validate_blocks(header: &Header) -> Result<()> {
    if header.blocks.keys().cloned().collect::<Vec<_>>() != header.changed {
        return Err("Chunk plans differ from changed files".into());
    }
    let mut count = 0u64;
    for (name, blocks) in &header.blocks {
        let target = header.target.files.get(name).ok_or("Chunk target missing")?;
        let mut total = 0u64;
        for block in blocks {
            let size = block.size();
            count = count.checked_add(1).ok_or("Chunk count overflow")?;
            if size == 0 || size > BLOCK_SIZE || count > MAX_TREE / BLOCK_SIZE + MAX_FILES as u64 {
                return Err("Invalid chunk size/count".into());
            }
            total = total.checked_add(size).ok_or("Chunk output overflow")?;
            if total > target.size {
                return Err("Chunk plan exceeds target size".into());
            }
            if let Block::Copy { offset, .. } = block {
                let base = header.base.files.get(name).ok_or("Chunk baseline missing")?;
                if offset.checked_add(size).is_none_or(|end| end > base.size) {
                    return Err("Copy chunk exceeds baseline".into());
                }
            }
        }
        if total != target.size {
            return Err("Chunk plan is incomplete".into());
        }
    }
    Ok(())
}

fn validate_header(header: &Header) -> Result<()> {
    validate_snapshot(&header.base)?;
    validate_snapshot(&header.target)?;
    let a = &header.base.config;
    let b = &header.target.config;
    if header.schema != 1 || a.channel != b.channel || a.platform != b.platform || b.revision <= a.revision {
        return Err("Invalid patch transition".into());
    }
    let changed: Vec<_> =
        header.target.files.iter().filter(|(name, entry)| header.base.files.get(*name) != Some(*entry)).map(|(name, _)| name.clone()).collect();
    if changed != header.changed {
        return Err("Patch payload does not match target inventory".into());
    }
    validate_blocks(header)
}
pub fn package(base: &Path, target: &Path, output: &Path, url: &str, notes: &str) -> Result<Feed> {
    allowed_url(url)?;
    if output.exists() {
        return Err("Patch packages are immutable; choose a new output path".into());
    }
    let header = make_header(base, target)?;
    let json = serde_json::to_vec(&header)?;
    if json.len() as u64 > MAX_JSON {
        return Err("Patch inventory too large".into());
    }
    let mut out = tempfile::NamedTempFile::new_in(output.parent().ok_or("No output parent")?)?;
    {
        let mut gz = flate2::write::GzEncoder::new(&mut out, flate2::Compression::default());
        gz.write_all(b"PCPATCH1")?;
        gz.write_all(&(json.len() as u64).to_le_bytes())?;
        gz.write_all(&json)?;
        for name in &header.changed {
            let mut source = File::open(target.join(name))?;
            let mut offset = 0u64;
            for block in header.blocks.get(name).ok_or("Missing chunk plan")? {
                if let Block::Data { size } = block {
                    source.seek(SeekFrom::Start(offset))?;
                    if std::io::copy(&mut std::io::Read::by_ref(&mut source).take(*size), &mut gz)? != *size {
                        return Err("Target changed during packaging".into());
                    }
                }
                offset += block.size();
            }
        }
        gz.finish()?;
    }
    if snapshot(base)? != header.base || snapshot(target)? != header.target {
        return Err("Release bundles changed during packaging".into());
    }
    out.as_file().sync_all()?;
    out.persist_noclobber(output).map_err(|e| e.error)?;
    let size = fs::metadata(output)?.len();
    if size > MAX_PACKAGE {
        return Err("Compressed patch exceeds limit".into());
    }
    let release = Release {
        from: header.base.config.revision,
        to: header.target.config.revision,
        base_tree: tree(&header.base)?,
        target_tree: tree(&header.target)?,
        url: url.into(),
        size,
        sha256: sha256(output)?,
        notes: notes.into(),
    };
    Ok(Feed {
        schema: 1,
        channel: header.target.config.channel.clone(),
        platform: header.target.config.platform.clone(),
        latest: header.target.config.revision,
        releases: vec![release],
    })
}

/// HTTPS is required except for explicit loopback development feeds. Every redirect is checked.
pub fn allowed_url(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value)?;
    let loopback = url.host_str().is_some_and(|host| host == "localhost" || host == "[::1]" || host.parse::<std::net::IpAddr>().is_ok_and(|a| a.is_loopback()));
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() || !(url.scheme() == "https" || url.scheme() == "http" && loopback) {
        return Err("Patch URLs require HTTPS (HTTP is allowed only on loopback)".into());
    }
    Ok(url)
}
fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(180))
        .connect_timeout(Duration::from_secs(15))
        .user_agent("PhotoCraft-Updater/1")
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.error("Too many patch redirects");
            }
            let downgrade = attempt.previous().last().is_some_and(|url| url.scheme() == "https" && attempt.url().scheme() != "https");
            if downgrade || allowed_url(attempt.url().as_str()).is_err() { attempt.error("Unsafe patch redirect") } else { attempt.follow() }
        }))
        .build()?)
}
pub fn check(app: &Path, feed_url: &str) -> Result<Option<Release>> {
    let current = snapshot(app)?;
    let response = client()?.get(allowed_url(feed_url)?).send()?.error_for_status()?;
    let feed: Feed = serde_json::from_slice(&read_bounded(response, MAX_JSON)?)?;
    if feed.schema != 1 || feed.channel != current.config.channel || feed.platform != current.config.platform || feed.releases.len() > 256 {
        return Err("Patch feed channel/platform/schema mismatch".into());
    }
    if feed.latest <= current.config.revision {
        return Ok(None);
    }
    let hash = tree(&current)?;
    let release = feed
        .releases
        .into_iter()
        .filter(|r| r.from == current.config.revision && r.to > r.from && r.to <= feed.latest && r.base_tree == hash)
        .max_by_key(|r| r.to)
        .ok_or("No patch for this exact installed baseline; install the latest full application")?;
    allowed_url(&release.url)?;
    if release.size > MAX_PACKAGE {
        return Err("Patch exceeds download limit".into());
    }
    Ok(Some(release))
}
/// Download into a sibling workspace, construct and verify a complete replacement before exit.
pub fn prepare(app: &Path, release: &Release) -> Result<PathBuf> {
    let app = app.canonicalize()?;
    let parent = app.parent().ok_or("No application parent")?;
    let workspace = tempfile::Builder::new().prefix(".photocraft-patch-").tempdir_in(parent)?;
    let package = workspace.path().join("update.pcp");
    let mut response = client()?.get(allowed_url(&release.url)?).send()?.error_for_status()?.take(release.size.min(MAX_PACKAGE) + 1);
    let mut file = File::create(&package)?;
    let size = std::io::copy(&mut response, &mut file)?;
    file.sync_all()?;
    if size != release.size || sha256(&package)? != release.sha256 {
        return Err("Patch download size or SHA-256 mismatch".into());
    }
    let header = stage(&app, &package, &workspace.path().join("next.app"))?;
    if header.base.config.revision != release.from
        || header.target.config.revision != release.to
        || tree(&header.base)? != release.base_tree
        || tree(&header.target)? != release.target_tree
    {
        return Err("Patch does not match feed metadata".into());
    }
    let ticket = Ticket { app, workspace: workspace.path().to_path_buf(), header };
    write_json(&workspace.path().join("ticket.json"), &ticket)?;
    Ok(workspace.keep().join("ticket.json"))
}
pub fn stage(app: &Path, package: &Path, target: &Path) -> Result<Header> {
    let mut input = flate2::read::GzDecoder::new(File::open(package)?);
    let mut magic = [0u8; 8];
    input.read_exact(&mut magic)?;
    if &magic != b"PCPATCH1" {
        return Err("Not a PhotoCraft patch".into());
    }
    let mut len = [0u8; 8];
    input.read_exact(&mut len)?;
    let len = u64::from_le_bytes(len);
    if len > MAX_JSON {
        return Err("Patch header too large".into());
    }
    let mut bytes = vec![0; usize::try_from(len)?];
    input.read_exact(&mut bytes)?;
    let header: Header = serde_json::from_slice(&bytes)?;
    validate_header(&header)?;
    if snapshot(app)? != header.base {
        return Err("Installed application differs from the exact patch baseline".into());
    }
    fs::create_dir(target)?;
    for (name, entry) in &header.target.files {
        let path = target.join(name);
        fs::create_dir_all(path.parent().ok_or("No file parent")?)?;
        if let Some(blocks) = header.blocks.get(name) {
            let mut output = File::create(&path)?;
            let mut base = if header.base.files.contains_key(name) { Some(File::open(app.join(name))?) } else { None };
            for block in blocks {
                let copied = match block {
                    Block::Copy { offset, size } => {
                        let source = base.as_mut().ok_or("Copy source missing")?;
                        source.seek(SeekFrom::Start(*offset))?;
                        std::io::copy(&mut source.take(*size), &mut output)?
                    }
                    Block::Data { size } => std::io::copy(&mut std::io::Read::by_ref(&mut input).take(*size), &mut output)?,
                };
                if copied != block.size() {
                    return Err("Truncated patch payload or baseline chunk".into());
                }
            }
        } else {
            fs::copy(app.join(name), &path)?;
        }
        set_mode(&path, entry.mode)?;
        if fs::metadata(&path)?.len() != entry.size || sha256(&path)? != entry.sha256 {
            return Err(format!("File verification failed: {name}").into());
        }
        File::open(&path)?.sync_all()?;
    }
    if input.read(&mut [0u8; 1])? != 0 {
        return Err("Unexpected patch payload".into());
    }
    if snapshot(target)? != header.target {
        return Err("Replacement bundle verification failed".into());
    }
    verify_codesign(target)?;
    Ok(header)
}
fn verify_codesign(app: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    if !std::process::Command::new("/usr/bin/codesign").args(["--verify", "--strict", "--deep"]).arg(app).status()?.success() {
        return Err("Replacement application code signature is invalid".into());
    }
    let _ = app;
    Ok(())
}
pub fn bundle_path() -> Result<PathBuf> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let app = exe.parent().and_then(Path::parent).and_then(Path::parent).ok_or("Not running from a PhotoCraft.app bundle")?;
    if exe.file_name().and_then(|s| s.to_str()) != Some("PhotoCraft") || !app.join(CONFIG).is_file() {
        return Err("Patch updates require a packaged PhotoCraft.app".into());
    }
    Ok(app.to_path_buf())
}
fn open_lock(app: &Path) -> Result<File> {
    let name = app.file_name().ok_or("Invalid application name")?.to_string_lossy();
    let path = app.parent().ok_or("No application parent")?.join(format!(".{name}.patch-use.lock"));
    let file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
    Ok(file)
}
pub fn use_lock(app: &Path) -> Result<File> {
    let file = open_lock(app)?;
    file.lock_shared()?;
    Ok(file)
}
pub fn install(ticket_path: &Path, timeout: Duration) -> Result<()> {
    let ticket: Ticket = read_json(ticket_path)?;
    let app = &ticket.app;
    let next = ticket.workspace.join("next.app");
    validate_ticket_location(ticket_path, &ticket)?;
    validate_header(&ticket.header)?;
    let lock = open_lock(app)?;
    let start = std::time::Instant::now();
    while lock.try_lock().is_err() {
        if start.elapsed() > timeout {
            return Err("Other PhotoCraft instances are still running; installation cancelled".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if snapshot(app)? != ticket.header.base || snapshot(&next)? != ticket.header.target {
        return Err("Application changed since patch preparation".into());
    }
    verify_codesign(&next)?;
    let backup = backup_path(&ticket)?;
    if backup.exists() {
        return Err(format!("Backup already exists: {}", backup.display()).into());
    }
    write_json(&ticket.workspace.join("install-status.json"), &serde_json::json!({"phase":"prepared","app":app,"backup":backup}))?;
    fs::rename(app, &backup)?;
    if let Err(error) = fs::rename(&next, app) {
        fs::rename(&backup, app)?;
        return Err(error.into());
    }
    // Release before launch: every application instance takes a shared lock on this bundle.
    lock.unlock()?;
    let health = ticket.workspace.join("started");
    let token = ticket.workspace.file_name().ok_or("No workspace name")?.to_string_lossy().into_owned();
    let launch = || {
        std::process::Command::new(app.join("Contents/MacOS/PhotoCraft"))
            .env("PHOTOCRAFT_PATCH_HEALTH", &health)
            .env("PHOTOCRAFT_PATCH_TOKEN", &token)
            .env("PHOTOCRAFT_NEW_INSTANCE", "1")
            .spawn()
    };
    let mut child = launch().ok();
    let start = std::time::Instant::now();
    let healthy = loop {
        if fs::read_to_string(&health).is_ok_and(|text| text == token) {
            break true;
        }
        if child.as_mut().is_none_or(|c| c.try_wait().ok().flatten().is_some()) || start.elapsed() > timeout {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if !healthy {
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
        lock.try_lock().map_err(|_| "Another PhotoCraft instance started; close it and use recover on the retained ticket")?;
        let failed = ticket.workspace.join("failed.app");
        fs::rename(app, failed)?;
        fs::rename(&backup, app)?;
        lock.unlock()?;
        let _ = std::process::Command::new(app.join("Contents/MacOS/PhotoCraft")).env("PHOTOCRAFT_NEW_INSTANCE", "1").spawn();
        write_json(&ticket.workspace.join("install-status.json"), &serde_json::json!({"phase":"rolled_back","revision":ticket.header.base.config.revision}))?;
        return Err("New application did not signal successful startup; previous version restored".into());
    }
    write_json(
        &ticket.workspace.join("install-status.json"),
        &serde_json::json!({"phase":"complete","revision":ticket.header.target.config.revision,"backup":backup}),
    )?;
    Ok(())
}

/// Restore the retained exact baseline after an interrupted swap or an explicit rollback.
/// Refuses to replace a running or locally modified application.
pub fn recover(ticket_path: &Path) -> Result<()> {
    let ticket: Ticket = read_json(ticket_path)?;
    validate_header(&ticket.header)?;
    let app = &ticket.app;
    validate_ticket_location(ticket_path, &ticket)?;
    let lock = open_lock(app)?;
    lock.try_lock().map_err(|_| "Quit all PhotoCraft instances before recovering")?;
    let backup = backup_path(&ticket)?;
    if snapshot(&backup)? != ticket.header.base {
        return Err("Recovery backup differs from the exact baseline".into());
    }
    if app.exists() {
        if snapshot(app)? != ticket.header.target {
            return Err("Installed application has changed; recovery refused".into());
        }
        let retained = ticket.workspace.join("recovered-target.app");
        if retained.exists() {
            return Err("A recovered target already exists".into());
        }
        fs::rename(app, &retained)?;
        if let Err(error) = fs::rename(&backup, app) {
            fs::rename(retained, app)?;
            return Err(error.into());
        }
    } else {
        fs::rename(&backup, app)?;
    }
    write_json(&ticket.workspace.join("install-status.json"), &serde_json::json!({"phase":"recovered","revision":ticket.header.base.config.revision}))?;
    Ok(())
}
fn validate_ticket_location(path: &Path, ticket: &Ticket) -> Result<()> {
    if path.parent() != Some(ticket.workspace.as_path())
        || ticket.workspace.parent() != ticket.app.parent()
        || !ticket.workspace.file_name().is_some_and(|name| name.to_string_lossy().starts_with(".photocraft-patch-"))
    {
        return Err("Invalid installation ticket location".into());
    }
    Ok(())
}
fn backup_path(ticket: &Ticket) -> Result<PathBuf> {
    Ok(ticket.app.with_file_name(format!(
        ".{}.patch-backup-{}",
        ticket.app.file_name().ok_or("No application name")?.to_string_lossy(),
        ticket.header.base.config.revision
    )))
}

/// Called only after the desktop has created a working window and completed its first UI pass.
pub fn acknowledge_startup() -> Result<()> {
    if let (Some(path), Ok(token)) = (std::env::var_os("PHOTOCRAFT_PATCH_HEALTH"), std::env::var("PHOTOCRAFT_PATCH_TOKEN")) {
        fs::write(path, token)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    fn sign(app: &Path) {
        #[cfg(target_os = "macos")]
        assert!(
            std::process::Command::new("/usr/bin/codesign").args(["--force", "--sign", "-", "--timestamp=none"]).arg(app).output().unwrap().status.success()
        );
        let _ = app;
    }
    fn fixture(parent: &Path, name: &str, revision: u64) -> PathBuf {
        let app = parent.join(name);
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::write(app.join("Contents/Info.plist"), b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>PhotoCraft</string><key>CFBundleIdentifier</key><string>ai.storyteller.photocraft.updater-test</string><key>CFBundleVersion</key><string>1</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>").unwrap();
        #[cfg(target_os = "macos")]
        fs::copy("/usr/bin/true", app.join("Contents/MacOS/PhotoCraft")).unwrap();
        #[cfg(not(target_os = "macos"))]
        fs::write(app.join("Contents/MacOS/PhotoCraft"), b"test executable").unwrap();
        init(&app, revision, "http://127.0.0.1:12345/feed.json", "0.9.0").unwrap();
        fs::write(app.join("Contents/Resources/unchanged"), vec![42; 1024 * 1024]).unwrap();
        sign(&app);
        app
    }
    fn archive(path: &Path, header: &Header, payload: &[u8]) {
        let mut gz = flate2::write::GzEncoder::new(File::create(path).unwrap(), flate2::Compression::default());
        let json = serde_json::to_vec(header).unwrap();
        gz.write_all(b"PCPATCH1").unwrap();
        gz.write_all(&(json.len() as u64).to_le_bytes()).unwrap();
        gz.write_all(&json).unwrap();
        gz.write_all(payload).unwrap();
        gz.finish().unwrap();
    }
    #[test]
    fn changed_files_only_round_trip_preserves_deletions_modes_and_signatures() {
        let temp = tempfile::tempdir().unwrap();
        let base = fixture(temp.path(), "base.app", 1);
        let target = fixture(temp.path(), "target.app", 2);
        fs::write(base.join("Contents/Resources/deleted"), b"old").unwrap();
        sign(&base);
        #[cfg(target_os = "macos")]
        fs::copy("/usr/bin/false", target.join("Contents/MacOS/PhotoCraft")).unwrap();
        fs::write(target.join("Contents/Resources/new"), b"new asset").unwrap();
        set_mode(&target.join("Contents/Resources/new"), 0o755).unwrap();
        sign(&target);
        let pkg = temp.path().join("update.pcp");
        package(&base, &target, &pkg, "https://example.com/update.pcp", "test").unwrap();
        let staged = temp.path().join("stage.app");
        let header = stage(&base, &pkg, &staged).unwrap();
        assert_eq!(snapshot(&target).unwrap(), snapshot(&staged).unwrap());
        assert!(!header.changed.iter().any(|name| name.ends_with("unchanged")));
        assert!(!staged.join("Contents/Resources/deleted").exists());
        assert!(fs::metadata(&pkg).unwrap().len() < 100_000);
    }
    #[test]
    fn modified_or_extra_baseline_files_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let base = fixture(temp.path(), "base.app", 1);
        let target = fixture(temp.path(), "target.app", 2);
        let pkg = temp.path().join("update.pcp");
        package(&base, &target, &pkg, "https://example.com/update.pcp", "").unwrap();
        fs::write(base.join("unexpected"), b"local modification").unwrap();
        let next = temp.path().join("stage.app");
        assert!(stage(&base, &pkg, &next).unwrap_err().to_string().contains("baseline"));
        assert!(!next.exists());
    }
    #[test]
    fn unsafe_paths_and_case_collisions_are_rejected() {
        for name in ["../outside", "/tmp/outside", "a/../../b", "a\\b", "C:/a", "a//b", "a/./b", "a/", "file.", "file "] {
            assert!(safe_path(name).is_err(), "{name}");
        }
        let temp = tempfile::tempdir().unwrap();
        let base = fixture(temp.path(), "base.app", 1);
        let mut s = snapshot(&base).unwrap();
        let entry = s.files.values().next().unwrap().clone();
        s.files.insert("Case".into(), entry.clone());
        s.files.insert("case".into(), entry);
        assert!(validate_snapshot(&s).unwrap_err().to_string().contains("Case"));
    }
    #[test]
    fn symlinks_and_file_directory_collisions_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let base = fixture(temp.path(), "base.app", 1);
        let mut s = snapshot(&base).unwrap();
        let entry = s.files.values().next().unwrap().clone();
        s.files.insert("Contents".into(), entry);
        assert!(validate_snapshot(&s).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/tmp", base.join("escape")).unwrap();
            assert!(snapshot(&base).is_err());
        }
    }
    #[test]
    fn downgrade_platform_and_payload_inventory_mismatch_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let base = fixture(temp.path(), "base.app", 1);
        let target = fixture(temp.path(), "target.app", 2);
        let mut h = make_header(&base, &target).unwrap();
        assert!(validate_header(&h).is_ok());
        h.target.config.revision = 1;
        assert!(validate_header(&h).is_err());
        h.target.config.revision = 2;
        h.target.config.platform = "other".into();
        assert!(validate_header(&h).is_err());
        h.target.config.platform = platform();
        h.changed.clear();
        assert!(validate_header(&h).is_err());
    }
    #[test]
    fn truncated_or_corrupt_payload_is_rejected_without_touching_baseline() {
        let temp = tempfile::tempdir().unwrap();
        let base = fixture(temp.path(), "base.app", 1);
        let target = fixture(temp.path(), "target.app", 2);
        let pkg = temp.path().join("update.pcp");
        package(&base, &target, &pkg, "https://example.com/update.pcp", "").unwrap();
        let original = snapshot(&base).unwrap();
        let bytes = fs::read(&pkg).unwrap();
        fs::write(&pkg, &bytes[..bytes.len() / 2]).unwrap();
        assert!(stage(&base, &pkg, &temp.path().join("stage.app")).is_err());
        assert_eq!(snapshot(&base).unwrap(), original);
        let header = make_header(&base, &target).unwrap();
        archive(&pkg, &header, &vec![0; 1024 * 1024]);
        assert!(stage(&base, &pkg, &temp.path().join("stage2.app")).is_err());
    }
    #[test]
    fn oversized_header_and_inventories_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let pkg = temp.path().join("bad.pcp");
        let mut gz = flate2::write::GzEncoder::new(File::create(&pkg).unwrap(), flate2::Compression::default());
        gz.write_all(b"PCPATCH1").unwrap();
        gz.write_all(&(MAX_JSON + 1).to_le_bytes()).unwrap();
        gz.finish().unwrap();
        assert!(stage(temp.path(), &pkg, &temp.path().join("stage")).unwrap_err().to_string().contains("header too large"));
    }
    #[test]
    fn transport_restricts_urls_credentials_and_redirects() {
        for url in
            ["file:///etc/passwd", "http://example.com/patch", "https://user:pass@example.com/patch", "ftp://localhost/a", "https://example.com/a#fragment"]
        {
            assert!(allowed_url(url).is_err(), "{url}");
        }
        for url in ["https://example.com/a", "http://127.0.0.1:123/a", "http://[::1]/a"] {
            assert!(allowed_url(url).is_ok(), "{url}");
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0; 4096];
            let _ = stream.read(&mut buffer);
            stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://example.com/unsafe\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        assert!(client().unwrap().get(format!("http://{address}/a")).send().is_err());
        thread.join().unwrap();
    }
    #[test]
    fn live_loopback_feed_download_and_exact_staging() {
        let temp = tempfile::tempdir().unwrap();
        let base = fixture(temp.path(), "base.app", 1);
        let target = fixture(temp.path(), "target.app", 2);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let pkg = temp.path().join("update.pcp");
        let feed = package(&base, &target, &pkg, &format!("http://{address}/update.pcp"), "live update").unwrap();
        let mut bad_feed = feed.clone();
        bad_feed.releases[0].sha256 = "0".repeat(64);
        let responses = [serde_json::to_vec(&feed).unwrap(), fs::read(&pkg).unwrap(), serde_json::to_vec(&bad_feed).unwrap(), fs::read(&pkg).unwrap()];
        let thread = std::thread::spawn(move || {
            for body in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = [0; 4096];
                let _ = stream.read(&mut buffer);
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        let url = format!("http://{address}/feed.json");
        let release = check(&base, &url).unwrap().unwrap();
        let ticket_path = prepare(&base, &release).unwrap();
        let ticket: Ticket = read_json(&ticket_path).unwrap();
        assert_eq!(snapshot(&ticket.workspace.join("next.app")).unwrap(), snapshot(&target).unwrap());
        let bad = check(&base, &url).unwrap().unwrap();
        assert!(prepare(&base, &bad).unwrap_err().to_string().contains("SHA-256"));
        fs::remove_dir_all(ticket.workspace).unwrap();
        thread.join().unwrap();
    }
    #[test]
    fn recover_interrupted_swap_and_refuse_modified_or_running_app() {
        let temp = tempfile::tempdir().unwrap();
        let base = fixture(temp.path(), "PhotoCraft.app", 1);
        let target = fixture(temp.path(), "target.app", 2);
        let workspace = tempfile::Builder::new().prefix(".photocraft-patch-").tempdir_in(temp.path()).unwrap();
        let a = snapshot(&base).unwrap();
        let ticket = Ticket { app: base.clone(), workspace: workspace.path().to_path_buf(), header: make_header(&base, &target).unwrap() };
        let path = workspace.path().join("ticket.json");
        write_json(&path, &ticket).unwrap();
        let backup = backup_path(&ticket).unwrap();
        fs::rename(&base, &backup).unwrap();
        recover(&path).unwrap();
        assert_eq!(snapshot(&base).unwrap(), a);
        fs::rename(&base, &backup).unwrap();
        fs::rename(&target, &base).unwrap();
        let lock = use_lock(&base).unwrap();
        assert!(recover(&path).is_err());
        drop(lock);
        fs::write(base.join("unexpected"), b"modification").unwrap();
        assert!(recover(&path).is_err());
        fs::remove_file(base.join("unexpected")).unwrap();
        recover(&path).unwrap();
        assert_eq!(snapshot(&base).unwrap(), a);
    }
    #[test]
    fn large_changed_file_reuses_verified_baseline_chunks_and_rejects_out_of_bounds_copy() {
        let temp = tempfile::tempdir().unwrap();
        let base = fixture(temp.path(), "base.app", 1);
        let target = fixture(temp.path(), "target.app", 2);
        let mut state = 42u32;
        let mut data: Vec<u8> = (0..2 * 1024 * 1024)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                (state >> 24) as u8
            })
            .collect();
        fs::write(base.join("Contents/Resources/large"), &data).unwrap();
        sign(&base);
        data[16 * BLOCK_SIZE as usize + 7] ^= 1;
        fs::write(target.join("Contents/Resources/large"), &data).unwrap();
        sign(&target);
        let pkg = temp.path().join("update.pcp");
        package(&base, &target, &pkg, "https://example.com/update.pcp", "chunk test").unwrap();
        let mut header = stage(&base, &pkg, &temp.path().join("staged.app")).unwrap();
        let blocks = header.blocks.get_mut("Contents/Resources/large").unwrap();
        assert_eq!(blocks.iter().filter(|block| matches!(block, Block::Data { .. })).count(), 1);
        assert_eq!(blocks.iter().filter(|block| matches!(block, Block::Copy { .. })).count(), 31);
        assert!(fs::metadata(&pkg).unwrap().len() < 160_000);
        blocks[0] = Block::Copy { offset: MAX_TREE, size: BLOCK_SIZE };
        assert!(validate_header(&header).unwrap_err().to_string().contains("exceeds baseline"));
    }

    #[test]
    fn shared_app_lock_excludes_installer_until_all_instances_close() {
        let temp = tempfile::tempdir().unwrap();
        let app = fixture(temp.path(), "base.app", 1);
        let a = use_lock(&app).unwrap();
        let b = use_lock(&app).unwrap();
        let other = open_lock(&app).unwrap();
        assert!(other.try_lock().is_err());
        drop(a);
        assert!(other.try_lock().is_err());
        drop(b);
        assert!(other.try_lock().is_ok());
    }
}
