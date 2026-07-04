# stacksaw

**View, review, and reshape stacked and staircased git branches** before upload
to a code-review system (Gerrit-style workflows, host-agnostic). One binary,
three faces:

1. **A TUI** — a column-based interface built on the "Rainbox" visual language:
   hue communicates identity/relationship, dimming communicates relevance.
2. **A per-repo core service** — owns git state, filesystem watching, lint
   scheduling and agent sessions; UI windows and CLI calls attach to it over a
   JSON-RPC protocol (SSP).
3. **A scriptable CLI** — every capability exposed non-interactively with stable
   JSON output, designed to be driven by external agents.

stacksaw can also *drive* agents (rebase-and-fix workflows) via the Agent Client
Protocol (ACP), with a first-class adapter for Google Antigravity.

> This repository implements spec **v0.1**. See the design pillars P1–P6 in the
> spec; section references (§n) throughout the source point back to it.

## Status of the name (M0 collision sweep)

A sweep of crates.io, PyPI, npm, Homebrew, and GitHub topics found **no
collision** for `stacksaw` (nearest neighbours are the unrelated `staked`,
`stacks`, `stacks-cli`, and `csaw`). The name is considered clear. No short
alias ships by default; define your own.

## Workspace layout (§4)

```
crates/
  stacksaw/            bin: clap entry, role dispatch, CLI porcelain, TUI loop
  stacksaw-core/       daemon: snapshots, SSP server, watch, sessions, config
  stacksaw-git/        gix reads, staircase model, ref transactions, undo, edit sessions
  stacksaw-ssp/        protocol types + Content-Length codec (shared client/server)
  stacksaw-ui/         ratatui app (SSP client) + RenderSurface seam
  stacksaw-rainbox/    OKLCH color engine (pure, property-tested)
  stacksaw-lint/       finding model, scheduler, built-ins, external-command tier
  stacksaw-lint-kotlin/ ktfqn — the reference tree-sitter linter
  stacksaw-agents/     ACP client, workflow orchestration, restack state machine
adapters/
  antigravity/         Python package: stacksaw-antigravity-adapter (Appendix A)
xtask/                 fixture generation, grammar query checks, benches
```

## Build & test

Requires a recent stable Rust toolchain and a system `git` (≥ 2.38 recommended
for `--update-refs`).

```console
cargo build                       # build the whole workspace
cargo test                        # run all unit + integration tests
cargo run -p xtask -- fixtures    # generate fixture repos under target/fixtures
cargo run -p xtask -- lint-queries # validate the ktfqn grammar node kinds
cargo run -p stacksaw -- --help   # CLI help
```

## Quick tour

```console
# In any git repo with a stacked branch tracking an upstream:
stacksaw ls                                  # staircases + segments
stacksaw status
stacksaw show <rev> --output=json            # commit + trailers + findings
stacksaw lint --stair <name> --output=json   # run built-in linters
stacksaw lint --commit HEAD --fix            # apply autofixes (amend + restack)
stacksaw fix --commit <rev>                  # edit-session-based autofix
stacksaw edit begin --commit <rev> --output=json
stacksaw undo                                # restore the latest checkpoint
stacksaw schema finding                      # JSON Schema for machine consumers
stacksaw core status                         # per-repo daemon lifecycle
stacksaw                                     # open a TUI window
```

The CLI works with **no daemon** (`--no-daemon` / `STACKSAW_NO_DAEMON=1`) for
hermetic CI and agent drivers; semantics are identical, only caching differs.

## Protocols (P3 — protocol-shaped seams)

- **UI/CLI ↔ core:** SSP — JSON-RPC 2.0 with LSP-style `Content-Length` framing
  (`stacksaw-ssp`, §5).
- **core ↔ agent:** ACP over agent-subprocess stdio (`stacksaw-agents`, §9).
- **agent → stacksaw (inbound):** the CLI with `schemars`-generated JSON Schemas
  (§10).

## Exit codes (§10)

`0` ok · `1` findings/differences exist · `2` usage · `3` repo/git error ·
`4` daemon/protocol error · `5` lock timeout · `10` mutation aborted by policy.

## License

Apache-2.0.
