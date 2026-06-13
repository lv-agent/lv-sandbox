use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("沙箱初始化失败: {0}")]
    SandboxInit(String),

    #[error("进程错误: {0}")]
    Process(String),

    #[error("工作空间错误: {0}")]
    Workspace(String),

    #[error("环境构建错误: {0}")]
    Env(String),

    #[error("配置错误: {0}")]
    Config(String),

    #[error("超时")]
    Timeout,

    #[error("容量不足")]
    NoCapacity,

    #[error("Profile 未找到: {0}")]
    ProfileNotFound(String),

    #[error("Landlock 错误: {0}")]
    Landlock(#[from] sandbox_landlock::LandlockError),

    #[error("seccomp 错误: {0}")]
    Seccomp(#[from] sandbox_seccomp::SeccompError),

    #[error("cgroup 错误: {0}")]
    Cgroup(#[from] sandbox_cgroup::CgroupError),

    #[error("序列化错误: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("系统调用错误: {0}")]
    Syscall(#[from] nix::errno::Errno),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}
