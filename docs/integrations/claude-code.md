# Use lv-sandbox from Claude Code

Goal: in ~5 minutes, have Claude Code run an agent's commands **inside** lv-sandbox,
and watch a dangerous command get contained.

## How it fits

```
Claude Code  ──stdio JSON-RPC──▶  sandbox-mcp (gateway)  ──HTTP──▶  sandbox-server (the sandbox)
```

`sandbox-mcp` is a thin gateway — it exposes 4 MCP tools and forwards them to a running
`sandbox-server`. Claude Code auto-loads the project's `.mcp.json` (included in this repo)
and the tools become available.

## 1. Start the sandbox server

```bash
docker run -d --name sandbox -p 8080:8080 \
  --read-only --tmpfs /tmp:rw,nosuid,nodev,size=1g \
  --tmpfs /sandboxes:rw,nosuid,nodev,size=100m,uid=10000,gid=10000 \
  --cap-drop=ALL --security-opt no-new-privileges \
  --pids-limit=1000 --memory=4g --cpus=4 --user 10000:10000 \
  ghcr.io/lv-agent/lv-sandbox:v0.3.0
curl http://127.0.0.1:8080/health   # → {"status":"ok", ...}
```

## 2. Point Claude Code at it

The repo ships a `.mcp.json`:

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

- In a clone of this repo, Claude Code auto-detects `.mcp.json` on startup and prompts
  you to approve the `sandbox` server.
- **Faster startup:** build the gateway once and point `command` at the binary:
  `cargo build --release -p sandbox-mcp`, then use
  `"command": "./target/release/sandbox-mcp"`.

The 4 tools the agent can now call:

| Tool | Purpose |
|---|---|
| `sandbox_run` | run a command in the sandbox, return the result |
| `sandbox_profiles` | list available profiles |
| `sandbox_status` | worker status |
| `sandbox_reload` | hot-reload config |

## 3. Use it in a conversation

Just ask Claude Code to run something, and tell it to use the sandbox:

> Run `echo hello agent` in the sandbox.

Claude Code calls `sandbox_run` (argv `["/bin/echo","hello agent"]`, profile `shell`) and
returns:

```
status: Completed, exit_code: 0
stdout: hello agent
```

## 4. Watch it contain a dangerous command

> Run `curl -s http://example.com` in the sandbox.

The agent calls `sandbox_run`. Inside the sandbox, `curl` tries `socket(AF_INET, …)` —
seccomp kills it. The agent gets back:

```
status: Killed
```

It didn't phone home. Nothing reached the network. The command was contained, and the
agent can report that honestly to you.

Try a file escape too:

> Run `cat /etc/passwd` in the sandbox.

→ `Completed` exit 1, stderr `cat: /etc/passwd: Permission denied` — Landlock confines the
task to its workspace; the host file is unreachable.

## Notes

- **The agent decides when to call the sandbox** — you steer it by asking it to "run … in
  the sandbox." For tighter control, build your own agent loop that always routes commands
  through `sandbox_run`.
- **Controlled egress** (instead of total block): give the profile an `egress_allowlist`
  and use the bundled helper — see [network-isolation.md](../network-isolation.md).
- **Server must be running** before Claude Code starts the gateway; the gateway polls
  `GET /jobs/{id}` for results (up to a 300s deadline per call).
