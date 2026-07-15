use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// job 工作空间元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobMetadata {
    pub job_id: String,
    pub created_at: u64,
    pub state: JobState,
    pub pid: Option<u32>,
    pub pgid: Option<u32>,
    pub sid: Option<u32>,
    pub pid_starttime: Option<String>,
    pub cgroup_path: Option<String>,
    pub workspace: String,
    pub timeout_ms: u64,
}

/// job 状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobState {
    Initializing,
    Running,
    Finished,
    Failed,
}

/// 工作空间管理器
pub struct WorkspaceManager {
    base_dir: PathBuf,
    disk_watermark_bytes: u64,
}

impl WorkspaceManager {
    pub fn new(base_dir: &Path, disk_watermark_bytes: u64) -> Self {
        Self {
            base_dir: base_dir.to_path_buf(),
            disk_watermark_bytes,
        }
    }

    /// 创建 job 工作空间目录结构
    pub fn create_job_workspace(&self, job_id: &str) -> Result<JobWorkspace, CoreError> {
        let base = self.base_dir.join(job_id);
        std::fs::create_dir_all(base.join("workspace"))?;
        std::fs::create_dir_all(base.join("tmp"))?;
        std::fs::create_dir_all(base.join("input"))?;
        std::fs::create_dir_all(base.join("output"))?;

        Ok(JobWorkspace {
            root: base.clone(),
            workspace: base.join("workspace"),
            tmp: base.join("tmp"),
            input: base.join("input"),
            output: base.join("output"),
        })
    }

