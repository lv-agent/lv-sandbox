use serde::Serialize;

/// 汇总所有沙箱机制的能力状态
#[derive(Debug, Clone, Serialize)]
pub struct CapabilityReport {
    pub landlock: sandbox_landlock::LandlockCapabilities,
    pub cgroup: sandbox_cgroup::CgroupAvailability,
    pub seccomp_available: bool,
}

impl CapabilityReport {
    /// 检测所有能力。runner 启动时调用一次。
    pub fn detect() -> Self {
        Self {
            landlock: sandbox_landlock::detect_capabilities(),
            cgroup: sandbox_cgroup::detect(),
            seccomp_available: detect_seccomp(),
        }
    }
}

/// 检测内核 seccomp 支持
///
/// 尝试创建一个 ScmpFilterContext（不实际加载）。
/// 如果 libseccomp 可以初始化 filter，说明内核支持 seccomp。
fn detect_seccomp() -> bool {
    libseccomp::ScmpFilterContext::new_filter(libseccomp::ScmpAction::Allow).is_ok()
}
