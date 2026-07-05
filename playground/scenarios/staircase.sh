#!/usr/bin/env bash
# The same six changes as single-stack, but split into a three-step staircase:
# each step is its own branch stacked on the previous, all sharing upstream main.
# stacksaw shows this as one staircase with three nested segments.
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/../lib.sh"

init_repo "staircase"

new_branch "step-1" "main"
track "step-1" "main"
commit "f1.txt" "Add f1: scaffold module"
commit "f2.txt" "Add f2: core logic"

new_branch "step-2" "step-1"
track "step-2" "main"
commit "f3.txt" "Add f3: error handling"
commit "f4.txt" "Add f4: tests"

new_branch "step-3" "step-2"
track "step-3" "main"
commit "f5.txt" "Add f5: docs"
commit "f6.txt" "Add f6: polish"

echo "built staircase at $REPO"
