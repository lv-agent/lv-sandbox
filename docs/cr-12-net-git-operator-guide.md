# CR-12 网络化 Git 沙箱 — Operator 指南

让牢内 agent 经 swap-proxy 安全 clone/push 远端 git 仓。本文 = env 契约 + 组装 recipe + 坑。

## 1. 拓扑
```
agent(牢内)→ git-remote-fixus helper → per-job SOCKS5h(UDS)→ swap-proxy(牢外)
  [sentinel→real token 改写 + Host 重写]→ 真 git 上游(github.com / 自建)
```
牢内只见 sentinel(公开占位值);真 token 只在 swap-proxy 进程。绕 swap-proxy 直连 → SOCKS allowlist 按 hostname 拒。

## 2. env 契约(两侧同 sentinel 必填)

**sandbox-server 主机进程**(`FIXUS_GIT_*`,git profile 在此读):
| env | 必填 | 说明 |
|---|---|---|
| `FIXUS_GIT_SENTINEL` | 是 | 公开占位值;进牢作 `FIXUS_GIT_SENTINEL`,helper 发 `Authorization: Bearer <它>` |
| `FIXUS_GIT_HELPER_DIR` | **是** | 含 `git-remote-fixus` 的目录;profile 把它前置到牢内 PATH,并授 landlock `ReadExecute`(牢内 PATH 默认仅 `/usr/bin:/bin`,且只有这俩有 exec 权,故不设此则 `git clone fixus::` 找不到/无法 exec helper) |
| `FIXUS_GIT_EGRESS_HOST` | 否(默 github.com) | swap-proxy 主机(allowlist target = helper 连接处) |
| `FIXUS_GIT_EGRESS_PORT` | 否(默 443) | swap-proxy 端口 |
| `FIXUS_GIT_CA_FILE` | 否(缺则 webpki) | swap-proxy 入站 TLS 证 PEM 路径 → 进牢作 `SANDBOX_CA_PEM` |
| `FIXUS_GIT_NPROC` | 否(默 256) | RLIMIT_NPROC 覆盖。**共享/构建 UID 主机**(broker+tools-bank+sandbox-server 同 UID,nproc 按 UID 全机计)需抬高(如 8192),否则牢内 fork 立即 EAGAIN |

**swap-proxy 进程**(`FIXUS_SWAP_*`,`fixus-egress-swap-proxy`):
| env | 必填 | 说明 |
|---|---|---|
| `FIXUS_SWAP_SENTINEL` | 是 | **必须与 `FIXUS_GIT_SENTINEL` 同值** |
| `FIXUS_SWAP_TOKEN` | 是(秘密) | 真 token;只在本进程;改写后 `Bearer <它>` 转发 |
| `FIXUS_SWAP_CERT_PEM` | 是 | 入站 TLS 证 PEM(无自签回退,fail-closed) |
| `FIXUS_SWAP_KEY_PEM` | 是(秘密) | 入站 TLS 私钥 PEM |
| `FIXUS_SWAP_LISTEN` | 否(默 127.0.0.1:8443) | bind |
| `FIXUS_SWAP_UPSTREAM` | 否(默 github.com:443) | 真上游 host:port |
| `FIXUS_SWAP_UPSTREAM_CA_PEM` | 否(缺则 webpki-roots) | 上游 CA |

## 3. 组装 recipe(可跑参考:`crates/git-remote-fixus/tests/cr12_e2e_full_stack.rs`)
1. 起 logdb-broker(`embedded:true` 内嵌 logdbd;`session_timeout_ms>0`;`bind_addr` 与下两者 `--broker-addr` 一致,如 5100;config 经 `LOGDB_BROKER_CONFIG` env 非 `--config`;embedded 模式 `logdbd_addr` 是裸 `host:port` 非 URL)。
2. 起 sandbox-server:`--config <yaml>`(base_dir 可写 / `fail_closed:false`(WSL2 landlock/cgroup 降级)/ `profiles.shell.rlimit.nproc:8192`)+ 全部 `FIXUS_GIT_*`(SENTINEL/HELPER_DIR/EGRESS_HOST/EGRESS_PORT/CA_FILE,共享 UID 主机加 NPROC)+ `PATH` 含 `git-remote-fixus`(供 sandbox-server 自身;牢内 PATH 由 profile 的 HELPER_DIR 注入)。
3. 起 sandbox-broker:`--broker-addr … --sandbox-url http://…:8080 --region … --group …`,**去所有 `*_PROXY` env**。
4. 起 tools-bank:`--broker-addr … --region … --port 3001`。
5. 起 swap-proxy(`fixus-egress-swap-proxy`)带上节 `FIXUS_SWAP_*` env。
6. (真 LLM turn 才需)起 fixus serve(:3000,+ redis 6379 + `REDIS_URL`/`SANDBOX_REGION`)+ fixlet(去 proxy + LLM 配置)。

**helper 安装**:operator 既可把 `git-remote-fixus` 装到系统 PATH(牢内默认 `/usr/bin:/bin`),也可经 `FIXUS_GIT_HELPER_DIR` 指向其目录(profile 注入牢内 PATH + 授 landlock `ReadExecute`)。后者对 landlock 是必需(仅 `/bin`+`/usr/bin` 默认带 exec 权)。

## 4. systemd 样例(swap-proxy unit)
```ini
[Unit]
Description=fixus egress swap-proxy
[Service]
EnvironmentFile=/etc/fixus/swap.env   # FIXUS_SWAP_*
ExecStart=/usr/local/bin/fixus-egress-swap-proxy
Restart=on-failure
# 不打印秘密:SwapConfig 不 derive Debug。
[Install]
WantedBy=multi-user.target
```

## 5. 坑
1. logdb-broker `session_timeout_ms>0`(否则 stale member 不驱逐,turn 认领死锁)。
2. 起所有 fixus 系进程前确认无 `HTTPS_PROXY` 等(常 down);sandbox-broker 尤其要去。
3. logdbd/broker stream 名只允许 `[a-zA-Z0-9_-]`,禁点号。
4. sandbox-server 配置三件套:`base_dir` 可写 / `fail_closed:false` / shell `nproc:8192`(RLIMIT_NPROC 按 UID 计);git profile 的 nproc 经 `FIXUS_GIT_NPROC` 另调。
5. helper 只支持 git 协议 v0/v1;`git -c protocol.version=0 clone …`。
6. **helper 必须对牢内可 exec**:仅设 PATH 不够(landlock);用 `FIXUS_GIT_HELPER_DIR`(授 `ReadExecute`)或装到 `/usr/bin`。
