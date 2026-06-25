# lv-sandbox

> 轻量级 Agent 沙箱：在一个 worker 内并发运行大量受隔离的任务，而非一任务一容器。

为 AI Agent（Claude Code、Hermes-Agent 等）提供安全的命令执行环境。每个任务在独立进程组中运行，叠加 Landlock + seccomp + rlimit + cgroup 多重隔离。

## 状态

> **v0.2.0 —— 早期版本,未做外部安全审计。** lv-sandbox 是一个年轻的开源项目,
> 尚未经过外部安全审计。请对照 [security.md](docs/security.md) 的威胁模型判断是否适用。

**适用** —— 运行 AI Agent 生成的命令,需要内核级失败半径控制又不想"一任务一容器";
单租户或可信租户 worker;Linux ≥ 5.13(Landlock);以"控制 Agent 误操作与一般越权"
为目标的团队。

**不适用** —— 完全不可信或敌意代码、多租户敌对负载、高保障生产环境。请改用
**gVisor / Kata / Firecracker(MicroVM)/ 一任务一容器**。

lv-sandbox 在**一个** worker 内叠加 Landlock + seccomp + cgroup —— 是 Agent 工作负载
的纵深防御,不是对抗内核漏洞利用的硬沙箱。

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
# 拉取官方镜像（或本地 docker build -t lv-sandbox:0.2.0 .）
docker pull ghcr.io/lv-agent/lv-sandbox:v0.2.0
docker run -d --name sandbox -p 8080:8080 \
  --read-only --tmpfs /tmp:rw,nosuid,nodev,size=1g \
  -v /safe/worker/sandboxes:/sandboxes:rw \
  --cap-drop=ALL --security-opt no-new-privileges \
  --pids-limit=1000 --memory=4g --cpus=4 --user 10000:10000 \
  ghcr.io/lv-agent/lv-sandbox:v0.2.0
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

## 看看效果

三个任务——一个正常,两个 agent 可能误踩的"危险"操作:

```bash
# 1) 正常命令 → 放行
curl -X POST localhost:8080/api/v1/jobs -H 'content-type: application/json' \
  -d '{"job_id":"ok","argv":["/bin/echo","hello agent"],"profile_name":"shell","timeout":"5s","custom_env":{}}'

# 2) 想读宿主密钥 → Landlock 拒绝(什么都不泄)
curl -X POST localhost:8080/api/v1/jobs -H 'content-type: application/json' \
  -d '{"job_id":"secret","argv":["/bin/cat","/etc/passwd"],"profile_name":"shell","timeout":"5s","custom_env":{}}'

# 3) 想"phone home" → seccomp 杀掉 socket
curl -X POST localhost:8080/api/v1/jobs -H 'content-type: application/json' \
  -d '{"job_id":"net","argv":["/usr/bin/curl","-s","http://example.com"],"profile_name":"shell","timeout":"5s","custom_env":{}}'
```

轮询 `GET /api/v1/jobs/{id}` 看各自结果:

| job | 结果 | 原因 |
|---|---|---|
| `ok` | `Completed`,stdout `hello agent` | 正常命令,放行 |
| `secret` | `Completed` 退出码 1,stderr `cat: /etc/passwd: Permission denied` | Landlock 把任务圈在工作区内,`/etc/passwd` 在圈外 |
| `net` | `Killed` | seccomp 杀掉 `socket(AF_INET)`——任务根本建不出 TCP/UDP socket |

30 秒看懂核心:**正常命令照跑,危险操作被兜住——不靠一任务一容器、不要特权、不会外联。**(要"受控出站"而非全断,见 [network-isolation.md](docs/zh/network-isolation.md)。)

## 文档

- 📐 [架构设计思路](docs/zh/architecture.md) — 为什么这样设计、高层架构、安全边界
- 📖 [使用指南](docs/zh/usage.md) — 构建、运行、配置、教程
- 🔌 [HTTP API 参考](docs/zh/api.md) — 端点、schema、状态码
- 🛡️ [安全模型](docs/zh/security.md) — 威胁边界与部署加固
- 🌐 [网络隔离](docs/zh/network-isolation.md) — 出站模型深度
- ⚖️ [方案对比](docs/zh/comparison.md) — 对照 Docker/gVisor/Kata/microVM,按威胁模型选型
- 🤖 [Claude Code 走查](docs/zh/integrations/claude-code.md) — 5 分钟把 agent 命令接进沙箱
- 🇬🇧 English docs: [README](README.md) · [Architecture](docs/architecture.md) · [Usage](docs/usage.md) · [API](docs/api.md) · [Security](docs/security.md) · [Network](docs/network-isolation.md) · [Comparison](docs/comparison.md) · [Claude Code](docs/integrations/claude-code.md)

## License

MIT OR Apache-2.0
