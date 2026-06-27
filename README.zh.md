# lv-sandbox

> **面向 AI Agent 的安全执行沙箱。** 六层内核隔离运行不可信命令——
> 无需每任务一个容器、无需特权、默认零网络。支持持久会话、快照、流式输出、
> Python SDK,适合代码解释器等 agent 工作流。

```text
  AI Agent ──▶ sandbox-mcp (MCP 网关)    ──┐
                                             ├──▶ sandbox-server ──▶ [ 任务1: Landlock+seccomp+cgroup ]
  你的应用 ──▶ Python SDK / CLI / HTTP      ─┘                     └─▶ [ 任务2: 隔离、并发 ]
                                                                       └─▶ [ 任务N: ... ]
```

每个任务跑在独立进程组中,叠加 **Landlock**(文件系统)+ **seccomp**(syscall,
仅 AF_UNIX)+ **cgroup v2**(内存/CPU/pids/IO)+ **rlimit** + **进程加固** +
**超时收割**。一个轻量 worker 跑上百个内核隔离任务,零额外特权。

## 30 秒上手

**启动 server:**

```bash
docker pull ghcr.io/lv-agent/lv-sandbox:v0.3.0
docker run -d --name sandbox -p 8080:8080 \
  --cap-drop=ALL --security-opt no-new-privileges \
  --pids-limit=1000 --memory=4g --cpus=4 \
  --tmpfs /sandboxes:rw,nosuid,nodev,size=100m,uid=10000,gid=10000 \
  --user 10000:10000 \
  ghcr.io/lv-agent/lv-sandbox:v0.3.0
```

**Python 使用**(E2B 式会话 + 代码解释器):

```python
pip install -e sdk/python    # 或:pip install lvsandbox

from lvsandbox import Client

lv = Client("http://127.0.0.1:8080")

# 一次性 job(同步等结果)
print(lv.jobs.run(["/bin/echo", "hello agent"]).stdout)     # → hello agent

# 持久会话——多步工作流
s = lv.sessions.create(profile="python")
s.files.put("plot.py", b"import matplotlib.pyplot as plt; plt.plot([1,2,3]); plt.savefig('chart.png')")
r, files = lv.run_python(open("plot.py").read())
print(r.stdout, [f.path for f in files])                    # → stdout + ["chart.png", ...]

# 流式输出
for ev in lv.jobs.run(["/bin/sh", "-c", "for i in 1 2 3; do echo tick $i; done"], stream=True):
    if ev.type == "stdout": print(ev.data, end="")
```

**CLI 使用:**

```bash
cargo build -p lv-cli
./target/debug/lvs jobs run -- /bin/echo "from CLI"
./target/debug/lvs sessions new --profile shell          # → 会话 id
./target/debug/lvs exec <id> -- /bin/sh -c 'echo hi > f.txt'
./target/debug/lvs files get <id> f.txt                   # → hi
./target/debug/lvs shell <id> -- /bin/sh                  # 交互 PTY
```

## 拦住了什么

```bash
# 正常命令 → 放行
# → {"status":"Completed","exit_code":0,...}

# 读宿主密钥 → Landlock 拒绝(什么都不泄)
# → {"status":"Completed","exit_code":1,"stderr":"/bin/cat: /etc/passwd: Permission denied"}

# 外联 → seccomp 在 socket() 创建处杀掉
# → {"status":"Killed",...}   (网络根本没碰到)

# 写爆磁盘 → disk_quota_mb 收割
# → {"status":"DiskQuotaExceeded",...}
```

不靠一任务一容器。不要特权。默认零外联。

## 特性

**隔离(内核级,每任务):**

- **Landlock** — 文件系统圈禁在工作区;其他任务文件 + 宿主密钥不可见
- **seccomp** — 危险 syscall 拦截;`socket()` 仅 AF_UNIX(零 TCP/UDP;白名单 UDS SOCKS5 代理可选开启)
- **cgroup v2** — 内存、CPU、pids、IO 速率限制
- **rlimit + 磁盘配额** — CPU 秒数、fd 数、进程数、文件大小、聚合工作区上限(`disk_quota_mb`)
- **进程加固** — NoNewPrivs、setsid、fd 清理、env 白名单
- **超时收割** — SIGTERM → SIGKILL,整进程组,无孤儿

**会话与持久化(E2B 式沙箱模型):**

- **持久会话** — 长期工作区,多次 `exec`、文件上传/下载
- **快照** — 工作区整树拷贝,可 fork 新会话
- **卷** — 命名持久目录,跨会话 + 跨重启存活
- **跨重启重连** — 会话/快照/卷 worker 重启后仍可用

**开发者体验:**

- **Python SDK**(`lvsandbox`)— 会话、文件、流式、`run_python()`、OpenAI/LangChain 工具 schema
- **CLI**(`lvs`)— 命令行管理一切,含交互 PTY(`lvs shell`)
- **流式 stdout**(SSE)— `?stream=true` 实时输出
- **交互 PTY** — WebSocket 终端,REPL / 调试
- **MCP 网关** — `sandbox-mcp` 对接 Claude Code / Hermes-Agent
- **代码解释器模式** — `list_files: true` 返回文件清单 + MIME(图表、数据、HTML)
- **生命周期 webhook** — 终态事件 POST 到你的 URL(免轮询)
- **Bearer API 鉴权** — `server.api_key`(默认关,本地零摩擦)
- **Prometheus 指标** + JSONL 审计日志 + `/health` 就绪检查

## 文档

- 📐 [架构](docs/zh/architecture.md) — 设计思路、分层、安全边界
- 📖 [使用指南](docs/zh/usage.md) — 构建、运行、配置、教程
- 🔌 [HTTP API 参考](docs/zh/api.md) — 端点、schema、状态码
- 🛡️ [安全模型](docs/zh/security.md) — 威胁边界与部署加固
- 🌐 [网络隔离](docs/zh/network-isolation.md) — 出站模型深度
- ⚖️ [方案对比](docs/zh/comparison.md) — vs Docker/gVisor/Kata/microVM/E2B
- 🤖 [Claude Code 走查](docs/zh/integrations/claude-code.md) — 端到端
- 🐍 [Python SDK](sdk/python/README.md) — `lvsandbox` 包
- 💻 [CLI](crates/lv-cli/README.md) — `lvs` 命令行
- 🇬🇧 [English docs](README.md)

## 状态

> **v0.3.0 — 早期,未做外部安全审计。** 适用性判断见[威胁模型](docs/zh/security.md)。

**最适合:** 运行 AI Agent 生成的命令,需要内核级失败半径控制又不想"一任务一容器";
单租户或可信租户 worker;Linux ≥ 5.13(Landlock)。

**不适合:** 完全不可信或敌意代码、多租户敌对负载、高保障生产环境。请用 MicroVM / gVisor / Kata。

## 环境要求

- Linux,宿主内核 ≥ 5.13(Landlock)
- Docker(镜像内置其余),或 Rust 1.75+ 源码构建

## License

MIT OR Apache-2.0
