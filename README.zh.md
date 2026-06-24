# lv-sandbox

> 轻量级 Agent 沙箱：在一个 worker 内并发运行大量受隔离的任务，而非一任务一容器。

为 AI Agent（Claude Code、Hermes-Agent 等）提供安全的命令执行环境。每个任务在独立进程组中运行，叠加 Landlock + seccomp + rlimit + cgroup 多重隔离。

## 特性

- **六重安全隔离**：Landlock（文件系统）+ seccomp（syscall）+ rlimit（资源）+ cgroup v2（内存/CPU/pids）+ 进程隔离（NoNewPrivs/setsid/fd 清理/env 白名单）+ 超时清理
- **默认零出站，白名单受控出站可选**：seccomp 把 `socket()` 限制为仅 `AF_UNIX`，任务建不出任何 TCP/UDP socket；profile 可按白名单经 UDS SOCKS5 代理开启受控出站。零特权——见[网络隔离](docs/zh/network-isolation.md)
- **并发执行**：一个 worker 同时跑上百个轻量任务，`Semaphore` 限流排队
- **YAML 配置**：内置 `shell`/`python`/`node` profile，可自定义，支持热重载
- **异步任务 + 取消**：提交立即返回，轮询取结果，可取消运行中任务（SIGTERM → SIGKILL）
- **HTTP API**：提交/查询/取消、列 profile、重载配置、Prometheus 指标
- **输出脱敏与就绪**：`stdout`/`stderr` 返回前清洗密钥；`/health` 报告安全机制生效状态
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

- 📐 [架构设计思路](docs/zh/architecture.md) — 为什么这样设计、高层架构、安全边界
- 📖 [使用指南](docs/zh/usage.md) — 构建、运行、配置、教程
- 🔌 [HTTP API 参考](docs/zh/api.md) — 端点、schema、状态码
- 🛡️ [安全模型](docs/zh/security.md) — 威胁边界与部署加固
- 🌐 [网络隔离](docs/zh/network-isolation.md) — 出站模型深度
- 🇬🇧 English docs: [README](README.md) · [Architecture](docs/architecture.md) · [Usage](docs/usage.md) · [API](docs/api.md) · [Security](docs/security.md) · [Network](docs/network-isolation.md)

## License

MIT OR Apache-2.0
