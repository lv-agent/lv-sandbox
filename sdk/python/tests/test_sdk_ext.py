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


# ----- P1b: PATCH 生命周期 + watch SSE -----


def test_set_timeout_metadata_alias_patch():
    seen = []

    def handler(req: httpx.Request):
        seen.append((req.method, req.url.path, json.loads(req.content)))
        return httpx.Response(200, json={"ok": True})

    c = _client(handler)
    s = c.sessions.get("s1")
    s.set_timeout(300)
    s.set_metadata({"x": "1"})
    s.set_alias("nm")
    assert seen[0] == ("PATCH", "/api/v1/sessions/s1", {"timeout_secs": 300})
    assert seen[1] == ("PATCH", "/api/v1/sessions/s1", {"metadata": {"x": "1"}})
    assert seen[2] == ("PATCH", "/api/v1/sessions/s1", {"alias": "nm"})


def test_watch_yields_created_modified_removed():
    body = (
        b'event: created\ndata: {"paths":["a.txt"]}\n\n'
        b'event: modified\ndata: {"paths":["a.txt"]}\n\n'
        b'event: removed\ndata: {"paths":["b.txt"]}\n\n'
    )

    def handler(req: httpx.Request):
        return httpx.Response(
            200, headers={"content-type": "text/event-stream"}, content=body
        )

    c = _client(handler)
    s = c.sessions.get("s1")
    events = list(s.watch(timeout_secs=5))
    assert events == [
        {"event": "created", "paths": ["a.txt"]},
        {"event": "modified", "paths": ["a.txt"]},
        {"event": "removed", "paths": ["b.txt"]},
    ]
