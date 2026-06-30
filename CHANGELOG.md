# Changelog

All notable changes to lv-sandbox are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/) (0.x: breaking changes
are allowed in minor bumps).

## [Unreleased]

### Added

- **seccomp allowlist mode (cr-045)** — profiles can opt into `seccomp_mode:
  allowlist` (default-deny + observed syscall allowlist, stronger than the
  default denylist; Phase 1/2 ship shell + python, node follows).
- **TypeScript/JS SDK (cr-044)** — `lvsandbox` npm package, zero dependencies.
- **Graceful shutdown (cr-043)** — on `SIGTERM`, the server waits for in-flight
  jobs to finish (configurable timeout, default 30s) before exiting.
- **Per-IP rate limiting (cr-042)** — fixed-window rate-limit middleware
  (`DashMap`), default off; `/health` is exempt, keyed per client IP.
- **Metrics hardening (cr-041)** — three new Prometheus metrics
  (`sandbox_job_seccomp_denied_total`, `sandbox_job_oom_killed_total`,
  `sandbox_job_queue_depth`); violation detection upgraded
  (SIGSYS → `SeccompDenied`, cgroup OOM → `OomKill`), wired into reporting.
- **Session TTL auto-reaping (cr-040)** — sessions idle past their TTL are
  destroyed automatically.
- **OpenTelemetry tracing (cr-039)** — OTel trace export (OTLP/HTTP via the
  `tracing-opentelemetry` bridge), default off.
- **Resource-usage reporting (cr-038)** — cgroup `memory_peak` / `cpu_usage` /
  `pids` surfaced in `ResourceSummary`.

### Changed

- The session `exec` path now emits Prometheus metrics, aligned with the
  scheduler (cr-041).

## [0.4.0] — 2026-06-27

### Highlights

Developer-experience release: a first-class **Python SDK** (`lvsandbox`), a full
**CLI** (`lvs`), **interactive PTY terminals**, **code-interpreter** file
listing, **agent-framework** tool schemas, auto-setup **templates**, and
default-on **disk IO rate limits** (cgroup `io.max`). The sandbox is now usable
end-to-end without writing any HTTP calls by hand.

### Added

- **Python SDK (cr-030)** — `lvsandbox` package: jobs, sessions, files,
  snapshots, volumes, and SSE streaming.
- **Lifecycle webhooks (cr-031)** — terminal job events POSTed to your URL with
  3 retries; default off.
- **CLI `lvs` (cr-032)** — manage jobs, sessions, exec, files, snapshots, and
  volumes from the terminal (`clap` + `reqwest`).
- **Interactive PTY (cr-033)** — WebSocket terminal via `openpty` +
  `TIOCSCTTY` on the server, `lvs shell <id>` on the client; for REPLs and
  debugging.
- **Code-interpreter file listing (cr-034)** — `exec` with `list_files: true`
  returns the workspace file listing with MIME types (charts, data, HTML).
- **Agent-framework integration (cr-035)** — SDK gains `run_python()`, an
  OpenAI tool schema, and LangChain tool wrappers.
- **Templates (cr-036)** — `templates` config section runs setup commands at
  startup to pre-install environments and registers them as profiles.
- **Disk IO rate limits (cr-037)** — cgroup `io.max` enabled by default
  (200 MB/s read + 100 MB/s write) with automatic block-device detection.

### Changed

- README rewritten with a 30-second start (SDK + CLI), an architecture diagram,
  grouped features, and a live security demo (EN + ZH).
- Usage guide gained a 6-step, 5-minute quickstart (EN + ZH).

## [0.3.0] — 2026-06-26

### Highlights

Sessions release: a full **E2B-style persistent-session model** lands —
sessions, **snapshots** (fork), **volumes** (cross-session persistence), and
**cross-restart reconnect** — plus the production plumbing underneath:
**audit logging**, **per-task disk quotas**, **API-key auth**, **streaming
stdout** (SSE), and **templates/env presets**.

### Added

- **Persistent sessions + file I/O (cr-026)** — long-lived workspaces with
  multiple `exec` calls, file upload/download, and path-traversal protection;
  new `/api/v1/sessions` resource.
- **Session snapshots (cr-027)** — full workspace copy; fork new sessions from a
  snapshot; `/sessions/.../snapshot` + `/snapshots` CRUD.
- **Volumes (cr-028)** — named directories that persist across sessions via
  symlink + landlock `ReadWrite`; `/volumes` CRUD + `create_session.volumes`.
- **Cross-restart reconnect (cr-029)** — session metadata persisted to disk;
  `rebuild_from_disk` on startup reconnects sessions, snapshots, and volumes
  (re-authorizes volume landlock on rebuild).
- **Audit logging (cr-021)** — JSONL audit of the job lifecycle
  (`AuditEventType` aligned with `JobStatus`, argv/exit_code fields); default
  off.
- **Per-task disk quota (cr-022)** — `disk_quota_mb` field (opt-in) with a
  watchdog (`select!` third branch) that reaps oversize workspaces; new
  `JobStatus::DiskQuotaExceeded`; surfaced in `dry_run`.
- **Bearer API-key auth (cr-023)** — `server.api_key` + `from_fn_with_state`
  middleware with constant-time comparison; MCP `with_api_key` +
  `SANDBOX_API_KEY` env; default off.
- **Streaming stdout / SSE (cr-024)** — `POST /jobs?stream=true` returns live
  `StreamEvent`s via SSE.
- **Templates & env presets (cr-025)** — `profile.env` baseline; sanitized env
  built in three priority stages; `build-template.sh` helper.
- **Runtime image (cr-020)** — the Docker image now ships `python3`, `node`,
  `requests`, and `httpx`.

### Fixed

- MCP tool argument-schema descriptions switched from Chinese to English.

## [0.2.1] — 2026-06-25

### Added
- The runtime Docker image now ships **`curl`** (for the "phone home → `Killed`" demo and
  ad-hoc debugging). The README quick demo's network example works against the published
  image out of the box.

### Fixed
- README quickstart `docker run` switched `/sandboxes` to a `tmpfs` (`uid=10000`) — the
  previous host-volume mount needed a manual `chown 10000:10000` or every job errored with
  `Permission denied` at workspace creation. The demo is now self-contained.

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
