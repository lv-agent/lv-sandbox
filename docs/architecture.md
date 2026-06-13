# Architecture

## Why it exists

AI agents executing external commands face real risk: deleting files, reading
secrets, infinite loops, fork bombs, dangerous syscalls. The common approach is
**one container per task**, but containers are slow to start and heavy on
resources — a poor fit for many lightweight tasks (e.g. running 100 small scripts
concurrently).

lv-sandbox takes a different trade-off: **run many isolated tasks concurrently
inside a single long-lived worker**, isolating each task with Linux kernel
security mechanisms (Landlock, seccomp, cgroup, rlimit) rather than a full
container. Light, fast, high-concurrency, while keeping strong isolation.

```text
Traditional: one task → one container (heavy)
lv-sandbox:   one worker → many tasks, each kernel-isolated (light)
```

> This targets agent mistakes and casual privilege escalation. If the task source
> is fully untrusted and the bar is high, use MicroVM / gVisor / Kata. See
> [Security boundary](#security-boundary).

---

## High-level architecture

```text
┌───────────────────────────────────────────────┐
│  Access layer                                  │
│  sandbox-server (HTTP)   sandbox-mcp (MCP)     │
└──────────────┬────────────────────┬───────────┘
               │                     │
               ▼                     ▼
┌───────────────────────────────────────────────┐
│  Scheduling   concurrency · metrics · reload   │
└──────────────────┬────────────────────────────┘
                   ▼
┌───────────────────────────────────────────────┐
│  Core   sandbox-core                           │
│  task execution · profile · workspace · life   │
└──────────────────┬────────────────────────────┘
                   ▼
        Landlock · seccomp · cgroup  (security primitives)
```

- **Core** (`sandbox-core`): owns the full task lifecycle, composes all security
  mechanisms.
- **Scheduling** (inside `sandbox-server`): concurrency limiting, metrics
  collection, hot config reload.
- **Access**: `sandbox-server` exposes HTTP; `sandbox-mcp` is a gateway that
  translates an AI Agent's MCP protocol into HTTP calls.
- **Security primitives**: three standalone crates, each wrapping one kernel
  security mechanism, reusable independently.

### Two ways in

```text
HTTP:   curl / app  ──HTTP──▶  sandbox-server
MCP:    AI Agent ──stdio──▶ sandbox-mcp ──HTTP──▶ sandbox-server
```

`sandbox-mcp` holds no sandbox engine — it only converts protocols. This lets
multiple AI Agents share a single sandbox-server, reusing its concurrency control
and metrics, without each maintaining its own engine.

---

## Security mechanisms

Each task runs in its own process group, layered with six isolation mechanisms:

| Mechanism | Effect |
|---|---|
| **Landlock** | restricts filesystem access — a task can only read/write its own workspace |
| **seccomp** | blocks dangerous syscalls (mount, ptrace, bpf, unshare, reboot, …) **and all network socket syscalls — default no-network: tasks cannot make outbound connections, listen, or send/receive traffic** |
| **rlimit** | caps process-level resources (CPU, file count, process count, file size) |
| **cgroup v2** | caps real task resource use (memory, CPU, pids); degrades gracefully if unavailable |
| **Process hardening** | NoNewPrivs disables privilege escalation, setsid detaches the controlling terminal, leaked fds closed, env allowlist |
| **Timeout reaping** | on timeout the whole group gets SIGTERM → SIGKILL — no orphans |

These are applied at runtime after **capability detection** of the actual
environment: Landlock probes its ABI version, seccomp probes availability, cgroup
probes controllers. When something is unsupported, the profile decides whether to
refuse execution (fail-closed) or degrade and continue (fail-open).

---

## Security boundary

### What it stops

- A task reading/writing another task's files
- A task reading sensitive files in the container, unnecessary `/sys`, `/proc`
- Infinite loops, fork bombs, fd exhaustion
- Filling up the workspace (resource limits + alerts)
- Spawning background processes to escape timeouts (whole-group cleanup)
- Calling dangerous syscalls
- **Making network connections** — all network socket syscalls are blocked by default (seccomp); tasks cannot phone home, reach the cloud metadata service (169.254.169.254), or open listeners
- Inheriting the runner's secret env vars or leaked fds

### What it does NOT fully stop

Hardened malicious code exploiting kernel bugs, advanced container escape,
strong multi-tenant isolation, all side channels. If the task source is fully
untrusted and the bar is high, use MicroVM / gVisor / Kata / one-container-per-task.

> **Network isolation today is a seccomp denylist, not a kernel-level cutoff.** It
> blocks programs that go through the standard libc `socket()`/`connect()` path, but
> a denylist is inherently not exhaustive. The stronger model — per-task network
> namespace (no NIC) plus a UDS egress proxy with domain allowlist + traffic audit —
> is planned (cr-017). For now, treat it as "default no-network for ordinary programs."

### Recommended deployment

Wrap the worker in an outer container (a worker container, not a per-task
container) for a boundary, and run sandbox-server as non-root inside it:

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

Rules: never `--privileged`, never mount the Docker socket or sensitive dirs,
read-only rootfs, run non-root, tmpfs for `/tmp`, only `/sandboxes` writable.
