use std::path::{Path, PathBuf};

use crate::detect::CgroupAvailability;
use crate::error::CgroupError;
use crate::resources::{CgroupResources, ResourceUsage};

/// 单个 job 的 cgroup 管理器
pub struct JobCgroup {
    path: PathBuf,
    job_id: String,
}

impl JobCgroup {
    /// 为指定 job 创建 cgroup 子组并配置资源限制。
    /// 在 fork 前调用。
    pub fn create(
        job_id: &str,
        parent: &Path,
        resources: &CgroupResources,
        _avail: &CgroupAvailability,
    ) -> Result<Self, CgroupError> {
        let cg_path = parent.join(format!("sandbox-{job_id}"));

        // 如果目录已存在（上次测试/崩溃残留），先清理
        if cg_path.exists() {
            // 尝试杀残留进程
            if let Ok(content) = std::fs::read_to_string(cg_path.join("cgroup.procs")) {
                for line in content.lines() {
                    if let Ok(pid) = line.trim().parse::<u32>() {
                        unsafe { libc::kill(pid as i32, libc::SIGKILL); }
                    }
                }
            }
            let _ = std::fs::remove_dir(&cg_path);
        }

        // 创建 cgroup 目录（mkdir 即创建 cgroup）
        std::fs::create_dir(&cg_path)?;

        // 写入资源限制
        let cg = Self {
            path: cg_path,
            job_id: job_id.to_string(),
        };

        cg.apply_resources(resources)?;

        Ok(cg)
    }

    /// 写入资源限制到 cgroup 文件
    fn apply_resources(&self, resources: &CgroupResources) -> Result<(), CgroupError> {
        // memory.max
        if let Some(max) = resources.memory_max {
            cg_write(&self.path, "memory.max", &max.to_string())?;
        }

        // cpu.max: "quota period" 或 "max 100000"
        if resources.cpu_max_quota.is_some() || resources.cpu_max_period.is_some() {
            let quota = resources.cpu_max_quota.map_or("max".to_string(), |q| q.to_string());
            let period = resources.cpu_max_period.unwrap_or(100000);
            cg_write(&self.path, "cpu.max", &format!("{quota} {period}"))?;
        }

        // pids.max
        if let Some(max) = resources.pids_max {
            cg_write(&self.path, "pids.max", &max.to_string())?;
        }

        // io.max (如果配置了)
        if let Some(ref io) = resources.io_max {
            let mut parts = vec![format!("{}:{}", io.major, io.minor)];
            if let Some(rbps) = io.read_bps {
                parts.push(format!("rbps={rbps}"));
            }
            if let Some(wbps) = io.write_bps {
                parts.push(format!("wbps={wbps}"));
            }
            if let Some(riops) = io.read_iops {
                parts.push(format!("riops={riops}"));
            }
            if let Some(wiops) = io.write_iops {
                parts.push(format!("wiops={wiops}"));
            }
            cg_write(&self.path, "io.max", &parts.join(" "))?;
        }

        Ok(())
    }

    /// 将进程迁移到此 cgroup。
    /// 写入 cgroup.procs 文件。
    pub fn migrate_process(&self, pid: u32) -> Result<(), CgroupError> {
        cg_write(&self.path, "cgroup.procs", &pid.to_string())
    }

    /// 读取当前资源使用统计
    pub fn resource_usage(&self) -> Result<ResourceUsage, CgroupError> {
        let memory_current = cg_read_u64(&self.path, "memory.current")?;
        let memory_peak = cg_read_u64(&self.path, "memory.peak")?;
        let pids_current = cg_read_u64(&self.path, "pids.current")?;

        // cpu.stat 格式: "usage_usec 12345\nuser_usec ...\n..."
        let cpu_usage_usec = std::fs::read_to_string(self.path.join("cpu.stat"))
            .ok()
            .and_then(|content| {
                for line in content.lines() {
                    if line.starts_with("usage_usec ") {
                        return line.split_whitespace().nth(1)?.parse::<u64>().ok();
                    }
                }
                None
            });

        Ok(ResourceUsage {
            memory_current,
            memory_peak,
            cpu_usage_usec,
            pids_current,
        })
    }

    /// 列出 cgroup 内所有进程
    pub fn processes(&self) -> Result<Vec<u32>, CgroupError> {
        let content = std::fs::read_to_string(self.path.join("cgroup.procs"))?;
        let pids: Vec<u32> = content
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect();
        Ok(pids)
    }

    /// 销毁 cgroup
    pub fn destroy(self) -> Result<(), CgroupError> {
        if self.path.exists() {
            // 杀掉残留进程
            if let Ok(procs) = self.processes() {
                for pid in procs {
                    unsafe {
                        libc::kill(pid as i32, libc::SIGKILL);
                    }
                }
            }
            std::fs::remove_dir(&self.path)?;
        }
        Ok(())
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn job_id(&self) -> &str {
        &self.job_id
    }
}

/// 写入 cgroup 文件
fn cg_write(cg_path: &Path, file: &str, value: &str) -> Result<(), CgroupError> {
    let path = cg_path.join(file);
    // 只在文件存在时写入（某些控制器可能不可用）
    if path.exists() {
        std::fs::write(&path, value).map_err(|e| {
            CgroupError::ResourceWrite(format!("写入 {:?} 失败: {}", path, e))
        })?;
    } else {
        tracing::debug!("跳过不存在的 cgroup 文件: {:?}", path);
    }
    Ok(())
}

/// 读取 cgroup 文件中的 u64 值
fn cg_read_u64(cg_path: &Path, file: &str) -> Result<Option<u64>, CgroupError> {
    let path = cg_path.join(file);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path).map_err(|e| {
        CgroupError::ReadFailed(format!("读取 {:?} 失败: {}", path, e))
    })?;
    // 某些文件可能返回 "max" 等非数值
    Ok(content.trim().parse::<u64>().ok())
}
