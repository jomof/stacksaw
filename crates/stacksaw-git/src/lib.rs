//! `stacksaw-git` — gix-backed reads, the staircase model, ref transactions,
//! checkpoints and undo (§2, §4, §9.5).

pub mod edit;
pub mod error;
pub mod model;
pub mod refs;
pub mod repo;
pub mod snapshot;

pub use error::{GitError, Result};
pub use model::{build_staircases, ModelOptions};
pub use repo::{BranchRef, CommitMeta, Repo};
pub use snapshot::{build_snapshot, changed_files, file_content, file_diff};
