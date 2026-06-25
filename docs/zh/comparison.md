# lv-sandbox 与同类方案对比

> 目的:帮你**按威胁模型**选对工具,而不是论证 lv-sandbox "最强"。不同工具优化的是不同威胁。lv-sandbox 自身的威胁边界见 [security.md](security.md)。

## 一句话版

大致两个阵营:

- **内核原语沙箱**(lv-sandbox、裸 Docker):用 Landlock/seccomp/cgroup 或 namespace 把**共享宿主内核**的进程关起来。轻、快、并发便宜。隔离强度取决于内核 + syscall 过滤。
- **虚拟化沙箱**(gVisor、Kata、Firecracker/microVM、E2B):每个任务放在用户态内核或真实 VM 后面。单任务更重更慢,但对抗内核漏洞/容器逃逸强得多。

lv-sandbox 在**第一阵营**里,走的是[设计哲学](security.md)里的**零特权、控制失败半径**路线:在不"一任务一容器"、不要任何额外 capability 的前提下,高并发地兜住 agent 的误操作与一般越权。

## 对比表

| 维度 | lv-sandbox | 裸 Docker(一任务一容器) | gVisor(runsc) | Kata Containers | Firecracker / microVM(如 E2B) |
|---|---|---|---|---|---|
| 隔离机制 | 一个 worker 内 Landlock+seccomp+cgroup | namespace+seccomp+cgroups | 用户态内核(拦截 syscall) | 一容器一 VM(硬件虚拟化) | 轻量 VM(硬件虚拟化) |
| 隔离强度 | 纵深防御;**不**抗内核漏洞利用 | 抗一般越权;有逃逸史(如 runc CVE) | 强(内核暴露面极小) | 很强(硬件隔离) | 很强(硬件隔离,设备模型最小) |
| 单任务冷启动 | **~毫秒**(fork/exec) | ~100ms–1s(起容器) | 容器速度 + 开销 | **秒级**(VM 启动) | ~125ms(Firecracker) |
| 并发模型 | 一个 worker 跑多个内核隔离任务 | 一任务一容器 | 一任务一容器 | 一任务一 VM | 一任务一 microVM |
| 网络出站 | **默认全断 + 白名单 SOCKS5,零特权** | 宿主防火墙 / iptables | 宿主网络 | VM 网络 | 可配置 |
| 需要的权限 | `--cap-drop=ALL`(无需) | 容器引擎 / root | ptrace 或 KVM | KVM / 嵌套虚拟化 | KVM |
| 适用威胁模型 | agent 误操作、一般越权、可信租户 | 通用隔离、多租户(需谨慎) | 容器内不可信代码 | 不可信 / 多租户生产 | 完全不可信代码、代码执行即服务 |
| 运维负担 | 单二进制 / 单容器 | 容器运行时 | runsc 运行时 | 更重(VM + containerd) | VM 基础设施或托管(E2B) |

## 该选哪个?

**选 lv-sandbox**:如果风险是"agent 犯傻或被轻度诱导"——读写错文件、死循环、想外联——而你要**一个 worker 高并发跑大量轻量、内核隔离的任务,零特权 + 真白名单出站**。典型:在可信租户 worker 上跑 AI agent 生成的命令/脚本。

**选 microVM(Kata / Firecracker / E2B)**:如果跑的是**完全不可信或敌意代码**、多租户负载、任意第三方依赖,或内核原语沙箱低于你的底线。代价是百毫秒~秒级冷启动 + VM 运维,换来硬件级隔离。代码执行即服务、高保障生产场景的正确选择。

**选 gVisor**:想要**对不可信代码强隔离、又留在容器生态**里(比全 VM 省事,比裸 Docker 强),接受其 syscall 拦截开销与兼容性注意点。

**选裸 Docker(一任务一容器)**:想用现成工具做通用隔离——但要意识到高并发时"一任务一容器"很重,且共享内核容器有真实的逃逸史。

## lv-sandbox 刻意不承诺的

- **不**对抗持有内核漏洞利用的坚定攻击者——Landlock/seccomp/cgroup 是内核特性,继承内核风险(见 [security.md](security.md))。
- **不**是**不可信、多租户**场景下 microVM 的替代品。
- **未**经过外部安全审计。

诚实的框架就是[设计哲学](security.md)那句:**按威胁模型选隔离等级,而不是按"是不是 Agent"。** lv-sandbox 是某一种常见威胁模型(agent 误操作、一般越权、高并发、零特权)的正确答案,也明确不是其它威胁模型的答案。
