use anyhow::Result;
use async_trait::async_trait;
use stacksaw_lint::Profile;
use stacksaw_ssp::types::{
    ChangeView, CommitDetail, CommitRecord, EditBegin, EditFinish, Finding, MutatePlan,
    MutateResult, ReviewNote, Snapshot,
};
use tokio::sync::broadcast;

use crate::service::ChangeEvent;

#[async_trait]
pub trait RepositoryHandle: Send + Sync {
    async fn generation(&self) -> u64;
    async fn subscribe(&self) -> broadcast::Receiver<ChangeEvent>;
    async fn snapshot(&self) -> Result<Snapshot>;
    async fn commit_detail(&self, oid: &str) -> Result<CommitDetail>;
    async fn commit_show(&self, rev: &str) -> Result<CommitRecord>;
    async fn change_view(&self, commit: &str, path: &str) -> Result<ChangeView>;
    async fn diff_range(&self, args: &[String]) -> Result<String>;
    async fn diff_interdiff(&self, a: &str, b: &str) -> Result<String>;
    async fn mutate(&self, plan: MutatePlan, if_generation: Option<u64>) -> Result<MutateResult>;
    async fn undo(&self, checkpoint: Option<&str>) -> Result<MutateResult>;
    async fn checkpoints_list(&self) -> Result<Vec<String>>;
    async fn worktree_dirty(&self) -> Result<bool>;
    async fn current_branch(&self) -> Result<Option<String>>;
    async fn note_add(&self, file: &str, line: u32, text: &str) -> Result<ReviewNote>;
    async fn note_list(&self) -> Result<Vec<ReviewNote>>;
    async fn lint(&self, commits: Vec<String>, profile: Profile) -> Result<Vec<Finding>>;
    async fn edit_begin(&self, commit: &str) -> Result<EditBegin>;
    async fn edit_finish(&self, token: &str, message: Option<&str>) -> Result<EditFinish>;
    async fn edit_abort(&self, token: &str) -> Result<()>;
    fn drain_prober(&self) -> bool;
}
