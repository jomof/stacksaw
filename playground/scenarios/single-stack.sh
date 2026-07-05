#!/usr/bin/env bash
# A single linear stack: one feature branch with several commits on top of main.
# stacksaw shows this as one staircase with a single segment.
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/../lib.sh"

init_repo "single-stack"
new_branch "feature" "main"
track "feature" "main"

commit "f1.txt" "Add f1: scaffold module"
commit "f2.txt" "Add f2: core logic"
commit "f3.txt" "Add f3: error handling"
commit "f4.txt" "Add f4: tests"
commit "f5.txt" "Add f5: docs"
commit "f6.txt" "Add f6: polish"

echo "built single-stack at $REPO"