    /// 写入 metadata
    pub fn write_metadata(&self, job_id: &str, meta: &JobMetadata) -> Result<(), CoreError> {
        let path = self.base_dir.join(job_id).join(".sandbox-meta.json");
        let json = serde_json::to_string_pretty(meta)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// 读取 metadata（崩溃恢复用）
    pub fn read_metadata(&self, job_id: &str) -> Result<Option<JobMetadata>, CoreError> {
        let path = self.base_dir.join(job_id).join(".sandbox-meta.json");
        if !path.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(path)?;
        let meta: JobMetadata = serde_json::from_str(&data)?;
        Ok(Some(meta))
    }

    /// 列出所有 job 目录（崩溃恢复用）
    pub fn list_jobs(&self) -> Result<Vec<String>, CoreError> {
        let mut jobs = Vec::new();
        if !self.base_dir.exists() {
            return Ok(jobs);
        }
        for entry in std::fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    jobs.push(name.to_string());
                }
            }
        }
        Ok(jobs)
    }

    /// 清理单个 job 工作空间
    pub fn cleanup_job(&self, job_id: &str) -> Result<(), CoreError> {
        let path = self.base_dir.join(job_id);
        if path.exists() {
            std::fs::remove_dir_all(path)?;
        }
        Ok(())
    }

    /// 检查磁盘水位
    ///
    /// 使用 statvfs 检查 base_dir 所在文件系统的可用空间。
    /// 可用空间 ≥ disk_watermark_bytes 时返回 true，否则返回 false。
    pub fn check_disk_watermark(&self) -> Result<bool, CoreError> {
        let stat = nix::sys::statvfs::statvfs(&self.base_dir)?;
        let available = stat.block_size() as u64 * stat.blocks_available() as u64;
        Ok(available >= self.disk_watermark_bytes)
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// 计算 job workspace 的总磁盘使用量（字节）
    ///
    /// 递归遍历 job 目录下所有文件，累加文件大小。
    /// 不存在的 job 返回 0。
    pub fn workspace_size(&self, job_id: &str) -> Result<u64, CoreError> {
        let path = self.base_dir.join(job_id);
        if !path.exists() {
            return Ok(0);
        }
        Ok(dir_size(&path))
    }

    /// 批量清理所有 job workspace
    ///
    /// 返回成功清理的 job 数量。
    /// 单个 job 清理失败不影响其他 job。
    pub fn cleanup_all_jobs(&self) -> Result<usize, CoreError> {
        let jobs = self.list_jobs()?;
        let mut cleaned = 0;
        for job_id in &jobs {
            if self.cleanup_job(job_id).is_ok() {
                cleaned += 1;
            }
        }
        Ok(cleaned)
    }

    // ==================== cr-026: 会话工作区 ====================

    /// 创建会话工作区(`base_dir/sessions/{id}/{workspace,tmp,input,output}`)。
    /// 与一次性 job 目录隔离(命名空间 sessions/),跨 exec 持久。
    pub fn create_session_workspace(&self, id: &str) -> Result<JobWorkspace, CoreError> {
        let base = self.base_dir.join("sessions").join(id);
        std::fs::create_dir_all(base.join("workspace"))?;
        std::fs::create_dir_all(base.join("tmp"))?;
        std::fs::create_dir_all(base.join("input"))?;
        std::fs::create_dir_all(base.join("output"))?;
        Ok(JobWorkspace {
            root: base.clone(),
            workspace: base.join("workspace"),
            tmp: base.join("tmp"),
            input: base.join("input"),
            output: base.join("output"),
        })
    }

    /// 清理会话工作区。
    pub fn cleanup_session(&self, id: &str) -> Result<(), CoreError> {
        let path = self.base_dir.join("sessions").join(id);
        if path.exists() {
            std::fs::remove_dir_all(path)?;
        }
        Ok(())
    }

    /// 列出所有会话 id(启动 recovery / 列会话用)。
    pub fn list_sessions(&self) -> Result<Vec<String>, CoreError> {
        let dir = self.base_dir.join("sessions");
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    ids.push(name.to_string());
                }
            }
        }
        Ok(ids)
    }

    // ==================== cr-027: 快照(磁盘-only,跨重启存活) ====================

    fn snapshots_dir(&self) -> PathBuf {
        self.base_dir.join("snapshots")
    }

    /// 建快照:把会话 workspace 整树拷贝到 `snapshots/{id}`。
    pub fn create_snapshot(&self, src_workspace: &Path, id: &str) -> Result<(), CoreError> {
        let dst = self.snapshots_dir().join(id);
        copy_dir_recursive(src_workspace, &dst)
    }

    /// 从快照恢复:把 `snapshots/{id}` 拷进目标 workspace。快照缺失 → Err。
    pub fn restore_snapshot(&self, id: &str, dst_workspace: &Path) -> Result<(), CoreError> {
        let src = self.snapshots_dir().join(id);
        if !src.exists() {
            return Err(CoreError::Workspace(format!("snapshot not found: {id}")));
        }
        copy_dir_recursive(&src, dst_workspace)
    }

    /// 列所有快照 id(扫盘)。
    pub fn list_snapshots(&self) -> Result<Vec<String>, CoreError> {
        let dir = self.snapshots_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    ids.push(name.to_string());
                }
            }
        }
        Ok(ids)
    }

    /// 删快照。
    pub fn cleanup_snapshot(&self, id: &str) -> Result<(), CoreError> {
        let p = self.snapshots_dir().join(id);
        if p.exists() {
            std::fs::remove_dir_all(p)?;
        }
        Ok(())
    }

    // ==================== cr-028: 卷(跨会话持久 rw) ====================

    fn volumes_dir(&self) -> PathBuf {
        self.base_dir.join("volumes")
    }
    pub fn volume_path(&self, name: &str) -> PathBuf {
        self.volumes_dir().join(name)
    }
    pub fn create_volume(&self, name: &str) -> Result<(), CoreError> {
        std::fs::create_dir_all(self.volume_path(name))?;
        Ok(())
    }
    pub fn list_volumes(&self) -> Result<Vec<String>, CoreError> {
        let dir = self.volumes_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    ids.push(name.to_string());
                }
            }
        }
        Ok(ids)
    }
    pub fn cleanup_volume(&self, name: &str) -> Result<(), CoreError> {
        let p = self.volume_path(name);
        if p.exists() {
            std::fs::remove_dir_all(p)?;
        }
        Ok(())
    }
}

/// 递归计算目录总大小(cr-022: 看门狗测量用,故 pub)
pub fn dir_size(path: &Path) -> u64 {    let mut total: u64 = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(file_type) = entry.file_type() {
                if file_type.is_file() {
                    if let Ok(metadata) = entry.metadata() {
                        total += metadata.len();
                    }
                } else if file_type.is_dir() {
                    total += dir_size(&entry.path());
                }
            }
        }
    }
    total
}

