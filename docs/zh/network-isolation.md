# 网络隔离与受控出站

> 范围:lv-sandbox 如何阻止任务发起网络连接,以及如何让某个 profile **按白名单受控出站**(经 UDS SOCKS5 代理)。完整威胁模型见 [security.md](security.md);配置语法见 [usage.md](usage.md#受控出站egress-allowlist)。

## 摘要

- **默认所有任务零网络出站**,连 TCP/UDP socket 都建不出来。
- profile 设置 `egress_allowlist` 即可开启**受控出站**。任务仍不能建原始 TCP/UDP socket——所有出站都经工作区内的 UDS 上的 SOCKS5 代理转发,代理只放行白名单内的 `host:port`。
- 强制力是**内核级**(seccomp)、**fail-closed**:不用代理的任务得到的是"零网络",而不是"无限制网络"。

**零特权**——默认 `--cap-drop=ALL` 容器部署下即可生效。

---

## 强制机制:seccomp 只允许 `AF_UNIX` socket

子进程 `exec` 前应用 seccomp filter。网络方面只做一件事:

> `socket(domain)` 当 `domain != AF_UNIX` 时被杀(`SIGSYS`)。

socket API 的其余部分(`connect`/`bind`/`send`/`recv`……)**放行**——但任务根本拿不到非 `AF_UNIX` 的 socket fd,这些调用只能作用于 Unix socket。`pre_exec` 里关闭继承 fd,杜绝 `AF_INET` fd 泄入。

### 为什么强制点在 `socket()` 而非 `connect()`

经典 seccomp-BPF 只能检查 syscall 的**寄存器参数**,不能解引用用户态指针。`connect()` 的 `sockaddr` 是指针,seccomp **无法**按目标地址过滤。因此强制点必须在 `socket()` 创建处——堵住 INET/RAW socket 的诞生。这是标准做法。

### 为什么 UDS 不是逃逸口

任务*能*建 `AF_UNIX` socket(为了连代理)。它能借 UDS 逃逸吗?不能:

- 连接 UDS 要打开其路径,受 **Landlock** 约束。Landlock 把任务圈在工作区内,唯一能连到的 UDS 是工作区内的代理 socket 文件;系统 UDS(`/var/run/docker.sock` 等)landlock 不可达。
- **抽象命名空间 socket**(路径以 `\0` 开头)绕过文件系统、绕过 Landlock,但它是纯本地 IPC,无法承载网络流量。最多让两个被沙箱化的任务本地互通(受控的次要信道,本轮不管)。

净效果:任务只有一条网络路径——白名单代理。

---

## 受控出站:UDS 上的 SOCKS5h

profile 有非空 `egress_allowlist` 时,**server 进程**(未被沙箱化的父进程,持有真实网络能力)为该 job 起一个 SOCKS5 代理:

```text
┌──────── server 进程(未被沙箱化,有真实网络)──────────────┐
│  run_job:                                                  │
│   ├─ bind <workspace>/.proxy.sock(UnixListener, tokio)     │
│   ├─ SOCKS5h 循环:accept → 白名单校验 → DNS → TCP          │
│   ├─ pre_exec:seccomp(只放 AF_UNIX)+ landlock + ……       │
│   └─ env SANDBOX_PROXY_SOCK=<workspace>/.proxy.sock        │
└──────────────────────┬─────────────────────────────────────┘
                       │ AF_UNIX(Landlock 圈在工作区内)
┌──────────────────────▼─────────────────────────────────────┐
│  任务进程(被沙箱化:只能建 AF_UNIX socket)                │
│   helper → 拨 SANDBOX_PROXY_SOCK → SOCKS5h CONNECT(host)   │
└──────────────────────┬─────────────────────────────────────┘
                       │ 代理:白名单 → DNS → 真 TCP
                       ▼
                 真实网络(仅白名单内)
```

### SOCKS5h(远程 DNS)

代理实现 SOCKS5([RFC 1928](https://www.rfc-editor.org/rfc/rfc1928))+ **远程 DNS**(`DOMAINNAME` 地址类型)。任务传 hostname,代理解析。这绕开了"任务没网做 DNS"的鸡生蛋问题。IP 字面量地址类型(`IPv4`/`IPv6`)被拒,强制用 hostname(保证白名单有意义、可审计)。

### 白名单模型

- **per-profile**,配置项 `egress_allowlist`。空/缺省 = **零出站**(代理都不起)。
- 每条规则:`host`(精确或 `*.example.com` 单段通配)+ 可选 `port`(不填 = 该 host 任意端口)。
- 大小写不敏感,默认拒绝。
- `dry_run: true` 返回解析后的白名单供预览。

```yaml
profiles:
  python:
    egress_allowlist:
      - host: "pypi.org"
      - host: "*.pypi.org"            # 命中 download.pypi.org,不命中 a.b.pypi.org
      - host: "files.pythonhosted.org"
        port: 443
```

---

## 任务怎么用(helper)

标准工具(`curl`/`pip`)不能原生指向 UDS 代理(代理 URL 必须是 `host:port`)。仓库提供薄 **helper**:拨 UDS、做 SOCKS5h 握手、在 relay 流上跑 HTTP——纯标准库,零三方依赖:

| helper | 用法 |
|---|---|
| `helpers/python/sandbox_net.py` | `import sandbox_net; r = sandbox_net.get("https://api.openai.com/...")` |
| `helpers/node/sandbox-net.js` | `const {request} = require('./sandbox-net'); request('GET', url, null, null, cb)` |
| `helpers/shell/sandbox-curl` | `sandbox-curl https://...`(委托 python helper) |

helper 读 `SANDBOX_PROXY_SOCK` 定位代理。

> helper 是**协作式**的——任务要主动用。但安全**不**依赖协作:绕过 helper 直连的任务会被 seccomp 杀(建不出 `AF_INET` socket)。helper 解决**易用性**,seccomp 保**安全性**。

HTTPS 时 helper 在 relay 流上做 TLS 升级(`ssl`/`tls`)。信任上游证书照常(`SSL_CERT_FILE` / `NODE_EXTRA_CA_CERTS`)。

---

## 阻止什么 / 不阻止什么

**阻止:**
- 任何原始 TCP/UDP 连接(seccomp 杀 `socket(AF_INET, …)`)。
- 访问云元数据服务、phone-home、开监听。
- 即使代理启用,访问白名单外的 host。
- 绕过代理(没有别的 socket 路径)。

**不阻止(设计内/范围外):**
- 代理**不**解密或审查 TLS——透明字节中继,不做内容级审计/DLP。
- HTTP/2 被代理**透明承载**(协议无关),但随仓库 helper 只到 HTTP/1.1。要 HTTP/2,任务需自带 h2 client over SOCKS5h relay。
- **无 per-task 网络命名空间(netns)**。零特权设计刻意避免 `CAP_SYS_ADMIN`,改用 seccomp 作强制层。
- 抽象命名空间 UDS 跨任务本地 chatter(次要本地信道)。

---

## 验证

实现由集成与 e2e 测试覆盖:

- seccomp:`socket(AF_INET)` → 被杀(`SIGSYS`);`socket(AF_UNIX)` → 放行。
- SOCKS5 代理:白名单内往返成功;白名单外 → SOCKS5 reply `0x02`;IPv4 字面量 → `0x02`;非 CONNECT → `0x07`;上游拒绝 → `0x05`。
- 畸形报文(错误版本、截断问候/请求/domain、超大 method 列表)优雅处理——不 panic、不挂。
- helper(python + node)经代理走 HTTP 与 HTTPS 往返。
- `JobProxy` 清理(cancel/早返回路径)——无 fd/task 泄漏。

运行:`cargo test --workspace`。
