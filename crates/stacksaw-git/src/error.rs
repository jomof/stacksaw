use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("could not discover a git repository at or above {0}")]
    NotARepo(PathBuf),
    #[error("git object database error: {0}")]
    Odb(String),
    #[error("reference error: {0}")]
    Reference(String),
    #[error("revision walk error: {0}")]
    Revwalk(String),
    #[error("could not resolve upstream for branch {0}")]
    NoUpstream(String),
    #[error("git command failed ({code}): {stderr}")]
    Command { code: i32, stderr: String },
    #[error("this operation requires git >= {required}, found {found}")]
    GitTooOld { required: String, found: String },
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, GitError>;
