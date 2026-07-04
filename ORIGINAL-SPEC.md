# stacksaw — Specification v0.1 (draft for implementation)

**Status:** Draft · **Audience:** implementers · **Normative language:** the key words MUST, MUST NOT, SHOULD, SHOULD NOT, and MAY are to be interpreted as described in RFC 2119.

---

## 1. Overview

`stacksaw` is a Rust terminal application for **viewing, reviewing, and reshaping stacked and staircased git branches** before upload to a code review system (Gerrit-style workflows, but host-agnostic). It is one binary with three faces:

1. **A TUI** — a column-based, btop-caliber interface built on a "Rainbox" visual language: hue communicates identity and relationship, dimming communicates relevance.
2. **A per-repo core service** — owns git state, filesystem watching, lint scheduling, and agent sessions. Multiple UI windows and CLI invocations attach to it over a JSON-RPC protocol in the LSP/DAP/MCP/ACP idiom.
3. **A scriptable CLI** — every capability is exposed as non-interactive commands with stable JSON output, designed to be *driven by* external agents (their own code review, bug fixing inside a stack) without MCP.

Agent intelligence is pluggable in the outbound direction too: stacksaw can *drive* agents for workflows like "rebase this stack onto upstream and fix lint failures introduced at each step," via the **Agent Client Protocol (ACP)**, with a first-class adapter for **Google Antigravity's Python SDK**.

### 1.1 Name

The executable is `stacksaw`. Rationale: it saws through stacks; it is short, pronounceable, grep-able, and as of this writing collides with no widely used CLI tool, crate, Homebrew formula, or apt package. **M0 MUST include a final collision sweep** (crates.io, PyPI, npm, Homebrew, Debian/Arch repos, GitHub topic search) before the name is frozen. No short alias is shipped by default; users MAY define their own.

### 1.2 Design pillars

- **P1 — Review-first.** The primary loop is: inspect the stack against upstream, read diffs, catch problems (linters, agents, your own eyes), fix, restack, repeat. Uploading is out of scope (§18).
- **P2 — One source of truth.** All windows and CLI calls observe the same snapshot stream from one core process per repository.
- **P3 — Protocol-shaped seams.** UI↔core is JSON-RPC (SSP, §5). Core↔agent is ACP (§9). Agent↔stacksaw (inbound) is the CLI with JSON schemas (§10). Nothing crosses a seam ad hoc.
- **P4 — Never surprising with refs.** Mutations are previewed, applied atomically via ref transactions, checkpointed, and undoable.
- **P5 — Fast on monorepos.** Commit-graph-backed walks, incremental invalidation from filesystem events, viewport-lazy rendering.
- **P6 — Legible beauty.** Color is information, never decoration; every hue and every dimming level has a defined meaning (§8.3).

---

## 2. Definitions

| Term | Definition |
|---|---|
| **Upstream (U)** | The ref a stack is reviewed against. Resolved per-branch from `branch.<name>.merge`/tracking config, overridable via `--upstream` or repo config. |
| **Stack** | The ordered commits in `U..tip(B)` for a single branch B. |
| **Staircase** | An ordered branch sequence B₁…Bₙ sharing upstream U where `tip(Bᵢ)` is an ancestor of `tip(Bᵢ₊₁)` for all i. Degenerate but common case: one commit per branch. |
| **Segment** | The commits a staircase step contributes: `Seg(Bᵢ) = (tip(Bᵢ₋₁), tip(Bᵢ)]`, with `tip(B₀) := merge-base(B₁, U)`. |
| **Segment tree** | Generalization when branches fork mid-staircase: a tree of segments rooted at the fork point. stacksaw MUST render trees, not only linear staircases. |
| **Snapshot** | An immutable, generation-numbered view of repo state (refs, staircases, statuses) served by core. |
| **Finding** | A structured issue attached to a commit/file/range, produced by a linter or an agent (§7.1). |
| **Workflow** | A named agent-mediated operation (`restack`, `review`, …) with a defined contract (§9.4). |
| **Adapter** | An external executable speaking ACP that fronts a specific agent runtime (e.g. Antigravity). |
| **Twin** | A commit duplicated across branches, detected by `patch-id` or a `Change-Id:` trailer match. |

---

## 3. Process model and roles

One binary, three entry modes:

```
stacksaw                     # open a UI window; spawn/attach core as needed
stacksaw <subcommand> …      # CLI porcelain; attaches to core if running, else one-shot
stacksaw core serve          # run the core service in the foreground (internal; daemonized by default)
```

### 3.1 Core lifecycle

- Core is **per-repository** (keyed by the resolved common `.git` dir, so linked worktrees share one core).
- First client spawns core detached (`stacksaw core serve --daemon`); core writes a **discovery file** `.git/stacksaw/daemon.json`:

```json
{ "pid": 41234, "endpoint": "unix:/run/user/1000/stacksaw/3f9c….sock",
  "protocolVersion": "1.0", "binaryVersion": "0.1.0", "startedAt": "2026-07-04T18:02:11Z" }
```

- Endpoint: Unix domain socket under `$XDG_RUNTIME_DIR/stacksaw/` (mode 0700 dir, 0600 socket, peer-UID checked), falling back to `$TMPDIR`; on Windows, named pipe `\\.\pipe\stacksaw-<repo-hash>` with an owner-only DACL.
- Stale detection: clients MUST validate pid liveness *and* complete an `initialize` handshake; on failure they delete the discovery file and respawn. Spawn races are settled with an exclusive lock on `.git/stacksaw/daemon.lock` (crate `fs4`).
- Core exits after the last client disconnects plus a grace period (default `core.idle_shutdown = "10m"`) so back-to-back CLI calls stay warm. `stacksaw core stop|status|verify` manage it explicitly.
- CLI commands MUST also work with **no core** (`STACKSAW_NO_DAEMON=1` or `--no-daemon`): they build a one-shot snapshot in-process. Semantics are identical; only caching differs. This is required for CI and for hermetic agent drivers.

