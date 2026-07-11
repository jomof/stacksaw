#![allow(unused_imports)]
use futures::{SinkExt, StreamExt};
use stacksaw_ssp::client::JsonRpcClient;
use stacksaw_ssp::message::Message;
use std::time::Duration;
use tokio::time::timeout;

#[tokio::test]
async fn test_client_hangs_when_reader_exits() {
    let (sink_tx, _sink_rx) = futures::channel::mpsc::unbounded::<Message>();
    let (stream_tx, stream_rx) = futures::channel::mpsc::unbounded::<Result<Message, String>>();

    let (client, _inbound) = JsonRpcClient::new(sink_tx, stream_rx);

    // Close the stream from the other side to force the reader task to exit.
    drop(stream_tx);
    
    // Give some time for the reader task to exit.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Now make a request. It should fail quickly because the connection is closed,
    // but due to the bug, it will hang.
    let request_fut = client.request("ping", None);
    
    match timeout(Duration::from_secs(1), request_fut).await {
        Ok(result) => {
            println!("Request finished with result: {:?}", result);
        }
        Err(_) => {
            panic!("BUG REPRODUCED: Request hung for 1 second!");
        }
    }
}
