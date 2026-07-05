#!/usr/bin/env bash
# A second, differently-rooted monorepo (pnpm workspace) so the recents view has
# to keep two monorepos apart. Its members label as packages/web, packages/api
# under their own "js-mono" group — never smeared together with bazel-mono.
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/../lib.sh"

ROOT="$REPOS_DIR/js-mono"
mono_root "$ROOT" "pnpm-workspace.yaml"

init_repo_at "$ROOT/packages/web"; shape_staircase
init_repo_at "$ROOT/packages/api"; shape_single_stack

echo "built js-mono at $ROOT"
echo "  open a member with: (cd $ROOT/packages/web && stacksaw)"
