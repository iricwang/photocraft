//! Bounded, project-scoped disk operations for the native project store.
//! No caller may turn an untrusted resource name into an arbitrary host path.
use crate::defold_project_model::{ProjectError, ProjectErrorCode, ProjectResult};
use serde::{Serialize, de::DeserializeOwned};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

fn io_error(error: impl std::fmt::Display) -> ProjectError {
    ProjectError::new(ProjectErrorCode::Io, error.to_string())
}

/// Resolve a resource inside its project, rejecting aliases through symlinks.
/// Parent directories can be created only below a verified canonical ancestor.
pub(crate) fn scoped_path(root: &Path, resource: &str, create_parent: bool) -> ProjectResult<PathBuf> {
    let relative = resource.strip_prefix('/').unwrap_or(resource);
    if relative.is_empty() || relative.contains(['\\', ':', '\0']) || relative.starts_with('/') {
        return Err(ProjectError::new(ProjectErrorCode::PathInvalid, "Invalid project resource path"));
    }
    let parts: Vec<_> = Path::new(relative).components().collect();
    if parts.is_empty()
        || parts.iter().any(|p| !matches!(p, Component::Normal(_)))
        || relative.split('/').any(|p| p.is_empty() || p == "." || p == ".." || p.ends_with([' ', '.']))
    {
        return Err(ProjectError::new(ProjectErrorCode::PathInvalid, "Resource path must be project-relative"));
    }
    let canonical = fs::canonicalize(root).map_err(io_error)?;
    let mut result = canonical.clone();
    for (index, part) in parts.iter().enumerate() {
        result.push(part.as_os_str());
        let last = index + 1 == parts.len();
        match fs::symlink_metadata(&result) {
            Ok(metadata) => {
                // canonicalize also catches Windows junctions whose FileType is not a symlink.
                let actual = fs::canonicalize(&result).map_err(io_error)?;
                if metadata.file_type().is_symlink() || !actual.starts_with(&canonical) {
                    return Err(ProjectError::new(ProjectErrorCode::PathInvalid, "Project resource crosses a link or project boundary"));
                }
                if !last && !metadata.is_dir() {
                    return Err(ProjectError::new(ProjectErrorCode::PathInvalid, "Resource parent is not a directory"));
                }
                if last && !metadata.is_file() {
                    return Err(ProjectError::new(ProjectErrorCode::PathInvalid, "Resource is not a regular file"));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !last && create_parent {
                    fs::create_dir(&result).map_err(io_error)?;
                } else if !last {
                    // A read of a nonexistent nested path stays within the same verified root.
                    continue;
                }
            }
            Err(error) => return Err(io_error(error)),
        }
    }
    Ok(result)
}

pub(crate) fn read_bounded(path: &Path, limit: u64) -> ProjectResult<Vec<u8>> {
    let file = File::open(path).map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(ProjectError::new(ProjectErrorCode::LimitExceeded, "Project resource exceeds its size limit"));
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes).map_err(io_error)?;
    if bytes.len() as u64 > limit {
        return Err(ProjectError::new(ProjectErrorCode::LimitExceeded, "Project resource grew beyond its size limit"));
    }
    Ok(bytes)
}

pub(crate) fn read_json<T: DeserializeOwned>(path: &Path, limit: u64) -> ProjectResult<T> {
    serde_json::from_slice(&read_bounded(path, limit)?).map_err(io_error)
}

/// Atomic replacement on the same volume. sync_all precedes the pointer change.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> ProjectResult<()> {
    let parent = path.parent().ok_or_else(|| ProjectError::new(ProjectErrorCode::PathInvalid, "Missing resource parent"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
    file.write_all(bytes).map_err(io_error)?;
    file.as_file().sync_all().map_err(io_error)?;
    file.persist(path).map_err(|error| io_error(error.error))?;
    Ok(())
}

pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> ProjectResult<()> {
    write_atomic(path, &serde_json::to_vec_pretty(value).map_err(io_error)?)
}

/// OS-backed lock: a crashed process cannot leave a permanent exclusive owner.
pub(crate) fn lock_project(root: &Path) -> ProjectResult<File> {
    let path = scoped_path(root, ".photocraft/local/store.lock", true)?;
    let file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path).map_err(io_error)?;
    file.try_lock().map_err(|_| ProjectError::new(ProjectErrorCode::Busy, "Another process is writing this project"))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_reject_host_and_parent_aliases() {
        let project = tempfile::tempdir().unwrap();
        for path in ["", "../outside", "/../outside", "C:/outside", "//server/share", "a\\b", "a/./b", "a//b", "a/../b", "a./b"] {
            assert!(scoped_path(project.path(), path, false).is_err(), "{path}");
        }
        assert_eq!(scoped_path(project.path(), "/assets/a.png", false).unwrap(), project.path().canonicalize().unwrap().join("assets/a.png"));
    }

    #[test]
    fn bounded_atomic_files_and_os_lock_survive_reopening() {
        let project = tempfile::tempdir().unwrap();
        let guard = lock_project(project.path()).unwrap();
        assert!(lock_project(project.path()).is_err());
        let path = scoped_path(project.path(), ".photocraft/sources/test.json", true).unwrap();
        write_atomic(&path, b"first").unwrap();
        assert!(read_bounded(&path, 4).is_err());
        write_atomic(&path, b"new").unwrap();
        assert_eq!(read_bounded(&path, 4).unwrap(), b"new");
        drop(guard);
        assert!(lock_project(project.path()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_inside_project_cannot_redirect_writes() {
        let project = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(other.path(), project.path().join("escape")).unwrap();
        assert!(scoped_path(project.path(), "escape/file", true).is_err());
        assert!(!other.path().join("file").exists());
    }
}
