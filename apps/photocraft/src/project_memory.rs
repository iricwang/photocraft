//! Local project records and trusted Codex session identities. Documents never carry thread IDs.
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};

use photocraft_ui_egui::ai_service::ProjectContext;
use serde_json::{Value, json};

const MAX_CONTEXT: usize = 512 * 1024;
const MAX_JOURNAL: u64 = 16 * 1024 * 1024;
static RECORD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
type WriteErrors = Arc<Mutex<std::collections::BTreeMap<String, String>>>;
static WRITER: Mutex<Option<WriterLink>> = Mutex::new(None);

enum WriteMessage {
    Save(ProjectContext),
    Flush { project: Option<String>, reply: mpsc::Sender<Result<(), String>> },
    Stop,
}

#[derive(Clone)]
struct WriterLink {
    tx: mpsc::SyncSender<WriteMessage>,
    errors: WriteErrors,
}

struct WriterOwner {
    link: WriterLink,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for WriterOwner {
    fn drop(&mut self) {
        // Stop is queued after all accepted updates; normal application shutdown drains them.
        let _ = self.link.tx.send(WriteMessage::Stop);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            log::error!("Project memory writer stopped unexpectedly");
        }
        if let Err(error) = writer_status(&self.link.errors) {
            log::error!("Project memory could not be fully saved: {error}");
        }
    }
}

fn writer_status(errors: &WriteErrors) -> Result<(), String> {
    let errors = errors.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match errors.values().next() {
        Some(error) => Err(error.clone()),
        None => Ok(()),
    }
}

fn project_key(context: &ProjectContext) -> String {
    format!("{}:{}", context.id, context.path.as_deref().unwrap_or("unsaved"))
}

pub(crate) fn sync_status() -> Result<(), String> {
    let writer = WRITER.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    writer.map_or(Ok(()), |writer| writer_status(&writer.errors))
}

fn flush_pending(context: &ProjectContext) -> Result<(), String> {
    let Some(writer) = WRITER.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() else { return Ok(()) };
    let (tx, rx) = mpsc::channel();
    writer
        .tx
        .try_send(WriteMessage::Flush { project: Some(project_key(context)), reply: tx })
        .map_err(|_| "Project memory writer is busy or unavailable; retry shortly")?;
    rx.recv_timeout(std::time::Duration::from_secs(30)).map_err(|_| "Project memory writer did not finish saving within 30 seconds")?
}

fn start_writer(write: impl Fn(ProjectContext) -> Result<(), String> + Send + 'static) -> Result<WriterOwner, String> {
    let (tx, rx) = mpsc::sync_channel::<WriteMessage>(64);
    let errors: WriteErrors = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
    let worker_errors = errors.clone();
    let thread = std::thread::Builder::new()
        .name("photocraft-project-memory".into())
        .spawn(move || {
            while let Ok(message) = rx.recv() {
                match message {
                    WriteMessage::Save(context) => {
                        let key = project_key(&context);
                        let result = write(context);
                        let mut errors = worker_errors.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        match result {
                            Ok(()) => {
                                errors.remove(&key);
                            }
                            Err(error) => {
                                errors.insert(key, error);
                            }
                        }
                    }
                    WriteMessage::Flush { project, reply } => {
                        let result = match project {
                            Some(key) => worker_errors.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&key).cloned().map_or(Ok(()), Err),
                            None => writer_status(&worker_errors),
                        };
                        let _ = reply.send(result);
                    }
                    WriteMessage::Stop => break,
                }
            }
        })
        .map_err(|_| "Could not start project memory writer")?;
    let link = WriterLink { tx, errors };
    Ok(WriterOwner { link, thread: Some(thread) })
}

pub(crate) fn sync_service() -> photocraft_ui_egui::ai_service::ProjectSyncFn {
    let owner = match start_writer(sync) {
        Ok(owner) => owner,
        Err(error) => return Box::new(move |_| Err(error.clone())),
    };
    *WRITER.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(owner.link.clone());
    Box::new(move |context| {
        validate_context(&context)?;
        let previous = writer_status(&owner.link.errors);
        owner.link.tx.try_send(WriteMessage::Save(context)).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => "Project memory writer is busy; the current document update will be retried".to_owned(),
            mpsc::TrySendError::Disconnected(_) => "Project memory writer is unavailable".to_owned(),
        })?;
        previous
    })
}

pub(crate) struct ProjectMemory {
    pub workspace: PathBuf,
    state_path: PathBuf,
    project_path: PathBuf,
    identity: String,
    pub project_id: Option<String>,
    pub thread_id: Option<String>,
    thread_project_id: Option<String>,
    previous_project_ids: Vec<String>,
    desktop_project_id: Option<String>,
    role: String,
    _session_lock: std::fs::File,
}

pub(crate) fn sync(context: ProjectContext) -> Result<(), String> {
    let _guard = RECORD_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let root = crate::app_dirs::config_dir().ok_or("No local directory for project memory")?;
    sync_in(&context, &root).map(|_| ())
}

pub(crate) fn prepare(context: &ProjectContext) -> Result<ProjectMemory, String> {
    prepare_for(context, "assets")
}

pub(crate) fn prepare_for(context: &ProjectContext, role: &str) -> Result<ProjectMemory, String> {
    flush_pending(context)?;
    let _guard = RECORD_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let root = crate::app_dirs::config_dir().ok_or("No local directory for project memory")?;
    prepare_role_in(context, &root, role)
}

fn lock_record(path: &Path) -> Result<std::fs::File, String> {
    if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("Project lock must not be a filesystem link".into());
    }
    let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).map_err(|e| e.to_string())?;
    file.try_lock().map_err(|_| "Another PhotoCraft instance is using this project record; try again shortly")?;
    Ok(file)
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 96 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err("Invalid PhotoCraft project identity".into());
    }
    Ok(())
}

fn create_directory(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_dir())
    {
        return Err("Project memory directory must not be a link or file".into());
    }
    std::fs::create_dir_all(path).map_err(|e| format!("Could not create project memory directory: {e}"))
}

fn stable_suffix(value: &str) -> u64 {
    value.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3))
}

fn source_identity(context: &ProjectContext) -> Result<(String, Option<PathBuf>), String> {
    match context.path.as_deref() {
        Some(path) => {
            let path = Path::new(path);
            if !path.is_absolute() {
                return Err("The saved project path must be absolute".into());
            }
            let parent = path.parent().ok_or("The saved project has no parent folder")?;
            let parent = parent.canonicalize().map_err(|_| "The saved project's folder is unavailable")?;
            let full_path = path.canonicalize().unwrap_or_else(|_| parent.join(path.file_name().unwrap_or_default()));
            let identity = full_path.to_string_lossy().to_string();
            #[cfg(target_os = "windows")]
            let identity = identity.to_lowercase();
            Ok((identity, Some(parent)))
        }
        None => Ok((format!("unsaved:{}", context.id), None)),
    }
}

