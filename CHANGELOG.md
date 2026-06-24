# Changelog

All notable changes to lv-sandbox are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/) (0.x: breaking changes
are allowed in minor bumps).

## [0.2.0] — 2026-06-25

### Highlights

Network isolation upgraded from a syscall denylist ("default no-network") to
**kernel-enforced, allowlisted controlled egress**, and the HTTP API is now fully
asynchronous with first-class cancellation.

### Added

- **Controlled egress (cr-019)** — seccomp now restricts `socket()` to `AF_UNIX`
  only, so a task cannot create a TCP/UDP socket at all. Profiles can opt into
  egress via an allowlisted SOCKS5h proxy over a per-job UDS (`egress_allowlist`,
  per-profile, default deny). Zero extra privileges. Bundled helpers
  (`helpers/python|node|shell`) with an HTTP and HTTPS path. See
  [docs/network-isolation.md](docs/network-isolation.md).
- **Async job API + cancel (cr-018)** — `POST /jobs` (returns `job_id`
  immediately), `GET /jobs/{id}` (poll), `POST /jobs/{id}/cancel`. Whole-group
  `SIGTERM`→`SIGKILL` reaping, no orphans.
- **stdin** (#72) — `stdin` field piped to the task.
- **Health readiness** (#76) — `/health` reports which security mechanisms are
  active (landlock/cgroup/seccomp) plus the disk watermark.
- **Profile dry-run + validation** (#77) — `dry_run: true` previews a profile's
  limits; invalid profiles fail-closed on load/reload.
- **Output redaction** (#78) — `stdout`/`stderr` scrubbed of common secret
  patterns (Bearer / AWS `AKIA` / GitHub / PEM) before return.
- **Docs** — new [api.md](docs/api.md), [security.md](docs/security.md),
  [network-isolation.md](docs/network-isolation.md); expanded architecture & README.

### Changed

- seccomp `/proc` scope tightened to `/proc/self` + global info files (cr-017) —
  a task can no longer read other tasks' `/proc/<pid>` (`cmdline`/`maps`/`environ`).
- Jobs killed by a signal now report `Killed` (previously `Completed`).
- Code is now English throughout (strings, logs, errors, test names); comments
  remain Chinese.

### ⚠️ Breaking

- The HTTP API replaces the synchronous `POST /api/v1/submit` with the
  asynchronous `/api/v1/jobs` resource (submit → poll → cancel). Clients must
  migrate.
- The network seccomp filter changed from "deny all socket syscalls" to
  "allow only `AF_UNIX`" — `socket(AF_INET, …)` is now killed. Default egress
  behavior (zero) is unchanged.

## [0.1.0]

Initial public release.

- Landlock + seccomp + rlimit + cgroup v2 isolation, synchronous HTTP API,
  `sandbox-mcp` MCP gateway, Docker image + binary tarball distribution.
