# HTTP API reference

Base path: `/api/v1`. Content type: `application/json` for request bodies and
JSON responses. All endpoints are unauthenticated by default (put the server
behind your own auth/network boundary in production).

Jobs are **asynchronous**: submitting returns a `job_id` immediately; poll
`GET /jobs/{id}` for the result. See [usage.md](usage.md#submit-a-task-async) for
a tutorial-style walkthrough.

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

**Errors**: `404 Not Found` — task unknown or already evicted.

---

## Cancel a task

### `POST /api/v1/jobs/{job_id}/cancel`

Cancel a running task. The process group receives `SIGTERM` then `SIGKILL`.

**Responses**

| Status | Body | When |
|---|---|---|
| `200 OK` | `{"job_id": "...", "status": "Cancelled"}` | cancelled |
| `404 Not Found` | `{"error": "任务不存在"}` | unknown job |
| `409 Conflict` | `{"error": "任务已完成,无法取消"}` | already finished |

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

Prometheus text format (`text/plain; version=0.0.4`). Exposes job counters,
running gauge, and fork/exec duration histogram.

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
- **Completed jobs are eventually evicted** from the in-memory job table; poll
  promptly rather than hours later.
