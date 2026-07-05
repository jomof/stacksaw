//! Recent-repository labeling for the Stacks-column MRU (§8.1 multi-repo).
//!
//! When you hop between the `.git` roots of a multi-monorepo workspace, the raw
//! paths are long and mostly redundant. This module turns a most-recently-used
//! list of repo workdirs into short, grouped, width-bounded labels:
//!
//! * repos are grouped by their **monorepo root** (the nearest ancestor bearing
//!   a marker like `.repo/` or `MODULE.bazel`), so each label only carries the
//!   part that distinguishes it *within* that monorepo;
//! * repos with no detected root fall into a loose group, labeled relative to
//!   `$HOME`;
//! * every label is middle-elided to fit the column so a deep path can never
//!   widen the Stacks column.
//!
//! Detection ([`detect_monorepo_root`]) is the only part that touches the
//! filesystem; the grouping/abbreviation ([`group_recents`]) is pure so it can
//! be unit-tested against hand-written paths.

use std::path::{Path, PathBuf};

/// Default monorepo-root markers, most-specific first. A directory is treated
/// as a monorepo root if it contains any of these. Deliberately small and
/// general — real setups extend this via config rather than us hardcoding one
/// vendor's convention.
pub const DEFAULT_MARKERS: &[&str] = &[
    ".repo",              // Google `repo` tool
    "MODULE.bazel",       // Bazel (bzlmod)
    "WORKSPACE.bazel",    // Bazel
    "WORKSPACE",          // Bazel (legacy)
    "pnpm-workspace.yaml", // pnpm workspaces
    "go.work",            // Go workspaces
];

/// The ellipsis used when a label is elided.
const ELLIPSIS: &str = "…";

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

/// A repository row in the recents view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentEntry {
    /// The repo workdir this row points at (unabbreviated, for switching).
    pub path: PathBuf,
    /// The abbreviated, width-fitted label to display.
    pub label: String,
    /// Whether this is the repo the window is currently attached to.
    pub current: bool,
}

/// A group of recents sharing a monorepo root (or the loose group).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentGroup {
    /// The monorepo root's display label, or `None` for the loose group (repos
    /// with no detected root).
    pub header: Option<String>,
    pub entries: Vec<RecentEntry>,
}

