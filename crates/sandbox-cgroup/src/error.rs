use thiserror::Error;

#[derive(Debug, Error)]
pub enum CgroupError {
    #[error("cgroup v2 不可用: {0}")]
    Unavailable(String),

    #[error("cgroup 创建失败: {0}")]
    CreateFailed(String),

    #[error("进程迁移失败: {0}")]
    MigrateFailed(String),

    #[error("资源写入失败: {0}")]
    ResourceWrite(String),

    #[error("读取失败: {0}")]
    ReadFailed(String),

    #[error("销毁失败: {0}")]
    DestroyFailed(String),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}
