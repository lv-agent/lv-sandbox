# lv-sandbox

> **A safety-first execution sandbox for AI agents.** Run untrusted commands
> with six layers of kernel isolation — no container per task, no privileges,
> no network by default. Persistent sessions, snapshots, streaming, and a
> Python SDK for code-interpreter workflows.

```text
  AI Agent ──▶ sandbox-mcp (MCP gateway)  ──┐
                                             ├──▶ sandbox-server ──▶ [ task 1: Landlock+seccomp+cgroup ]
  Your app  ──▶ Python SDK / CLI / HTTP    ──┘                     └─▶ [ task 2: isolated, concurrent ]
                                                                       └─▶ [ task N: ... ]
```

Each task runs in its own process group wrapped in **Landlock** (filesystem) +
**seccomp** (syscalls, AF_UNIX-only) + **cgroup v2** (mem/CPU/pids/IO) +
**rlimit** + **process hardening** + **timeout reaping**. Hundreds of
kernel-isolated tasks from one lightweight worker, with zero extra privileges.

## 30-second start

**Start the server:**

```bash
docker pull ghcr.io/lv-agent/lv-sandbox:v0.4.0
docker run -d --name sandbox -p 8080:8080 \
  --cap-drop=ALL --security-opt no-new-privileges \
  --pids-limit=1000 --memory=4g --cpus=4 \
  --tmpfs /sandboxes:rw,nosuid,nodev,size=100m,uid=10000,gid=10000 \
  --user 10000:10000 \
  ghcr.io/lv-agent/lv-sandbox:v0.4.0
```

**Use it from Python** (E2B-style sessions + code interpreter):

```python
pip install -e sdk/python    # or: pip install lvsandbox

from lvsandbox import Client

lv = Client("http://127.0.0.1:8080")

# one-shot job (blocks until done)
print(lv.jobs.run(["/bin/echo", "hello agent"]).stdout)     # → hello agent

# persistent session — multi-step workflows
s = lv.sessions.create(profile="python")
s.files.put("plot.py", b"import matplotlib.pyplot as plt; plt.plot([1,2,3]); plt.savefig('chart.png')")
r, files = lv.run_python(open("plot.py").read())
print(r.stdout, [f.path for f in files])                    # → stdout + ["chart.png", ...]

# stream live output
for ev in lv.jobs.run(["/bin/sh", "-c", "for i in 1 2 3; do echo tick $i; done"], stream=True):
    if ev.type == "stdout": print(ev.data, end="")
```

**Or from the CLI:**

```bash
cargo build -p lv-cli
./target/debug/lvs jobs run -- /bin/echo "from CLI"
./target/debug/lvs sessions new --profile shell          # → session id
./target/debug/lvs exec <id> -- /bin/sh -c 'echo hi > f.txt'
./target/debug/lvs files get <id> f.txt                   # → hi
./target/debug/lvs shell <id> -- /bin/sh                  # interactive PTY
```

## What it stops

```bash
# normal command → works
curl -s localhost:8080/api/v1/jobs/ok   # → {"status":"Completed","exit_code":0,...}

# read a host secret → Landlock denies (nothing leaks)
# → {"status":"Completed","exit_code":1,"stderr":"/bin/cat: /etc/passwd: Permission denied"}

# phone home → seccomp kills the socket at creation
# → {"status":"Killed",...}   (network never reached)

# fill the disk → disk_quota_mb reaps it
# → {"status":"DiskQuotaExceeded",...}
```

No container per task. No `--privileged`. No outbound network by default.

## Features

**Isolation (kernel-level, per task):**

- **Landlock** — filesystem confined to the task workspace; other tasks' files
  and host secrets are invisible
- **seccomp** — dangerous syscalls blocked; `socket()` restricted to AF_UNIX
  (zero TCP/UDP — controlled egress via allowlisted UDS SOCKS5 proxy, opt-in)
- **cgroup v2** — memory, CPU, pids, and IO rate limits
- **rlimit + disk quota** — CPU seconds, fd count, process count, file size,
  aggregate workspace cap (`disk_quota_mb`)
- **Process hardening** — NoNewPrivs, setsid, fd cleanup, env allowlist
- **Timeout reaping** — SIGTERM → SIGKILL, whole process group, no orphans

**Sessions & persistence (E2B-style sandbox model):**

- **Persistent sessions** — long-lived workspaces with multiple `exec` calls,
  file upload/download, snapshots (fork), and persistent volumes
- **Snapshots** — full workspace copy; fork new sessions from it
- **Volumes** — named directories that persist across sessions and restarts
- **Cross-restart reconnect** — sessions, snapshots, and volumes survive worker
  restart

**Developer experience:**

- **Python SDK** (`lvsandbox`) — sessions, files, streaming, `run_python()`,
  OpenAI / LangChain tool schemas
- **CLI** (`lvs`) — manage everything from the terminal, including interactive
  PTY (`lvs shell`)
- **Streaming stdout** (SSE) — `?stream=true` for live output
- **Interactive PTY** — WebSocket terminal for REPLs and debugging
- **MCP gateway** — `sandbox-mcp` for Claude Code / Hermes-Agent
- **Code interpreter mode** — `list_files: true` returns workspace file listing
  with MIME types (charts, data, HTML)
- **Lifecycle webhooks** — terminal events POSTed to your URL (no polling)
- **Bearer API auth** — `server.api_key` (default off, zero local friction)
- **Prometheus metrics** + JSONL audit log + `/health` readiness

## Documentation

- 📐 [Architecture](docs/architecture.md) — design bet, layers, security boundary
- 📖 [Usage guide](docs/usage.md) — build, run, config, tutorial
- 🔌 [HTTP API reference](docs/api.md) — endpoints, schemas, status codes
- 🛡️ [Security model](docs/security.md) — threat boundary & deployment hardening
- 🌐 [Network isolation](docs/network-isolation.md) — egress model deep-dive
- ⚖️ [How it compares](docs/comparison.md) — vs Docker / gVisor / Kata / microVM / E2B
- 🤖 [Claude Code walkthrough](docs/integrations/claude-code.md) — end-to-end
- 🐍 [Python SDK](sdk/python/README.md) — `lvsandbox` package
- 💻 [CLI](crates/lv-cli/README.md) — `lvs` command-line tool
- 🇨🇳 [中文文档](README.zh.md)

## Status

> **v0.4.0 — early, not security-audited.** Decide fit using the
> [threat model](docs/security.md).

**Best fit:** running AI-agent-generated commands where you want kernel-level
blast-radius control *without* a container per task; single-tenant or
trusted-tenant workers on Linux ≥ 5.13 (Landlock).

**Not for:** fully untrusted or hostile code, multi-tenant hostile workloads,
or high-assurance production. Use MicroVM / gVisor / Kata instead.

## Requirements

- Linux, host kernel ≥ 5.13 (Landlock)
- Docker (image ships everything), or Rust 1.75+ for source builds

## License

MIT OR Apache-2.0