### 3.2 Multiple UI windows

Every window is an independent SSP client. Core broadcasts change notifications; view state (focus, collapse, scroll) is client-local. Windows MAY join a named **link group** (`ui/link`) to synchronize selection (e.g., diff in one terminal, graph in another). AC: two windows attached to one repo both reflect a `git commit` made in a third terminal within 150 ms (p95).

---

## 4. Crate stack (prescriptive)

Implementers MUST use the following unless a documented ADR supersedes an entry. Pin exact versions in `Cargo.lock`; the table gives the contract, not the version.

| Concern | Crate(s) | Notes |
|---|---|---|
| Terminal UI | `ratatui` | Immediate-mode widgets; `TestBackend` for snapshot tests. |
| Terminal backend, input | `crossterm` | Mouse capture (click/drag/wheel/move), kitty keyboard enhancement flags, `BeginSynchronizedUpdate`/`EndSynchronizedUpdate` for tear-free frames. |
| Async runtime | `tokio` (+ `tokio-util`) | Multi-threaded runtime in core; `codec::Framed` for SSP framing. |
| Git reads | `gix` (gitoxide) | Refs, ODB, revwalk (commit-graph-aware), status, mailmap, `patch-id`-equivalent hashing. Pure Rust, no libgit2. |
| Git mutations | system `git` via `tokio::process` | Rebase/amend/cherry-pick/worktree shell out to the user's git so hooks, `rerere`, sequencer semantics, and `--update-refs` behave exactly as users expect. `git2` MUST NOT be introduced. |
| Diff hunking | `imara-diff` | Histogram/Myers; same engine gix uses. |
| Intraline diff | `similar` | Word-level refinement inside changed line pairs only. |
| Syntax highlight | `syntect` + `two-face` | Broad grammar coverage for the diff pane; highlight viewport-only, cache per `(blob-oid, theme)` in redb. |
| Lint parsing | `tree-sitter`, `tree-sitter-kotlin` | Pinned grammar version; queries compile-checked in CI (§7.5). |
| Color math | `palette` | All Rainbox math in OKLCH (§8.3); quantize to 256-color when truecolor is absent. |
| Terminal bg detection | `terminal-colorsaurus` (optional feature) | Adapts dim-mixing target to the real background; config fallback `ui.background = "dark"\|"light"`. |
| RPC framing/types | internal crate `stacksaw-ssp`: `serde`, `serde_json` | JSON-RPC 2.0, LSP-style `Content-Length` framing (§5.1). No external JSON-RPC framework; the codec is ~100 LOC and must be exact. |
| Local sockets | `tokio` `UnixListener` / `net::windows::named_pipe`; `interprocess` MAY unify if Windows parity demands it | |
| FS watching | `notify` + `notify-debouncer-full`, `ignore` | §6. |
| CLI | `clap` v4 (derive) + `clap_complete` | Completions for bash/zsh/fish/pwsh shipped. |
| JSON schemas | `schemars` | Every `--output=json` payload has a schema; `stacksaw schema <name>` prints it. |
| Config | `toml` + `serde`; paths via `directories` | Layered merge is bespoke (§11); no config framework. |
| Cache/state store | `redb` | Lint results, highlight cache, layout state. Pure-Rust embedded KV; one file `.git/stacksaw/cache.redb`. |
| Locks | `fs4` | Advisory repo/mutation locks. |
| Time | `jiff` | TZ-correct ages, half-life decay. |
| Fuzzy match | `nucleo-matcher` | Command palette, ref pickers. |
| Errors | `thiserror` (libs), `anyhow` (bin) | |
| Logging | `tracing`, `tracing-subscriber`, `tracing-appender` | File logging only while a TUI owns the terminal; `stacksaw core log --follow` tails it. |
| Parallelism | `rayon` | Per-commit lint fan-out on a dedicated pool bridged via `spawn_blocking`; never on the tokio worker threads. |
| Hashing | `blake3` | Cache keys: `(commit-oid, linter-id, linter-version, config-hash)`. |
| Text width | `unicode-width`, `unicode-segmentation` | Truncation/ellipsis correctness incl. CJK. |
| Clipboard | OSC 52 emission (built-in), `arboard` optional | OSC 52 keeps copy working over SSH. |
| WASM linters (phase 2) | `wasmtime` (component model) + `wit-bindgen` | §7.4 tier 2. |
| Testing | `insta`, `assert_cmd`, `tempfile`, fixture repos built by `xtask` | |

**Workspace layout:**

```
crates/
  stacksaw/          # bin: clap entry, role dispatch
  stacksaw-core/     # daemon: snapshots, scheduler, sessions
  stacksaw-git/      # gix wrapper, staircase model, ref transactions, undo
  stacksaw-ssp/      # protocol types + Content-Length codec (shared client/server)
  stacksaw-ui/       # ratatui app (SSP client)
  stacksaw-rainbox/  # color engine (pure functions; property-tested)
  stacksaw-lint/     # finding model, scheduler, built-ins, external-command tier
  stacksaw-lint-kotlin/  # ktfqn (§7.5)
  stacksaw-agents/   # ACP client, workflow orchestration, scratch worktrees
adapters/
  antigravity/       # Python package: stacksaw-antigravity-adapter (Appendix A)
xtask/               # fixture generation, grammar query checks, release
```

---

## 5. SSP — the Stacksaw Session Protocol (UI/CLI ↔ core)

### 5.1 Transport and framing

JSON-RPC 2.0 over the local socket from §3.1, framed **exactly like LSP/DAP**:

```
Content-Length: <bytes>\r\n
\r\n
{ "jsonrpc": "2.0", … }
```

This deliberately matches LSP so existing inspectors, fuzzers, and codec muscle memory apply. Requests, responses, notifications, batch-free. Cancellation uses `$/cancelRequest`; long operations report `$/progress` with a token, as in LSP.

### 5.2 Lifecycle

