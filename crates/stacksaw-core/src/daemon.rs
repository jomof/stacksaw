//! Daemon lifecycle: spawn, serve, idle shutdown, status/verify (§3.1).

use std::fs;
use std::path::Path;
use std::process;
use std::time::{Duration, Instant};

use fs4::fs_std::FileExt;
use stacksaw_git::Repo;

use crate::config;
use crate::discovery::{self, DaemonInfo};
use crate::server::{self, ClientCounter};
use crate::service::Service;
use crate::watch;
use tokio::time;

/// Parse a duration like `10m`, `30s`, `2h` into a [`Duration`].
pub fn parse_duration(s: &str) -> Duration {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| c.is_alphabetic()).unwrap_or(s.len()));
    let n: u64 = num.parse().unwrap_or(0);
    match unit {
        "s" => Duration::from_secs(n),
        "m" => Duration::from_secs(n * 60),
        "h" => Duration::from_secs(n * 3600),
        "ms" => Duration::from_millis(n),
        _ => Duration::from_secs(n),
    }
}

/// Run the core service in the foreground until idle-shutdown or error (§3.1).
pub async fn run(repo_path: &Path) -> anyhow::Result<()> {
    let repo = Repo::discover(repo_path)?;
    let git_dir = repo.common_dir();
    let repo_root = repo.workdir().unwrap_or_else(|| repo_path.to_path_buf());
    drop(repo);

    // Settle spawn races with an exclusive lock (§3.1).
    fs::create_dir_all(git_dir.join("stacksaw"))?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(discovery::lock_file(&git_dir))?;
    lock.lock_exclusive()?;

    let (cfg, _prov) = config::load(&repo_root, &git_dir);
    let idle = parse_duration(&cfg.core.idle_shutdown);

    let socket = discovery::socket_path(&git_dir)?;
    let service = Service::new(repo_root.clone(), git_dir.clone(), cfg);
    let _watch_guard = watch::spawn(service.clone())?;

    // Publish discovery (§3.1).
    let info = DaemonInfo {
        pid: process::id(),
        endpoint: discovery::endpoint_for(&socket),
        protocol_version: stacksaw_ssp::PROTOCOL_VERSION.to_string(),
        binary_version: env!("CARGO_PKG_VERSION").to_string(),
        started_at: jiff::Timestamp::now().to_string(),
    };
    discovery::write(&git_dir, &info)?;

    let counter = ClientCounter::default();
    let serve_counter = counter.clone();
    let serve_service = service.clone();
    let serve_socket = socket.clone();
    let serve = tokio::spawn(async move {
        if let Err(e) = server::serve(serve_service, &serve_socket, serve_counter).await {
            tracing::error!("serve loop error: {e}");
        }
    });

    // Idle shutdown: exit after the last client leaves plus a grace period so
    // back-to-back CLI calls stay warm (§3.1).
    let mut ever_connected = false;
    let mut idle_since: Option<Instant> = None;
    let mut tick = time::interval(Duration::from_secs(1));
    loop {
        tick.tick().await;
        let n = counter.count();
        if n > 0 {
            ever_connected = true;
            idle_since = None;
        } else if ever_connected {
            match idle_since {
                None => idle_since = Some(Instant::now()),
                Some(t) if t.elapsed() >= idle => {
                    tracing::info!("idle for {idle:?}, shutting down");
                    break;
                }
                _ => {}
            }
        }
        if serve.is_finished() {
            break;
        }
    }

    serve.abort();
    discovery::remove(&git_dir);
    let _ = fs::remove_file(&socket);
    let _ = FileExt::unlock(&lock);
    Ok(())
}

/// Report status of the daemon for this repo (§3.1 `core status`).
pub fn status(repo_path: &Path) -> anyhow::Result<Option<DaemonInfo>> {
    let repo = Repo::discover(repo_path)?;
    let git_dir = repo.common_dir();
    let Some(info) = discovery::read(&git_dir) else {
        return Ok(None);
    };
    if discovery::pid_alive(info.pid) {
        Ok(Some(info))
    } else {
        discovery::remove(&git_dir);
        Ok(None)
    }
}

/// Stop the daemon for this repo (§3.1 `core stop`).
pub fn stop(repo_path: &Path) -> anyhow::Result<bool> {
    let repo = Repo::discover(repo_path)?;
    let git_dir = repo.common_dir();
    let Some(info) = discovery::read(&git_dir) else {
        return Ok(false);
    };
    #[cfg(unix)]
    {
        // SIGTERM (15) for a graceful stop.
        unsafe { libc_kill(info.pid as i32, 15) };
    }
    discovery::remove(&git_dir);
    if let Ok(socket) = discovery::socket_path(&git_dir) {
        let _ = fs::remove_file(socket);
    }
    Ok(true)
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
    fn parses_durations() {
        assert_eq!(parse_duration("30s"), Duration::from_secs(30));
        assert_eq!(parse_duration("10m"), Duration::from_secs(600));
        assert_eq!(parse_duration("2h"), Duration::from_secs(7200));
        assert_eq!(parse_duration("500ms"), Duration::from_millis(500));
    }
}
