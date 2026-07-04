# TODOs

Deferred and partial work, tracked against the spec. Items are foundations that
exist in-code but are not fully realized; each notes the spec section and where
the work lives.

## Agents & restack

- [ ] **Live agent-in-the-loop restack (§9.5 steps 5–6).** The mechanics
  (checkpoint → scratch worktree → rebase `--update-refs`/fallback → classify →
  atomic ref move → cleanup) and the `RestackOutcome::Paused` handoff are in
  `stacksaw-agents/src/restack.rs`, but the automated loop that delegates each
  stop to an ACP agent, applies its fix, and runs `git rebase --continue` (with
  the max-attempts backoff) is not yet driven end-to-end.
  - AC to satisfy: fake-ACP-agent deterministically fixes a seeded ktfqn
    violation at step 2 of a 3-step staircase; refs move atomically; `undo`
    restores byte-identical refs.
- [ ] **`agent run <workflow>` (§9.4).** `stacksaw agent run` currently reports
  "not configured". Wire config-declared ACP agents (§9.2 drop-ins) to
  `AcpClient`, forward `session/update` to the UI/CLI, and map permission
  requests through the `Policy` (§9.3).
- [ ] **`agent/permission` forwarding to the UI (§5.3).** Server→client
  permission prompts are modeled but not rendered in the Checks column.
- [ ] **`_stacksaw/*` extension mini-schema (§17.2).** Version the extension
  payloads (`workflowContext`, `taskDone`).

## Staircase reshaping (§10.1 `stair`)

- [ ] `stair new` — create a new staircase/branch off upstream.
- [ ] `stair insert-after <rev>` — insert an empty commit/branch step.
- [ ] `stair fold <branch>` — fold a branch into its parent segment.
- [x] `stair rename` — implemented (git branch -m).

## Linters

- [ ] **WASM component linter tier (§7.4 tier 3, phase 2).** `wasmtime` +
  published WIT world `stacksaw:lint@1`. Explicitly must not block v1.
- [ ] **`content_pure` cache reuse across patch-id-preserving restacks (§7.2).**
  Cache keys exist (`scheduler::cache_key`); the redb-backed store and the
  patch-id equivalence check are not yet wired in core.
- [ ] **External-linter trust gate (§7.3).** The direnv-style one-time
  per-config-hash trust prompt is specified but not enforced before executing
  repo-declared external commands / agents.
- [ ] **Changed-range population for diff scope (§7.2).** `build_lint_jobs`
  leaves `changed_ranges` empty, so diff-scoped linters currently see whole
  files. Populate per-hunk changed lines from `imara-diff`.

## Core & watching

- [ ] **Ignore-aware worktree watching (§6).** The watcher watches `.git`
  recursively and the worktree root; it should descend the worktree using the
  `ignore` crate's matcher so `target/`, `node_modules/`, Bazel outputs, etc.
  are never traversed.
- [ ] **Targeted invalidation + reconciliation (§6).** Map ref/index/path events
  to scoped invalidation (drop only affected diff/highlight cache entries) and
  implement the 30 s mtime/hash reconciliation walk beyond the current timer.
- [ ] **`lint/run` streaming (§5.3).** Findings are computed synchronously and
  the run id returned; emit `lint/didFinish` notifications and support
  `lint/cancel`.
- [ ] **Delta snapshots + `snapshot/didAdvance` deltas (§5.3).** Only full
  snapshots are served today.
- [ ] **redb cache/state store (§4).** Lint results, highlight cache, and layout
  state are not yet persisted to `.git/stacksaw/cache.redb`.
- [ ] **`core verify` full re-sync (§3.1).** Currently resets the daemon; should
  force a full ref/index re-walk without necessarily stopping.
- [ ] **Multi-window `ui/link` selection sync (§3.2, open question §17.3).**

## UI

- [ ] **Diff column (§8.5).** The selected file's unified diff now renders
  (colored add/delete/hunk lines, scrollable). Still TODO: side-by-side mode
  (`s`), `similar` intraline emphasis, `syntect` viewport-lazy highlighting with
  a per-`(blob-oid, theme)` cache, inline findings + autofix preview, review
  notes, and interdiff mode (`I`).
- [ ] **Files column content (§8.1).** The selected commit's changed files now
  render (name-status, colored). Still TODO: multi-selected commit ranges,
  per-file selection driving the Diff column, and rename old→new display.
- [ ] **Mouse input (§8.2).** Click-to-select (stack row, commit card) and
  wheel-scroll + focus-on-click are implemented. Still TODO: divider
  drag/resize, collapse chevrons, header double-click zoom, findings-chip jumps,
  and hit-testing for Files/Diff once those columns render real content.
- [ ] **Command palette + help overlay (§8.2).** `:` `nucleo`-fuzzy palette
  exposing every action; `?` help; contextual hint bar.
- [ ] **Accessibility presets (§8.3).** `deuteranopia`, `tritanopia`, `mono`
  palettes; terminal background auto-detection (`terminal-colorsaurus`);
  `NO_COLOR` / `--ascii`.
- [ ] **Synchronized-update framing (§8.2).** Wrap frames in
  `BeginSynchronizedUpdate`/`EndSynchronizedUpdate`; kitty keyboard flags.
- [ ] **`--gui` wgpu renderer (§12, stretch).** The `RenderSurface` seam is in
  place; the pixel renderer is a non-goal for v1.

## CLI

- [ ] **Daemon-attached fast path for reads.** CLI reads run in-process; attach
  to a running core via `SspClient` when present for warm caching.
- [ ] **Optimistic concurrency & locking (§10).** `--if-generation N`,
  `--wait[=30s]`/`--no-wait` mutation lock handling, and the full exit-code
  contract for lock-timeout / policy-abort paths.
- [ ] **`comment export` for upload tooling (§8.5).** Notes are stored/listed;
  export mapping to a review-tool-friendly shape is minimal.

## Testing & QA (§14)

- [ ] SSP conformance suite covering **every** method, error code, cancellation
  mid-`lint/run`, and version-mismatch handshake (current test covers a subset).
- [ ] Fuzz the framing codec.
- [ ] Pre-2.38 git container test for the sequential `rebase --onto` fallback.
- [ ] `xtask bench` for the §8.6 performance budgets in CI.
- [ ] Antigravity adapter contract test against the fake harness (no API key).
