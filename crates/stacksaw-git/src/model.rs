//! The staircase / segment-tree model (§2). Builds [`Staircase`] DTOs from the
//! repository's local branches and their upstreams.

use std::collections::{BTreeMap, HashMap};

use stacksaw_ssp::types::{
    CommitSummary, FindingCounts, RebaseStatus, Segment, Staircase, WORKTREE_OID,
};

use crate::error::Result;
use crate::repo::{BranchRef, Repo};
use stacksaw_ssp::git_ref::GitRef;

/// Options controlling staircase construction.
#[derive(Debug, Clone, Default)]
pub struct ModelOptions {
    /// Fallback upstream when a branch has no tracking configuration.
    pub default_upstream: Option<String>,
}

/// Build all staircases in the repository, grouped by shared upstream and
/// shaped into segment trees (§2). A group of ancestry-linked branches only
/// forms a staircase when its branches share a non-empty common name prefix;
/// otherwise it is "a bunch of branches" and each surfaces on its own.
pub fn build_staircases(repo: &Repo, opts: &ModelOptions) -> Result<Vec<Staircase>> {
    let branches = repo.local_branches()?;

    // Resolve an upstream ref name + oid for each branch. The configured
    // tracking upstream wins; the model default is a fallback.
    let mut groups: BTreeMap<String, Vec<ResolvedMember>> = BTreeMap::new();
    for b in branches.iter() {
        let mut candidates: Vec<String> = b
            .upstream
            .as_ref()
            .map(|r| r.full().to_string())
            .into_iter()
            .collect();
        if let Some(ref default) = opts.default_upstream {
            candidates.push(default.clone());
            // Fallback from remote tracking to local branch tracking
            if let Some(local_name) = GitRef::new(default).tracking_local_name() {
                candidates.push(format!("refs/heads/{local_name}"));
            }
        }
        candidates.push("refs/heads/main".to_string());
        candidates.push("refs/heads/master".to_string());

        let resolved = candidates.into_iter().find_map(|name| {
            if name == b.full_name {
                return None; // the branch *is* the upstream (e.g. self-tracking)
            }
            repo.resolve(&name).ok().map(|oid| (name, oid))
        });
        let Some((upstream_name, upstream_oid)) = resolved else {
            continue; // no upstream resolves (e.g. `origin/main` gone/unfetched)
        };
        if repo.is_ancestor(b.tip, upstream_oid)? {
            continue; // skip branches already merged to upstream
        }
        groups
            .entry(upstream_name)
            .or_default()
            .push(ResolvedMember {
                branch: b.clone(),
                upstream_oid,
            });
    }

    let mut staircases = Vec::new();
    for (upstream_name, members) in groups {
        staircases.extend(build_group(repo, &upstream_name, members)?);
    }

    // Always surface the checked-out state as a staircase, even when no upstream
    // resolves: a lone branch (e.g. `main`) or a *detached HEAD* shows its
    // reachable commits as a stack rather than leaving Stacks empty (§2, §8). The
    // stack is keyed by `head_ref` — the branch name, or the short HEAD oid when
    // detached — so the same key drives the dirty/worktree injection downstream.
    let head_ref = repo.head_ref_label().ok().flatten();
    if let Some(head) = &head_ref {
        if !branch_is_shown(&staircases, head) {
            let synthetic = match branches.iter().find(|b| &b.name == head) {
                // On a branch with no resolvable upstream: root at its history.
                Some(b) => {
                    let label = b
                        .upstream
                        .clone()
                        .or_else(|| {
                            opts.default_upstream
                                .as_ref()
                                .map(|s| GitRef::new(s.clone()))
                        })
                        .map(|u| short_upstream(&u))
                        .unwrap_or_else(|| "(root)".to_string());
                    Some(build_rootless_staircase(repo, b.tip, head, &label)?)
                }
                // Detached HEAD: no branch to key on. Root the stack *at* HEAD
                // (its own upstream) so it lists no commits — walking all
                // reachable history would flood Commits with the whole log.
                // Only uncommitted work (injected downstream) surfaces here.
                None => Some(detached_staircase(head)),
            };
            if let Some(s) = synthetic {
                staircases.push(s);
            }
        }
    }

    // Detect twins across all staircases by Change-Id trailer (§2).
    annotate_twins(repo, &mut staircases)?;

    // Open on the checked-out state: move the staircase representing HEAD to
    // front (matched by the same `head_ref` key used to build it).
    if let Some(head) = &head_ref {
        if let Some(pos) = staircases
            .iter()
            .position(|s| s.segments.iter().any(|seg| seg.branch.short() == head))
        {
            staircases.swap(0, pos);
        }
    }

    Ok(staircases)
}

