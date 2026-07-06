# CLAUDE.md

Guidance for AI agents (and humans) working in the `stacksaw` repository.

## What this is

`stacksaw` is a Rust terminal application for **viewing, reviewing, and
reshaping stacked/staircased git branches** before upload to a code-review
system. One binary, three faces: a TUI, a per-repo core service, and a
scriptable CLI. It implements design spec **v0.1**, kept verbatim in
[`ORIGINAL-SPEC.md`](ORIGINAL-SPEC.md) — that document is the source of truth
for behavior and section numbers. Source comments cite it as `(§n)`; keep those
references accurate (and consistent with `ORIGINAL-SPEC.md`) when you change
behavior.

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
- **Color is information (P6).** All color *math* lives in `stacksaw-rainbox` in
  OKLCH and is pure/property-tested. All style *choices* live in `theme.toml`
  (see [Theming](#theming-ui)). Every colored state also has a glyph or text —
  hue is never the sole carrier.
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

## Agent workflow

- **Always install before offering to commit.** After making changes the user
  will want to try, run `cargo install --path crates/stacksaw` and confirm it
  succeeded *before* asking whether to commit — never ask about committing while
  the installed binary is stale.

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

## Theming (UI)

`crates/stacksaw-ui/theme.toml` is the **single source of UI style** — treat it
as our CSS. It is embedded at compile time and resolved by
`stacksaw-ui/src/theme.rs` into a `Theme` the renderer queries; `app.rs` must
not hardcode colors, glyphs, or modifiers. To restyle or add a marked-up
element, add/extend a role in the TOML and consume it via `theme.style(...)` /
`theme.glyph(...)`, rather than writing a literal into rendering code.

- **Cascade.** Style resolves in layers, each overriding the previous:
  `[base]` → `[window.<id>]` → `[role.<id>]` (may `extends` another role) →
  `[role.<id>.<state>]` (per-state delta) → `[state.<id>]` (applied last). A
  role sets any of `fg`, `bg`, `glyph`, and the modifier flags.
- **Palette.** `[palette]` holds named semantic color tokens (`palette.warn`,
  `palette.ok`, …). Reference them, don't inline raw colors in roles.
- **Rainbow = identity (hue) + relevance (fade).** A `fg = { rainbow = "<src>" }`
  takes its hue from an `[identity.<src>]` source (`hash` of a renderer-supplied
  key, or `arc` position). Relevance is the orthogonal fade axis; the renderer
  passes a per-instance value (e.g. recents by MRU age) and `[rainbow.dim]` /
  `contrast_floor` parameterize the OKLCH dimming (which itself runs in
  `stacksaw-rainbox`). To add a new hued element, add an identity + a role that
  uses it — never call the color math directly with a hardcoded hue.
- **What does *not* belong in `theme.toml`.** It holds *style parameters* only.
  Keep in code: per-instance *data* that feeds styling (relevance values, arc
  index/total), layout budgets (elision widths, indents, context rows), free UI
  copy (command titles, headings), and git status letters (`A`/`M`/`D`/`R`/`C`).
  If a value isn't a color, glyph, or modifier choice, it probably isn't a theme
  concern.

## Where to start for common tasks

- New linter → `stacksaw-lint` (built-in) or the external-command tier; wire it
  into `default_builtins()` / config.
- New SSP method → add the constant in `stacksaw-ssp::method`, handle it in
  `stacksaw-core::server::dispatch`, and cover it in the conformance test.
- New CLI command → `cli.rs` (clap) + a handler in `commands.rs`/`main.rs`;
  give every read `--output=json` and a schema in `schema.rs`.
- New UI color/glyph/style → add or extend a role in `stacksaw-ui/theme.toml`
  and consume it; don't hardcode style in `app.rs` (see [Theming](#theming-ui)).
- Deferred work is tracked in `TODOs.md`.
