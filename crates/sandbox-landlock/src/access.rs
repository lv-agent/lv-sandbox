use std::path::PathBuf;

/// 文件系统访问权限级别
#[derive(Debug, Clone, Copy)]
pub enum AccessFs {
    /// 读写 + 执行所需访问
    ReadWriteExecute,
    /// 只读 + 执行所需访问
    ReadExecute,
    /// 只读
    ReadOnly,
    /// 读写
    ReadWrite,
}

/// 单条访问规则
#[derive(Debug, Clone)]
pub struct AccessRule {
    pub path: PathBuf,
    pub access: AccessFs,
}

impl AccessRule {
    pub fn new(path: impl Into<PathBuf>, access: AccessFs) -> Self {
        Self {
            path: path.into(),
            access,
        }
    }
}
