"""cr-083 S3: Commands.run(SSE 回调)+ Process。"""
import json

import httpx
import pytest

from lvsandbox_e2b import Sandbox
from lvsandbox_e2b.exceptions import SandboxNotRunningError, TimeoutException


def _sb(handler):
    return Sandbox.connect(
        "s1", base_url="http://test", _transport=httpx.MockTransport(handler)
    )


def _stream(events):
    out = b""
    for ev, data in events:
        out += f"event: {ev}\ndata: {json.dumps(data)}\n\n".encode()
    return out


_RESULT_OK = {
    "job_id": "x",
    "status": "Completed",
    "exit_code": 0,
    "stdout": [],
    "stderr": [],
    "duration": {"secs": 0, "nanos": 1},
    "timed_out": False,
}


def test_run_on_stdout_incremental_and_exit_code():
    body = _stream(
        [
            ("started", {"job_id": "x"}),
            ("stdout", {"data": "hel"}),
            ("stdout", {"data": "lo"}),
            ("result", _RESULT_OK),
        ]
    )

    def handler(req: httpx.Request):
        return httpx.Response(
            200, headers={"content-type": "text/event-stream"}, content=body
        )

    chunks = []
    sb = _sb(handler)
    p = sb.commands.run("echo hello", on_stdout=lambda b: chunks.append(b))
    assert b"".join(chunks) == b"hello"
    assert p.exit_code == 0
    assert p.stdout == b"hello"


def test_run_nonzero_exit_does_not_raise():
    result = dict(_RESULT_OK)
    result.update(exit_code=2, stderr=[101, 114, 114])  # b"err"
    body = _stream([("started", {"job_id": "x"}), ("result", result)])

    def handler(req: httpx.Request):
        return httpx.Response(
            200, headers={"content-type": "text/event-stream"}, content=body
        )

    errs = []
    sb = _sb(handler)
    p = sb.commands.run("false", on_stderr=lambda b: errs.append(b))
    assert p.exit_code == 2
    assert b"".join(errs) == b"err"
    assert p.stderr == b"err"


def test_run_timeout_raises():
    result = dict(_RESULT_OK)
    result.update(status="TimedOut", exit_code=None, timed_out=True)
    body = _stream([("started", {"job_id": "x"}), ("result", result)])

    def handler(req: httpx.Request):
        return httpx.Response(
            200, headers={"content-type": "text/event-stream"}, content=body
        )

    sb = _sb(handler)
    with pytest.raises(TimeoutException):
        sb.commands.run("sleep 999", timeout=1)


def test_run_on_exit_fires_with_process():
    body = _stream([("started", {"job_id": "x"}), ("result", _RESULT_OK)])

    def handler(req: httpx.Request):
        return httpx.Response(
            200, headers={"content-type": "text/event-stream"}, content=body
        )

    exited = []
    sb = _sb(handler)
    sb.commands.run("true", on_exit=lambda p: exited.append(p.exit_code))
    assert exited == [0]


def test_run_passes_cwd_to_exec_stream():
    # 回归:integration 暴露 exec_stream 缺 cwd(Commands.run 走流式路径)
    seen = {}

    def handler(req: httpx.Request):
        if req.method == "POST":
            seen["body"] = json.loads(req.content)
        return httpx.Response(
            200,
            headers={"content-type": "text/event-stream"},
            content=_stream([("started", {"job_id": "x"}), ("result", _RESULT_OK)]),
        )

    sb = _sb(handler)
    sb.commands.run("pwd", cwd="sub")
    assert seen["body"]["cwd"] == "sub"


def test_run_no_result_raises_not_running():
    # 回归:server 对已 kill session 的流式 exec 返 200+空流;无 result 即失败。
    def handler(req: httpx.Request):
        return httpx.Response(
            200, headers={"content-type": "text/event-stream"}, content=b""
        )

    sb = _sb(handler)
    with pytest.raises(SandboxNotRunningError):
        sb.commands.run("ls")
