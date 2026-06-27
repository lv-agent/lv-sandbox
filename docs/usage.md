# Usage guide

## Quick start (5 minutes)

**Step 1 — Start the server:**

```bash
docker pull ghcr.io/lv-agent/lv-sandbox:v0.3.0
docker run -d --name sandbox -p 8080:8080 \
  --cap-drop=ALL --security-opt no-new-privileges \
  --tmpfs /sandboxes:rw,nosuid,nodev,size=100m,uid=10000,gid=10000 \
  --user 10000:10000 \
  ghcr.io/lv-agent/lv-sandbox:v0.3.0
curl http://127.0.0.1:8080/health     # → {"status":"ok",...}
```

→ See [Build & run](#build--run) for production options (host volumes, source build, etc.)

**Step 2 — Run a command (one-shot job):**

```bash
curl -X POST http://127.0.0.1:8080/api/v1/jobs \
  -H 'content-type: application/json' \
  -d '{"job_id":"d1","argv":["/bin/echo","hello"],"profile_name":"shell","timeout":"5s","custom_env":{}}'
# → {"job_id":"d1","status":"Running"}
curl http://127.0.0.1:8080/api/v1/jobs/d1
# → {"status":"Completed","exit_code":0,"stdout":"hello\n",...}
```

→ See [Submit a task](#submit-a-task-async) for polling, cancel, stdin, dry-run.

**Step 3 — Install the Python SDK and use sessions:**

```bash
pip install -e sdk/python     # from repo, or: pip install lvsandbox
```

```python
from lvsandbox import Client

lv = Client("http://127.0.0.1:8080")

# persistent session — files survive across exec calls
s = lv.sessions.create(profile="python")
s.files.put("hello.py", b"print('from sandbox')")
print(s.exec(["/usr/bin/python3", "hello.py"]).stdout)    # → from sandbox

# code interpreter: run Python + see generated files
r, files = lv.run_python("import matplotlib; print('ok')")
print(r.stdout, [f.path for f in files])
```

→ See [Sessions](#sessions-persistent-sandboxes), [Python SDK](#python-sdk--agent-framework-integration), [Snapshots](#snapshots-fork-a-session), [Volumes](#volumes-persistent-storage).

**Step 4 — Install the CLI:**

```bash
cargo build -p lv-cli
./target/debug/lvs jobs run -- /bin/echo "from CLI"
./target/debug/lvs sessions new --profile shell
./target/debug/lvs exec <id> -- /bin/sh -c 'echo hi > out.txt'
./target/debug/lvs files get <id> out.txt                 # → hi
./target/debug/lvs shell <id> -- /bin/sh                  # interactive terminal
```

→ See [CLI](#cli) for all commands.

**Step 5 — Configure profiles & security:**

```yaml
# config.yaml
server:
  listen_addr: "0.0.0.0:8080"
  api_key: "your-secret"          # Bearer auth (omit = off)
sandbox:
  base_dir: "/sandboxes"
  fail_closed: false
profiles:
  heavy:
    disk_quota_mb: 100            # per-task aggregate workspace cap
    rlimit: { cpu_seconds: 30 }
    default_timeout: "60s"
templates:
  data-science:                   # auto-setup at startup
    setup: "pip install --target /opt/ds pandas numpy"
    env: { PYTHONPATH: "/opt/ds" }
```

→ See [Config reference](#config-reference), [Profiles](#profiles), [Templates](#templates-pre-baked-environments), [Authentication](#authentication), [Disk quota](#disk-quota-per-task), [Controlled egress](#controlled-egress-egress-allowlist).

**Step 6 — Connect Claude Code:**

```bash
# .mcp.json — Claude Code auto-loads this
{ "mcpServers": { "sandbox": { "command": "cargo",
  "args": ["run","-p","sandbox-mcp","--quiet","--"],
  "env": { "SANDBOX_SERVER_URL": "http://127.0.0.1:8080" } } } }
```

→ See [MCP integration](#mcp-integration-claude-code--hermes-agent).

---

## Requirements

- Linux, host kernel ≥ 5.13 (Landlock)
- **Docker**: only Docker is needed — the image ships the rest
- **Source build**: Rust 1.75+, `libseccomp-dev` (build) / `libseccomp2` (run)
- Recommended to run as a non-root user inside a container (see [Architecture · Recommended deployment](architecture.md#recommended-deployment))

## Build & run

Two options: **Docker image** (recommended, turnkey) or build from source.

### Docker (recommended)

The image ships `libseccomp2`, a non-root user (uid 10000) and a default config —
`docker run` and go. Two ways to get it:

**Option A: pull from ghcr.io (fastest)**

```bash
docker pull ghcr.io/lv-agent/lv-sandbox:v0.3.0
docker tag ghcr.io/lv-agent/lv-sandbox:v0.3.0 lv-sandbox:0.3.0   # optional, to reuse the commands below
```

**Option B: build locally**

```bash
# build the image
docker build -t lv-sandbox:0.3.0 .

# or one command that also produces a binary tar.gz (fallback for non-Docker hosts)
bash scripts/build-release.sh
```

**Run the container**:

```bash
docker run -d --name sandbox \
  -p 8080:8080 \
  --read-only --tmpfs /tmp:rw,nosuid,nodev,size=1g \
  -v /safe/worker/sandboxes:/sandboxes:rw \
  --cap-drop=ALL --security-opt no-new-privileges \
  --pids-limit=1000 --memory=4g --cpus=4 \
  --user 10000:10000 \
  lv-sandbox:0.3.0
```

Notes:

- Host Linux kernel ≥ 5.13 (Landlock); Docker's default seccomp (libseccomp 2.5+)
  already permits Landlock syscalls — no extra config needed
- The mounted `/safe/worker/sandboxes` host dir must be writable by uid 10000:
  `chown 10000:10000 /safe/worker/sandboxes`
- The in-image config lives at `/etc/sandbox-server/config.yaml`; override with
  `-v your-config.yaml:/etc/sandbox-server/config.yaml:ro`
- If cgroup v2 is unavailable inside the container it degrades to rlimit
  (the in-image config sets `fail_closed: false`)
- No `--privileged` needed

Health check: `curl http://127.0.0.1:8080/health`

`build-release.sh` produces `dist/lv-sandbox-<version>-x86_64-gnu.tar.gz` containing
`sandbox-server` / `sandbox-mcp` / a sample config / a quick-start note — unpack
and run `./sandbox-server --config config.yaml.example` (needs host `libseccomp2`).

### Build from source

Build needs `libseccomp-dev`, run needs `libseccomp2`.

```bash
cargo build --workspace --release
./target/release/sandbox-server --config config.yaml
```

Config lookup order: `--config` arg > `SANDBOX_CONFIG` env > `/etc/sandbox-server/config.yaml` > built-in default.

---

## HTTP API

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/v1/jobs` | submit a task (async — returns `job_id` immediately, runs in background) |
| `GET` | `/api/v1/jobs/{id}` | query job status/result (`Running` or the final `JobResult`) |
| `POST` | `/api/v1/jobs/{id}/cancel` | cancel a running job |
| `GET` | `/api/v1/status` | worker status (running count, concurrency cap, uptime) |
| `GET` | `/api/v1/profiles` | list available profiles |
| `POST` | `/api/v1/reload` | hot-reload config (update profiles without restart) |
| `GET` | `/metrics` | Prometheus metrics |
| `GET` | `/health` | readiness — landlock/cgroup/seccomp status + disk watermark |

### Authentication

By default the API has no authentication (zero-friction local dev). Set
`server.api_key` to require an `Authorization: Bearer <key>` header on
`/api/v1/*` and `/metrics`; `/health` stays open for readiness probes. Missing
or wrong credentials return `401 {"error":"unauthorized"}` (constant-time
compare). If enabled, `sandbox-mcp` must set `SANDBOX_API_KEY` to the same value,
otherwise the gateway is rejected.

```yaml
server:
  api_key: "secret-token"   # unset = no auth (default)
```

### Submit a task (async)

Submit returns the `job_id` immediately (`202 Accepted`); the task runs in the
background. Poll `GET /jobs/{id}` for the result.

```bash
curl -X POST http://127.0.0.1:8080/api/v1/jobs \
  -H 'content-type: application/json' \
  -d '{
    "job_id": "demo-1",
    "argv": ["/bin/echo", "hello sandbox"],
    "profile_name": "shell",
    "timeout": "5s",
    "custom_env": {}
  }'
```

Response:

```json
{ "job_id": "demo-1", "status": "Running" }
```

The request body also accepts optional fields:
- `stdin` — UTF-8 text piped to the task's stdin (for `cat` or scripts that read input)
- `dry_run: true` — validate without executing; returns the profile's limits
  (timeout, landlock, max stdout, fail_closed) instead of running. Useful for
  CI or previewing which restrictions apply.

Query the result:

```bash
curl http://127.0.0.1:8080/api/v1/jobs/demo-1
```

Response (once done):

```json
{
  "job_id": "demo-1",
  "status": "Completed",
  "exit_code": 0,
  "signal": null,
  "stdout": "hello sandbox\n",
  "stderr": "",
  "duration_ms": 3,
  "timed_out": false
}
```

Cancel a running job:

```bash
curl -X POST http://127.0.0.1:8080/api/v1/jobs/demo-1/cancel
```

`status` values: `Completed`, `TimedOut`, `Killed`, `Cancelled`, `Error`.

### Streaming stdout (SSE)

Add `?stream=true` to get the response as a `text/event-stream` of live stdout
instead of a `job_id`. Events:

- `started` — `{"job_id": "..."}` (first event)
- `stdout` — `{"data": "<chunk>"}` (one per output chunk; UTF-8, binary is lossy)
- `result` — the final `JobResult` (status, exit_code, stdout, stderr, …; last
  event, then the stream closes)

stderr is **not** streamed — it only appears in the `result` event. The job runs
under the full sandbox profile (landlock/seccomp/cgroup/timeout/cancel/quota), is
registered for cancel, and queryable via `GET /jobs/{id}` after the stream.

```bash
curl -N -X POST 'http://127.0.0.1:8080/api/v1/jobs?stream=true' \
  -H 'content-type: application/json' \
  -d '{"job_id":"s","argv":["/bin/sh","-c","for i in 1 2 3; do echo tick $i; sleep 0.2; done"],"profile_name":"shell"}'
```

### Interactive terminal (PTY)

Open a WebSocket to `/api/v1/sessions/{id}/tty?argv=/bin/sh` for an interactive
terminal — stdin/stdout flow bidirectionally over a PTY (raw mode, terminal
signals). The command runs under the full sandbox profile (same as exec).

```bash
# via the CLI:
lvs shell <session-id> -- /bin/sh
```

The server sends a JSON control message `{"type":"exit",...}` when the process
exits (or `{"type":"timeout"}`).

### Code interpreter mode (list_files)

Pass `"list_files": true` on session exec to get the workspace file listing
(with MIME type) in the response — the agent sees what files were produced
(charts, data, HTML) without a separate `files ls` call:

```bash
curl -X POST ".../sessions/$SID/exec" \
  -d '{"argv":["/usr/bin/python3","plot.py"],"list_files":true}'
# → { ..., "files": [{"path":"chart.png","size":12345,"mime":"image/png"}, ...] }
```

### Output redaction

`stdout`/`stderr` in `GET /jobs/{id}` responses are redacted — common secret
patterns (Bearer tokens, AWS `AKIA` keys, GitHub tokens, PEM private keys) are
replaced with `[REDACTED]` before returning, so credentials a task accidentally
reads (e.g. `~/.aws/credentials`) don't leak into the agent's context.

### Controlled egress (egress allowlist)

By default tasks have zero egress. Set `egress_allowlist` on a profile to allow
specific hosts (port optional):

```yaml
profiles:
  python:
    egress_allowlist:
      - host: "pypi.org"
      - host: "*.pypi.org"
      - host: "files.pythonhosted.org"
        port: 443
```

- A task can only egress via `SANDBOX_PROXY_SOCK` (a SOCKS5h proxy over a UDS in
  the workspace) — seccomp blocks creating any TCP/UDP socket at the `socket()`
  call itself.
- Task code uses the bundled helper (e.g. `helpers/python/sandbox_net.py`), which
  routes through the proxy automatically:
  `import sandbox_net; r = sandbox_net.get("https://api.openai.com/...")`.
- `*` matches a single leftmost label (`*.pypi.org` matches `download.pypi.org`,
  not `a.b.pypi.org`).
- `dry_run: true` returns the profile's `egress_allowlist` for previewing.

### Disk quota (per-task)

A profile can cap a task's **aggregate** workspace usage. Set `disk_quota_mb`; a
task whose workspace grows past the cap is reaped (`SIGTERM` → `SIGKILL`) and the
result carries `status: "DiskQuotaExceeded"`.

```yaml
profiles:
  heavy:
    disk_quota_mb: 50      # workspace aggregate cap in MB; unset = unlimited
```

How it works: a watchdog measures the workspace size every 250 ms and kills the
whole process group once the cap is exceeded. This is **best-effort** — a bursty
write can overshoot by up to `250 ms × write-rate` between polls; the per-file
`fsize_mb` rlimit narrows that window. `disk_quota_mb` caps the *total* workspace;
`fsize_mb` caps a *single file* (they compose). `dry_run: true` returns the cap
for previewing. Unset = no cap (the default).

Built-in profiles also set a default **IO rate limit** via cgroup v2 `io.max`
(200 MB/s read, 100 MB/s write) — generous enough for normal workloads, but
prevents a runaway task from saturating disk bandwidth.

### Profiles

Three built-in profiles, chosen per task at submit time:

| profile | use case | mem cap | default timeout |
|---|---|---|---|
| `shell` | simple shell commands | 128 MB | 5s |
| `python` | Python scripts | 256 MB | 5s |
| `node` | Node.js scripts | 256 MB | 5s |

> The Docker image bundles `python3` (with `requests` / `httpx`) and `node`, so the
> `python` / `node` profiles run out of the box. Installing extra packages needs an
> egress allowlist (see [Controlled egress](#controlled-egress-egress-allowlist)) —
> install into the task workspace.

Custom profiles are added via the config file (see [Config reference](#config-reference)).

### Templates (pre-baked environments)

A "template" is just a profile that bundles a pre-installed package set (a
read-only dir) plus baseline env vars so the runtime finds them. Build the dir
once when building the worker image, then reference it from a profile.

```bash
# in your worker image build:
scripts/build-template.sh data-science "pandas numpy scikit-learn"
```

```yaml
profiles:
  data-science:
    extra_readonly_paths: ["/opt/templates/data-science"]
    env:
      PYTHONPATH: "/opt/templates/data-science"
      MPLBACKEND: "Agg"
    rlimit:
      cpu_seconds: 30
    default_timeout: "60s"
```

Profile `env` is the baseline (operator-trusted): it can set/override `PATH` and
`LANG`, and add any key. Per-request `custom_env` can only *add* keys the profile
hasn't set. `HOME`/`TMPDIR` always point at the workspace and can never be
overridden.

Alternatively, define templates with an auto-setup command that runs once at
worker startup:

```yaml
templates:
  data-science:
    setup: "pip install --target /opt/templates/ds pandas numpy scikit-learn"
    extra_readonly_paths: ["/opt/templates/ds"]
    env:
      PYTHONPATH: "/opt/templates/ds"
    default_timeout: "60s"
```

The `setup` command runs at server start (e.g. installing packages into a
shared dir); then the profile is registered like any other.

---

## Sessions (persistent sandboxes)

A **session** is a long-lived sandbox: a persistent workspace + a bound profile
that survives across multiple `exec` calls. Use it for multi-step agent
workflows (upload a file → run it → read results → iterate) without re-bundling
everything into one command. One-shot `POST /jobs` remains for single commands.

```bash
# 1. create
SID=$(curl -s -X POST http://127.0.0.1:8080/api/v1/sessions \
  -H 'content-type: application/json' -d '{"profile_name":"shell"}' | jq -r .session_id)

# 2. upload a script (raw bytes)
curl -X PUT "http://127.0.0.1:8080/api/v1/sessions/$SID/files/run.sh" --data-binary @run.sh

# 3. exec (shares the workspace with step 2). Add ?stream=true for live SSE stdout.
curl -X POST "http://127.0.0.1:8080/api/v1/sessions/$SID/exec" \
  -H 'content-type: application/json' -d '{"argv":["/bin/sh","run.sh"]}'

# 4. list / download / destroy
curl "http://127.0.0.1:8080/api/v1/sessions/$SID/files"            # list
curl "http://127.0.0.1:8080/api/v1/sessions/$SID/files/out.txt"    # download
curl -X DELETE "http://127.0.0.1:8080/api/v1/sessions/$SID"
```

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/sessions` | create (`{profile_name, env?}`) → `{session_id}` |
| `GET` | `/sessions` | list |
| `GET` | `/sessions/{id}` | status |
| `DELETE` | `/sessions/{id}` | destroy |
| `POST` | `/sessions/{id}/exec` | run a command (`?stream=true` for SSE) |
| `PUT` | `/sessions/{id}/files/{path}` | upload (raw bytes) |
| `GET` | `/sessions/{id}/files/{path}` | download |
| `GET` | `/sessions/{id}/files?path=` | list |
| `DELETE` | `/sessions/{id}/files/{path}` | delete |

Notes:

- The profile is **bound at create**; all execs in the session use it.
- Execs in a session are **serialized** (one at a time).
- File paths are confined to the session workspace (`..`/absolute rejected).
- Every exec runs under the full sandbox profile (landlock/seccomp/cgroup/
  timeout/cancel/quota) — same as one-shot jobs.
- Sessions **survive worker restart**: their state (workspace + bound profile)
  is rebuilt on startup, so a session id stays usable after a restart
  (reconnect). Snapshots and volumes also survive restart.
- There is no background TTL reaper yet — clean up with an explicit `DELETE`.

### Snapshots (fork a session)

A snapshot is a full copy of a session's `workspace/`. Save a prepared
environment and fork new sessions from it (the snapshot holds files only —
profile/env are given at restore).

```bash
SNAP=$(curl -s -X POST http://127.0.0.1:8080/api/v1/sessions/$SID/snapshot | jq -r .snapshot_id)
curl    http://127.0.0.1:8080/api/v1/snapshots                 # list
curl -X POST http://127.0.0.1:8080/api/v1/sessions \
  -H 'content-type: application/json' \
  -d "{\"profile_name\":\"shell\",\"from_snapshot\":\"$SNAP\"}" # fork
curl -X DELETE http://127.0.0.1:8080/api/v1/snapshots/$SNAP
```

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/sessions/{id}/snapshot` | snapshot a session → `{snapshot_id}` |
| `GET` | `/snapshots` | list |
| `DELETE` | `/snapshots/{id}` | delete |

Snapshots persist on disk and **survive worker restart**. A snapshot waits for
any running exec to finish (taken while the session is quiet).

### Volumes (persistent storage)

A **volume** is a named directory that persists across sessions and restarts.
Mount one into a session and the task sees it at `workspace/<mount>` (a symlink
to the volume); writes survive the session's destruction. Landlock grants the
volume read-write.

```bash
curl -X POST http://127.0.0.1:8080/api/v1/volumes -H 'content-type: application/json' -d '{"name":"data"}'
curl     http://127.0.0.1:8080/api/v1/volumes                                  # list
# mount into a session (task writes ./volumes/data, persists across sessions):
curl -X POST http://127.0.0.1:8080/api/v1/sessions \
  -H 'content-type: application/json' \
  -d '{"profile_name":"shell","volumes":[{"name":"data","mount":"volumes/data"}]}'
curl -X DELETE http://127.0.0.1:8080/api/v1/volumes/data
```

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/volumes` | create (`{name}`) |
| `GET` | `/volumes` | list |
| `DELETE` | `/volumes/{name}` | delete |

## Python SDK & agent framework integration

The [`lvsandbox`](../sdk/python) Python package wraps the HTTP API with an
E2B-style interface — sessions, files, snapshots, volumes, streaming, and
code-interpreter helpers:

```python
from lvsandbox import Client

lv = Client("http://127.0.0.1:8080")
s = lv.sessions.create(profile="python")
s.files.put("plot.py", open("plot.py","rb").read())
r, files = lv.run_python(open("plot.py").read())   # write + exec + list_files
print(r.stdout)
print([f.path for f in files])                       # → ["chart.png", ...]
```

Agent-framework integrations:
- `lv.openai_tool_schema()` → JSON schema for OpenAI function calling.
- `lv.langchain_tool()` → LangChain `BaseTool` (requires `langchain-core`).

See the [SDK README](../sdk/python/README.md) for full API.

## CLI

The [`lvs`](../crates/lv-cli) command-line tool manages jobs, sessions, files,
snapshots, and volumes from the terminal:

```bash
lvs sessions new --profile shell          # → session id
lvs exec <id> -- /bin/echo hi             # session exec (exits with code)
lvs files put <id> run.sh ./local.sh      # upload
lvs files get <id> out.txt                # download
lvs shell <id> -- /bin/sh                 # interactive PTY
```

See the [CLI README](../crates/lv-cli/README.md) for all commands.

## MCP integration (Claude Code / Hermes-Agent)

`sandbox-mcp` wraps the sandbox as 4 MCP tools an AI Agent can call directly:

| Tool | Purpose |
|---|---|
| `sandbox_run` | run a command in the sandbox, return the result |
| `sandbox_profiles` | list available profiles |
| `sandbox_status` | query worker status |
| `sandbox_reload` | hot-reload config |

### Wire up Claude Code

A `.mcp.json` is provided at the repo root:

```json
{
  "mcpServers": {
    "sandbox": {
      "command": "cargo",
      "args": ["run", "-p", "sandbox-mcp", "--quiet", "--"],
      "env": {
        "SANDBOX_SERVER_URL": "http://127.0.0.1:8080",
        "RUST_LOG": "info"
      }
    }
  }
}
```

Precondition: sandbox-server is already running on `127.0.0.1:8080`. Claude Code
auto-loads `.mcp.json` on start, spawns the `sandbox-mcp` gateway, and the sandbox
tools become available in the conversation.

> For production, point `command` at a prebuilt binary
> (`./target/release/sandbox-mcp`) to avoid compiling on every start.

### Hermes-Agent

Add the same MCP server connection info to Hermes-Agent's config; it talks stdio
JSON-RPC.

---

## Config reference

```yaml
server:
  listen_addr: "0.0.0.0:8080"
  max_concurrent_jobs: 100      # max concurrent tasks
  log_level: "info"
  log_format: "json"            # json | text
  audit:                        # cr-021: opt-in audit log (off by default)
    enabled: false
    path: "/var/log/sandbox/audit.jsonl"
  api_key: "secret-token"       # cr-023: Bearer token; unset = no auth (default)
  webhooks: []                    # cr-031: lifecycle webhook URLs; empty = off (default)

sandbox:
  base_dir: "/sandboxes"        # task workspace root
  disk_watermark_mb: 1024       # disk watermark — reject new tasks below this (0 = disable)
  default_profile: "shell"
  fail_closed: true             # refuse to run when a security mechanism is unavailable

profiles:
  shell:
    rlimit:
      cpu_seconds: 2
      nofile: 64
      nproc: 32
      fsize_mb: 10
    max_stdout_mb: 5            # stdout truncation threshold
    default_timeout: "5s"

  python:
    extra_readonly_paths:       # extra read-only paths (e.g. offline dep libs)
      - "/opt/sandbox-libs/python3"
    rlimit:
      cpu_seconds: 5
      nofile: 128
    max_stdout_mb: 10
    default_timeout: "30s"

  # custom profile: unset fields inherit the shell defaults
  custom_task:
    rlimit:
      cpu_seconds: 10
    max_stdout_mb: 20
    default_timeout: "60s"
    disk_quota_mb: 100          # cr-022: workspace aggregate cap (MB); unset = unlimited
    extra_readonly_paths:
      - "/data/shared"
```

After editing, call `POST /api/v1/reload` to hot-reload — no restart needed.

#### Audit log

Set `server.audit.enabled: true` to write a JSONL audit trail (off by default).
Each line is a self-contained event: `started` / `completed` / `timed_out` /
`killed` / `cancelled` / `failed`, with `argv`, `exit_code`, `signal`, `duration_ms`.
The file is operator-side (not returned to agents); protect it like any log that
may contain commands.

#### Webhooks

Set `server.webhooks` to a list of URLs to receive a `POST` of the terminal
event (the same AuditEvent JSON: `event_type`, `job_id`/`session_id`, `status`,
`exit_code`, `argv`, …) whenever a job or session exec finishes — no polling
needed. Off by default; delivery is async (fire-and-forget, 3 retries). The
payload includes `argv`, so protect the endpoint like the audit file.

```yaml
server:
  webhooks: ["https://your-service/lvsandbox-hook"]
```

### Timeout format

`timeout` / `default_timeout` accept: `5s`, `100ms`, `1m`, or a plain number (seconds).

---

## Components

| Component | Type | Responsibility |
|---|---|---|
| `sandbox-server` | binary | HTTP service + scheduling + config + metrics |
| `sandbox-mcp` | binary | MCP gateway, faces the AI Agent |
| `sandbox-core` | library | task execution core, reusable |
| `sandbox-landlock` | library | Landlock filesystem isolation |
| `sandbox-seccomp` | library | seccomp syscall filtering |
| `sandbox-cgroup` | library | cgroup v2 resource management |

---

## Testing

```bash
# all tests
cargo test --workspace

# end-to-end only
cargo test -p sandbox-e2e

# verify the Docker image (in-container e2e: health + a real echo task)
bash scripts/verify-image.sh
```
