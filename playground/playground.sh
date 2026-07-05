#!/usr/bin/env bash
# playground — generate scratch git repos to experiment with stacksaw.
#
# Scenarios live in playground/scenarios/*.sh and are idempotent: (re)building
# one wipes its repo and recreates it. Generated repos live under
# playground/repos/ and are git-ignored (the scenario scripts are the
# source-controlled, reproducible definition).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCENARIOS="$HERE/scenarios"

usage() {
  cat <<'EOF'
Usage:
  ./playground.sh list             List available scenarios.
  ./playground.sh build <name>     (Re)build one scenario (idempotent).
  ./playground.sh build all        (Re)build every scenario.
  ./playground.sh path <name>      Print the generated repo path.

Open a built repo with:
  (cd "$(./playground.sh path <name>)" && stacksaw)
EOF
}

scenario_names() {
  for f in "$SCENARIOS"/*.sh; do
    [ -e "$f" ] || continue
    basename "$f" .sh
  done
}

build_one() {
  local name="$1" f="$SCENARIOS/$1.sh"
  if [ ! -f "$f" ]; then
    echo "unknown scenario: $name (try './playground.sh list')" >&2
    exit 1
  fi
  bash "$f"
}

case "${1:-list}" in
  list)
    echo "scenarios:"
    scenario_names | sed 's/^/  /'
    ;;
  build)
    target="${2:-}"
    [ -n "$target" ] || { usage; exit 1; }
    if [ "$target" = all ]; then
      for n in $(scenario_names); do build_one "$n"; done
    else
      build_one "$target"
    fi
    ;;
  path)
    target="${2:-}"
    [ -n "$target" ] || { usage; exit 1; }
    echo "$HERE/repos/$target"
    ;;
  -h | --help | help)
    usage
    ;;
  *)
    usage
    exit 1
    ;;
esac
