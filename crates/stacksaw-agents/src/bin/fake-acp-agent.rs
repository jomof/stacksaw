//! A deterministic fake ACP agent for tests and CI (§9.5 AC, §14).
//!
//! Speaks newline-delimited JSON-RPC 2.0 on stdio. It implements just enough of
//! ACP to exercise the client: `initialize`, `session/new`, `session/prompt`.
//! On each prompt it emits a streamed `session/update` then ends the turn. If
//! the prompt mentions a ktfqn task, it announces a fix via a tool-call update,
//! mimicking an agent that "deterministically fixes a seeded violation".

use std::io::{BufRead, Write};

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg): Result<serde_json::Value, _> = serde_json::from_str(&line) else {
            continue;
        };
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = msg.get("id").cloned();

        match method {
            "initialize" => {
                respond(
                    &mut out,
                    id,
                    serde_json::json!({
                        "protocolVersion": 1,
                        "agentCapabilities": { "promptCapabilities": { "image": false } }
                    }),
                );
            }
            "session/new" => {
                respond(
                    &mut out,
                    id,
                    serde_json::json!({ "sessionId": "fake-session-1" }),
                );
            }
            "session/prompt" => {
                let session_id = msg
                    .get("params")
                    .and_then(|p| p.get("sessionId"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("fake-session-1")
                    .to_string();
                let text = msg
                    .get("params")
                    .and_then(|p| p.get("prompt"))
                    .map(|p| p.to_string())
                    .unwrap_or_default();

                // Stream a thought.
                notify(
                    &mut out,
                    "session/update",
                    serde_json::json!({
                        "sessionId": session_id,
                        "update": {
                            "sessionUpdate": "agent_thought_chunk",
                            "content": { "type": "text", "text": "Inspecting the failing step…" }
                        }
                    }),
                );

                if text.contains("ktfqn") {
                    notify(
                        &mut out,
                        "session/update",
                        serde_json::json!({
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "tool_call",
                                "toolCallId": "fix-1",
                                "title": "stacksaw fix --commit HEAD",
                                "status": "completed"
                            }
                        }),
                    );
                }

                notify(
                    &mut out,
                    "session/update",
                    serde_json::json!({
                        "sessionId": session_id,
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": { "type": "text", "text": "Done." }
                        }
                    }),
                );

                respond(&mut out, id, serde_json::json!({ "stopReason": "end_turn" }));
            }
            "" => { /* a response or notification from the client; ignore */ }
            _ => {
                if let Some(id) = id {
                    respond_error(&mut out, id, -32601, "method not found");
                }
            }
        }
    }
}

fn send(out: &mut impl Write, value: serde_json::Value) {
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

fn respond(out: &mut impl Write, id: Option<serde_json::Value>, result: serde_json::Value) {
    if let Some(id) = id {
        send(
            out,
            serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        );
    }
}

fn respond_error(out: &mut impl Write, id: serde_json::Value, code: i64, message: &str) {
    send(
        out,
        serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    );
}

fn notify(out: &mut impl Write, method: &str, params: serde_json::Value) {
    send(
        out,
        serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params }),
    );
}
