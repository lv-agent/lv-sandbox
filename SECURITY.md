# Security

## Reporting a vulnerability

If you discover a security issue in lv-sandbox, please **do not** open a public
GitHub issue. Report it privately so we can triage and fix it before disclosure.

- Open a **private security advisory**: repo → *Security* → *Advisories* →
  *Report a vulnerability*, **or**
- Contact the maintainer (see the repo owner profile).

Please include: a description, reproducible steps, and the assessed impact. We
aim to acknowledge within **72 hours** and to publish a fix + advisory once a
patch is ready. We request a **90-day** coordinated-disclosure window.

## Threat model

lv-sandbox is designed to **contain AI-agent mistakes and casual
privilege-escalation attempts** — see [docs/security.md](docs/security.md) for
the full model.

- **In scope:** a task reading/writing another task's files, reading the
  runner's secrets, fork bombs / resource exhaustion, escaping timeouts via
  background processes, dangerous syscalls, and **all raw network egress**
  (`socket(AF_INET, …)` is killed by seccomp; controlled egress is opt-in via an
  allowlisted SOCKS5 proxy).
- **Out of scope:** hardened malicious code exploiting kernel bugs, advanced
  container escape, strong multi-tenant isolation, and all side channels.

> lv-sandbox has **not** undergone an external security audit. It layers Linux
> kernel primitives (Landlock, seccomp, cgroup) inside a single worker. For fully
> untrusted, high-assurance code, use **MicroVM / gVisor / Kata /
> one-container-per-task** instead.

## Known boundaries

- **The HTTP API has no built-in authentication.** Place the server behind your
  own auth/network boundary in production.
- **One worker runs many tasks.** Wrap the worker in an outer container and run
  non-root (see [deployment hardening](docs/security.md#recommended-deployment)).
- **Linux only, kernel ≥ 5.13** (Landlock).
- **Completed jobs are evicted** from the in-memory job table — this is not a
  durable audit log. Capture results/`/metrics` externally if you need forensics.
- **Controlled egress is a transparent relay.** The SOCKS5 proxy enforces
  destination allowlists but does not decrypt or inspect TLS.

## Disclosure policy

Coordinated disclosure. Reporters are credited in the advisory unless they prefer
to remain anonymous.
