# How lv-sandbox compares

> Goal: help you pick the right tool for your threat model — not to argue lv-sandbox
> is "best." Different tools optimize for different threats. See
> [security.md](security.md) for lv-sandbox's own threat boundary.

## The short version

There are roughly two camps:

- **Kernel-primitive sandboxes** (lv-sandbox, plain Docker): use Landlock/seccomp/cgroups
  or namespaces to confine a process **sharing the host kernel**. Light, fast, cheap at
  concurrency. Isolation is only as strong as the kernel + the syscall filter.
- **Virtualization sandboxes** (gVisor, Kata, Firecracker/microVM, E2B): put each task
  behind a userspace kernel or a real VM. Heavier and slower per task, but far stronger
  against kernel exploits and container escapes.

lv-sandbox sits in the **first** camp, and within it takes the *zero-privilege,
blast-radius-first* stance described in the [design philosophy](security.md):
contain agent mistakes and casual escalation at high concurrency, **without** a container
per task and **without** any extra capabilities.

## Comparison table

| Dimension | lv-sandbox | Docker (one per task) | gVisor (runsc) | Kata Containers | Firecracker / microVM (e.g. E2B) |
|---|---|---|---|---|---|
| Isolation mechanism | Landlock + seccomp + cgroup, in one worker | namespaces + seccomp + cgroups | userspace kernel (syscall intercept) | VM-per-container (hardware virt) | lightweight VM (hardware virt) |
| Isolation strength | defense-in-depth; **not** vs kernel exploits | good vs casual; escape history (e.g. runc CVEs) | strong (tiny kernel surface) | very strong (HW-isolated) | very strong (HW-isolated, minimal device model) |
| Cold start per task | **~ms** (fork/exec) | ~100ms–1s (container start) | container speed + overhead | **seconds** (VM boot) | ~125ms (Firecracker boot) |
| Concurrency model | one worker → many kernel-isolated tasks | one container per task | one container per task | one VM per task | one microVM per task |
| Network egress | **default deny + allowlisted SOCKS5, zero-privilege** | host firewall / iptables | host networking | VM networking | configurable |
| Privileges needed | `--cap-drop=ALL` (none) | container engine / root | ptrace or KVM | KVM / nested virt | KVM |
| Best-fit threat model | agent mistakes, casual escalation, trusted-tenant | general isolation, multi-tenant (with care) | untrusted code in containers | untrusted / multi-tenant prod | fully untrusted code, code-exec-as-a-service |
| Ops weight | single binary / one container | container runtime | runsc runtime | heavier (VM + containerd) | VM infra or managed (E2B) |

## Which should you pick?

**Pick lv-sandbox if** your risk is "the agent does something dumb or is mildly
manipulated" — it reads/writes the wrong file, loops forever, tries to phone home — and
you want **hundreds of lightweight, kernel-isolated tasks from one worker, with no extra
privileges and a real egress allowlist**. Typical: running an AI agent's generated
commands/scripts on a trusted-tenant worker.

**Pick a microVM (Kata / Firecracker / E2B) if** you run **fully untrusted or hostile
code**, multi-tenant workloads, arbitrary third-party dependencies, or anything where a
kernel-primitive sandbox is below your bar. You pay seconds-or-hundreds-of-ms of cold
start and VM ops, and you get hardware-grade isolation. This is the right call for
code-execution-as-a-service and high-assurance production.

**Pick gVisor if** you want strong isolation for untrusted code **but stay in the
container ecosystem** (less ops than full VMs, stronger than plain Docker), accepting its
syscall-interception overhead and compatibility caveats.

**Pick plain Docker (one per task) if** you want general-purpose isolation today with
minimal new tooling — but mind that "container per task" gets heavy at high concurrency,
and shared-kernel containers have a real escape history.

## What lv-sandbox deliberately does NOT claim

- It is **not** a defense against a determined adversary with kernel exploits — Landlock,
  seccomp, and cgroups are kernel features and inherit kernel risk. (See [security.md](security.md).)
- It is **not** a replacement for microVMs in **untrusted, multi-tenant** settings.
- It **has not** undergone an external security audit.

The honest framing is the [design philosophy](security.md):
**choose the isolation level by threat model, not by "is it an Agent."** lv-sandbox is the
right answer for one specific, common threat model — agent mistakes and casual escalation
at high concurrency, zero-privilege — and is explicitly not the answer for others.
