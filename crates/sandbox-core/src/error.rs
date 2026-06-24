use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("sandbox init failed: {0}")]
    SandboxInit(String),

    #[error("process error: {0}")]
    Process(String),

    #[error("workspace error: {0}")]
    Workspace(String),

    #[error("env build error: {0}")]
    Env(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("timeout")]
    Timeout,

    #[error("capacity exceeded")]
    NoCapacity,

    #[error("profile not found: {0}")]
    ProfileNotFound(String),

    #[error("Landlock error: {0}")]
    Landlock(#[from] sandbox_landlock::LandlockError),

    #[error("seccomp error: {0}")]
    Seccomp(#[from] sandbox_seccomp::SeccompError),

    #[error("cgroup error: {0}")]
    Cgroup(#[from] sandbox_cgroup::CgroupError),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("syscall error: {0}")]
    Syscall(#[from] nix::errno::Errno),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
