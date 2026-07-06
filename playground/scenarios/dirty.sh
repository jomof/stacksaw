#!/usr/bin/env bash
# A repo with a dirty working tree and no branches beyond main: no feature
# stacks at all, just uncommitted work sitting on main. Exercises stacksaw's
# "uncommitted" marker and the no-staircase state.
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/../lib.sh"

init_repo "dirty"
mkdir -p "$REPO/src"

# A little committed history on main so the file/diff panes have content.
commit "src/lib.rs" "Add lib: initial module"
commit "src/util.rs" "Add util: helpers"

# Leave a mix of uncommitted changes — and no new branches, only main:
#   - a modified tracked file (unstaged)
printf '// src/lib.rs\nAdd lib: initial module\n\n// work in progress\n' \
  > "$REPO/src/lib.rs"
#   - a staged-but-uncommitted new file
printf '// src/feature.rs\nWIP: new feature\n' > "$REPO/src/feature.rs"
gitr add src/feature.rs
#   - an untracked file
printf 'scratch notes\n' > "$REPO/NOTES.md"

echo "built dirty at $REPO"
