//! Core discovery, socket resolution, and stale detection (§3.1).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The discovery file written at `.git/stacksaw/daemon.json` (§3.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonInfo {
    pub pid: u32,
    /// e.g. `unix:/run/user/1000/stacksaw/<hash>.sock`.
    pub endpoint: String,
    pub protocol_version: String,
    pub binary_version: String,
    pub started_at: String,
}

impl DaemonInfo {
    /// The socket path portion of a `unix:` endpoint.
    pub fn socket_path(&self) -> Option<PathBuf> {
        self.endpoint.strip_prefix("unix:").map(PathBuf::from)
    }
}

/// Path of the discovery file for a repo (keyed by common git dir).
pub fn daemon_file(git_common_dir: &Path) -> PathBuf {
    git_common_dir.join("stacksaw").join("daemon.json")
}

/// Path of the spawn lock (§3.1 spawn race resolution).
pub fn lock_file(git_common_dir: &Path) -> PathBuf {
    git_common_dir.join("stacksaw").join("daemon.lock")
}

/// A short, stable hash of the repo's common git dir, for socket naming.
pub fn repo_hash(git_common_dir: &Path) -> String {
    let canonical = std::fs::canonicalize(git_common_dir)
        .unwrap_or_else(|_| git_common_dir.to_path_buf());
    blake3::hash(canonical.to_string_lossy().as_bytes()).to_hex()[..16].to_string()
}

/// Resolve the runtime dir under which sockets live: `$XDG_RUNTIME_DIR/stacksaw`
/// then `$TMPDIR/stacksaw` (§3.1). The directory is created 0700 on unix.
pub fn runtime_dir() -> std::io::Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("stacksaw");
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}

/// The socket path for a repo.
pub fn socket_path(git_common_dir: &Path) -> std::io::Result<PathBuf> {
    Ok(runtime_dir()?.join(format!("{}.sock", repo_hash(git_common_dir))))
}

/// The `unix:` endpoint string for a socket path.
pub fn endpoint_for(socket: &Path) -> String {
    format!("unix:{}", socket.display())
}

/// Read and parse the discovery file, if present.
pub fn read(git_common_dir: &Path) -> Option<DaemonInfo> {
    let bytes = std::fs::read(daemon_file(git_common_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Write the discovery file (parent dirs created).
pub fn write(git_common_dir: &Path, info: &DaemonInfo) -> std::io::Result<()> {
    let path = daemon_file(git_common_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(info)?;
    std::fs::write(&path, bytes)?;
    Ok(())
}

/// Remove a stale discovery file.
pub fn remove(git_common_dir: &Path) {
    let _ = std::fs::remove_file(daemon_file(git_common_dir));
}

/// Best-effort liveness check for a pid (§3.1). On unix uses `kill(pid, 0)`
/// semantics via signal 0; elsewhere assumes alive (the handshake is the real
/// gate).
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Signal 0 performs error checking without sending a signal.
        let rc = unsafe { libc_kill(pid as i32, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(1 /* EPERM */)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_hash_is_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let a = repo_hash(tmp.path());
        let b = repo_hash(tmp.path());
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn endpoint_roundtrips() {
        let p = PathBuf::from("/run/user/1000/stacksaw/abc.sock");
        let ep = endpoint_for(&p);
        let info = DaemonInfo {
            pid: 1,
            endpoint: ep,
            protocol_version: "1.0".into(),
            binary_version: "0.1.0".into(),
            started_at: "now".into(),
        };
        assert_eq!(info.socket_path().unwrap(), p);
    }

    #[test]
    fn current_process_is_alive() {
        assert!(pid_alive(std::process::id()));
    }
}
