"""cr-083 S6: AsyncSandbox 镜像 + replicate。"""
import asyncio
import json

import httpx

from lvsandbox_e2b import AsyncSandbox, Sandbox


def _mock(handler):
    return dict(base_url="http://test", _transport=httpx.MockTransport(handler))


def test_async_create_and_get_info():
    async def run():
        def handler(req: httpx.Request):
            p = req.url.path
            if p == "/api/v1/sessions":
                return httpx.Response(201, json={"session_id": "s1"})
            if p == "/api/v1/sessions/s1":
                return httpx.Response(
                    200,
                    json={"session_id": "s1", "state": "RUNNING", "started_at": 1700000000},
                )
            return httpx.Response(404, json={"error": "no"})

        sb = await AsyncSandbox.create(template="base", timeout=60, **_mock(handler))
        assert sb.sandbox_id == "s1"
        si = await sb.get_info()
        assert si.state == "RUNNING"

    asyncio.run(run())


def test_async_commands_run_callbacks():
    async def run():
        body = (
            b'event: started\ndata: {"job_id":"x"}\n\n'
            b'event: stdout\ndata: {"data":"hi"}\n\n'
            b'event: result\ndata: {"job_id":"x","status":"Completed","exit_code":0,'
            b'"stdout":[],"stderr":[],"duration":{"secs":0,"nanos":1},"timed_out":false}\n\n'
        )

        def handler(req: httpx.Request):
            return httpx.Response(
                200, headers={"content-type": "text/event-stream"}, content=body
            )

        sb = await AsyncSandbox.connect("s1", **_mock(handler))
        out = []
        p = await sb.commands.run("echo hi", on_stdout=lambda b: out.append(b))
        assert b"".join(out) == b"hi"
        assert p.exit_code == 0

    asyncio.run(run())


def test_async_filesystem_read_write():
    async def run():
        def handler(req: httpx.Request):
            if req.method == "PUT":
                return httpx.Response(200, json={"ok": True})
            if req.method == "GET" and req.url.path.endswith("/files/f"):
                return httpx.Response(200, content=b"data")
            return httpx.Response(404, json={"error": "no"})

        sb = await AsyncSandbox.connect("s1", **_mock(handler))
        await sb.filesystem.write("/f", b"data")
        assert await sb.filesystem.read("/f") == "data"

    asyncio.run(run())


def test_async_set_timeout_patch():
    async def run():
        seen = {}

        def handler(req: httpx.Request):
            seen.update(method=req.method, body=json.loads(req.content))
            return httpx.Response(200, json={"ok": True})

        sb = await AsyncSandbox.connect("s1", **_mock(handler))
        await sb.set_timeout(45)
        assert seen["method"] == "PATCH" and seen["body"] == {"timeout_secs": 45}

    asyncio.run(run())


def test_replicate_creates_n_from_snapshot():
    calls = {"n": 0}

    def handler(req: httpx.Request):
        if req.url.path == "/api/v1/sessions/s1/snapshot":
            return httpx.Response(201, json={"snapshot_id": "snap"})
        if req.url.path == "/api/v1/sessions":
            calls["n"] += 1
            return httpx.Response(201, json={"session_id": f"s{calls['n']}"})
        return httpx.Response(404, json={"error": "no"})

    sb = Sandbox.connect("s1", **_mock(handler))
    clones = sb.replicate(2)
    assert len(clones) == 2
    assert [c.sandbox_id for c in clones] == ["s1", "s2"]
