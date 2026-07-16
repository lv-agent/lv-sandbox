# Network isolation & controlled egress

> Scope: how lv-sandbox prevents a task from making network connections, and how
> you can opt a profile into **controlled, allowlisted egress** through a UDS
> SOCKS5 proxy. For the broader threat model see [security.md](security.md); for
> configuration syntax see [usage.md](usage.md#controlled-egress-egress-allowlist).

## TL;DR

- **By default every task has zero network egress.** It cannot create a TCP/UDP
  socket at all.
- A profile can opt into **controlled egress** by setting `egress_allowlist`. The
  task still cannot make raw TCP/UDP sockets — all egress is funneled through a
  SOCKS5 proxy over a Unix domain socket (UDS) in the task's workspace, and the
  proxy only forwards to allowlisted `host:port` pairs.
- Enforcement is **kernel-level** (seccomp) and **fail-closed**: a task that
  ignores the proxy gets no network, not unrestricted network.

This needs **no extra privileges** — it works under the default `--cap-drop=ALL`
container deployment.

---

## Enforcement: seccomp allows only `AF_UNIX` sockets

The sandbox seccomp filter is applied in the child process before `exec`. For
network it does one thing:

> `socket(domain)` is killed (`SIGSYS`) unless `domain == AF_UNIX`.

Everything else in the socket API (`connect`, `bind`, `send`, `recv`, …) is
**allowed** — but a task can never obtain a non-`AF_UNIX` socket fd in the first
place, so those calls can only ever operate on Unix sockets. Inherited fds are
closed in `pre_exec`, so no `AF_INET` fd leaks in.

### Why the cut is at `socket()`, not `connect()`

Classic seccomp-BPF can only inspect the **register arguments** of a syscall; it
cannot dereference user-space pointers. `connect()` takes a pointer to a
`sockaddr`, so seccomp **cannot** filter by destination address. Therefore the
enforcement point must be `socket()` creation — deny the *birth* of an
INET/RAW socket. This is the standard approach.

### Why a UDS is not an escape hatch

A task *can* create `AF_UNIX` sockets (it needs to, to reach the egress proxy).
Could it use a UDS to escape? No:

- Connecting to a UDS requires opening its path, which is governed by **Landlock**.
  Landlock confines the task to its own workspace, so the only UDS it can reach is
  the proxy's socket file inside the workspace. System UDS paths
  (`/var/run/docker.sock`, etc.) are not landlock-reachable.
- **Abstract namespace sockets** (paths beginning with `\0`) bypass the filesystem
  and thus Landlock, but they are pure local IPC — they cannot carry traffic to a
  network. They at most allow two sandboxed tasks to talk to each other locally
  (a contained side-channel, out of scope here).

Net effect: the task has exactly one network path — the allowlisted proxy.

---

## Controlled egress: SOCKS5h over UDS

When a profile has a non-empty `egress_allowlist`, the sandbox **server process**
(the un-sandboxed parent, which holds real network capability) starts a per-job
SOCKS5 proxy:

```text
┌──────── server process (un-sandboxed, real network) ────────┐
│  run_job:                                                    │
│   ├─ bind <workspace>/.proxy.sock  (UnixListener, tokio)     │
│   ├─ SOCKS5h loop: accept → allowlist check → DNS → TCP      │
│   ├─ pre_exec: seccomp(AF_UNIX-only) + landlock + …          │
│   └─ env SANDBOX_PROXY_SOCK=<workspace>/.proxy.sock          │
└──────────────────────┬───────────────────────────────────────┘
                       │ AF_UNIX (landlock-confined to workspace)
┌──────────────────────▼───────────────────────────────────────┐
│  task process (sandboxed: only AF_UNIX sockets)              │
│   helper → dial SANDBOX_PROXY_SOCK → SOCKS5h CONNECT(host)   │
└──────────────────────┬───────────────────────────────────────┘
                       │ proxy: allowlist → DNS → real TCP
                       ▼
                 real network (allowlisted only)
```

### SOCKS5h (remote DNS)

The proxy implements SOCKS5 ([RFC 1928](https://www.rfc-editor.org/rfc/rfc1928))
with **remote DNS** (the `DOMAINNAME` address type). The task passes a hostname;
the proxy resolves it. This sidesteps the chicken-and-egg of "the task has no
network to do DNS with." IP-literal address types (`IPv4`/`IPv6`) are rejected,
forcing hostnames (which keeps the allowlist meaningful and auditable).

### Allowlist model

- **Per-profile**, defined in config as `egress_allowlist`. Empty/absent = **zero
  egress** (no proxy is even started).
- Each rule: `host` (exact, or `*.example.com` single-leftmost-label wildcard) +
  optional `port` (absent = any port for that host).
- Matching is case-insensitive. Default deny.
- `dry_run: true` returns the resolved allowlist for previewing.

```yaml
profiles:
  python:
    egress_allowlist:
      - host: "pypi.org"
      - host: "*.pypi.org"            # matches download.pypi.org, NOT a.b.pypi.org
      - host: "files.pythonhosted.org"
        port: 443
```

---

## How tasks use it (helpers)

Standard tools (`curl`, `pip`) cannot natively target a UDS proxy (the proxy URL
must be a `host:port`). The repo ships thin **helpers** that dial the UDS,
perform the SOCKS5h handshake, and run HTTP over the relayed stream — pure
stdlib, no third-party deps:

| Helper | Use |
|---|---|
| `helpers/python/sandbox_net.py` | `import sandbox_net; r = sandbox_net.get("https://api.openai.com/...")` |
| `helpers/node/sandbox-net.js` | `const {request} = require('./sandbox-net'); request('GET', url, null, null, cb)` |
| `helpers/shell/sandbox-curl` | `sandbox-curl https://...` (delegates to the python helper) |

The helpers read `SANDBOX_PROXY_SOCK` to find the proxy.

> The helpers are **cooperative** — a task must choose to use them. But security
> does **not** depend on cooperation: a task that bypasses the helper and tries a
> raw TCP connection is killed by seccomp (no `AF_INET` socket). The helper is
> for *usability*; seccomp is for *security*.

For HTTPS the helper upgrades the relayed stream with TLS (`ssl`/`tls`). Trust the
upstream cert the usual way (`SSL_CERT_FILE` / `NODE_EXTRA_CA_CERTS`).

---

## Git over the egress proxy (CR-12)

The [`git` profile](usage.md#git-access-git-profile-cr-12) reuses the
SOCKS5h-over-UDS path above unchanged — it does **not** open a new egress
channel. A sandboxed task running `git clone` / `push` is still AF_UNIX-only at
the `socket()` layer; git's network is funneled through a git remote helper.

### Pieces

- **`git` profile** (`crates/sandbox-core/src/profile.rs` → `SandboxProfile::git()`)
  builds on the shell profile and adds a single-rule `egress_allowlist` so the
  cr-019 proxy is started for the job. The destination comes from env vars read
  at profile construction (set on the sandbox-server process):

  | env var | default | meaning |
  |---|---|---|
  | `FIXUS_GIT_EGRESS_HOST` | `github.com` | host the proxy will forward to |
  | `FIXUS_GIT_EGRESS_PORT` | `443` | port (parse failure falls back to `443`) |
  | `FIXUS_GIT_CA_FILE` | *(unset)* | PEM file whose contents are injected into the jail as `SANDBOX_CA_PEM` |

- **`git-remote-fixus`** (`crates/git-remote-fixus`, installed on `PATH` in the
  jail — conventionally `/usr/bin/git-remote-fixus`) is a standard git remote
  helper. It owns no networking of its own: it dials `SANDBOX_PROXY_SOCK` (the
  per-job UDS proxy, injected by cr-019), does the SOCKS5h `DOMAIN`-`CONNECT`
  handshake, then rustls TLS, then git smart-HTTP (`info/refs` →
  `git-upload-pack` / `git-receive-pack`). It advertises the `fixus::https://`
  URL scheme:

  ```text
  ┌──────── task (git profile: AF_UNIX-only sockets) ──────────┐
  │  git clone fixus::https://github.com/org/repo.git          │
  │   └─ git-remote-fixus → UnixStream::connect(SANDBOX_PROXY_SOCK) ─┘
  └──────────────────────┬─────────────────────────────────────┘
                         │ AF_UNIX (landlock-confined to workspace)
  ┌──────────────────────▼─────────────────────────────────────┐
  │  cr-019 SOCKS5h UDS proxy (server process, real network)   │
  │   allowlist check → DNS → TCP → github.com:443             │
  └────────────────────────────────────────────────────────────┘
  ```

### Trust & the CA path

The helper dialer's CA precedence (`crates/git-remote-fixus/src/dialer.rs`):

1. `SANDBOX_CA_PEM` — PEM content passed verbatim (the jail-injection channel;
   set by the `git` profile from `FIXUS_GIT_CA_FILE`). Use this for self-hosted
   GitLab / internal-CA upstreams.
2. `SANDBOX_CA_FILE` — PEM file path (testing, or when a file is already on a
   jail-readable path).
3. webpki-roots built-in roots (default; real-CA upstreams like `github.com`).

Any source that is missing or unparseable falls through to the next; the dialer
never skips verification (failure = do not trust).

### What does *not* change

- `socket(AF_INET, …)` stays killed — the `git` profile does **not** widen
  seccomp. git's only network path is still the allowlisted UDS proxy.
- A non-allowlisted host is still rejected with SOCKS5 `REP = 0x02`; IPv4-literal
  `ATYP` is still rejected (forces hostname / remote DNS).
- Credentials inside the jail are sentinel values; the real token lives only in
  an operator-implemented credential-exit proxy outside the jail. See
  [security.md · Git egress & the sentinel credential model](security.md#git-egress--the-sentinel-credential-model-cr-12).

---

## What it stops / does not stop

**Stops:**
- Any raw TCP/UDP connection (seccomp kills `socket(AF_INET, …)`).
- Reaching the cloud metadata service, phone-home, opening listeners.
- Egress to non-allowlisted hosts even when a proxy is active.
- Bypassing the proxy (there is no other socket path).

**Does not stop (by design / out of scope):**
- The proxy does **not** decrypt or inspect TLS — it is a transparent byte relay.
  Content-level auditing/DLP is not performed.
- HTTP/2 is **transparently carried** by the proxy (it is protocol-agnostic), but
  the bundled helpers speak HTTP/1.1 only. For HTTP/2 a task must bring its own
  h2-capable client over the SOCKS5h relay.
- There is **no per-task network namespace** (netns). The zero-privilege design
  deliberately avoids `CAP_SYS_ADMIN`. seccomp is the enforcement layer instead.
- Abstract-namespace UDS cross-task chatter (local side-channel).

---

## Verification

The implementation is covered by integration and e2e tests:

- seccomp: `socket(AF_INET)` → killed (`SIGSYS`); `socket(AF_UNIX)` → allowed.
- SOCKS5 proxy: allowlisted round-trip succeeds; non-allowlisted → SOCKS5 reply
  `0x02`; IPv4-literal → `0x02`; non-CONNECT → `0x07`; upstream refused → `0x05`.
- Malformed frames (bad version, truncated greeting/request/domain, oversized
  method list) are handled gracefully — no panic, no hang.
- Helpers (python + node) round-trip over the proxy for both HTTP and HTTPS.
- `JobProxy` cleans up (cancel/early-return paths) — no fd/task leak.
- CR-12: `git-remote-fixus::dialer::non_allowlisted_host_is_denied` — the git
  helper's dialer is rejected with `PermissionDenied` when the destination is
  off the allowlist (same `REP = 0x02` closure as the proxy).

Run with: `cargo test --workspace`.
