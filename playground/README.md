# playground

Scratch git repositories for experimenting with `stacksaw`.

Each scenario under `scenarios/` is a small, **idempotent** script that builds a
`.git` root demonstrating a particular stack shape. Re-running a scenario wipes
its repo and recreates the exact same state, so the scenarios — not the
generated repos — are what live in source control. Generated repos land under
`repos/` and are git-ignored.

## Usage

```sh
./playground.sh list             # list scenarios
./playground.sh build <name>     # (re)build one scenario
./playground.sh build all        # (re)build every scenario
./playground.sh path <name>      # print a scenario's repo path

# Explore a built repo:
(cd "$(./playground.sh path single-stack)" && stacksaw)
```

## Scenarios

- **single-stack** — one `feature` branch with six commits on top of `main`;
  renders as a single staircase with one segment.
- **staircase** — the same six changes split into `step-1` → `step-2` →
  `step-3` (two commits each), all tracking `main`; renders as one staircase
  with three nested segments.

## Adding a scenario

Drop a new `scenarios/<name>.sh` that sources `lib.sh` and drives it with the
`init_repo` / `new_branch` / `track` / `commit` helpers. It is picked up
automatically by `list` and `build all`.
