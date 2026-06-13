use serde::Serialize;

/// Landlock ABI 运行时能力检测结果
#[derive(Debug, Clone, Serialize)]
pub struct LandlockCapabilities {
    /// Landlock 是否可用
    pub supported: bool,
    /// ABI 版本号
    pub abi_version: u32,
    /// 文件系统访问控制
    pub fs_access: bool,
    /// TCP 端口网络控制
    pub network_tcp_port: bool,
    /// Socket 网络控制
    pub network_socket: bool,
    /// 信号控制
    pub signal_control: bool,
}

/// 检测当前内核的 Landlock ABI 能力
///
/// 使用 `landlock` crate 的 `landlock_create_ruleset` syscall 查询 ABI 版本。
/// 结果在 runner 启动时获取一次并缓存。
pub fn detect() -> LandlockCapabilities {
    // landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)
    // 成功时返回 ABI 版本号，失败时返回 -1
    let abi_version = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0usize,
            1u32, // LANDLOCK_CREATE_RULESET_VERSION
        )
    };

    if abi_version < 0 {
        return LandlockCapabilities {
            supported: false,
            abi_version: 0,
            fs_access: false,
            network_tcp_port: false,
            network_socket: false,
            signal_control: false,
        };
    }

    let abi_v = abi_version as u32;
    LandlockCapabilities {
        supported: true,
        abi_version: abi_v,
        fs_access: abi_v >= 1,
        network_tcp_port: abi_v >= 4,
        network_socket: abi_v >= 5,
        signal_control: abi_v >= 6,
    }
}