/// True when some staircase already contains a segment for `branch`.
fn branch_is_shown(staircases: &[Staircase], branch: &str) -> bool {
    staircases
        .iter()
        .any(|s| s.segments.iter().any(|seg| seg.branch.short() == branch))
}

/// Strip ref prefixes so an upstream reads as `origin/main` / `main`.
fn short_upstream(name: &str) -> String {
    GitRef::new(name).short().to_string()
}
/// a short oid for a detached HEAD).
fn build_rootless_staircase(
    repo: &Repo,
    tip: gix::ObjectId,
    name: &str,
    upstream_label: &str,
) -> Result<Staircase> {
    let oids = repo.commits_reachable(tip, Some(100))?;
    let mut commits = Vec::with_capacity(oids.len());
    for oid in oids {
        commits.push(commit_summary(repo, oid)?);
    }
    let ahead = commits.len() as u32;
    Ok(Staircase {
        name: name.to_string(),
        upstream: GitRef::new(upstream_label),
        ahead,
        behind: 0,
        dirty: false,
        rebase: RebaseStatus::default(),
        conflict: None,
        segments: vec![Segment {
            branch: GitRef::new(name),
            parent: None,
            stale: false,
            commits,
        }],
    })
}

/// A zero-commit staircase for a detached HEAD, keyed by its short oid. HEAD is
/// its own upstream (nothing ahead), so the Commits column stays empty until the
/// snapshot builder appends the virtual worktree commit for uncommitted work.
fn detached_staircase(name: &str) -> Staircase {
    Staircase {
        name: name.to_string(),
        upstream: GitRef::new(name),
        ahead: 0,
        behind: 0,
        dirty: false,
        rebase: RebaseStatus::default(),
        conflict: None,
        segments: vec![Segment {
            branch: GitRef::new(name),
            parent: None,
            stale: false,
            commits: Vec::new(),
        }],
    }
}

/// Summarize one commit into the DTO carried in snapshots.
fn commit_summary(repo: &Repo, oid: gix::ObjectId) -> Result<CommitSummary> {
    let meta = repo.commit_meta(oid)?;
    Ok(CommitSummary {
        oid: meta.oid.to_string(),
        short: meta.short(),
        subject: meta.subject.clone(),
        author: meta.author_name.clone(),
        author_time: meta.author_time,
        parents: meta.parents.iter().map(|p| p.to_string()).collect(),
        change_id: meta.change_id.clone(),
        patch_id: meta.patch_id.clone(),
        finding_counts: FindingCounts::default(),
        twins: vec![],
        // Line stats are filled in later by the snapshot builder in one batched
        // git call (see `snapshot::annotate_commit_stats`).
        added: 0,
        deleted: 0,
    })
}

struct ResolvedMember {
    branch: BranchRef,
    upstream_oid: gix::ObjectId,
}

fn build_group(
    repo: &Repo,
    upstream_name: &str,
    members: Vec<ResolvedMember>,
) -> Result<Vec<Staircase>> {
    let mut parents = find_parents(repo, &members)?;
    let recovered_bases = recover_stale_links(repo, &members, &mut parents)?;
    let components = compute_components(&parents);
    group_into_staircases(
        repo,
        upstream_name,
        &members,
        &parents,
        &recovered_bases,
        components,
    )
}

/// 1. Parent Identification: parents[i] = index of the nearest ancestor branch in this group, or None.
fn find_parents(repo: &Repo, members: &[ResolvedMember]) -> Result<Vec<Option<usize>>> {
    let n = members.len();
    let mut parents = vec![None; n];
    for i in 0..n {
        let tip_i = members[i].branch.tip;
        let mut best: Option<usize> = None;
        for j in 0..n {
            if i == j {
                continue;
            }
            let tip_j = members[j].branch.tip;
            // j is a candidate ancestor of i if tip_j is an ancestor of tip_i.
            if tip_j != tip_i && repo.is_ancestor(tip_j, tip_i)? {
                // Prefer the *nearest* ancestor: the one that is a descendant of
                // all other candidates.
                match best {
                    None => best = Some(j),
                    Some(cur) => {
                        if repo.is_ancestor(members[cur].branch.tip, tip_j)? {
                            best = Some(j);
                        }
                    }
                }
            }
        }
        parents[i] = best;
    }
    Ok(parents)
}

