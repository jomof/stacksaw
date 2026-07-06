#!/usr/bin/env bash
# A repo sitting at a detached HEAD with a dirty working tree: HEAD points at a
# commit rather than a branch, and there is uncommitted work on top. Exercises
# stacksaw's detached-HEAD label alongside the "uncommitted" marker.
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/../lib.sh"

init_repo "detached"
mkdir -p "$REPO/src"

# A little committed history on main so the file/diff panes have content.
commit "src/lib.rs" "Add lib: initial module"
commit "src/util.rs" "Add util: helpers"

# Detach HEAD (checked out at the tip commit, not on a branch).
gitr checkout -q --detach

# Leave a mix of uncommitted changes on the detached HEAD:
#   - a modified tracked file (unstaged)
printf '// src/lib.rs\nAdd lib: initial module\n\n// work in progress\n' \
  > "$REPO/src/lib.rs"
#   - a staged-but-uncommitted new file
printf '// src/feature.rs\nWIP: new feature\n' > "$REPO/src/feature.rs"
gitr add src/feature.rs
#   - an untracked file
printf 'scratch notes\n' > "$REPO/NOTES.md"

echo "built detached at $REPO"