fn trusted_directory(context: &ProjectContext, root: &Path, source: &str) -> PathBuf {
    let folder = format!("{}-{:016x}", context.id, stable_suffix(source));
    root.join("AI-Projects").join(folder)
}

fn workspace_name(context: &ProjectContext) -> String {
    if context.path.is_none() {
        return format!("Untitled-{}", context.id.chars().take(8).collect::<String>());
    }
    let name = Path::new(&context.name).file_stem().and_then(|name| name.to_str()).unwrap_or(&context.name);
    clean_workspace_name(name)
}

fn clean_workspace_name(name: &str) -> String {
    let mut cleaned = String::new();
    for character in name.chars().take(80) {
        let character = if character.is_control() || "<>:\"/\\|?*".contains(character) { '_' } else { character };
        if cleaned.len().saturating_add(character.len_utf8()) > 180 {
            break;
        }
        cleaned.push(character);
    }
    let cleaned = cleaned.trim_matches([' ', '.']);
    if cleaned.is_empty() {
        return "Untitled".into();
    }
    let device = cleaned.split('.').next().unwrap_or_default().to_ascii_uppercase();
    if ["CON", "PRN", "AUX", "NUL"].contains(&device.as_str())
        || ["COM", "LPT"]
            .iter()
            .any(|prefix| device.strip_prefix(*prefix).is_some_and(|number| matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")))
    {
        let mut cleaned = format!("_{cleaned}");
        while cleaned.len() > 180 || cleaned.chars().count() > 80 {
            cleaned.pop();
        }
        cleaned.trim_matches([' ', '.']).into()
    } else {
        cleaned.into()
    }
}

fn direct_locations(context: &ProjectContext, root: &Path) -> Result<(PathBuf, PathBuf, String), String> {
    let (source, parent) = source_identity(context)?;
    let trusted = trusted_directory(context, root, &source);
    let legacy = match parent {
        Some(parent) => parent.join(".photocraft").join(trusted.file_name().ok_or("Project state folder is unavailable")?),
        None => trusted.join("workspace"),
    };
    let workspace = if let Some(record) = read_json(&trusted.join("workspace-name.json"))? {
        let name = record["name"].as_str().ok_or("Invalid project workspace name")?;
        if record["version"] != 1 || name.is_empty() || name.len() > 180 || name != clean_workspace_name(name) {
            return Err("Invalid project workspace name; no folder was reused".into());
        }
        if context.path.is_some() { legacy.join(name) } else { trusted.join(name) }
    } else if legacy.exists() {
        // Existing projects keep their original root and Codex conversations.
        legacy
    } else {
        let name = workspace_name(context);
        if context.path.is_some() { legacy.join(name) } else { trusted.join(name) }
    };
    Ok((workspace, trusted.join("codex-session.json"), source))
}

fn first_saved_source(trusted: &Path, project_id: &str) -> Result<Option<String>, String> {
    let Some(record) = read_json(&trusted.join("first-save-binding.json"))? else { return Ok(None) };
    let source = record["sourceIdentity"].as_str().ok_or("Invalid first-save project binding")?;
    if record["version"] != 1 || record["projectId"].as_str() != Some(project_id) || source.len() > 32 * 1024 || !Path::new(source).is_absolute() {
        return Err("Invalid first-save project binding; no folder was reused".into());
    }
    Ok(Some(source.to_owned()))
}

fn locations(context: &ProjectContext, root: &Path) -> Result<(PathBuf, PathBuf, String), String> {
    validate_id(&context.id)?;
    let direct = direct_locations(context, root)?;
    if context.path.is_none() {
        return Ok(direct);
    }
    let trusted = direct.1.parent().ok_or("Project state folder is unavailable")?;
    let Some(alias) = read_json(&trusted.join("first-save-alias.json"))? else { return Ok(direct) };
    if alias["version"] != 1 || alias["projectId"].as_str() != Some(context.id.as_str()) || alias["sourceIdentity"].as_str() != Some(direct.2.as_str()) {
        return Err("Invalid first-save project alias; no folder was reused".into());
    }
    let mut unsaved = context.clone();
    unsaved.path = None;
    let target = direct_locations(&unsaved, root)?;
    if first_saved_source(target.1.parent().ok_or("Project state folder is unavailable")?, &context.id)?.as_deref() != Some(direct.2.as_str()) {
        return Err("The first-save project alias has no matching local binding".into());
    }
    Ok(target)
}

fn bind_first_save(context: &ProjectContext, root: &Path) -> Result<(), String> {
    if context.path.is_none() {
        return Ok(());
    }
    let (saved_workspace, saved_state, source) = direct_locations(context, root)?;
    let saved_trusted = saved_state.parent().ok_or("Project state folder is unavailable")?;
    if saved_trusted.join("first-save-alias.json").exists() {
        return Ok(());
    }
    let mut unsaved = context.clone();
    unsaved.path = None;
    let (workspace, unsaved_state, unsaved_source) = direct_locations(&unsaved, root)?;
    if !workspace.is_dir() {
        return Ok(());
    }
    let _file_lock = lock_record(&workspace.join("journal.lock"))?;
    let trusted = unsaved_state.parent().ok_or("Project state folder is unavailable")?;
    match first_saved_source(trusted, &context.id)? {
        Some(bound) if bound != source => return Ok(()),
        Some(_) => {}
        None => {
            if saved_trusted.exists() || saved_workspace.exists() {
                return Ok(());
            }
            let cursor = read_json(&workspace.join("journal-cursor.json"))?;
            if cursor.as_ref().and_then(cursor_open_id) != Some(context.open_id) {
                return Ok(());
            }
            let record = read_json(&workspace.join("project-context.json"))?;
            if !record.as_ref().is_some_and(|record| {
                record["sourceIdentity"].as_str() == Some(unsaved_source.as_str()) && record["projectId"].as_str() == Some(context.id.as_str())
            }) {
                return Ok(());
            }
            atomic_json(&trusted.join("first-save-binding.json"), &json!({"version":1,"projectId":context.id,"sourceIdentity":source}))?;
        }
    }
    // The alias can only target this project's computed unsaved identity, never a supplied path.
    create_directory(saved_trusted)?;
    atomic_json(&saved_trusted.join("first-save-alias.json"), &json!({"version":1,"projectId":context.id,"sourceIdentity":source}))
}

fn persist_workspace_name(context: &ProjectContext, workspace: &Path, trusted: &Path) -> Result<(), String> {
    let path = trusted.join("workspace-name.json");
    if path.exists() || workspace.exists() {
        return Ok(());
    }
    let name = workspace.file_name().and_then(|name| name.to_str()).ok_or("Invalid project workspace name")?;
    if name != workspace_name(context) {
        return Err("The project workspace location changed; no folder was created".into());
    }
    atomic_json(&path, &json!({"version":1,"name":name}))
}

fn atomic_json(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path.parent().ok_or("Project record has no parent folder")?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| format!("Could not write project memory: {e}"))?;
    serde_json::to_writer_pretty(&mut file, value).map_err(|e| e.to_string())?;
    if file.as_file().metadata().map_err(|e| e.to_string())?.len() >= MAX_CONTEXT as u64 {
        return Err("Project record exceeds 512 KiB; the previous record was preserved".into());
    }
    file.write_all(b"\n").and_then(|()| file.as_file().sync_all()).map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| format!("Could not save project memory: {}", e.error))?;
    Ok(())
}