/// 2. Stale Link Recovery (§4 restack detection).
///    For each orphan root, if a same-family sibling has a *former* tip (from its
///    reflog) that is an ancestor of this root, that sibling is the intended parent
///    and the link is stale (needs a restack).
fn recover_stale_links(
    repo: &Repo,
    members: &[ResolvedMember],
    parents: &mut [Option<usize>],
) -> Result<Vec<Option<gix::ObjectId>>> {
    let n = members.len();
    let mut recovered_bases = vec![None; n];
    for i in 0..n {
        if parents[i].is_some() {
            continue;
        }
        let tip_i = members[i].branch.tip;
        let name_i = members[i].branch.name.clone();
        let mut best: Option<(usize, gix::ObjectId)> = None;
        for (j, member_j) in members.iter().enumerate().take(n) {
            if i == j || common_prefix([name_i.as_str(), member_j.branch.name.as_str()]).is_none() {
                continue;
            }
            let tip_j = member_j.branch.tip;
            // Only a genuine ancestry break (neither tip reaches the other) is a
            // restack candidate; a normal parent/child link is handled above.
            if repo.is_ancestor(tip_i, tip_j)? || repo.is_ancestor(tip_j, tip_i)? {
                continue;
            }
            for h in repo.reflog_oids(&member_j.branch.name) {
                // `h` must be a *former* tip `j` has since abandoned (a rewrite /
                // amend) that `i` still descends from. If `j` still descends from
                // `h`, then `h` is just an old point on `j`s own history (its
                // branch-creation base or a fast-forward) — not an amend, so `i`
                // resting on it is coincidence, not a stale link.
                if h == tip_i || repo.is_ancestor(h, tip_j)? || !repo.is_ancestor(h, tip_i)? {
                    continue;
                }
                // Prefer the nearest former tip (a descendant of the prior best).
                best = match best {
                    Some((_, cur)) if !repo.is_ancestor(cur, h)? => best,
                    _ => Some((j, h)),
                };
            }
        }
        if let Some((j, h)) = best {
            // Never introduce a cycle into the parent graph.
            let mut p = Some(j);
            let mut cyclic = false;
            while let Some(x) = p {
                if x == i {
                    cyclic = true;
                    break;
                }
                p = parents[x];
            }
            if !cyclic {
                parents[i] = Some(j);
                recovered_bases[i] = Some(h);
            }
        }
    }
    Ok(recovered_bases)
}

/// 3. Connected Components over the parent relation → one staircase each.
fn compute_components(parents: &[Option<usize>]) -> Vec<Vec<usize>> {
    let n = parents.len();
    let mut comp_of = vec![None; n];
    let mut components: Vec<Vec<usize>> = Vec::new();
    for i in 0..n {
        if comp_of[i].is_some() {
            continue;
        }
        // Walk to root.
        let mut root = i;
        while let Some(p) = parents[root] {
            root = p;
        }
        // Assign component id keyed by root.
        let cid = components
            .iter()
            .position(|c| c.first() == Some(&root))
            .unwrap_or_else(|| {
                components.push(vec![root]);
                components.len() - 1
            });
        if comp_of[root].is_none() {
            comp_of[root] = Some(cid);
        }
        comp_of[i] = Some(cid);
        if i != root && !components[cid].contains(&i) {
            components[cid].push(i);
        }
    }
    components
}

