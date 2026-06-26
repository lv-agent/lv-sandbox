# 使用指南

## 环境要求

- Linux，宿主内核 ≥ 5.13（Landlock 所需）
- **Docker 部署**：仅需 Docker，镜像内置其余依赖
- **源码构建**：Rust 1.75+、`libseccomp-dev`（编译）/ `libseccomp2`（运行）
- 推荐以非 root 用户在容器内运行（见[架构文档 · 推荐部署](architecture.md#推荐部署)）

## 构建与运行

两种方式：**Docker 镜像**（推荐，开箱即用）或源码构建。

### Docker 部署（推荐）

镜像内置 `libseccomp2` 运行时、非 root 用户（uid 10000）和默认配置，`docker run` 即用。两种获取方式：

**方式 A：从 ghcr.io 拉取（最快）**

```bash
docker pull ghcr.io/lv-agent/lv-sandbox:v0.3.0
docker tag ghcr.io/lv-agent/lv-sandbox:v0.3.0 lv-sandbox:0.3.0   # 可选，便于复用下方命令
```

**方式 B：本地构建**

```bash
# 本地构建镜像
docker build -t lv-sandbox:0.3.0 .

# 或一条命令同时产出镜像 + 二进制 tar.gz（无 Docker 环境的兜底）
bash scripts/build-release.sh
```

**运行容器**：

```bash
docker run -d --name sandbox \
  -p 8080:8080 \
  --read-only --tmpfs /tmp:rw,nosuid,nodev,size=1g \
  -v /safe/worker/sandboxes:/sandboxes:rw \
  --cap-drop=ALL --security-opt no-new-privileges \
  --pids-limit=1000 --memory=4g --cpus=4 \
  --user 10000:10000 \
  lv-sandbox:0.3.0
```

要点：

- 宿主 Linux 内核 ≥ 5.13（Landlock 所需）；docker 默认 seccomp（libseccomp 2.5+）已放行 Landlock syscall，无需额外配置
- 挂载的 `/safe/worker/sandboxes` 宿主目录需可被 uid 10000 写入：`chown 10000:10000 /safe/worker/sandboxes`
- 镜像内置配置位于 `/etc/sandbox-server/config.yaml`，用 `-v your-config.yaml:/etc/sandbox-server/config.yaml:ro` 覆盖
- 容器内 cgroup v2 受限时自动降级到 rlimit 兜底（内置配置已设 `fail_closed: false`）
- 无需 `--privileged`

健康检查：`curl http://127.0.0.1:8080/health`

`build-release.sh` 产出的 `dist/lv-sandbox-<版本>-x86_64-gnu.tar.gz` 内含 `sandbox-server`/`sandbox-mcp`/示例配置/快速说明，解压后 `./sandbox-server --config config.yaml.example` 即可运行（需宿主 `libseccomp2`）。

### 源码构建

编译需 `libseccomp-dev`，运行需 `libseccomp2`。

```bash
cargo build --workspace --release
./target/release/sandbox-server --config config.yaml
```

配置文件查找优先级：`--config` 参数 > `SANDBOX_CONFIG` 环境变量 > `/etc/sandbox-server/config.yaml` > 内置默认。

---

## HTTP API

| 方法 | 路径 | 说明 |
|---|---|---|
| `POST` | `/api/v1/jobs` | 提交任务（异步——立即返回 `job_id`，后台运行） |
| `GET` | `/api/v1/jobs/{id}` | 查询任务状态/结果（`Running` 或完成时的 `JobResult`） |
| `POST` | `/api/v1/jobs/{id}/cancel` | 取消运行中的任务 |
| `GET` | `/api/v1/status` | 查询 worker 状态（运行数、并发上限、uptime） |
| `GET` | `/api/v1/profiles` | 列出所有可用 profile |
| `POST` | `/api/v1/reload` | 热重载配置（无需重启更新 profile） |
| `GET` | `/metrics` | Prometheus 指标 |
| `GET` | `/health` | 就绪检查——landlock/cgroup/seccomp 状态 + 磁盘水位 |

### 鉴权

默认无鉴权(本地零摩擦开发)。配 `server.api_key` 后,`/api/v1/*` 与 `/metrics`
需带 `Authorization: Bearer <key>` 头;`/health` 仍放行(探活)。缺失或错误凭证
返回 `401 {"error":"unauthorized"}`(常量时间比较)。开启后,`sandbox-mcp` 须配
`SANDBOX_API_KEY` 同值,否则网关被拒。

```yaml
server:
  api_key: "secret-token"   # 缺省 = 不鉴权(默认)
```

### 提交任务（异步）

提交后立即返回 `job_id`（`202 Accepted`），任务在后台执行。轮询 `GET /jobs/{id}` 获取结果。

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

返回：

```json
{ "job_id": "demo-1", "status": "Running" }
```

请求体还支持可选字段：
- `stdin`——UTF-8 文本，通过管道传给子进程 stdin（如 `cat` 或读取输入的脚本）
- `dry_run: true`——只校验不执行；返回 profile 的限制（timeout、landlock、max stdout、fail_closed）而非运行任务。适合 CI 验证或预览将应用哪些限制。

查询结果：

```bash
curl http://127.0.0.1:8080/api/v1/jobs/demo-1
```

返回（完成后）：

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

取消运行中的任务：

```bash
curl -X POST http://127.0.0.1:8080/api/v1/jobs/demo-1/cancel
```

`status` 可能值：`Completed`、`TimedOut`、`Killed`、`Cancelled`、`Error`。

### 流式 stdout（SSE）

加 `?stream=true`,响应变成 `text/event-stream`,实时推 stdout 而非返回 `job_id`。事件:

- `started` — `{"job_id": "..."}`(首个事件)
- `stdout` — `{"data": "<块>"}`(每个输出块一条;UTF-8,二进制 lossy)
- `result` — 终态 `JobResult`(status、exit_code、stdout、stderr……;末事件,发完关流)

stderr **不流式**——只出现在 `result` 事件里。job 跑在全 profile 约束下
(landlock/seccomp/cgroup/timeout/cancel/quota),已注册可 cancel,流结束后仍可
`GET /jobs/{id}` 查询。

```bash
curl -N -X POST 'http://127.0.0.1:8080/api/v1/jobs?stream=true' \
  -H 'content-type: application/json' \
  -d '{"job_id":"s","argv":["/bin/sh","-c","for i in 1 2 3; do echo tick $i; sleep 0.2; done"],"profile_name":"shell"}'
```

### 输出脱敏

`GET /jobs/{id}` 响应中的 `stdout`/`stderr` 会被脱敏——常见密钥模式（Bearer token、AWS `AKIA` 密钥、GitHub token、PEM 私钥）在返回前替换为 `[REDACTED]`，避免任务误读的凭证（如 `~/.aws/credentials`）泄露进 agent 上下文。

### 受控出站（egress allowlist）

默认所有任务零出站。在 profile 配置 `egress_allowlist` 可放行特定 host（可选 port）：

```yaml
profiles:
  python:
    egress_allowlist:
      - host: "pypi.org"
      - host: "*.pypi.org"
      - host: "files.pythonhosted.org"
        port: 443
```

- 任务**只能**通过 `SANDBOX_PROXY_SOCK`（工作区内 UDS 上的 SOCKS5h 代理）出站——seccomp 拦在 `socket()` 创建处，任务建不出任何 TCP/UDP socket。
- 任务代码用随仓库提供的 helper（`helpers/python/sandbox_net.py` 等）发请求，helper 自动经代理：`import sandbox_net; r = sandbox_net.get("https://api.openai.com/...")`。
- `*` 只匹配最左单个 label（`*.pypi.org` 命中 `download.pypi.org`，不命中 `a.b.pypi.org`）。
- `dry_run: true` 的响应含 `egress_allowlist`，可预览将放行哪些 host。

### 磁盘配额(每任务)

profile 可给任务的**聚合**工作区用量设上限。配 `disk_quota_mb`;任务工作区增长
超过上限即被收割(`SIGTERM` → `SIGKILL`),结果 `status` 为 `DiskQuotaExceeded`。

```yaml
profiles:
  heavy:
    disk_quota_mb: 50      # 工作区聚合上限(MB);缺省 = 不限
```

工作方式:看门狗每 250ms 测一次工作区大小,超过上限即杀整进程组。这是**尽力而为**
——两次轮询间的突发写最多超出 `250ms × 写速`;单文件 `fsize_mb` rlimit 收窄该窗口。
`disk_quota_mb` 限**总**工作区,`fsize_mb` 限**单文件**(两者互补)。`dry_run: true`
的响应含该上限。缺省 = 不限(默认)。

### Profile

内置三个 profile，按任务运行时选择：

| profile | 适用 | 内存上限 | 默认超时 |
|---|---|---|---|
| `shell` | 简单 shell 命令 | 128 MB | 5s |
| `python` | Python 脚本 | 256 MB | 5s |
| `node` | Node.js 脚本 | 256 MB | 5s |

> Docker 镜像内置 `python3`（含 `requests`/`httpx`）与 `node`,故 `python`/`node`
> profile 开箱即用。装额外包需配出站白名单(见[受控出站](#受控出站egress-allowlist))——
> 装到任务工作区。

自定义 profile 通过配置文件添加（见 [配置参考](#配置参考)）。

### 模板（预装环境）

"模板"就是一个 profile——它捆绑一组预装包(只读目录)+ baseline 环境变量,让运行时找得到。构镜像时把目录装好一次,profile 引用即可。

```bash
# 构 worker 镜像时:
scripts/build-template.sh data-science "pandas numpy scikit-learn"
```

```yaml
profiles:
  data-science:
    extra_readonly_paths: ["/opt/templates/data-science"]
    env:
      PYTHONPATH: "/opt/templates/data-science"
      MPLBACKEND: "Agg"
    rlimit:
      cpu_seconds: 30
    default_timeout: "60s"
```

profile 的 `env` 是 baseline(operator 可信):可设/覆盖 `PATH`、`LANG`,可加任意 key。请求级 `custom_env` 只能**加** profile 没设的 key。`HOME`/`TMPDIR` 永远指工作区,任何情况下都不可覆盖。

---

## 会话（持久沙箱）

**会话**是一个长期存活的沙箱:持久工作区 + 绑定 profile,跨多次 `exec` 存活。用于多步
agent 工作流(上传文件 → 跑 → 读结果 → 迭代),不必把所有事塞进一条命令。一次性
`POST /jobs` 仍用于单条命令。

```bash
# 1. 建
SID=$(curl -s -X POST http://127.0.0.1:8080/api/v1/sessions \
  -H 'content-type: application/json' -d '{"profile_name":"shell"}' | jq -r .session_id)

# 2. 上传脚本(原始字节)
curl -X PUT "http://127.0.0.1:8080/api/v1/sessions/$SID/files/run.sh" --data-binary @run.sh

# 3. exec(与第 2 步共享工作区)。加 ?stream=true 看实时 SSE stdout。
curl -X POST "http://127.0.0.1:8080/api/v1/sessions/$SID/exec" \
  -H 'content-type: application/json' -d '{"argv":["/bin/sh","run.sh"]}'

# 4. 列 / 下载 / 销毁
curl "http://127.0.0.1:8080/api/v1/sessions/$SID/files"            # 列
curl "http://127.0.0.1:8080/api/v1/sessions/$SID/files/out.txt"    # 下载
curl -X DELETE "http://127.0.0.1:8080/api/v1/sessions/$SID"
```

| 方法 | 路径 | 用途 |
|---|---|---|
| `POST` | `/sessions` | 建(`{profile_name, env?}`)→ `{session_id}` |
| `GET` | `/sessions` | 列 |
| `GET` | `/sessions/{id}` | 状态 |
| `DELETE` | `/sessions/{id}` | 销毁 |
| `POST` | `/sessions/{id}/exec` | 跑命令(`?stream=true` 走 SSE) |
| `PUT` | `/sessions/{id}/files/{path}` | 上传(原始字节) |
| `GET` | `/sessions/{id}/files/{path}` | 下载 |
| `GET` | `/sessions/{id}/files?path=` | 列 |
| `DELETE` | `/sessions/{id}/files/{path}` | 删 |

注意:

- profile **建会话时绑定**,会话内所有 exec 用它。
- 会话内 exec **串行**(一次一个)。
- 文件路径圈在会话工作区内(拒 `..`/绝对路径)。
- 每次 exec 跑在全 profile 约束下(landlock/seccomp/cgroup/timeout/cancel/quota)——与一次性 job 同。
- 会话**跨 worker 重启存活**:状态(工作区 + 绑定 profile)启动时重建,session id 重启后仍可用(重连)。快照与卷同样跨重启。
- 暂无后台 TTL reaper——用显式 `DELETE` 清理。

### 快照（fork 会话）

快照是会话 `workspace/` 的整树拷贝。备好环境后保存,再 fork 出新会话(快照只存文件——
profile/env 在恢复时给)。

```bash
SNAP=$(curl -s -X POST http://127.0.0.1:8080/api/v1/sessions/$SID/snapshot | jq -r .snapshot_id)
curl    http://127.0.0.1:8080/api/v1/snapshots                  # 列
curl -X POST http://127.0.0.1:8080/api/v1/sessions \
  -H 'content-type: application/json' \
  -d "{\"profile_name\":\"shell\",\"from_snapshot\":\"$SNAP\"}"  # fork
curl -X DELETE http://127.0.0.1:8080/api/v1/snapshots/$SNAP
```

| 方法 | 路径 | 用途 |
|---|---|---|
| `POST` | `/sessions/{id}/snapshot` | 快照会话 → `{snapshot_id}` |
| `GET` | `/snapshots` | 列 |
| `DELETE` | `/snapshots/{id}` | 删 |

快照落盘,**跨 worker 重启存活**。快照会等运行中 exec 完成(静默时拍)。

### 卷（持久存储）

**卷**是命名目录,跨会话、跨重启持久。挂进会话后,任务在 `workspace/<mount>` 见到它(symlink 指向卷);写入在会话销毁后仍保留。Landlock 授卷读写。

```bash
curl -X POST http://127.0.0.1:8080/api/v1/volumes -H 'content-type: application/json' -d '{"name":"data"}'
curl     http://127.0.0.1:8080/api/v1/volumes                                   # 列
# 挂进会话(任务写 ./volumes/data,跨会话持久):
curl -X POST http://127.0.0.1:8080/api/v1/sessions \
  -H 'content-type: application/json' \
  -d '{"profile_name":"shell","volumes":[{"name":"data","mount":"volumes/data"}]}'
curl -X DELETE http://127.0.0.1:8080/api/v1/volumes/data
```

| 方法 | 路径 | 用途 |
|---|---|---|
| `POST` | `/volumes` | 建(`{name}`) |
| `GET` | `/volumes` | 列 |
| `DELETE` | `/volumes/{name}` | 删 |

## MCP 集成（Claude Code / Hermes-Agent）

`sandbox-mcp` 把沙箱封装为 4 个 MCP 工具，AI Agent 可直接调用：

| 工具 | 功能 |
|---|---|
| `sandbox_run` | 在沙箱中执行命令，返回结果 |
| `sandbox_profiles` | 列出可用 profile |
| `sandbox_status` | 查询 worker 状态 |
| `sandbox_reload` | 热重载配置 |

### 接入 Claude Code

项目根目录已提供 `.mcp.json`：

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

前提：sandbox-server 已在 `127.0.0.1:8080` 运行。Claude Code 启动时自动加载 `.mcp.json`，拉起 `sandbox-mcp` 网关，即可在对话中调用沙箱工具。

> 生产环境建议把 `command` 改为编译好的二进制（`./target/release/sandbox-mcp`），避免每次启动编译。

### Hermes-Agent

在 Hermes-Agent 的配置中添加同样的 MCP server 连接信息，通过 stdio JSON-RPC 通信即可。

---

## 配置参考

```yaml
server:
  listen_addr: "0.0.0.0:8080"
  max_concurrent_jobs: 100      # 最大并发任务数
  log_level: "info"
  log_format: "json"            # json | text
  audit:                        # cr-021: 审计日志(默认关)
    enabled: false
    path: "/var/log/sandbox/audit.jsonl"
  api_key: "secret-token"       # cr-023: Bearer token;缺省 = 不鉴权(默认)
  webhooks: []                    # cr-031: 生命周期 webhook URL;空 = 不推(默认)

sandbox:
  base_dir: "/sandboxes"        # 任务工作空间根目录
  disk_watermark_mb: 1024       # 磁盘水位，低于则拒绝新任务（0 = 禁用）
  default_profile: "shell"
  fail_closed: true             # 安全机制不可用时是否拒绝执行

profiles:
  shell:
    rlimit:
      cpu_seconds: 2
      nofile: 64
      nproc: 32
      fsize_mb: 10
    max_stdout_mb: 5            # stdout 截断阈值
    default_timeout: "5s"

  python:
    extra_readonly_paths:       # 额外只读路径（如离线依赖库）
      - "/opt/sandbox-libs/python3"
    rlimit:
      cpu_seconds: 5
      nofile: 128
    max_stdout_mb: 10
    default_timeout: "30s"

  # 自定义 profile：未设字段继承 shell 默认值
  custom_task:
    rlimit:
      cpu_seconds: 10
    max_stdout_mb: 20
    default_timeout: "60s"
    disk_quota_mb: 100          # cr-022: 工作区聚合上限(MB);缺省 = 不限
    extra_readonly_paths:
      - "/data/shared"
```

修改配置后调用 `POST /api/v1/reload` 热重载，无需重启服务。

#### 审计日志

设 `server.audit.enabled: true` 开启 JSONL 审计轨迹(默认关)。每行自包含一个事件:
started / completed / timed_out / killed / cancelled / failed,带 argv、exit_code、
signal、duration_ms。审计文件在运维侧(不返 agent);按"可能含命令的日志"加以访问控制。

#### Webhook

配 `server.webhooks` 为 URL 列表,job/session exec 终态时 POST 同一份 AuditEvent
JSON(event_type、job_id/session_id、status、exit_code、argv……)——免轮询。默认关;
异步投递(fire-and-forget,3 次重试)。payload 含 argv,须按审计文件保护端点。

```yaml
server:
  webhooks: ["https://your-service/lvsandbox-hook"]
```

### 超时格式

`timeout` / `default_timeout` 支持：`5s`、`100ms`、`1m`，或纯数字（秒）。

---

## 组件

| 组件 | 类型 | 职责 |
|---|---|---|
| `sandbox-server` | 二进制 | HTTP 服务 + 调度 + 配置 + 指标 |
| `sandbox-mcp` | 二进制 | MCP 网关，对接 AI Agent |
| `sandbox-core` | 库 | 任务执行内核，可复用 |
| `sandbox-landlock` | 库 | Landlock 文件系统隔离 |
| `sandbox-seccomp` | 库 | seccomp syscall 过滤 |
| `sandbox-cgroup` | 库 | cgroup v2 资源管理 |

---

## 测试

```bash
# 全部测试
cargo test --workspace

# 仅端到端
cargo test -p sandbox-e2e

# 验证 Docker 镜像（容器内端到端：health + 真跑一个 echo 任务）
bash scripts/verify-image.sh
```
