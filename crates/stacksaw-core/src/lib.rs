//! `stacksaw-core` — the per-repository core service (§3). Owns git state,
//! filesystem watching, and per-repository semantic queries.

pub mod config;
pub mod core;
pub mod prober;
pub mod recent;
pub mod service;
pub mod watch;

pub use config::{Config, Provenance};
pub use core::Core;
pub use service::{ChangeEvent, Service};
