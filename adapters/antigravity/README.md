# stacksaw-antigravity-adapter

A pip/uv-installable ACP adapter bridging **stacksaw (ACP client) ↔ Google
Antigravity (agent)** (spec Appendix A).

## Install

```console
uv tool install stacksaw-antigravity-adapter
# or
pipx install stacksaw-antigravity-adapter
```

## Configure stacksaw to drive it

```toml
# ~/.config/stacksaw/agents/antigravity.toml
[agents.antigravity]
command   = "uvx"
args      = ["stacksaw-antigravity-adapter"]
protocol  = "acp"
workflows = ["restack", "review"]
env       = { GEMINI_API_KEY = "${env:GEMINI_API_KEY}" }
```

Then:

```console
stacksaw agent list
stacksaw restack --agent antigravity --fix-lints
```

## Contract

- **stdio ACP** on the stacksaw side (`agent-client-protocol`).
- **`Agent`/`LocalAgentConfig`** with streamed thoughts/tool-calls on the
  Antigravity side (`google-antigravity`).
- stacksaw's own inbound CLI (`stacksaw lint|ls|show|diff --output=json`) is
  registered as Antigravity tools, so the agent inspects the stack through the
  same JSON surface every other client uses.

> Both SDKs are young (§17.1). Pin exact versions in your lockfile; CI canaries
> the next Antigravity release.
