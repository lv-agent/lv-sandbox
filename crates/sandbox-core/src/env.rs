use std::collections::HashMap;
use std::path::Path;

/// 永不可覆盖的隔离核心(HOME/TMPDIR 必须指 landlock 可写根)。
const PROTECTED: &[&str] = &["HOME", "TMPDIR"];

/// 从零构建白名单环境变量(不继承 runner 的任何环境变量)。
///
/// cr-025 三阶段优先级:
/// 1. 核心默认(PATH / HOME=home_root / TMPDIR=home_root/tmp / LANG)
/// 2. `profile_env`(template baseline,operator 可信):覆盖非保护项,可设 PATH/LANG/任意 key
/// 3. `extra`(request custom_env,agent 传):只加**新** key(非保护、未被前两阶段占用)
///
/// HOME/TMPDIR 永不被覆盖。
///
/// **`home_root` 契约**:必须是 landlock 可写根 —— 即 `for_job` 编译进 ruleset 的 workspace
/// 路径(`JobWorkspace::workspace`),**不是** session root。landlock 只对该 workspace 授
/// ReadWrite;若传 root,则 HOME/TMPDIR 落在不可写区,牢内任何写 `$HOME`/`$TMPDIR` 的程序
/// (git 的 pack/lock/gitconfig 等)必败。TMPDIR=`home_root/tmp` 须由 workspace 创建逻辑预建。
pub fn build_sanitized_env(
    _job_id: &str,
    home_root: &Path,
    profile_env: &HashMap<String, String>,
    extra: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut env = HashMap::new();

    // 1. 核心默认
    env.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
    env.insert(
        "HOME".to_string(),
        home_root.to_string_lossy().to_string(),
    );
    env.insert(
        "TMPDIR".to_string(),
        home_root.join("tmp").to_string_lossy().to_string(),
    );
    env.insert("LANG".to_string(), "C.UTF-8".to_string());

    // 2. profile.env(template baseline):覆盖非保护项,可加任意 key
    for (k, v) in profile_env {
        if !PROTECTED.contains(&k.as_str()) {
            env.insert(k.clone(), v.clone());
        }
    }

    // 3. request custom_env:只加新 key(非保护、未占用)
    for (k, v) in extra {
        if !PROTECTED.contains(&k.as_str()) && !env.contains_key(k.as_str()) {
            env.insert(k.clone(), v.clone());
        }
    }

    env
}
