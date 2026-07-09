//! Drives the ACP client against the bundled fake agent (§9.5 AC / §14).

use stacksaw_agents::acp::AcpClient;
use stacksaw_agents::Incoming;

use std::env;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialize_new_session_prompt_roundtrip() {
    let agent = env!("CARGO_BIN_EXE_fake-acp-agent");
    let cwd = env::temp_dir();

    let mut client = AcpClient::spawn(agent, &[], &[], &cwd)
        .await
        .expect("spawn fake agent");

    let init = client.initialize().await.expect("initialize");
    assert_eq!(init.protocol_version, 1);

    let session = client.new_session(&cwd).await.expect("new session");
    assert_eq!(session, "fake-session-1");

    // Prompt referencing a ktfqn task; expect streamed updates + end_turn.
    let stop = client
        .prompt(&session, "Fix the ktfqn violation at step 2")
        .await
        .expect("prompt");
    assert_eq!(stop, "end_turn");

    // Drain the streamed session updates that arrived during the turn.
    let mut saw_tool_call = false;
    while let Ok(incoming) = client.incoming.try_recv() {
        if let Incoming::Notification(n) = incoming {
            if n.method == "session/update" {
                let text = serde_json::to_string(&n.params).unwrap();
                if text.contains("tool_call") {
                    saw_tool_call = true;
                }
            }
        }
    }
    assert!(saw_tool_call, "agent should have announced a fix tool-call");

    client.shutdown().await;
}
