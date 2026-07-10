//! `stacksaw-core` — the per-repository core service (§3). Owns git state,
//! filesystem watching, lint scheduling and agent sessions, and serves the SSP
//! protocol to all UI/CLI clients (P2 one source of truth).

pub mod client;
pub mod config;
pub mod core;
pub mod daemon;
pub mod discovery;
pub mod prober;
pub mod recent;
pub mod server;
pub mod service;
pub mod watch;

pub use client::SspClient;
pub use config::{Config, Provenance};
pub use core::Core;
pub use discovery::DaemonInfo;
pub use service::{build_lint_jobs, ChangeEvent, Service};
