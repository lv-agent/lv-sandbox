# 安全模型与威胁边界

本文说明 **lv-sandbox 设计上阻止什么、假设什么、刻意不承诺什么**。它是安全态势的权威参考;[architecture.md](architecture.md) 给组件视角,[network-isolation.md](network-isolation.md) 深入讲出站。

## 威胁模型

**范围内**:遏制 *AI agent 的误操作* 与 *随意的权限提升*——任务不小心(或天真地)删了不该删的文件、读了密钥、fork 炸弹、phone home、跑了危险 syscall。目标:在一个长驻 worker 内,以低成本、高并发,把一个行为不端的轻量任务的爆炸半径压到最小。

**范围外(明确)**:利用内核漏洞的恶意代码、高级容器逃逸、强多租户隔离、所有侧信道(时序、Rowhammer……)。如果任务源**完全不可信**且要求高,**这不是合适的工具**——请用 MicroVM / gVisor / Kata / 一任务一容器。

> lv-sandbox 在**一个** worker 内叠加 Linux 内核原语(Landlock/seccomp/cgroup)。它是 agent 工作负载的纵深防御,不是对抗持有内核漏洞的坚定攻击者的硬沙箱。

## 防御层

每个任务跑在独立进程组中,叠加以下机制(在环境能力探测后,于 `pre_exec` 应用):

| 层 | 作用 |
|---|---|
| **Landlock** | 文件系统圈禁:任务只能读写自己的工作区(+ 只读全局集)。`/proc` 收缩到自己的 `/proc/self` + 全局信息文件(cpuinfo/meminfo),看不到其他任务的 `/proc/<pid>`。 |
| **seccomp** | 拒绝危险 syscall(mount/ptrace/bpf/unshare/reboot/io_uring……)**且把 `socket()` 限制为仅 `AF_UNIX`**——见 [network-isolation.md](network-isolation.md)。 |
| **cgroup v2** | 限制真实资源用量(内存/CPU/pids)。cgroup v2 不可用时降级到 rlimit。 |
| **rlimit** | 进程级上限(CPU 秒数、fd 数、进程数、文件大小、禁 core)。 |
| **进程加固** | `NoNewPrivs`(防提权)、`setsid`(脱离控制终端)、关闭继承 fd、**环境变量白名单**(runner 的密钥到不了任务)。 |
| **超时收割** | 超时/cancel 时整进程组 `SIGTERM` → `SIGKILL`,无孤儿后台进程。 |
| **输出脱敏** | 返回给调用方的 `stdout`/`stderr` 清洗常见密钥模式(Bearer token、AWS `AKIA`、GitHub token、PEM 私钥),避免任务读到的凭证泄进 agent 上下文。 |

## 阻止什么

- 任务读写其他任务的文件,或宿主敏感文件。
- 经 `/proc`(`cmdline`/`maps`/`environ`)窥探其他任务。
- fork 炸弹、fd 耗尽、无限占 CPU(资源上限 + 超时)。
- 撑爆工作区(资源上限 + 磁盘水位准入 + 可选每任务 `disk_quota_mb` 看门狗:工作区超上限即收割)。
- **会话文件 I/O 逃逸**——经会话 API 的上传/下载/列目录圈在会话工作区内(拒 `..`/绝对路径);卷仅对 operator 声明的卷目录授予 Landlock 读写。
- 靠后台进程逃超时(整组清理)。
- 调用危险 syscall。
- **发起网络连接**——`socket(AF_INET, …)` 被杀;出站只能经白名单 UDS SOCKS5 代理(profile 按需开启)。
- 继承 runner 的密钥环境变量或泄漏 fd。

### Git 出站与哨兵凭据模型(CR-12)

