//! The staircase / segment-tree model (§2). Builds [`Staircase`] DTOs from the
//! repository's local branches and their upstreams.

use std::collections::{BTreeMap, HashMap};

use stacksaw_ssp::types::{CommitSummary, FindingCounts, Segment, Staircase};

use crate::error::Result;
use crate::repo::{BranchRef, Repo};

/// Options controlling staircase construction.
#[derive(Debug, Clone, Default)]
pub struct ModelOptions {
    /// Fallback upstream when a branch has no tracking configuration.
    pub default_upstream: Option<String>,
}

/// Build all staircases in the repository, grouped by shared upstream and
/// shaped into segment trees (§2).
pub fn build_staircases(repo: &Repo, opts: &ModelOptions) -> Result<Vec<Staircase>> {
    let branches = repo.local_branches()?;
    let head_branch = repo.head_branch().ok().flatten();

    // Resolve an upstream ref name + oid for each branch. The configured
    // tracking upstream wins; the model default is a fallback.
    let mut groups: BTreeMap<String, Vec<ResolvedMember>> = BTreeMap::new();
    for b in branches.iter() {
        let candidates = b
            .upstream
            .iter()
            .cloned()
            .chain(opts.default_upstream.clone());
        let resolved = candidates.into_iter().find_map(|name| {
            if name == b.full_name {
                return None; // the branch *is* the upstream (e.g. self-tracking)
            }
            repo.resolve(&name).ok().map(|oid| (name, oid))
        });
        let Some((upstream_name, upstream_oid)) = resolved else {
            continue; // no upstream resolves (e.g. `origin/main` gone/unfetched)
        };
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
        let group_staircases = build_group(repo, &upstream_name, members)?;
        staircases.extend(group_staircases);
    }

    // Always surface the current branch, even when no upstream resolves, so
    // running `stacksaw` on a lone branch (e.g. `main`) shows its commits as a
    // stack. The base falls back to the branch root (§2, §8).
    if let Some(head) = &head_branch {
        if !branch_is_shown(&staircases, head) {
            if let Some(b) = branches.iter().find(|b| &b.name == head) {
                let label = b
                    .upstream
                    .clone()
                    .or_else(|| opts.default_upstream.clone())
                    .map(|u| short_upstream(&u))
                    .unwrap_or_else(|| "(root)".to_string());
                staircases.push(build_rootless_staircase(repo, b, &label)?);
            }
        }
    }

    // Detect twins across all staircases by Change-Id trailer (§2).
    annotate_twins(&mut staircases);

    // Open on the current branch: move the staircase containing HEAD to front.
    if let Some(head) = &head_branch {
        if let Some(pos) = staircases
            .iter()
            .position(|s| s.segments.iter().any(|seg| &seg.branch == head))
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
        .any(|s| s.segments.iter().any(|seg| seg.branch == branch))
}

/// Strip ref prefixes so an upstream reads as `origin/main` / `main`.
fn short_upstream(name: &str) -> String {
    name.strip_prefix("refs/remotes/")
        .or_else(|| name.strip_prefix("refs/heads/"))
        .unwrap_or(name)
        .to_string()
}

/// Build a single-segment staircase for a branch with no resolvable upstream:
/// every commit reachable from its tip, treated as ahead of an empty upstream.
fn build_rootless_staircase(
    repo: &Repo,
    branch: &BranchRef,
    upstream_label: &str,
) -> Result<Staircase> {
    let oids = repo.commits_reachable(branch.tip)?;
    let mut commits = Vec::with_capacity(oids.len());
    for oid in oids {
        commits.push(commit_summary(repo, oid)?);
    }
    let ahead = commits.len() as u32;
    Ok(Staircase {
        name: branch.name.clone(),
        upstream: upstream_label.to_string(),
        ahead,
        behind: 0,
        dirty: false,
        segments: vec![Segment {
            branch: branch.name.clone(),
            parent: None,
            commits,
        }],
    })
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
    let n = members.len();

    // parent[i] = index of the nearest ancestor branch in this group, or None.
    let mut parent: Vec<Option<usize>> = vec![None; n];
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
        parent[i] = best;
    }

    // Connected components over the parent relation → one staircase each.
    let mut comp_of: Vec<Option<usize>> = vec![None; n];
    let mut components: Vec<Vec<usize>> = Vec::new();
    for i in 0..n {
        if comp_of[i].is_some() {
            continue;
        }
        // Walk to root.
        let mut root = i;
        while let Some(p) = parent[root] {
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

    let mut out = Vec::new();
    for comp in &components {
        out.push(build_staircase(repo, upstream_name, &members, &parent, comp)?);
    }
    Ok(out)
}

fn build_staircase(
    repo: &Repo,
    upstream_name: &str,
    members: &[ResolvedMember],
    parent: &[Option<usize>],
    comp: &[usize],
) -> Result<Staircase> {
    // Order members topologically (root first, then by branch name).
    let mut ordered: Vec<usize> = comp.to_vec();
    ordered.sort_by(|&a, &b| {
        let da = depth(parent, a);
        let db = depth(parent, b);
        da.cmp(&db).then(members[a].branch.name.cmp(&members[b].branch.name))
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
        let base = match parent[m] {
            Some(p) => members[p].branch.tip,
            None => repo.merge_base(tip, upstream_oid)?,
        };
        let oids = repo.commits_between(base, tip)?;
        let mut commits = Vec::with_capacity(oids.len());
        for oid in oids {
            commits.push(commit_summary(repo, oid)?);
        }
        total_ahead += commits.len() as u32;
        segments.push(Segment {
            branch: members[m].branch.name.clone(),
            parent: parent[m].and_then(|p| seg_index.get(&p).copied()),
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

    Ok(Staircase {
        name: members[tip_most].branch.name.clone(),
        upstream: upstream_name.to_string(),
        ahead: total_ahead,
        behind,
        dirty: false,
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

/// Link commits that share a `Change-Id` across segments/staircases (§2 twins).
fn annotate_twins(staircases: &mut [Staircase]) {
    let mut by_change: HashMap<String, Vec<String>> = HashMap::new();
    for s in staircases.iter() {
        for seg in &s.segments {
            for c in &seg.commits {
                if let Some(cid) = &c.change_id {
                    by_change.entry(cid.clone()).or_default().push(c.oid.clone());
                }
            }
        }
    }
    for s in staircases.iter_mut() {
        for seg in &mut s.segments {
            for c in &mut seg.commits {
                if let Some(cid) = &c.change_id {
                    if let Some(oids) = by_change.get(cid) {
                        c.twins = oids.iter().filter(|o| **o != c.oid).cloned().collect();
                    }
                }
            }
        }
    }
}