/// Group and abbreviate a most-recently-used list of repositories.
///
/// * `current` — the repo the window is currently on (marked, not filtered).
/// * `recents` — `(repo_workdir, detected_monorepo_root)` pairs, already in
///   MRU order (most-recent first). Detection is the caller's job so this stays
///   pure; use [`detect_monorepo_root`] to fill the second field.
/// * `home` — `$HOME`, used to contract loose-repo and header paths.
/// * `budget` — max label width in characters; longer labels are middle-elided.
///
/// Groups appear in first-seen order, so the current repo's group (normally at
/// the top of an MRU list) leads.
pub fn group_recents(
    current: &Path,
    recents: &[(PathBuf, Option<PathBuf>)],
    home: Option<&Path>,
    budget: usize,
) -> Vec<RecentGroup> {
    // Preserve first-seen order of roots; `None` collates into the loose group.
    let mut order: Vec<Option<PathBuf>> = Vec::new();
    for (_, root) in recents {
        if !order.iter().any(|r| r == root) {
            order.push(root.clone());
        }
    }

    order
        .into_iter()
        .map(|root| {
            let header = root
                .as_deref()
                .map(|r| elide(&home_contract(r, home), budget));
            let entries = recents
                .iter()
                .filter(|(_, r)| *r == root)
                .map(|(path, r)| {
                    let full = match r {
                        Some(root) => relative_label(path, root),
                        None => home_contract(path, home),
                    };
                    RecentEntry {
                        path: path.clone(),
                        label: elide(&full, budget),
                        current: path == current,
                    }
                })
                .collect();
            RecentGroup { header, entries }
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

/// Contract a leading `$HOME` to `~`, otherwise return the absolute path.
fn home_contract(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home {
        if let Ok(rest) = path.strip_prefix(home) {
            if rest.as_os_str().is_empty() {
                return "~".into();
            }
            return format!("~/{}", rest.to_string_lossy());
        }
    }
    path.to_string_lossy().into_owned()
}

/// Middle-elide `label` to at most `budget` characters. Prefers a
/// segment-aware `first/…/last` form for paths; falls back to a character-level
/// middle elision when even that is too wide.
fn elide(label: &str, budget: usize) -> String {
    if label.chars().count() <= budget {
        return label.to_string();
    }
    // Try to keep the first and last path segments (context + identity).
    let segs: Vec<&str> = label.split('/').collect();
    if segs.len() >= 3 {
        let candidate = format!("{}/{ELLIPSIS}/{}", segs[0], segs[segs.len() - 1]);
        if candidate.chars().count() <= budget {
            return candidate;
        }
    }
    char_elide(label, budget)
}

/// Character-level middle elision: keep a prefix and suffix around an ellipsis.
fn char_elide(label: &str, budget: usize) -> String {
    let chars: Vec<char> = label.chars().collect();
    if budget == 0 {
        return String::new();
    }
    if budget == 1 || chars.len() <= 1 {
        return ELLIPSIS.to_string();
    }
    let keep = budget - 1; // room for the ellipsis
    let front = keep.div_ceil(2);
    let back = keep - front;
    let head: String = chars[..front].iter().collect();
    let tail: String = chars[chars.len() - back..].iter().collect();
    format!("{head}{ELLIPSIS}{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn single_monorepo_groups_and_labels_relatively() {
        let current = p("/w/bazel-mono/services/payments");
        let recents = [
            (p("/w/bazel-mono/services/payments"), Some(p("/w/bazel-mono"))),
            (p("/w/bazel-mono/services/auth"), Some(p("/w/bazel-mono"))),
            (p("/w/bazel-mono/libs/proto"), Some(p("/w/bazel-mono"))),
        ];
        let groups = group_recents(&current, &recents, None, 40);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].header.as_deref(), Some("/w/bazel-mono"));
        let labels: Vec<_> = groups[0].entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["services/payments", "services/auth", "libs/proto"]);
        assert!(groups[0].entries[0].current);
        assert!(!groups[0].entries[1].current);
    }

    #[test]
    fn multiple_monorepos_and_loose_repos_form_separate_groups() {
        let home = p("/home/me");
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
            (p("/home/me/dotfiles"), None),
        ];
        let groups = group_recents(&current, &recents, Some(&home), 40);

        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].header.as_deref(), Some("~/w/bazel-mono"));
        assert_eq!(groups[1].header.as_deref(), Some("~/w/js-mono"));
        // Loose group: no header, path contracted against $HOME.
        assert_eq!(groups[2].header, None);
        assert_eq!(groups[2].entries[0].label, "~/dotfiles");
    }

    #[test]
    fn nested_monorepo_uses_the_nearest_root() {
        // A monorepo (inner) nested inside another (outer): the innermost root
        // groups the repo, so `thing` is not smeared under `outer`.
        let current = p("/w/outer/shared/util");
        let recents = [
            (
                p("/w/outer/team/inner/projects/thing"),
                Some(p("/w/outer/team/inner")),
            ),
            (p("/w/outer/shared/util"), Some(p("/w/outer"))),
        ];
        let groups = group_recents(&current, &recents, None, 40);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].header.as_deref(), Some("/w/outer/team/inner"));
        assert_eq!(groups[0].entries[0].label, "projects/thing");
        assert_eq!(groups[1].header.as_deref(), Some("/w/outer"));
        assert_eq!(groups[1].entries[0].label, "shared/util");
    }

    #[test]
    fn deep_labels_are_middle_elided_to_fit() {
        let current = p("/w/mono/a");
        let recents = [(
            p("/w/mono/services/very/deep/nested/payments"),
            Some(p("/w/mono")),
        )];
        // "services/very/deep/nested/payments" is 34 chars; budget 20 forces
        // the segment-aware first/…/last form.
        let groups = group_recents(&current, &recents, None, 20);
        assert_eq!(groups[0].entries[0].label, "services/…/payments");
    }

    #[test]
    fn char_elision_kicks_in_when_segments_alone_dont_fit() {
        // A single long segment (no interior '/') must fall back to char elide.
        let out = elide("supercalifragilisticexpialidocious", 11);
        assert_eq!(out.chars().count(), 11);
        assert!(out.contains(ELLIPSIS));
    }

    #[test]
    fn detect_finds_nearest_ancestor_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path().join("outer");
        let inner = outer.join("team/inner");
        let repo = inner.join("projects/thing");
        std::fs::create_dir_all(&repo).unwrap();
        // Outer is a Bazel root; inner is a `repo` root.
        std::fs::write(outer.join("WORKSPACE.bazel"), "").unwrap();
        std::fs::create_dir_all(inner.join(".repo")).unwrap();

        let root = detect_monorepo_root(&repo, DEFAULT_MARKERS).unwrap();
        assert_eq!(root, inner, "nearest (innermost) marker wins");

        // A repo only under the outer root resolves to outer.
        let shared = outer.join("shared/util");
        std::fs::create_dir_all(&shared).unwrap();
        assert_eq!(
            detect_monorepo_root(&shared, DEFAULT_MARKERS).unwrap(),
            outer
        );
    }

    #[test]
    fn no_marker_yields_no_root() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("plain/repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert_eq!(detect_monorepo_root(&repo, DEFAULT_MARKERS), None);
    }
}