[`git` profile](usage.md#git-出站git-profilecr-12) 让任务开启受控出站以 `git clone`/`push`。
安全相关要点(靠强制,不靠协作):

- **不变量 1 —— jail 内凭据绕不过代理。** 即使任务拿到凭据、想直连 `github.com` phone home,
  请求也死在 `socket()`:seccomp `deny_network()` 杀一切 `socket(domain != AF_UNIX)`(`KillProcess` / `SIGSYS`)。
  git 拿不到原始 INET socket —— 其全部网络走 `git-remote-fixus` helper,而 helper 自身只拨 per-job
  SOCKS5h UDS 代理。这证明 jail 内凭据是真哨兵,而非伪装的真 token:真 token 也没有 socket 可搭。
  自动化:`crates/sandbox-seccomp`(`deny_network` → 单条 `socket` 规则,`domain != AF_UNIX` 即杀)
  与 cr-019 e2e 套件。

- **不变量 2 —— 真凭据仅存于代理进程。** 这是设计保证:真 token 由 jail **外**的**凭据出口代理**持有,
  绝不注入任务。jail 内 git 对**伪造/哨兵**值跑标准凭据流程;出口代理识别哨兵 → 掉包为真 token →
  转发到 `github.com:443`。故任务被完全攻陷也无真凭据可外泄。

- **白名单收口(目标闭包)。** 白名单外的 host 被代理以 SOCKS5 `REP = 0x02`(*connection not allowed by ruleset*)拒绝;
  IP 字面量 `ATYP` 直接被拒(强制 hostname/远程 DNS,保持白名单有意义)。自动化:
  `sandbox_core::proxy::non_allowlisted_denied`、`sandbox_core::proxy::proxy_rejects_ipv4_literal`、
  `git-remote-fixus::dialer::non_allowlisted_host_is_denied`。

**掉包代理实现体与哨兵 scheme 不在 CR-12 范围**(operator 自行实现)。CR-12 只定义 jail 侧白名单形态
(`FIXUS_GIT_EGRESS_HOST`/`FIXUS_GIT_EGRESS_PORT`,默认 `github.com:443`),并保证标准 git 凭据流程对其运行;
不交付持有真 token 的代理。数据路径见
[network-isolation.md · 经出站代理路由 git](network-isolation.md#经出站代理路由-git-cr-12)。

## 不阻止什么

- 基于内核漏洞的逃逸(Landlock/seccomp/cgroup 是内核特性,内核 bug 可颠覆它们)。
- 侧信道攻击。
- 互怀敌意的多租户之间的强隔离。
- 出站的**内容审查**——出站代理是透明中继,只强制目标白名单,不解密/不审计 TLS 载荷。
- 被沙箱化的任务之间经抽象命名空间 UDS 的本地 chatter(受控的次要信道,非出站路径)。

## fail-closed vs fail-open

每个安全机制都受运行时**能力探测**门控。机制不可用时(Landlock 不支持、cgroup v2 缺失),profile 的 `fail_closed` 决定:

- `fail_closed: true` → **拒绝执行**任务(需要该机制的 profile 的安全默认)。
- `fail_closed: false` → 降级(如 cgroup → rlimit)继续。

环境支持时,seccomp 与 Landlock **总是应用**,与白名单无关——网络白名单只决定出站代理起不起,不决定 seccomp 生不生效。

## 推荐部署

把 worker 包在**外层容器**里(worker 容器,非 per-task 容器),`sandbox-server` 以非 root 在其中运行:

```bash
docker run -d --name sandbox \
  -p 8080:8080 \
  --read-only --tmpfs /tmp:rw,nosuid,nodev,size=1g \
  -v /safe/worker/sandboxes:/sandboxes:rw \
  --cap-drop=ALL --security-opt no-new-privileges \
  --pids-limit=1000 --memory=4g --cpus=4 \
  --user 10000:10000 \
  your-worker-image
```

铁律:绝不 `--privileged`;绝不挂 Docker socket 或宿主敏感目录;只读 rootfs;非 root 运行;`/tmp` 用 tmpfs;仅 `/sandboxes` 可写。网络出站模型**无需额外 capability**(基于 seccomp,非 netns)。

## 运维注意

- HTTP API **可选 Bearer API key**——配 `server.api_key` 后 `/api/v1/*` 与 `/metrics` 需 `Authorization: Bearer <key>`(默认关;`/health` 放行探活)。即便开启,生产仍建议再加网络边界。
- **完成的 job 会被淘汰**出内存表。要持久记录,开启 `server.audit`(JSONL 审计轨迹,记录每个 job 的命令与结果);否则外部捕获结果/`/metrics`。
- 宿主内核 **≥ 5.13**(Landlock 所需)。
