use std::path::Path;

use landlock::{
    Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreated,
    RulesetCreatedAttr, ABI,
};

use crate::access::AccessFs as OurAccessFs;
use crate::error::LandlockError;
use crate::policy::FsPolicy;
use crate::LandlockCapabilities;

/// 预编译的 Landlock ruleset。
/// 在 fork 前构建（可分配内存），在 pre_exec 闭包中调用 apply()（只做 syscall）。
#[derive(Debug)]
pub struct PreparedRuleset {
    inner: Option<RulesetCreated>,
}

impl PreparedRuleset {
    /// 从 FsPolicy 构建预编译 ruleset。
    /// 此方法会进行内存分配，必须在 fork 前调用。
    pub fn prepare(
        policy: &FsPolicy,
        caps: &LandlockCapabilities,
    ) -> Result<Self, LandlockError> {
        if !caps.supported || !caps.fs_access {
            return Err(LandlockError::Unavailable(
                "内核不支持 Landlock 或不支持文件系统访问控制".into(),
            ));
        }

        let abi = abi_from_version(caps.abi_version)?;

        // 获取当前 ABI 对应的完整文件系统访问权限集
        let all_access = AccessFs::from_all(abi);

        // 创建 ruleset：处理所有已知的文件系统访问
        let mut created = Ruleset::default()
            .handle_access(all_access)
            .map_err(|e| LandlockError::RulesetCreate(format!("{e}")))?
            .create()
            .map_err(|e| LandlockError::RulesetCreate(format!("{e}")))?;

        // 添加每条策略规则
        for rule in policy.rules() {
            let access = our_access_to_landlock(rule.access, abi);

            // 路径必须存在才能创建 PathFd
            let path = Path::new(&rule.path);
            if !path.exists() {
                tracing::debug!("跳过不存在的路径: {:?}", path);
                continue;
            }

            let path_fd = match PathFd::new(path) {
                Ok(fd) => fd,
                Err(e) => {
                    tracing::debug!("跳过无法打开的路径 {:?}: {}", path, e);
                    continue;
                }
            };

            created = created
                .add_rule(PathBeneath::new(path_fd, access))
                .map_err(|e| LandlockError::RuleAdd(format!("{e}")))?;
        }

        Ok(Self { inner: Some(created) })
    }

    /// 应用 Landlock 策略到当前进程。
    /// 必须在 pre_exec 闭包中调用（fork 后、exec 前）。
    /// 内部 `take()` 出 RulesetCreated 并消耗它调用 `restrict_self()`。
    pub fn apply(&self) -> Result<(), LandlockError> {
        let created = self
            .inner
            .as_ref()
            .ok_or_else(|| LandlockError::RestrictSelf("ruleset 已被消费".into()))?;

        // restrict_self 消费 RulesetCreated，但我们需要 &self 签名
        // 所以用 try_clone() 获取独立副本
        let cloned = created
            .try_clone()
            .map_err(|e| LandlockError::RestrictSelf(format!("clone 失败: {e}")))?;

        cloned
            .restrict_self()
            .map_err(|e| LandlockError::RestrictSelf(format!("{e}")))?;
        Ok(())
    }
}

/// 将我们的 AccessFs 映射到 landlock crate 的 AccessFs 权限位
fn our_access_to_landlock(access: OurAccessFs, abi: ABI) -> landlock::BitFlags<AccessFs> {
    match access {
        OurAccessFs::ReadWriteExecute => AccessFs::from_all(abi),
        OurAccessFs::ReadExecute => {
            // 读取 + 执行 + 目录遍历
            AccessFs::from_read(abi) | AccessFs::Execute
        }
        OurAccessFs::ReadOnly => AccessFs::from_read(abi),
        OurAccessFs::ReadWrite => AccessFs::from_read(abi) | AccessFs::from_write(abi),
    }
}

/// 从数值 ABI 版本构建 ABI 枚举
fn abi_from_version(v: u32) -> Result<ABI, LandlockError> {
    match v {
        0 => Err(LandlockError::AbiTooLow {
            required: 1,
            actual: 0,
        }),
        1 => Ok(ABI::V1),
        2 => Ok(ABI::V2),
        3 => Ok(ABI::V3),
        4 => Ok(ABI::V4),
        5 => Ok(ABI::V5),
        6 => Ok(ABI::V6),
        _ => {
            tracing::warn!("未知的 Landlock ABI 版本 {}，使用 V7 作为最佳近似", v);
            Ok(ABI::V7)
        }
    }
}