/// cr-027: 递归拷贝目录树(src 内容 → dst)。目录递归、文件拷贝;符号链接忽略(v1)。
pub fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), CoreError> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

// ==================== cr-026: 文件 I/O(会话工作区) ====================

/// 目录条目(list_files 返回)。
/// cr-085 M1: 补 mode/owner/group/时间戳/symlink_target(对齐 E2B FileInfo)。
#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    /// 是否符号链接(基于 symlink_metadata,不跟随)。
    pub is_symlink: bool,
    /// 完整 st_mode(含文件类型位,POSIX 风格,对齐 Python stat.st_mode)。
    pub mode: u32,
    /// 所有者用户名(getpwuid_r 解析,失败 fallback uid 数字字符串)。
    pub owner: String,
    /// 所属组名(getgrgid_r 解析,失败 fallback gid 数字字符串)。
    pub group: String,
    /// mtime,Unix 秒。
    pub modified_at: Option<i64>,
    /// ctime(inode change;Linux 无通用 birth time,作为 created_at 的妥协)。
    pub created_at: Option<i64>,
    /// 符号链接目标路径(仅 is_symlink=true)。
    pub symlink_target: Option<String>,
}

/// cr-026: 规范化相对路径,圈在 base 内。拒空、绝对路径、含 `..`(ParentDir 组件)。
/// 注:不解析符号链接(v1 限制);文件 I/O 由 API 侧发起(可信调用方)。
pub fn sanitize_relpath(base: &Path, rel: &str) -> Result<PathBuf, CoreError> {
    if rel.is_empty() {
        return Err(CoreError::Workspace("empty path".to_string()));
    }
    let p = Path::new(rel);
    if p.is_absolute() {
        return Err(CoreError::Workspace(format!(
            "absolute path not allowed: {rel}"
        )));
    }
    if p
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(CoreError::Workspace(format!(
            "parent-dir not allowed: {rel}"
        )));
    }
    Ok(base.join(p))
}

/// 上传文件(自动建父目录)。
pub fn put_file(base: &Path, rel: &str, data: &[u8]) -> Result<(), CoreError> {
    let path = sanitize_relpath(base, rel)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, data)?;
    Ok(())
}

/// 读取文件(不存在 → Err)。
pub fn get_file(base: &Path, rel: &str) -> Result<Vec<u8>, CoreError> {
    let path = sanitize_relpath(base, rel)?;
    Ok(std::fs::read(path)?)
}

/// 列目录(返回条目)。空 rel = base 根目录。
/// cr-085 M1: 用 symlink_metadata(不跟随符号链接),填充富元数据。
pub fn list_files(base: &Path, rel: &str) -> Result<Vec<FileEntry>, CoreError> {
    let dir = if rel.is_empty() {
        base.to_path_buf()
    } else {
        sanitize_relpath(base, rel)?
    };
    let mut entries = Vec::new();
    for e in std::fs::read_dir(&dir)? {
        let e = e?;
        let path = e.path();
        // symlink_metadata:不跟随 symlink,才能识别 symlink 本身及其自身 size/mode。
        let md = std::fs::symlink_metadata(&path)?;
        entries.push(build_file_entry(&path, &md));
    }
    Ok(entries)
}

/// cr-085 M6: 从单个路径建 FileEntry(symlink_metadata,复用 list_files 富元数据逻辑)。
/// 供 server 侧 find/search 按需取单条目,避免重复 uid→name/symlink 解析。
/// 路径不存在 → Err(io NotFound)。
pub fn file_entry(path: &Path) -> Result<FileEntry, CoreError> {
    let md = std::fs::symlink_metadata(path)?;
    Ok(build_file_entry(path, &md))
}

/// cr-085 M1/M6: 从 symlink_metadata + 路径构造 FileEntry(name 取 basename)。
/// M6 从 list_files 抽出,供 file_entry(单路径)复用。
fn build_file_entry(path: &Path, md: &std::fs::Metadata) -> FileEntry {
    let is_symlink = md.file_type().is_symlink();
    let symlink_target = if is_symlink {
        std::fs::read_link(path)
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
    } else {
        None
    };
    FileEntry {
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        size: md.len(),
        is_dir: md.is_dir(),
        is_symlink,
        mode: md.mode(),
        owner: user_name(md.uid()),
        group: group_name(md.gid()),
        modified_at: Some(md.mtime()),
        created_at: Some(md.ctime()),
        symlink_target,
    }
}

