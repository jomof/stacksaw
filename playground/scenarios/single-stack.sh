#!/usr/bin/env bash
# A single linear stack: one feature branch with several commits on top of main.
# stacksaw shows this as one staircase with a single segment.
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/../lib.sh"

init_repo "single-stack"
shape_single_stack

echo "built single-stack at $REPO"
