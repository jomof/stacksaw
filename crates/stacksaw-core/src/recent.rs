//! Recent-repository labeling for the Stacks-column MRU (§8.1 multi-repo).
//!
//! When you hop between the `.git` roots of a multi-monorepo workspace, the raw
//! paths are long and mostly redundant. This module turns a most-recently-used
//! list of repo workdirs into a flat, recency-ordered list of short labels:
//!
//! * each repo is one independent row — no nesting — carrying a `parent` (its
//!   **monorepo root** basename, e.g. `bazel-mono`) and a `label` (its path
//!   *within* that monorepo, e.g. `libs/proto`);
//! * the monorepo root is the nearest ancestor bearing a marker like `.repo/`
//!   or `MODULE.bazel`; repos with no detected root show just their basename
//!   and no parent;
//! * rows stay in MRU order (most-recent first) so the renderer can dim them by
//!   recency; label elision is left to the renderer, which knows the width.
//!
//! Detection ([`detect_monorepo_root`]) is the only part that touches the
//! filesystem; the labeling ([`flatten_recents`]) is pure so it can be
//! unit-tested against hand-written paths.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Most repositories retained in the MRU.
const MAX_RECENTS: usize = 12;

/// One remembered repository: its workdir and when it was last opened.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentRecord {
    pub path: PathBuf,
    #[serde(default)]
    pub last_opened_ms: u64,
}

/// The persisted most-recently-used list of repositories opened in the TUI,
/// ordered most-recent first. Stored at the user data dir's `recent.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecentStore {
    #[serde(default)]
    pub repos: Vec<RecentRecord>,
}

impl RecentStore {
    /// Where the MRU is persisted (`<data_dir>/recent.json`), if a user data
    /// directory can be resolved.
    pub fn store_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "stacksaw").map(|d| d.data_dir().join("recent.json"))
    }

    /// Load the MRU, returning an empty store if it is missing or unreadable.
    pub fn load() -> RecentStore {
        Self::store_path()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Promote `path` to most-recent: canonicalize it, move it to the front,
    /// drop entries whose directory no longer exists, and cap the list.
    pub fn record(&mut self, path: &Path) {
        let key = canonical(path);
        self.repos.retain(|r| canonical(&r.path) != key);
        self.repos.insert(
            0,
            RecentRecord {
                path: key,
                last_opened_ms: now_ms(),
            },
        );
        self.repos.retain(|r| r.path.is_dir());
        self.repos.truncate(MAX_RECENTS);
    }

    /// Persist the MRU, creating the data directory if needed.
    pub fn save(&self) -> io::Result<()> {
        let Some(path) = Self::store_path() else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self).unwrap_or_default();
        fs::write(path, json)
    }
}

fn canonical(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Default monorepo-root markers, most-specific first. A directory is treated
/// as a monorepo root if it contains any of these. Deliberately small and
/// general — real setups extend this via config rather than us hardcoding one
/// vendor's convention.
pub const DEFAULT_MARKERS: &[&str] = &[
    ".repo",               // Google `repo` tool
    "MODULE.bazel",        // Bazel (bzlmod)
    "WORKSPACE.bazel",     // Bazel
    "WORKSPACE",           // Bazel (legacy)
    "pnpm-workspace.yaml", // pnpm workspaces
    "go.work",             // Go workspaces
];

/// The repo's currently checked-out branch, read straight from `.git/HEAD`
/// (no git subprocess): `ref: refs/heads/<b>` yields `<b>`; a detached HEAD
/// yields its short (7-char) oid. Returns `None` if the repo can't be read.
///
/// Cheap enough to poll for every recents row on each refresh tick, which is
/// how the ledger stays in sync with checkouts made elsewhere (§6) — no
/// per-repo filesystem watcher required.
pub fn current_branch(workdir: &Path) -> Option<String> {
    let git_dir = resolve_git_dir(workdir)?;
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(reference) => {
            let reference = reference.trim();
            Some(
                reference
                    .strip_prefix("refs/heads/")
                    .unwrap_or(reference)
                    .to_string(),
            )
        }
        // Detached HEAD: the file holds a bare oid; show it abbreviated.
        None if !head.is_empty() => Some(head.chars().take(7).collect()),
        None => None,
    }
}

/// Resolve a workdir's git directory. Usually `<workdir>/.git`, but for
/// worktrees and submodules `.git` is a file reading `gitdir: <path>` that
/// points at the real directory (possibly relative to the workdir).
fn resolve_git_dir(workdir: &Path) -> Option<PathBuf> {
    let dot_git = workdir.join(".git");
    let meta = fs::metadata(&dot_git).ok()?;
    if meta.is_dir() {
        return Some(dot_git);
    }
    let contents = fs::read_to_string(&dot_git).ok()?;
    let target = contents.trim().strip_prefix("gitdir:")?.trim();
    let target = Path::new(target);
    Some(if target.is_absolute() {
        target.to_path_buf()
    } else {
        workdir.join(target)
    })
}

