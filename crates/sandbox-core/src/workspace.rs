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

/// 单个 job 的工作空间路径
#[derive(Debug, Clone)]
pub struct JobWorkspace {
    pub root: PathBuf,
    pub workspace: PathBuf,
    pub tmp: PathBuf,
    pub input: PathBuf,
    pub output: PathBuf,
}