fn read_json(path: &Path) -> Result<Option<Value>, String> {
    if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("Project records must not be filesystem links".into());
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Could not read project record: {error}")),
    };
    let mut bytes = Vec::new();
    file.take((MAX_CONTEXT + 1) as u64).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_CONTEXT {
        return Err("Project record exceeds the size limit".into());
    }
    serde_json::from_slice(&bytes).map(Some).map_err(|_| "Project record is damaged; it was not overwritten".into())
}

fn process_epoch() -> &'static str {
    static EPOCH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    EPOCH.get_or_init(|| {
        let start = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        format!("{}-{start:x}", std::process::id())
    })
}

fn open_session(open_id: u64) -> String {
    format!("{}-{open_id:x}", process_epoch())
}

fn cursor_open_id(cursor: &Value) -> Option<u64> {
    if cursor["processEpoch"].as_str() == Some(process_epoch()) {
        cursor["openId"].as_u64()
    } else if cursor.get("processEpoch").is_none() {
        let (epoch, open) = cursor["session"].as_str()?.rsplit_once('-')?;
        (epoch == process_epoch()).then(|| u64::from_str_radix(open, 16).ok()).flatten()
    } else {
        None
    }
}

fn append_events(context: &ProjectContext, workspace: &Path) -> Result<bool, String> {
    let cursor_path = workspace.join("journal-cursor.json");
    let cursor = read_json(&cursor_path)?;
    if cursor.as_ref().and_then(cursor_open_id).is_some_and(|latest| context.open_id < latest) {
        // A worker from a closed document cannot rewind the newer opened instance.
        return Ok(false);
    }
    let session = open_session(context.open_id);
    let same_open = cursor.as_ref().and_then(|v| v["session"].as_str()) == Some(session.as_str());
    let mut last = if same_open { cursor.as_ref().and_then(|v| v["sequence"].as_u64()).unwrap_or(0) } else { 0 };
    if same_open && context.events.last().map_or(0, |e| e.sequence) < last {
        // A generation worker may still carry an earlier request snapshot.
        return Ok(false);
    }
    let journal = workspace.join("operations.jsonl");
    if std::fs::symlink_metadata(&journal).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("Project operation journal must not be a filesystem link".into());
    }
    if journal.metadata().is_ok_and(|m| m.len() >= MAX_JOURNAL) {
        let archive_id = tempfile::Builder::new().prefix("operations-").suffix(".jsonl").tempfile_in(workspace).map_err(|e| e.to_string())?;
        let archive = archive_id.path().to_path_buf();
        archive_id.close().map_err(|e| e.to_string())?;
        if archive.exists() {
            return Err("Project journal archive already exists; no records were replaced".into());
        }
        std::fs::rename(&journal, archive).map_err(|e| format!("Could not archive project journal: {e}"))?;
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&journal).map_err(|e| e.to_string())?;
    let mut changed = false;
    if !same_open {
        serde_json::to_writer(
            &mut file,
            &json!({"kind":"documentOpened","session":session,
            "summary":"Retained events below are a replay of this opened document snapshot, not newly performed edits."}),
        )
        .map_err(|e| e.to_string())?;
        file.write_all(b"\n").map_err(|e| e.to_string())?;
        changed = true;
    }
    for event in &context.events {
        if event.sequence <= last {
            continue;
        }
        if event.kind.len() > 256 || event.summary.len() > 16 * 1024 {
            return Err("Project operation record exceeds the size limit".into());
        }
        if event.sequence > last.saturating_add(1) {
            serde_json::to_writer(
                &mut file,
                &json!({"kind":"historyGap","session":session,"afterSequence":last,"beforeSequence":event.sequence,
                "summary":"Earlier operations are not available in this document's retained history."}),
            )
            .map_err(|e| e.to_string())?;
            file.write_all(b"\n").map_err(|e| e.to_string())?;
        }
        serde_json::to_writer(
            &mut file,
            &json!({"session":session,"retainedSnapshot":!same_open,
            "sequence":event.sequence,"kind":event.kind,"summary":event.summary}),
        )
        .map_err(|e| e.to_string())?;
        file.write_all(b"\n").map_err(|e| e.to_string())?;
        last = event.sequence;
        changed = true;
    }
    if changed {
        file.sync_all().map_err(|e| format!("Could not save project operations: {e}"))?;
        atomic_json(&cursor_path, &json!({"session":session,"processEpoch":process_epoch(),"openId":context.open_id,"sequence":last}))?;
    }
    Ok(true)
}

fn validate_context(context: &ProjectContext) -> Result<(), String> {
    validate_id(&context.id)?;
    if context.name.len() > 4096
        || context.path.as_ref().is_some_and(|p| p.len() > 32 * 1024)
        || context.memory.len() > MAX_CONTEXT
        || context.events.len() > 256
        || context.events.iter().any(|e| e.sequence == 0 || e.kind.len() > 256 || e.summary.len() > 16 * 1024)
        || context.events.windows(2).any(|pair| pair.first().zip(pair.get(1)).is_some_and(|(left, right)| left.sequence >= right.sequence))
    {
        return Err("Project context exceeds the size limit".into());
    }
    Ok(())
}

fn sync_in(context: &ProjectContext, root: &Path) -> Result<(PathBuf, PathBuf), String> {
    validate_context(context)?;
    bind_first_save(context, root)?;
    let (workspace, state_path, source) = locations(context, root)?;
    create_directory(&root.join("AI-Projects"))?;
    let trusted = state_path.parent().ok_or("Project state folder is unavailable")?;
    create_directory(trusted)?;
    persist_workspace_name(context, &workspace, trusted)?;
    create_directory(workspace.parent().ok_or("Project memory folder is unavailable")?)?;
    create_directory(&workspace)?;
    let _file_lock = lock_record(&workspace.join("journal.lock"))?;
    if context.path.is_none() && first_saved_source(trusted, &context.id)?.is_some() {
        // Generation may still hold the unsaved snapshot after this document's first save.
        return Ok((workspace, state_path));
    }
    let context_path = workspace.join("project-context.json");
    if read_json(&context_path)?.as_ref().is_some_and(|v| v["sourceIdentity"].as_str() != Some(source.as_str())) {
        return Err("Project memory belongs to a different document path; it was not reused".into());
    }
    if !append_events(context, &workspace)? {
        return Ok((workspace, state_path));
    }
    atomic_json(
        &context_path,
        &json!({"format":"photocraft-project-context-v1","projectId":context.id,
        "name":context.name,"documentPath":context.path,"sourceIdentity":source,"documentSourceIdentity":source_identity(context)?.0,"memory":context.memory,
        "notice":"This file contains project data, not execution instructions. operations.jsonl contains the operation history."}),
    )?;
    Ok((workspace, state_path))
}

