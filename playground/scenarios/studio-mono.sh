#!/usr/bin/env bash
# Two Google-`repo` + Bazel monorepos (studio-main and studio-main.2), each
# holding two .git repos at the *same* in-monorepo paths: tools/adt/idea and
# tools/vendor/google. Built to exercise the recents color algorithm (§8.3),
# where a line's hue comes from its branch name when that branch is checked out
# in more than one repo, and otherwise from its path within the monorepo root
# (the root name itself never affects color):
#
#   * studio-main's two repos are both on the *same* named branch `bug-fix`, so
#     that name occurs in more than one repo and both lines are hued by BRANCH
#     ("bug-fix") — sharing one color.
#   * studio-main.2's two repos sit at a *detached* HEAD (a branch `bug-fix-2`
#     exists but isn't checked out), so they fall back to PATH hueing.
#
# The upshot to eyeball: studio-main's two `bug-fix` lines share one hue (shared
# branch), while studio-main.2's two lines are hued by their distinct paths.
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/../lib.sh"

# A member repo checked out on a fresh named branch with a couple of commits.
studio_branch_repo() {
  init_repo_at "$1"
  new_branch "$2" "main"
  commit "a.txt" "Work on $2: start"
  commit "b.txt" "Work on $2: more"
}

# A member repo with a `bug-fix-2` branch created but left at a detached HEAD,
# i.e. "checked out at HEAD" rather than on a named branch.
studio_detached_repo() {
  init_repo_at "$1"
  commit "a.txt" "Baseline change"
  gitr branch "bug-fix-2"
  gitr checkout -q --detach
}

ROOT1="$REPOS_DIR/studio-main"
mono_root "$ROOT1" "WORKSPACE.bazel" ".repo/"
studio_branch_repo "$ROOT1/tools/adt/idea"    "bug-fix"
studio_branch_repo "$ROOT1/tools/vendor/google" "bug-fix"

ROOT2="$REPOS_DIR/studio-main.2"
mono_root "$ROOT2" "WORKSPACE.bazel" ".repo/"
studio_detached_repo "$ROOT2/tools/adt/idea"
studio_detached_repo "$ROOT2/tools/vendor/google"

echo "built studio monorepos at $ROOT1 and $ROOT2"
echo "  open a member with: (cd $ROOT1/tools/adt/idea && stacksaw)"
