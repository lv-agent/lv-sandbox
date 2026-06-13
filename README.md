# lv-sandbox

> A **safety-first execution sandbox for AI Agents** — run untrusted agent
> commands with kernel-level isolation, without spinning up a container per task.

AI agents (Claude Code, Hermes-Agent, coding assistants, autonomous tool-users)
need to execute shell commands, scripts and generated code on the host. Letting
them do that directly is dangerous: one bad command can delete files, read
secrets, fork-bomb the machine, or invoke privileged syscalls. **lv-sandbox is
the guard rail** — every command runs inside a hardened Linux process group
wrapped in six layers of kernel isolation (Landlock + seccomp + rlimit +
cgroup v2 + process hardening + timeout reaping).

The design bet: **one long-lived worker runs many concurrently-isolated tasks**,
using kernel security primitives instead of a full container per task. Light,
fast, high-concurrency — strong enough to contain agent mistakes and casual
privilege-escalation attempts.

## Why

| Traditional | lv-sandbox |
|---|---|
| one task → one container (heavy) | one worker → many tasks, each kernel-isolated (light) |

Spinning up a container per command is slow and expensive when an agent fires off
hundreds of small tasks. lv-sandbox isolates each task at the kernel level inside
a single worker — fast cold-start, low overhead, high throughput.

> Built for **agent mistakes and casual escape attempts**. For fully untrusted,
> high-assurance code, use MicroVM / gVisor / Kata. See
> [Security boundary](docs/architecture.md#security-boundary).

## Features

- **Six-layer isolation** — Landlock (filesystem) + seccomp (syscalls) + rlimit
  (resources) + cgroup v2 (mem/CPU/pids) + process hardening (NoNewPrivs / setsid
  / fd cleanup / env allowlist) + timeout reaping
- **High concurrency** — one worker runs hundreds of lightweight tasks, bounded
  by a `Semaphore`
- **YAML profiles** — built-in `shell` / `python` / `node`, fully customisable,
  hot-reloadable
- **HTTP API** — submit, status, list profiles, reload, Prometheus metrics
- **MCP integration** — `sandbox-mcp` gateway for Claude Code / Hermes-Agent

## Quick start

**Docker (recommended)**:

```bash
# Pull the published image (or build locally: docker build -t lv-sandbox:0.1.0 .)
docker pull ghcr.io/lv-agent/lv-sandbox:v0.1.0
docker run -d --name sandbox -p 8080:8080 \
  --read-only --tmpfs /tmp:rw,nosuid,nodev,size=1g \
  -v /safe/worker/sandboxes:/sandboxes:rw \
  --cap-drop=ALL --security-opt no-new-privileges \
  --pids-limit=1000 --memory=4g --cpus=4 --user 10000:10000 \
  ghcr.io/lv-agent/lv-sandbox:v0.1.0
```

**Or build from source** (needs `libseccomp-dev` / `libseccomp2`):

```bash
cargo build --workspace --release
./target/release/sandbox-server
```

Run a task:

```bash
curl -X POST http://127.0.0.1:8080/api/v1/submit \
  -H 'content-type: application/json' \
  -d '{"job_id":"demo-1","argv":["/bin/echo","hello"],"profile_name":"shell","timeout":"5s","custom_env":{}}'
```

## Documentation

- 📐 [Architecture](docs/architecture.md) — the design bet, layers, security boundary
- 📖 [Usage guide](docs/usage.md) — build, HTTP API, MCP integration, config reference
- 🇨🇳 中文文档：[README](README.zh.md) · [架构](docs/zh/architecture.md) · [使用指南](docs/zh/usage.md)

## Requirements

- Linux, host kernel ≥ 5.13 (Landlock)
- Docker (the image ships everything else), or Rust 1.75+ for source builds

## License

MIT OR Apache-2.0
