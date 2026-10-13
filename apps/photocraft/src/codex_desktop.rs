//! Read-only adapter for the installed Codex desktop's local-project projection.
//! Registration goes through the desktop deep link and its normal folder-consent flow.
//! Never write desktop state or start a model turn here. This private projection may change;
//! unsupported layouts fail explicitly rather than pretending a server-only project is visible.
use serde_json::Value;
use std::{
    ffi::OsString,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const MAX_STATE_BYTES: usize = 16 << 20;
const MAX_WAIT: Duration = Duration::from_secs(90);
const PROJECTS: &str = "local-projects";
const SERVER_IDS: &str = "app-server-project-id-by-legacy-project-id-by-host";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DesktopProject {
    pub desktop_id: String,
    pub server_id: String,
    pub name: String,
}

fn codex_home_from(env: impl Fn(&str) -> Option<OsString>, windows: bool) -> Result<PathBuf, String> {
    let nonempty = |key| env(key).filter(|value| !value.is_empty()).map(PathBuf::from);
    let path = if let Some(home) = nonempty("CODEX_HOME") {
        home
    } else {
        let home = if windows { nonempty("USERPROFILE").or_else(|| nonempty("HOME")) } else { nonempty("HOME") };
        home.ok_or("Cannot locate Codex desktop data: no user home directory")?.join(".codex")
    };
    if !path.is_absolute() {
        return Err("Cannot locate Codex desktop data: CODEX_HOME or the user home must be an absolute path".into());
    }
    Ok(path)
}

fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("The Codex project folder must be an absolute path".into());
    }
    let resolved = path.canonicalize().map_err(|e| format!("Cannot access the Codex project folder: {e}"))?;
    if !resolved.is_dir() {
        return Err("The Codex project folder must be an existing directory".into());
    }
    Ok(resolved)
}

fn same_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.as_os_str().to_string_lossy().eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn is_matching_path(candidate: &str, canonical: &Path) -> bool {
    let path = Path::new(candidate);
    path.is_absolute() && path.canonicalize().is_ok_and(|resolved| same_path(&resolved, canonical))
}

fn required_text<'a>(value: &'a Value, field: &str, max: usize) -> Result<&'a str, String> {
    value
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= max && !v.chars().any(char::is_control))
        .ok_or_else(|| format!("Unsupported Codex desktop state: invalid {field}"))
}

fn project_from_state(state: &Value, workspace: &Path, home: &Path) -> Result<Option<DesktopProject>, String> {
    let object = state.as_object().ok_or("Unsupported Codex desktop state: expected a JSON object")?;
    let Some(projects) = object.get(PROJECTS) else { return Ok(None) };
    let projects = projects.as_object().ok_or("Unsupported Codex desktop state: local-projects must be an object")?;
    let mut candidate = None;
    for (key, project) in projects {
        let roots = project.get("rootPaths").and_then(Value::as_array).ok_or("Unsupported Codex desktop state: project rootPaths must be an array")?;
        let mut matches = false;
        for root in roots {
            let root = root.as_str().ok_or("Unsupported Codex desktop state: project rootPaths must contain paths")?;
            matches |= is_matching_path(root, workspace);
        }
        if !matches {
            continue;
        }
        if roots.len() != 1 {
            return Err("This folder belongs to a multi-folder Codex project; PhotoCraft needs a dedicated single-folder project".into());
        }
        if candidate.is_some() {
            return Err("Several Codex desktop projects use this folder; resolve the duplicate project before generating".into());
        }
        let id = required_text(project.get("id").unwrap_or(&Value::Null), "project id", 256)?;
        if id != key {
            return Err("Unsupported Codex desktop state: project identity does not match its key".into());
        }
        let name = required_text(project.get("name").unwrap_or(&Value::Null), "project name", 1024)?;
        candidate = Some((id, name));
    }
    let Some((desktop_id, name)) = candidate else { return Ok(None) };
    let Some(hosts) = object.get(SERVER_IDS) else { return Ok(None) };
    let hosts = hosts.as_object().ok_or("Unsupported Codex desktop state: server project mappings must be an object")?;
    let mut host_map = None;
    for (host, mapping) in hosts {
        let Some(path) = host.strip_prefix("local:") else { continue };
        if !is_matching_path(path, home) {
            continue;
        }
        if host_map.is_some() {
            return Err("Several Codex desktop local hosts match CODEX_HOME; cannot choose a project safely".into());
        }
        host_map = Some(mapping.as_object().ok_or("Unsupported Codex desktop state: local server project mapping must be an object")?);
    }
    let Some(server_id) = host_map.and_then(|map| map.get(desktop_id)) else { return Ok(None) };
    let server_id = required_text(server_id, "server project id", 256)?;
    Ok(Some(DesktopProject { desktop_id: desktop_id.into(), server_id: server_id.into(), name: name.into() }))
}