`initialize` (client→core) negotiates `protocolVersion` (semver; core rejects incompatible majors), declares client kind (`ui`, `cli`, `automation`) and capabilities; `initialized` notification completes the handshake; `shutdown`/`exit` mirror LSP. Unknown methods return JSON-RPC `-32601`; unknown fields MUST be ignored (additive evolution).

### 5.3 Method surface (v1)

| Method | Dir | Purpose |
|---|---|---|
| `initialize` / `shutdown` / `exit` | C→S | Lifecycle. |
| `subscribe { topics }` | C→S | Topics: `refs`, `worktree`, `lint`, `agents`, `snapshot`. |
| `workspace/snapshot { generation? }` | C→S | Full or delta snapshot: staircases, segment trees, branch metadata, ahead/behind vs U, dirty state. |
| `commit/get { oid }` | C→S | Message, trailers, parents, twin links, finding counts. |
| `diff/range { from, to, path?, algorithm? }` | C→S | Hunk list; intraline spans; rename detection on. |
| `diff/interdiff { rangeA, rangeB }` | C→S | `git range-diff` semantics between two stack versions (§8.6). |
| `lint/run { scope, linters? }` | C→S | Schedules; results stream as notifications. Returns run id. |
| `lint/cancel { runId }` | C→S | |
| `agent/list` · `agent/start { agent, workflow, params }` · `agent/prompt { sessionId, text }` · `agent/cancel` | C→S | Outbound agent control (§9). |
| `agent/permission { sessionId, request }` | **S→C** | Core forwards ACP permission requests to whichever window holds the session lease; UI renders approve/deny. |
| `mutate/apply { plan, ifGeneration }` | C→S | Executes a mutation plan (restack, amend, fix-apply) under the repo mutation lock; optimistic-concurrency via generation. |
| `ui/link { group }` / `ui/didFocus` | C↔S | Optional multi-window selection sync. |
| `refs/didChange` · `worktree/didChange` · `snapshot/didAdvance { generation }` · `lint/didFinish { runId, findings }` · `agent/didUpdate { sessionId, event }` | **S→C** | Subscription notifications. |

AC: a fake-client conformance suite exercises every method, every error code, cancellation mid-`lint/run`, and a version-mismatch handshake.

---

## 6. External change detection

stacksaw MUST notice changes made by any other tool (git CLI, IDEs, agents) without polling-by-default.

- Watch set (via `notify` + `notify-debouncer-full`, 50 ms debounce, event coalescing): `.git/HEAD`, `.git/refs/**`, `.git/packed-refs`, `.git/index`, `.git/MERGE_HEAD`/sequencer dirs, `.git/worktrees/*`, and the worktree(s).
- Worktree watching MUST honor ignore rules using the `ignore` crate's matcher so `target/`, `node_modules/`, Bazel output roots etc. are never descended into.
- Event → invalidation mapping is targeted: ref events re-resolve refs and staircases only; index events refresh status; path events refresh status for those paths and drop affected diff/highlight cache entries. Every invalidation bumps the snapshot generation and emits `snapshot/didAdvance`.
- Editors that save via atomic rename MUST be handled (watch parent dirs, re-stat on `Rename`).
- Safety valves: a low-frequency reconciliation walk (default 30 s, cheap: compares ref/index mtimes + hashes) catches dropped events; `notify`'s poll backend is auto-selected on filesystems where inotify/FSEvents are unreliable (network mounts); `stacksaw core verify` forces a full re-sync.
- AC: `git commit` in another terminal is reflected in an open UI in <150 ms p95; a 500-file `git checkout` causes at most one snapshot advance after debounce.

---

## 7. Findings and the linter framework

### 7.1 Finding model (shared by linters *and* agents)

```json
{
  "schemaVersion": 1,
  "source": "linter:ktfqn",
  "code": "ktfqn/avoid-fqn",
  "severity": "error | warning | info",
  "commit": "8c1f…",
  "location": { "file": "app/src/Main.kt",
                "range": { "start": {"line": 41, "col": 9}, "end": {"line": 41, "col": 42} } },
  "message": "Use an import for java.util.concurrent.ConcurrentHashMap",
  "suggestion": { "edits": [ { "file": "…", "range": …, "newText": "ConcurrentHashMap" },
                             { "file": "…", "insertAfterLine": 12, "newText": "import java.util.concurrent.ConcurrentHashMap" } ] },
  "tags": ["autofixable"]
}
```

Message-level findings (commit-message linters) use `location: { "messageLine": 1 }`. Suggestions are ordinary edit lists so `stacksaw fix`, the UI, and agents all apply them through one code path.

### 7.2 Scheduling and caching

Linters run **per commit, against that commit's tree/diff**, in parent-before-child order, fanned out on the rayon pool. Results are cached in redb keyed by `blake3(commit-oid ‖ linter-id ‖ linter-version ‖ config-hash)`; a restack that preserves a patch-id MAY reuse results when the linter declares itself `content_pure = true`. Default lint scope is `diff` — only lines/files the commit itself touches — because this is a review tool, not a repo-wide janitor. `scope = "file"` widens per linter.

### 7.3 Configuration and trust

Linters are declared in layered config (§11). Repo-level config (`.stacksaw.toml`, checked in) may declare **external commands**; because that is arbitrary code execution, stacksaw MUST require one-time trust per repo config hash (direnv-style: first run shows the commands and asks to allow; decision stored in user state). Built-in linters need no trust.

### 7.4 Extension tiers

1. **Built-in** (Rust, in-process): commit-message rules, copyright header, ktfqn.
2. **External command linters** — the primary extension point, in the spirit of git hooks/pre-commit. Contract: stacksaw execs the command with a JSON job on stdin (`{ commit, files: [{path, oldOid, newOid, changedRanges}], repoRoot, worktree, configBlob, cacheDir }`) and reads a JSON findings array on stdout; nonzero exit without valid JSON = linter error (surfaced, not fatal); wall-clock timeout (default 30 s) and cwd pinned to a read-only checkout when `mode = "tree"`.
3. **WASM component linters** (phase 2, stretch): `wasmtime` + a published WIT world `stacksaw:lint@1` — true sandboxing and portability for shareable linters. MUST NOT block v1.

