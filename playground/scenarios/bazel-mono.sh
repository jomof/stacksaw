#!/usr/bin/env bash
# A Bazel + Google-`repo` rooted monorepo holding three independent .git repos.
# The root carries two markers (WORKSPACE.bazel and .repo/), so either signal
# anchors it. stacksaw's recents view groups these three under "bazel-mono" and
# labels them by their in-monorepo path: services/payments, services/auth,
# libs/proto.
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/../lib.sh"

ROOT="$REPOS_DIR/bazel-mono"
mono_root "$ROOT" "WORKSPACE.bazel" ".repo/"

init_repo_at "$ROOT/services/payments"; shape_staircase
init_repo_at "$ROOT/services/auth";     shape_single_stack
init_repo_at "$ROOT/libs/proto";        shape_single_stack

echo "built bazel-mono at $ROOT"
echo "  open a member with: (cd $ROOT/services/payments && stacksaw)"