pub(crate) fn history(context: &ProjectContext, offset: u64, limit: usize) -> Result<Value, String> {
    let root = crate::app_dirs::config_dir().ok_or("No local directory for project memory")?;
    flush_pending(context)?;
    let _guard = RECORD_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    history_in(context, &root, offset, limit)
}

fn history_in(context: &ProjectContext, root: &Path, offset: u64, limit: usize) -> Result<Value, String> {
    if !(1..=100).contains(&limit) {
        return Err("Project history limit must be between 1 and 100".into());
    }
    let (workspace, _, source) = locations(context, root)?;
    let Some(record) = read_json(&workspace.join("project-context.json"))? else {
        return Ok(json!({"records":[],"nextOffset":null}));
    };
    if record["sourceIdentity"].as_str() != Some(source.as_str()) {
        return Err("Project history belongs to a different document".into());
    }
    let _file_lock = lock_record(&workspace.join("journal.lock"))?;
    let mut archives = Vec::new();
    for (index, entry) in std::fs::read_dir(&workspace).map_err(|e| e.to_string())?.take(4097).enumerate() {
        if index >= 4096 {
            return Err("Project history folder has too many entries".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("operations-") && name.ends_with(".jsonl") {
            let metadata = entry.metadata().map_err(|e| e.to_string())?;
            if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
                return Err("Project journals must be regular files".into());
            }
            archives.push((metadata.modified().unwrap_or(std::time::UNIX_EPOCH), entry.path()));
        }
    }
    if archives.len() > 4096 {
        return Err("Project history has too many archive segments".into());
    }
    archives.sort();
    let mut files: Vec<PathBuf> = archives.into_iter().map(|(_, path)| path).collect();
    let current = workspace.join("operations.jsonl");
    if current.exists() {
        files.push(current);
    }
    let mut base = 0_u64;
    let mut records = Vec::new();
    let mut next = offset;
    let mut used = 0_usize;
    for path in files {
        let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("Project journals must be regular files".into());
        }
        let end = base.checked_add(metadata.len()).ok_or("Project history offset overflow")?;
        if next >= end {
            base = end;
            continue;
        }
        let local = next.saturating_sub(base);
        let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        if local > 0 {
            file.seek(SeekFrom::Start(local - 1)).map_err(|e| e.to_string())?;
            let mut newline = [0_u8; 1];
            file.read_exact(&mut newline).map_err(|e| e.to_string())?;
            if newline != [b'\n'] {
                return Err("Project history offset must point to a record boundary".into());
            }
        } else {
            file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        }
        let mut reader = std::io::BufReader::new(file);
        loop {
            let mut line = Vec::new();
            let count = reader.by_ref().take((MAX_CONTEXT + 1) as u64).read_until(b'\n', &mut line).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            if count > MAX_CONTEXT {
                return Err("Project history record exceeds 512 KiB".into());
            }
            if count > MAX_CONTEXT - 1024 {
                return Err("Project history record exceeds the response budget".into());
            }
            if records.len() >= limit || used.saturating_add(count) > MAX_CONTEXT - 1024 {
                return Ok(json!({"records":records,"nextOffset":next}));
            }
            let value: Value = serde_json::from_slice(&line).map_err(|_| "Project history contains a damaged record")?;
            records.push(value);
            used = used.saturating_add(count);
            next = next.checked_add(count as u64).ok_or("Project history offset overflow")?;
        }
        base = end;
    }
    if offset > base {
        return Err("Project history offset is past the end of the journal".into());
    }
    Ok(json!({"records":records,"nextOffset":null}))
}

#[cfg(test)]
pub(super) fn prepare_in(context: &ProjectContext, root: &Path) -> Result<ProjectMemory, String> {
    prepare_role_in(context, root, "assets")
}

fn prepare_role_in(context: &ProjectContext, root: &Path, role: &str) -> Result<ProjectMemory, String> {
    if !["assets", "renderer"].contains(&role) {
        return Err("Unknown PhotoCraft conversation role".into());
    }
    let (workspace, base_path) = sync_in(context, root)?;
    let state_path = base_path.with_file_name(format!("codex-{role}-session.json"));
    let project_path = base_path.with_file_name("codex-project.json");
    let session_lock = lock_record(&base_path.with_file_name(format!("codex-{role}-session.lock")))?;
    let identity = workspace.to_string_lossy().to_string();
    let stored = read_json(&state_path)?;
    let matching = stored.as_ref().filter(|v| v["workspace"].as_str() == Some(identity.as_str()));
    let read_id = |key: &str| -> Result<Option<String>, String> {
        matching
            .and_then(|v| v.get(key))
            .filter(|v| !v.is_null())
            .map(|v| {
                let id = v.as_str().ok_or("Invalid local Codex identity")?;
                validate_id(id)?;
                Ok(id.to_owned())
            })
            .transpose()
    };
    let own_project = read_id("projectId")?;
    let shared = read_json(&project_path)?;
    let shared_project = if let Some(shared) = &shared {
        if shared["workspace"].as_str() != Some(identity.as_str()) {
            return Err("The local Codex project binding belongs to a different workspace".into());
        }
        let id = shared["projectId"].as_str().ok_or("Invalid shared Codex project identity")?;
        validate_id(id)?;
        Some(id.to_owned())
    } else {
        None
    };
    let previous_project_ids = match shared.as_ref().and_then(|v| v.get("previousProjectIds")) {
        None => Vec::new(),
        Some(value) => {
            let values = value.as_array().filter(|v| v.len() <= 16).ok_or("Invalid previous Codex project bindings")?;
            values
                .iter()
                .map(|value| {
                    let id = value.as_str().ok_or("Invalid previous Codex project identity")?;
                    validate_id(id)?;
                    Ok(id.to_owned())
                })
                .collect::<Result<Vec<_>, String>>()?
        }
    };
    if own_project.as_ref().zip(shared_project.as_ref()).is_some_and(|(own, shared)| own != shared && !previous_project_ids.contains(own)) {
        return Err("The local Codex conversation belongs to a different project".into());
    }
    let project_id = shared_project.or_else(|| own_project.clone());
    let thread_id = read_id("threadId")?;
    if thread_id.is_some() && project_id.is_none() {
        return Err("The local Codex conversation is missing its project binding".into());
    }
    let desktop_project_id = shared.as_ref().and_then(|v| v["desktopProjectId"].as_str()).map(str::to_owned);
    if let Some(id) = &desktop_project_id {
        validate_id(id)?;
    }
    Ok(ProjectMemory {
        workspace,
        state_path,
        project_path,
        identity,
        project_id,
        thread_id,
        thread_project_id: own_project,
        previous_project_ids,
        desktop_project_id,
        role: role.to_owned(),
        _session_lock: session_lock,
    })
}