### 7.5 Built-in linter specs

**`commitmsg`** — configurable rules: subject ≤ N chars (default warn 50 / error 72), blank line after subject, body wrap ≤ 72, required trailer patterns (e.g. `^Change-Id: I[0-9a-f]{40}$`, `^Bug: b/\d+$`), forbidden prefixes on upload profile (`WIP`, `fixup!`, `squash!`), subject-mood heuristic OFF by default. Profiles: `local` (lenient) vs `upload` (strict); `stacksaw lint --profile upload` is the pre-flight.

**`copyright`** — per-extension comment styles, template with `{year}` and `{holder}`; applies to files *added* in a commit by default (`mode = "added"`, options `touched|all`); `{year}` validates against the commit's author year with a grace window; autofix inserts the header after shebang/license-guard lines.

**`ktfqn`** — the reference tree-sitter linter, specified in full:

- **Goal.** Flag Kotlin code that references a type or member by fully-qualified name inline (`val m = java.util.concurrent.ConcurrentHashMap<K,V>()`) instead of importing it and using the short name.
- **Scope guard (per requirements):** only dotted paths whose **first segment is in a configured well-known-root set** are flagged — default `["com", "org", "net", "io", "java", "javax", "jakarta", "kotlin", "kotlinx", "android", "androidx", "dev", "edu", "gov"]` — with ≥ 2 segments and an initial-uppercase segment somewhere in the chain (heuristic for "reaches a type"). This deliberately never fires on ordinary receiver chains like `config.build.flavor`.
- **Mechanics.** Parse with pinned `tree-sitter-kotlin`. Capture candidate nodes in both positions:

```scm
;; illustrative — node names MUST be validated against the pinned grammar by a
;; compile-time query check in CI (xtask lint-queries)
(user_type)               @candidate.type   ;; type position: com.foo.Bar
(navigation_expression)   @candidate.expr   ;; expression position: com.foo.Bar.baz()
(import_header)           @ctx.import
(package_header)          @ctx.package
```

  For each candidate, take the **root-most** dotted chain (skip if an ancestor was already captured, to avoid double-flagging `a.b.C` inside `a.b.C.d`), reconstruct the dotted text, and apply the scope guard. Skip anything inside `@ctx.import`/`@ctx.package`, inside KDoc, or in generated files (configurable glob).
