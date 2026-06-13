use thiserror::Error;

#[derive(Debug, Error)]
pub enum SeccompError {
    #[error("seccomp 不可用: {0}")]
    Unavailable(String),

    #[error("BPF filter 创建失败: {0}")]
    FilterCreate(String),

    #[error("规则添加失败: {0}")]
    RuleAdd(String),

    #[error("filter 加载失败: {0}")]
    Load(String),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}
