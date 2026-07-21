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

**seccomp mode (opt-in, cr-045).** The default is a *denylist* — allow all
syscalls, kill a blocklist of dangerous ones. A profile may instead set
`seccomp_mode: allowlist`: default-deny (`KillProcess`) plus an observed
allowlist of the syscalls the runtime actually needs. This closes the denylist's
"any newly-added dangerous syscall is allowed by default" gap, at the cost of
maintaining a complete per-runtime allowlist — an incomplete one kills the task
with `SeccompDenied` / SIGSYS (observable, not silent). `fail_closed` is forced
on for allowlist profiles. Phase 1/2/3 ship shell + python + node allowlists.
**Node.js 22+ and io_uring (cr-047):** node 22+ libuv probes `io_uring_setup` at startup.
The **denylist** (default) lets `io_uring_setup` pass seccomp and relies on the **host
`kernel.io_uring_disabled=2`** to return ENOSYS at the kernel layer — libuv then falls
back to epoll. Set it on the host in production: `sysctl -w kernel.io_uring_disabled=2`
(without it io_uring is usable by tasks = escape surface). **allowlist mode** (default
KILL) cannot support node 22+ — a kernel seccomp bug makes default-KILL + ERRNO rules
ineffective, so node 22+ under allowlist is killed; use denylist + sysctl, or node 18/20
(no io_uring). See `veps/cr-047-libseccomp-issue.md`.

## What it stops

- A task reading/writing another task's files, or sensitive host files.
- Snooping other tasks via `/proc` (`cmdline`/`maps`/`environ`).
- Fork bombs, fd exhaustion, infinite CPU (resource caps + timeout).
- Filling the workspace (resource limits + disk watermark admission + opt-in
  per-task `disk_quota_mb` watchdog that reaps a task whose workspace exceeds
  the cap).
- **Session file-I/O escape** — uploads/downloads/listing via the session API
  are confined to the session workspace (`..`/absolute paths rejected); a
  volume is granted read-write by Landlock only for the operator-declared
  volume directory.
- Escaping timeouts via background processes (whole-group cleanup).
- Calling dangerous syscalls.
- **Making network connections** — `socket(AF_INET, …)` is killed; egress is
  only possible through an allowlisted UDS SOCKS5 proxy (opt-in per profile).
- Inheriting the runner's secret env vars or leaked fds.

### Git egress & the sentinel credential model (CR-12)

The [`git` profile](usage.md#git-access-git-profile-cr-12) opts a task into
controlled egress so it can `git clone` / `push`. The security-relevant points
(live enforcement, not cooperation):

- **Invariant 1 — an in-jail credential cannot bypass the proxy.** Even if a
  task obtains a credential and tries to phone-home directly to `github.com`,
  the attempt dies at `socket()`: seccomp `deny_network()` kills any
  `socket(domain != AF_UNIX)` (`KillProcess` / `SIGSYS`). git never gets a raw
  INET socket — all of its network goes through the `git-remote-fixus` helper,
  which itself only dials the per-job SOCKS5h UDS proxy. This proves the
  in-jail credential is a true sentinel, not a disguised real token: a real
  token still has no socket to ride out on. Automated in
  `crates/sandbox-seccomp` (`deny_network` → single `socket` rule,
  `domain != AF_UNIX` killed) and the cr-019 e2e suite.

- **Invariant 2 — real credentials exist only in the proxy process.** This is a
  design guarantee: the real token is held by the **credential-exit proxy**
  *outside* the jail, never injected into the task. Inside the jail git runs its
  standard credential flow against a **fake / sentinel** value; the exit proxy
  recognizes the sentinel, swaps it for the real token, and forwards to
  `github.com:443`. The task therefore has no real credential to exfiltrate
  even if it is fully compromised.

- **Allowlist 收口 (destination closure).** A non-allowlisted host is rejected
  by the proxy with SOCKS5 `REP = 0x02` (*connection not allowed by ruleset*),
  and IP-literal `ATYP`s are rejected outright (forcing hostname / remote DNS,
  keeping the allowlist meaningful). Automated: `sandbox_core::proxy::non_allowlisted_denied`,
  `sandbox_core::proxy::proxy_rejects_ipv4_literal`, and
  `git-remote-fixus::dialer::non_allowlisted_host_is_denied`.

- **Invariant 3 — wrong/missing sentinel → 401, no upstream contact.** The
  swap-proxy compares the bearer token against the expected sentinel
  (case-sensitive); on mismatch or absence it returns `401 Unauthorized`
  **without** opening any connection to the upstream. The real token is never
  sent on a request that did not first prove possession of the sentinel.

**G2 (2026-07-21) — the sentinel seam is now formalized and shipped in-tree.**
A **reference swap-proxy** lives at `crates/egress-swap-proxy` (binary
`fixus-egress-swap-proxy`): out-of-jail, TLS-terminates the jail helper's
connection, recognizes the sentinel, swaps it for the real token, and forwards
to the real upstream (default `github.com:443`). The real token exists **only**
in that process. The in-tree SOCKS proxy (`crates/sandbox-core/src/proxy.rs`)
stays a transparent relay (unchanged) — it is a separate component from the
swap-proxy. All three invariants above are exercised end-to-end by the
`egress_swap_proxy` and cr-019 test suites.

CR-12 still defines the jail-side allowlist shape (`FIXUS_GIT_EGRESS_HOST` /
`FIXUS_GIT_EGRESS_PORT`, default `github.com:443`); **G2 adds the swap-proxy
seam contract that the allowlist points at.** The production swap-proxy **can
still be operator-implemented** against the same contract — the in-tree one is a
reference, not a hard dependency. Operators point
`FIXUS_GIT_EGRESS_HOST`/`FIXUS_GIT_EGRESS_PORT` at whichever swap-proxy they
run, and must supply a real TLS cert: the reference proxy **fail-closes** if
`FIXUS_SWAP_CERT_PEM` / `FIXUS_SWAP_KEY_PEM` are absent — there is no
self-signed fallback in the binary. Env contract and wiring recipe in
[usage.md · Credentials: sentinel + exit-proxy swap](usage.md#credentials-sentinel--exit-proxy-swap);
data path in
[network-isolation.md · Git over the egress proxy](network-isolation.md#git-over-the-egress-proxy-cr-12).

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

- **Optional Bearer API key** on the HTTP API — set `server.api_key` to require
  `Authorization: Bearer <key>` on `/api/v1/*` and `/metrics` (off by default;
  `/health` stays open for probes). Even with it on, place the server behind
  your own network boundary in production.
- **Completed jobs are evicted** from the in-memory table. For a durable record,
  enable `server.audit` (a JSONL audit trail of every job's command + outcome);
  otherwise capture results/`/metrics` externally.
- **Host kernel ≥ 5.13** is required for Landlock.
