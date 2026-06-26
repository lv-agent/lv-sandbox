use std::collections::HashMap;
use std::path::Path;

/// 永不可覆盖的隔离核心(HOME/TMPDIR 必须指工作区)。
const PROTECTED: &[&str] = &["HOME", "TMPDIR"];

/// 从零构建白名单环境变量(不继承 runner 的任何环境变量)。
///
/// cr-025 三阶段优先级:
/// 1. 核心默认(PATH / HOME=workspace / TMPDIR=workspace/tmp / LANG)
/// 2. `profile_env`(template baseline,operator 可信):覆盖非保护项,可设 PATH/LANG/任意 key
/// 3. `extra`(request custom_env,agent 传):只加**新** key(非保护、未被前两阶段占用)
///
/// HOME/TMPDIR 永不被覆盖。
pub fn build_sanitized_env(
    _job_id: &str,
    job_workspace: &Path,
    profile_env: &HashMap<String, String>,
    extra: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut env = HashMap::new();

    // 1. 核心默认
    env.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
    env.insert(
        "HOME".to_string(),
        job_workspace.to_string_lossy().to_string(),
    );
    env.insert(
        "TMPDIR".to_string(),
        job_workspace.join("tmp").to_string_lossy().to_string(),
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
