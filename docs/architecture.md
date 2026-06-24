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
| **Landlock** | restricts filesystem access — a task can only read/write its own workspace; **/proc is scoped** to its own `/proc/self` + global info (cpuinfo/meminfo), not other tasks' `/proc/<pid>` |
| **seccomp** | blocks dangerous syscalls (mount, ptrace, bpf, unshare, reboot, io_uring, …) **and restricts `socket()` to `AF_UNIX` only — a task cannot create a TCP/UDP socket at all; controlled egress is opt-in via an allowlisted UDS SOCKS5 proxy** (see [network-isolation.md](network-isolation.md)) |
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
- **Making network connections** — `socket(AF_INET, …)` is killed (seccomp); tasks cannot phone home, reach the cloud metadata service (169.254.169.254), or open listeners. Controlled, allowlisted egress is opt-in per profile via a UDS SOCKS5 proxy.
- **Snooping other tasks via /proc** — /proc is scoped: a task can only read its own `/proc/self` + global info (cpuinfo/meminfo), not other tasks' `/proc/<pid>` (cmdline/maps/environ)
- Inheriting the runner's secret env vars or leaked fds
- **Leaking read secrets into agent context** — `stdout`/`stderr` are redacted before being returned (Bearer/AWS/GitHub tokens, PEM private keys)

### What it does NOT fully stop

Hardened malicious code exploiting kernel bugs, advanced container escape,
strong multi-tenant isolation, all side channels. If the task source is fully
untrusted and the bar is high, use MicroVM / gVisor / Kata / one-container-per-task.

> **Network isolation is enforced at `socket()` creation** — seccomp allows only
> `AF_UNIX`, so a task cannot create a TCP/UDP socket at all (not a bypassable
> denylist of high-level calls). Controlled egress is opt-in per profile through an
> allowlisted UDS SOCKS5 proxy. Full details: [network-isolation.md](network-isolation.md).

The full threat model (what's stopped, what isn't, `fail-closed` behavior, and the
hardened deployment template) lives in [**security.md**](security.md).

---

## Further documentation

- [usage.md](usage.md) — build, run, configuration, tutorial
- [api.md](api.md) — HTTP API reference (endpoints, schemas, status codes)
- [security.md](security.md) — threat model & deployment hardening
- [network-isolation.md](network-isolation.md) — egress model deep-dive
