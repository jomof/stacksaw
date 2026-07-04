"""Entry point: serve the ACP agent over stdio (Appendix A)."""

from __future__ import annotations

import asyncio


def main() -> None:
    import acp

    from .agent import AntigravityAcpAgent

    asyncio.run(acp.stdio_serve(AntigravityAcpAgent()))


if __name__ == "__main__":
    main()