/// 4. Validating and naming staircases via common prefixes.
fn group_into_staircases(
    repo: &Repo,
    upstream_name: &str,
    members: &[ResolvedMember],
    parents: &[Option<usize>],
    recovered_bases: &[Option<gix::ObjectId>],
    components: Vec<Vec<usize>>,
) -> Result<Vec<Staircase>> {
    let mut out = Vec::new();
    for comp in components {
        // A staircase's constituent branches must share a non-empty common name
        // prefix (§2 constraint); without one it is not a staircase but merely a
        // bunch of ancestry-linked branches, so each is surfaced on its own.
        let is_staircase = comp.len() < 2
            || common_prefix(comp.iter().map(|&m| members[m].branch.name.as_str())).is_some();
        if is_staircase {
            out.push(build_staircase(
                repo,
                upstream_name,
                members,
                parents,
                recovered_bases,
                &comp,
            )?);
        } else {
            for m in comp {
                out.push(build_staircase(
                    repo,
                    upstream_name,
                    members,
                    parents,
                    recovered_bases,
                    &[m],
                )?);
            }
        }
    }
    Ok(out)
}

/// The longest common prefix shared by `names`, trimmed of trailing separator
/// punctuation (`/`, `-`, `_`, `.`, space) so `step-1`/`step-2`/`step-3` reads
/// as `step` and `feat/a`/`feat/b` as `feat`. Returns `None` when the names
/// share no non-separator prefix — the mark of "a bunch of branches" rather
/// than a staircase (§2). Compared by `char`, never bytes, so it is safe on
/// multi-byte names.
fn common_prefix<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut iter = names.into_iter();
    let first: Vec<char> = iter.next()?.chars().collect();
    let mut len = first.len();
    for name in iter {
        let shared = first
            .iter()
            .zip(name.chars())
            .take_while(|(a, b)| **a == *b)
            .count();
        len = len.min(shared);
        if len == 0 {
            return None;
        }
    }
    let prefix: String = first[..len].iter().collect();
    let trimmed = prefix.trim_end_matches(['/', '-', '_', '.', ' ']);
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn build_staircase(
    repo: &Repo,
    upstream_name: &str,
    members: &[ResolvedMember],
    parent: &[Option<usize>],
    recovered_base: &[Option<gix::ObjectId>],
    comp: &[usize],
) -> Result<Staircase> {
    // Order members topologically (root first, then by branch name).
    let mut ordered: Vec<usize> = comp.to_vec();
    ordered.sort_by(|&a, &b| {
        let da = depth(parent, a);
        let db = depth(parent, b);
        da.cmp(&db)
            .then(members[a].branch.name.cmp(&members[b].branch.name))
    });

    // Map member index → segment index in the emitted order.
    let mut seg_index: HashMap<usize, usize> = HashMap::new();
    for (idx, &m) in ordered.iter().enumerate() {
        seg_index.insert(m, idx);
    }

    let upstream_oid = members[ordered[0]].upstream_oid;
    let mut segments = Vec::new();
    let mut total_ahead = 0u32;

    for &m in &ordered {
        let tip = members[m].branch.tip;
        // A stale (recovered) link lists its commits against the parent's *former*
        // tip, since the parent's current tip is no longer an ancestor.
        let base = match recovered_base[m] {
            Some(h) => h,
            None => match parent[m] {
                Some(p) => members[p].branch.tip,
                None => repo.merge_base(tip, upstream_oid)?,
            },
        };
        let oids = repo.commits_between(base, tip)?;
        let mut commits = Vec::with_capacity(oids.len());
        for oid in oids {
            commits.push(commit_summary(repo, oid)?);
        }
        total_ahead += commits.len() as u32;
        segments.push(Segment {
            branch: members[m].branch.full_name.clone(),
            parent: parent[m].and_then(|p| seg_index.get(&p).copied()),
            stale: recovered_base[m].is_some(),
            commits,
        });
    }

    // behind = commits upstream has that the staircase root lacks.
    let root_tip = members[ordered[0]].branch.tip;
    let root_base = repo.merge_base(root_tip, upstream_oid)?;
    let behind = repo.commits_between(root_base, upstream_oid)?.len() as u32;

    // Name after the tip-most (deepest) segment's branch.
    let tip_most = *ordered
        .iter()
        .max_by_key(|&&m| depth(parent, m))
        .unwrap_or(&ordered[0]);

    // A multi-branch staircase is named by the common prefix its branches share
    // (§2) — the family name of the stack (`feat` for `feat/a`+`feat/b`) — while
    // a lone branch keeps its own name. `build_group` guarantees a common prefix
    // exists whenever it emits more than one branch here, so the fallback only
    // guards the direct/single-branch callers.
    let name = if ordered.len() > 1 {
        common_prefix(ordered.iter().map(|&m| members[m].branch.name.as_str()))
            .unwrap_or_else(|| members[tip_most].branch.name.clone())
    } else {
        members[tip_most].branch.name.clone()
    };

    Ok(Staircase {
        name,
        upstream: GitRef::new(upstream_name),
        ahead: total_ahead,
        behind,
        dirty: false,
        rebase: RebaseStatus::default(),
        conflict: None,
        segments,
    })
}