impl ProjectMemory {
    pub(crate) fn ensure_desktop(&mut self, cancel: &std::sync::atomic::AtomicBool) -> Result<(), String> {
        let project = crate::codex_desktop::ensure_registration(&self.workspace, cancel, std::time::Duration::from_secs(90))?;
        self.bind_desktop(project)
    }

    pub(super) fn bind_desktop(&mut self, project: crate::codex_desktop::DesktopProject) -> Result<(), String> {
        validate_id(&project.server_id)?;
        validate_id(&project.desktop_id)?;
        if let Some(previous) = &self.project_id
            && previous != &project.server_id
            && !self.previous_project_ids.contains(previous)
        {
            if self.previous_project_ids.len() >= 16 {
                return Err("Too many previous Codex project bindings; no conversation was changed".into());
            }
            self.previous_project_ids.push(previous.clone());
        }
        self.project_id = Some(project.server_id);
        self.desktop_project_id = Some(project.desktop_id);
        self.save()
    }

    pub(crate) fn migration_request(&self) -> Option<(&'static str, Value)> {
        let thread = self.thread_id.as_ref()?;
        let project = self.project_id.as_ref()?;
        (self.thread_project_id.as_ref() != Some(project)).then(|| ("thread/read", json!({"threadId":thread,"includeTurns":false})))
    }

    pub(crate) fn accept_migration_source(&mut self, response: &Value) -> Result<Option<(&'static str, Value)>, String> {
        let thread = response.pointer("/result/thread").ok_or("Codex did not return the existing conversation")?;
        if thread["id"].as_str() != self.thread_id.as_deref() || thread["ephemeral"] == true {
            return Err("Codex returned another or temporary conversation; no project membership was changed".into());
        }
        if thread["projectId"].as_str() == self.project_id.as_deref() {
            self.accept_thread(response)?;
            return Ok(None);
        }
        if self.thread_project_id.is_none() || thread["projectId"].as_str() != self.thread_project_id.as_deref() {
            return Err("This conversation was moved to another Codex project; its membership was not changed".into());
        }
        Ok(Some(("thread/metadata/update", json!({"threadId":self.thread_id,"projectId":self.project_id}))))
    }

    pub(crate) fn accept_migration(&mut self, response: &Value) -> Result<(), String> {
        self.accept_thread(response).map(|_| ())
    }

    pub(crate) fn thread_name_request(&self, context: &ProjectContext) -> (&'static str, Value) {
        let name: String = context.name.chars().filter(|c| !c.is_control()).take(120).collect();
        let role = if self.role == "renderer" { "Image generation" } else { "AI Studio" };
        ("thread/name/set", json!({"threadId":self.thread_id,"name":format!("{name} - {role}")}))
    }

    pub(crate) fn project_request(&self, context: &ProjectContext) -> (&'static str, Value) {
        match &self.project_id {
            Some(id) => ("project/read", json!({"projectId":id})),
            None => {
                // Stable across restarts; this is a local identity suffix, not a security hash.
                let suffix = stable_suffix(&self.identity);
                (
                    "project/create",
                    json!({"idempotencyKey":format!("photocraft-{}-{suffix:016x}",context.id),
                    "name":format!("PhotoCraft - {}",context.name.chars().take(160).collect::<String>()),
                    "roots":[{"path":self.workspace}],"metadata":{"photocraftProjectId":context.id}}),
                )
            }
        }
    }

    pub(crate) fn accept_project(&mut self, response: &Value) -> Result<(), String> {
        let project = response.pointer("/result/project").ok_or("Codex did not return the linked project")?;
        let id = project["id"].as_str().ok_or("Codex returned an invalid project identity")?;
        validate_id(id)?;
        if self.project_id.as_deref().is_some_and(|expected| expected != id) {
            return Err("Codex returned another project; no conversation was started".into());
        }
        let roots_match = project["roots"].as_array().is_some_and(|roots| {
            roots.len() == 1
                && roots.iter().any(|v| {
                    v["path"].as_str().is_some_and(|p| {
                        Path::new(p) == self.workspace
                            || Path::new(p).canonicalize().ok().zip(self.workspace.canonicalize().ok()).is_some_and(|(left, right)| left == right)
                    })
                })
        });
        if !roots_match {
            return Err("The linked Codex project's folder changed; no conversation was started".into());
        }
        self.project_id = Some(id.to_owned());
        self.save()
    }

    pub(crate) fn thread_request(&self, mut params: Value) -> (&'static str, Value) {
        params["cwd"] = json!(self.workspace);
        if let Some(thread) = &self.thread_id {
            if let Some(object) = params.as_object_mut() {
                for key in ["ephemeral", "environments", "experimentalRawEvents"] {
                    object.remove(key);
                }
            }
            params["threadId"] = json!(thread);
            params["excludeTurns"] = json!(true);
            ("thread/resume", params)
        } else {
            params["ephemeral"] = json!(false);
            params["projectId"] = json!(self.project_id);
            ("thread/start", params)
        }
    }

    pub(crate) fn accept_thread(&mut self, response: &Value) -> Result<String, String> {
        let thread = response.pointer("/result/thread").ok_or("Codex returned no project conversation")?;
        let id = thread["id"].as_str().ok_or("Codex returned an invalid conversation identity")?;
        validate_id(id)?;
        if self.thread_id.as_deref().is_some_and(|expected| expected != id) || thread["projectId"].as_str() != self.project_id.as_deref() {
            return Err("Codex returned a conversation from another project; no generation was started".into());
        }
        if thread["ephemeral"] == true {
            return Err("Codex did not persist the project conversation".into());
        }
        self.thread_id = Some(id.to_owned());
        self.thread_project_id = self.project_id.clone();
        self.save()?;
        Ok(id.to_owned())
    }

