"""E2B-shaped Commands (cr-083 §6.2).

`run` 走 lv-sandbox exec SSE(事件 started/stdout/result;**无 stderr 事件**):
- on_stdout:从 `stdout` 事件**增量**触发;
- on_stderr / on_exit:在结束时从 `result` 事件**批量**触发(stderr 在 result 里是
  Vec<u8> = JSON 字节数组,需 decode);
- 非零 exit **不抛**(§6.4),只置 `process.exit_code`;超时(timed_out)才抛
  TimeoutException。
"""
from __future__ import annotations

from typing import Callable, Optional

from ._handle import translating, translating_async
from .exceptions import SandboxNotRunningError, TimeoutException
from .models import Process


def _decode_maybe_bytes(v) -> bytes:
    """result 事件里 stdout/stderr 是 Vec<u8> -> JSON 字节数组;兼容 str/None。"""
    if v is None:
        return b""
    if isinstance(v, str):
        return v.encode()
    if isinstance(v, list):
        return bytes(b & 0xFF for b in v)
    return b""


class Commands:
    def __init__(self, client, sandbox_id: str):
        self._c = client
        self._sid = sandbox_id

    def run(
        self,
        cmd: str,
        *,
        background: bool = False,
        envs: Optional[dict] = None,
        user: str = "user",
        cwd: Optional[str] = None,
        timeout: Optional[int] = None,
        on_stdout: Optional[Callable] = None,
        on_stderr: Optional[Callable] = None,
        on_exit: Optional[Callable] = None,
    ) -> Process:
        if background:
            raise NotImplementedError(
                "background commands not supported (cr-083 §4)"
            )
        argv = ["/bin/sh", "-c", cmd]
        kw: dict = {}
        if envs:
            kw["env"] = envs
        if cwd is not None:
            kw["cwd"] = cwd
        if timeout is not None:
            kw["timeout"] = f"{int(timeout)}s"
        p = Process()
        stdout_acc = []
        with translating("command"):
            for ev in self._c.sessions.get(self._sid).exec_stream(argv, **kw):
                if ev.type == "stdout" and ev.stdout is not None:
                    chunk = (
                        ev.stdout.encode() if isinstance(ev.stdout, str) else ev.stdout
                    )
                    stdout_acc.append(chunk)
                    if on_stdout:
                        on_stdout(chunk)
                elif ev.type == "result" and isinstance(ev.data, dict):
                    d = ev.data
                    if d.get("timed_out"):
                        raise TimeoutException(f"command timed out: {cmd}")
                    p.exit_code = d.get("exit_code")
                    err = _decode_maybe_bytes(d.get("stderr"))
                    if err and on_stderr:
                        on_stderr(err)
                    p.stdout = b"".join(stdout_acc)
                    p.stderr = err
                    if on_exit:
                        on_exit(p)
        # 流式 exec 对不存在的 session 返 200+空流(server 行为);无 result 即失败。
        if p.exit_code is None:
            raise SandboxNotRunningError(
                f"no result from command (session may not be running): {cmd}"
            )
        return p

    # ----- server-gated(cr-083 S7:无 server 支撑,显式 defer) -----

    def send_stdin(self, pid, data) -> None:
        raise NotImplementedError(
            "commands.send_stdin needs interactive exec stdin (cr-085 M9, deferred)"
        )

    def list(self) -> list:
        raise NotImplementedError(
            "commands.list needs a process-list endpoint (no server support)"
        )

    def kill(self, pid, signal: int = 9) -> None:
        raise NotImplementedError(
            "commands.kill needs a process-kill endpoint (no server support)"
        )


class AsyncCommands:
    """Async counterpart of :class:`Commands` (cr-083 §6.2 AsyncSandbox)。"""

    def __init__(self, client, sandbox_id: str):
        self._c = client
        self._sid = sandbox_id

    async def run(
        self,
        cmd: str,
        *,
        background: bool = False,
        envs: Optional[dict] = None,
        user: str = "user",
        cwd: Optional[str] = None,
        timeout: Optional[int] = None,
        on_stdout: Optional[Callable] = None,
        on_stderr: Optional[Callable] = None,
        on_exit: Optional[Callable] = None,
    ) -> Process:
        if background:
            raise NotImplementedError(
                "background commands not supported (cr-083 §4)"
            )
        argv = ["/bin/sh", "-c", cmd]
        kw: dict = {}
        if envs:
            kw["env"] = envs
        if cwd is not None:
            kw["cwd"] = cwd
        if timeout is not None:
            kw["timeout"] = f"{int(timeout)}s"
        p = Process()
        stdout_acc = []
        async with translating_async("command"):
            s = await self._c.sessions.get(self._sid)
            async for ev in s.exec_stream(argv, **kw):
                if ev.type == "stdout" and ev.stdout is not None:
                    chunk = (
                        ev.stdout.encode() if isinstance(ev.stdout, str) else ev.stdout
                    )
                    stdout_acc.append(chunk)
                    if on_stdout:
                        on_stdout(chunk)
                elif ev.type == "result" and isinstance(ev.data, dict):
                    d = ev.data
                    if d.get("timed_out"):
                        raise TimeoutException(f"command timed out: {cmd}")
                    p.exit_code = d.get("exit_code")
                    err = _decode_maybe_bytes(d.get("stderr"))
                    if err and on_stderr:
                        on_stderr(err)
                    p.stdout = b"".join(stdout_acc)
                    p.stderr = err
                    if on_exit:
                        on_exit(p)
        if p.exit_code is None:
            raise SandboxNotRunningError(
                f"no result from command (session may not be running): {cmd}"
            )
        return p

    # ----- server-gated(cr-083 S7:async 同 defer) -----

    async def send_stdin(self, pid, data) -> None:
        raise NotImplementedError(
            "commands.send_stdin needs interactive exec stdin (cr-085 M9, deferred)"
        )

    async def list(self) -> list:
        raise NotImplementedError(
            "commands.list needs a process-list endpoint (no server support)"
        )

    async def kill(self, pid, signal: int = 9) -> None:
        raise NotImplementedError(
            "commands.kill needs a process-kill endpoint (no server support)"
        )