fn depth(parent: &[Option<usize>], mut i: usize) -> usize {
    let mut d = 0;
    while let Some(p) = parent[i] {
        i = p;
        d += 1;
    }
    d
}

/// Link commits that share a `Change-Id` or `patch-id` across segments/staircases (§2 twins).
fn annotate_twins(repo: &Repo, staircases: &mut [Staircase]) -> Result<()> {
    // 1. Collect oids that need patch-id (those without change-id).
    let mut needs_patch_id = Vec::new();
    for s in staircases.iter() {
        for seg in &s.segments {
            for c in &seg.commits {
                if c.change_id.is_none() && c.oid != WORKTREE_OID {
                    needs_patch_id.push(c.oid.clone());
                }
            }
        }
    }

    // 2. Batch compute patch-ids.
    let patch_ids = repo.patch_ids(&needs_patch_id)?;

    // 3. Fill in patch_ids and build the twin maps.
    let mut by_change: HashMap<String, Vec<String>> = HashMap::new();
    let mut by_patch: HashMap<String, Vec<String>> = HashMap::new();

    for s in staircases.iter_mut() {
        for seg in &mut s.segments {
            for c in &mut seg.commits {
                if let Some(cid) = &c.change_id {
                    by_change
                        .entry(cid.clone())
                        .or_default()
                        .push(c.oid.clone());
                } else if let Some(pid) = patch_ids.get(&c.oid) {
                    c.patch_id = Some(pid.clone());
                    by_patch.entry(pid.clone()).or_default().push(c.oid.clone());
                }
            }
        }
    }

    // 4. Annotate twins.
    for s in staircases.iter_mut() {
        for seg in &mut s.segments {
            for c in &mut seg.commits {
                let twins = if let Some(cid) = &c.change_id {
                    by_change.get(cid)
                } else if let Some(pid) = &c.patch_id {
                    by_patch.get(pid)
                } else {
                    None
                };
                if let Some(oids) = twins {
                    c.twins = oids.iter().filter(|o| **o != c.oid).cloned().collect();
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::iter;

    use super::common_prefix;

    #[test]
    fn common_prefix_names_a_staircase_by_its_family() {
        // Slash- and dash-delimited stacks read as their family name.
        assert_eq!(common_prefix(["feat/a", "feat/b"]).as_deref(), Some("feat"));
        assert_eq!(
            common_prefix(["step-1", "step-2", "step-3"]).as_deref(),
            Some("step")
        );
        // A mid-word shared run still counts.
        assert_eq!(
            common_prefix(["feature-x", "feature-y"]).as_deref(),
            Some("feature")
        );
    }

    #[test]
    fn common_prefix_rejects_a_bunch_of_branches() {
        // No shared prefix at all → not a staircase.
        assert_eq!(common_prefix(["alice", "bob"]), None);
        // A prefix made only of separators trims to nothing → not a staircase.
        assert_eq!(common_prefix(["/a", "/b"]), None);
        // A single name has no "common" prefix to speak of here (callers treat
        // one-branch groups as plain branches, not staircases).
        assert_eq!(common_prefix(iter::empty::<&str>()), None);
    }

    #[test]
    fn test_compute_components() {
        use super::compute_components;
        // 0 -> 1 -> 2
        // 3 -> 4
        // 5 (lone)
        let parents = vec![Some(1), Some(2), None, Some(4), None, None];
        let mut components = compute_components(&parents);
        for c in &mut components {
            c.sort();
        }
        components.sort();

        let mut expected = vec![vec![0, 1, 2], vec![3, 4], vec![5]];
        for c in &mut expected {
            c.sort();
        }
        expected.sort();

        assert_eq!(components, expected);
    }
}