    fn save(&self) -> Result<(), String> {
        let _guard = RECORD_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(project_id) = &self.project_id {
            if let Some(shared) = read_json(&self.project_path)?
                && shared["projectId"].as_str().is_some_and(|id| id != project_id && !self.previous_project_ids.iter().any(|old| old == id))
            {
                return Err("The Codex project binding changed in another worker; retry without changing its conversations".into());
            }
            atomic_json(
                &self.project_path,
                &json!({"version":2,"workspace":self.identity,"projectId":project_id,
                "desktopProjectId":self.desktop_project_id,"previousProjectIds":self.previous_project_ids}),
            )?;
        }
        let role_project = if self.thread_id.is_some() { self.thread_project_id.as_ref().or(self.project_id.as_ref()) } else { self.project_id.as_ref() };
        atomic_json(&self.state_path, &json!({"version":2,"workspace":self.identity,"projectId":role_project,"threadId":self.thread_id}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::ai::ProjectEvent;

    fn context(path: Option<String>) -> ProjectContext {
        ProjectContext { id: "test-project-1".into(), open_id: 1, name: "Player".into(), path, memory: "Green controls".into(), events: vec![] }
    }

    #[test]
    fn memory_and_operations_survive_reopening_without_duplicate_events() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = context(None);
        context.events.push(ProjectEvent { sequence: 1, kind: "layer.move".into(), summary: "Moved Play".into() });
        let mut first = prepare_in(&context, dir.path()).unwrap();
        let response = json!({"result":{"project":{"id":"codex-project","roots":[{"path":first.workspace}]}}});
        first.accept_project(&response).unwrap();
        first.accept_thread(&json!({"result":{"thread":{"id":"codex-thread","projectId":"codex-project","ephemeral":false}}})).unwrap();
        drop(first);
        let second = prepare_in(&context, dir.path()).unwrap();
        assert_eq!(second.thread_id.as_deref(), Some("codex-thread"));
        assert_eq!(std::fs::read_to_string(second.workspace.join("operations.jsonl")).unwrap().lines().count(), 2);
        assert!(std::fs::read_to_string(second.workspace.join("project-context.json")).unwrap().contains("Green controls"));
        let (method, params) = second.thread_request(json!({"ephemeral":true,"config":{"mcp_servers":{"fresh":true}}}));
        assert_eq!(method, "thread/resume");
        assert_eq!(params["config"]["mcp_servers"]["fresh"], true);
        assert!(params.get("ephemeral").is_none());
        assert_eq!(params["threadId"], "codex-thread");
    }

    #[test]
    fn active_writer_rejection_keeps_each_roles_original_conversation_for_retry() {
        let dir = tempfile::tempdir().unwrap();
        let context = context(None);
        for role in ["assets", "renderer"] {
            let thread_id = format!("original-{role}");
            let mut first = prepare_role_in(&context, dir.path(), role).unwrap();
            first.accept_project(&json!({"result":{"project":{"id":"same-project","roots":[{"path":first.workspace}]}}})).unwrap();
            first.accept_thread(&json!({"result":{"thread":{"id":thread_id,"projectId":"same-project","ephemeral":false}}})).unwrap();
            let workspace = first.workspace.clone();
            let state_path = first.state_path.clone();
            let project_path = first.project_path.clone();
            drop(first);
            let original_state = std::fs::read(&state_path).unwrap();
            let original_project = std::fs::read(&project_path).unwrap();

            let mut rejected = prepare_role_in(&context, dir.path(), role).unwrap();
            let (method, params) = rejected.thread_request(json!({"ephemeral":true}));
            assert_eq!(method, "thread/resume");
            assert_eq!(params["threadId"], thread_id);
            let response = json!({"id":3,"error":{"code":-32600,"message":"thread already has an active writer"}});
            assert!(crate::ai_generation::request_error(&response["error"]).contains("no replacement conversation"));
            assert!(rejected.accept_thread(&response).is_err());
            assert_eq!(rejected.thread_id.as_deref(), Some(thread_id.as_str()));
            drop(rejected);
            assert_eq!(std::fs::read(&state_path).unwrap(), original_state);
            assert_eq!(std::fs::read(&project_path).unwrap(), original_project);

            let retry = prepare_role_in(&context, dir.path(), role).unwrap();
            assert_eq!(retry.workspace, workspace);
            assert_eq!(retry.project_id.as_deref(), Some("same-project"));
            assert_eq!(retry.thread_id.as_deref(), Some(thread_id.as_str()));
            assert!(retry.migration_request().is_none());
            assert_eq!(retry.project_request(&context), ("project/read", json!({"projectId":"same-project"})));
            let (method, params) = retry.thread_request(json!({"ephemeral":true,"config":{"mcp_servers":{"fresh":true}}}));
            assert_eq!(method, "thread/resume", "retry must never start or fork a replacement conversation");
            assert_eq!(params["threadId"], thread_id);
            assert!(params.get("ephemeral").is_none());
            assert!(params.get("history").is_none(), "resume must keep the server's existing design history");
            assert_eq!(params["config"]["mcp_servers"]["fresh"], true);
        }
    }

    #[test]
    fn first_save_keeps_the_unsaved_workspace_thread_and_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = context(None);
        context.events.push(ProjectEvent { sequence: 1, kind: "create".into(), summary: "Original edit".into() });
        let mut old = prepare_in(&context, dir.path()).unwrap();
        old.project_id = Some("old-project".into());
        old.thread_id = Some("old-thread".into());
        old.save().unwrap();
        let workspace = old.workspace.clone();
        let state_path = old.state_path.clone();
        let journal = std::fs::read(workspace.join("operations.jsonl")).unwrap();
        drop(old);
        context.path = Some(dir.path().join("player.psd").to_string_lossy().into());
        let new = prepare_in(&context, dir.path()).unwrap();
        assert_eq!(new.thread_id.as_deref(), Some("old-thread"));
        assert_eq!(new.project_id.as_deref(), Some("old-project"));
        assert_eq!(new.workspace, workspace);
        assert_eq!(new.state_path, state_path);
        assert_eq!(std::fs::read(workspace.join("operations.jsonl")).unwrap(), journal);
        assert_eq!(read_json(&workspace.join("project-context.json")).unwrap().unwrap()["documentPath"], json!(context.path));
        assert_eq!(history_in(&context, dir.path(), 0, 100).unwrap()["records"].as_array().unwrap().len(), 2);
        drop(new);
        context.open_id += 1;
        assert_eq!(prepare_in(&context, dir.path()).unwrap().thread_id.as_deref(), Some("old-thread"));
    }

    #[test]
    fn save_as_after_first_save_keeps_a_separate_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = context(None);
        let (original_workspace, _) = sync_in(&context, dir.path()).unwrap();
        context.path = Some(dir.path().join("first.psd").to_string_lossy().into());
        assert_eq!(sync_in(&context, dir.path()).unwrap().0, original_workspace);
        context.path = Some(dir.path().join("copy.psd").to_string_lossy().into());
        let copy = prepare_in(&context, dir.path()).unwrap();
        assert_ne!(copy.workspace, original_workspace);
        assert!(copy.thread_id.is_none());
    }

    #[test]
    fn old_unsaved_worker_cannot_replace_first_saved_context() {
        let dir = tempfile::tempdir().unwrap();
        let unsaved = context(None);
        let (workspace, _) = sync_in(&unsaved, dir.path()).unwrap();
        let mut saved = unsaved.clone();
        saved.path = Some(dir.path().join("player.psd").to_string_lossy().into());
        saved.memory = "Saved project context".into();
        sync_in(&saved, dir.path()).unwrap();
        let record = std::fs::read(workspace.join("project-context.json")).unwrap();
        let journal = std::fs::read(workspace.join("operations.jsonl")).unwrap();
        sync_in(&unsaved, dir.path()).unwrap();
        assert_eq!(std::fs::read(workspace.join("project-context.json")).unwrap(), record);
        assert_eq!(std::fs::read(workspace.join("operations.jsonl")).unwrap(), journal);
    }

