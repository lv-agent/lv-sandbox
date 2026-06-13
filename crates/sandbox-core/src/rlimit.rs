use serde::{Deserialize, Serialize};

/// rlimit 资源限制配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RlimitConfig {
    /// RLIMIT_CPU: CPU 时间（秒）
    pub cpu_seconds: Option<u64>,
    /// RLIMIT_NOFILE: 最大打开文件数
    pub nofile: Option<u64>,
    /// RLIMIT_NPROC: 最大进程数
    pub nproc: Option<u64>,
    /// RLIMIT_FSIZE: 单文件最大大小（字节）
    pub fsize_bytes: Option<u64>,
    /// RLIMIT_CORE: core 文件大小（0 = 禁用）
    pub core: Option<u64>,
    /// RLIMIT_STACK: 栈大小（字节）
    pub stack_bytes: Option<u64>,
    /// RLIMIT_MEMLOCK: 锁定内存上限（0 = 禁用）
    pub memlock: Option<u64>,
    /// RLIMIT_AS: 虚拟地址空间上限（字节，谨慎启用）
    pub address_space_bytes: Option<u64>,
}

impl RlimitConfig {
    pub fn new() -> Self {
        Self {
            cpu_seconds: None,
            nofile: None,
            nproc: None,
            fsize_bytes: None,
            core: None,
            stack_bytes: None,
            memlock: None,
            address_space_bytes: None,
        }
    }

    pub fn cpu_seconds(mut self, secs: u64) -> Self {
        self.cpu_seconds = Some(secs);
        self
    }

    pub fn nofile(mut self, limit: u64) -> Self {
        self.nofile = Some(limit);
        self
    }

    pub fn nproc(mut self, limit: u64) -> Self {
        self.nproc = Some(limit);
        self
    }

    pub fn fsize_mb(mut self, mb: u64) -> Self {
        self.fsize_bytes = Some(mb * 1024 * 1024);
        self
    }

    pub fn core_disabled(mut self) -> Self {
        self.core = Some(0);
        self
    }

    pub fn core_disabled_with(mut self, value: u64) -> Self {
        self.core = Some(value);
        self
    }

    pub fn stack_mb(mut self, mb: u64) -> Self {
        self.stack_bytes = Some(mb * 1024 * 1024);
        self
    }

    pub fn memlock_disabled(mut self) -> Self {
        self.memlock = Some(0);
        self
    }

    pub fn memlock_with(mut self, value: u64) -> Self {
        self.memlock = Some(value);
        self
    }

    pub fn address_space_gb(mut self, gb: u64) -> Self {
        self.address_space_bytes = Some(gb * 1024 * 1024 * 1024);
        self
    }

    /// 应用所有 rlimit 到当前进程。
    /// 在 pre_exec 闭包中调用。只执行 setrlimit syscall，不进行内存分配。
    pub fn apply(&self) -> Result<(), crate::error::CoreError> {
        use nix::sys::resource::{setrlimit, Resource};

        if let Some(v) = self.cpu_seconds {
            setrlimit(Resource::RLIMIT_CPU, v, v)?;
        }
        if let Some(v) = self.fsize_bytes {
            setrlimit(Resource::RLIMIT_FSIZE, v, v)?;
        }
        if let Some(v) = self.nofile {
            setrlimit(Resource::RLIMIT_NOFILE, v, v)?;
        }
        if let Some(v) = self.nproc {
            setrlimit(Resource::RLIMIT_NPROC, v, v)?;
        }
        if let Some(v) = self.core {
            setrlimit(Resource::RLIMIT_CORE, v, v)?;
        }
        if let Some(v) = self.stack_bytes {
            setrlimit(Resource::RLIMIT_STACK, v, v)?;
        }
        if let Some(v) = self.memlock {
            setrlimit(Resource::RLIMIT_MEMLOCK, v, v)?;
        }
        if let Some(v) = self.address_space_bytes {
            setrlimit(Resource::RLIMIT_AS, v, v)?;
        }

        Ok(())
    }
}

impl Default for RlimitConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// 预设 profile: 轻量 shell 任务
pub fn shell_default() -> RlimitConfig {
    RlimitConfig::new()
        .cpu_seconds(2)
        .nofile(64)
        .nproc(32)
        .fsize_mb(10)
        .core_disabled()
        .stack_mb(8)
        .memlock_disabled()
}

/// 预设 profile: Python 任务
pub fn python_default() -> RlimitConfig {
    RlimitConfig::new()
        .cpu_seconds(2)
        .nofile(64)
        .nproc(32)
        .fsize_mb(10)
        .core_disabled()
        .stack_mb(8)
        .memlock_disabled()
    // RLIMIT_AS 默认不启用，需要实测后配置
}

/// 预设 profile: Node.js 任务
pub fn node_default() -> RlimitConfig {
    RlimitConfig::new()
        .cpu_seconds(2)
        .nofile(64)
        .nproc(32)
        .fsize_mb(10)
        .core_disabled()
        .stack_mb(8)
        .memlock_disabled()
    // RLIMIT_AS 默认不启用，需要实测后配置
}
