# HTTP API 参考

基础路径:`/api/v1`。请求体与 JSON 响应均为 `application/json`。默认无鉴权(生产环境请自行在 server 前加鉴权/网络边界)。

任务是**异步**的:提交立即返回 `job_id`,轮询 `GET /jobs/{id}` 取结果。教程式说明见 [usage.md](usage.md#提交任务异步)。

---

## 提交任务

### `POST /api/v1/jobs`

后台执行一个任务。返回 `202 Accepted` + `job_id`。

**请求体**

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `job_id` | string | 是 | 调用方指定;原样返回,用于轮询/cancel |
| `argv` | string[] | 是 | `argv[0]` 为可执行文件;须绝对路径(任务 `PATH` 极简) |
| `profile_name` | string | 是 | 已注册的 profile(见 `GET /profiles`) |
| `timeout` | string | 否 | 如 `"5s"`/`"100ms"`/`"1m"` 或纯数字(秒);缺省用 profile 的 `default_timeout` |
| `custom_env` | object | 否 | 任务额外环境变量(小幅白名单透传,如 `TZ`、`SSL_CERT_FILE`) |
| `stdin` | string | 否 | UTF-8 文本,经管道传入任务 stdin |
| `dry_run` | bool | 否 | `true` 时不执行,返回 profile 的限制(含 `egress_allowlist`) |

**响应 `202 Accepted`**(正常提交)

```json
{ "job_id": "demo-1", "status": "Running" }
```

**响应 `200 OK`**(`dry_run: true`)—— `DryRunSummary`

```json
{
  "profile": "python",
  "dry_run": true,
  "default_timeout_secs": 5,
  "max_stdout_mb": 5,
  "landlock": "Python",
  "fail_closed": false,
  "egress_allowlist": [ { "host": "pypi.org" }, { "host": "files.pythonhosted.org", "port": 443 } ]
}
```

**错误**

| 状态码 | 触发 |
|---|---|
| `400 Bad Request` | `timeout` 格式无效;体 `{"error": "..."}` |
| `404 Not Found` | `dry_run: true` 但 profile 不存在 |

---

## 查询任务

### `GET /api/v1/jobs/{job_id}`

轮询状态/结果。**`stdout`/`stderr` 已脱敏**后返回(见 [usage.md](usage.md#输出脱敏))。

**响应 `200 OK`** —— `JobResponse`

运行中(`job_id`/`status` 之外的字段省略):

```json
{ "job_id": "demo-1", "status": "Running" }
```

完成时:

```json
{
  "job_id": "demo-1",
  "status": "Completed",
  "exit_code": 0,
  "signal": null,
  "stdout": "hello\n",
  "stderr": "",
  "duration_ms": 12,
  "timed_out": false
}
```

**`status` 取值**

| 值 | 含义 |
|---|---|
| `Running` | 执行中 |
| `Completed` | 正常退出(任意退出码,含非零) |
| `TimedOut` | 超时被杀 |
| `Killed` | 被信号杀死(如 seccomp `SIGSYS` 违规、外部信号) |
| `Cancelled` | 经 `POST /jobs/{id}/cancel` 取消 |
| `Error` | 沙箱/初始化错误 |

**错误**:`404 Not Found` —— 任务不存在或已被淘汰。

---

## 取消任务

### `POST /api/v1/jobs/{job_id}/cancel`

取消运行中的任务。进程组收到 `SIGTERM` 然后 `SIGKILL`。

**响应**

| 状态码 | 体 | 触发 |
|---|---|---|
| `200 OK` | `{"job_id": "...", "status": "Cancelled"}` | 已取消 |
| `404 Not Found` | `{"error": "任务不存在"}` | 未知 job |
| `409 Conflict` | `{"error": "任务已完成,无法取消"}` | 已完成 |

---

## Worker 状态

### `GET /api/v1/status`

```json
{ "running_jobs": 3, "max_concurrent": 100, "uptime_secs": 4521 }
```

---

## Profile

### `GET /api/v1/profiles`

```json
{ "profiles": ["shell", "python", "node"] }
```

### `POST /api/v1/reload`

热重载配置文件(不重启更新 profile)。**fail-closed**:任一 profile 无效则整次 reload 中止。

| 状态码 | 体 |
|---|---|
| `200 OK` | `{ "success": true, "message": "...", "profiles_loaded": [...] }` |
| `500` | `{ "success": false, "message": "...", "profiles_loaded": [] }`(profile 无效) |

---

## 健康检查与指标

### `GET /health`

就绪检查——报告当前环境实际生效的安全机制:

```json
{
  "status": "ok",
  "landlock": { "supported": true, "abi_version": 5 },
  "cgroup": { "available": true, "controllers": ["Memory", "Cpu", "Pids"] },
  "seccomp": true,
  "disk_watermark_ok": true
}
```

### `GET /metrics`

Prometheus 文本格式(`text/plain; version=0.0.4`)。暴露 job 计数、运行中 gauge、fork/exec 耗时直方图。

---

## 超时格式

`timeout` / `default_timeout` 接受:

- `5s` —— 秒
- `100ms` —— 毫秒
- `1m` —— 分
- 纯数字 —— 秒(如 `"30"`)

## 备注

- **`argv[0]` 须绝对路径。** 任务环境极简(`PATH` 仅 `/usr/bin:/bin`),请把二进制解析成全路径。
- **`custom_env` 是白名单透传**,非全量继承——仅已知安全变量 + 你的额外项会设置。
- **完成的 job 最终会被淘汰**出内存 job 表;请及时轮询,别隔几小时再查。
