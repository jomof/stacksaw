#!/usr/bin/env bash
# The "nested monorepo" twist: an inner monorepo (a `repo` checkout) living
# inside an outer one (a Bazel workspace). This exercises nearest-ancestor
# anchoring — a repo is grouped by the *closest* enclosing root, not the
# outermost:
#
#   nested-mono/                 (WORKSPACE.bazel)      <- outer root
#     shared/util/.git                                  -> grouped under outer
#     team/inner/                (.repo/)               <- inner root
#       projects/thing/.git                             -> grouped under inner
#
# So `thing` labels as projects/thing under "team/inner", while `util` labels as
# shared/util under the outer root — the inner repo is never mislabeled against
# the far-up outer root.
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/../lib.sh"

ROOT="$REPOS_DIR/nested-mono"
mono_root "$ROOT" "WORKSPACE.bazel"
mono_root "$ROOT/team/inner" ".repo/"

init_repo_at "$ROOT/shared/util";           shape_single_stack
init_repo_at "$ROOT/team/inner/projects/thing"; shape_staircase

echo "built nested-mono at $ROOT"
echo "  inner repo groups under team/inner: (cd $ROOT/team/inner/projects/thing && stacksaw)"
echo "  outer repo groups under the root:   (cd $ROOT/shared/util && stacksaw)"
