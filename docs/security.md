# Security model & threat boundary

This document states **what lv-sandbox is designed to stop, what it assumes, and
what it deliberately does not promise**. It is the authoritative reference for
the security posture; [architecture.md](architecture.md) gives the component
view and [network-isolation.md](network-isolation.md) covers egress in depth.

## Threat model

**In scope:** containing *AI-agent mistakes* and *casual privilege escalation* —
a task that accidentally (or naively) tries to delete files it shouldn't, read
secrets, fork-bomb, phone home, or run a known-dangerous syscall. The goal is to
make the blast radius of a misbehaving lightweight task small, cheaply, at high
concurrency, inside a single long-lived worker.

**Out of scope (explicitly):** hardened malicious code exploiting kernel
vulnerabilities, advanced container escapes, strong multi-tenant isolation, and
all side channels (timing, Rowhammer, …). If the task source is **fully
untrusted** and the bar is high, this is the wrong tool — use MicroVM / gVisor /
Kata / one-container-per-task.

> lv-sandbox layers Linux kernel primitives (Landlock, seccomp, cgroup) inside
> **one** worker. It is defense-in-depth for agent workloads, not a hard sandbox
> against a determined adversary with kernel exploits.

## Defense layers

Each task runs in its own process group, layered with these mechanisms (applied
in `pre_exec` after environment capability detection):

| Layer | What it does |
|---|---|
| **Landlock** | Filesystem confinement: a task can only read/write its own workspace (+ a read-only global set). `/proc` is scoped to its own `/proc/self` plus global info files (cpuinfo/meminfo), not other tasks' `/proc/<pid>`. |
| **seccomp** | Denies dangerous syscalls (mount, ptrace, bpf, unshare, reboot, io_uring, …) **and restricts `socket()` to `AF_UNIX` only** — see [network-isolation.md](network-isolation.md). |
| **cgroup v2** | Caps real resource use (memory, CPU, pids). Degrades to rlimit if cgroup v2 is unavailable. |
| **rlimit** | Process-level caps (CPU seconds, fd count, process count, file size, core disabled). |
| **Process hardening** | `NoNewPrivs` (blocks privilege escalation), `setsid` (detach controlling tty), inherited fds closed, **env allowlist** (the runner's secrets never reach the task). |
| **Timeout reaping** | On timeout/cancel the whole process group gets `SIGTERM` → `SIGKILL`; no orphaned background processes. |
| **Output redaction** | `stdout`/`stderr` returned to the caller are scrubbed of common secret patterns (Bearer tokens, AWS `AKIA` keys, GitHub tokens, PEM private keys) so credentials a task reads don't leak into agent context. |

## What it stops

- A task reading/writing another task's files, or sensitive host files.
- Snooping other tasks via `/proc` (`cmdline`/`maps`/`environ`).
- Fork bombs, fd exhaustion, infinite CPU (resource caps + timeout).
- Filling the workspace (resource limits + disk watermark admission).
- Escaping timeouts via background processes (whole-group cleanup).
- Calling dangerous syscalls.
- **Making network connections** — `socket(AF_INET, …)` is killed; egress is
  only possible through an allowlisted UDS SOCKS5 proxy (opt-in per profile).
- Inheriting the runner's secret env vars or leaked fds.

## What it does NOT stop

- Kernel exploit-based escapes (Landlock/seccomp/cgroup are kernel features; a
  kernel bug can subvert them).
- Side-channel attacks.
- Strong multi-tenant isolation between mutually hostile tenants.
- **Content inspection** of egress — the egress proxy is a transparent relay; it
  enforces destination allowlists but does not decrypt or audit TLS payloads.
- Local chatter between sandboxed tasks over abstract-namespace UDS (a contained
  side-channel, not an egress path).

## fail-closed vs fail-open

Each security mechanism is gated on a runtime **capability probe**. When a
mechanism is unavailable (e.g. Landlock unsupported, cgroup v2 absent), the
profile's `fail_closed` flag decides:

- `fail_closed: true` → **refuse to run** the task (safe default for profiles
  that *require* the mechanism).
- `fail_closed: false` → degrade (e.g. cgroup → rlimit) and continue.

seccomp and Landlock, when the environment supports them, are always applied
regardless of allowlist — the network allowlist only governs whether the egress
proxy is started, not whether seccomp is enforced.

## Recommended deployment

Wrap the worker in an **outer container** (a worker container, not a per-task
one) and run `sandbox-server` as non-root inside it:

```bash
docker run -d --name sandbox \
  -p 8080:8080 \
  --read-only --tmpfs /tmp:rw,nosuid,nodev,size=1g \
  -v /safe/worker/sandboxes:/sandboxes:rw \
  --cap-drop=ALL --security-opt no-new-privileges \
  --pids-limit=1000 --memory=4g --cpus=4 \
  --user 10000:10000 \
  your-worker-image
```

Rules: never `--privileged`; never mount the Docker socket or sensitive host
dirs; read-only rootfs; run non-root; tmpfs for `/tmp`; only `/sandboxes`
writable. No extra capabilities are needed for the network egress model (it is
seccomp-based, not netns-based).

## Operational notes

- **No built-in auth** on the HTTP API — place the server behind your own
  auth/network boundary in production.
- **Completed jobs are evicted** from the in-memory table. For a durable record,
  enable `server.audit` (a JSONL audit trail of every job's command + outcome);
  otherwise capture results/`/metrics` externally.
- **Host kernel ≥ 5.13** is required for Landlock.
