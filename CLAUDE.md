# CLAUDE.md

Guidance for AI agents (and humans) working in the `stacksaw` repository.

## What this is

`stacksaw` is a Rust terminal application for **viewing, reviewing, and
reshaping stacked/staircased git branches** before upload to a code-review
system. One binary, three faces: a TUI, a per-repo core service, and a
scriptable CLI. It implements design spec **v0.1** — source comments cite the
spec as `(§n)`; keep those references accurate when you change behavior.

## Golden rules

- **Protocol-shaped seams (P3).** Nothing crosses a seam ad hoc.
  - UI/CLI ↔ core is **SSP** (JSON-RPC 2.0, LSP-style `Content-Length` framing) —
    `stacksaw-ssp`.
  - core ↔ agent is **ACP** (JSON-RPC 2.0, newline-delimited over stdio) —
    `stacksaw-agents`.
  - agent → stacksaw (inbound) is the **CLI** with `schemars` JSON Schemas.
- **Git reads use `gix`; git mutations shell out to the user's `git`.**
  Never introduce `git2`/libgit2. Mutations must go through ref transactions
  (`git update-ref --stdin`) so hooks, `rerere`, and `--update-refs` behave as
  users expect (§4).
- **Never surprising with refs (P4).** Mutations are previewed, applied
  atomically, checkpointed, and undoable. Always write a checkpoint before
  moving real refs.
- **Color is information (P6).** All color math lives in `stacksaw-rainbox` in
  OKLCH and is pure/property-tested. Every colored state also has a glyph or
  text — hue is never the sole carrier.
- **The CLI must work with no daemon.** `--no-daemon` / `STACKSAW_NO_DAEMON=1`
  builds an in-process snapshot with identical semantics (only caching differs).
  Keep this path hermetic for CI and agent drivers.
- **Additive protocol evolution.** Unknown JSON-RPC fields MUST be ignored;
  bump `schemaVersion` rather than breaking readers.

## Workspace layout

See `README.md §Workspace layout`. Dependency direction (lower depends on
nothing higher):

```
ssp  →  git, lint-kotlin      (ssp is the shared vocabulary)
lint →  ssp, lint-kotlin
git  →  ssp
agents → ssp, git
core → ssp, git, lint, agents
ui   → ssp, rainbox
bin (stacksaw) → everything
```

Do not introduce cycles. Shared serializable DTOs live in `stacksaw-ssp::types`.

## Build & test

```console
cargo build                          # whole workspace; keep it warning-clean
cargo test                           # all unit + integration tests
cargo run -p xtask -- lint-queries   # validate ktfqn grammar node kinds
cargo run -p xtask -- fixtures       # generate fixture repos (target/fixtures)
cargo run -p stacksaw -- --help
```

- Tests use `insta`/`TestBackend` (UI golden frames), `proptest` (rainbox),
  `tempfile` fixture repos, and a bundled **fake ACP agent**
  (`crates/stacksaw-agents/src/bin/fake-acp-agent.rs`).
- Integration tests that shell out to `git` require a system `git` (≥ 2.38 for
  `--update-refs`).

## Conventions

- Errors: `thiserror` in libs, `anyhow` in the binary.
- Do not add narrating comments; comments explain non-obvious intent only.
- Tree-sitter: the grammar crate (`tree-sitter-kotlin`) pins the `tree-sitter`
  **0.20** core — keep those aligned; validate node kinds in `xtask
  lint-queries` before relying on new ones.
- Findings, suggestions, and edits share **one apply path**
  (`stacksaw_lint::apply::apply_suggestion`). When applying multiple
  suggestions, merge their edits and apply once against the original coordinate
  space (ranges then insertions), or offsets will drift.

## Where to start for common tasks

- New linter → `stacksaw-lint` (built-in) or the external-command tier; wire it
  into `default_builtins()` / config.
- New SSP method → add the constant in `stacksaw-ssp::method`, handle it in
  `stacksaw-core::server::dispatch`, and cover it in the conformance test.
- New CLI command → `cli.rs` (clap) + a handler in `commands.rs`/`main.rs`;
  give every read `--output=json` and a schema in `schema.rs`.
- Deferred work is tracked in `TODOs.md`.
