# lv-sandbox

> 轻量级 Agent 沙箱：在一个 worker 内并发运行大量受隔离的任务，而非一任务一容器。

为 AI Agent（Claude Code、Hermes-Agent 等）提供安全的命令执行环境。每个任务在独立进程组中运行，叠加 Landlock + seccomp + rlimit + cgroup 多重隔离。

## 特性

- **六重安全隔离**：Landlock（文件系统）+ seccomp（syscall）+ rlimit（资源）+ cgroup v2（内存/CPU/pids）+ 进程隔离（NoNewPrivs/setsid/fd 清理/env 白名单）+ 超时清理
- **默认禁网**：seccomp 阻断所有网络 socket 系统调用，任务无法发起出站连接或开监听端口（基于黑名单；内核级 netns 隔离规划中——见[安全边界](docs/architecture.md#安全边界)）
- **并发执行**：一个 worker 同时跑上百个轻量任务，`Semaphore` 限流排队
- **YAML 配置**：内置 `shell`/`python`/`node` profile，可自定义，支持热重载
- **HTTP API**：提交任务、查询状态、列 profile、重载配置、Prometheus 指标
- **MCP 集成**：`sandbox-mcp` 网关对接 Claude Code / Hermes-Agent

## 快速开始

**Docker（推荐）**：

```bash
# 拉取官方镜像（或本地 docker build -t lv-sandbox:0.1.0 .）
docker pull ghcr.io/lv-agent/lv-sandbox:v0.1.0
docker run -d --name sandbox -p 8080:8080 \
  --read-only --tmpfs /tmp:rw,nosuid,nodev,size=1g \
  -v /safe/worker/sandboxes:/sandboxes:rw \
  --cap-drop=ALL --security-opt no-new-privileges \
  --pids-limit=1000 --memory=4g --cpus=4 --user 10000:10000 \
  ghcr.io/lv-agent/lv-sandbox:v0.1.0
```

**或源码构建**（需 libseccomp-dev / libseccomp2）：

```bash
cargo build --workspace --release
./target/release/sandbox-server
```

执行一个任务（异步——提交立即返回 `job_id`，用 `GET /jobs/{id}` 轮询结果）：

```bash
curl -X POST http://127.0.0.1:8080/api/v1/jobs \
  -H 'content-type: application/json' \
  -d '{"job_id":"demo-1","argv":["/bin/echo","hello"],"profile_name":"shell","timeout":"5s","custom_env":{}}'
# → {"job_id":"demo-1","status":"Running"}
curl http://127.0.0.1:8080/api/v1/jobs/demo-1
```

## 文档

- 📐 [架构设计思路](docs/architecture.md) — 为什么这样设计、高层架构、安全边界
- 📖 [使用指南](docs/usage.md) — 构建、HTTP API、MCP 集成、配置参考

## License

MIT OR Apache-2.0