/// Walk up from `start` to the nearest ancestor directory containing any of
/// `markers`, returning that ancestor — the monorepo root — or `None`.
///
/// "Nearest" is load-bearing: for a monorepo nested inside another monorepo the
/// innermost root wins, which is what makes labels stay meaningful (a repo is
/// named relative to the closest thing that groups it, not some far-up root).
pub fn detect_monorepo_root(start: &Path, markers: &[&str]) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| markers.iter().any(|m| dir.join(m).exists()))
        .map(Path::to_path_buf)
}

/// One repository row in the recents ledger: an independent, single-line entry
/// (no nesting) positioned by MRU recency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentEntry {
    /// The repo workdir this row points at (unabbreviated, for switching).
    pub path: PathBuf,
    /// The monorepo root's basename (e.g. `bazel-mono`), shown as a dim prefix.
    /// `None` for a loose repo with no detected monorepo root.
    pub parent: Option<String>,
    /// The repo's label: its path within the monorepo (e.g. `libs/proto`), or
    /// its own basename for a loose repo.
    pub label: String,
    /// Whether this is the repo the window is currently attached to.
    pub current: bool,
}

/// Flatten a most-recently-used list of repositories into labeled rows.
///
/// * `current` — the repo the window is currently on (marked, not filtered).
/// * `recents` — `(repo_workdir, detected_monorepo_root)` pairs, already in
///   MRU order (most-recent first). Detection is the caller's job so this stays
///   pure; use [`detect_monorepo_root`] to fill the second field.
///
/// The result preserves MRU order one-to-one: each repo becomes its own row
/// with a `parent` (monorepo basename) and a root-relative `label`. There is no
/// grouping or nesting — the renderer places rows by recency and dims by age.
/// Labels are not elided here; the renderer trims them to the live width.
pub fn flatten_recents(current: &Path, recents: &[(PathBuf, Option<PathBuf>)]) -> Vec<RecentEntry> {
    recents
        .iter()
        .map(|(path, root)| {
            let parent = root.as_deref().map(root_basename);
            let label = match root {
                Some(root) => relative_label(path, root),
                None => root_basename(path),
            };
            RecentEntry {
                path: path.clone(),
                parent,
                label,
                current: path == current,
            }
        })
        .collect()
}

