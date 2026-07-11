use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use futures::{Sink, SinkExt, Stream, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::message::{Message, Notification, Request, RequestId, Response};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("connection closed")]
    Closed,
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("rpc error {code}: {message}")]
    Rpc { code: i64, message: String },
}

#[derive(Debug)]
pub enum Incoming {
    Notification(Notification),
    Request(Request),
}

pub struct JsonRpcClient {
    next_id: AtomicI64,
    pending: Arc<Mutex<HashMap<RequestId, oneshot::Sender<Response>>>>,
    outbound: mpsc::UnboundedSender<Message>,
}

impl JsonRpcClient {
    pub fn new<S, ST, E>(mut sink: S, mut stream: ST) -> (Self, mpsc::UnboundedReceiver<Incoming>)
    where
        S: Sink<Message> + Unpin + Send + 'static,
        ST: Stream<Item = Result<Message, E>> + Unpin + Send + 'static,
        E: std::fmt::Display + Send + 'static,
    {
        let pending: Arc<Mutex<HashMap<RequestId, oneshot::Sender<Response>>>> =
            Arc::new(Mutex::new(HashMap::default()));
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
        let (in_tx, in_rx) = mpsc::unbounded_channel::<Incoming>();

        let pending_reader = pending.clone();

        // Writer task
        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
        });

        // Reader task
        tokio::spawn(async move {
            while let Some(res) = stream.next().await {
                match res {
                    Ok(Message::Response(resp)) => {
                        if let Some(tx) = pending_reader.lock().await.remove(&resp.id) {
                            let _ = tx.send(resp);
                        }
                    }
                    Ok(Message::Request(req)) => {
                        let _ = in_tx.send(Incoming::Request(req));
                    }
                    Ok(Message::Notification(n)) => {
                        let _ = in_tx.send(Incoming::Notification(n));
                    }
                    Err(e) => {
                        eprintln!("JsonRpcClient reader error: {e}");
                        break;
                    }
                }
            }
        });

        (
            JsonRpcClient {
                next_id: AtomicI64::new(1),
                pending,
                outbound: out_tx,
            },
            in_rx,
        )
    }

    pub async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let id = RequestId::Number(id);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), tx);

        let req = Request::new(id, method, params);
        self.outbound
            .send(Message::Request(req))
            .map_err(|_| ClientError::Closed)?;

        let resp = rx.await.map_err(|_| ClientError::Closed)?;
        if let Some(err) = resp.error {
            return Err(ClientError::Rpc {
                code: err.code,
                message: err.message,
            });
        }
        Ok(resp.result.unwrap_or(Value::Null))
    }

    pub fn notify(&self, method: &str, params: Option<Value>) -> Result<(), ClientError> {
        let n = Notification::new(method, params);
        self.outbound
            .send(Message::Notification(n))
            .map_err(|_| ClientError::Closed)
    }

    pub fn respond(&self, id: RequestId, result: Value) -> Result<(), ClientError> {
        let resp = Response::ok(id, result);
        self.outbound
            .send(Message::Response(resp))
            .map_err(|_| ClientError::Closed)
    }
}
