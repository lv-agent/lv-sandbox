# 在 Claude Code 里用 lv-sandbox

目标:5 分钟内让 Claude Code 把 agent 的命令**放进** lv-sandbox 执行,并亲眼看到一个危险命令被兜住。

## 结构

```
Claude Code  ──stdio JSON-RPC──▶  sandbox-mcp(网关)  ──HTTP──▶  sandbox-server(沙箱本体)
```

`sandbox-mcp` 是个薄网关——暴露 4 个 MCP 工具,转发给运行中的 `sandbox-server`。Claude Code 启动时自动加载仓库自带的 `.mcp.json`,工具即可用。

## 1. 启动沙箱服务

```bash
docker run -d --name sandbox -p 8080:8080 \
  --read-only --tmpfs /tmp:rw,nosuid,nodev,size=1g \
  --tmpfs /sandboxes:rw,nosuid,nodev,size=100m,uid=10000,gid=10000 \
  --cap-drop=ALL --security-opt no-new-privileges \
  --pids-limit=1000 --memory=4g --cpus=4 --user 10000:10000 \
  ghcr.io/lv-agent/lv-sandbox:v0.2.0
curl http://127.0.0.1:8080/health   # → {"status":"ok", ...}
```

## 2. 让 Claude Code 连上它

仓库自带 `.mcp.json`:

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

- 在本仓库的 clone 里,Claude Code 启动时自动发现 `.mcp.json`,提示你批准 `sandbox` 服务。
- **启动更快**:先 `cargo build --release -p sandbox-mcp` 编一次,把 `command` 指向二进制:
  `"command": "./target/release/sandbox-mcp"`。

agent 现在能调用的 4 个工具:

| 工具 | 作用 |
|---|---|
| `sandbox_run` | 在沙箱里执行命令,返回结果 |
| `sandbox_profiles` | 列出可用 profile |
| `sandbox_status` | worker 状态 |
| `sandbox_reload` | 热重载配置 |

## 3. 在对话里用

直接让 Claude Code 跑点什么,并要求它用沙箱:

> 在沙箱里运行 `echo hello agent`。

Claude Code 调用 `sandbox_run`(argv `["/bin/echo","hello agent"]`,profile `shell`),返回:

```
status: Completed, exit_code: 0
stdout: hello agent
```

## 4. 看它兜住危险命令

> 在沙箱里运行 `curl -s http://example.com`。

agent 调 `sandbox_run`。沙箱内 `curl` 尝试 `socket(AF_INET, …)`——seccomp 直接杀掉。agent 拿到:

```
status: Killed
```

没外联,没碰到网络,命令被兜住,agent 据实回报。

再试文件越界:

> 在沙箱里运行 `cat /etc/passwd`。

→ `Completed` 退出码 1,stderr `cat: /etc/passwd: Permission denied`——Landlock 把任务圈在工作区,宿主文件够不着。

## 备注

- **何时调沙箱由 agent 决定**——你用"在沙箱里运行 …"来引导它。要更强控制,自己写一个 agent 循环,强制所有命令走 `sandbox_run`。
- **要"受控出站"而非全断**:给 profile 配 `egress_allowlist` + 用随仓库 helper,见 [network-isolation.md](../network-isolation.md)。
- **server 必须先于网关启动**;网关用 `GET /jobs/{id}` 轮询结果(单次调用上限 300s)。
