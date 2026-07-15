"""cr-083 S2: Sandbox lifecycle + SandboxInfo (mock 单测)."""
import json
import os

import httpx
import pytest

from lvsandbox_e2b import Sandbox
from lvsandbox_e2b.exceptions import SandboxException


def _mock(handler, **kw):
    kw.setdefault("base_url", "http://test")
    kw["_transport"] = httpx.MockTransport(handler)
    return kw


def test_create_maps_template_and_timeout():
    seen = {}

    def handler(req: httpx.Request):
        seen["body"] = json.loads(req.content)
        return httpx.Response(201, json={"session_id": "s1"})

    sb = Sandbox.create(template="base", timeout=60, **_mock(handler))
    assert sb.sandbox_id == "s1"
    assert seen["body"]["profile_name"] == "shell"  # base -> shell
    assert seen["body"]["timeout_secs"] == 60  # E2B timeout(秒) -> timeout_secs
    assert "cwd" not in seen["body"]


def test_connect_holds_id_without_call():
    def handler(req):  # 不应被调用
        pytest.fail("connect must not hit the server")

    sb = Sandbox.connect("abc", **_mock(handler))
    assert sb.sandbox_id == "abc"


def test_get_info_maps_sandbox_info():
    info = {
        "session_id": "s1",
        "profile": "shell",
        "state": "RUNNING",
        "started_at": 1700000000,
        "alias": "a",
        "cpu_count": 2,
        "memory_size": 536870912,
        "template_id": "shell",
        "metadata": {"k": "v"},
        "execs": 3,
    }

    def handler(req: httpx.Request):
        return httpx.Response(200, json=info)

    sb = Sandbox.connect("s1", **_mock(handler))
    si = sb.get_info()
    assert si.sandbox_id == "s1"
    assert si.state == "RUNNING"
    assert si.started_at == 1700000000
    assert si.cpu_count == 2
    assert si.metadata == {"k": "v"}


def test_list_filters_client_side_by_metadata():
    payload = {
        "sessions": [
            {"session_id": "s1", "metadata": {"env": "prod"}},
            {"session_id": "s2", "metadata": {"env": "dev"}},
        ]
    }

    def handler(req: httpx.Request):
        # server 不支持过滤;query 不带 state/metadata
        assert "state" not in req.url.params and "metadata" not in req.url.params
        return httpx.Response(200, json=payload)

    res = Sandbox.list(metadata={"env": "prod"}, **_mock(handler))
    assert [s.sandbox_id for s in res] == ["s1"]


def test_list_filters_by_state():
    payload = {"sessions": [
        {"session_id": "s1", "state": "RUNNING"},
        {"session_id": "s2", "state": "STOPPED"},
    ]}

    def handler(req: httpx.Request):
        return httpx.Response(200, json=payload)

    res = Sandbox.list(state="RUNNING", **_mock(handler))
    assert [s.sandbox_id for s in res] == ["s1"]


def test_set_timeout_patches_timeout_secs():
    seen = {}

    def handler(req: httpx.Request):
        seen.update(method=req.method, body=json.loads(req.content))
        return httpx.Response(200, json={"ok": True})

    sb = Sandbox.connect("s1", **_mock(handler))
    sb.set_timeout(120)
    assert seen["method"] == "PATCH"
    assert seen["body"] == {"timeout_secs": 120}


def test_kill_deletes_session():
    seen = {}

    def handler(req: httpx.Request):
        seen.update(method=req.method, path=req.url.path)
        return httpx.Response(200, json={"ok": True})

    sb = Sandbox.connect("s1", **_mock(handler))
    sb.kill()
    assert seen == {"method": "DELETE", "path": "/api/v1/sessions/s1"}


def test_sandbox_id_before_create_raises():
    sb = Sandbox(base_url="http://test")  # 无 sandbox_id
    with pytest.raises(SandboxException):
        _ = sb.sandbox_id
