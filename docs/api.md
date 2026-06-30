# HTTP API reference

Base path: `/api/v1`. Content type: `application/json` for request bodies and
JSON responses. See [usage.md](usage.md) for a tutorial-style walkthrough.

## Authentication

By default the API has no auth. Set `server.api_key` to require
`Authorization: Bearer <key>` on `/api/v1/*` and `/metrics` (`/health` stays
open for probes). Missing or wrong credentials → `401 {"error":"unauthorized"}`.
If enabled, `sandbox-mcp` must set `SANDBOX_API_KEY` to the same value.

Jobs are **asynchronous**: submitting returns a `job_id` immediately; poll
`GET /jobs/{id}` for the result.

---

## Submit a task

### `POST /api/v1/jobs`

Submit a task for background execution. Returns `202 Accepted` with the `job_id`.

**Request body**

| Field | Type | Required | Notes |
|---|---|---|---|
| `job_id` | string | yes | caller-chosen id; returned as-is and used in polling/cancel |
| `argv` | string[] | yes | `argv[0]` is the executable; must be an absolute path (task `PATH` is minimal) |
| `profile_name` | string | yes | a registered profile (see `GET /profiles`) |
| `timeout` | string | no | e.g. `"5s"`, `"100ms"`, `"1m"`, or a bare number (seconds); defaults to the profile's `default_timeout` |
| `custom_env` | object | no | extra env vars for the task (a small allowlist is passed through, e.g. `TZ`, `SSL_CERT_FILE`) |
| `stdin` | string | no | UTF-8 text piped to the task's stdin |
| `dry_run` | bool | no | if `true`, do not execute — return the profile's limits (incl. `egress_allowlist`) |

**Response `202 Accepted`** (normal submit)

```json
{ "job_id": "demo-1", "status": "Running" }
```

**Response `200 OK`** (`dry_run: true`) — `DryRunSummary`

```json
{
  "profile": "python",
  "dry_run": true,
  "default_timeout_secs": 5,
  "max_stdout_mb": 5,
  "landlock": "Python",
  "fail_closed": false,
  "egress_allowlist": [ { "host": "pypi.org" }, { "host": "files.pythonhosted.org", "port": 443 } ]
}
```

**Errors**

| Status | When |
|---|---|
| `400 Bad Request` | invalid `timeout` format; body `{"error": "..."}` |
| `404 Not Found` | `dry_run: true` but profile does not exist |

---

## Query a task

### `GET /api/v1/jobs/{job_id}`

