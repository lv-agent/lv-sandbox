# Compatibility Matrix — `lvsandbox-e2b` vs E2B

API-**surface** compatible (class/method/param/exception names align with the E2B
Python SDK), **not** wire-compatible (lv-sandbox has no envd/gRPC data plane; see
`veps/cr-083-e2b-api-compatibility.md`).

Legend: ✅ supported (sync + async) · ⚠️ partial (caveat) · ❌ not supported (blocked)

## Sandbox

| E2B API | Status | Notes |
|---|---|---|
| `Sandbox.create(template, envs, ...)` | ✅ | `template`→profile (`base`→`shell`); `timeout`→`timeout_secs` |
| `Sandbox.connect(id)` | ✅ | lazy; server rebuilds across restart |
| `Sandbox.list(query)` | ✅ | **client-side** filter (server has no query filter) |
| `Sandbox.kill()` | ✅ | |
| `Sandbox.get_info()` | ✅ | `started_at` absolute; `created_at`/`last_activity` are age |
| `Sandbox.set_timeout(t)` | ✅ | PATCH `timeout_secs` |
| `Sandbox.set_metadata(md)` | ✅ | PATCH (full-replace semantics) |
| `Sandbox.keep_alive(min)` | ⚠️ | activity-touch via PATCH (resets `last_activity`); reaper is global-TTL (cr-040), not per-session extension |
| `Sandbox.get_metrics()` | ❌ | server exposes only global Prometheus `/metrics`; no per-sandbox resource endpoint (cr-085 §5.4) |
| `Sandbox.pause()/resume()` | ❌ | cgroup freeze not implemented (Phase 3 optional) |
| `Sandbox.replicate(n)` | ✅ | N× `from_snapshot`; no true fork semantics |

## Commands

| E2B API | Status | Notes |
|---|---|---|
| `commands.run(cmd, on_stdout/on_stderr/on_exit)` | ✅ | `on_stdout` incremental; `on_stderr` batch at end (no stderr SSE event); non-zero exit sets `exit_code`, no raise; timeout raises `TimeoutException` |
| `commands.run(..., background=True)` | ❌ | no native background |
| `commands.send_stdin(pid, data)` | ❌ | exec stdin is one-shot (cr-085 M9 deferred) |
| `commands.list()` | ❌ | no process-list endpoint |
| `commands.kill(pid)` | ❌ | no process-kill endpoint |

## Filesystem

| E2B API | Status | Notes |
|---|---|---|
| `filesystem.read/write/list/remove` | ✅ | `read` text/bytes |
| `filesystem.make_dir` | ✅ | |
| `filesystem.exists` | ✅ | HEAD 200/404 |
| `filesystem.find(pattern)` | ✅ | glob; returns `FileInfo` with workspace-relative full path |
| `filesystem.search(pattern)` | ✅ | grep-style matches (path+line+text), **not** `FileInfo` |
| `filesystem.watch_dir` | ✅ | SSE `created`/`modified`/`removed` |

## Snapshot

| E2B API | Status | Notes |
|---|---|---|
| `snapshot.create()` | ✅ | |
| `snapshot.list()` | ⚠️ | server returns bare ids; `SnapshotInfo` metadata unavailable |
| `snapshot.delete(id)` | ✅ | |

## Other

| E2B API | Status | Notes |
|---|---|---|
| `AsyncSandbox` | ✅ | mirrors Sandbox/Commands/Filesystem/Snapshot (async) |
| `pty.*` | ❌ | not in v1 scope |
| `Template.build()` | ❌ | lv-sandbox uses profiles, no image build |

## Exception surface

Aligned with E2B (§6.1): `E2BError` → `SandboxException` (`TimeoutException`,
`SandboxNotRunningError`), `FileException` (`FileNotFoundException`,
`PermissionDeniedError`), `CommandException` (`CommandExitException`),
`TemplateException`, `PTYException`, `AuthenticationException`,
`RateLimitException`. HTTP→exception mapping (§6.4): 404→session/file (by
context), 408→`TimeoutException`, 401→`AuthenticationException`,
429→`RateLimitException`.