- **Fix.** Suggest: insert `import <fqn-up-to-type>` in sorted position among existing imports, replace the flagged occurrence(s in changed ranges) with the short name. If the short name is already imported from a *different* package or declared in-file, downgrade to `info` with no autofix (it's deliberate disambiguation).
- **Config:**

```toml
[lint.ktfqn]
enabled = true
roots = ["com", "kotlinx", "java", "javax", "org", "androidx", "kotlin"]
scope = "diff"          # only changed lines
severity = "warning"
exclude = ["**/generated/**"]
```

- AC: fixture corpus covers FQN-in-type-position, FQN-in-call-chain, import-header immunity, receiver-chain immunity (`foo.bar.baz()` with lowercase-only segments and non-root first segment), KDoc immunity, and autofix idempotence.

---

## 8. UX specification

### 8.1 Columns

The layout primitive is the **column**. Default arrangement, left→right:

```
┌─Stacks──┬─Commits──────────────┬─Files──────┬─Diff───────────────────────────┬─Checks─┐
│ ○ main  │ ╭─ feat/wire-proto ─╮│ M app/A.kt │ @@ -12,6 +12,9 @@              │ ⚠ 2    │
│ ● feat/*│ │ 8c1f Add codec    ││ A app/B.kt │  …side-by-side or unified…     │ ● agent│
│   3 stps│ ╰┬──────────────────╯│ M doc/x.md │                                │  log   │
│ ○ fix/… │ ╭┴ feat/use-proto ──╮│            │                                │        │
│         │ │ 22ab Route calls  ││            │                                │        │
└─────────┴──────────────────────┴────────────┴────────────────────────────────┴────────┘
```

1. **Stacks** — branches/staircases in the repo, one row each, hue-chipped, with ahead/behind vs upstream and dirty markers.
2. **Commits** — the selected staircase rendered as a segment tree (§8.4).
3. **Files** — changed files of the selected commit or multi-selected range.
4. **Diff** — the widest column; §8.5.
5. **Checks/Agents** — findings summary, live agent session stream, permission prompts. Hidden by default; opens on activity or `5`.

Column behaviors (all MUST):
- **Collapse** to a 3-cell spine: rotated title, identity color strip, count badges. Click spine or `1..5` to expand. Because horizontal space is precious, collapsed state is the norm for Stacks and Checks.
- **Resize** by dragging dividers; double-click a divider to reset; widths persist per repo.
- **Zoom** (`z` or double-click a header): focused column maximizes, others collapse to spines; `z` again restores.
- **Responsive auto-collapse** when the terminal narrows, in priority order Diff > Commits > Files > Stacks > Checks; under 100 columns, **deck mode**: a single full-width column with a breadcrumb (`Stacks ▸ feat/* ▸ 8c1f ▸ A.kt`), `←`/`→` or click crumbs to move between decks. Minimum supported size 80×24.

### 8.2 Input

- **Mouse (crossterm capture):** click to focus/select; wheel scrolls the **hovered** column (tracked from motion events), not merely the focused one; drag dividers; click collapse chevrons; middle-click header collapses; double-click header zooms; click a hunk gutter to stage a review note; click findings chips to jump.
- **Keyboard:** full parity — every mouse action has a binding (Appendix D). `Tab`/`Shift-Tab` and `h/l` move across columns, `j/k` within; `space` multi-selects commits to form a range; `:` opens a `nucleo`-fuzzy command palette exposing *every* action (discoverability requirement); `?` overlays help; a one-line hint bar shows contextual keys.
- Rendering uses diffed buffers (ratatui) wrapped in synchronized-update escapes; input-to-paint latency budget ≤ 16 ms; animations (spinners, dim transitions ~120 ms ease-out) tick at most 30 fps and only while visible.

### 8.3 Rainbox — the color system (normative)

All color math happens in **OKLCH** (`palette`), then converts to terminal RGB; if truecolor is absent, quantize to the 256-color cube by nearest OKLab distance; honor `NO_COLOR`; `--ascii` strips non-ASCII glyphs.

**Identity.** Every branch owns a hue:
- Unrelated branches: `hue = golden_angle_sequence(stable_hash(branch_name))` — maximal separation, stable across sessions.
- Staircase steps: an **evenly spaced arc** so order is legible at a glance — for step *i* of *n*: `h(i) = h₀ + span · i/(max(n−1,1))`, default `h₀ = 250°, span = −190°` (blue → magenta → orange). The staircase literally reads as a rainbow ramp.
- Commits inherit their segment's hue; files and hunk accents inherit the commit's.

**Relationships.** Parent→child edges between segments render in the **hue midpoint** of the two segments. Twins (§2) show a `⧉` chip in the sibling segment's hue. The upstream lane is fixed neutral gray so review context never competes with identity colors.

**Relevance dimming.** Each element carries relevance `r ∈ [0,1]`, the max of weighted signals:
- *temporal*: `exp(−ln2 · age/half_life)`, default half-life 14 d (branches use last-commit age);
- *topological*: 1.0 for the focused element, 0.75 same segment, 0.55 adjacent segment, 0.30 unrelated;
- *attention*: elements with open findings or an active agent get a floor of 0.85;
- *state*: merged/landed branches clamp to 0.15; upstream context commits 0.20;
- *search*: palette/filter matches force 1.0.

Dim factor `d = 1 − r` applies as `L' = lerp(L, L_bg, 0.75·d)` and `C' = C · (1 − 0.85·d)` — perceptually uniform fade toward the (detected) background. A **contrast floor** MUST clamp `|L' − L_bg| ≥ 0.18` so nothing becomes unreadable. Selection overrides to full chroma plus an inverted header cell.

**Accessibility.** Ship palette presets `default`, `deuteranopia`, `tritanopia`, `mono` (dimming-only); hue is never the *sole* carrier of a distinction — every colored state also has a glyph or text (chips: `✓ ✗ ⚠ ⧉ ✎`, ahead/behind arrows, etc.).

AC: `stacksaw-rainbox` is a pure crate with property tests (contrast floor holds for all `r`, both backgrounds; 256-color quantization round-trips within ΔE budget) and golden SVG swatches rendered in docs.

### 8.4 The staircase view (Commits column)

- Segments render as **stair steps**: each branch segment indents one cell and opens with a riser `╭┴ branch-name ─` pill in its hue; commits are cards (`hash · subject · chips · age(dimmed) · author initials`). A `compact` mode drops indentation and uses per-segment colored gutter bars instead (dense staircases of 20+ steps).
- Forked segment trees draw split risers; the trunk-first ordering is stable (topological, then ref name).
- Header shows `upstream main ↑3 ↓12 · fetched 2h ago`; behind-upstream commits available to view dimmed at the bottom (`state` relevance).
- Multi-select builds a range: Files/Diff then show the combined diff `A^..B`.
- Chips per card: lint status (`✓`/counts), agent activity spinner, twin `⧉`, dirty (`✎` if the commit is HEAD and worktree dirty).

### 8.5 Diff column

- Unified or side-by-side (`s` toggles; auto side-by-side ≥ 160 cols). Word-level intraline emphasis (`similar`) computed only for paired changed lines. Syntax highlighting via syntect, viewport-lazy, cached.
- **Findings inline:** gutter marks in severity color; expanding (`enter` on mark or click) inserts virtual annotation lines showing message + autofix preview; `n`/`p` cycle findings; `a` applies an autofix (routes through `mutate/apply` as an amend-and-restack plan, §9.5 mechanics, with preview).
- **Review notes:** `c` on a line drafts a local note (stored under `.git/stacksaw/notes/`, never in commits); notes render like findings with source `note:me`; `stacksaw comment export --output=json` emits them for any upload tooling. This keeps stacksaw Gerrit-shaped without Gerrit coupling.
- **Interdiff mode** (`I`): pick two versions of the stack — reflog checkpoints (§9.5) or explicit refs — and render `git range-diff`-style commit pairing with per-pair diffs-of-diffs. This is the killer review feature for re-uploads.

### 8.6 Performance budgets (acceptance criteria)

On a linux-kernel-scale repo with commit-graph present: cold `stacksaw` to first painted frame < 300 ms (shell paints immediately; data streams in); ref change → repaint < 150 ms p95; scroll at 60 fps with highlight cache warm; core RSS < 150 MB for the baseline view. `xtask bench` runs these against generated fixture repos in CI.

---

## 9. Outbound agents: stacksaw drives intelligence

### 9.1 Protocol choice: ACP

stacksaw acts as an **Agent Client Protocol (ACP) client**. ACP is the established, LSP-shaped standard for exactly this seam — JSON-RPC 2.0 over the agent subprocess's stdio, with sessions, streamed updates, permission requests, and client-provided fs/terminal capabilities — and it already has native agents (Gemini CLI, Goose, Copilot CLI, others) plus official SDKs, including Python. Choosing ACP means:

- any existing ACP agent works with **zero adapter code**;
- runtimes without native ACP (Antigravity) need only a thin adapter executable (Appendix A);
- "configurable from the built binary" is satisfied the same way editors configure LSP servers: **declarations in user config pointing at commands** — no recompilation, no dynamic linking.

stacksaw-specific structure rides on ACP's extension mechanism (`_`-prefixed extension methods/notifications), namespaced `_stacksaw/*`, and MUST degrade gracefully: if an agent ignores `_stacksaw/workflowContext`, the same information is embedded in the initial prompt text.

### 9.2 Configuration (the plugin point)

```toml
# ~/.config/stacksaw/config.toml  — or drop-in files in ~/.config/stacksaw/agents/*.toml
[agents.antigravity]
command   = "uvx"
args      = ["stacksaw-antigravity-adapter"]     # any ACP-speaking executable
protocol  = "acp"
workflows = ["restack", "review"]                # which contracts it accepts
env       = { GEMINI_API_KEY = "${env:GEMINI_API_KEY}" }

[agents.gemini]                                   # native ACP agent: no adapter at all
command   = "gemini"
args      = ["--experimental-acp"]
protocol  = "acp"
workflows = ["review"]
```

Drop-in discovery (`agents/*.toml`) lets adapter packages install themselves. Repo-level agent declarations follow the same trust gate as external linters (§7.3). `stacksaw agent list` shows resolved agents and a `doctor` handshake check.

### 9.3 Execution sandbox

Agent workflows that mutate history run in a **scratch linked worktree** (`git worktree add --detach`), never the user's checkout. Because linked worktrees share refs, workflows operate on a **detached HEAD** and stage results as candidate commits; real refs move only in the final atomic transaction (§9.5). The agent's fs and terminal capabilities (served by stacksaw over ACP) are rooted at the scratch worktree; requests outside it are denied. Every tool-permission request the agent raises is forwarded to the UI (`agent/permission`) or auto-resolved per policy:

```toml
[agents.policy]
allow = ["git add", "git rebase --continue", "read:**"]
ask   = ["git *", "write:**"]        # surfaced in the Checks column / CLI prompt
deny  = ["git push*", "network:*"]
```

### 9.4 Workflow contracts

A workflow is a named contract: structured context in, structured result out, human-readable streaming in between.

**`review`** — context: staircase description, per-commit diffs (or the agent may pull them itself via the inbound CLI, §10), review guidelines blob from config. Result: findings conforming to §7.1 with `source: "agent:<name>"`, rendered exactly like linter findings (inline gutter marks, Checks column). Nothing is mutated.

**`restack`** — "rebase onto upstream, fixing issues along the way." Parameters: `{ staircase, onto, fixPolicy: { linters: [...], failOn: "error" }, conflictPolicy: "agent" | "stop" }`.

### 9.5 The restack state machine (normative)

Requires git ≥ 2.38 for `--update-refs`; stacksaw MUST detect the version and fall back to sequential per-branch `rebase --onto` with an explicit ref map.

```
1  CHECKPOINT   write refs/stacksaw/checkpoints/<ts>/* for every ref in the staircase
2  SPAWN        scratch worktree at old tip (detached)
3  REBASE       git rebase --onto <onto> <fork> --update-refs \
                  --exec 'stacksaw lint --commit HEAD --profile upload --output=json --fail-on error'
4  RUN          sequencer proceeds; each stop is classified:
                  CONFLICT   → conflict task
                  EXEC-FAIL  → lint task (the failing findings JSON is the task payload)
5  DELEGATE     build ACP prompt: task type, commit under edit, findings/conflict files,
                  allowed tools, "when done run: git add -A && git rebase --continue"
                  (agent may instead signal _stacksaw/taskDone and stacksaw continues for it)
6  LOOP         back to 4 until the sequencer finishes or retries exhaust
                  (default max 3 agent attempts per stop, then pause for the human)
7  VERIFY       every step re-lints clean; staircase shape preserved (same segment count
                  unless the plan said otherwise); tree diffs vs plan expectations
8  PROPOSE      show the plan: per-branch old→new oids, interdiff available on demand
9  COMMIT-REFS  git update-ref --stdin transaction moving all staircase refs atomically
                  (mode auto|confirm, default confirm in UI, --yes in CLI)
10 CLEANUP      remove worktree; keep checkpoints for `stacksaw undo`
```

The `--exec` line in step 3 is the heart of "rebase while fixing lint issues along the way": **stacksaw's own CLI is the oracle** inside the rebase, so the agent, the human, and CI all judge success identically. `stacksaw undo [<checkpoint>]` restores any checkpoint via the same atomic transaction.

AC: an integration test drives the full loop with a **fake ACP agent** binary that deterministically fixes a seeded ktfqn violation at step 2 of a 3-step staircase fixture, verifying refs move atomically and `undo` restores byte-identical refs.

---

## 10. Inbound automation: standalone agents drive stacksaw (CLI, not MCP)

An external agent doing its own review, or fixing a bug in the middle of a staircase, uses the same `stacksaw` binary as a **command-line tool**. Design promises:

- **Stable JSON.** Every read supports `--output=json` (single envelope) or `--output=jsonl` (streams). Payloads carry `schemaVersion`; evolution is additive; `stacksaw schema <name>` prints the JSON Schema (schemars-generated) for machine consumption and prompt-stuffing.
- **Non-interactive by construction.** No hidden prompts: `--yes` / `--no-input` everywhere; errors are structured on stderr (`{"error": {"code": "...", "message": "..."}}`) when `--output=json`.
- **Exit codes:** `0` ok · `1` findings/differences exist (lint, diff --quiet) · `2` usage · `3` repo/git error · `4` daemon/protocol error · `5` lock timeout · `10` mutation aborted by policy.
- **Concurrency:** mutations take the repo mutation lock (`--wait[=30s]`/`--no-wait`), and accept `--if-generation N` for optimistic concurrency against a previously observed snapshot.
- **Determinism:** stable ordering, no ANSI when stdout is not a TTY, `STACKSAW_NO_DAEMON=1` for hermetic runs.

### 10.1 Command surface (v1)

```
stacksaw ls        [--output=json]                 # staircases + segments overview
stacksaw status    [--output=json]
stacksaw show <rev> [--output=json]                # commit + trailers + findings
stacksaw diff  [<range>] [--patch|--output=json] [--name-only]
stacksaw interdiff <refA> <refB> [--output=json]
stacksaw lint  [--commit X|--range A..B|--stair NAME|--all] [--profile P]
               [--fail-on error|warning] [--fix] [--output=json]
stacksaw fix   --commit X [--linter L] [--yes]     # apply suggestions: amend + restack descendants
stacksaw restack [--onto U] [--agent NAME] [--fix-lints] [--yes] [--output=jsonl]
stacksaw stair new|insert-after <rev>|fold <branch>|rename …
stacksaw edit  begin --commit X --output=json      # → { token, worktree }  (see below)
stacksaw edit  finish --token T [--message-file F] [--yes]
stacksaw edit  abort  --token T
stacksaw comment add|ls|export
stacksaw watch --output=jsonl                      # live event stream (refs/worktree/lint)
stacksaw undo  [<checkpoint-id>] ; stacksaw checkpoints ls
stacksaw agent list|run <workflow> [--agent N] [--output=jsonl]
stacksaw schema [<name>] ; stacksaw core serve|stop|status|verify
```

### 10.2 Edit sessions — the flagship inbound primitive

"Fix a bug in commit 3 of the staircase" without making the agent learn interactive rebase:

```console
$ stacksaw ls --output=json | jq '.staircases[0].segments[2].commits[0].oid'
"22ab9e…"
$ stacksaw edit begin --commit 22ab9e --output=json
{ "schemaVersion":1, "token":"e7f2…", "worktree":"/tmp/stacksaw/edit-e7f2",
  "commit":"22ab9e…", "descendants":4 }
# …agent edits files in that worktree with its own tools…
$ stacksaw lint --commit WORKTREE --output=json --fail-on error   # optional self-check
$ stacksaw edit finish --token e7f2 --yes --output=json
{ "rewrites":[{"old":"22ab9e…","new":"91cc0d…"}, …],
  "updatedRefs":["refs/heads/feat-3","refs/heads/feat-4"],
  "checkpoint":"2026-07-04T18-40-12Z" }
```

`finish` amends the commit from the worktree state, restacks all descendants (`--update-refs` machinery from §9.5), runs the ref transaction, and reports the old→new map — everything an orchestrating agent needs to update its own bookkeeping. Sessions are crash-safe: `edit abort` or token GC (default 24 h) removes the worktree; real refs were never touched until `finish`.

---

## 11. Configuration

Layers, later wins: built-in defaults → `/etc/stacksaw/config.toml` → `~/.config/stacksaw/config.toml` (+ `agents/*.toml`, `linters/*.toml` drop-ins) → repo `.stacksaw.toml` (checked in; shareable lint/staircase policy) → `.git/stacksaw/config.toml` (local, unshared) → environment (`STACKSAW_*`) → flags. Repo layers that declare executables require the §7.3 trust gate. `stacksaw config show --origin` prints the merged view with provenance per key.

```toml
[ui]         theme = "default"; background = "auto"; date_style = "relative"
[rainbox]    staircase_arc = [250, -190]; half_life = "14d"; contrast_floor = 0.18
[upstream]   default = "origin/main"
[lint]       profile = "local"
[watch]      reconcile_interval = "30s"
```

---

## 12. GPU acceleration analysis (requested)

**Short answer: not for v1, and mostly not ever — but the design keeps one door open.**

- **Rendering.** A TUI's output is a grid of cells; rasterizing glyphs onto pixels is the *terminal emulator's* job, and users already get GPU acceleration by running kitty/Alacritty/WezTerm/Ghostty. What stacksaw controls is how little it asks the terminal to redraw: ratatui's diffed double-buffer plus synchronized-update sequences (§8.2) is the correct optimization, and no in-process GPU work can improve on it.
- **Compute.** The hot paths — commit-graph walks, status, imara-diff hunking, tree-sitter parsing, fuzzy matching — are branchy, pointer-chasing, small-batch workloads with poor arithmetic intensity. PCIe transfer and kernel-launch overhead would dominate at interactive sizes. The winning levers are the ones specified: commit-graph files, incremental invalidation (§6), rayon fan-out (§7.2), and viewport-lazy highlighting with a persistent cache — syntect on a 20k-line file is the only real throughput risk, and caching beats GPUs there.
- **The one legitimate future** is a pixel renderer: a `stacksaw --gui` mode drawing the same scene via `wgpu` (Zed-style) for smooth sub-cell animation, minimaps, and proportional fonts. To keep that possible cheaply, `stacksaw-ui` MUST keep a `RenderSurface` seam between layout (columns, cards, rainbox colors as abstract OKLCH) and the ratatui backend. Building that renderer is explicitly a non-goal for v1.

---

## 13. Security

Scratch-worktree isolation and permission gating for agents (§9.3); trust gate for repo-declared executables (§7.3); socket/pipe restricted to the owning user with peer verification (§3.1); agent subprocess env is allowlisted, never inherited wholesale; external linters get no network guarantee only via documentation in v1 (true sandboxing arrives with the WASM tier); logs scrub `Authorization`-shaped strings; `stacksaw` never phones home.

## 14. Testing & QA

- **UI:** ratatui `TestBackend` + `insta` golden frames for every column state, both backgrounds, 80×24 and 220×60, deck mode, `--ascii`.
- **Rainbox:** property tests (§8.3 AC).
- **Protocol:** SSP conformance client (§5.3 AC); fuzz the framing codec.
- **Git engine:** fixture repos generated by `xtask` (linear stack, 8-step staircase, forked segment tree, twins, mid-rebase state, pre-2.38 git in a container for the fallback path).
- **Linters:** corpus per built-in; ktfqn matrix (§7.5 AC); query-compile check pinned to the grammar.
- **Agents:** fake-ACP-agent integration (§9.5 AC); adapter contract test runs the Antigravity adapter against the fake harness in CI (no API key needed).
- **Inbound CLI:** `assert_cmd` golden JSON, schema validation of every payload against `stacksaw schema`.

## 15. Milestones

- **M0** — workspace, discovery/daemon lifecycle, snapshot model, `ls/status/show --output=json`, name-collision sweep.
- **M1** — read-only TUI: columns, collapse/resize/zoom, mouse, Rainbox, staircase view, diff pane. *First demo.*
- **M2** — linter framework + commitmsg/copyright/ktfqn, findings inline, `lint`/`fix`, caching.
- **M3** — SSP hardening, multi-window, watch stream, edit sessions, checkpoints/undo.
- **M4** — outbound ACP: agent config/trust, review workflow, restack state machine, Antigravity adapter published.
- **M5** — interdiff, review notes/export, deck mode polish, perf budgets green in CI. **v0.1 release.**
- **Stretch** — WASM linter tier; `--gui` wgpu renderer spike.

## 16. Non-goals (v1)

Direct Gerrit/GitHub/GitLab API integration (export seams only); replacing git porcelain generally; hosting anything; Windows-first polish (build must pass; UX parity may lag); MCP server mode (inbound is CLI by design — revisit only if demand proves out).

## 17. Open questions

1. The Antigravity SDK is pre-1.0 (0.1.x); the adapter pins an exact version and CI canaries the next one.
2. ACP extension-method payloads for `_stacksaw/*` need a versioned mini-schema of their own.
3. Whether `ui/link` selection sync is worth shipping in v1 or deferring to M5 feedback.
4. redb single-writer semantics vs. multi-process no-daemon mode: no-daemon runs read the cache read-only and skip writes on lock contention — confirm this is acceptable for CI ergonomics.

---

## Appendix A — Antigravity adapter (`stacksaw-antigravity-adapter`)

A pip/uv-installable Python package bridging **ACP (client side: stacksaw) ↔ Google Antigravity SDK (agent side)**. Both halves are existing libraries, so the adapter is mostly glue:

- `agent-client-protocol` (PyPI) — official ACP Python SDK: pydantic models, stdio JSON-RPC transport, async agent base class.
- `google-antigravity` (PyPI) — Antigravity SDK: `Agent`/`LocalAgentConfig` async context manager, streamed tokens/`thoughts`/`tool_calls`, custom Python tools, capability/policy gating (read-only by default; writes enabled explicitly).

```python
#!/usr/bin/env python3
"""ACP agent fronting Google Antigravity for stacksaw workflows."""
import asyncio, json, subprocess
import acp                                    # pip install agent-client-protocol
from google.antigravity import Agent, LocalAgentConfig, CapabilitiesConfig

def _stacksaw(*argv: str) -> str:
    """Expose stacksaw's inbound CLI (§10) to the model as a tool."""
    out = subprocess.run(["stacksaw", *argv, "--output=json"],
                         capture_output=True, text=True, timeout=120)
    return out.stdout or out.stderr

def stacksaw_lint(commit: str) -> str:
    """Run stacksaw lint on a commit; returns findings JSON."""
    return _stacksaw("lint", "--commit", commit, "--profile", "upload")

def stacksaw_ls() -> str:
    """Describe the current staircases as JSON."""
    return _stacksaw("ls")

class AntigravityAcpAgent(acp.Agent):          # names per acp SDK's agent base
    async def new_session(self, params):
        cfg = LocalAgentConfig(
            system_instructions=WORKFLOW_PERSONA,          # restack/review persona
            tools=[stacksaw_lint, stacksaw_ls],
            capabilities=CapabilitiesConfig(),             # enable writes: rebase edits
        )
        self._ag = await Agent(cfg).__aenter__()           # held for session lifetime
        return self._make_session_id()

    async def prompt(self, params):
        text = acp.helpers.text_of(params)                 # incl. _stacksaw/workflowContext
        resp = await self._ag.chat(text)
        async for thought in resp.thoughts:                # → ACP thought-chunk updates
            await self.session_update(params.session_id, acp.helpers.thought(thought))
        async for call in resp.tool_calls:                 # → ACP tool-call updates;
            await self.session_update(params.session_id,   #   Antigravity HITL policy →
                                      acp.helpers.tool_call(call))  # ACP request_permission
        await self.session_update(params.session_id,
                                  acp.helpers.agent_text(await resp.text()))
        return acp.helpers.end_turn()

if __name__ == "__main__":
    asyncio.run(acp.stdio_serve(AntigravityAcpAgent()))
```

*Skeleton, not gospel:* exact symbol names for the session base class, helper builders, and the Antigravity permission-hook → ACP `session/request_permission` mapping MUST be pinned against the locked versions of both SDKs at implementation time (both are young; the ACP SDK tracks the spec schema, the Antigravity SDK is 0.1.x). The load-bearing ideas are stable: stdio ACP on one side, `Agent`/`LocalAgentConfig` with streamed thoughts/tool-calls on the other, and **stacksaw's own CLI registered as Antigravity tools** — the agent inspects and lints through the same JSON surface every other client uses.

## Appendix B — SSP wire sample

```
Content-Length: 133

{"jsonrpc":"2.0","id":7,"method":"lint/run","params":{"scope":{"stair":"feat/wire-proto"},"linters":["ktfqn","commitmsg"]}}
Content-Length: 78

{"jsonrpc":"2.0","id":7,"result":{"runId":"r42","scheduled":6}}
Content-Length: 214

{"jsonrpc":"2.0","method":"lint/didFinish","params":{"runId":"r42","findings":[{"schemaVersion":1,"source":"linter:ktfqn","code":"ktfqn/avoid-fqn","severity":"warning","commit":"8c1f…","message":"Use an import for …"}]}}
```

## Appendix C — Default keybindings (excerpt)

`1..5` focus/expand column · `Tab`/`h l` columns · `j k` rows · `space` range-select · `enter` drill in · `z` zoom · `s` split/unified · `I` interdiff · `n p` findings · `a` apply fix · `c` comment · `R` restack · `L` lint stair · `u` undo · `:` palette · `?` help · `q` quit (with dirty-agent-session guard).