Poll status/result. **`stdout`/`stderr` are redacted** before being returned (see
[usage.md](usage.md#output-redaction)).

**Response `200 OK`** — `JobResponse`

While running (fields beyond `job_id`/`status` are omitted):

```json
{ "job_id": "demo-1", "status": "Running" }
```

When done:

```json
{
  "job_id": "demo-1",
  "status": "Completed",
  "exit_code": 0,
  "signal": null,
  "stdout": "hello\n",
  "stderr": "",
  "duration_ms": 12,
  "timed_out": false
}
```

**`status` values**

| Value | Meaning |
|---|---|
| `Running` | still executing |
| `Completed` | exited normally (any exit code, incl. non-zero) |
| `TimedOut` | killed on timeout |
| `Killed` | killed by a signal (e.g. seccomp `SIGSYS` violation, external signal) |
| `Cancelled` | cancelled via `POST /jobs/{id}/cancel` |
| `Error` | sandbox/init error |

> **Sandbox violations (cr-041)**: a `Killed` result may carry
> `sandbox_violations` detailing the cause — `SeccompDenied` (SIGSYS: the task
> called a blocked syscall) or `OomKill` (cgroup OOM). These surface in the
> result, metrics (`sandbox_job_seccomp_denied_total` / `sandbox_job_oom_killed_total`),
> and the audit log.

**Errors**: `404 Not Found` — task unknown or already evicted.

---

## Cancel a task

### `POST /api/v1/jobs/{job_id}/cancel`

Cancel a running task. The process group receives `SIGTERM` then `SIGKILL`.

**Responses**

| Status | Body | When |
|---|---|---|
| `200 OK` | `{"job_id": "...", "status": "Cancelled"}` | cancelled |
| `404 Not Found` | `{"error": "job not found"}` | unknown job |
| `409 Conflict` | `{"error": "job already finished, cannot cancel"}` | already finished |

---

## Streaming (SSE)

Add `?stream=true` to `POST /jobs` (or `POST /sessions/{id}/exec`) to receive a
`text/event-stream` of live stdout instead of a `job_id`: events `started` →
`stdout` (one per chunk) → `result` (final `JobResult`, then the stream closes).
stderr is **not** streamed (only in `result`).

---

## Sessions (persistent sandboxes)

A session is a long-lived workspace + bound profile, surviving across `exec`
calls and worker restart. See [usage.md](usage.md#sessions-persistent-sandboxes).

### `POST /api/v1/sessions`

```json
{ "profile_name": "shell", "env": {}, "from_snapshot": null,
  "volumes": [{"name":"data","mount":"volumes/data"}] }
```

→ `201 {"session_id": "..."}`. `from_snapshot` forks from a snapshot; `volumes`
mounts persistent volumes. The profile is **bound at create**.

### `GET /api/v1/sessions` · `GET /api/v1/sessions/{id}` · `DELETE /api/v1/sessions/{id}`

List, status, destroy.

### `POST /api/v1/sessions/{id}/exec`

Run a command in the session's persistent workspace (shares files across calls).
Body like `POST /jobs`; supports `?stream=true`. Execs in a session are
**serialized** (one at a time).

### Session files

| Method | Path | Purpose |
|---|---|---|
| `PUT` | `/sessions/{id}/files/{path}` | upload (raw bytes) |
| `GET` | `/sessions/{id}/files/{path}` | download |
| `GET` | `/sessions/{id}/files?path=` | list |
| `DELETE` | `/sessions/{id}/files/{path}` | delete |

Paths are confined to the workspace (`..`/absolute → `400`).

---

## Snapshots

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/sessions/{id}/snapshot` | snapshot → `201 {"snapshot_id":"..."}` |
| `GET` | `/snapshots` | list |
| `DELETE` | `/snapshots/{id}` | delete |

A snapshot is a full copy of a session's workspace; create a session with
`from_snapshot` to fork. Survives restart.

---

## Volumes

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/volumes` `{name}` | create |
| `GET` | `/volumes` | list |
| `DELETE` | `/volumes/{name}` | delete |

A named persistent directory mounted into sessions (read-write, via symlink +
landlock); survives session destroy and restart.

---

## Worker status

### `GET /api/v1/status`

```json
{ "running_jobs": 3, "max_concurrent": 100, "uptime_secs": 4521 }
```

---

## Profiles

### `GET /api/v1/profiles`

```json
{ "profiles": ["shell", "python", "node"] }
```

### `POST /api/v1/reload`

Hot-reload the config file (update profiles without restart). **Fail-closed**:
any invalid profile aborts the whole reload.

| Status | Body |
|---|---|
| `200 OK` | `{ "success": true, "message": "...", "profiles_loaded": [...] }` |
| `500` | `{ "success": false, "message": "...", "profiles_loaded": [] }` (invalid profile) |

---

## Health & metrics

### `GET /health`

Readiness check — reports which security mechanisms are actually active in this
environment:

```json
{
  "status": "ok",
  "landlock": { "supported": true, "abi_version": 5 },
  "cgroup": { "available": true, "controllers": ["Memory", "Cpu", "Pids"] },
  "seccomp": true,
  "disk_watermark_ok": true
}
```

### `GET /metrics`

Prometheus text format (`text/plain; version=0.0.4`).

| Metric | Type | Description |
|--------|------|-------------|
| `sandbox_job_started_total` | counter | jobs started |
| `sandbox_job_finished_total` | counter | jobs finished |
| `sandbox_job_timeout_total` | counter | jobs timed out |
| `sandbox_running_jobs` | gauge | currently running jobs |
| `sandbox_fork_exec_duration_seconds` | histogram | fork→exec latency (buckets: 1ms–100ms) |
| `sandbox_job_seccomp_denied_total` | counter | jobs killed by seccomp (SIGSYS) |
| `sandbox_job_oom_killed_total` | counter | jobs killed by cgroup OOM killer |
| `sandbox_job_queue_depth` | gauge | jobs waiting for semaphore permit |
| `sandbox_rate_limit_denied_total` | counter | requests rejected by per-IP rate limit (cr-042) |

---

## Duration format

`timeout` / `default_timeout` accept:

- `5s` — seconds
- `100ms` — milliseconds
- `1m` — minutes
- a bare number — seconds (e.g. `"30"`)

## Notes

- **`argv[0]` must be absolute.** The task environment is minimal (`PATH` is
  `/usr/bin:/bin`); resolve binaries to their full path.
- **`custom_env` is allowlisted**, not a free pass-through — only known-safe vars
  plus your extras are set.
- **`dry_run: true`** on `POST /jobs` validates without executing — returns the
  profile's limits (timeout, landlock, max stdout, fail_closed, egress allowlist,
  disk_quota_mb).
- **`disk_quota_mb`** (per profile) caps a task's aggregate workspace usage; a
  task that exceeds it is reaped with `status: "DiskQuotaExceeded"`.
- **`io.max`** (built-in profiles, cgroup v2) throttles disk I/O rate (200 MB/s
  read / 100 MB/s write by default) — prevents bandwidth starvation.
- **`seccomp_mode`** (per profile, opt-in, cr-045) — set `seccomp_mode: allowlist`
  to flip from the default denylist to default-deny + an observed syscall
  allowlist (stronger; `shell` + `python` in Phase 1/2). An incomplete allowlist kills the
  task with `SeccompDenied` (SIGSYS); `fail_closed` is auto-enabled.
- **`list_files: true`** on session exec returns a `files` array (path + size +
  MIME) in the response — see what was produced without a separate `files ls`.
- **`POST /sessions/{id}/exec` + `?stream=true`** → SSE (same as `POST /jobs`).
- **`GET /sessions/{id}/tty`** → WebSocket upgrade for interactive PTY (stdin ↔
  stdout over a pseudo-terminal).
- **Completed jobs are eventually evicted** from the in-memory job table; poll
  promptly rather than hours later. (Sessions/snapshots/volumes persist on disk.)
