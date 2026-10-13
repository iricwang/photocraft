//! A stable MCP process reads a short-lived task lease afresh for every call.
use serde_json::Value;
use std::fs::{self, File, Metadata};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) struct LiveContext {
    path: PathBuf,
    limit: u64,
}

fn validate_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("Task context must use an absolute local path without parent traversal".into());
    }
    #[cfg(windows)]
    if path.components().any(|c| matches!(c, Component::Prefix(p) if !matches!(p.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))) {
        return Err("Task context must use a local drive path".into());
    }
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) if linked(&meta) => return Err("Task context cannot use symbolic links or reparse points".into()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("Cannot inspect task context path".into()),
        }
    }
    Ok(())
}

fn linked(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_type().is_symlink() || meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    meta.file_type().is_symlink()
}

fn same_file(a: &Metadata, b: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if a.dev() != b.dev() || a.ino() != b.ino() {
            return false;
        }
    }
    a.len() == b.len() && a.modified().ok() == b.modified().ok() && a.created().ok() == b.created().ok()
}

impl LiveContext {
    pub(crate) fn new(path: PathBuf, limit: u64) -> Result<Self, String> {
        validate_path(&path)?;
        Ok(Self { path, limit })
    }

    pub(crate) fn read(&self) -> Result<Value, String> {
        validate_path(&self.path)?;
        let before = fs::symlink_metadata(&self.path).map_err(|_| "No active PhotoCraft task context")?;
        if !before.is_file() || linked(&before) || before.len() > self.limit {
            return Err("Invalid or oversized task context".into());
        }
        let file = File::open(&self.path).map_err(|_| "Cannot open active task context")?;
        let opened = file.metadata().map_err(|_| "Cannot inspect active task context")?;
        if !same_file(&before, &opened) {
            return Err("Task context changed while opening".into());
        }
        let mut bytes = Vec::new();
        file.take(self.limit + 1).read_to_end(&mut bytes).map_err(|_| "Cannot read active task context")?;
        validate_path(&self.path)?;
        let after = fs::symlink_metadata(&self.path).map_err(|_| "Task context was withdrawn")?;
        if bytes.len() as u64 > self.limit || !after.is_file() || linked(&after) || !same_file(&before, &after) {
            return Err("Task context changed while reading".into());
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| "Malformed task context")?;
        let lease = value.get("leaseId").and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() <= 256);
        let expires = value.get("expiresAtMs").and_then(Value::as_u64);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "Invalid system clock")?.as_millis();
        if value.get("version").and_then(Value::as_u64) != Some(1) || lease.is_none() || expires.is_none_or(|expiry| u128::from(expiry) <= now) {
            return Err("Invalid or expired PhotoCraft task lease".into());
        }
        Ok(value)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) struct Fixture(pub PathBuf);
    impl Fixture {
        pub(crate) fn new() -> Self {
            let mut nonce = [0_u8; 8];
            getrandom::fill(&mut nonce).unwrap();
            let path = std::env::temp_dir().join(format!("photocraft-live-{}-{:x}.json", std::process::id(), u64::from_le_bytes(nonce)));
            Self(path)
        }
        pub(crate) fn write(&self, value: &Value) {
            fs::write(&self.0, serde_json::to_vec(value).unwrap()).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    pub(crate) fn lease() -> Value {
        serde_json::json!({"version":1,"leaseId":"task-a","expiresAtMs":u64::MAX})
    }

    #[test]
    fn missing_expired_malformed_oversized_and_relative_contexts_fail_closed() {
        assert!(LiveContext::new(PathBuf::from("relative.json"), 1024).is_err());
        let fixture = Fixture::new();
        let live = LiveContext::new(fixture.0.clone(), 1024).unwrap();
        assert!(live.read().is_err());
        let mut value = lease();
        fixture.write(&value);
        assert!(live.read().is_ok());
        value["expiresAtMs"] = 1.into();
        fixture.write(&value);
        assert!(live.read().is_err());
        value = lease();
        value["leaseId"] = "".into();
        fixture.write(&value);
        assert!(live.read().is_err());
        fs::write(&fixture.0, "{".repeat(1025)).unwrap();
        assert!(live.read().is_err());
        fs::write(&fixture.0, "{").unwrap();
        assert!(live.read().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_context_is_rejected() {
        let target = Fixture::new();
        target.write(&lease());
        let link = Fixture::new();
        std::os::unix::fs::symlink(&target.0, &link.0).unwrap();
        assert!(LiveContext::new(link.0.clone(), 1024).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn network_and_device_paths_are_rejected() {
        for path in [r"\\server\share\lease.json", r"\\?\UNC\server\share\lease.json", r"\\.\pipe\lease", r"C:\tasks\..\lease.json"] {
            assert!(LiveContext::new(PathBuf::from(path), 1024).is_err());
        }
    }
}
