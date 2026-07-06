# playground

Scratch git repositories for experimenting with `stacksaw`.

Each scenario under `scenarios/` is a small, **idempotent** script that builds a
`.git` root demonstrating a particular stack shape. Re-running a scenario wipes
its repo and recreates the exact same state, so the scenarios — not the
generated repos — are what live in source control. Generated repos land under
`repos/` and are git-ignored.

## Usage

```sh
./playground.sh list             # list scenarios
./playground.sh build <name>     # (re)build one scenario
./playground.sh build all        # (re)build every scenario
./playground.sh path <name>      # print a scenario's repo path

# Explore a built repo:
(cd "$(./playground.sh path single-stack)" && stacksaw)
```

## Scenarios

Single-repo shapes:

- **single-stack** — one `feature` branch with six commits on top of `main`;
  renders as a single staircase with one segment. The six commits add real
  Kotlin files (under `src/main/kotlin/demo/`), so the Diff column exercises
  syntax highlighting.
- **staircase** — the same six Kotlin changes split into `step-1` → `step-2` →
  `step-3` (two commits each), all tracking `main`; renders as one staircase
  with three nested segments.
- **dirty** — just `main` (no feature branches) with a dirty working tree: a
  modified tracked file, a staged-but-uncommitted new file, and an untracked
  file. Exercises the "uncommitted" marker and the no-staircase state.
- **detached** — a detached HEAD (checked out at a commit, not a branch) with
  the same dirty working tree. Exercises the detached-HEAD label together with
  the "uncommitted" marker.

Multi-`.git` monorepos (each builds a tree of several repos under one root):

- **bazel-mono** — a `WORKSPACE.bazel` + `.repo/` root over three repos
  (`services/payments`, `services/auth`, `libs/proto`). Exercises the recents
  view grouping repos under one monorepo and labeling them by in-repo path.
- **js-mono** — a `pnpm-workspace.yaml` root over `packages/web` +
  `packages/api`; a *second* monorepo so labels from two roots stay separate.
- **nested-mono** — a `repo` monorepo nested inside a Bazel monorepo, to
  exercise **nearest-ancestor** anchoring (the inner repo groups under the inner
  root, not the outer one).
- **studio-mono** — two `repo` + Bazel monorepos (`studio-main`,
  `studio-main.2`) each holding repos at the same paths (`tools/adt/idea`,
  `tools/vendor/google`), with one on a named branch (`bug-fix`) and the other
  pair at a detached HEAD. Exercises the recents **color** algorithm: shared
  branch names share a hue, everything else is hued by path — so the two
  same-path repos match across roots regardless of root name.

## On "nested monorepos"

Putting several monorepos side by side under `repos/` does **not** create a
super-monorepo: `repos/` carries no root marker, so each monorepo is anchored
independently and there is no common root to smear them under. Genuine nesting
only arises when one marker sits above another (the `nested-mono` scenario), and
that case is well-defined — a repo is always grouped by the *closest* enclosing
root. So it is safe to open members of different playground monorepos from one
stacksaw session; they simply show up as distinct groups.

## Adding a scenario

Drop a new `scenarios/<name>.sh` that sources `lib.sh`. Drive a single repo with
`init_repo` + `new_branch` / `track` / `commit` (or the `shape_single_stack` /
`shape_staircase` shortcuts, which commit real Kotlin), or build a monorepo tree
with `mono_root <root> <marker>…` followed by `init_repo_at <path>` for each
member. Use `commit` for one-line churn and `commit_stdin <file> <msg>` (body on
stdin, e.g. via a heredoc) for multi-line file bodies. New scenarios are picked
up automatically by `list` and `build all`.
