use thiserror::Error;

#[derive(Debug, Error)]
pub enum LandlockError {
    #[error("Landlock 不可用: {0}")]
    Unavailable(String),

    #[error("ABI 版本不满足要求: 需要 {required}, 实际 {actual}")]
    AbiTooLow { required: u32, actual: u32 },

    #[error("Ruleset 创建失败: {0}")]
    RulesetCreate(String),

    #[error("规则添加失败: {0}")]
    RuleAdd(String),

    #[error("restrict_self 失败: {0}")]
    RestrictSelf(String),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}
