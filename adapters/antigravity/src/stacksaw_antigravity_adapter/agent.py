"""The ACP agent that fronts Google Antigravity (Appendix A).

*Skeleton, not gospel:* the exact symbol names for the session base class,
helper builders, and the Antigravity permission-hook → ACP
``session/request_permission`` mapping MUST be pinned against the locked
versions of both SDKs at implementation time (§17.1). The load-bearing ideas
are stable: stdio ACP on one side, ``Agent``/``LocalAgentConfig`` with streamed
thoughts/tool-calls on the other, and stacksaw's own CLI registered as
Antigravity tools.
"""

from __future__ import annotations

import acp  # pip install agent-client-protocol
from google.antigravity import Agent, CapabilitiesConfig, LocalAgentConfig

from .tools import stacksaw_diff, stacksaw_lint, stacksaw_ls, stacksaw_show

WORKFLOW_PERSONA = """\
You are stacksaw's agent for restack/review workflows. You inspect and lint a
staircase of git commits through the stacksaw CLI tools, propose minimal fixes,
and — when driving a restack — resolve each stop (lint failure or conflict)
before signalling completion. Never push; never touch refs directly.
"""


class AntigravityAcpAgent(acp.Agent):  # names per the acp SDK's agent base
    async def new_session(self, params):
        cfg = LocalAgentConfig(
            system_instructions=WORKFLOW_PERSONA,
            tools=[stacksaw_lint, stacksaw_ls, stacksaw_show, stacksaw_diff],
            capabilities=CapabilitiesConfig(),  # enable writes: rebase edits
        )
        self._ag = await Agent(cfg).__aenter__()  # held for session lifetime
        return self._make_session_id()

    async def prompt(self, params):
        # Includes _stacksaw/workflowContext, degrading to embedded text (§9.1).
        text = acp.helpers.text_of(params)
        resp = await self._ag.chat(text)

        async for thought in resp.thoughts:  # → ACP thought-chunk updates
            await self.session_update(params.session_id, acp.helpers.thought(thought))

        async for call in resp.tool_calls:  # → ACP tool-call updates; the
            await self.session_update(  # Antigravity HITL policy maps to
                params.session_id, acp.helpers.tool_call(call)  # ACP permission
            )

        await self.session_update(
            params.session_id, acp.helpers.agent_text(await resp.text())
        )
        return acp.helpers.end_turn()
