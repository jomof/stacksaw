//! `stacksaw-git` — gix-backed reads, the staircase model, ref transactions,
//! checkpoints and undo (§2, §4, §9.5).

pub mod archive;
pub mod edit;
pub mod error;
pub mod executor;
pub mod model;
pub mod numstat;
pub mod rebase_probe;
pub mod refs;
pub mod repo;
pub mod reshape;
pub mod snapshot;

pub use error::{GitError, Result};
pub use model::{build_staircases, ModelOptions};
pub use rebase_probe::{probe_rebase, RebaseProbe};
pub use repo::{BranchRef, CommitMeta, Repo};
pub use snapshot::{
    annotate_rebase, build_snapshot, changed_files, commit_message, file_content, file_diff,
    rebase_probe_oids, restack_probe_oids,
};
