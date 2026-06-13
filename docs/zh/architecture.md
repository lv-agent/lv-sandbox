# 架构设计思路

## 为什么需要它

AI Agent 执行外部命令时面临风险：误删文件、读取密钥、死循环、fork 炸弹、调用危险 syscall。常见做法是**一个任务启动一个容器**，但容器启动慢、资源开销大，不适合大量轻量任务（如同时跑 100 个小脚本）。

lv-sandbox 的取舍是：**一个常驻 worker 内并发运行多个受隔离的任务**，用 Linux 内核安全机制（Landlock、seccomp、cgroup、rlimit）而非完整容器来隔离每个任务。轻量、快速、可高并发，同时保持足够的隔离强度。

```text
传统：一个任务 → 一个容器（重）
本方案：一个 worker → 多个任务，每个任务内核级隔离（轻）
```

> 这针对 Agent 误操作与一般越权访问。若任务来源完全不可信且要求极高，应使用 MicroVM / gVisor / Kata。详见[安全边界](#安全边界)。

---

## 高层架构

```text
┌───────────────────────────────────────────────┐
│  接入层                                         │
│  sandbox-server (HTTP)   sandbox-mcp (MCP)    │
└──────────────┬────────────────────┬───────────┘
               │                     │
               ▼                     ▼
┌───────────────────────────────────────────────┐
│  调度层   并发控制 · 指标 · 热重载              │
└──────────────────┬────────────────────────────┘
                   ▼
┌───────────────────────────────────────────────┐
│  内核层   sandbox-core                         │
│  任务执行 · profile · workspace · 生命周期     │
└──────────────────┬────────────────────────────┘
                   ▼
        Landlock · seccomp · cgroup（安全原语）
```

- **内核层**（`sandbox-core`）：负责任务完整生命周期，组合所有安全机制。
- **调度层**（`sandbox-server` 内）：并发限流、指标采集、配置热重载。
- **接入层**：`sandbox-server` 暴露 HTTP；`sandbox-mcp` 作网关，把 AI Agent 的 MCP 协议转为 HTTP 调用。
- **安全原语**：三个独立 crate，各自封装一种内核安全机制，可单独复用。

### 两种接入方式

```text
方式一（HTTP）：   curl / 应用  ──HTTP──▶  sandbox-server
方式二（MCP）：    AI Agent ──stdio──▶ sandbox-mcp ──HTTP──▶ sandbox-server
```

`sandbox-mcp` 不持有沙箱引擎，只做协议转换。这样多个 AI Agent 可共享同一个 sandbox-server，复用其并发控制与指标，而无需各自维护引擎。

---

## 安全机制

每个任务在独立进程组中执行，叠加六重隔离：

| 机制 | 作用 |
|---|---|
| **Landlock** | 限制文件系统访问，任务只能读写自己的 workspace |
| **seccomp** | 阻断危险 syscall（mount、ptrace、bpf、unshare、reboot 等），**并阻断所有网络 socket 系统调用——默认禁网：任务无法发起出站连接、监听端口或收发网络流量** |
| **rlimit** | 限制进程级资源（CPU、文件数、进程数、文件大小等） |
| **cgroup v2** | 限制任务级真实资源消耗（内存、CPU、pids），不可用时优雅降级 |
| **进程隔离** | NoNewPrivs 禁用提权、setsid 脱离控制终端、关闭泄漏 fd、env 白名单 |
| **超时清理** | 超时后整组 SIGTERM → SIGKILL，无孤儿进程 |

这些机制在运行时按实际环境**能力检测**后应用：Landlock 检测 ABI 版本、seccomp 检测可用性、cgroup 检测控制器。不支持时按 profile 配置决定是拒绝执行（fail-closed）还是降级继续（fail-open）。

---

## 安全边界

### 能防

- 任务读写其他任务的文件
- 任务读取容器内敏感文件、`/sys`、`/proc` 不必要信息
- 死循环、fork 炸弹、fd 耗尽
- 写爆 workspace（资源限制 + 告警）
- 创建后台进程逃避超时（进程组整组清理）
- 调用危险 syscall
- **发起网络连接**——默认阻断所有网络 socket 系统调用（seccomp），任务无法外联、无法访问云元数据服务（169.254.169.254）、无法开监听端口
- 继承 runner 的 secret 环境变量或泄漏的 fd

### 不完全防

强恶意代码攻击内核漏洞、高强度容器逃逸、多租户强隔离、所有侧信道。若任务来源完全不可信且安全要求高，应使用 MicroVM / gVisor / Kata / 单任务容器。

> **当前网络隔离是 seccomp 黑名单，不是内核级断网。** 它能挡住走标准 libc `socket()`/`connect()` 路径的程序，但黑名单天然不穷尽。更强的方案——per-task 网络命名空间（无网卡）+ UDS 出口代理（域名白名单 + 流量审计）——规划中（cr-017）。当前请将其视为「对常规程序默认禁网」。

### 推荐部署

外层用 worker 容器（非任务容器）提供边界，sandbox-server 在容器内非 root 运行：

```bash
docker run --rm \
  --read-only \
  --tmpfs /tmp:rw,nosuid,nodev,size=1g \
  -v /safe/worker/sandboxes:/sandboxes:rw \
  --cap-drop=ALL \
  --security-opt=no-new-privileges \
  --pids-limit=1000 --memory=4g --cpus=4 \
  --user 10000:10000 \
  your-worker-image
```

要点：不 `--privileged`、不挂 Docker socket / 敏感目录、rootfs 只读、非 root 运行、`/tmp` 用 tmpfs、只给 `/sandboxes` 可写。
