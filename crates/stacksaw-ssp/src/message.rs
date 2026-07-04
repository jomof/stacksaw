//! JSON-RPC 2.0 message model (§5.1). Batch messages are intentionally
//! unsupported — the protocol is batch-free.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON-RPC request identifier. Per the spec it may be a string or a number.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
}

impl From<i64> for RequestId {
    fn from(v: i64) -> Self {
        RequestId::Number(v)
    }
}

impl From<String> for RequestId {
    fn from(v: String) -> Self {
        RequestId::String(v)
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestId::Number(n) => write!(f, "{n}"),
            RequestId::String(s) => write!(f, "{s}"),
        }
    }
}

/// Any framed message on the wire. Serde's untagged deref distinguishes the
/// three shapes by presence of `id` and `method`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    Request(Request),
    Response(Response),
    Notification(Notification),
}

impl Message {
    pub fn as_request(&self) -> Option<&Request> {
        match self {
            Message::Request(r) => Some(r),
            _ => None,
        }
    }
}

/// A method call expecting a response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: JsonRpcVersion,
    pub id: RequestId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    pub fn new(id: impl Into<RequestId>, method: impl Into<String>, params: Option<Value>) -> Self {
        Request {
            jsonrpc: JsonRpcVersion,
            id: id.into(),
            method: method.into(),
            params,
        }
    }
}

/// A fire-and-forget message with no `id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: JsonRpcVersion,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Notification {
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Notification {
            jsonrpc: JsonRpcVersion,
            method: method.into(),
            params,
        }
    }
}

/// A response to a [`Request`]. Exactly one of `result`/`error` is present.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: JsonRpcVersion,
    pub id: RequestId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

impl Response {
    pub fn ok(id: RequestId, result: Value) -> Self {
        Response {
            jsonrpc: JsonRpcVersion,
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: RequestId, error: ResponseError) -> Self {
        Response {
            jsonrpc: JsonRpcVersion,
            id,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ResponseError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        ResponseError {
            code: code as i64,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// Standard and stacksaw-specific JSON-RPC error codes (§5.2, §10 exit codes
/// map loosely to these on the CLI seam).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    ParseError = -32700,
    InvalidRequest = -32600,
    MethodNotFound = -32601,
    InvalidParams = -32602,
    InternalError = -32603,
    /// Client/core protocol major mismatch during `initialize`.
    IncompatibleVersion = -32000,
    /// Optimistic-concurrency failure: `ifGeneration` did not match.
    GenerationConflict = -32001,
    /// Request was cancelled via `$/cancelRequest`.
    RequestCancelled = -32002,
    /// A mutation was refused by agent policy (§9.3).
    PolicyDenied = -32003,
    /// The mutation lock could not be acquired in time.
    LockTimeout = -32004,
}

/// A zero-sized marker that (de)serializes as the literal string `"2.0"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JsonRpcVersion;

impl Serialize for JsonRpcVersion {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("2.0")
    }
}

impl<'de> Deserialize<'de> for JsonRpcVersion {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = String::deserialize(d)?;
        if v == "2.0" {
            Ok(JsonRpcVersion)
        } else {
            Err(serde::de::Error::custom(format!(
                "unsupported jsonrpc version {v:?}, expected \"2.0\""
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_roundtrips() {
        let req = Request::new(7, "lint/run", Some(json!({"scope": {"stair": "feat"}})));
        let text = serde_json::to_string(&req).unwrap();
        assert!(text.contains("\"jsonrpc\":\"2.0\""));
        let back: Message = serde_json::from_str(&text).unwrap();
        assert_eq!(back.as_request().unwrap().method, "lint/run");
    }

    #[test]
    fn response_omits_null_result() {
        let resp = Response::err(
            RequestId::Number(1),
            ResponseError::new(ErrorCode::MethodNotFound, "nope"),
        );
        let text = serde_json::to_string(&resp).unwrap();
        assert!(!text.contains("result"));
        assert!(text.contains("-32601"));
    }

    #[test]
    fn rejects_bad_version() {
        let bad = r#"{"jsonrpc":"1.0","id":1,"method":"x"}"#;
        assert!(serde_json::from_str::<Request>(bad).is_err());
    }
}
