# 使用指南

## 环境要求

- Linux，宿主内核 ≥ 5.13（Landlock 所需）
- **Docker 部署**：仅需 Docker，镜像内置其余依赖
- **源码构建**：Rust 1.75+、`libseccomp-dev`（编译）/ `libseccomp2`（运行）
- 推荐以非 root 用户在容器内运行（见[架构文档 · 推荐部署](architecture.md#推荐部署)）

## 构建与运行

两种方式：**Docker 镜像**（推荐，开箱即用）或源码构建。

### Docker 部署（推荐）

镜像内置 `libseccomp2` 运行时、非 root 用户（uid 10000）和默认配置，`docker run` 即用。两种获取方式：

**方式 A：从 ghcr.io 拉取（最快）**

```bash
docker pull ghcr.io/lv-agent/lv-sandbox:v0.1.0
docker tag ghcr.io/lv-agent/lv-sandbox:v0.1.0 lv-sandbox:0.1.0   # 可选，便于复用下方命令
```

**方式 B：本地构建**

```bash
# 本地构建镜像
docker build -t lv-sandbox:0.1.0 .

# 或一条命令同时产出镜像 + 二进制 tar.gz（无 Docker 环境的兜底）
bash scripts/build-release.sh
```

**运行容器**：

```bash
docker run -d --name sandbox \
  -p 8080:8080 \
  --read-only --tmpfs /tmp:rw,nosuid,nodev,size=1g \
  -v /safe/worker/sandboxes:/sandboxes:rw \
  --cap-drop=ALL --security-opt no-new-privileges \
  --pids-limit=1000 --memory=4g --cpus=4 \
  --user 10000:10000 \
  lv-sandbox:0.1.0
```

要点：

- 宿主 Linux 内核 ≥ 5.13（Landlock 所需）；docker 默认 seccomp（libseccomp 2.5+）已放行 Landlock syscall，无需额外配置
- 挂载的 `/safe/worker/sandboxes` 宿主目录需可被 uid 10000 写入：`chown 10000:10000 /safe/worker/sandboxes`
- 镜像内置配置位于 `/etc/sandbox-server/config.yaml`，用 `-v your-config.yaml:/etc/sandbox-server/config.yaml:ro` 覆盖
- 容器内 cgroup v2 受限时自动降级到 rlimit 兜底（内置配置已设 `fail_closed: false`）
- 无需 `--privileged`

健康检查：`curl http://127.0.0.1:8080/health`

`build-release.sh` 产出的 `dist/lv-sandbox-<版本>-x86_64-gnu.tar.gz` 内含 `sandbox-server`/`sandbox-mcp`/示例配置/快速说明，解压后 `./sandbox-server --config config.yaml.example` 即可运行（需宿主 `libseccomp2`）。

### 源码构建

编译需 `libseccomp-dev`，运行需 `libseccomp2`。

```bash
cargo build --workspace --release
./target/release/sandbox-server --config config.yaml
```

配置文件查找优先级：`--config` 参数 > `SANDBOX_CONFIG` 环境变量 > `/etc/sandbox-server/config.yaml` > 内置默认。

---

## HTTP API

| 方法 | 路径 | 说明 |
|---|---|---|
| `POST` | `/api/v1/jobs` | 提交任务（异步——立即返回 `job_id`，后台运行） |
| `GET` | `/api/v1/jobs/{id}` | 查询任务状态/结果（`Running` 或完成时的 `JobResult`） |
| `POST` | `/api/v1/jobs/{id}/cancel` | 取消运行中的任务 |
| `GET` | `/api/v1/status` | 查询 worker 状态（运行数、并发上限、uptime） |
| `GET` | `/api/v1/profiles` | 列出所有可用 profile |
| `POST` | `/api/v1/reload` | 热重载配置（无需重启更新 profile） |
| `GET` | `/metrics` | Prometheus 指标 |
| `GET` | `/health` | 健康检查 |

### 提交任务（异步）

提交后立即返回 `job_id`（`202 Accepted`），任务在后台执行。轮询 `GET /jobs/{id}` 获取结果。

```bash
curl -X POST http://127.0.0.1:8080/api/v1/jobs \
  -H 'content-type: application/json' \
  -d '{
    "job_id": "demo-1",
    "argv": ["/bin/echo", "hello sandbox"],
    "profile_name": "shell",
    "timeout": "5s",
    "custom_env": {}
  }'
```

返回：

```json
{ "job_id": "demo-1", "status": "Running" }
```

请求体还支持可选的 `stdin` 字段——UTF-8 文本，通过管道传给子进程 stdin（如 `cat` 或读取输入的脚本）。

查询结果：

```bash
curl http://127.0.0.1:8080/api/v1/jobs/demo-1
```

返回（完成后）：

```json
{
  "job_id": "demo-1",
  "status": "Completed",
  "exit_code": 0,
  "signal": null,
  "stdout": "hello sandbox\n",
  "stderr": "",
  "duration_ms": 3,
  "timed_out": false
}
```

取消运行中的任务：

```bash
curl -X POST http://127.0.0.1:8080/api/v1/jobs/demo-1/cancel
```

`status` 可能值：`Completed`、`TimedOut`、`Killed`、`Cancelled`、`Error`。

### Profile

内置三个 profile，按任务运行时选择：

| profile | 适用 | 内存上限 | 默认超时 |
|---|---|---|---|
| `shell` | 简单 shell 命令 | 128 MB | 5s |
| `python` | Python 脚本 | 256 MB | 5s |
| `node` | Node.js 脚本 | 256 MB | 5s |

自定义 profile 通过配置文件添加（见 [配置参考](#配置参考)）。

---

## MCP 集成（Claude Code / Hermes-Agent）

`sandbox-mcp` 把沙箱封装为 4 个 MCP 工具，AI Agent 可直接调用：

| 工具 | 功能 |
|---|---|
| `sandbox_run` | 在沙箱中执行命令，返回结果 |
| `sandbox_profiles` | 列出可用 profile |
| `sandbox_status` | 查询 worker 状态 |
| `sandbox_reload` | 热重载配置 |

### 接入 Claude Code

项目根目录已提供 `.mcp.json`：

```json
{
  "mcpServers": {
    "sandbox": {
      "command": "cargo",
      "args": ["run", "-p", "sandbox-mcp", "--quiet", "--"],
      "env": {
        "SANDBOX_SERVER_URL": "http://127.0.0.1:8080",
        "RUST_LOG": "info"
      }
    }
  }
}
```

前提：sandbox-server 已在 `127.0.0.1:8080` 运行。Claude Code 启动时自动加载 `.mcp.json`，拉起 `sandbox-mcp` 网关，即可在对话中调用沙箱工具。

> 生产环境建议把 `command` 改为编译好的二进制（`./target/release/sandbox-mcp`），避免每次启动编译。

### Hermes-Agent

在 Hermes-Agent 的配置中添加同样的 MCP server 连接信息，通过 stdio JSON-RPC 通信即可。

---

## 配置参考

```yaml
server:
  listen_addr: "0.0.0.0:8080"
  max_concurrent_jobs: 100      # 最大并发任务数
  log_level: "info"
  log_format: "json"            # json | text

sandbox:
  base_dir: "/sandboxes"        # 任务工作空间根目录
  disk_watermark_mb: 1024       # 磁盘水位，低于则拒绝新任务（0 = 禁用）
  default_profile: "shell"
  fail_closed: true             # 安全机制不可用时是否拒绝执行

profiles:
  shell:
    rlimit:
      cpu_seconds: 2
      nofile: 64
      nproc: 32
      fsize_mb: 10
    max_stdout_mb: 5            # stdout 截断阈值
    default_timeout: "5s"

  python:
    extra_readonly_paths:       # 额外只读路径（如离线依赖库）
      - "/opt/sandbox-libs/python3"
    rlimit:
      cpu_seconds: 5
      nofile: 128
    max_stdout_mb: 10
    default_timeout: "30s"

  # 自定义 profile：未设字段继承 shell 默认值
  custom_task:
    rlimit:
      cpu_seconds: 10
    max_stdout_mb: 20
    default_timeout: "60s"
    extra_readonly_paths:
      - "/data/shared"
```

修改配置后调用 `POST /api/v1/reload` 热重载，无需重启服务。

### 超时格式

`timeout` / `default_timeout` 支持：`5s`、`100ms`、`1m`，或纯数字（秒）。

---

## 组件

| 组件 | 类型 | 职责 |
|---|---|---|
| `sandbox-server` | 二进制 | HTTP 服务 + 调度 + 配置 + 指标 |
| `sandbox-mcp` | 二进制 | MCP 网关，对接 AI Agent |
| `sandbox-core` | 库 | 任务执行内核，可复用 |
| `sandbox-landlock` | 库 | Landlock 文件系统隔离 |
| `sandbox-seccomp` | 库 | seccomp syscall 过滤 |
| `sandbox-cgroup` | 库 | cgroup v2 资源管理 |

---

## 测试

```bash
# 全部测试
cargo test --workspace

# 仅端到端
cargo test -p sandbox-e2e

# 验证 Docker 镜像（容器内端到端：health + 真跑一个 echo 任务）
bash scripts/verify-image.sh
```
