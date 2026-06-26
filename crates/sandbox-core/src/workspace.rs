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
}

/// 递归计算目录总大小(cr-022: 看门狗测量用,故 pub)
pub fn dir_size(path: &Path) -> u64 {
    let mut total: u64 = 0;
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

// ==================== cr-026: 文件 I/O(会话工作区) ====================

/// 目录条目(list_files 返回)。
#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
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
pub fn list_files(base: &Path, rel: &str) -> Result<Vec<FileEntry>, CoreError> {
    let dir = if rel.is_empty() {
        base.to_path_buf()
    } else {
        sanitize_relpath(base, rel)?
    };
    let mut entries = Vec::new();
    for e in std::fs::read_dir(&dir)? {
        let e = e?;
        let md = e.metadata()?;
        entries.push(FileEntry {
            name: e.file_name().to_string_lossy().into_owned(),
            size: md.len(),
            is_dir: md.is_dir(),
        });
    }
    Ok(entries)
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

/// 单个 job 的工作空间路径
#[derive(Debug, Clone)]
pub struct JobWorkspace {
    pub root: PathBuf,
    pub workspace: PathBuf,
    pub tmp: PathBuf,
    pub input: PathBuf,
    pub output: PathBuf,
}
