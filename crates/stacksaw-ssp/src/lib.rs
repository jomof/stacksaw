//! `stacksaw-ssp` — the Stacksaw Session Protocol.
//!
//! JSON-RPC 2.0 messages framed exactly like LSP/DAP with `Content-Length`
//! headers (see spec §5.1). This crate is shared by both the core server and
//! its clients (UI / CLI). It intentionally avoids any external JSON-RPC
//! framework; the codec is small and must be exact.

pub mod client;
pub mod codec;
pub mod git_ref;
pub mod message;
pub mod method;
pub mod types;

pub use codec::{CodecError, ContentLengthCodec};
pub use message::{ErrorCode, Message, Notification, Request, RequestId, Response, ResponseError};

/// The wire protocol version advertised in `initialize`. Clients and the core
/// negotiate on the major component; incompatible majors are rejected (§5.2).
pub const PROTOCOL_VERSION: &str = "1.0";

/// Returns true when a client protocol version is compatible with ours.
///
/// Compatibility is defined on the major version only: additive minor/patch
/// evolution never breaks a peer, but a major bump does (§5.2).
pub fn is_compatible(peer: &str) -> bool {
    fn major(v: &str) -> Option<u64> {
        v.split('.').next()?.parse().ok()
    }
    match (major(peer), major(PROTOCOL_VERSION)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibility_is_major_only() {
        assert!(is_compatible("1.0"));
        assert!(is_compatible("1.7"));
        assert!(is_compatible("1.99.3"));
        assert!(!is_compatible("2.0"));
        assert!(!is_compatible("0.9"));
        assert!(!is_compatible("garbage"));
    }
}
