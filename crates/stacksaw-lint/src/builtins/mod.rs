//! Tier-1 built-in linters (§7.4): in-process, in Rust, needing no trust gate.

pub mod commitmsg;
pub mod copyright;
pub mod ktfqn;

pub use commitmsg::{CommitMsgConfig, CommitMsgLinter};
pub use copyright::{CopyrightConfig, CopyrightLinter};
pub use ktfqn::KtfqnLinter;
