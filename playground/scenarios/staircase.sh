#!/usr/bin/env bash
# The same six changes as single-stack, but split into a three-step staircase:
# each step is its own branch stacked on the previous, all sharing upstream main.
# stacksaw shows this as one staircase with three nested segments.
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/../lib.sh"

init_repo "staircase"
shape_staircase

echo "built staircase at $REPO"
