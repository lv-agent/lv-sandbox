use thiserror::Error;

#[derive(Debug, Error)]
pub enum CgroupError {
    #[error("cgroup v2 unavailable: {0}")]
    Unavailable(String),

    #[error("cgroup creation failed: {0}")]
    CreateFailed(String),

    #[error("process migration failed: {0}")]
    MigrateFailed(String),

    #[error("resource write failed: {0}")]
    ResourceWrite(String),

    #[error("read failed: {0}")]
    ReadFailed(String),

    #[error("destroy failed: {0}")]
    DestroyFailed(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
