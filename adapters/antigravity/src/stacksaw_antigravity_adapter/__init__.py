"""ACP ↔ Google Antigravity adapter for stacksaw (Appendix A).

Bridges stacksaw (the ACP *client*) to the Google Antigravity SDK (the agent).
Both halves are existing libraries; this package is mostly glue. See
``__main__`` for the entry point and ``agent`` for the ACP agent implementation.
"""

__version__ = "0.1.0"

# stacksaw's inbound CLI (§10) is registered as Antigravity tools so the model
# inspects and lints through the same JSON surface every other client uses.
STACKSAW_TOOLS = ["stacksaw_lint", "stacksaw_ls", "stacksaw_show", "stacksaw_diff"]