fn read_project(state_path: &Path, home: &Path, workspace: &Path) -> Result<Option<DesktopProject>, String> {
    let file = match File::open(state_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Cannot read Codex desktop project state: {error}")),
    };
    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|e| format!("Cannot read Codex desktop project state: {e}"))?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err("Codex desktop project state exceeds the 16 MiB adapter limit".into());
    }
    let state: Value = serde_json::from_slice(&bytes).map_err(|e| format!("Cannot parse Codex desktop project state: {e}"))?;
    let home = home.canonicalize().map_err(|e| format!("Cannot resolve CODEX_HOME: {e}"))?;
    project_from_state(&state, workspace, &home)
}

/// Finds the installed desktop's registered single-root project. A visible entry whose
/// app-server mapping is not persisted yet remains pending (`None`). No files are modified.
pub(crate) fn desktop_project_for_workspace(workspace: &Path) -> Result<Option<DesktopProject>, String> {
    let workspace = canonical_directory(workspace)?;
    let home = codex_home_from(|key| std::env::var_os(key), cfg!(windows))?;
    read_project(&home.join(".codex-global-state.json"), &home, &workspace)
}

fn registration_url(workspace: &Path) -> Result<String, String> {
    let workspace = canonical_directory(workspace)?;
    let text = workspace.to_str().ok_or("The Codex project folder is not valid UTF-8")?;
    #[cfg(windows)]
    let text = if let Some(unc) = text.strip_prefix("\\\\?\\UNC\\") { format!("\\\\{unc}") } else { text.strip_prefix("\\\\?\\").unwrap_or(text).to_owned() };
    let mut url = reqwest::Url::parse("codex://new").map_err(|e| format!("Cannot construct Codex desktop link: {e}"))?;
    url.query_pairs_mut().append_pair("path", text.as_ref());
    Ok(url.to_string())
}

/// Opens only the normal folder-registration/composer route. It never supplies a prompt,
/// approves folder access, sends a message, or starts image generation.
pub(crate) fn request_registration(workspace: &Path) -> Result<(), String> {
    open::that(registration_url(workspace)?).map_err(|e| format!("Cannot open Codex desktop. Open Codex and try again: {e}"))
}

fn ensure_with(
    cancel: &AtomicBool,
    timeout: Duration,
    mut lookup: impl FnMut() -> Result<Option<DesktopProject>, String>,
    open: impl FnOnce() -> Result<(), String>,
    mut elapsed: impl FnMut() -> Duration,
    mut sleep: impl FnMut(Duration),
) -> Result<DesktopProject, String> {
    let timeout = timeout.min(MAX_WAIT);
    let cancelled = || if cancel.load(Ordering::Relaxed) { Err("Codex project registration cancelled".to_owned()) } else { Ok(()) };
    cancelled()?;
    if let Some(project) = lookup()? {
        cancelled()?;
        return Ok(project);
    }
    if elapsed() >= timeout {
        return Err("Codex project registration timed out before opening the desktop".into());
    }
    cancelled()?;
    open()?;
    let mut polls = 0u32;
    loop {
        cancelled()?;
        let remaining = timeout.saturating_sub(elapsed());
        if remaining.is_zero() {
            return Err(
                "Codex has not registered this project yet. Complete its folder-access confirmation, then generate again; no image generation was started"
                    .into(),
            );
        }
        sleep(remaining.min(if polls < 5 { Duration::from_millis(100) } else { Duration::from_millis(250) }));
        cancelled()?;
        if let Some(project) = lookup()? {
            cancelled()?;
            return Ok(project);
        }
        polls = polls.saturating_add(1);
    }
}

