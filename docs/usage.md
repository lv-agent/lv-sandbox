# Usage guide

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
docker pull ghcr.io/lv-agent/lv-sandbox:v0.1.0
docker tag ghcr.io/lv-agent/lv-sandbox:v0.1.0 lv-sandbox:0.1.0   # optional, to reuse the commands below
```

**Option B: build locally**

```bash
# build the image
docker build -t lv-sandbox:0.1.0 .

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
  lv-sandbox:0.1.0
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

The request body also accepts an optional `stdin` field — UTF-8 text piped to the
task's stdin (e.g. for `cat` or scripts that read input).

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

### Profiles

Three built-in profiles, chosen per task at submit time:

| profile | use case | mem cap | default timeout |
|---|---|---|---|
| `shell` | simple shell commands | 128 MB | 5s |
| `python` | Python scripts | 256 MB | 5s |
| `node` | Node.js scripts | 256 MB | 5s |

Custom profiles are added via the config file (see [Config reference](#config-reference)).

---

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
    extra_readonly_paths:
      - "/data/shared"
```

After editing, call `POST /api/v1/reload` to hot-reload — no restart needed.

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
