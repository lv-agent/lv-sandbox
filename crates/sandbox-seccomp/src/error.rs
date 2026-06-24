use thiserror::Error;

#[derive(Debug, Error)]
pub enum SeccompError {
    #[error("seccomp unavailable: {0}")]
    Unavailable(String),

    #[error("BPF filter creation failed: {0}")]
    FilterCreate(String),

    #[error("failed to add rule: {0}")]
    RuleAdd(String),

    #[error("filter load failed: {0}")]
    Load(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