/// Label a repo as its path relative to its monorepo root (`/`-joined). Falls
/// back to the root's own basename when the repo *is* the root.
fn relative_label(path: &Path, root: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".".into()),
        Ok(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        // Not actually under the root (shouldn't happen); show the tail.
        Err(_) => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

/// The final path component of a path, used for the `parent` (monorepo root)
/// prefix and for loose-repo labels (both want a short, recognizable name
/// rather than a full path).
fn root_basename(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn monorepo_rows_carry_parent_and_relative_label() {
        let current = p("/w/bazel-mono/services/payments");
        let recents = [
            (
                p("/w/bazel-mono/services/payments"),
                Some(p("/w/bazel-mono")),
            ),
            (p("/w/bazel-mono/services/auth"), Some(p("/w/bazel-mono"))),
            (p("/w/bazel-mono/libs/proto"), Some(p("/w/bazel-mono"))),
        ];
        let rows = flatten_recents(&current, &recents);

        assert_eq!(rows.len(), 3);
        assert!(rows
            .iter()
            .all(|r| r.parent.as_deref() == Some("bazel-mono")));
        let labels: Vec<_> = rows.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["services/payments", "services/auth", "libs/proto"]);
        assert!(rows[0].current);
        assert!(!rows[1].current);
    }

    #[test]
    fn rows_preserve_mru_order_across_monorepos_and_loose_repos() {
        // Recency must not be reordered by grouping: an auth row from the same
        // monorepo as row 0 still sorts after a more-recent js-mono row.
        let current = p("/home/me/w/bazel-mono/services/payments");
        let recents = [
            (
                p("/home/me/w/bazel-mono/services/payments"),
                Some(p("/home/me/w/bazel-mono")),
            ),
            (
                p("/home/me/w/js-mono/packages/web"),
                Some(p("/home/me/w/js-mono")),
            ),
            (
                p("/home/me/w/bazel-mono/services/auth"),
                Some(p("/home/me/w/bazel-mono")),
            ),
            (p("/home/me/dotfiles"), None),
        ];
        let rows = flatten_recents(&current, &recents);

        let view: Vec<_> = rows
            .iter()
            .map(|r| (r.parent.as_deref(), r.label.as_str()))
            .collect();
        assert_eq!(
            view,
            [
                (Some("bazel-mono"), "services/payments"),
                (Some("js-mono"), "packages/web"),
                (Some("bazel-mono"), "services/auth"),
                // Loose repo: no parent, labeled by basename.
                (None, "dotfiles"),
            ]
        );
    }

    #[test]
    fn nested_monorepo_labels_relative_to_the_nearest_root() {
        // A monorepo (inner) nested inside another (outer): the innermost root
        // is the parent, so `thing` is labeled under `inner`, not `outer`.
        let current = p("/w/outer/shared/util");
        let recents = [
            (
                p("/w/outer/team/inner/projects/thing"),
                Some(p("/w/outer/team/inner")),
            ),
            (p("/w/outer/shared/util"), Some(p("/w/outer"))),
        ];
        let rows = flatten_recents(&current, &recents);

        assert_eq!(rows[0].parent.as_deref(), Some("inner"));
        assert_eq!(rows[0].label, "projects/thing");
        assert_eq!(rows[1].parent.as_deref(), Some("outer"));
        assert_eq!(rows[1].label, "shared/util");
    }

    #[test]
    fn detect_finds_nearest_ancestor_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path().join("outer");
        let inner = outer.join("team/inner");
        let repo = inner.join("projects/thing");
        fs::create_dir_all(&repo).unwrap();
        // Outer is a Bazel root; inner is a `repo` root.
        fs::write(outer.join("WORKSPACE.bazel"), "").unwrap();
        fs::create_dir_all(inner.join(".repo")).unwrap();

        let root = detect_monorepo_root(&repo, DEFAULT_MARKERS).unwrap();
        assert_eq!(root, inner, "nearest (innermost) marker wins");

        // A repo only under the outer root resolves to outer.
        let shared = outer.join("shared/util");
        fs::create_dir_all(&shared).unwrap();
        assert_eq!(
            detect_monorepo_root(&shared, DEFAULT_MARKERS).unwrap(),
            outer
        );
    }

    #[test]
    fn current_branch_reads_head_ref_and_detached_oid() {
        let tmp = tempfile::tempdir().unwrap();

        // Attached HEAD: `ref: refs/heads/<b>` resolves to the branch name.
        let attached = tmp.path().join("attached");
        fs::create_dir_all(attached.join(".git")).unwrap();
        fs::write(attached.join(".git/HEAD"), "ref: refs/heads/feat/proto\n").unwrap();
        assert_eq!(current_branch(&attached).as_deref(), Some("feat/proto"));

        // Detached HEAD: a bare oid shows abbreviated.
        let detached = tmp.path().join("detached");
        fs::create_dir_all(detached.join(".git")).unwrap();
        fs::write(
            detached.join(".git/HEAD"),
            "1234567890abcdef1234567890abcdef12345678\n",
        )
        .unwrap();
        assert_eq!(current_branch(&detached).as_deref(), Some("1234567"));

        // A `.git` *file* (worktree/submodule) is followed to the real gitdir.
        let real = tmp.path().join("real-gitdir");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let linked = tmp.path().join("linked");
        fs::create_dir_all(&linked).unwrap();
        fs::write(linked.join(".git"), format!("gitdir: {}\n", real.display())).unwrap();
        assert_eq!(current_branch(&linked).as_deref(), Some("main"));

        // A non-repo directory yields nothing.
        let bare = tmp.path().join("bare");
        fs::create_dir_all(&bare).unwrap();
        assert_eq!(current_branch(&bare), None);
    }

    #[test]
    fn no_marker_yields_no_root() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("plain/repo");
        fs::create_dir_all(&repo).unwrap();
        assert_eq!(detect_monorepo_root(&repo, DEFAULT_MARKERS), None);
    }

    #[test]
    fn record_moves_to_front_dedupes_and_drops_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();

        let mut store = RecentStore::default();
        store.record(&a);
        store.record(&b);
        store.record(&a); // revisiting a floats it back to the front

        assert_eq!(store.repos.len(), 2, "no duplicate entry for a");
        assert_eq!(store.repos[0].path, canonical(&a));
        assert_eq!(store.repos[1].path, canonical(&b));

        // A removed directory is pruned on the next record.
        fs::remove_dir_all(&b).unwrap();
        store.record(&a);
        assert_eq!(store.repos.len(), 1);
        assert_eq!(store.repos[0].path, canonical(&a));
    }
}
