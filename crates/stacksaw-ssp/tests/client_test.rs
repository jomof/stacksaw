use futures::{Sink, Stream};
use serde_json::json;
use stacksaw_ssp::client::{Incoming, JsonRpcClient};
use stacksaw_ssp::message::{Message, Notification, Request, RequestId, Response};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc;

struct MockTransport {
    sink_tx: mpsc::UnboundedSender<Message>,
    stream_rx: mpsc::UnboundedReceiver<Message>,
}

impl Sink<Message> for MockTransport {
    type Error = std::io::Error;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        self.sink_tx
            .send(item)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "closed"))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

impl Stream for MockTransport {
    type Item = Result<Message, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.stream_rx.poll_recv(cx).map(|opt| opt.map(Ok))
    }
}

#[tokio::test]
async fn test_request_response_correlation() {
    let (sink_tx, mut sink_rx) = mpsc::unbounded_channel();
    let (stream_tx, stream_rx) = mpsc::unbounded_channel();
    let transport = MockTransport { sink_tx, stream_rx };

    let (sink, stream) = futures::StreamExt::split(transport);
    let (client, _incoming) = JsonRpcClient::new(sink, stream);
    let client = std::sync::Arc::new(client);

    let client_clone = client.clone();
    let req_fut = tokio::spawn(async move {
        client_clone.request("test", Some(json!({"foo": "bar"}))).await
    });

    // Expect request on sink
    let msg = sink_rx.recv().await.unwrap();
    let req = match msg {
        Message::Request(r) => r,
        _ => panic!("Expected request"),
    };
    assert_eq!(req.method, "test");
    let id = req.id.clone();

    // Send response back
    let resp = Response::ok(id, json!({"result": "ok"}));
    stream_tx.send(Message::Response(resp)).unwrap();

    let res = req_fut.await.unwrap().unwrap();
    assert_eq!(res, json!({"result": "ok"}));
}

#[tokio::test]
async fn test_interleaved_requests() {
    let (sink_tx, mut sink_rx) = mpsc::unbounded_channel();
    let (stream_tx, stream_rx) = mpsc::unbounded_channel();
    let transport = MockTransport { sink_tx, stream_rx };

    let (sink, stream) = futures::StreamExt::split(transport);
    let (client, _incoming) = JsonRpcClient::new(sink, stream);
    let client = std::sync::Arc::new(client);

    let client1 = client.clone();
    let fut1 = tokio::spawn(async move { client1.request("req1", None).await });
    let client2 = client.clone();
    let fut2 = tokio::spawn(async move { client2.request("req2", None).await });

    let msg1 = sink_rx.recv().await.unwrap();
    let msg2 = sink_rx.recv().await.unwrap();

    let id1 = match msg1 {
        Message::Request(r) => r.id,
        _ => panic!(),
    };
    let id2 = match msg2 {
        Message::Request(r) => r.id,
        _ => panic!(),
    };

    // Respond in reverse order
    stream_tx
        .send(Message::Response(Response::ok(id2, json!(2))))
        .unwrap();
    stream_tx
        .send(Message::Response(Response::ok(id1, json!(1))))
        .unwrap();

    assert_eq!(fut1.await.unwrap().unwrap(), json!(1));
    assert_eq!(fut2.await.unwrap().unwrap(), json!(2));
}

#[tokio::test]
async fn test_notifications() {
    let (sink_tx, _sink_rx) = mpsc::unbounded_channel();
    let (stream_tx, stream_rx) = mpsc::unbounded_channel();
    let transport = MockTransport { sink_tx, stream_rx };

    let (sink, stream) = futures::StreamExt::split(transport);
    let (_client, mut incoming) = JsonRpcClient::new(sink, stream);

    stream_tx
        .send(Message::Notification(Notification::new(
            "event",
            Some(json!(42)),
        )))
        .unwrap();

    let msg = incoming.recv().await.unwrap();
    match msg {
        Incoming::Notification(n) => {
            assert_eq!(n.method, "event");
            assert_eq!(n.params.unwrap(), json!(42));
        }
        _ => panic!("Expected notification"),
    }
}

#[tokio::test]
async fn test_server_to_client_request() {
    let (sink_tx, mut sink_rx) = mpsc::unbounded_channel();
    let (stream_tx, stream_rx) = mpsc::unbounded_channel();
    let transport = MockTransport { sink_tx, stream_rx };

    let (sink, stream) = futures::StreamExt::split(transport);
    let (client, mut incoming) = JsonRpcClient::new(sink, stream);

    let server_req = Request::new(99, "ask", Some(json!("question")));
    stream_tx.send(Message::Request(server_req)).unwrap();

    let msg = incoming.recv().await.unwrap();
    let id = match msg {
        Incoming::Request(r) => {
            assert_eq!(r.method, "ask");
            r.id
        }
        _ => panic!("Expected request"),
    };

    client.respond(id, json!("answer")).unwrap();

    let msg = sink_rx.recv().await.unwrap();
    match msg {
        Message::Response(resp) => {
            assert_eq!(resp.id, RequestId::Number(99));
            assert_eq!(resp.result.unwrap(), json!("answer"));
        }
        _ => panic!("Expected response"),
    }
}