/// cr-085 M1: uid → 用户名(getpwuid_r,线程安全;失败 fallback uid 数字)。
fn user_name(uid: u32) -> String {
    let mut buf = [0u8; 1024];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            &mut pwd,
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() {
        return uid.to_string();
    }
    unsafe { std::ffi::CStr::from_ptr((*result).pw_name) }
        .to_string_lossy()
        .into_owned()
}

/// cr-085 M1: gid → 组名(getgrgid_r,线程安全;失败 fallback gid 数字)。
fn group_name(gid: u32) -> String {
    let mut buf = [0u8; 1024];
    let mut grp: libc::group = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::group = std::ptr::null_mut();
    let rc = unsafe {
        libc::getgrgid_r(
            gid,
            &mut grp,
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() {
        return gid.to_string();
    }
    unsafe { std::ffi::CStr::from_ptr((*result).gr_name) }
        .to_string_lossy()
        .into_owned()
}

/// 删除文件或目录。
pub fn delete_file(base: &Path, rel: &str) -> Result<(), CoreError> {
    let path = sanitize_relpath(base, rel)?;
    if path.is_dir() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// cr-085 M5: 建目录(sanitize 圈定,递归 create_dir_all)。E2B filesystem.make_dir。
pub fn make_dir(base: &Path, rel: &str) -> Result<(), CoreError> {
    let path = sanitize_relpath(base, rel)?;
    std::fs::create_dir_all(path)?;
    Ok(())
}

/// cr-085 M5: 路径是否存在(文件或目录;symlink_metadata 不跟随 symlink)。E2B filesystem.exists。
pub fn exists(base: &Path, rel: &str) -> bool {
    match sanitize_relpath(base, rel) {
        Ok(p) => p.symlink_metadata().is_ok(),
        Err(_) => false,
    }
}

/// 单个 job 的工作空间路径
#[derive(Debug, Clone)]
pub struct JobWorkspace {
    pub root: PathBuf,
    pub workspace: PathBuf,
    pub tmp: PathBuf,
    pub input: PathBuf,
    pub output: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// cr-085 M1: list_files 返回 mode 权限位 + owner/group(E2B FileInfo 对齐)。
    #[test]
    fn list_files_returns_mode_owner_group() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        std::fs::write(base.join("a.txt"), b"hi").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            base.join("a.txt"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();

        let entries = list_files(base, "").unwrap();
        let a = entries.iter().find(|e| e.name == "a.txt").unwrap();
        assert_eq!(a.mode & 0o777, 0o644, "mode low 9 bits = permissions");
        assert!(!a.owner.is_empty(), "owner resolved");
        assert!(!a.group.is_empty(), "group resolved");
    }

    /// cr-085 M1: symlink 不被跟随——识别为 symlink + 给出 target,size 为 symlink 自身(非 target 内容)。
    /// 若实现回退为 e.metadata()(跟随),size 断言会失败 → 测试有效。
    #[test]
    fn list_files_identifies_symlink_without_following() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        std::fs::write(base.join("target.txt"), b"target-content-12345").unwrap();
        std::os::unix::fs::symlink("target.txt", base.join("link.txt")).unwrap();

        let entries = list_files(base, "").unwrap();
        let link = entries.iter().find(|e| e.name == "link.txt").unwrap();
        assert!(link.is_symlink, "link 识别为 symlink");
        assert_eq!(link.symlink_target.as_deref(), Some("target.txt"));
        // size 是 symlink 自身(target 路径字节数),非 target 文件内容长度
        assert_eq!(link.size, "target.txt".len() as u64);
        assert!(!link.is_dir);
    }

    /// cr-085 M1: modified_at/created_at 填充(unix 秒,非零)。
    #[test]
    fn list_files_returns_timestamps() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        std::fs::write(base.join("a.txt"), b"hi").unwrap();

        let entries = list_files(base, "").unwrap();
        let a = entries.iter().find(|e| e.name == "a.txt").unwrap();
        assert!(a.modified_at.unwrap_or(0) > 0, "modified_set");
        assert!(a.created_at.unwrap_or(0) > 0, "created_set");
    }
}
