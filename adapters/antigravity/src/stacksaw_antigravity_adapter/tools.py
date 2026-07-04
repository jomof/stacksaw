"""stacksaw's inbound CLI (§10) exposed to the model as Antigravity tools.

Every tool shells out to the same ``stacksaw`` binary with ``--output=json``,
so the agent, the human, and CI judge success identically (§9.5).
"""

from __future__ import annotations

import subprocess


def _stacksaw(*argv: str) -> str:
    """Invoke the stacksaw inbound CLI and return its JSON output."""
    out = subprocess.run(
        ["stacksaw", *argv, "--output=json"],
        capture_output=True,
        text=True,
        timeout=120,
    )
    return out.stdout or out.stderr


def stacksaw_lint(commit: str) -> str:
    """Run stacksaw lint on a commit; returns findings JSON."""
    return _stacksaw("lint", "--commit", commit, "--profile", "upload")


def stacksaw_ls() -> str:
    """Describe the current staircases as JSON."""
    return _stacksaw("ls")


def stacksaw_show(rev: str) -> str:
    """Show a commit with trailers and findings as JSON."""
    return _stacksaw("show", rev)


def stacksaw_diff(rng: str = "") -> str:
    """Show a diff for an optional range as JSON."""
    args = ["diff"]
    if rng:
        args.append(rng)
    return _stacksaw(*args)