    #[test]
    fn first_save_needs_the_same_open_document_in_the_same_process() {
        for different_process in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut context = context(None);
            let (workspace, _) = sync_in(&context, dir.path()).unwrap();
            if different_process {
                atomic_json(&workspace.join("journal-cursor.json"), &json!({"processEpoch":"another-process","openId":context.open_id,"sequence":0})).unwrap();
            } else {
                context.open_id += 1;
            }
            context.path = Some(dir.path().join("player.psd").to_string_lossy().into());
            assert_ne!(sync_in(&context, dir.path()).unwrap().0, workspace);
        }
    }

    #[test]
    fn workspace_names_are_safe_and_stable_after_document_rename() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = context(Some(dir.path().join("player.psd").to_string_lossy().into()));
        context.name = "Music.Player.psd".into();
        let (workspace, state_path) = sync_in(&context, dir.path()).unwrap();
        assert_eq!(workspace.file_name().unwrap(), "Music.Player");
        context.name = "Renamed.psd".into();
        assert_eq!(sync_in(&context, dir.path()).unwrap().0, workspace);
        atomic_json(&state_path.with_file_name("workspace-name.json"), &json!({"version":1,"name":"../outside"})).unwrap();
        assert!(sync_in(&context, dir.path()).is_err());
        assert_eq!(clean_workspace_name("CON"), "_CON");
        assert_eq!(clean_workspace_name(".."), "Untitled");
        assert_eq!(clean_workspace_name("A/B:C*D?"), "A_B_C_D_");
        assert!(clean_workspace_name(&"x".repeat(500)).len() <= 180);
        let reserved = clean_workspace_name(&format!("CON.{}", "x".repeat(100)));
        assert_eq!(clean_workspace_name(&reserved), reserved);
        let unsaved = super::tests::context(None);
        assert_eq!(locations(&unsaved, dir.path()).unwrap().0.file_name().unwrap(), "Untitled-test-pro");
    }

    #[test]
    fn existing_legacy_workspace_locations_are_preserved() {
        for saved in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let context = context(saved.then(|| dir.path().join("player.psd").to_string_lossy().into()));
            let (source, parent) = source_identity(&context).unwrap();
            let trusted = trusted_directory(&context, dir.path(), &source);
            let workspace = parent.map_or_else(|| trusted.join("workspace"), |parent| parent.join(".photocraft").join(trusted.file_name().unwrap()));
            std::fs::create_dir_all(&workspace).unwrap();
            assert_eq!(sync_in(&context, dir.path()).unwrap().0, workspace);
            assert!(!trusted.join("workspace-name.json").exists());
        }
    }

    #[test]
    fn malformed_first_save_alias_cannot_redirect_project_history() {
        let dir = tempfile::tempdir().unwrap();
        let context = context(Some(dir.path().join("player.psd").to_string_lossy().into()));
        let (_, state_path) = sync_in(&context, dir.path()).unwrap();
        atomic_json(&state_path.with_file_name("first-save-alias.json"), &json!({"version":1,"projectId":"foreign-project","sourceIdentity":"../outside"}))
            .unwrap();
        assert!(sync_in(&context, dir.path()).is_err());
        assert!(history_in(&context, dir.path(), 0, 100).is_err());
    }

    #[test]
    fn desktop_registration_retains_both_role_threads_until_verified_migration() {
        let dir = tempfile::tempdir().unwrap();
        let context = context(None);
        for role in ["assets", "renderer"] {
            let mut memory = prepare_role_in(&context, dir.path(), role).unwrap();
            memory.accept_project(&json!({"result":{"project":{"id":"old-project","roots":[{"path":memory.workspace}]}}})).unwrap();
            memory.accept_thread(&json!({"result":{"thread":{"id":format!("old-{role}"),"projectId":"old-project","ephemeral":false}}})).unwrap();
        }
        let mut assets = prepare_in(&context, dir.path()).unwrap();
        assets
            .bind_desktop(crate::codex_desktop::DesktopProject { desktop_id: "desktop-1".into(), server_id: "visible-project".into(), name: "Player".into() })
            .unwrap();
        assert_eq!(assets.migration_request().unwrap().0, "thread/read");
        assert!(assets.accept_migration_source(&json!({"result":{"thread":{"id":"old-assets","projectId":"foreign-project"}}})).is_err());
        let update =
            assets.accept_migration_source(&json!({"result":{"thread":{"id":"old-assets","projectId":"old-project","ephemeral":false}}})).unwrap().unwrap();
        assert_eq!(update.0, "thread/metadata/update");
        assert_eq!(update.1["projectId"], "visible-project");
        drop(assets);
        let mut assets = prepare_in(&context, dir.path()).unwrap();
        assert_eq!(assets.thread_id.as_deref(), Some("old-assets"));
        assert_eq!(assets.thread_project_id.as_deref(), Some("old-project"));
        assert!(
            assets
                .accept_migration_source(&json!({"result":{"thread":{"id":"old-assets","projectId":"visible-project","ephemeral":false}}}))
                .unwrap()
                .is_none()
        );
        assert!(assets.migration_request().is_none());
        let renderer = prepare_role_in(&context, dir.path(), "renderer").unwrap();
        assert_eq!(renderer.thread_id.as_deref(), Some("old-renderer"));
        assert_eq!(renderer.project_id.as_deref(), Some("visible-project"));
        assert!(renderer.migration_request().is_some());
    }

    #[test]
    fn rejects_traversal_damaged_mapping_and_cross_project_thread() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = context(None);
        context.id = "../bad".into();
        assert!(prepare_in(&context, dir.path()).is_err());
        context.id = "safe".into();
        let mut local = prepare_in(&context, dir.path()).unwrap();
        local.project_id = Some("my-project".into());
        assert!(local.accept_project(&json!({"result":{"project":{"id":"my-project","roots":[{"path":local.workspace},{"path":dir.path()}]}}})).is_err());
        assert!(local.accept_thread(&json!({"result":{"thread":{"id":"other","projectId":"foreign"}}})).is_err());
        std::fs::write(&local.state_path, "broken").unwrap();
        drop(local);
        assert!(prepare_in(&context, dir.path()).err().unwrap().contains("damaged"));
    }

    #[test]
    fn missing_operation_range_is_marked_instead_of_fabricated() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = context(None);
        context.events.push(ProjectEvent { sequence: 9, kind: "layer.create".into(), summary: "Created Play".into() });
        let local = prepare_in(&context, dir.path()).unwrap();
        let lines = std::fs::read_to_string(local.workspace.join("operations.jsonl")).unwrap();
        assert!(lines.contains("historyGap"));
        assert!(lines.contains("Created Play"));
    }

    #[test]
    fn copying_a_psd_in_the_same_directory_cannot_borrow_its_codex_thread() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = context(Some(dir.path().join("first.psd").to_string_lossy().into()));
        let mut first = prepare_in(&context, dir.path()).unwrap();
        first.project_id = Some("private-project".into());
        first.thread_id = Some("private-thread".into());
        first.save().unwrap();
        context.path = Some(dir.path().join("copied.psd").to_string_lossy().into());
        let second = prepare_in(&context, dir.path()).unwrap();
        assert_ne!(first.workspace, second.workspace);
        assert_ne!(first.state_path, second.state_path);
        assert!(second.thread_id.is_none());
    }

    #[test]
    fn reopened_older_snapshot_records_new_identical_operations_without_skipping() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = context(None);
        context.events = vec![
            ProjectEvent { sequence: 1, kind: "move".into(), summary: "Move layer".into() },
            ProjectEvent { sequence: 2, kind: "move".into(), summary: "Move layer".into() },
        ];
        let (workspace, _) = sync_in(&context, dir.path()).unwrap();
        context.open_id = 2;
        context.events.pop();
        sync_in(&context, dir.path()).unwrap();
        context.events.push(ProjectEvent { sequence: 2, kind: "move".into(), summary: "Move layer".into() });
        sync_in(&context, dir.path()).unwrap();
        let records: Vec<Value> =
            std::fs::read_to_string(workspace.join("operations.jsonl")).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        assert_eq!(records.iter().filter(|record| record["sequence"] == 2).count(), 2);
        assert_eq!(records.last().unwrap()["retainedSnapshot"], false);
        assert_eq!(records.iter().filter(|record| record["kind"] == "documentOpened").count(), 2);
    }

    #[test]
    fn stale_generation_snapshot_cannot_replace_newer_context() {
        let dir = tempfile::tempdir().unwrap();
        let old = context(None);
        let mut current = old.clone();
        current.memory = "Newest edit".into();
        current.events.push(ProjectEvent { sequence: 1, kind: "move".into(), summary: "New edit".into() });
        let (workspace, _) = sync_in(&current, dir.path()).unwrap();
        sync_in(&old, dir.path()).unwrap();
        assert_eq!(read_json(&workspace.join("project-context.json")).unwrap().unwrap()["memory"], "Newest edit");
    }

    #[test]
    fn closed_document_worker_cannot_replay_over_a_newer_open_instance() {
        for legacy_cursor in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut old = context(Some(dir.path().join("player.psd").to_string_lossy().into()));
            old.memory = "Old worker context".into();
            old.events.push(ProjectEvent { sequence: 9, kind: "move".into(), summary: "Old edit".into() });
            let (workspace, _) = sync_in(&old, dir.path()).unwrap();
            let mut current = old.clone();
            current.open_id = 2;
            current.memory = "Newly opened project context".into();
            current.events = vec![ProjectEvent { sequence: 1, kind: "move".into(), summary: "New edit".into() }];
            sync_in(&current, dir.path()).unwrap();
            if legacy_cursor {
                atomic_json(&workspace.join("journal-cursor.json"), &json!({"session":open_session(current.open_id),"sequence":1})).unwrap();
            }
            let before_context = std::fs::read(workspace.join("project-context.json")).unwrap();
            let before_journal = std::fs::read(workspace.join("operations.jsonl")).unwrap();
            let renderer = prepare_role_in(&old, dir.path(), "renderer").unwrap();
            assert_eq!(renderer.workspace, workspace, "old jobs can still resolve their trusted binding");
            assert_eq!(std::fs::read(workspace.join("project-context.json")).unwrap(), before_context);
            assert_eq!(std::fs::read(workspace.join("operations.jsonl")).unwrap(), before_journal);
            assert!(!history_in(&old, dir.path(), 0, 100).unwrap()["records"].as_array().unwrap().is_empty());
        }
    }

    #[test]
    fn history_pages_across_archives_and_refuses_non_record_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = context(None);
        context.events.push(ProjectEvent { sequence: 1, kind: "create".into(), summary: "New layer".into() });
        let (workspace, _) = sync_in(&context, dir.path()).unwrap();
        std::fs::rename(workspace.join("operations.jsonl"), workspace.join("operations-first.jsonl")).unwrap();
        context.events.push(ProjectEvent { sequence: 2, kind: "move".into(), summary: "Moved layer".into() });
        sync_in(&context, dir.path()).unwrap();
        let first = history_in(&context, dir.path(), 0, 2).unwrap();
        assert_eq!(first["records"].as_array().unwrap().len(), 2);
        let second = history_in(&context, dir.path(), first["nextOffset"].as_u64().unwrap(), 2).unwrap();
        assert_eq!(second["records"][0]["sequence"], 2);
        assert!(second["nextOffset"].is_null());
        assert!(history_in(&context, dir.path(), 1, 2).is_err());
        assert!(history_in(&context, dir.path(), 0, 101).is_err());
    }

    #[test]
    fn role_locks_prevent_competing_threads_but_allow_nested_renderer() {
        let dir = tempfile::tempdir().unwrap();
        let context = context(None);
        let mut assets = prepare_in(&context, dir.path()).unwrap();
        assets.accept_project(&json!({"result":{"project":{"id":"shared-project","roots":[{"path":assets.workspace}]}}})).unwrap();
        assert!(prepare_in(&context, dir.path()).is_err());
        let renderer = prepare_role_in(&context, dir.path(), "renderer").unwrap();
        assert_eq!(assets.workspace, renderer.workspace);
        assert_ne!(assets.state_path, renderer.state_path);
        assert_eq!(renderer.project_id.as_deref(), Some("shared-project"));
        assert_eq!(renderer.project_request(&context).0, "project/read");
    }

    #[test]
    fn background_writer_flushes_and_drains_all_accepted_updates_on_drop() {
        let records = Arc::new(Mutex::new(Vec::new()));
        let saved = records.clone();
        let writer = start_writer(move |context| {
            saved.lock().unwrap().push(context.open_id);
            Ok(())
        })
        .unwrap();
        for index in 0..20 {
            let mut context = context(None);
            context.open_id = index;
            writer.link.tx.send(WriteMessage::Save(context)).unwrap();
        }
        let (tx, rx) = mpsc::channel();
        writer.link.tx.send(WriteMessage::Flush { project: None, reply: tx }).unwrap();
        rx.recv().unwrap().unwrap();
        assert_eq!(records.lock().unwrap().len(), 20);
        writer.link.tx.send(WriteMessage::Save(context(None))).unwrap();
        drop(writer);
        assert_eq!(records.lock().unwrap().len(), 21);
    }
}
