//! LSP/DAP-style `Content-Length` framing (§5.1).
//!
//! ```text
//! Content-Length: <bytes>\r\n
//! \r\n
//! { "jsonrpc": "2.0", … }
//! ```
//!
//! Implemented as a [`tokio_util::codec`] `Encoder`/`Decoder` so it composes
//! with `Framed` over any `AsyncRead + AsyncWrite`. The decoder is tolerant of
//! extra headers (e.g. a `Content-Type`) but requires `Content-Length`.

use std::io;
use std::str;

use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use crate::message::Message;

/// Maximum single-message size we will buffer. Guards against a hostile or
/// buggy peer announcing an enormous `Content-Length`.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
/// Maximum size of the header block we will buffer before erroring.
pub const MAX_HEADER_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("malformed header: {0}")]
    Header(String),
    #[error("declared Content-Length {0} exceeds maximum {MAX_MESSAGE_BYTES}")]
    TooLarge(usize),
    #[error("json (de)serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Codec for [`Message`] values over a Content-Length framed byte stream.
#[derive(Debug, Default)]
pub struct ContentLengthCodec {
    /// Parsed content length for the message currently being assembled.
    expected: Option<usize>,
}

impl ContentLengthCodec {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attempts to parse the header block, returning the payload length and the
    /// number of header bytes consumed. Returns `Ok(None)` if the terminating
    /// blank line has not yet arrived.
    fn parse_headers(src: &[u8]) -> Result<Option<(usize, usize)>, CodecError> {
        let sep = b"\r\n\r\n";
        let limit = std::cmp::min(src.len(), MAX_HEADER_BYTES);
        let Some(end) = src[..limit]
            .windows(sep.len())
            .position(|w| w == sep)
            .map(|p| p + sep.len())
        else {
            if src.len() >= MAX_HEADER_BYTES {
                return Err(CodecError::Header("headers exceed maximum size".into()));
            }
            return Ok(None);
        };

        let header_text = str::from_utf8(&src[..end])
            .map_err(|_| CodecError::Header("headers are not valid utf-8".into()))?;

        let mut content_length: Option<usize> = None;
        for line in header_text.split("\r\n") {
            if line.is_empty() {
                continue;
            }
            let Some((name, value)) = line.split_once(':') else {
                return Err(CodecError::Header(format!(
                    "no colon in header line {line:?}"
                )));
            };
            if name.trim().eq_ignore_ascii_case("content-length") {
                let n: usize = value
                    .trim()
                    .parse()
                    .map_err(|_| CodecError::Header(format!("bad Content-Length {value:?}")))?;
                content_length = Some(n);
            }
        }

        match content_length {
            Some(n) if n > MAX_MESSAGE_BYTES => Err(CodecError::TooLarge(n)),
            Some(n) => Ok(Some((n, end))),
            None => Err(CodecError::Header("missing Content-Length header".into())),
        }
    }
}

impl Decoder for ContentLengthCodec {
    type Item = Message;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        loop {
            match self.expected {
                None => match Self::parse_headers(src)? {
                    None => return Ok(None),
                    Some((len, header_bytes)) => {
                        src.advance(header_bytes);
                        self.expected = Some(len);
                    }
                },
                Some(len) => {
                    if src.len() < len {
                        // Reserve so the read side can fill in one syscall.
                        src.reserve(len - src.len());
                        return Ok(None);
                    }
                    let payload = src.split_to(len);
                    self.expected = None;
                    let msg = serde_json::from_slice::<Message>(&payload)?;
                    return Ok(Some(msg));
                }
            }
        }
    }
}

impl Encoder<Message> for ContentLengthCodec {
    type Error = CodecError;

    fn encode(&mut self, item: Message, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let body = serde_json::to_vec(&item)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        dst.reserve(header.len() + body.len());
        dst.put_slice(header.as_bytes());
        dst.put_slice(&body);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Notification, Request};
    use serde_json::json;

    fn frame(msg: Message) -> BytesMut {
        let mut buf = BytesMut::new();
        ContentLengthCodec::new().encode(msg, &mut buf).unwrap();
        buf
    }

    #[test]
    fn encode_then_decode_roundtrips() {
        let msg = Message::Request(Request::new(7, "lint/run", Some(json!({"scope": "diff"}))));
        let mut buf = frame(msg);
        let out = ContentLengthCodec::new().decode(&mut buf).unwrap().unwrap();
        assert_eq!(out.as_request().unwrap().method, "lint/run");
        assert!(buf.is_empty(), "no trailing bytes left");
    }

    #[test]
    fn decodes_two_messages_in_one_buffer() {
        let mut buf = frame(Message::Notification(Notification::new("a", None)));
        buf.unsplit(frame(Message::Notification(Notification::new("b", None))));
        let mut codec = ContentLengthCodec::new();
        let m1 = codec.decode(&mut buf).unwrap().unwrap();
        let m2 = codec.decode(&mut buf).unwrap().unwrap();
        assert!(matches!(m1, Message::Notification(n) if n.method == "a"));
        assert!(matches!(m2, Message::Notification(n) if n.method == "b"));
        assert!(codec.decode(&mut buf).unwrap().is_none());
    }

    #[test]
    fn partial_input_yields_none() {
        let full = frame(Message::Notification(Notification::new("hello", None)));
        let mut codec = ContentLengthCodec::new();
        // Feed one byte at a time; only the final feed should yield a message.
        let mut acc = BytesMut::new();
        let mut produced = 0;
        for b in full.iter() {
            acc.put_u8(*b);
            if codec.decode(&mut acc).unwrap().is_some() {
                produced += 1;
            }
        }
        assert_eq!(produced, 1);
    }

    #[test]
    fn tolerates_extra_headers() {
        let body = br#"{"jsonrpc":"2.0","method":"ping"}"#;
        let mut buf = BytesMut::new();
        buf.put_slice(
            format!(
                "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        );
        buf.put_slice(body);
        let out = ContentLengthCodec::new().decode(&mut buf).unwrap().unwrap();
        assert!(matches!(out, Message::Notification(n) if n.method == "ping"));
    }

    #[test]
    fn rejects_oversize_length() {
        let mut buf = BytesMut::new();
        buf.put_slice(format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE_BYTES + 1).as_bytes());
        let err = ContentLengthCodec::new().decode(&mut buf).unwrap_err();
        assert!(matches!(err, CodecError::TooLarge(_)));
    }

    #[test]
    fn rejects_missing_length() {
        let mut buf = BytesMut::new();
        buf.put_slice(b"X-Foo: bar\r\n\r\n");
        assert!(ContentLengthCodec::new().decode(&mut buf).is_err());
    }
}
