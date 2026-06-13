use crate::profile::{CompareOperator, SeccompCondition};
use crate::syscall::Syscall;

// CLONE_NEW* 标志位
const CLONE_NEWNS: u64 = 0x00020000;
const CLONE_NEWUTS: u64 = 0x04000000;
const CLONE_NEWIPC: u64 = 0x08000000;
const CLONE_NEWUSER: u64 = 0x10000000;
const CLONE_NEWPID: u64 = 0x20000000;
const CLONE_NEWNET: u64 = 0x40000000;
const CLONE_NEWCGROUP: u64 = 0x02000000;

const CLONE_NEW_MASK: u64 =
    CLONE_NEWNS | CLONE_NEWUTS | CLONE_NEWIPC | CLONE_NEWUSER | CLONE_NEWPID | CLONE_NEWNET | CLONE_NEWCGROUP;

/// 返回 clone/clone3 的 namespace flag 过滤条件。
/// 如果 clone flags 参数中包含任何 CLONE_NEW* 标志位，则拒绝。
///
/// 用于 SeccompProfile::deny_with_conditions()。
pub fn clone_namespace_conditions() -> Vec<SeccompCondition> {
    vec![SeccompCondition {
        arg_index: 0, // clone 的第一个参数是 flags
        operator: CompareOperator::MaskedEqual,
        value: CLONE_NEW_MASK,
        mask: Some(CLONE_NEW_MASK),
    }]
}

/// 返回需要条件过滤 namespace flag 的 syscall 列表
pub fn clone_syscalls() -> Vec<Syscall> {
    vec![Syscall::Clone, Syscall::Clone3]
}