/// Blocking helper for a generation worker, never the UI thread. Opens the consent route at
/// most once and waits at most 90 seconds for both desktop and app-server project identities.
pub(crate) fn ensure_registration(workspace: &Path, cancel: &AtomicBool, timeout: Duration) -> Result<DesktopProject, String> {
    let workspace = canonical_directory(workspace)?;
    let started = Instant::now();
    ensure_with(cancel, timeout, || desktop_project_for_workspace(&workspace), || request_registration(&workspace), || started.elapsed(), std::thread::sleep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;

    #[test]
    #[ignore = "Requires a running Codex desktop and explicit folder-consent approval; no model turn"]
    fn codex_desktop_real_registration_smoke() {
        let workspace = PathBuf::from(std::env::var_os("PHOTOCRAFT_CODEX_PROJECT_SMOKE_WORKSPACE").expect("Set an existing PhotoCraft workspace"));
        let project = ensure_registration(&workspace, &AtomicBool::new(false), Duration::from_secs(90)).unwrap();
        assert_eq!(desktop_project_for_workspace(&workspace).unwrap(), Some(project.clone()));
        eprintln!("Registered desktop project: {} ({}) -> {}", project.name, project.desktop_id, project.server_id);
    }

    fn fixture(root: &Path) -> (Value, PathBuf, PathBuf) {
        let home = root.join("codex-home");
        let workspace = root.join("player");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let host = format!("local:{}", home.display());
        (
            json!({PROJECTS:{"desktop-1":{"id":"desktop-1","name":"player","rootPaths":[workspace]}},
            SERVER_IDS:{host:{"desktop-1":"server-1"}},"unrelated-sensitive-setting":"never returned"}),
            home.canonicalize().unwrap(),
            workspace.canonicalize().unwrap(),
        )
    }

    #[test]
    fn desktop_lookup_matches_only_the_local_single_root_project() {
        let temp = tempfile::tempdir().unwrap();
        let (mut state, home, workspace) = fixture(temp.path());
        assert_eq!(
            project_from_state(&state, &workspace, &home).unwrap(),
            Some(DesktopProject { desktop_id: "desktop-1".into(), server_id: "server-1".into(), name: "player".into() })
        );
        state[SERVER_IDS] = json!({"remote:host":{"desktop-1":"wrong-server"}});
        assert_eq!(project_from_state(&state, &workspace, &home).unwrap(), None);
        state[PROJECTS]["desktop-1"]["rootPaths"] = json!([workspace, home]);
        assert!(project_from_state(&state, &workspace, &home).unwrap_err().contains("multi-folder"));
    }

    #[test]
    fn duplicates_and_changed_schema_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let (mut state, home, workspace) = fixture(temp.path());
        state[PROJECTS]["desktop-2"] = json!({"id":"desktop-2","name":"duplicate","rootPaths":[workspace]});
        assert!(project_from_state(&state, &workspace, &home).unwrap_err().contains("Several"));
        state[PROJECTS] = json!([]);
        assert!(project_from_state(&state, &workspace, &home).unwrap_err().contains("Unsupported"));
        assert!(project_from_state(&json!([]), &workspace, &home).is_err());
    }

    #[test]
    fn reads_are_bounded_and_missing_or_incomplete_registration_is_pending() {
        let temp = tempfile::tempdir().unwrap();
        let (mut state, home, workspace) = fixture(temp.path());
        let path = temp.path().join("desktop-state-fixture.json");
        assert_eq!(read_project(&path, &home, &workspace).unwrap(), None);
        state.as_object_mut().unwrap().remove(SERVER_IDS);
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(read_project(&path, &home, &workspace).unwrap(), None);
        std::fs::write(&path, b"{").unwrap();
        assert!(read_project(&path, &home, &workspace).unwrap_err().contains("Cannot parse"));
        std::fs::write(&path, vec![b' '; MAX_STATE_BYTES + 1]).unwrap();
        assert!(read_project(&path, &home, &workspace).unwrap_err().contains("16 MiB"));
    }

    #[test]
    fn deep_link_contains_only_an_encoded_existing_directory() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("player #1 & mix=2");
        std::fs::create_dir(&folder).unwrap();
        let link = registration_url(&folder).unwrap();
        let parsed = reqwest::Url::parse(&link).unwrap();
        assert_eq!(parsed.scheme(), "codex");
        assert_eq!(parsed.host_str(), Some("new"));
        let pairs: Vec<_> = parsed.query_pairs().collect();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "path");
        assert!(Path::new(pairs[0].1.as_ref()).is_absolute());
        assert!(is_matching_path(&pairs[0].1, &folder.canonicalize().unwrap()));
        assert!(!link.contains('#') && link.contains("%23") && link.contains("%26"));
        assert!(registration_url(Path::new("relative")).is_err());
        assert!(registration_url(&folder.join("missing")).is_err());
    }

    #[test]
    fn home_resolution_honors_override_and_platform_home() {
        let temp = tempfile::tempdir().unwrap();
        let override_home = temp.path().join("custom");
        let user = temp.path().join("user");
        let resolve = |key: &str| match key {
            "CODEX_HOME" => Some(override_home.as_os_str().to_owned()),
            "HOME" | "USERPROFILE" => Some(user.as_os_str().to_owned()),
            _ => None,
        };
        assert_eq!(codex_home_from(resolve, true).unwrap(), override_home);
        assert_eq!(codex_home_from(|key| (key == "HOME").then(|| user.as_os_str().to_owned()), false).unwrap(), user.join(".codex"));
        assert_eq!(codex_home_from(|key| (key == "USERPROFILE").then(|| user.as_os_str().to_owned()), true).unwrap(), user.join(".codex"));
        assert!(codex_home_from(|_| None, false).is_err());
        assert!(codex_home_from(|key| (key == "CODEX_HOME").then(|| OsString::from("relative")), false).is_err());
    }

    fn sample_project() -> DesktopProject {
        DesktopProject { desktop_id: "desktop".into(), server_id: "server".into(), name: "player".into() }
    }

    #[test]
    fn existing_registration_never_opens_desktop() {
        let opens = Cell::new(0);
        let project = ensure_with(
            &AtomicBool::new(false),
            Duration::from_secs(1),
            || Ok(Some(sample_project())),
            || {
                opens.set(opens.get() + 1);
                Ok(())
            },
            || Duration::ZERO,
            |_| {},
        )
        .unwrap();
        assert_eq!(project, sample_project());
        assert_eq!(opens.get(), 0);
    }

    #[test]
    fn registration_opens_once_then_polls_without_real_sleep_or_desktop_side_effects() {
        let opens = Cell::new(0);
        let polls = Cell::new(0);
        let elapsed = Cell::new(Duration::ZERO);
        let project = ensure_with(
            &AtomicBool::new(false),
            Duration::from_secs(2),
            || {
                polls.set(polls.get() + 1);
                Ok((polls.get() == 8).then(sample_project))
            },
            || {
                opens.set(opens.get() + 1);
                Ok(())
            },
            || elapsed.get(),
            |d| elapsed.set(elapsed.get() + d),
        )
        .unwrap();
        assert_eq!(project, sample_project());
        assert_eq!(opens.get(), 1);
        assert_eq!(elapsed.get(), Duration::from_secs(1));
    }

    #[test]
    fn cancellation_timeout_and_schema_failure_never_start_generation() {
        let cancel = AtomicBool::new(true);
        let opens = Cell::new(0);
        let opener = || {
            opens.set(opens.get() + 1);
            Ok(())
        };
        assert!(ensure_with(&cancel, Duration::from_secs(1), || Ok(None), opener, || Duration::ZERO, |_| {}).unwrap_err().contains("cancelled"));
        assert_eq!(opens.get(), 0);
        cancel.store(false, Ordering::Relaxed);
        let elapsed = Cell::new(Duration::ZERO);
        assert!(
            ensure_with(&cancel, Duration::from_secs(500), || Ok(None), opener, || elapsed.get(), |d| elapsed.set(elapsed.get() + d))
                .unwrap_err()
                .contains("no image generation")
        );
        assert_eq!(elapsed.get(), MAX_WAIT);
        assert_eq!(opens.get(), 1);
        assert!(ensure_with(&cancel, Duration::from_secs(1), || Err("incompatible desktop".into()), opener, || Duration::ZERO, |_| {}).is_err());
        assert_eq!(opens.get(), 1);
        assert!(
            ensure_with(&cancel, Duration::from_secs(1), || Ok(None), opener, || Duration::ZERO, |_| cancel.store(true, Ordering::Relaxed))
                .unwrap_err()
                .contains("cancelled")
        );
        assert_eq!(opens.get(), 2);
    }
}
