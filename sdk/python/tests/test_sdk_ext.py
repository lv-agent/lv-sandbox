"""Layer 0 (cr-083 P1a/P1b/P1c/P2): cr-085 SDK surface —— mock 单测。

用 httpx.MockTransport 注入 Client(transport=),无需 server。
"""
import asyncio
import json

import httpx

from lvsandbox import Client


def _client(handler):
    return Client("http://test", transport=httpx.MockTransport(handler))


# ----- P1a: create/exec 新字段 -----


def test_create_sends_timeout_secs_alias_metadata():
    captured = {}

    def handler(req: httpx.Request):
        captured["body"] = json.loads(req.content)
        return httpx.Response(201, json={"session_id": "s1"})

    c = _client(handler)
    s = c.sessions.create(
        profile="python", timeout_secs=120, alias="a1", metadata={"k": "v"}
    )
    assert s.id == "s1"
    body = captured["body"]
    assert body["profile_name"] == "python"
    assert body["timeout_secs"] == 120
    assert body["alias"] == "a1"
    assert body["metadata"] == {"k": "v"}
    assert "cwd" not in body  # create 无 cwd


def test_exec_sends_cwd_and_custom_env():
    captured = {}

    def handler(req: httpx.Request):
        captured["body"] = json.loads(req.content)
        return httpx.Response(
            200,
            json={
                "job_id": "session:s1",
                "status": "Completed",
                "exit_code": 0,
                "stdout": "hi",
                "stderr": "",
                "duration_ms": 5,
                "timed_out": False,
            },
        )

    c = _client(handler)
    s = c.sessions.get("s1")
    r = s.exec(["/bin/sh", "-c", "echo hi"], cwd="sub", env={"E": "1"})
    body = captured["body"]
    assert body["cwd"] == "sub"
    assert body["custom_env"] == {"E": "1"}
    assert r.exit_code == 0
    assert r.stdout == "hi"
